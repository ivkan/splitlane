// The phone's side of the protocol in crates/splitlane-remote, in WebCrypto
// alone. The same module runs in the page and under `node --test`, so it
// uses nothing but `crypto.subtle` and typed arrays.
//
// Public keys are uncompressed SEC1 points (`exportKey("raw")`), the ECDH
// secret is the shared point's x coordinate (`deriveBits(..., 256)`), and
// every list of fields that is hashed or MACed goes through `put`, each
// field with its length in front.

const { subtle } = globalThis.crypto;
const enc = new TextEncoder();
const dec = new TextDecoder();

export const VERSION = 1;
export const KIND_PAIR = 1;
export const KIND_HELLO = 2;
export const KIND_SEALED = 3;
export const MAX_FRAME_LEN = 64 * 1024;
export const ROOM_LEN = 16;
export const SECRET_LEN = 32;
export const SALT_LEN = 16;
export const PUBLIC_KEY_LEN = 65;
export const MAX_NAME_LEN = 64;

const P256 = { name: "ECDH", namedCurve: "P-256" };

export function b64u(bytes) {
  let text = "";
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function unb64u(text) {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) throw new Error("not base64url");
  const plain = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(plain, (c) => c.charCodeAt(0));
}

export function concat(...parts) {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

export function put(fields) {
  return concat(
    ...fields.flatMap((field) => {
      const len = new Uint8Array(4);
      new DataView(len.buffer).setUint32(0, field.length);
      return [len, field];
    }),
  );
}

function u64(n) {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(n));
  return out;
}

async function hkdf(ikm, salt, info, bits) {
  const key = await subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  return new Uint8Array(await subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info }, key, bits));
}

/// What a QR code's link holds: `...#pair=1.<room>.<machine>.<secret>.<expiry>`.
/// Anything else, or another version, is not a link this page understands.
export function parseLink(link) {
  const hash = String(link).indexOf("#");
  if (hash < 0) return null;
  const fragment = String(link).slice(hash + 1);
  if (!fragment.startsWith("pair=")) return null;
  const parts = fragment.slice("pair=".length).split(".");
  if (parts.length !== 5 || parts[0] !== String(VERSION) || !/^\d{1,15}$/.test(parts[4])) return null;
  try {
    const room = unb64u(parts[1]);
    const machine = unb64u(parts[2]);
    const secret = unb64u(parts[3]);
    if (room.length !== ROOM_LEN || secret.length !== SECRET_LEN) return null;
    if (machine.length !== PUBLIC_KEY_LEN || machine[0] !== 4) return null;
    return { room, machine, secret, expires: Number(parts[4]) };
  } catch {
    return null;
  }
}

export function expired(offer, nowUnix = Date.now() / 1000) {
  return nowUnix >= offer.expires;
}

/// The name a computer is told this phone goes by: what the machine accepts
/// (`verify_pairing`), or null.
export function cleanName(name) {
  const trimmed = String(name).trim();
  // Control characters are what the machine refuses in a name.
  // eslint-disable-next-line no-control-regex
  if (!trimmed || /[\u0000-\u001f\u007f-\u009f]/.test(trimmed)) return null;
  return enc.encode(trimmed).length <= MAX_NAME_LEN ? trimmed : null;
}

/// A new key pair for this phone. The private key cannot be exported, so a
/// script that later runs on the page could use it but not carry it off.
export async function newPhoneKey() {
  const pair = await subtle.generateKey(P256, false, ["deriveBits"]);
  const pub = new Uint8Array(await subtle.exportKey("raw", pair.publicKey));
  return { privateKey: pair.privateKey, pub };
}

export async function sharedSecret(privateKey, peerPub) {
  const peer = await subtle.importKey("raw", peerPub, P256, true, []);
  return new Uint8Array(await subtle.deriveBits({ name: "ECDH", public: peer }, privateKey, 256));
}

