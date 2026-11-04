// Reading a waiting agent's question off its screen. Run from web/: `node --test`.
import { test } from "node:test";
import assert from "node:assert/strict";

import { screenPrompt, sendStep } from "../public/app/prompt.js";

const permission = [
  "⏺ I'll run the release check.",
  "",
  "╭──────────────────────────────────────────────╮",
  "│ Bash command                                 │",
  "│                                              │",
  "│   gh pr checks 55 --watch                    │",
  "│   Wait for PR 55 checks                      │",
  "│                                              │",
  "│ Do you want to proceed?                      │",
  "│ ❯ 1. Yes                                     │",
  "│   2. Yes, and don't ask again for gh pr      │",
  "│      checks commands in this project         │",
  "│   3. No, and tell Claude what to do          │",
  "│      differently (esc)                       │",
  "╰──────────────────────────────────────────────╯",
  "",
].join("\n");

test("a permission menu gives its question and its choices", () => {
  const { lines, choices } = screenPrompt(permission);
  assert.deepEqual(
    choices.map((c) => [c.key, c.label, c.chosen]),
    [
      ["1", "Yes", true],
      ["2", "Yes, and don't ask again for gh pr checks commands in this project", false],
      ["3", "No, and tell Claude what to do differently (esc)", false],
    ],
  );
  assert.ok(lines.some((l) => l.includes("gh pr checks 55 --watch")));
  assert.ok(lines.some((l) => l.includes("Do you want to proceed?")));
  assert.ok(lines.every((l) => !/[│╭╰]/.test(l)), lines.join("\n"));
});

test("a numbered list in an answer above the prompt is not a menu", () => {
  const screen = ["Steps:", "1. build", "2. test", "", "> type here", "? for shortcuts"].join("\n");
  const { choices, lines } = screenPrompt(screen);
  assert.deepEqual(choices, []);
  assert.equal(lines.at(-1), "? for shortcuts");
});

test("an empty screen is nothing to show", () => {
  assert.deepEqual(screenPrompt(""), { lines: [], choices: [] });
});

test("Send with an empty field confirms the highlighted choice of a menu on screen", () => {
  const prompt = screenPrompt(permission);
  assert.ok(prompt.choices.length);
  const step = sendStep("  ", prompt);
  assert.deepEqual(step.params, { text: "", submit: true });
  assert.match(step.done, /^Pressed Enter on 1\. Yes/);
});

test("Send with text sends it, menu or not", () => {
  const prompt = screenPrompt(permission);
  assert.deepEqual(sendStep("2", prompt).params, { text: "2", submit: true });
  assert.deepEqual(sendStep("fix it", null).params, { text: "fix it", submit: true });
});

test("Send with an empty field and no menu says so instead of doing nothing", () => {
  assert.ok(sendStep("", null).refuse);
  assert.ok(sendStep("", { lines: ["Done."], choices: [] }).refuse);
});
