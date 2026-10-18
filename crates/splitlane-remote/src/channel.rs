//! One connection's encrypted channel between the machine and a phone.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::keys::{DeviceKey, PublicKey};
use crate::pairing::RoomId;
use crate::{put, Error};

pub const SALT_LEN: usize = 16;

/// Bytes of counter in front of each frame.
const COUNTER_LEN: usize = 8;

/// What each side sends in the clear when a connection starts. A side's
/// own salt is made by [`Handshake::start`] and nowhere else, so it is
/// fresh on every connection; see the crate's documentation for why that
/// matters. The peer's arrives as bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Salt(pub [u8; SALT_LEN]);

impl Salt {
    fn random() -> Self {
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        Self(salt)
    }
}

/// One side's half of a connection's start: its own fresh salt, to send,
/// and spent by [`Handshake::finish`], which takes the handshake by value.
/// A salt can therefore never be used for two channels, which is what
/// keeps a key from ever meeting a nonce twice.
pub struct Handshake {
    role: Role,
    salt: Salt,
}

impl Handshake {
    pub fn start(role: Role) -> Self {
        Self {
            role,
            salt: Salt::random(),
        }
    }

    /// For the fixed inputs of the test vectors only.
    #[cfg(feature = "test-vectors")]
    #[doc(hidden)]
    pub fn with_salt(role: Role, salt: Salt) -> Self {
        Self { role, salt }
    }

    /// The salt to send to the peer.
    pub fn salt(&self) -> Salt {
        self.salt
    }

    /// The channel, once the peer's salt has arrived.
    pub fn finish(
        self,
        me: &DeviceKey,
        peer: &PublicKey,
        room: RoomId,
        peer_salt: Salt,
    ) -> Channel {
        let (machine_salt, phone_salt) = match self.role {
            Role::Machine => (self.salt, peer_salt),
            Role::Phone => (peer_salt, self.salt),
        };
        Channel::new(self.role, me, peer, room, machine_salt, phone_salt)
    }
}

/// Which end of the channel this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Machine,
    Phone,
}

/// The channel for one connection: a key and a counter for each direction,
/// and the transcript hash every frame is bound to.
pub struct Channel {
    sending: Aes256Gcm,
    receiving: Aes256Gcm,
    sent: u64,
    received: u64,
    transcript: [u8; 32],
}

impl Channel {
    /// The channel `me` has with `peer` in `room`, given both salts of this
    /// connection. Both sides build the same transcript whichever they are.
    fn new(
        role: Role,
        me: &DeviceKey,
        peer: &PublicKey,
        room: RoomId,
        machine_salt: Salt,
        phone_salt: Salt,
    ) -> Self {
        let mine = me.public();
        let (machine, phone) = match role {
            Role::Machine => (&mine, peer),
            Role::Phone => (peer, &mine),
        };
        let mut transcript = Vec::new();
        put(&mut transcript, b"splitlane-session-v1");
        put(&mut transcript, &room.0);
        put(&mut transcript, machine.as_bytes());
        put(&mut transcript, phone.as_bytes());
        put(&mut transcript, &machine_salt.0);
        put(&mut transcript, &phone_salt.0);
        let transcript: [u8; 32] = Sha256::digest(&transcript).into();

        let shared = me.shared(peer);
        let hkdf = Hkdf::<Sha256>::new(Some(&transcript), &*shared);
        let key = |direction: &[u8]| {
            let mut info = Vec::new();
            put(&mut info, b"splitlane-key-v1");
            put(&mut info, direction);
            let mut key = zeroize::Zeroizing::new([0u8; 32]);
            // 32 bytes are far below HKDF's limit, so expanding cannot fail.
            let _ = hkdf.expand(&info, &mut *key);
            Aes256Gcm::new((&*key).into())
        };
        let (to_phone, to_machine) = (key(b"machine-to-phone"), key(b"phone-to-machine"));
        let (sending, receiving) = match role {
            Role::Machine => (to_phone, to_machine),
            Role::Phone => (to_machine, to_phone),
        };
        Self {
            sending,
            receiving,
            sent: 0,
            received: 0,
            transcript,
        }
    }

    /// Encrypt one frame: its counter, then the ciphertext and tag.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let counter = self.sent;
        self.sent = counter.checked_add(1).ok_or(Error::Exhausted)?;
        let ciphertext = self
            .sending
            .encrypt(
                &nonce(counter),
                Payload {
                    msg: plaintext,
                    aad: &self.transcript,
                },
            )
            .map_err(|_| Error::Cipher)?;
        let mut frame = Vec::with_capacity(COUNTER_LEN + ciphertext.len());
        frame.extend_from_slice(&counter.to_be_bytes());
        frame.extend_from_slice(&ciphertext);
        Ok(frame)
    }

    /// Decrypt the next frame. One out of order, seen before, cut short or
    /// changed is refused, and the channel is then no use: the caller
    /// closes the connection.
    pub fn open(&mut self, frame: &[u8]) -> Result<Vec<u8>, Error> {
        let (counter, ciphertext) = frame
            .split_first_chunk::<COUNTER_LEN>()
            .ok_or(Error::Unopened)?;
        let counter = u64::from_be_bytes(*counter);
        if counter != self.received {
            return Err(Error::Replayed);
        }
        let plaintext = self
            .receiving
            .decrypt(
                &nonce(counter),
                Payload {
                    msg: ciphertext,
                    aad: &self.transcript,
                },
            )
            .map_err(|_| Error::Unopened)?;
        self.received = counter.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(plaintext)
    }
}

