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

// Byte ranges for the site's videos. Safari plays a <video> only from a
// server that answers `Range` with 206, and seeking anywhere needs it too.
// The asset binding answers a range request with the whole file and 200
// (checked on splitlane.dev against a 160 KB image), so the Worker cuts the
// range itself. Only one range per request, as browsers ask for media; a
// multi-range request gets the whole file, which HTTP allows. The videos are
// a few megabytes, so holding one in memory to cut it is cheap. Should the
// binding start answering ranges itself, its 206 passes through untouched.
const RANGED = /^\/video\/[^/]+\.mp4$/;

/// The single byte range `header` asks of a body `size` bytes long, as
/// inclusive [start, end]; "unsatisfiable" when it lies wholly past the end;
/// null when there is no usable range and the whole body should be sent.
export function parseRange(header, size) {
  const m = /^bytes=(\d*)-(\d*)$/.exec((header ?? "").trim());
  if (!m || (m[1] === "" && m[2] === "")) return null;
  if (m[1] === "") {
    // The last N bytes.
    const n = Number(m[2]);
    if (n === 0) return "unsatisfiable";
    return [Math.max(0, size - n), size - 1];
  }
  const start = Number(m[1]);
  const end = m[2] === "" ? size - 1 : Math.min(Number(m[2]), size - 1);
  if (m[2] !== "" && Number(m[2]) < start) return null;
  if (start >= size) return "unsatisfiable";
  return [start, end];
}

/// A video response answering the request's `Range`, or the response as it is.
export async function withRange(request, response) {
  if (!RANGED.test(new URL(request.url).pathname)) return response;
  if (response.status !== 200 || response.headers.has("Content-Range")) return response;
  const headers = new Headers(response.headers);
  headers.set("Accept-Ranges", "bytes");
  const header = request.headers.get("Range");
  if (!header || request.method !== "GET") return new Response(response.body, { status: 200, headers });
  const body = new Uint8Array(await response.arrayBuffer());
  const range = parseRange(header, body.length);
  if (range === null) return new Response(body, { status: 200, headers });
  headers.delete("Content-Length");
  if (range === "unsatisfiable") {
    headers.set("Content-Range", `bytes */${body.length}`);
    return new Response(null, { status: 416, headers });
  }
  const [start, end] = range;
  headers.set("Content-Range", `bytes ${start}-${end}/${body.length}`);
  headers.set("Content-Length", String(end - start + 1));
  return new Response(body.slice(start, end + 1), { status: 206, headers });
}

/// Every request to splitlane.dev: an old address of the phone page is sent
/// on, anything else is the site's file (or its 404 page) with the policy.
export async function handle(request, env) {
  const moved = redirect(request);
  if (moved) return moved;
  if (env?.ASSETS) return withSiteHeaders(await withRange(request, await env.ASSETS.fetch(request)));
  return withSiteHeaders(
    new Response("Not found\n", { status: 404, headers: { "Content-Type": "text/plain; charset=utf-8" } }),
  );
}
