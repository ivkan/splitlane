// The site's files, served the way the Worker serves them. Run from site/:
// `node --test`.
//
// ASSETS here is a small stand-in for Cloudflare's asset binding, set up as
// wrangler.toml sets it (auto-trailing-slash, 404-page), reading public/
// from disk. What these tests check is what the Worker and the files do with
// it: the policy on every response, the 404 page, and that no file loads
// anything from another origin or relies on inline code the policy refuses.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { join, relative, extname, dirname, posix } from "node:path";
import { fileURLToPath } from "node:url";

import worker from "../src/index.js";
import { SITE_CSP } from "../src/site.js";

const site = dirname(dirname(fileURLToPath(import.meta.url)));
const root = join(site, "public");
const ORIGIN = "https://splitlane.dev";

const files = (function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });
})(root).map((path) => "/" + relative(root, path).split("\\").join("/"));

const TEXT = new Set([".html", ".css", ".js", ".xml", ".txt", ".webmanifest", ".svg", ".json"]);
const textFiles = files.filter((f) => TEXT.has(extname(f)));
const htmlFiles = files.filter((f) => f.endsWith(".html"));
const read = (file) => readFileSync(join(root, file), "utf8");
const isFile = (file) => existsSync(join(root, file)) && statSync(join(root, file)).isFile();

const TYPES = { ".html": "text/html; charset=utf-8", ".css": "text/css", ".js": "text/javascript" };
const ASSETS = {
  async fetch(request) {
    const path = decodeURIComponent(new URL(request.url).pathname);
    const type = (f) => ({ "Content-Type": TYPES[extname(f)] ?? "application/octet-stream" });
    if (path.endsWith("/") && isFile(path + "index.html"))
      return new Response(read(path + "index.html"), { headers: type(".html") });
    if (!path.endsWith("/") && !path.endsWith(".html") && isFile(path))
      return new Response(readFileSync(join(root, path)), { headers: type(path) });
    if (!path.endsWith("/") && isFile(path + "/index.html"))
      return new Response(null, { status: 307, headers: { Location: path + "/" } });
    return new Response(read("/404.html"), { status: 404, headers: type(".html") });
  },
};
const serve = (path) => worker.fetch(new Request(ORIGIN + path), { ASSETS });

// Every address a file of the site is served at.
const addresses = files.map((f) => f.replace(/(^|\/)index\.html$/, "$1")).filter((a) => a !== "/404.html");

test("wrangler.toml serves public/ through the Worker, with a 404 page", () => {
  const toml = readFileSync(join(site, "wrangler.toml"), "utf8");
  const assets = toml.slice(toml.indexOf("[assets]"));
  assert.ok(toml.includes("[assets]"));
  assert.match(assets, /^directory\s*=\s*"\.\/public"/m);
  assert.match(assets, /^binding\s*=\s*"ASSETS"/m);
  assert.match(assets, /^run_worker_first\s*=\s*true/m);
  assert.match(assets, /^not_found_handling\s*=\s*"404-page"/m);
  assert.ok(isFile("/404.html"));
});

test("every page and every asset carries the policy", async () => {
  assert.ok(addresses.includes("/") && addresses.includes("/css/site.css") && addresses.includes("/img/hero-light-1440.webp"));
  for (const address of addresses) {
    const response = await serve(address);
    assert.equal(response.status, 200, address);
    assert.equal(response.headers.get("Content-Security-Policy"), SITE_CSP, address);
    assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff", address);
  }
});

test("a missing page is the site's 404 page, with status 404 and the policy", async () => {
  for (const path of ["/nope", "/blog/no-such-post/", "/img/missing.webp"]) {
    const response = await serve(path);
    assert.equal(response.status, 404, path);
    assert.equal(response.headers.get("Content-Security-Policy"), SITE_CSP, path);
    assert.match(await response.text(), /Page not found/, path);
  }
  const page = read("/404.html");
  assert.match(page, /<meta name="robots" content="noindex">/);
  assert.doesNotMatch(page, /rel="canonical"/);
});

test("nothing is served under /app, whatever the files are", async () => {
  assert.ok(!files.some((f) => f === "/app" || f.startsWith("/app/")));
  const response = await serve("/app/index.html");
  assert.equal(response.status, 302);
});

