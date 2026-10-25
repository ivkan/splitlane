//! The long-lived keys of the machine and of each phone.

use p256::elliptic_curve::sec1::ToSec1Point;
use p256::elliptic_curve::Generate;
use zeroize::Zeroizing;

use crate::Error;

/// Length of an uncompressed SEC1 P-256 point.
pub const PUBLIC_KEY_LEN: usize = 65;

/// A public key: a point checked to be on the curve, and its uncompressed
/// SEC1 bytes.
#[derive(Clone)]
pub struct PublicKey {
    bytes: [u8; PUBLIC_KEY_LEN],
    point: p256::PublicKey,
}

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let point = p256::PublicKey::from_sec1_bytes(bytes).map_err(|_| Error::BadKey)?;
        Ok(Self::from_point(point))
    }

    fn from_point(point: p256::PublicKey) -> Self {
        let mut bytes = [0u8; PUBLIC_KEY_LEN];
        bytes.copy_from_slice(point.to_sec1_point(false).as_bytes());
        Self { bytes, point }
    }

    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.bytes
    }
}

impl PartialEq for PublicKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for PublicKey {}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({:02x}{:02x}…)", self.bytes[1], self.bytes[2])
    }
}

/// A private key. Its bytes are wiped when it is dropped.
pub struct DeviceKey(p256::SecretKey);

impl DeviceKey {
    pub fn generate() -> Self {
        Self(p256::SecretKey::generate())
    }

    /// The 32-byte scalar, for storing the key; see [`Self::from_bytes`].
    pub fn to_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes().into())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        p256::SecretKey::from_slice(bytes)
            .map(Self)
            .map_err(|_| Error::BadKey)
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from_point(self.0.public_key())
    }

    /// The ECDH secret with `peer`: the shared point's x coordinate.
    pub(crate) fn shared(&self, peer: &PublicKey) -> Zeroizing<[u8; 32]> {
        let shared = p256::ecdh::diffie_hellman(self.0.to_nonzero_scalar(), peer.point.as_affine());
        Zeroizing::new((*shared.raw_secret_bytes()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_reach_the_same_secret() {
        let (a, b) = (DeviceKey::generate(), DeviceKey::generate());
        assert_eq!(*a.shared(&b.public()), *b.shared(&a.public()));
    }

    #[test]
    fn a_key_survives_storage() {
        let key = DeviceKey::generate();
        let again = DeviceKey::from_bytes(&*key.to_bytes()).unwrap();
        assert_eq!(key.public(), again.public());
    }

    #[test]
    fn what_is_not_a_point_is_refused() {
        assert_eq!(PublicKey::from_bytes(&[4u8; 65]), Err(Error::BadKey));
        assert_eq!(PublicKey::from_bytes(&[]), Err(Error::BadKey));
        let key = DeviceKey::generate().public();
        assert_eq!(PublicKey::from_bytes(key.as_bytes()), Ok(key));
    }
}
