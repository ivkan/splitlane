# web

The page a phone pairs with and follows Splitlane on, served at
<https://app.splitlane.dev/>. It is static files and nothing else: no build
step, no dependencies, no code of anyone else's. The phone's private key is in
the page's storage, and a script on the page could use it, so the page runs
only its own scripts (see `public/_headers`) and loads nothing from anywhere
but its own origin and the relay. That origin is the page's alone: the site
on `splitlane.dev` may run whatever script it needs without reaching the key.

| File | What it is |
|---|---|
| `public/protocol.js` | The protocol of [`crates/splitlane-remote`](../crates/splitlane-remote/src/lib.rs) in WebCrypto: the link, the pairing MAC and code, the channel |
| `public/link.js` | The WebSocket to the relay, the handshake, pairing and requests |
| `public/store.js` | Paired computers in IndexedDB, each key a non-exportable `CryptoKey` |
| `public/app.js` | The screens: pairing, sessions by project, one session's card |
| `public/sw.js` | Keeps the page's files so it opens without a network |
| `public/markdown.js` | Reads an agent's answer as Markdown into a tree and builds elements from it with text nodes only; links are drawn, not followed |
| `public/prompt.js` | Reads a waiting agent's question and numbered choices off the foot of its screen |
| `public/version.js` | The page's version, shown on the phone; raise it with every change to the page (`test/version.test.mjs` fails until you do and prints the hash to record) |
| `public/_headers` | Content Security Policy and the other response headers |

What a phone may ask, and what the computer answers, is in
`src-app/src/remote/surface.rs`: `list`, `status`, `read`, `answer`, and with
the right to reply, `send`, `key`, `interrupt` and `open`, which starts a
session.

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
npx wrangler@4.145.0 dev --port 8791   # the page at http://127.0.0.1:8791/
```

Pair with `splitlane remote pair` and open its link with
`https://app.splitlane.dev/` replaced by `http://127.0.0.1:8791/`. The page
talks to the production relay, the only connection its Content Security
Policy allows.

## Deploy

```bash
npx wrangler@4.145.0 deploy
```

A Worker with static assets alone, on `app.splitlane.dev` as a custom domain:
Cloudflare keeps its DNS record and certificate. Nothing else goes on this
origin. The page used to be at `splitlane.dev/app`; the redirect that keeps
its old links working is in [`../site`](../site/README.md).
