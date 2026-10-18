// The relay between Splitlane on a computer and a phone paired with it.
//
// One Durable Object per pairing (a "room"), with one machine socket and at
// most a couple of phone sockets. Frames are forwarded between the two sides
// unread: they are sealed end to end (see the splitlane-remote crate), and
// the relay knows only which room a frame is for, how long it is and when it
// came. Nothing is stored but a hash of the machine's join token and when
// the machine last joined.

// The header the machine's join token travels in; a URL ends up in logs.
const JOIN_HEADER = "x-splitlane-join";
// splitlane_remote::wire::MAX_FRAME_LEN.
const MAX_FRAME = 64 * 1024;
// A room is 16 random bytes in unpadded base64url; a token is 32.
const ROOM_PATTERN = /^\/room\/([A-Za-z0-9_-]{22})$/;
const TOKEN_PATTERN = /^[A-Za-z0-9_-]{43}$/;
// Two phones at once covers a page open in two tabs; a third would only be
// someone who knows the room keeping the phone out, so the oldest goes.
const MAX_PHONES = 2;
// Frames a socket may send: a steady rate and a burst on top of it. The
// bridge answers one request with one frame, so a person tapping never comes
// near it.
const FRAMES_PER_SECOND = 20;
const FRAME_BURST = 100;
// A room the machine has not joined for this long is forgotten. A machine
// joins every time its bridge starts, which is whenever the computer runs
// Splitlane with a phone paired.
const ROOM_TTL_MS = 180 * 24 * 60 * 60 * 1000;

const MACHINE = "machine";
const PHONE = "phone";

export class Room {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    // Per-socket allowance. Lost when the object hibernates, which only
    // ever gives a socket a fresh burst.
    this.allowance = new WeakMap();
  }

  async fetch(req) {
    const role = new URL(req.url).searchParams.get("role");
    if (role !== MACHINE && role !== PHONE) {
      return text(400, "role must be machine or phone");
    }
    if (req.headers.get("Upgrade") !== "websocket") {
      return text(426, "expected a websocket");
    }
    const claimed = await this.ctx.storage.get("machine");
    if (role === MACHINE) {
      const token = req.headers.get(JOIN_HEADER) ?? "";
      if (!TOKEN_PATTERN.test(token)) return text(401, "the machine needs its join token");
      const hash = await sha256(token);
      if (claimed === undefined) {
        // The first machine to join a room owns it. It joins before showing
        // the room in a QR code, so nobody else has seen the address yet.
        const ip = req.headers.get("cf-connecting-ip") ?? "unknown";
        if (!(await allowed(this.env.CLAIMS, ip))) {
          log("claim refused: too many from one address");
          return text(429, "too many new rooms from this address");
        }
        await this.ctx.storage.put("machine", hash);
        log("room claimed");
      } else if (!equal(hash, claimed)) {
        log("machine refused: wrong token");
        return text(403, "this room belongs to another machine");
      }
      await this.ctx.storage.put("joined", Date.now());
      await this.ctx.storage.setAlarm(Date.now() + ROOM_TTL_MS);
      // A machine that joins again has lost its old connection, or is the
      // one bridge that should be running; either way the old socket goes.
      for (const old of this.ctx.getWebSockets(MACHINE)) old.close(1000, "replaced");
    } else {
      if (claimed === undefined) return text(404, "no such room");
      const phones = this.ctx
        .getWebSockets(PHONE)
        .sort((a, b) => joinedAt(a) - joinedAt(b));
      for (const old of phones.slice(0, Math.max(0, phones.length - MAX_PHONES + 1))) {
        old.close(1000, "replaced");
      }
    }
    const [client, server] = Object.values(new WebSocketPair());
    // Hibernation API: the object can be evicted while sockets stay open.
    this.ctx.acceptWebSocket(server, [role]);
    server.serializeAttachment({ joined: Date.now() });
    return new Response(null, { status: 101, webSocket: client });
  }

  webSocketMessage(ws, msg) {
    if (typeof msg === "string") {
      ws.close(1003, "frames are binary");
      return;
    }
    if (msg.byteLength > MAX_FRAME) {
      ws.close(1009, "frame too long");
      return;
    }
    if (!this.spend(ws)) {
      log("socket closed: too many frames");
      ws.close(1008, "too many frames");
      return;
    }
    const [role] = this.ctx.getTags(ws);
    const peer = role === MACHINE ? PHONE : MACHINE;
    for (const s of this.ctx.getWebSockets(peer)) {
      try {
        s.send(msg);
      } catch {
        // A peer socket that is closing; the other peers still get it.
      }
    }
  }

  webSocketClose(ws, code) {
    // 1005 ("no status") and 1006 arrive from clients but may not be sent
    // back.
    const reply = code === 1005 || code === 1006 ? 1000 : code;
    try {
      ws.close(reply, "closed");
    } catch {
      // Already closed.
    }
  }

  webSocketError() {}

  async alarm() {
    const joined = (await this.ctx.storage.get("joined")) ?? 0;
    const due = joined + ROOM_TTL_MS;
    if (this.ctx.getWebSockets(MACHINE).length > 0 || Date.now() < due) {
      await this.ctx.storage.setAlarm(Math.max(due, Date.now() + 24 * 60 * 60 * 1000));
      return;
    }
    for (const ws of this.ctx.getWebSockets()) ws.close(1001, "room forgotten");
    await this.ctx.storage.deleteAll();
    log("room forgotten");
  }

  // A token bucket per socket.
  spend(ws) {
    const now = Date.now();
    const was = this.allowance.get(ws) ?? { left: FRAME_BURST, at: now };
    const left = Math.min(FRAME_BURST, was.left + ((now - was.at) / 1000) * FRAMES_PER_SECOND);
    if (left < 1) {
      this.allowance.set(ws, { left, at: now });
      return false;
    }
    this.allowance.set(ws, { left: left - 1, at: now });
    return true;
  }
}

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    if (url.pathname === "/health") return text(200, "ok");
    const room = url.pathname.match(ROOM_PATTERN);
    if (!room) return text(404, "not found");
    const ip = req.headers.get("cf-connecting-ip") ?? "unknown";
    if (!(await allowed(env.JOINS, ip))) {
      log("join refused: too many from one address");
      return text(429, "too many connections from this address");
    }
    return env.ROOM.get(env.ROOM.idFromName(room[1])).fetch(req);
  },
};

function text(status, body) {
  return new Response(body, { status });
}

// Rooms and addresses stay out of the logs: an error is reported by what it
// was, not by whom.
function log(event) {
  console.log(event);
}

async function allowed(limiter, key) {
  // Without the binding (an old deployment, a test) nothing is limited.
  if (!limiter) return true;
  const { success } = await limiter.limit({ key });
  return success;
}

function joinedAt(ws) {
  return ws.deserializeAttachment()?.joined ?? 0;
}

async function sha256(value) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function equal(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}
