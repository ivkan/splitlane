// The blog's posts and their publication dates: the one place a date is set.
//
// A post's date appears in five places: its page (<time> and
// article:published_time), the list on /blog/, the feed's <pubDate> and the
// sitemap's <lastmod> (the post's and /blog/'s). Change `published` here,
// then write it everywhere:
//
//   node tools/posts.mjs           # rewrites the files that disagree
//   node tools/posts.mjs --check   # lists them, writes nothing
//
// `node --test` fails while any file disagrees with this list.
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

export const ORIGIN = "https://splitlane.dev";

// Newest first.
export const POSTS = [
  {
    slug: "running-claude-code-sessions-at-once",
    title: "Running 4 to 7 Claude Code sessions at once (14 at peak)",
    description:
      "Two Claude Code sessions in two projects are already enough for me to lose the one that is waiting. What I tried, what I dropped, and the rules I keep.",
    published: "2026-11-10",
  },
];

const MONTHS = ["January", "February", "March", "April", "May", "June", "July",
  "August", "September", "October", "November", "December"];

const utc = (date) => new Date(`${date}T00:00:00Z`);
// "11 November 2026", as the site writes dates.
export const longDate = (date) => {
  const d = utc(date);
  return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
};
// RFC 822, as RSS wants it: "Wed, 11 Nov 2026 00:00:00 GMT".
export const rssDate = (date) => utc(date).toUTCString();

const time = (date) => `<time datetime="${date}">${longDate(date)}</time>`;
const TIME = /<time datetime="[^"]*">[^<]*<\/time>/;

// Every file that carries a date, as { file, text } with the dates set from
// POSTS. `root` is site/public.
export function expected(root) {
  const read = (file) => readFileSync(join(root, file), "utf8");
  const out = new Map();
  const edit = (file, change) => out.set(file, change(out.get(file) ?? read(file)));

  for (const post of POSTS) {
    const url = `${ORIGIN}/blog/${post.slug}/`;
    edit(`/blog/${post.slug}/index.html`, (text) =>
      text
        .replace(/(<meta property="article:published_time" content=")[^"]*/, `$1${post.published}`)
        .replace(TIME, time(post.published)));
    edit("/blog/index.html", (text) =>
      text.replace(/<article>[\s\S]*?<\/article>/g, (entry) =>
        entry.includes(`href="/blog/${post.slug}/"`) ? entry.replace(TIME, time(post.published)) : entry));
    edit("/blog/feed.xml", (text) =>
      text.replace(/<item>[\s\S]*?<\/item>/g, (item) =>
        item.includes(`<link>${url}</link>`)
          ? item.replace(/<pubDate>[^<]*<\/pubDate>/, `<pubDate>${rssDate(post.published)}</pubDate>`)
          : item));
    edit("/sitemap.xml", (text) =>
      text.replace(new RegExp(`(<loc>${url}</loc><lastmod>)[^<]*`), `$1${post.published}`));
  }
  if (POSTS.length)
    edit("/sitemap.xml", (text) =>
      text.replace(new RegExp(`(<loc>${ORIGIN}/blog/</loc><lastmod>)[^<]*`), `$1${POSTS[0].published}`));
  return out;
}

// The files whose dates disagree with POSTS; with `write`, fixes them.
export function sync(root, { write = false } = {}) {
  const stale = [];
  for (const [file, text] of expected(root)) {
    if (readFileSync(join(root, file), "utf8") === text) continue;
    stale.push(file);
    if (write) writeFileSync(join(root, file), text);
  }
  return stale;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = join(dirname(dirname(fileURLToPath(import.meta.url))), "public");
  const check = process.argv.includes("--check");
  const stale = sync(root, { write: !check });
  for (const file of stale) console.log(`${check ? "differs" : "updated"}: public${file}`);
  if (!stale.length) console.log("every date matches tools/posts.mjs");
  if (check && stale.length) process.exitCode = 1;
}