/// Four zero bytes, then the counter: unique under one key, which is all
/// GCM needs, because every connection has keys of its own.
fn nonce(counter: u64) -> Nonce<aes_gcm::aead::consts::U12> {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Pair {
        machine: Channel,
        phone: Channel,
    }

    fn pair_with(machine_salt: Salt, phone_salt: Salt) -> (Pair, DeviceKey, DeviceKey, RoomId) {
        let (machine_key, phone_key) = (DeviceKey::generate(), DeviceKey::generate());
        let room = RoomId::random();
        let pair = Pair {
            machine: Channel::new(
                Role::Machine,
                &machine_key,
                &phone_key.public(),
                room,
                machine_salt,
                phone_salt,
            ),
            phone: Channel::new(
                Role::Phone,
                &phone_key,
                &machine_key.public(),
                room,
                machine_salt,
                phone_salt,
            ),
        };
        (pair, machine_key, phone_key, room)
    }

    fn pair() -> Pair {
        pair_with(Salt::random(), Salt::random()).0
    }

    #[test]
    fn frames_go_both_ways() {
        let mut pair = pair();
        let frame = pair.machine.seal(b"confirm").unwrap();
        assert_eq!(pair.phone.open(&frame).unwrap(), b"confirm");
        let frame = pair.phone.seal(b"confirm").unwrap();
        assert_eq!(pair.machine.open(&frame).unwrap(), b"confirm");
        for n in 0..3u8 {
            let frame = pair.phone.seal(&[n]).unwrap();
            assert_eq!(pair.machine.open(&frame).unwrap(), [n]);
        }
    }

    #[test]
    fn a_frame_is_not_taken_twice_or_out_of_order() {
        let mut pair = pair();
        let first = pair.machine.seal(b"one").unwrap();
        let second = pair.machine.seal(b"two").unwrap();
        assert_eq!(pair.phone.open(&second), Err(Error::Replayed));
        assert_eq!(pair.phone.open(&first).unwrap(), b"one");
        assert_eq!(pair.phone.open(&first), Err(Error::Replayed));
        assert_eq!(pair.phone.open(&second).unwrap(), b"two");
    }

    #[test]
    fn a_changed_or_cut_frame_does_not_open() {
        let mut pair = pair();
        let mut frame = pair.machine.seal(b"answer").unwrap();
        let last = frame.len() - 1;
        frame[last] ^= 1;
        assert_eq!(pair.phone.open(&frame), Err(Error::Unopened));
        assert_eq!(pair.phone.open(&[0, 0, 0]), Err(Error::Unopened));
    }

    #[test]
    fn a_frame_sent_one_way_does_not_open_the_other() {
        let mut pair = pair();
        let frame = pair.machine.seal(b"to the phone").unwrap();
        assert_eq!(pair.machine.open(&frame), Err(Error::Unopened));
    }

    /// The reason for the salts: a frame recorded on one connection, with
    /// the same keys and the same counter, does not open on the next.
    #[test]
    fn a_frame_from_an_earlier_connection_does_not_open() {
        let (mut first, machine_key, phone_key, room) = pair_with(Salt::random(), Salt::random());
        let recorded = first.machine.seal(b"run this").unwrap();
        let mut next = Channel::new(
            Role::Phone,
            &phone_key,
            &machine_key.public(),
            room,
            Salt::random(),
            Salt::random(),
        );
        assert_eq!(next.open(&recorded), Err(Error::Unopened));
    }

    #[test]
    fn sides_that_disagree_on_the_transcript_cannot_talk() {
        let (machine_key, phone_key) = (DeviceKey::generate(), DeviceKey::generate());
        let room = RoomId::random();
        let salt = Salt::random();
        let mut machine = Channel::new(
            Role::Machine,
            &machine_key,
            &phone_key.public(),
            room,
            salt,
            salt,
        );
        let mut phone = Channel::new(
            Role::Phone,
            &phone_key,
            &machine_key.public(),
            RoomId::random(),
            salt,
            salt,
        );
        let frame = machine.seal(b"confirm").unwrap();
        assert_eq!(phone.open(&frame), Err(Error::Unopened));
    }
}
