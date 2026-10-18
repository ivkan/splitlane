// A phone's connection to a computer through the relay: one WebSocket to
// the room, the handshake, and requests over the channel it sets up.

import {
  KIND_HELLO,
  KIND_SEALED,
  b64u,
  confirmationCode,
  decodeFrame,
  helloFrame,
  newPhoneKey,
  newSalt,
  openChannel,
  pairFrame,
  pairingMac,
  unb64u,
} from "./protocol.js";

export const RELAY = "wss://relay.splitlane.dev";

/// How long the computer has to answer a hello, and a request.
const HELLO_WAIT_MS = 8000;
const REQUEST_WAIT_MS = 25000;

/// One WebSocket to a room as the phone, with the frames it receives
/// queued for `next`.
export function joinRoom(relay, roomText) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`${relay}/room/${roomText}?role=phone`);
    ws.binaryType = "arraybuffer";
    const queue = [];
    let waiter = null;
    let closed = false;
    const wake = () => {
      if (waiter) {
        const w = waiter;
        waiter = null;
        w();
      }
    };
    ws.onmessage = (event) => {
      if (!(event.data instanceof ArrayBuffer)) return;
      const frame = decodeFrame(new Uint8Array(event.data));
      if (frame) {
        queue.push(frame);
        wake();
      }
    };
    // A socket that never opened may close without an error first.
    ws.onclose = () => {
      closed = true;
      wake();
      reject(new Error("the relay could not be reached"));
    };
    ws.onerror = () => reject(new Error("the relay could not be reached"));
    ws.onopen = () => {
      resolve({
        send: (bytes) => ws.send(bytes),
        close: () => ws.close(),
        get closed() {
          return closed;
        },
        /// The next frame, or an error once `ms` pass or the socket closes.
        next(ms) {
          if (queue.length) return Promise.resolve(queue.shift());
          if (closed) return Promise.reject(new Error("the connection closed"));
          return new Promise((ok, fail) => {
            const timer = setTimeout(() => {
              waiter = null;
              fail(new Error("timeout"));
            }, ms);
            waiter = () => {
              clearTimeout(timer);
              if (queue.length) ok(queue.shift());
              else fail(new Error("the connection closed"));
            };
          });
        },
      });
    };
  });
}

/// Start a channel on `room`: the phone's hello, the computer's, and each
/// side's confirmation. A computer that does not answer is a timeout; one
/// that no longer knows this phone answers the hello but never confirms.
export async function handshake(room, keys) {
  const phoneSalt = newSalt();
  room.send(helloFrame(phoneSalt));
  let frame;
  do {
    frame = await room.next(HELLO_WAIT_MS);
  } while (frame.kind !== KIND_HELLO);
  const channel = await openChannel({ ...keys, machineSalt: frame.body, phoneSalt, role: "phone" });
  room.send(await channel.seal({ type: "confirm" }));
  const confirm = await nextMessage(room, channel, HELLO_WAIT_MS);
  if (confirm.type !== "confirm") throw new Error("the computer did not confirm the channel");
  return channel;
}

export async function nextMessage(room, channel, ms) {
  let frame;
  do {
    frame = await room.next(ms);
  } while (frame.kind !== KIND_SEALED);
  return channel.open(frame.body);
}

