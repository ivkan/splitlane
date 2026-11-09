# site

The Worker on `splitlane.dev`. Today it does one thing: the phone's page used
to be at `splitlane.dev/app` and is now at
[`app.splitlane.dev`](../web/README.md), so `/app` and everything under it
redirect there. Every other path is a 404 until the site is built here.

| Old address | New address |
|---|---|
| `/app`, `/app/`, `/app/index.html` | `https://app.splitlane.dev/?moved` |
| `/app/<file>` | `https://app.splitlane.dev/<file>` |

Desktop builds from before the move print and QR-encode
`https://splitlane.dev/app#pair=...`. The fragment is never sent to a server,
and a browser carries it across a redirect whose `Location` has none, so such
a link opens the new page ready to pair. `?moved` makes the page say why a
phone paired on the old address has to pair again: a browser keeps storage
per origin, so the old pairing stays behind. The redirect also sends
`Clear-Site-Data` to wipe that storage, since the site that takes this origin
over will run scripts of its own.

## The site

The site goes here, beside the redirect: an `[assets]` block in
`wrangler.toml` with `binding = "ASSETS"` and `run_worker_first = true`, so
that every request passes through `src/index.js`. The redirect then keeps
answering `/app` whatever files the site has (it must not serve anything
under `/app`), and every other response gets the site's Content Security
Policy.

That policy (`SITE_CSP` in `src/index.js`) is strict on purpose: scripts,
styles and media from `splitlane.dev` only, nothing inline, no third-party
script. A phone paired on the old page keeps its keys in this origin's
storage until it opens `/app` again and its browser honours
`Clear-Site-Data`, which Safari does not. On Android an installed page shares
that storage with Chrome, so a script running on `splitlane.dev` could use
those keys - not copy them, they cannot be exported - to reach the computer
as that phone, until the phone is removed in Settings → Phone. Analytics,
embeds or a script CDN can come once pairings from the old page are gone.
The Worker sets the header itself, so a `_headers` file cannot override it.

## Test

```bash
cd site
node --test
```

## Deploy

```bash
npx wrangler@4.145.0 deploy
```

Only after the page is live on `app.splitlane.dev`: until then the redirect
leads nowhere.
