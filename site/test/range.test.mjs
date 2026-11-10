// Byte ranges on /video/*.mp4. Run from site/: `node --test`.
//
// ASSETS here answers as the real binding was seen to on splitlane.dev: the
// whole file with 200, whatever `Range` asked for.
import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../src/index.js";
import { SITE_CSP, parseRange } from "../src/site.js";

const ORIGIN = "https://splitlane.dev";
const BODY = Uint8Array.from({ length: 1000 }, (_, i) => i % 251);

const ASSETS = {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/video/demo.mp4" || path === "/img/demo.webp")
      return new Response(BODY, {
        headers: { "Content-Type": "video/mp4", "Content-Length": String(BODY.length), ETag: '"v1"' },
      });
    return new Response("Page not found", { status: 404, headers: { "Content-Type": "text/html" } });
  },
};
const get = (path, headers = {}, method = "GET") =>
  worker.fetch(new Request(ORIGIN + path, { method, headers }), { ASSETS });
const bytes = async (response) => new Uint8Array(await response.arrayBuffer());

test("parseRange reads the forms browsers send", () => {
  assert.deepEqual(parseRange("bytes=0-", 1000), [0, 999]);
  assert.deepEqual(parseRange("bytes=0-1", 1000), [0, 1]);
  assert.deepEqual(parseRange("bytes=100-199", 1000), [100, 199]);
  assert.deepEqual(parseRange("bytes=900-5000", 1000), [900, 999]);
  assert.deepEqual(parseRange("bytes=-100", 1000), [900, 999]);
  assert.deepEqual(parseRange("bytes=-5000", 1000), [0, 999]);
  assert.equal(parseRange("bytes=1000-", 1000), "unsatisfiable");
  assert.equal(parseRange("bytes=-0", 1000), "unsatisfiable");
  for (const ignored of [null, "", "bytes=", "bytes=-", "bytes=5-3", "items=0-1", "bytes=0-1,5-9", "bytes=a-b"])
    assert.equal(parseRange(ignored, 1000), null, String(ignored));
});

test("a range of a video is answered with 206 and just those bytes", async () => {
  const response = await get("/video/demo.mp4", { Range: "bytes=100-199" });
  assert.equal(response.status, 206);
  assert.equal(response.headers.get("Content-Range"), "bytes 100-199/1000");
  assert.equal(response.headers.get("Content-Length"), "100");
  assert.equal(response.headers.get("Accept-Ranges"), "bytes");
  assert.equal(response.headers.get("Content-Type"), "video/mp4");
  assert.equal(response.headers.get("ETag"), '"v1"');
  assert.equal(response.headers.get("Content-Security-Policy"), SITE_CSP);
  assert.deepEqual(await bytes(response), BODY.slice(100, 200));
});

test("Safari's opening probe, bytes=0-1, gets two bytes", async () => {
  const response = await get("/video/demo.mp4", { Range: "bytes=0-1" });
  assert.equal(response.status, 206);
  assert.equal(response.headers.get("Content-Range"), "bytes 0-1/1000");
  assert.deepEqual(await bytes(response), BODY.slice(0, 2));
});

test("an open-ended and a suffix range reach the end of the file", async () => {
  const open = await get("/video/demo.mp4", { Range: "bytes=990-" });
  assert.equal(open.headers.get("Content-Range"), "bytes 990-999/1000");
  assert.deepEqual(await bytes(open), BODY.slice(990));
  const suffix = await get("/video/demo.mp4", { Range: "bytes=-10" });
  assert.equal(suffix.headers.get("Content-Range"), "bytes 990-999/1000");
  assert.deepEqual(await bytes(suffix), BODY.slice(990));
});

test("a range past the end is 416, with the size", async () => {
  const response = await get("/video/demo.mp4", { Range: "bytes=1000-" });
  assert.equal(response.status, 416);
  assert.equal(response.headers.get("Content-Range"), "bytes */1000");
  assert.equal(response.headers.get("Content-Security-Policy"), SITE_CSP);
});

test("no range, or one it does not take, is the whole video, saying ranges work", async () => {
  for (const headers of [{}, { Range: "bytes=0-1,5-9" }, { Range: "lines=1-2" }]) {
    const response = await get("/video/demo.mp4", headers);
    assert.equal(response.status, 200, JSON.stringify(headers));
    assert.equal(response.headers.get("Accept-Ranges"), "bytes");
    assert.equal(response.headers.get("Content-Range"), null);
    assert.deepEqual(await bytes(response), BODY);
  }
});

test("a 206 the binding made itself passes through", async () => {
  const ranged = {
    async fetch() {
      return new Response(BODY.slice(0, 2), {
        status: 206,
        headers: { "Content-Range": "bytes 0-1/1000", "Content-Type": "video/mp4" },
      });
    },
  };
  const response = await worker.fetch(
    new Request(ORIGIN + "/video/demo.mp4", { headers: { Range: "bytes=0-1" } }),
    { ASSETS: ranged },
  );
  assert.equal(response.status, 206);
  assert.equal(response.headers.get("Content-Range"), "bytes 0-1/1000");
  assert.deepEqual(await bytes(response), BODY.slice(0, 2));
});

test("only videos are cut; a missing video stays the 404 page", async () => {
  const image = await get("/img/demo.webp", { Range: "bytes=0-1" });
  assert.equal(image.status, 200);
  assert.equal(image.headers.get("Accept-Ranges"), null);
  assert.equal((await bytes(image)).length, 1000);
  const missing = await get("/video/none.mp4", { Range: "bytes=0-1" });
  assert.equal(missing.status, 404);
  assert.equal(missing.headers.get("Content-Range"), null);
});
