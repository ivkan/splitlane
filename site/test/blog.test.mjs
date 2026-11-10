// The blog's posts: each one's page, its entry on /blog/, in the feed and in
// the sitemap, all agreeing with tools/posts.mjs. Run from site/:
// `node --test`. site.test.mjs covers what every page shares (the policy,
// meta, files referred to, no inline code); this file covers what a post adds.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import worker from "../src/index.js";
import { SITE_CSP } from "../src/site.js";
import { POSTS, ORIGIN, sync, longDate, rssDate } from "../tools/posts.mjs";

const root = join(dirname(dirname(fileURLToPath(import.meta.url))), "public");
const read = (file) => readFileSync(join(root, file), "utf8");
const meta = (page, attr, name) =>
  page.match(new RegExp(`<meta ${attr}="${name}" content="([^"]*)"`))?.[1];
const escape = (text) => text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

test("tools/posts.mjs lists at least one post, newest first, with real dates", () => {
  assert.ok(POSTS.length > 0);
  for (const post of POSTS) {
    assert.match(post.slug, /^[a-z0-9]+(-[a-z0-9]+)*$/, post.slug);
    assert.match(post.published, /^\d{4}-\d{2}-\d{2}$/, post.slug);
    assert.ok(!Number.isNaN(Date.parse(post.published)), post.slug);
  }
  const dates = POSTS.map((p) => p.published);
  assert.deepEqual(dates, [...dates].sort().reverse());
  assert.equal(longDate("2026-11-04"), "4 November 2026");
  assert.equal(rssDate("2026-11-11"), "Wed, 11 Nov 2026 00:00:00 GMT");
});

test("every date on the site matches tools/posts.mjs", () => {
  assert.deepEqual(sync(root), [], "run `node tools/posts.mjs` to write the dates from tools/posts.mjs");
});

for (const post of POSTS) {
  const path = `/blog/${post.slug}/`;
  const url = ORIGIN + path;

  test(`${post.slug}: served with the policy, and its title and description everywhere`, async () => {
    const ASSETS = { fetch: async () => new Response(read(path + "index.html"), { headers: { "Content-Type": "text/html; charset=utf-8" } }) };
    const response = await worker.fetch(new Request(url), { ASSETS });
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("Content-Security-Policy"), SITE_CSP);

    const page = read(path + "index.html");
    const title = escape(post.title);
    const description = escape(post.description);
    assert.ok(page.includes(`<title>${title}</title>`));
    assert.ok(page.includes(`<h1 id="title">${title}</h1>`));
    assert.equal(meta(page, "name", "description"), description);
    for (const [attr, prefix] of [["property", "og"], ["name", "twitter"]]) {
      assert.equal(meta(page, attr, `${prefix}:title`), title, prefix);
      assert.equal(meta(page, attr, `${prefix}:description`), description, prefix);
    }
    assert.equal(meta(page, "property", "og:type"), "article");
    assert.equal(meta(page, "property", "article:published_time"), post.published);
    assert.ok(page.includes(`<time datetime="${post.published}">${longDate(post.published)}</time>`));
    assert.ok(page.includes('href="/blog/feed.xml"'));
  });

  test(`${post.slug}: on /blog/, in the feed and in the sitemap`, () => {
    const entry = read("/blog/index.html").match(new RegExp(`<article>((?:(?!</article>)[\\s\\S])*href="${path}"[\\s\\S]*?)</article>`))?.[1];
    assert.ok(entry, "listed on /blog/");
    assert.ok(entry.includes(`<time datetime="${post.published}">`));
    assert.ok(entry.includes(escape(post.title)));

    const item = read("/blog/feed.xml").match(new RegExp(`<item>((?:(?!</item>)[\\s\\S])*<link>${url}</link>[\\s\\S]*?)</item>`))?.[1];
    assert.ok(item, "in the feed");
    assert.ok(item.includes(`<title>${escape(post.title)}</title>`));
    assert.ok(item.includes(`<guid isPermaLink="true">${url}</guid>`));
    assert.ok(item.includes(`<pubDate>${rssDate(post.published)}</pubDate>`));
    assert.ok(item.includes(`<description>${escape(post.description)}</description>`));

    assert.ok(read("/sitemap.xml").includes(`<url><loc>${url}</loc><lastmod>${post.published}</lastmod></url>`));
  });

  test(`${post.slug}: no draft left in it, and outside links are well formed`, () => {
    const page = read(path + "index.html");
    const article = page.slice(page.indexOf("<main"), page.indexOf("</main>"));
    assert.doesNotMatch(article, /<!--/, "an HTML comment from the draft");
    assert.doesNotMatch(article, /\]\(|```|^#+ /m, "markdown left unconverted");
    // A post does not vouch for discussion threads it quotes.
    for (const [tag] of article.matchAll(/<a\b[^>]*href="https?:\/\/(?:[a-z]+\.)?reddit\.com[^"]*"[^>]*>/g))
      assert.match(tag, /\srel="[^"]*\bnofollow\b/, tag);
    for (const [, href] of article.matchAll(/<a\b[^>]*\shref="([^"]*)"/g)) {
      if (href.startsWith("/")) continue;
      assert.ok(URL.canParse(href) && new URL(href).protocol === "https:", href);
    }
    // In-page anchors point at an id on the page.
    for (const [, id] of article.matchAll(/href="#([^"]+)"/g)) assert.ok(page.includes(`id="${id}"`), id);
    // JSON-LD would need an inline <script>, which the policy refuses.
    assert.doesNotMatch(page, /application\/ld\+json/);
  });
}

// Width and height from a PNG's IHDR chunk.
const pngSize = (buffer) => {
  assert.equal(buffer.subarray(1, 4).toString("latin1"), "PNG");
  return [buffer.readUInt32BE(16), buffer.readUInt32BE(20)];
};

test("each post has its own 1280x640 social preview, and its meta says so", () => {
  for (const post of POSTS) {
    const path = `/blog/${post.slug}/`;
    const page = read(path + "index.html");
    const image = `${ORIGIN}${path}preview.png`;
    const file = join(root, path, "preview.png");
    assert.ok(existsSync(file), `${post.slug}: run tools/post-preview.sh`);
    assert.deepEqual(pngSize(readFileSync(file)), [1280, 640], post.slug);
    assert.equal(meta(page, "property", "og:image"), image, post.slug);
    assert.equal(meta(page, "name", "twitter:image"), image, post.slug);
    assert.equal(meta(page, "property", "og:image:width"), "1280", post.slug);
    assert.equal(meta(page, "property", "og:image:height"), "640", post.slug);
    const alt = meta(page, "property", "og:image:alt") ?? "";
    assert.ok(alt.length >= 30 && !alt.startsWith("Splitlane with several"), `${post.slug}: og:image:alt is the post's own`);
    assert.equal(meta(page, "name", "twitter:image:alt"), alt, post.slug);
  }
});

test("a post's video has its clips and posters on disk", () => {
  for (const post of POSTS) {
    const page = read(`/blog/${post.slug}/index.html`);
    for (const [, file] of page.matchAll(/\s(?:src|poster|data-poster-dark)="(\/video\/[^"]+)"/g))
      assert.ok(existsSync(join(root, file)), `${post.slug}: ${file}`);
  }
});
