// The tool that puts a recorded clip into a video slot. Run from site/:
// `node --test`. It runs on the real page, in memory; the clips themselves
// do not exist until they are recorded.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { SLOTS, placeVideo } from "../tools/video-slot.mjs";

const site = dirname(dirname(fileURLToPath(import.meta.url)));
const page = readFileSync(join(site, "public", "index.html"), "utf8");

test("every slot the tool knows is marked on the page exactly once", () => {
  const marked = [...page.matchAll(/data-video-slot="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual([...marked].sort(), Object.keys(SLOTS).sort());
});

for (const slot of Object.keys(SLOTS)) {
  for (const light of [false, true]) {
    test(`${slot}, ${light ? "dark and light" : "dark only"}: the slot becomes a video the policy allows`, () => {
      const out = placeVideo(page, slot, { light });
      const fig = out.match(new RegExp(`<figure[^>]*data-video-slot="${slot}"[\\s\\S]*?</figure>`))[0];
      assert.match(fig, /<video data-autoplay muted loop playsinline controls preload="none"/);
      assert.match(fig, new RegExp(`class="${SLOTS[slot].frame}"`));
      assert.match(fig, /\saria-label="[^"]{40,}"/);
      assert.doesNotMatch(fig, /<img|<picture|\sstyle="|\son[a-z]+="/);
      const refs = [...fig.matchAll(/\s(?:src|poster|data-poster-dark)="([^"]*)"/g)].map((m) => m[1]);
      const want = light
        ? [`/video/${slot}-light-poster.webp`, `/video/${slot}-dark-poster.webp`, `/video/${slot}-dark.mp4`, `/video/${slot}-light.mp4`]
        : [`/video/${slot}-dark-poster.webp`, `/video/${slot}-dark.mp4`];
      assert.deepEqual(refs, want);
      if (light) assert.match(fig, /<source media="\(prefers-color-scheme: dark\)" src="\/video\/[a-z]+-dark\.mp4"/);
      // Only the slot changed: the rest of the page is byte for byte the same.
      const before = page.split(/<(?:figure|div)\b[^>]*data-video-slot="[^"]+"/).length;
      assert.equal(out.split(/<(?:figure|div)\b[^>]*data-video-slot="[^"]+"/).length, before);
      for (const other of Object.keys(SLOTS).filter((s) => s !== slot))
        assert.equal(out.includes(`data-video-slot="${other}"`), true);
      assert.ok(out.length > page.length / 2);
    });
  }
}

test("the host panel, all of it, gives way to the video", () => {
  const out = placeVideo(page, "host", { light: false });
  assert.doesNotMatch(out, /Keeps running/);
  assert.match(out, /Close the window\. The agents keep working\./);
  assert.match(out, /<\/video>\n\s*<\/figure>\n\s*<\/div>\n<\/section>/);
});

test("an unknown slot is refused", () => {
  assert.throws(() => placeVideo(page, "nope", { light: false }), /no slot named nope/);
});