export async function pairingMac(offer, phonePub, name) {
  const key = await subtle.importKey("raw", offer.secret, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const message = put([
    enc.encode("splitlane-pair-v1"),
    new Uint8Array([VERSION]),
    u64(offer.expires),
    offer.room,
    offer.machine,
    phonePub,
    enc.encode(name),
  ]);
  return new Uint8Array(await subtle.sign("HMAC", key, message));
}

/// The six digits both sides show; the person types the phone's on the
/// computer.
export async function confirmationCode(shared, room, machinePub, phonePub) {
  const bytes = await hkdf(shared, room, put([enc.encode("splitlane-code-v1"), machinePub, phonePub]), 64);
  const n = new DataView(bytes.buffer).getBigUint64(0) % 1000000n;
  return String(n).padStart(6, "0");
}

export function pairFrame(phonePub, name, mac) {
  const body = enc.encode(JSON.stringify({ phone: b64u(phonePub), name, mac: b64u(mac) }));
  return concat(new Uint8Array([KIND_PAIR]), body);
}

export function newSalt() {
  return crypto.getRandomValues(new Uint8Array(SALT_LEN));
}

export function helloFrame(salt) {
  return concat(new Uint8Array([KIND_HELLO]), salt);
}

/// One relay frame: `{kind, body}`, or null for what is not one.
export function decodeFrame(bytes) {
  if (bytes.length === 0 || bytes.length > MAX_FRAME_LEN) return null;
  const kind = bytes[0];
  const body = bytes.subarray(1);
  if (kind === KIND_HELLO && body.length !== SALT_LEN) return null;
  if (kind !== KIND_PAIR && kind !== KIND_HELLO && kind !== KIND_SEALED) return null;
  return { kind, body };
}

function nonce(counter) {
  const out = new Uint8Array(12);
  new DataView(out.buffer).setBigUint64(4, counter);
  return out;
}

/// The channel of one connection, from the side named by `role` ("phone",
/// or "machine" in tests). Keys come from the shared secret and the
/// transcript, a hash of the room, both keys and both salts, which every
/// frame also carries as associated data. Frames are numbered from zero and
/// taken only in order.
export async function openChannel({ privateKey, machinePub, phonePub, room, machineSalt, phoneSalt, role = "phone" }) {
  const peer = role === "phone" ? machinePub : phonePub;
  const shared = await sharedSecret(privateKey, peer);
  const transcript = new Uint8Array(
    await subtle.digest(
      "SHA-256",
      put([enc.encode("splitlane-session-v1"), room, machinePub, phonePub, machineSalt, phoneSalt]),
    ),
  );
  const key = async (direction) =>
    subtle.importKey(
      "raw",
      await hkdf(shared, transcript, put([enc.encode("splitlane-key-v1"), enc.encode(direction)]), 256),
      "AES-GCM",
      false,
      ["encrypt", "decrypt"],
    );
  const [toMachine, toPhone] = [await key("phone-to-machine"), await key("machine-to-phone")];
  const [sendKey, receiveKey] = role === "phone" ? [toMachine, toPhone] : [toPhone, toMachine];
  let sent = 0n;
  let received = 0n;
  return {
    shared,
    /// A message as the body of a sealed frame: the counter, then the
    /// ciphertext.
    async seal(message) {
      const counter = sent++;
      const plain = enc.encode(JSON.stringify(message));
      const sealed = new Uint8Array(
        await subtle.encrypt({ name: "AES-GCM", iv: nonce(counter), additionalData: transcript }, sendKey, plain),
      );
      return concat(new Uint8Array([KIND_SEALED]), u64(counter), sealed);
    },
    /// The message in a sealed frame's body. Throws on a frame out of
    /// order, or one that does not open: the channel is then over.
    async open(body) {
      if (body.length < 8 + 16) throw new Error("a sealed frame too short to be one");
      const counter = new DataView(body.buffer, body.byteOffset, 8).getBigUint64(0);
      if (counter !== received) throw new Error("a frame out of order or again");
      const plain = await subtle.decrypt(
        { name: "AES-GCM", iv: nonce(counter), additionalData: transcript },
        receiveKey,
        body.subarray(8),
      );
      received++;
      return JSON.parse(dec.decode(plain));
    },
  };
}

/// A short, stable name for a public key, for telling computers apart.
export async function fingerprint(pub) {
  const hash = new Uint8Array(await subtle.digest("SHA-256", pub));
  return Array.from(hash.subarray(0, 2), (b) => b.toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase();
}
