// The relay's rules, against `wrangler dev` on this machine.
// Run from relay/: `node --test test/relay.test.mjs`.
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";

const PORT = 8000 + Math.floor(Math.random() * 1000);
const BASE = `ws://127.0.0.1:${PORT}`;
let dev;

before(async () => {
  dev = spawn("npx", ["--yes", "wrangler@4.145.0", "dev", "--port", String(PORT), "--ip", "127.0.0.1"], {
    cwd: new URL("..", import.meta.url),
    stdio: "ignore",
  });
  // Two minutes: a first run waits for npx to fetch wrangler.
  for (let i = 0; i < 240; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${PORT}/health`)).ok) return;
    } catch {
      // Not up yet.
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error("wrangler dev did not start");
});

after(() => dev?.kill());

const b64 = (bytes) => Buffer.from(bytes).toString("base64url");
const room = () => b64(randomBytes(16));
const token = () => b64(randomBytes(32));

// Resolves with the open socket, or with the close code if the relay refused.
function join(room, role, joinToken) {
  return new Promise((ok) => {
    const headers = joinToken ? { "x-splitlane-join": joinToken } : {};
    const ws = new WebSocket(`${BASE}/room/${room}?role=${role}`, { headers });
    ws.binaryType = "arraybuffer";
    ws.onopen = () => ok({ ws });
    ws.onerror = () => ok({ refused: true });
  });
}

function next(ws) {
  return new Promise((ok) => {
    ws.onmessage = (e) => ok({ data: new Uint8Array(e.data) });
    ws.onclose = (e) => ok({ closed: e.code });
  });
}

test("frames cross between the machine and the phone", async () => {
  const r = room();
  const machine = await join(r, "machine", token());
  const phone = await join(r, "phone");
  phone.ws.send(new Uint8Array([3, 1, 2]));
  assert.deepEqual((await next(machine.ws)).data, new Uint8Array([3, 1, 2]));
  machine.ws.send(new Uint8Array([2, 9]));
  assert.deepEqual((await next(phone.ws)).data, new Uint8Array([2, 9]));
  machine.ws.close();
  phone.ws.close();
});

test("only the machine that claimed a room may join it as the machine", async () => {
  const r = room();
  const own = token();
  const first = await join(r, "machine", own);
  assert.ok(first.ws);
  assert.ok((await join(r, "machine", token())).refused);
  assert.ok((await join(r, "machine")).refused);
  // The rightful machine joining again replaces its old socket.
  const again = await join(r, "machine", own);
  assert.ok(again.ws);
  assert.equal((await next(first.ws)).closed, 1000);
  again.ws.close();
});

test("a phone cannot open a room no machine has claimed", async () => {
  assert.ok((await join(room(), "phone")).refused);
});

test("a third phone pushes out the oldest", async () => {
  const r = room();
  const machine = await join(r, "machine", token());
  const one = await join(r, "phone");
  const closing = next(one.ws);
  await new Promise((r) => setTimeout(r, 20));
  const two = await join(r, "phone");
  await new Promise((r) => setTimeout(r, 20));
  const three = await join(r, "phone");
  assert.equal((await closing).closed, 1000);
  for (const s of [machine, two, three]) s.ws.close();
});

test("text and oversized frames close the socket", async () => {
  const r = room();
  const machine = await join(r, "machine", token());
  const phone = await join(r, "phone");
  phone.ws.send("hello");
  assert.equal((await next(phone.ws)).closed, 1003);
  const big = await join(r, "phone");
  big.ws.send(new Uint8Array(64 * 1024 + 1));
  assert.equal((await next(big.ws)).closed, 1009);
  machine.ws.close();
});

test("a socket sending too fast is closed", async () => {
  const r = room();
  const machine = await join(r, "machine", token());
  const phone = await join(r, "phone");
  const closed = next(phone.ws);
  for (let i = 0; i < 300; i++) phone.ws.send(new Uint8Array([3]));
  assert.equal((await closed).closed, 1008);
  machine.ws.close();
});