// Everything a browser fetches by itself while showing a file: the places
// where another origin's URL would load something into this one. Links a
// person follows (<a href>) may point anywhere.
function loads(file, text) {
  const found = [];
  const add = (url) => found.push(url.trim());
  if (file.endsWith(".html")) {
    for (const [, url] of text.matchAll(/\s(?:src|poster|data|data-poster-dark)="([^"]*)"/g)) add(url);
    for (const [, set] of text.matchAll(/\ssrcset="([^"]*)"/g))
      for (const part of set.split(",")) add(part.trim().split(/\s+/)[0]);
    for (const [tag] of text.matchAll(/<link\b[^>]*>/g)) {
      const href = tag.match(/\shref="([^"]*)"/);
      if (href && !/rel="(canonical|alternate)"/.test(tag)) add(href[1]);
    }
    for (const [tag] of text.matchAll(/<meta\b[^>]*>/g)) {
      const content = tag.match(/\scontent="(https?:[^"]*)"/);
      if (content) add(content[1]);
    }
  }
  if (file.endsWith(".css")) {
    for (const [, url] of text.matchAll(/url\(\s*["']?([^"')]+)/g)) add(url);
    for (const [, url] of text.matchAll(/@import\s+["']([^"']+)/g)) add(url);
  }
  if (file.endsWith(".js")) {
    for (const [url] of text.matchAll(/\b(?:https?:)?\/\/[^\s"'`)]+/g)) add(url);
    for (const [, url] of text.matchAll(/\bimport\s*\(?\s*["']([^"']+)/g)) add(url);
  }
  return found;
}

const local = (url) => url.startsWith("/") && !url.startsWith("//");
assert.ok(!local("//cdn.example.com/x.js") && local("/x.js"));
const ours = (url) => local(url) || url === ORIGIN || url.startsWith(ORIGIN + "/");

test("no file loads anything from another origin", () => {
  for (const file of textFiles) {
    for (const url of loads(file, read(file))) {
      assert.ok(ours(url), `${file} loads ${url}`);
    }
  }
  // And no stylesheet or script names a third-party host at all.
  for (const file of textFiles.filter((f) => /\.(css|js)$/.test(f))) {
    assert.doesNotMatch(read(file), /https?:\/\//, file);
  }
});

test("no page relies on inline script or style, which the policy refuses", () => {
  for (const file of htmlFiles) {
    const page = read(file);
    for (const [tag] of page.matchAll(/<script\b[^>]*>/g)) assert.match(tag, /\ssrc="/, `${file}: ${tag}`);
    assert.doesNotMatch(page, /<style\b/, file);
    assert.doesNotMatch(page, /\sstyle="/, file);
    assert.doesNotMatch(page, /\son[a-z]+="/, file);
    assert.doesNotMatch(page, /href="javascript:/, file);
  }
});

test("every file a page refers to on this site exists", () => {
  for (const file of [...htmlFiles, ...textFiles.filter((f) => f.endsWith(".css"))]) {
    const text = read(file);
    const refs = [
      ...loads(file, text).filter(local),
      ...[...text.matchAll(/<a\b[^>]*\shref="(\/[^"#?]*)/g)].map((m) => m[1]),
    ];
    for (const ref of refs) {
      const path = posix.normalize(ref.split(/[?#]/)[0]);
      const exists = path.endsWith("/") ? isFile(path + "index.html") : isFile(path);
      assert.ok(exists, `${file} refers to ${ref}`);
    }
  }
});

test("every indexable page has its meta, and is in the sitemap", () => {
  const sitemap = read("/sitemap.xml");
  const listed = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
  const pages = htmlFiles.filter((f) => f !== "/404.html");
  for (const file of pages) {
    const page = read(file);
    const url = ORIGIN + file.replace(/index\.html$/, "");
    const meta = (attr, name) => page.match(new RegExp(`<meta ${attr}="${name}" content="([^"]*)"`))?.[1];
    assert.match(page, /<html lang="en">/, file);
    assert.match(page, /<title>[^<]{10,70}<\/title>/, file);
    assert.ok((meta("name", "description") ?? "").length >= 50, `${file}: description`);
    assert.ok(page.includes(`<link rel="canonical" href="${url}">`), `${file}: canonical`);
    assert.equal(meta("property", "og:url"), url, file);
    // The site's preview, or a page's own one next to it (a blog post's).
    const image = meta("property", "og:image") ?? "";
    assert.ok(image === `${ORIGIN}/social-preview.png` || image === `${url}preview.png`, `${file}: og:image ${image}`);
    assert.ok(isFile(image.slice(ORIGIN.length)), `${file}: ${image}`);
    assert.equal(meta("name", "twitter:image"), image, file);
    assert.equal(meta("name", "twitter:card"), "summary_large_image", file);
    assert.match(page, /<link rel="icon" href="\/favicon\.ico"/, file);
    assert.match(page, /<link rel="apple-touch-icon" href="\/apple-touch-icon\.png">/, file);
    assert.ok(listed.includes(url), `${url} is not in the sitemap`);
  }
  for (const url of listed) {
    const path = url.slice(ORIGIN.length);
    assert.ok(isFile(path.endsWith("/") ? path + "index.html" : path), `sitemap lists ${url}`);
  }
  assert.match(read("/robots.txt"), new RegExp(`^Sitemap: ${ORIGIN}/sitemap.xml$`, "m"));
  assert.ok(isFile("/social-preview.png"));
});

test("the blog has a feed that says where it lives", () => {
  const feed = read("/blog/feed.xml");
  assert.match(feed, /^<\?xml version="1.0" encoding="UTF-8"\?>/);
  assert.match(feed, /<rss version="2.0"/);
  assert.ok(feed.includes(`<atom:link href="${ORIGIN}/blog/feed.xml" rel="self"`));
  assert.ok(read("/blog/index.html").includes('href="/blog/feed.xml"'));
});
