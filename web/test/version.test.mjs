// The page's version against the page. It is what a phone shows to say which
// page it runs, so a page that changed under the version before it would say
// it is up to date when it is not. Run from web/: `node --test`.
import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import { PAGE_FILES_SHA256, PAGE_VERSION } from "../public/app/version.js";

const app = fileURLToPath(new URL("../public/app/", import.meta.url));

/// Every file the page is made of but `version.js`, by name, in name order.
function pageFilesSha256() {
  const hash = createHash("sha256");
  const names = readdirSync(app, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => relative(app, join(entry.parentPath ?? entry.path, entry.name)).split(sep).join("/"))
    .filter((name) => name !== "version.js")
    .sort();
  for (const name of names) {
    hash.update(`${name}\0`);
    hash.update(readFileSync(join(app, name)));
    hash.update("\0");
  }
  return hash.digest("hex");
}

test("a changed page carries a new version", () => {
  assert.match(PAGE_VERSION, /^\d{4}-\d{2}-\d{2}\.\d+$/);
  const now = pageFilesSha256();
  assert.equal(
    PAGE_FILES_SHA256,
    now,
    `the page changed: raise PAGE_VERSION in public/app/version.js (today's date and a counter, e.g. 2026-10-25.1) and set PAGE_FILES_SHA256 to ${now}`,
  );
});
