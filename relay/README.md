# relay

The Cloudflare Worker behind `wss://relay.splitlane.dev`, which carries
frames between Splitlane on a computer and a phone paired with it. Neither
side opens a port: both keep an outgoing WebSocket to their room here.

The relay is trusted with nothing. Every frame is sealed end to end between
the computer's key and the phone's (the protocol is described in
[`crates/splitlane-remote`](../crates/splitlane-remote/src/lib.rs)), and the
relay forwards it without reading it. What it can see is which room a frame
is for, how long it is and when it was sent.

## Rooms

A room is a Durable Object, one per paired phone, addressed by 16 random
bytes. It holds one machine socket and at most two phone sockets.

- **The machine role is claimed.** The first machine to join a room sends a
  join token (`x-splitlane-join`, derived from the machine's key and the
  room) and the room keeps its SHA-256. Later machine joins must show the
  same token. The room's address is in the QR code shown for pairing, so
  without this, whoever photographed one could take the computer's place.
  The computer joins before it shows the code, so nobody else knows the
  address while it is unclaimed.
- **A phone may join only a claimed room.** A third phone pushes out the
  oldest.
- **Limits:** binary frames only, 64 KiB each; 20 frames a second per socket
  with a burst of 100; 60 connections and 10 new rooms a minute per address.
  A socket over a limit is closed.
- **Stored:** the token's hash and when the machine last joined. A room the
  machine has not joined for 180 days is deleted. Logs name events, never a
  room or an address.

## Test and deploy

```bash
cd relay
node --test test/relay.test.mjs   # runs the Worker in `wrangler dev`
npx wrangler@4.145.0 deploy
```

The app reads `SPLITLANE_RELAY_URL` instead of the default, so a debug build
can be pointed at `ws://127.0.0.1:8787` from `wrangler dev`.
