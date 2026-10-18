//! `splitlane remote pair`: pairing a phone from the terminal.

use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use splitlane_remote::wire::{Frame, Message, PairRequest};
use splitlane_remote::{
    Channel, DeviceKey, Handshake, Offer, Role, code_matches, confirmation_code, verify_pairing,
};

use super::relay::{self, Received, RelaySocket};
use super::{Link, now_unix, store};

/// How long an offer lasts.
const OFFER_TTL_SECS: u64 = 300;
/// Codes a person may type before the offer is dropped. A million codes is
/// few enough to try them all; three is enough for typos.
const CODE_ATTEMPTS: usize = 3;
/// Where a phone opens the link.
const PAGE: &str = "https://splitlane.dev/app";

/// How a pairing ended without a phone.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unpaired {
    Expired,
    /// The person did not type the phone's code.
    WrongCode,
    /// No code was typed: input closed.
    NoCode,
}

/// Pair over `link`: wait for a phone's request that matches `offer`, set up
/// the channel, and keep the phone only once `ask_code` returns the code
/// the phone shows. Returns the phone's request.
pub(crate) fn pair_on(
    link: &mut impl Link,
    key: &DeviceKey,
    offer: &Offer,
    now: &dyn Fn() -> u64,
    ask_code: &mut dyn FnMut(&str) -> Option<String>,
    keep: &mut dyn FnMut(&PairRequest) -> io::Result<()>,
) -> io::Result<Result<PairRequest, Unpaired>> {
    let deadline = Instant::now() + Duration::from_secs(offer.expires_unix.saturating_sub(now()));
    let mut request: Option<PairRequest> = None;
    let mut channel: Option<Channel> = None;
    loop {
        if Instant::now() >= deadline || offer.expired(now()) {
            return Ok(Err(Unpaired::Expired));
        }
        let Received::Frame(frame) = link.receive(Duration::from_secs(1))? else {
            continue;
        };
        match frame {
            Frame::Pair(asked)
                if verify_pairing(offer, &asked.phone, &asked.name, &asked.mac, now()).is_ok() =>
            {
                request = Some(asked);
                channel = None;
            }
            // A request that does not match, or one before any: nothing.
            Frame::Pair(_) => {}
            Frame::Hello(phone_salt) => {
                if let Some(asked) = &request {
                    let handshake = Handshake::start(Role::Machine);
                    link.send(&Frame::Hello(handshake.salt()))?;
                    channel = Some(handshake.finish(key, &asked.phone, offer.room, phone_salt));
                }
            }
            Frame::Sealed(sealed) => {
                let (Some(asked), Some(open)) = (&request, channel.as_mut()) else {
                    continue;
                };
                if !matches!(
                    open.open(&sealed).map(|plain| Message::from_bytes(&plain)),
                    Ok(Ok(Message::Confirm))
                ) {
                    channel = None;
                    continue;
                }
                if offer.expired(now()) {
                    send(link, open, &refused("the pairing link expired"))?;
                    return Ok(Err(Unpaired::Expired));
                }
                send(link, open, &Message::Confirm)?;
                let code = confirmation_code(key, &asked.phone, offer);
                for _ in 0..CODE_ATTEMPTS {
                    let Some(typed) = ask_code(&asked.name) else {
                        send(link, open, &refused("no code was entered"))?;
                        return Ok(Err(Unpaired::NoCode));
                    };
                    // A code typed after the link expired pairs nothing:
                    // the five minutes are the whole window.
                    if offer.expired(now()) {
                        send(link, open, &refused("the pairing link expired"))?;
                        return Ok(Err(Unpaired::Expired));
                    }
                    if code_matches(&typed, &code) {
                        // Kept before the phone is told, so a phone that
                        // hears `paired` is one this machine will answer.
                        if let Err(e) = keep(asked) {
                            send(
                                link,
                                open,
                                &refused("the computer could not save the pairing"),
                            )?;
                            return Err(e);
                        }
                        send(link, open, &Message::Paired)?;
                        return Ok(Ok(asked.clone()));
                    }
                }
                send(
                    link,
                    open,
                    &refused("the code typed on the computer did not match"),
                )?;
                return Ok(Err(Unpaired::WrongCode));
            }
        }
    }
}

