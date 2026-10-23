// The page's protocol module against the vectors the Rust crate writes
// (crates/splitlane-remote/tests/vectors.json), and against itself playing
// the computer. Run from web/: `node --test`.
import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  KIND_HELLO,
  KIND_PAIR,
  KIND_SEALED,
  b64u,
  cleanName,
  confirmationCode,
  decodeFrame,
  expired,
  newPhoneKey,
  newSalt,
  openChannel,
  pairFrame,
  pairingMac,
  parseLink,
  sharedSecret,
} from "../public/app/protocol.js";

const v = JSON.parse(readFileSync(new URL("../../crates/splitlane-remote/tests/vectors.json", import.meta.url), "utf8"));
const unhex = (s) => new Uint8Array(Buffer.from(s, "hex"));
const hex = (b) => Buffer.from(b).toString("hex");
const { subtle } = globalThis.crypto;

// The vectors' keys: private scalars of 0x11 and 0x22 bytes.
async function privateKey(fill, pub) {
  const jwk = {
    kty: "EC",
    crv: "P-256",
    d: b64u(new Uint8Array(32).fill(fill)),
    x: b64u(pub.slice(1, 33)),
    y: b64u(pub.slice(33)),
  };
  return subtle.importKey("jwk", jwk, { name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
}

const machinePub = unhex(v.machine_public);
const phonePub = unhex(v.phone_public);

test("the link reads as the computer wrote it", () => {
  const offer = parseLink(v.link);
  assert.ok(offer);
  assert.equal(hex(offer.machine), v.machine_public);
  assert.equal(offer.expires, 1800000300);
  assert.equal(offer.room.length, 16);
  assert.ok(expired(offer, 1800000300));
  assert.ok(!expired(offer, 1800000299));
});

test("what is not a pairing link is refused", () => {
  const good = v.link;
  for (const bad of [
    "https://splitlane.dev/app",
    good.replace("#pair=1.", "#pair=2."),
    good.replace("#pair=", "#pear="),
    good + ".1",
    good.replace(/\.\d+$/, ".soon"),
    good.replace(".BAIX", ".AAIX"),
    good.replace(/pair=1\.[^.]+/, "pair=1.MzMz"),
  ]) {
    assert.equal(parseLink(bad), null, bad);
  }
});

test("the pairing MAC and the code match the computer's", async () => {
  const offer = parseLink(v.link);
  assert.equal(hex(await pairingMac(offer, phonePub, "Pixel")), v.pairing_mac);
  const phone = await privateKey(0x22, phonePub);
  const shared = await sharedSecret(phone, machinePub);
  assert.equal(await confirmationCode(shared, offer.room, machinePub, phonePub), v.code);
});

test("the computer's first frame opens, and the phone's is the one it expects", async () => {
  const offer = parseLink(v.link);
  const channel = await openChannel({
    privateKey: await privateKey(0x22, phonePub),
    machinePub,
    phonePub,
    room: offer.room,
    machineSalt: new Uint8Array(16).fill(0x55),
    phoneSalt: new Uint8Array(16).fill(0x66),
  });
  assert.deepEqual(await channel.open(unhex(v.machine_first_frame)), { type: "confirm" });
  const sealed = await channel.seal({ type: "confirm" });
  assert.equal(sealed[0], KIND_SEALED);
  assert.equal(hex(sealed.subarray(1)), v.phone_first_frame);
});

test("frames are taken once and in order", async () => {
  const room = crypto.getRandomValues(new Uint8Array(16));
  const machine = await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
  const machineRaw = new Uint8Array(await subtle.exportKey("raw", machine.publicKey));
  const phone = await newPhoneKey();
  const [machineSalt, phoneSalt] = [newSalt(), newSalt()];
  const common = { machinePub: machineRaw, phonePub: phone.pub, room, machineSalt, phoneSalt };
  const onPhone = await openChannel({ ...common, privateKey: phone.privateKey, role: "phone" });
  const onMachine = await openChannel({ ...common, privateKey: machine.privateKey, role: "machine" });
  const first = (await onPhone.seal({ type: "request", id: 1, method: "list" })).subarray(1);
  const second = (await onPhone.seal({ type: "request", id: 2, method: "list" })).subarray(1);
  await assert.rejects(onMachine.open(second), /out of order/);
  assert.equal((await onMachine.open(first)).id, 1);
  await assert.rejects(onMachine.open(first), /out of order/);
  assert.equal((await onMachine.open(second)).id, 2);
  // A frame from a connection with other salts does not open.
  const other = await openChannel({ ...common, phoneSalt: newSalt(), privateKey: phone.privateKey });
  const stray = (await other.seal({ type: "confirm" })).subarray(1);
  const fresh = await openChannel({ ...common, privateKey: machine.privateKey, role: "machine" });
  await assert.rejects(fresh.open(stray));
});

test("the phone's private key cannot be exported", async () => {
  const { privateKey } = await newPhoneKey();
  assert.equal(privateKey.extractable, false);
  await assert.rejects(subtle.exportKey("jwk", privateKey));
});

test("frames decode by kind", () => {
  const pair = decodeFrame(pairFrame(phonePub, "Pixel", new Uint8Array(32)));
  assert.equal(pair.kind, KIND_PAIR);
  assert.equal(JSON.parse(new TextDecoder().decode(pair.body)).name, "Pixel");
  assert.equal(decodeFrame(new Uint8Array([KIND_HELLO, ...new Uint8Array(16)])).kind, KIND_HELLO);
  for (const bad of [[], [0], [KIND_HELLO, 1, 2], [9, 1]]) assert.equal(decodeFrame(new Uint8Array(bad)), null);
  assert.equal(decodeFrame(new Uint8Array(64 * 1024 + 1).fill(KIND_SEALED)), null);
});

test("a name is what the computer accepts", () => {
  assert.equal(cleanName("  Pixel 8 "), "Pixel 8");
  assert.equal(cleanName(""), null);
  assert.equal(cleanName("a\u0007b"), null);
  assert.equal(cleanName("я".repeat(33)), null);
  assert.equal(cleanName("я".repeat(32)), "я".repeat(32));
});
