//! The machine's connection to its room on the relay.
//!
//! One WebSocket to `<relay>/room/<room>?role=machine`, binary frames each
//! way, joined with the room's token in a header (a URL ends up in logs). The relay forwards them to the phone in the same room and reads
//! nothing in them; see `splitlane_remote::wire`.

use std::io;
use std::net::{TcpStream, ToSocketAddrs as _};
use std::time::Duration;

use splitlane_remote::wire::{Frame, MAX_FRAME_LEN};
use splitlane_remote::{DeviceKey, RoomId};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// The header the machine's join token travels in; the relay's
/// `JOIN_HEADER` reads the same name.
const JOIN_HEADER: &str = "x-splitlane-join";

/// How long a close waits for the relay's answer.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// How long one write may wait on the network. A connection whose far end
/// is gone stops taking bytes once the kernel's buffer is full, and without
/// a limit the write would hold the bridge for as long as the kernel keeps
/// retrying.
const WRITE_WAIT: Duration = Duration::from_secs(15);
/// How long joining may take: the TCP connection to each address, then the
/// TLS and WebSocket handshakes. A relay address that swallows packets, or
/// a server that accepts and never answers, would otherwise hold the bridge
/// in a join it never comes back from.
const JOIN_WAIT: Duration = Duration::from_secs(15);

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
    /// The relay sent something that is not for the bridge: a pong, as a
    /// rule. It carries nothing, but it says the connection still works.
    Alive,
}

impl RelaySocket {
    /// Join `room` as the machine whose key is `machine`.
    pub(crate) fn connect(relay: &str, room: RoomId, machine: &DeviceKey) -> io::Result<Self> {
        let url = format!(
            "{}/room/{}?role=machine",
            relay.trim_end_matches('/'),
            room.to_text()
        );
        let mut request = url.into_client_request().map_err(io::Error::other)?;
        let token = room.join_token(machine);
        request.headers_mut().insert(
            JOIN_HEADER,
            token
                .parse()
                .map_err(|_| io::Error::other("a join token that is not a header value"))?,
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_LEN + 1))
            .max_frame_size(Some(MAX_FRAME_LEN + 1));
        let stream = connect_tcp(request.uri())?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(JOIN_WAIT))?;
        stream.set_write_timeout(Some(JOIN_WAIT))?;
        let (socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut socket = Self { socket };
        socket.set_write_timeout(Some(WRITE_WAIT))?;
        Ok(socket)
    }

    pub(crate) fn send(&mut self, frame: &Frame) -> io::Result<()> {
        self.socket
            .send(Message::Binary(frame.encode().into()))
            .map_err(io::Error::other)
    }

    /// Wait up to `wait` for the next frame. Pings are answered on the way.
    pub(crate) fn receive(&mut self, wait: Duration) -> io::Result<Received> {
        self.set_read_timeout(Some(wait))?;
        match self.socket.read() {
            Ok(Message::Binary(bytes)) => Ok(match Frame::decode(&bytes) {
                Ok(frame) => Received::Frame(frame),
                Err(_) => Received::Junk,
            }),
            Ok(Message::Close(_)) => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "the relay closed the connection",
            )),
            // Pings are answered by the library as it reads; text is not
            // part of the protocol. Either way the relay is there.
            Ok(_) => Ok(Received::Alive),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(Received::Quiet)
            }
            Err(e) => Err(io::Error::other(e)),
        }
    }

    /// End the connection with a close handshake, so that the last frame
    /// sent reaches the relay. A socket that is just dropped with unread
    /// data on it is reset, and a reset can take the frame sent just
    /// before it along: a phone told nothing after the computer kept it.
    pub(crate) fn close(mut self) {
        let _ = self.socket.close(None);
        let _ = self.set_read_timeout(Some(CLOSE_WAIT));
        let deadline = std::time::Instant::now() + CLOSE_WAIT;
        // Read until the relay's own close comes back, which ends the read
        // with an error.
        while std::time::Instant::now() < deadline && self.socket.read().is_ok() {}
    }

    /// Ask the relay for a pong, which comes back as [`Received::Alive`].
    /// Cloudflare answers it for the room without waking it; a connection
    /// that gets no answer is no longer one the phone can reach the machine
    /// through. See `bridge::Liveness`.
    pub(crate) fn ping(&mut self) -> io::Result<()> {
        self.socket
            .send(Message::Ping(Vec::new().into()))
            .map_err(io::Error::other)
    }

    fn set_read_timeout(&mut self, wait: Option<Duration>) -> io::Result<()> {
        self.tcp().and_then(|stream| stream.set_read_timeout(wait))
    }

    fn set_write_timeout(&mut self, wait: Option<Duration>) -> io::Result<()> {
        self.tcp().and_then(|stream| stream.set_write_timeout(wait))
    }

    fn tcp(&mut self) -> io::Result<&mut TcpStream> {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => Ok(stream),
            MaybeTlsStream::Rustls(stream) => Ok(&mut stream.sock),
            // Without a timeout a quiet relay would block the bridge for
            // ever: no keep-alive, no expiry checked.
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a relay connection of a kind this build cannot time out",
            )),
        }
    }
}

/// Open the TCP connection under the relay's URL, trying each address it
/// resolves to for at most [`JOIN_WAIT`].
fn connect_tcp(uri: &tungstenite::http::Uri) -> io::Result<TcpStream> {
    let host = uri
        .host()
        .ok_or_else(|| io::Error::other("a relay URL without a host"))?;
    let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("wss") => 443,
        _ => 80,
    });
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let mut last = io::Error::other("the relay's name resolves to no address");
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, JOIN_WAIT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_pong_from_the_relay_comes_back_as_alive() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        // A relay that answers pings, as Cloudflare does for the room, and
        // sends nothing else.
        let relay = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut ws = tungstenite::accept(stream).expect("handshake");
            while ws.read().is_ok() {}
        });
        let room = RoomId::random();
        let mut socket =
            RelaySocket::connect(&format!("ws://{addr}"), room, &DeviceKey::generate())
                .expect("connect");
        assert!(matches!(
            socket.receive(Duration::from_millis(200)).expect("quiet"),
            Received::Quiet
        ));
        socket.ping().expect("ping");
        assert!(matches!(
            socket.receive(Duration::from_secs(5)).expect("pong"),
            Received::Alive
        ));
        socket.close();
        relay.join().expect("relay thread");
    }
}
