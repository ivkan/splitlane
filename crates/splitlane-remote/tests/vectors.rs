//! Fixed inputs and what they must give, for the page on the phone to check
//! itself against: `vectors.json` beside this file is what WebCrypto has to
//! produce from the same inputs.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking, like the tests that call them"
)]

use splitlane_remote::{
    confirmation_code, pairing_mac, DeviceKey, Handshake, Offer, PublicKey, Role, RoomId, Salt,
};

const MACHINE_KEY: [u8; 32] = [0x11; 32];
const PHONE_KEY: [u8; 32] = [0x22; 32];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn field<'a>(json: &'a str, key: &str) -> &'a str {
    let at = json.find(&format!("\"{key}\": \"")).unwrap() + key.len() + 5;
    let end = json[at..].find('"').unwrap();
    &json[at..at + end]
}

struct Fixed {
    machine: DeviceKey,
    phone: DeviceKey,
    offer: Offer,
    machine_salt: Salt,
    phone_salt: Salt,
}

fn fixed() -> Fixed {
    let machine = DeviceKey::from_bytes(&MACHINE_KEY).unwrap();
    let phone = DeviceKey::from_bytes(&PHONE_KEY).unwrap();
    let offer = Offer {
        room: RoomId([0x33; 16]),
        machine: machine.public(),
        secret: [0x44; 32],
        expires_unix: 1_800_000_300,
    };
    Fixed {
        machine,
        phone,
        offer,
        machine_salt: Salt([0x55; 16]),
        phone_salt: Salt([0x66; 16]),
    }
}

/// What the vectors hold, computed here. Printed by the ignored test below
/// when the file has to be made again.
fn compute() -> String {
    let f = fixed();
    let mut machine = Handshake::with_salt(Role::Machine, f.machine_salt).finish(
        &f.machine,
        &f.phone.public(),
        f.offer.room,
        f.phone_salt,
    );
    let frame = machine.seal(b"{\"type\":\"confirm\"}").unwrap();
    format!(
        "{{\n  \"machine_public\": \"{}\",\n  \"phone_public\": \"{}\",\n  \"link\": \"{}\",\n  \"pairing_mac\": \"{}\",\n  \"code\": \"{}\",\n  \"machine_first_frame\": \"{}\"\n}}\n",
        hex(f.machine.public().as_bytes()),
        hex(f.phone.public().as_bytes()),
        f.offer.to_link("https://splitlane.dev/app"),
        hex(&pairing_mac(&f.offer, &f.phone.public(), "Pixel")),
        confirmation_code(&f.machine, &f.phone.public(), &f.offer),
        hex(&frame),
    )
}

#[test]
fn the_fixed_inputs_give_the_recorded_outputs() {
    let recorded = include_str!("vectors.json");
    let computed = compute();
    for key in [
        "machine_public",
        "phone_public",
        "link",
        "pairing_mac",
        "code",
        "machine_first_frame",
    ] {
        assert_eq!(
            field(&computed, key),
            field(recorded, key),
            "{key}: run the ignored `print_vectors` test to see the new values"
        );
    }
}

/// The page's side, as Rust receives it: `phone_first_frame` was sealed
/// with WebCrypto from the same inputs, by an implementation that shares no
/// code with this crate, and opens here.
#[test]
fn a_frame_sealed_by_the_page_opens_here() {
    let recorded = include_str!("vectors.json");
    let f = fixed();
    let mut machine = Handshake::with_salt(Role::Machine, f.machine_salt).finish(
        &f.machine,
        &PublicKey::from_bytes(&f.phone.public().as_bytes()[..]).unwrap(),
        f.offer.room,
        f.phone_salt,
    );
    let frame = unhex(field(recorded, "phone_first_frame"));
    assert_eq!(machine.open(&frame).unwrap(), b"{\"type\":\"confirm\"}");
}

#[test]
#[ignore]
fn print_vectors() {
    print!("{}", compute());
}
