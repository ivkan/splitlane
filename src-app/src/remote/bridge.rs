//! The bridge: one connection per paired phone, answering its requests.

use std::io;
use std::time::{Duration, Instant};

use serde_json::Value;
use splitlane_remote::wire::{Frame, Message};
use splitlane_remote::{Channel, DeviceKey, Handshake, PublicKey, Role, RoomId};

use super::relay::{self, Received, RelaySocket};
use super::{Link, store, surface};

/// How long one wait for a frame lasts before the bridge looks up.
const TICK: Duration = Duration::from_secs(5);
/// How often a quiet connection is pinged.
const KEEP_ALIVE: Duration = Duration::from_secs(30);
/// Wait before joining the room again after the connection dropped, and
/// the longest it grows to.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Where one phone's connection stands.
enum State {
    /// No channel: waiting for the phone's hello.
    Idle,
    /// Salts exchanged; the phone's confirmation has not arrived.
    Confirming(Channel),
    Open(Channel),
}

/// What the bridge answers a request with.
pub(crate) type Answerer<'a> = dyn FnMut(&str, &Value) -> Result<Value, String> + 'a;

/// Serve one phone on `link` until the link fails.
pub(crate) fn serve_connection(
    link: &mut impl Link,
    key: &DeviceKey,
    phone: &PublicKey,
    room: RoomId,
    answer: &mut Answerer<'_>,
) -> io::Result<()> {
    let mut state = State::Idle;
    let mut last_sent = Instant::now();
    loop {
        let frame = match link.receive(TICK)? {
            Received::Frame(frame) => frame,
            Received::Junk => continue,
            Received::Quiet => {
                if last_sent.elapsed() >= KEEP_ALIVE {
                    link.keep_alive()?;
                    last_sent = Instant::now();
                }
                continue;
            }
        };
        state = match (frame, state) {
            // A hello always starts over: the phone connected again.
            (Frame::Hello(phone_salt), _) => {
                let handshake = Handshake::start(Role::Machine);
                link.send(&Frame::Hello(handshake.salt()))?;
                last_sent = Instant::now();
                State::Confirming(handshake.finish(key, phone, room, phone_salt))
            }
            (Frame::Sealed(sealed), State::Confirming(mut channel)) => {
                match channel
                    .open(&sealed)
                    .map(|plain| Message::from_bytes(&plain))
                {
                    Ok(Ok(Message::Confirm)) => {
                        send(link, &mut channel, &Message::Confirm)?;
                        last_sent = Instant::now();
                        State::Open(channel)
                    }
                    // Not the same keys, or not the protocol: drop the
                    // channel and wait for the phone to start again.
                    _ => State::Idle,
                }
            }
            (Frame::Sealed(sealed), State::Open(mut channel)) => {
                match channel
                    .open(&sealed)
                    .map(|plain| Message::from_bytes(&plain))
                {
                    Ok(Ok(Message::Request { id, method, params })) => {
                        let reply = match answer(&method, &params) {
                            Ok(result) => Message::Response {
                                id,
                                result: Some(result),
                                error: None,
                            },
                            Err(error) => Message::Response {
                                id,
                                result: None,
                                error: Some(error),
                            },
                        };
                        send(link, &mut channel, &reply)?;
                        last_sent = Instant::now();
                        State::Open(channel)
                    }
                    // Anything else the phone may say is for the pairing
                    // step, and out of place here; the channel stays.
                    Ok(Ok(_)) => State::Open(channel),
                    // A frame that does not open ends the channel: its
                    // counters can no longer agree.
                    _ => State::Idle,
                }
            }
            // A sealed frame with no channel, or a pairing request on a
            // room that is already paired.
            (_, state) => state,
        };
    }
}

fn send(link: &mut impl Link, channel: &mut Channel, message: &Message) -> io::Result<()> {
    let sealed = channel
        .seal(&message.to_bytes())
        .map_err(|e| io::Error::other(e.to_string()))?;
    link.send(&Frame::Sealed(sealed))
}

