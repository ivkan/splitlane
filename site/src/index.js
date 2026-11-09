// splitlane.dev. The phone's page was served at /app until it moved to an
// origin of its own; desktop builds from before the move still print and
// QR-encode https://splitlane.dev/app#pair=..., and phones still have it on
// their home screens. Those addresses are answered here.
//
// The pairing secret is in the fragment, which a browser never sends, so it
// never reaches this Worker. A Location without a fragment makes the browser
// carry the request's own fragment over to the new address, which is how a
// pairing link survives the redirect.

export const PAGE = "https://app.splitlane.dev";

// The page itself, as the old address had it. These arrive at the new page
// with `?moved`, which tells it to say why an old pairing is gone.
const PAGE_PATHS = new Set(["/app", "/app/", "/app/index.html"]);

/// Where an old address of the page now is, or null for any other path.
export function movedTo(pathname) {
  if (PAGE_PATHS.has(pathname)) return `${PAGE}/?moved`;
  if (pathname.startsWith("/app/")) return `${PAGE}/${pathname.slice("/app/".length)}`;
  return null;
}

/// The redirect for an old address of the page, or null.
export function redirect(request) {
  const to = movedTo(new URL(request.url).pathname);
  if (to === null) return null;
  return new Response(null, {
    // Temporary and never cached, so the rule stays in this Worker's hands
    // and every visit carries the header below.
    status: 302,
    headers: {
      Location: to,
      "Cache-Control": "no-store",
      "Referrer-Policy": "no-referrer",
      // A phone paired on the old page keeps its keys in this origin's
      // storage, and the site that takes the origin over runs script of its
      // own. The keys are of no use here any more, so they are wiped: the
      // page's storage, its service worker and its cached files.
      "Clear-Site-Data": '"cache", "storage"',
    },
  });
}

// Every other response on splitlane.dev carries this policy, set here so
// that no file of the site can loosen it on its own: scripts, styles, media
// and connections from this origin only, nothing inline, nothing framed.
//
// Why so strict on a marketing site. A phone paired on the old page keeps
// its keys in this origin's storage until it opens /app again and its
// browser honours Clear-Site-Data (Safari does not); on Android an
// installed page shares that storage with Chrome. Splitlane on the computer
// revokes every key paired before the move on its first start at that
// version, so those keys open nothing on an updated computer - but a
// computer not yet updated still answers them. Beyond that, the policy
// stands on its own: this origin was the page's, people have its address on
// their phones and in old pairing codes, and the site has no need to run
// anyone else's code. Loosening it is a change to this file, with this
// reason in front of whoever makes it.
export const SITE_CSP =
  "default-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

function withSiteHeaders(response) {
  const out = new Response(response.body, response);
  out.headers.set("Content-Security-Policy", SITE_CSP);
  out.headers.set("X-Content-Type-Options", "nosniff");
  return out;
}

export default {
  async fetch(request, env) {
    const moved = redirect(request);
    if (moved) return moved;
    if (env?.ASSETS) return withSiteHeaders(await env.ASSETS.fetch(request));
    return withSiteHeaders(
      new Response("Not found\n", { status: 404, headers: { "Content-Type": "text/plain; charset=utf-8" } }),
    );
  },
};
