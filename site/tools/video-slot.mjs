#!/usr/bin/env node
// Put a recorded clip into one of the page's video slots.
//
//   node tools/video-slot.mjs <slot> [--check]
//
// A slot is an element of public/index.html marked `data-video-slot="<slot>"`:
// today a screenshot (or, for `host`, a panel of facts) in a frame of fixed
// aspect ratio. Once tools/build-media.sh --video has written the clip as
// public/video/<slot>-dark.mp4 (and, where the slot has one, <slot>-light.mp4),
// this replaces the slot's contents with a <video> of the same frame, so
// nothing on the page moves. Each clip needs its <name>-poster.webp beside it.
//
// The light clip is optional: without it both page themes play the dark one.
// With both, the browser picks the clip by `prefers-color-scheme` on
// <source media>, and public/js/site.js picks the matching poster, which an
// attribute cannot do by itself.
//
// --check says what would be written and writes nothing.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// What each slot shows, in the words a screen reader says in place of the
// picture. They describe the clips record-demo makes; change them with it.
export const SLOTS = {
  hero: {
    frame: "media r-desktop hero-shot",
    label:
      "Splitlane with three agent sessions in one project, two Claude Code and one Codex. They start on their tasks; the right-hand one stops on a permission prompt to run npm test, and its pane header, its row in the rail and the title bar all change to say it is waiting for you.",
  },
  state: {
    frame: "media r-desktop section-media detail",
    label:
      "A detail of the Splitlane window: an agent pane stops on a permission prompt, its header changes to 'waiting for you', and the title bar's count changes with it.",
  },
  grid: {
    frame: "media r-desktop section-media",
    label:
      "Splitlane's row of three agent panes becomes a two-by-two grid, and the fourth pane opens on the project's diff with its changed-files column.",
  },
  fleet: {
    frame: "media r-wide section-media",
    label:
      "A Claude Code session named plan opens three sessions in Splitlane and hands each a task. They appear under it in the rail and in panes beside it, each pane saying who opened it, and the one with no room left waits in the rail; the one told to run the tests stops on a permission prompt and says it is waiting for you.",
  },
  host: {
    frame: "media r-desktop",
    label:
      "Three agents at work in Splitlane. The window closes; a moment later Splitlane opens again and every session is in its pane, still working.",
  },
};

const WIDTH = 1280;
// The recording's own frame: a 1440x1000 window for r-desktop, 1600x824 for
// r-wide, scaled to 1280 wide and rounded to even, as build-media.sh does.
const HEIGHT = { "r-desktop": 888, "r-wide": 658 };

/// The <video> markup for a slot, given which clips exist.
export function videoMarkup(slot, { light }, indent = "    ") {
  const spec = SLOTS[slot];
  const ratio = spec.frame.includes("r-wide") ? "r-wide" : "r-desktop";
  const i = indent;
  const lines = [
    `${i}<figure class="${spec.frame}" data-video-slot="${slot}">`,
    `${i}  <video data-autoplay muted loop playsinline controls preload="none"`,
    `${i}    width="${WIDTH}" height="${HEIGHT[ratio]}"`,
    light
      ? `${i}    poster="/video/${slot}-light-poster.webp" data-poster-dark="/video/${slot}-dark-poster.webp"`
      : `${i}    poster="/video/${slot}-dark-poster.webp"`,
    `${i}    aria-label="${spec.label.replaceAll('"', "&quot;")}">`,
  ];
  if (light) {
    lines.push(`${i}    <source media="(prefers-color-scheme: dark)" src="/video/${slot}-dark.mp4" type="video/mp4">`);
    lines.push(`${i}    <source src="/video/${slot}-light.mp4" type="video/mp4">`);
  } else {
    lines.push(`${i}    <source src="/video/${slot}-dark.mp4" type="video/mp4">`);
  }
  lines.push(`${i}  </video>`, `${i}</figure>`);
  return lines.join("\n");
}

/// The page with one slot's element replaced by its video.
export function placeVideo(html, slot, clips) {
  if (!SLOTS[slot]) throw new Error(`no slot named ${slot}; slots: ${Object.keys(SLOTS).join(", ")}`);
  // Slots are a <figure> or a <div class="panel">; neither nests its own tag.
  const open = new RegExp(`^([ \\t]*)<(figure|div)\\b[^>]*\\sdata-video-slot="${slot}"[^>]*>`, "m");
  const m = open.exec(html);
  if (!m) throw new Error(`public/index.html has no element with data-video-slot="${slot}"`);
  const close = `</${m[2]}>`;
  const end = html.indexOf(close, m.index + m[0].length);
  if (end < 0) throw new Error(`the ${slot} slot is never closed`);
  return html.slice(0, m.index) + videoMarkup(slot, clips, m[1]) + html.slice(end + close.length);
}

function main(argv) {
  const [slot, flag] = argv;
  const site = dirname(dirname(fileURLToPath(import.meta.url)));
  const pub = join(site, "public");
  if (!slot || !SLOTS[slot]) {
    console.error(`usage: node tools/video-slot.mjs <${Object.keys(SLOTS).join("|")}> [--check]`);
    return 2;
  }
  const has = (name) => existsSync(join(pub, "video", name));
  const missing = [`${slot}-dark.mp4`, `${slot}-dark-poster.webp`].filter((f) => !has(f));
  if (missing.length) {
    console.error(`missing in public/video: ${missing.join(", ")} - run tools/build-media.sh --video first`);
    return 1;
  }
  const light = has(`${slot}-light.mp4`);
  if (light && !has(`${slot}-light-poster.webp`)) {
    console.error(`public/video/${slot}-light.mp4 has no ${slot}-light-poster.webp`);
    return 1;
  }
  const page = join(pub, "index.html");
  const out = placeVideo(readFileSync(page, "utf8"), slot, { light });
  if (flag === "--check") {
    console.log(videoMarkup(slot, { light }));
    return 0;
  }
  writeFileSync(page, out);
  console.log(`public/index.html: the ${slot} slot now plays ${light ? "the dark and light clips" : "the dark clip"}`);
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exit(main(process.argv.slice(2)));