/// A paired computer, connected: `request(method, params)` resolves with
/// the result or rejects with the computer's words. `onClose` is called
/// once, when the connection ends for any reason.
export async function connect(relay, machine, onClose) {
  const room = await joinRoom(relay, machine.room);
  let channel;
  try {
    channel = await handshake(room, {
      privateKey: machine.key,
      machinePub: machine.machine,
      phonePub: machine.phone,
      room: unb64u(machine.room),
    });
  } catch (error) {
    room.close();
    throw error;
  }
  const pending = new Map();
  let nextId = 1;
  let ended = false;
  const end = (error) => {
    if (ended) return;
    ended = true;
    room.close();
    for (const { reject, timer } of pending.values()) {
      clearTimeout(timer);
      reject(error);
    }
    pending.clear();
    onClose?.(error);
  };
  (async () => {
    for (;;) {
      let frame;
      try {
        // Waits for as long as the socket lives; the relay and the
        // computer keep it alive.
        frame = await room.next(24 * 3600 * 1000);
      } catch (error) {
        return end(error);
      }
      if (frame.kind !== KIND_SEALED) continue;
      let message;
      try {
        message = await channel.open(frame.body);
      } catch {
        // Not this channel's: the same page open in another tab shares the
        // room. Its frames are skipped; if the computer has moved to the
        // other channel, the next request times out and this one starts
        // again.
        continue;
      }
      if (message.type === "response" && pending.has(message.id)) {
        const { resolve, reject, timer } = pending.get(message.id);
        pending.delete(message.id);
        clearTimeout(timer);
        if (message.error != null) reject(new Error(message.error));
        else resolve(message.result ?? null);
      }
    }
  })();
  // Sealing is numbered, so frames go out one at a time in order.
  let sending = Promise.resolve();
  return {
    request(method, params = null) {
      if (ended) return Promise.reject(new Error("not connected"));
      const id = nextId++;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error("the computer did not answer in time"));
          // Answers are in order, so a lost one leaves the channel unsure.
          end(new Error("timeout"));
        }, REQUEST_WAIT_MS);
        pending.set(id, { resolve, reject, timer });
        sending = sending
          .then(async () => room.send(await channel.seal({ type: "request", id, method, params })))
          .catch((error) => end(error));
      });
    },
    close: () => end(new Error("closed")),
    get open() {
      return !ended;
    },
  };
}

/// Pair with the computer whose offer is `offer`: send this phone's key and
/// name, show `onCode` the six digits once the channel is up, and wait for
/// the person to type them on the computer. Resolves with what to keep for
/// the computer, or rejects with why it did not pair.
export async function pair(relay, offer, name, onCode) {
  const key = await newPhoneKey();
  const mac = await pairingMac(offer, key.pub, name);
  const room = await joinRoom(relay, b64u(offer.room));
  try {
    room.send(pairFrame(key.pub, name, mac));
    let channel;
    try {
      channel = await handshake(room, {
        privateKey: key.privateKey,
        machinePub: offer.machine,
        phonePub: key.pub,
        room: offer.room,
      });
    } catch (error) {
      if (error.message === "timeout") {
        throw new Error("The computer did not answer. Is the pairing code still on its screen?");
      }
      throw error;
    }
    onCode(await confirmationCode(channel.shared, offer.room, offer.machine, key.pub));
    const kept = { room: b64u(offer.room), machine: offer.machine, phone: key.pub, key: key.privateKey };
    // Until the offer lapses, and a little after: the computer says so then.
    const wait = Math.max(0, offer.expires * 1000 - Date.now()) + 15000;
    let verdict;
    try {
      verdict = await nextMessage(room, channel, wait);
    } catch (error) {
      // The connection ended with no answer. The computer may have kept
      // the phone and its answer been lost on the way; only a computer
      // that kept this key can confirm a channel with it.
      if (await answersAsPaired(relay, kept)) return kept;
      throw error;
    }
    if (verdict.type === "paired") return kept;
    if (verdict.type === "refused") throw new Error(sentence(verdict.reason));
    throw new Error("The computer answered something this page does not understand.");
  } finally {
    room.close();
  }
}

/// Whether the computer now confirms a channel with the key just paired.
/// Its bridge takes up a new phone within seconds, so this asks for a while.
async function answersAsPaired(relay, kept) {
  const until = Date.now() + 30000;
  while (Date.now() < until) {
    try {
      const conn = await connect(relay, kept);
      conn.close();
      return true;
    } catch {
      await new Promise((r) => setTimeout(r, 2000));
    }
  }
  return false;
}

function sentence(text) {
  const s = String(text ?? "").trim();
  return s ? s[0].toUpperCase() + s.slice(1) + (/[.!?]$/.test(s) ? "" : ".") : "The computer refused.";
}