fn refused(reason: &str) -> Message {
    Message::Refused {
        reason: reason.to_string(),
    }
}

fn send(link: &mut impl Link, channel: &mut Channel, message: &Message) -> io::Result<()> {
    let sealed = channel
        .seal(&message.to_bytes())
        .map_err(|e| io::Error::other(e.to_string()))?;
    link.send(&Frame::Sealed(sealed))
}

/// `splitlane remote pair`.
pub(crate) fn pair() -> Result<(), String> {
    let dir = store::dir().ok_or("cannot work out where Splitlane keeps its files")?;
    let key = store::machine_key(&dir).map_err(|e| format!("machine key: {e}"))?;
    let offer = Offer::new(&key.public(), now_unix(), OFFER_TTL_SECS);
    let mut socket = RelaySocket::connect(&relay::relay_url(), offer.room, &key)
        .map_err(|e| format!("cannot reach the relay: {e}"))?;
    println!(
        "Open this link on the phone within five minutes:\n\n  {}\n",
        offer.to_link(PAGE)
    );
    let stdin = io::stdin();
    let mut ask_code = |name: &str| {
        print!("Type the six digits {name} shows: ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    };
    let mut keep = |asked: &PairRequest| {
        store::add_device(
            &dir,
            store::Device::new(offer.room, &asked.name, &asked.phone, now_unix()),
        )
    };
    let asked = match pair_on(
        &mut socket,
        &key,
        &offer,
        &now_unix,
        &mut ask_code,
        &mut keep,
    )
    .map_err(|e| format!("pairing ended: {e}"))?
    {
        Ok(asked) => asked,
        Err(Unpaired::Expired) => return Err("the link expired before a phone answered it".into()),
        Err(Unpaired::WrongCode) => return Err("the code did not match; nothing was paired".into()),
        Err(Unpaired::NoCode) => return Err("no code was entered; nothing was paired".into()),
    };
    println!(
        "Paired {}. `splitlane remote serve` answers it.",
        asked.name
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::testing;
    use splitlane_remote::pairing_mac;

    const NOW: u64 = 1_800_000_000;

    fn received(link: &mut testing::QueueLink) -> Frame {
        match link.receive(Duration::from_secs(5)).unwrap() {
            Received::Frame(frame) => Some(frame),
            _ => None,
        }
        .expect("a frame from the machine")
    }

    /// The phone's half of pairing, up to the machine's verdict.
    fn phone_pairs(
        mut link: testing::QueueLink,
        offer: &Offer,
        phone: &DeviceKey,
    ) -> (String, Message) {
        let name = "Pixel";
        let request = PairRequest {
            phone: phone.public(),
            name: name.into(),
            mac: pairing_mac(offer, &phone.public(), name),
        };
        link.send(&Frame::Pair(request)).unwrap();
        let handshake = Handshake::start(Role::Phone);
        link.send(&Frame::Hello(handshake.salt())).unwrap();
        let Frame::Hello(machine_salt) = received(&mut link) else {
            return (String::new(), Message::Confirm);
        };
        let mut channel = handshake.finish(phone, &offer.machine, offer.room, machine_salt);
        link.send(&Frame::Sealed(
            channel.seal(&Message::Confirm.to_bytes()).unwrap(),
        ))
        .unwrap();
        let mut next = || match received(&mut link) {
            Frame::Sealed(sealed) => Message::from_bytes(&channel.open(&sealed).unwrap()).unwrap(),
            other => Message::Refused {
                reason: format!("{other:?}"),
            },
        };
        assert_eq!(next(), Message::Confirm);
        let code = confirmation_code(phone, &offer.machine, offer);
        (code, next())
    }

    fn run(typed: Vec<Option<String>>) -> (Result<PairRequest, Unpaired>, String, Message) {
        let machine = DeviceKey::generate();
        let offer = Offer::new(&machine.public(), NOW, 300);
        let phone = DeviceKey::generate();
        let (mut machine_end, phone_end) = testing::pair();
        let phone_offer = offer.clone();
        let phone_thread = std::thread::spawn(move || phone_pairs(phone_end, &phone_offer, &phone));
        let code_holder = std::sync::Arc::new(std::sync::Mutex::new(typed.into_iter()));
        let mut ask = |_: &str| code_holder.lock().unwrap().next().flatten();
        let outcome = pair_on(
            &mut machine_end,
            &machine,
            &offer,
            &|| NOW,
            &mut ask,
            &mut |_| Ok(()),
        )
        .unwrap();
        let (code, verdict) = phone_thread.join().unwrap();
        (outcome, code, verdict)
    }

    #[test]
    fn the_phone_is_kept_once_its_code_is_typed() {
        // The code is only known once the phone has it, so the test types
        // it from the machine's side of the same derivation.
        let machine = DeviceKey::generate();
        let offer = Offer::new(&machine.public(), NOW, 300);
        let phone = DeviceKey::generate();
        let code = confirmation_code(&machine, &phone.public(), &offer);
        let (mut machine_end, phone_end) = testing::pair();
        let phone_offer = offer.clone();
        let phone_thread = std::thread::spawn(move || phone_pairs(phone_end, &phone_offer, &phone));
        let mut ask = |name: &str| {
            assert_eq!(name, "Pixel");
            Some(format!("{} {}\n", &code[..3], &code[3..]))
        };
        let mut kept = Vec::new();
        let outcome = pair_on(
            &mut machine_end,
            &machine,
            &offer,
            &|| NOW,
            &mut ask,
            &mut |asked| {
                kept.push(asked.name.clone());
                Ok(())
            },
        )
        .unwrap();
        let (shown, verdict) = phone_thread.join().unwrap();
        assert_eq!(kept, ["Pixel"]);
        assert_eq!(shown, code);
        assert_eq!(verdict, Message::Paired);
        assert_eq!(outcome.unwrap().name, "Pixel");
    }

    #[test]
    fn three_wrong_codes_pair_nothing() {
        let wrong = Some("000000".to_string());
        let (outcome, _, verdict) = run(vec![wrong.clone(), wrong.clone(), wrong]);
        assert_eq!(outcome, Err(Unpaired::WrongCode));
        assert!(matches!(verdict, Message::Refused { .. }));
    }

    #[test]
    fn no_code_pairs_nothing() {
        let (outcome, _, verdict) = run(vec![None]);
        assert_eq!(outcome, Err(Unpaired::NoCode));
        assert!(matches!(verdict, Message::Refused { .. }));
    }

    #[test]
    fn a_pairing_that_cannot_be_saved_tells_the_phone_so() {
        let machine = DeviceKey::generate();
        let offer = Offer::new(&machine.public(), NOW, 300);
        let phone = DeviceKey::generate();
        let code = confirmation_code(&machine, &phone.public(), &offer);
        let (mut machine_end, phone_end) = testing::pair();
        let phone_offer = offer.clone();
        let phone_thread = std::thread::spawn(move || phone_pairs(phone_end, &phone_offer, &phone));
        let outcome = pair_on(
            &mut machine_end,
            &machine,
            &offer,
            &|| NOW,
            &mut |_| Some(code.clone()),
            &mut |_| Err(io::Error::other("disk full")),
        );
        assert!(outcome.is_err());
        let (_, verdict) = phone_thread.join().unwrap();
        assert!(matches!(verdict, Message::Refused { .. }), "{verdict:?}");
    }

    #[test]
    fn an_expired_offer_waits_for_nobody() {
        let machine = DeviceKey::generate();
        let offer = Offer::new(&machine.public(), NOW, 300);
        let (mut machine_end, _phone_end) = testing::pair();
        let outcome = pair_on(
            &mut machine_end,
            &machine,
            &offer,
            &|| NOW + 300,
            &mut |_| None,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(outcome, Err(Unpaired::Expired));
    }
}
