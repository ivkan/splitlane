//! The pairing offer, the phone's answer to it, and the code that confirms
//! the two met.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;

use crate::keys::{DeviceKey, PublicKey};
use crate::{put, Error, VERSION};

pub const ROOM_LEN: usize = 16;
pub const SECRET_LEN: usize = 32;
/// Longest device name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

const LINK_KEY: &str = "pair=";

/// The relay room two sides meet in. Random, and the only thing the relay
/// learns about who is talking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoomId(pub [u8; ROOM_LEN]);

impl RoomId {
    pub fn random() -> Self {
        let mut room = [0u8; ROOM_LEN];
        OsRng.fill_bytes(&mut room);
        Self(room)
    }

    /// As the relay's URL path takes it.
    pub fn to_text(self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

/// What the machine shows as a QR code to start pairing.
///
/// Its `Debug` leaves the secret out, and two offers are compared in
/// constant time: the secret is what lets a phone pair.
#[derive(Clone)]
pub struct Offer {
    pub room: RoomId,
    pub machine: PublicKey,
    pub secret: [u8; SECRET_LEN],
    /// Seconds since the Unix epoch after which neither side accepts it.
    pub expires_unix: u64,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("room", &self.room)
            .field("machine", &self.machine)
            .field("secret", &"<hidden>")
            .field("expires_unix", &self.expires_unix)
            .finish()
    }
}

impl PartialEq for Offer {
    fn eq(&self, other: &Self) -> bool {
        bool::from(subtle::ConstantTimeEq::ct_eq(
            &self.secret[..],
            &other.secret[..],
        )) & (self.room == other.room)
            & (self.machine == other.machine)
            & (self.expires_unix == other.expires_unix)
    }
}

impl Eq for Offer {}

impl Offer {
    pub fn new(machine: &PublicKey, now_unix: u64, ttl_secs: u64) -> Self {
        let mut secret = [0u8; SECRET_LEN];
        OsRng.fill_bytes(&mut secret);
        Self {
            room: RoomId::random(),
            machine: machine.clone(),
            secret,
            expires_unix: now_unix.saturating_add(ttl_secs),
        }
    }

    /// The link the QR code holds: `base` with the offer in its fragment,
    /// `#pair=1.<room>.<machine key>.<secret>.<expiry>`, binary parts in
    /// unpadded base64url. A fragment never leaves the browser.
    pub fn to_link(&self, base: &str) -> String {
        format!(
            "{base}#{LINK_KEY}{VERSION}.{}.{}.{}.{}",
            URL_SAFE_NO_PAD.encode(self.room.0),
            URL_SAFE_NO_PAD.encode(self.machine.as_bytes()),
            URL_SAFE_NO_PAD.encode(self.secret),
            self.expires_unix
        )
    }

    pub fn from_link(link: &str) -> Result<Self, Error> {
        let (_, fragment) = link.split_once('#').ok_or(Error::BadLink)?;
        let offer = fragment.strip_prefix(LINK_KEY).ok_or(Error::BadLink)?;
        let parts: Vec<&str> = offer.split('.').collect();
        let [version, room, machine, secret, expires] = parts[..] else {
            return Err(Error::BadLink);
        };
        if version != VERSION.to_string() {
            return Err(Error::BadLink);
        }
        let bytes = |part: &str| URL_SAFE_NO_PAD.decode(part).map_err(|_| Error::BadLink);
        Ok(Self {
            room: RoomId(bytes(room)?.try_into().map_err(|_| Error::BadLink)?),
            machine: PublicKey::from_bytes(&bytes(machine)?)?,
            secret: bytes(secret)?.try_into().map_err(|_| Error::BadLink)?,
            expires_unix: expires.parse().map_err(|_| Error::BadLink)?,
        })
    }

    pub fn expired(&self, now_unix: u64) -> bool {
        now_unix >= self.expires_unix
    }
}

/// What the phone sends with its key: a MAC with the offer's secret over
/// everything the pairing is about, the offer's version and expiry
/// included, so that nothing in it can be swapped for something else from
/// another offer or another protocol.
///
/// A relay can deliver the same request again while the offer lasts, and
/// it verifies again. It is the confirmation code that pairs: a caller
/// keeps nobody's key on a verified request alone.
pub fn pairing_mac(offer: &Offer, phone: &PublicKey, name: &str) -> [u8; 32] {
    let mut message = Vec::new();
    put(&mut message, b"splitlane-pair-v1");
    put(&mut message, &[VERSION]);
    put(&mut message, &offer.expires_unix.to_be_bytes());
    put(&mut message, &offer.room.0);
    put(&mut message, offer.machine.as_bytes());
    put(&mut message, phone.as_bytes());
    put(&mut message, name.as_bytes());
    #[allow(clippy::expect_used)] // HMAC takes a key of any length.
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&offer.secret).expect("HMAC accepts any key length");
    mac.update(&message);
    mac.finalize().into_bytes().into()
}