/// How often `serve` looks for phones paired since it started.
const RESCAN: Duration = Duration::from_secs(10);

/// `splitlane remote serve`: answer every paired phone until stopped,
/// taking up phones paired while it runs.
pub(crate) fn serve() -> Result<(), String> {
    let dir = store::dir().ok_or("cannot work out where Splitlane keeps its files")?;
    let key =
        std::sync::Arc::new(store::machine_key(&dir).map_err(|e| format!("machine key: {e}"))?);
    let relay = relay::relay_url();
    let mut served: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        let devices = store::devices(&dir).map_err(|e| format!("paired phones: {e}"))?;
        for device in devices {
            if served.contains(&device.room) {
                continue;
            }
            let (Some(room), Some(phone)) = (device.room_id(), device.public_key()) else {
                log::warn!(
                    "skipping a paired phone whose record does not parse: {}",
                    device.name
                );
                continue;
            };
            served.insert(device.room.clone());
            let (key, relay, name) = (key.clone(), relay.clone(), device.name.clone());
            eprintln!("serving {name}");
            std::thread::spawn(move || serve_forever(&relay, &key, &phone, room, &name));
        }
        if served.is_empty() {
            return Err("no phone is paired; pair one with `splitlane remote pair`".into());
        }
        std::thread::sleep(RESCAN);
    }
}

