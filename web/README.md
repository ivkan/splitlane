# web

The page a phone pairs with and follows Splitlane on, served at
<https://splitlane.dev/app>. It is static files and nothing else: no build
step, no dependencies, no code of anyone else's. The phone's private key is in
the page's storage, and a script on the page could use it, so the page runs
only its own scripts (see `public/_headers`) and loads nothing from anywhere
but its own origin and the relay.

| File | What it is |
|---|---|
| `public/app/protocol.js` | The protocol of [`crates/splitlane-remote`](../crates/splitlane-remote/src/lib.rs) in WebCrypto: the link, the pairing MAC and code, the channel |
| `public/app/link.js` | The WebSocket to the relay, the handshake, pairing and requests |
| `public/app/store.js` | Paired computers in IndexedDB, each key a non-exportable `CryptoKey` |
| `public/app/app.js` | The screens: pairing, sessions by project, one session's card |
| `public/app/sw.js` | Keeps the page's files so it opens without a network |
| `public/app/markdown.js` | Reads an agent's answer as Markdown into a tree and builds elements from it with text nodes only; links are drawn, not followed |
| `public/app/prompt.js` | Reads a waiting agent's question and numbered choices off the foot of its screen |
| `public/app/version.js` | The page's version, shown on the phone; raise it with every change to the page (`test/version.test.mjs` fails until you do and prints the hash to record) |
| `public/_headers` | Content Security Policy and the other response headers |

What a phone may ask, and what the computer answers, is in
`src-app/src/remote/surface.rs`: `list`, `status`, `read`, `answer`, and with
the right to reply, `send`, `key` and `interrupt`.

## Test

```bash
cd web
node --test
```

The tests check the module against the vectors the Rust crate writes
(`crates/splitlane-remote/tests/vectors.json`), so a change on either side
that breaks the other fails here.

## Try it against a computer

```bash
cd web
npx wrangler@4.145.0 dev --port 8791   # the page at http://127.0.0.1:8791/app/
```

Pair with `splitlane remote pair` and open its link with
`https://splitlane.dev/app` replaced by `http://127.0.0.1:8791/app`. The page
talks to the production relay, the only connection its Content Security
Policy allows.

## Deploy

```bash
npx wrangler@4.145.0 deploy
```

A Worker with static assets alone, on `splitlane.dev` as a custom domain:
Cloudflare keeps its DNS record and certificate. The page is under `app/`;
anything else the domain serves goes beside it in `public/`.
