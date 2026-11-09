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
over may run scripts of its own.

## The site

The site goes here, beside the redirect: an `[assets]` block in
`wrangler.toml` with `run_worker_first = ["/app", "/app/*"]`, so that the
redirect keeps answering those paths whatever files the site has. It must not
serve anything under `/app`.

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
