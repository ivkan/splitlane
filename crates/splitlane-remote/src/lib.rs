//! Pairing a phone with Splitlane, and the channel between them.
//!
//! The phone and the machine never talk directly. Both keep an outgoing
//! connection to a relay, which forwards frames between them and is trusted
//! with nothing: it sees which room a frame is for, how long it is and when
//! it was sent. Everything else is end to end, between two long-lived P-256
//! keys, one on the machine and one on the phone.
//!
//! Every primitive is one WebCrypto offers in every browser, so the page on
//! the phone carries no cryptographic code of its own: P-256 ECDH, HKDF and
//! HMAC over SHA-256, AES-256-GCM. Public keys travel as uncompressed SEC1
//! points (what `exportKey("raw")` gives) and the ECDH secret is the shared
//! point's x coordinate (what `deriveBits(…, 256)` gives).
//!
//! # Pairing
//!
//! 1. The machine shows an [`Offer`] as a link in a QR code: a fresh room,
//!    the machine's public key, a one-time secret and an expiry. The link's
//!    data is in its fragment, which a browser never sends anywhere.
//! 2. The phone makes its key pair and sends, through the room, its public
//!    key and a name with [`pairing_mac`] over both keys, the room and the
//!    name. Both sides check the expiry.
//! 3. Both sides derive a six-digit [`confirmation_code`] from the shared
//!    secret. The phone shows it and the person types it on the machine.
//!    Only then does the machine keep the phone's key. The code is what a
//!    photograph of the QR code does not give: whoever paired with it would
//!    see a code on their own screen, not on the person's phone.
//!
//! # Each connection
//!
//! Both sides send a fresh random [`Salt`] in the clear: each makes its own
//! in [`Handshake::start`], and [`Handshake::finish`] spends it. Each
//! direction gets its own AES-256-GCM key, derived from the shared secret
//! and a hash of everything the connection agreed on (the transcript:
//! room, both keys, both salts), and every frame carries that hash as
//! associated data. A
//! frame's nonce is a counter from zero, and a frame is taken only with the
//! next number. Because each side's salt is new on every connection, the
//! keys are too: a counter that starts again at zero never meets a key it
//! was used with, and a frame recorded on an earlier connection does not
//! open on this one.
//!
//! The first frame each side sends is a confirmation. Opening it proves the
//! other side derived the same keys from the same transcript, so a mistake
//! anywhere above fails at once and not as silence later.
//!
//! There is no forward secrecy: whoever later steals either long-lived key
//! can read traffic recorded before. Getting it would take ephemeral keys
//! per connection signed by the long-lived ones, and that is left until
//! there is a reason.

mod channel;
mod keys;
mod pairing;

pub use channel::{Channel, Handshake, Role, Salt, SALT_LEN};
pub use keys::{DeviceKey, PublicKey, PUBLIC_KEY_LEN};
pub use pairing::{
    confirmation_code, pairing_mac, verify_pairing, Offer, RoomId, MAX_NAME_LEN, ROOM_LEN,
    SECRET_LEN,
};

/// The version of everything in this crate, carried in an offer's link.
pub const VERSION: u8 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not a P-256 public key")]
    BadKey,
    #[error("not a pairing link this version understands")]
    BadLink,
    #[error("the pairing offer has expired")]
    Expired,
    #[error("the pairing request does not match the offer")]
    BadMac,
    #[error("the device name is empty or longer than {MAX_NAME_LEN} bytes")]
    BadName,
    #[error("a frame arrived out of order or again")]
    Replayed,
    #[error("a frame did not open: wrong keys, or it was changed on the way")]
    Unopened,
    #[error("the channel has sent as many frames as its counter can number")]
    Exhausted,
    #[error("the cipher refused to seal a frame")]
    Cipher,
}

/// Append `field` to `out` with its length in front, so that no two
/// different lists of fields hash or MAC the same.
fn put(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u32).to_be_bytes());
    out.extend_from_slice(field);
}