/// Keep one phone's room joined, joining again after every drop. A panic
/// while answering is logged and the room joined again: one bad request
/// must not leave the phone unanswered until a restart.
fn serve_forever(relay: &str, key: &DeviceKey, phone: &PublicKey, room: RoomId, name: &str) {
    let mut retry = RETRY_FIRST;
    loop {
        match RelaySocket::connect(relay, room) {
            Ok(mut socket) => {
                retry = RETRY_FIRST;
                let mut answer = |method: &str, params: &Value| {
                    surface::Surface::current()
                        .and_then(|current| surface::answer(&current, method, params))
                };
                let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_connection(&mut socket, key, phone, room, &mut answer)
                }));
                match served {
                    Ok(Err(e)) => log::info!("{name}: the relay connection ended ({e})"),
                    Ok(Ok(())) => {}
                    Err(_) => log::error!("{name}: answering a request panicked; joining again"),
                }
            }
            Err(e) => log::info!("{name}: cannot reach the relay ({e})"),
        }
        std::thread::sleep(retry);
        retry = (retry * 2).min(RETRY_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::testing;
    use serde_json::json;

    fn hello(link: &mut testing::QueueLink) -> splitlane_remote::Salt {
        match link.receive(Duration::from_secs(5)).unwrap() {
            Received::Frame(Frame::Hello(salt)) => Some(salt),
            _ => None,
        }
        .expect("a hello from the machine")
    }

    /// The phone's side, as the page will do it.
    struct Phone {
        link: testing::QueueLink,
        channel: Channel,
    }

    impl Phone {
        fn connect(
            mut link: testing::QueueLink,
            key: &DeviceKey,
            machine: &PublicKey,
            room: RoomId,
        ) -> Self {
            let handshake = Handshake::start(Role::Phone);
            link.send(&Frame::Hello(handshake.salt())).unwrap();
            let machine_salt = hello(&mut link);
            let mut channel = handshake.finish(key, machine, room, machine_salt);
            let confirm = channel.seal(&Message::Confirm.to_bytes()).unwrap();
            link.send(&Frame::Sealed(confirm)).unwrap();
            let mut phone = Self { link, channel };
            assert_eq!(phone.next(), Message::Confirm);
            phone
        }

        fn next(&mut self) -> Message {
            let sealed = match self.link.receive(Duration::from_secs(5)).unwrap() {
                Received::Frame(Frame::Sealed(sealed)) => Some(sealed),
                _ => None,
            }
            .expect("a sealed frame from the machine");
            Message::from_bytes(&self.channel.open(&sealed).unwrap()).unwrap()
        }

        fn ask(&mut self, id: u64, method: &str, params: Value) -> Message {
            let request = Message::Request {
                id,
                method: method.into(),
                params,
            };
            let sealed = self.channel.seal(&request.to_bytes()).unwrap();
            self.link.send(&Frame::Sealed(sealed)).unwrap();
            self.next()
        }
    }

    fn start_bridge(
        machine: DeviceKey,
        phone: PublicKey,
        room: RoomId,
    ) -> (testing::QueueLink, std::thread::JoinHandle<io::Result<()>>) {
        let (mut bridge_end, phone_end) = testing::pair();
        let thread = std::thread::spawn(move || {
            let mut answer = |method: &str, params: &Value| match method {
                "list" => Ok(json!({ "agents": [{ "tool": "claude" }] })),
                _ => Err(format!("no {method} here {params}")),
            };
            serve_connection(&mut bridge_end, &machine, &phone, room, &mut answer)
        });
        (phone_end, thread)
    }

    #[test]
    fn a_paired_phone_asks_and_is_answered() {
        let (machine, phone_key) = (DeviceKey::generate(), DeviceKey::generate());
        let machine_public = machine.public();
        let room = RoomId::random();
        let (link, _bridge) = start_bridge(machine, phone_key.public(), room);
        let mut phone = Phone::connect(link, &phone_key, &machine_public, room);
        assert_eq!(
            phone.ask(1, "list", Value::Null),
            Message::Response {
                id: 1,
                result: Some(json!({ "agents": [{ "tool": "claude" }] })),
                error: None
            }
        );
        assert!(
            matches!(
                phone.ask(2, "send", Value::Null),
                Message::Response {
                    id: 2,
                    error: Some(_),
                    ..
                }
            ),
            "an unknown request is answered with an error"
        );
    }

    #[test]
    fn a_stranger_in_the_room_gets_nothing() {
        let machine = DeviceKey::generate();
        let machine_public = machine.public();
        let room = RoomId::random();
        let paired = DeviceKey::generate().public();
        let (mut link, _bridge) = start_bridge(machine, paired, room);
        // Someone who knows the room but is not the paired phone.
        let stranger = DeviceKey::generate();
        let handshake = Handshake::start(Role::Phone);
        link.send(&Frame::Hello(handshake.salt())).unwrap();
        let machine_salt = hello(&mut link);
        let mut channel = handshake.finish(&stranger, &machine_public, room, machine_salt);
        let confirm = channel.seal(&Message::Confirm.to_bytes()).unwrap();
        link.send(&Frame::Sealed(confirm)).unwrap();
        let request = Message::Request {
            id: 1,
            method: "list".into(),
            params: Value::Null,
        };
        link.send(&Frame::Sealed(channel.seal(&request.to_bytes()).unwrap()))
            .unwrap();
        // The machine's keys differ from the stranger's, so it neither
        // confirms nor answers.
        assert!(matches!(
            link.receive(Duration::from_millis(500)).unwrap(),
            Received::Quiet
        ));
    }

    #[test]
    fn a_phone_that_connects_again_gets_a_new_channel() {
        let (machine, phone_key) = (DeviceKey::generate(), DeviceKey::generate());
        let machine_public = machine.public();
        let room = RoomId::random();
        let (link, _bridge) = start_bridge(machine, phone_key.public(), room);
        let mut first = Phone::connect(link, &phone_key, &machine_public, room);
        first.ask(1, "list", Value::Null);
        // Same link, as the relay keeps the machine's side: a new hello.
        let mut again = Phone::connect(first.link, &phone_key, &machine_public, room);
        assert!(matches!(
            again.ask(1, "list", Value::Null),
            Message::Response {
                id: 1,
                result: Some(_),
                ..
            }
        ));
    }
}
