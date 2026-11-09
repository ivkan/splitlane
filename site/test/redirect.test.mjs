// The redirect from the phone page's old address. Run from site/: `node --test`.
import { test } from "node:test";
import assert from "node:assert/strict";

import worker, { PAGE } from "../src/index.js";

const get = (path) => worker.fetch(new Request(`https://splitlane.dev${path}`), {});

test("the old page goes to the new one, saying it moved", async () => {
  for (const path of ["/app", "/app/", "/app/index.html", "/app?x=1"]) {
    const response = await get(path);
    assert.equal(response.status, 302, path);
    assert.equal(response.headers.get("Location"), `${PAGE}/?moved`, path);
  }
});

test("the page's files go to the same files at the new address", async () => {
  for (const [path, to] of [
    ["/app/app.js", `${PAGE}/app.js`],
    ["/app/sw.js", `${PAGE}/sw.js`],
    ["/app/manifest.webmanifest", `${PAGE}/manifest.webmanifest`],
    ["/app/vendor/jsQR.js", `${PAGE}/vendor/jsQR.js`],
  ]) {
    assert.equal((await get(path)).headers.get("Location"), to, path);
  }
});

test("a pairing link keeps its fragment and nothing of it reaches the server", async () => {
  const link = "https://splitlane.dev/app#pair=1.room.machine.secret.1800000300";
  // A browser never sends the fragment, so the Worker sees the link without
  // it; and given one anyway, it builds the new address from the path alone.
  for (const seen of [link.split("#")[0], link]) {
    const location = (await worker.fetch(new Request(seen), {})).headers.get("Location");
    assert.ok(!location.includes("#") && !location.includes("secret"), location);
  }
  const response = await worker.fetch(new Request(link.split("#")[0]), {});
  const location = response.headers.get("Location");
  // A Location with no fragment of its own takes the request's (RFC 9110,
  // 10.2.2), so the phone lands on the new page with the pairing link.
  assert.ok(!location.includes("#"), location);
  const landed = new URL(location);
  landed.hash = new URL(link).hash;
  assert.equal(landed.href, `${PAGE}/?moved#pair=1.room.machine.secret.1800000300`);
});

test("the old origin's storage is wiped and the redirect is never cached", async () => {
  const response = await get("/app/");
  assert.equal(response.headers.get("Clear-Site-Data"), '"cache", "storage"');
  assert.equal(response.headers.get("Cache-Control"), "no-store");
});

test("everything else is left to the site", async () => {
  for (const path of ["/", "/application", "/apps/", "/docs/app/"]) {
    assert.equal((await get(path)).status, 404, path);
  }
  const served = await worker.fetch(new Request("https://splitlane.dev/"), {
    ASSETS: { fetch: async () => new Response("site", { status: 200 }) },
  });
  assert.equal(await served.text(), "site");
});