/// The machine's check of a pairing request: within the offer's time, a
/// name it can show, and the MAC compared in constant time.
pub fn verify_pairing(
    offer: &Offer,
    phone: &PublicKey,
    name: &str,
    mac: &[u8],
    now_unix: u64,
) -> Result<(), Error> {
    if offer.expired(now_unix) {
        return Err(Error::Expired);
    }
    if name.is_empty() || name.len() > MAX_NAME_LEN || name.chars().any(char::is_control) {
        return Err(Error::BadName);
    }
    let expected = pairing_mac(offer, phone, name);
    if bool::from(subtle::ConstantTimeEq::ct_eq(&expected[..], mac)) {
        Ok(())
    } else {
        Err(Error::BadMac)
    }
}

/// The six digits both sides show once the phone's key has arrived. Only
/// the holders of the two private keys can work them out.
///
/// A million codes is few enough to try them all, so the caller takes a
/// small number of attempts per offer and then drops the offer.
pub fn confirmation_code(me: &DeviceKey, peer: &PublicKey, offer: &Offer) -> String {
    let mine = me.public();
    // Ordered machine first whichever side asks, so both get one code.
    let (machine, phone) = if mine == offer.machine {
        (&mine, peer)
    } else {
        (peer, &mine)
    };
    let shared = me.shared(peer);
    let mut info = Vec::new();
    put(&mut info, b"splitlane-code-v1");
    put(&mut info, machine.as_bytes());
    put(&mut info, phone.as_bytes());
    // Eight bytes, so that reducing them to six digits leaves no bias worth
    // the name.
    let mut out = [0u8; 8];
    // Eight bytes are far below HKDF's limit, so expanding cannot fail.
    let _ = Hkdf::<Sha256>::new(Some(&offer.room.0), &*shared).expand(&info, &mut out);
    format!("{:06}", u64::from_be_bytes(out) % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn offer() -> (DeviceKey, Offer) {
        let machine = DeviceKey::generate();
        let offer = Offer::new(&machine.public(), NOW, 300);
        (machine, offer)
    }

    #[test]
    fn an_offer_survives_its_link() {
        let (_, offer) = offer();
        let link = offer.to_link("https://splitlane.dev/app");
        assert!(link.starts_with("https://splitlane.dev/app#pair=1."));
        assert_eq!(Offer::from_link(&link), Ok(offer));
    }

    #[test]
    fn an_offer_never_prints_its_secret() {
        let (_, offer) = offer();
        let printed = format!("{offer:?}");
        assert!(printed.contains("<hidden>"), "{printed}");
        assert!(
            !printed.contains(&format!("{:?}", offer.secret)),
            "{printed}"
        );
    }

    #[test]
    fn a_link_that_is_not_an_offer_is_refused() {
        let (_, offer) = offer();
        let link = offer.to_link("https://splitlane.dev/app");
        for bad in [
            "https://splitlane.dev/app",
            "https://splitlane.dev/app#pair=",
            link.split_once('#').unwrap().1,
            &link.replace("#pair=1.", "#pair=2."),
            &link.replace("#pair=1.", "#pair=1.x."),
        ] {
            assert_eq!(Offer::from_link(bad), Err(Error::BadLink), "{bad}");
        }
    }

    #[test]
    fn a_request_is_taken_only_as_it_was_made_and_in_time() {
        let (_, offer) = offer();
        let phone = DeviceKey::generate().public();
        let mac = pairing_mac(&offer, &phone, "Pixel");
        assert_eq!(verify_pairing(&offer, &phone, "Pixel", &mac, NOW), Ok(()));
        // Another name, another key, another offer's secret, too late.
        assert_eq!(
            verify_pairing(&offer, &phone, "Pixe1", &mac, NOW),
            Err(Error::BadMac)
        );
        let other = DeviceKey::generate().public();
        assert_eq!(
            verify_pairing(&offer, &other, "Pixel", &mac, NOW),
            Err(Error::BadMac)
        );
        let (_, another) = self::offer();
        assert_eq!(
            verify_pairing(&another, &phone, "Pixel", &mac, NOW),
            Err(Error::BadMac)
        );
        assert_eq!(
            verify_pairing(&offer, &phone, "Pixel", &mac, NOW + 300),
            Err(Error::Expired)
        );
        assert_eq!(
            verify_pairing(&offer, &phone, "", &mac, NOW),
            Err(Error::BadName)
        );
    }

    #[test]
    fn both_sides_show_the_same_code_and_a_stranger_another() {
        let (machine, offer) = offer();
        let phone = DeviceKey::generate();
        let at_machine = confirmation_code(&machine, &phone.public(), &offer);
        let at_phone = confirmation_code(&phone, &offer.machine, &offer);
        assert_eq!(at_machine, at_phone);
        assert_eq!(at_machine.len(), 6);
        assert!(at_machine.bytes().all(|b| b.is_ascii_digit()));
        // Someone who photographed the QR code pairs with a key of their own,
        // and gets a code of their own.
        let stranger = DeviceKey::generate();
        assert_ne!(
            confirmation_code(&stranger, &offer.machine, &offer),
            at_phone
        );
    }
}
