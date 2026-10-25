// The Markdown reader against what agents write. Run from web/: `node --test`.
import { test } from "node:test";
import assert from "node:assert/strict";

import { inline, parse, render } from "../public/app/markdown.js";

test("emphasis, code and links read as such", () => {
  assert.deepEqual(inline("**Bold** and *it* and `x*y`"), [
    { type: "strong", inline: ["Bold"] },
    " and ",
    { type: "em", inline: ["it"] },
    " and ",
    { type: "code", text: "x*y" },
  ]);
  assert.deepEqual(inline("see [the spec](https://x.test/a)"), [
    "see ",
    { type: "link", href: "https://x.test/a", inline: ["the spec"] },
  ]);
});

test("an underscore inside a word and a lone asterisk stay letters", () => {
  assert.deepEqual(inline("snake_case_name"), ["snake_case_name"]);
  assert.deepEqual(inline("2 * 3 = 6"), ["2 * 3 = 6"]);
  assert.deepEqual(inline("an \\*escaped\\* star"), ["an *escaped* star"]);
});

test("an answer like the one on the phone reads as paragraphs and lists", () => {
  const text = [
    "**Answers to the plan's questions**",
    "(`.specflow/reference/SPEC-397.md`):",
    "1. The proof cell runs on the Mac.",
    "2. Key removal and an LWW write over an OR slot.",
    "",
    "**The split is accepted**, one part at a time:",
    "- **397a** — the rig",
    "  - a nested item",
    "- **397b** — the series",
  ].join("\n");
  const tree = parse(text);
  assert.deepEqual(
    tree.map((b) => b.type),
    ["paragraph", "list", "paragraph", "list"],
  );
  assert.deepEqual(tree[0].inline, [
    { type: "strong", inline: ["Answers to the plan's questions"] },
    { type: "break" },
    "(",
    { type: "code", text: ".specflow/reference/SPEC-397.md" },
    "):",
  ]);
  assert.equal(tree[1].ordered, true);
  assert.equal(tree[1].items.length, 2);
  const bullets = tree[3];
  assert.equal(bullets.ordered, false);
  assert.equal(bullets.items.length, 2);
  assert.deepEqual(
    bullets.items[0].map((b) => b.type),
    ["paragraph", "list"],
  );
});

test("fenced code keeps its text exactly", () => {
  const tree = parse("```rust\nlet a = *b;\n  **not bold**\n```\nafter");
  assert.deepEqual(tree[0], { type: "code", lang: "rust", text: "let a = *b;\n  **not bold**" });
  assert.equal(tree[1].type, "paragraph");
});

test("headings, rules, quotes and tables", () => {
  const tree = parse("## Plan\n\n---\n\n> quoted *here*\n\n| a | b |\n|---|:-:|\n| 1 | `2` |");
  assert.deepEqual(
    tree.map((b) => b.type),
    ["heading", "rule", "quote", "table"],
  );
  assert.equal(tree[0].level, 2);
  assert.deepEqual(tree[3].head, [["a"], ["b"]]);
  assert.deepEqual(tree[3].rows, [[["1"], [{ type: "code", text: "2" }]]]);
});

test("rendering makes text nodes only, and a link that does not navigate", () => {
  const made = [];
  const h = (tag, props, ...children) => {
    const el = { tag, props, children: children.flat() };
    made.push(el);
    return el;
  };
  render(parse("<img src=x onerror=alert(1)> [go](javascript:alert(1))"), h);
  assert.ok(made.every((el) => !Object.keys(el.props).some((k) => k.startsWith("on") || k === "href")));
  const p = made.find((el) => el.tag === "p");
  assert.equal(p.children[0], "<img src=x onerror=alert(1)> ");
  assert.equal(made.find((el) => el.tag === "span").props.class, "md-link");
});

test("thirty thousand lone markers read quickly", () => {
  const text = "*a ".repeat(10_000) + "_b ".repeat(10_000);
  const started = performance.now();
  const tree = parse(text);
  assert.ok(performance.now() - started < 1000);
  assert.equal(tree[0].inline.join(""), text.trim());
});

test("thirty thousand nested quotes do not run out of stack", () => {
  const tree = parse(">".repeat(30_000) + " deep");
  let depth = 0;
  for (let node = tree[0]; node?.type === "quote"; node = node.blocks[0]) depth++;
  assert.ok(depth <= 12);
});

test("a pipe in a sentence above a rule is not a table, and a pipe in code is not a cell", () => {
  assert.deepEqual(
    parse("choose a|b\n---").map((b) => b.type),
    ["paragraph", "rule"],
  );
  const table = parse("| cmd | note |\n|---|---|\n| `a|b` | pipe |")[0];
  assert.deepEqual(table.rows[0], [[{ type: "code", text: "a|b" }], ["pipe"]]);
});
