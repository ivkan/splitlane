//! The machine's connection to its room on the relay.
//!
//! One WebSocket to `<relay>/room/<room>?role=machine`, binary frames each
//! way. The relay forwards them to the phone in the same room and reads
//! nothing in them; see `splitlane_remote::wire`.

use std::io;
use std::net::TcpStream;
use std::time::Duration;

use splitlane_remote::RoomId;
use splitlane_remote::wire::{Frame, MAX_FRAME_LEN};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// The relay unless `SPLITLANE_RELAY_URL` names another, as a test does.
pub(crate) const DEFAULT_RELAY: &str = "wss://relay.splitlane.dev";

pub(crate) fn relay_url() -> String {
    std::env::var("SPLITLANE_RELAY_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_RELAY.to_string())
}

pub(crate) struct RelaySocket {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

/// What a wait for a frame came to.
pub(crate) enum Received {
    Frame(Frame),
    /// Nothing arrived within the wait.
    Quiet,
    /// The peer sent something that is not a frame; it is dropped.
    Junk,
}

impl RelaySocket {
    /// Join `room` as the machine.
    pub(crate) fn connect(relay: &str, room: RoomId) -> io::Result<Self> {
        let url = format!(
            "{}/room/{}?role=machine",
            relay.trim_end_matches('/'),
            room.to_text()
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_LEN + 1))
            .max_frame_size(Some(MAX_FRAME_LEN + 1));
        let (socket, _) = tungstenite::client::connect_with_config(url.as_str(), Some(config), 3)
            .map_err(io::Error::other)?;
        Ok(Self { socket })
    }

    pub(crate) fn send(&mut self, frame: &Frame) -> io::Result<()> {
        self.socket
            .send(Message::Binary(frame.encode().into()))
            .map_err(io::Error::other)
    }

    /// Wait up to `wait` for the next frame. Pings are answered on the way.
    pub(crate) fn receive(&mut self, wait: Duration) -> io::Result<Received> {
        self.set_read_timeout(Some(wait))?;
        loop {
            match self.socket.read() {
                Ok(Message::Binary(bytes)) => {
                    return Ok(match Frame::decode(&bytes) {
                        Ok(frame) => Received::Frame(frame),
                        Err(_) => Received::Junk,
                    });
                }
                Ok(Message::Close(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "the relay closed the connection",
                    ));
                }
                // Pings are answered by the library as it reads; text is
                // not part of the protocol.
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(Received::Quiet);
                }
                Err(e) => return Err(io::Error::other(e)),
            }
        }
    }

    /// Keep the connection alive through proxies and the relay's idle
    /// handling.
    pub(crate) fn ping(&mut self) -> io::Result<()> {
        self.socket
            .send(Message::Ping(Vec::new().into()))
            .map_err(io::Error::other)
    }

    fn set_read_timeout(&mut self, wait: Option<Duration>) -> io::Result<()> {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_read_timeout(wait),
            MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(wait),
            // Without a timeout a quiet relay would block the bridge for
            // ever: no keep-alive, no expiry checked.
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a relay connection of a kind this build cannot time out",
            )),
        }
    }
}
