//! What travels through the relay, and what travels inside the channel.
//!
//! Each WebSocket message is one binary frame. Its first byte says what it
//! is:
//!
//! - [`KIND_PAIR`]: a pairing request, JSON in the clear (the phone's key,
//!   its name and the MAC; see [`crate::pairing_mac`]).
//! - [`KIND_HELLO`]: a connection's start, one side's [`crate::Salt`].
//! - [`KIND_SEALED`]: a frame of the channel; see [`crate::Channel`].
//!
//! A connection always starts with the phone: it sends its hello, the
//! machine answers with its own, and each side's first sealed frame is
//! [`Message::Confirm`]. A phone that connects again sends a new hello and
//! gets a new channel; the machine never speaks first, because a relay
//! drops what arrives for a peer that is not there.
//!
//! Inside the channel, every frame is one JSON [`Message`].

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::channel::{Salt, SALT_LEN};
use crate::keys::PublicKey;
use crate::Error;

pub const KIND_PAIR: u8 = 1;
pub const KIND_HELLO: u8 = 2;
pub const KIND_SEALED: u8 = 3;

/// Largest frame either side sends or takes, and the relay's bound too.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// One relay frame, parsed.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Pair(PairRequest),
    Hello(Salt),
    Sealed(Vec<u8>),
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Pair(request) => {
                let mut out = vec![KIND_PAIR];
                #[allow(clippy::expect_used)] // A struct of strings always serialises.
                out.extend_from_slice(
                    &serde_json::to_vec(&request.to_wire()).expect("strings serialise"),
                );
                out
            }
            Frame::Hello(salt) => {
                let mut out = vec![KIND_HELLO];
                out.extend_from_slice(&salt.0);
                out
            }
            Frame::Sealed(frame) => {
                let mut out = Vec::with_capacity(frame.len() + 1);
                out.push(KIND_SEALED);
                out.extend_from_slice(frame);
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_FRAME_LEN {
            return Err(Error::BadFrame);
        }
        let (&kind, body) = bytes.split_first().ok_or(Error::BadFrame)?;
        match kind {
            KIND_PAIR => {
                let wire: PairWire = serde_json::from_slice(body).map_err(|_| Error::BadFrame)?;
                Ok(Frame::Pair(PairRequest::from_wire(&wire)?))
            }
            KIND_HELLO => {
                let salt: [u8; SALT_LEN] = body.try_into().map_err(|_| Error::BadFrame)?;
                Ok(Frame::Hello(Salt(salt)))
            }
            KIND_SEALED => Ok(Frame::Sealed(body.to_vec())),
            _ => Err(Error::BadFrame),
        }
    }
}

/// The phone's answer to an offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRequest {
    pub phone: PublicKey,
    pub name: String,
    pub mac: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct PairWire {
    phone: String,
    name: String,
    mac: String,
}

impl PairRequest {
    fn to_wire(&self) -> PairWire {
        PairWire {
            phone: URL_SAFE_NO_PAD.encode(self.phone.as_bytes()),
            name: self.name.clone(),
            mac: URL_SAFE_NO_PAD.encode(self.mac),
        }
    }

    fn from_wire(wire: &PairWire) -> Result<Self, Error> {
        let bytes = |text: &str| URL_SAFE_NO_PAD.decode(text).map_err(|_| Error::BadFrame);
        Ok(Self {
            phone: PublicKey::from_bytes(&bytes(&wire.phone)?)?,
            name: wire.name.clone(),
            mac: bytes(&wire.mac)?.try_into().map_err(|_| Error::BadFrame)?,
        })
    }
}

/// One message inside the channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Each side's first frame; opening it proves both derived one key.
    Confirm,
    /// The machine kept the phone's key: the person typed its code.
    Paired,
    /// The machine did not keep it, and why, for the phone to show.
    Refused { reason: String },
    /// A question from the phone. `id` comes back on its answer.
    Request {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// The answer to a request: `result`, or `error` with words for a person.
    Response {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Something changed on the machine that the phone did not ask about.
    Event {
        name: String,
        #[serde(default)]
        data: Value,
    },
}

impl Message {
    pub fn to_bytes(&self) -> Vec<u8> {
        #[allow(clippy::expect_used)] // Every variant is plain data.
        serde_json::to_vec(self).expect("plain data serialises")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        serde_json::from_slice(bytes).map_err(|_| Error::BadFrame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::DeviceKey;

    #[test]
    fn frames_survive_the_wire() {
        let request = PairRequest {
            phone: DeviceKey::generate().public(),
            name: "Pixel".into(),
            mac: [7; 32],
        };
        for frame in [
            Frame::Pair(request),
            Frame::Hello(Salt([9; SALT_LEN])),
            Frame::Sealed(vec![1, 2, 3]),
        ] {
            assert_eq!(Frame::decode(&frame.encode()), Ok(frame));
        }
    }

    #[test]
    fn what_is_not_a_frame_is_refused() {
        for bad in [&[][..], &[0], &[KIND_HELLO, 1, 2], &[KIND_PAIR, b'{']] {
            assert_eq!(Frame::decode(bad), Err(Error::BadFrame), "{bad:?}");
        }
        let huge = vec![KIND_SEALED; MAX_FRAME_LEN + 1];
        assert_eq!(Frame::decode(&huge), Err(Error::BadFrame));
    }

    #[test]
    fn messages_read_as_the_page_writes_them() {
        let request: Message =
            serde_json::from_str(r#"{"type":"request","id":3,"method":"list"}"#).unwrap();
        assert_eq!(
            request,
            Message::Request {
                id: 3,
                method: "list".into(),
                params: Value::Null
            }
        );
        let answer = Message::Response {
            id: 3,
            result: Some(serde_json::json!({"agents": []})),
            error: None,
        };
        assert_eq!(
            String::from_utf8(answer.to_bytes()).unwrap(),
            r#"{"type":"response","id":3,"result":{"agents":[]}}"#
        );
        assert_eq!(
            Message::from_bytes(&Message::Confirm.to_bytes()),
            Ok(Message::Confirm)
        );
    }
}
