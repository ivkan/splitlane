// What a waiting agent is asking, read off the foot of its screen.
//
// A hook says only that the agent waits ("Claude needs your permission");
// the question itself - the command, the file, the choices - is drawn in the
// terminal, and the phone showed nothing of it. This takes the last lines
// of the screen and, when they end in a numbered menu, its choices, so the
// phone can show the question and offer each choice as a button that presses
// its digit.

/// Box-drawing characters agents frame their prompts with.
const FRAME = /^\s*[│┃║╭╮╰╯┌┐└┘─━═]+\s?|\s*[│┃║╭╮╰╯┌┐└┘─━═]+\s*$/g;
/// `❯ 1. Yes`, `  2) No`: a menu line, the pointer marking the chosen one.
const CHOICE = /^(?:([❯›>▶])\s*)?(\d{1,2})[.)]\s+(\S.*)$/;
/// How many lines of the question are shown above the menu, or of the screen
/// when there is no menu.
const CONTEXT = 10;

/// `{ lines, choices }` for `screen`: the lines to show, and the menu's
/// choices (`{ key, label, chosen }`, in order), empty when the screen ends
/// in no menu.
export function screenPrompt(screen) {
  const lines = String(screen ?? "")
    .replace(/\r/g, "")
    .split("\n")
    .map((line) => line.replace(FRAME, ""));
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();

  // The last run of menu lines numbered 1, 2, 3...: a numbered list higher
  // up in the agent's answer is not what it is asking.
  let end = -1;
  for (let i = lines.length - 1; i >= 0 && i >= lines.length - 25; i--) {
    if (CHOICE.test(lines[i].trim())) {
      end = i;
      break;
    }
  }
  const choices = [];
  let start = end;
  if (end >= 0) {
    // Up to the choice numbered 1.
    for (let i = end; i >= 0 && end - i < 20; i--) {
      const m = lines[i].trim().match(CHOICE);
      if (!m) continue;
      start = i;
      if (m[2] === "1") break;
    }
    // Then down again: a line under a choice that is not one is that choice
    // running on, and the last choice may run on past `end`.
    for (let i = start; i < lines.length && i <= end + 2; i++) {
      const line = lines[i].trim();
      const m = line.match(CHOICE);
      if (m) {
        choices.push({ key: m[2], label: m[3].trim(), chosen: Boolean(m[1]) });
      } else if (!line) {
        if (i > end) break;
      } else if (choices.length && (i <= end || /^\s{2,}/.test(lines[i]))) {
        choices[choices.length - 1].label += ` ${line}`;
      }
    }
    const numbered = choices.every((c, i) => Number(c.key) === i + 1);
    // A menu is the screen's last thing: under it at most a line or two of
    // its own (`Esc to cancel`), never the agent's input line.
    const after = lines.slice(end + 1).filter((line) => line.trim());
    const atFoot = after.length <= 3 && !after.some((line) => /^\s*[>❯]\s/.test(line));
    if (choices.length < 2 || !numbered || !atFoot) choices.length = 0;
  }
  const from = choices.length ? Math.max(0, start - CONTEXT) : Math.max(0, lines.length - CONTEXT - 2);
  const to = choices.length ? Math.min(lines.length, end + 3) : lines.length;
  const shown = squeeze(lines.slice(from, to));
  return { lines: shown, choices };
}

/// Blank lines at the edges dropped and runs of them made one.
function squeeze(lines) {
  const out = [];
  for (const line of lines) {
    if (!line.trim() && (!out.length || !out[out.length - 1].trim())) continue;
    out.push(line.replace(/\s+$/, ""));
  }
  while (out.length && !out[out.length - 1].trim()) out.pop();
  return out;
}
