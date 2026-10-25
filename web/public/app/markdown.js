// An agent's answer is Markdown, and shown as plain text it is asterisks and
// backticks. This reads the part of Markdown agents write - headings,
// paragraphs, emphasis, inline code, fenced code, lists, quotes, rules and
// tables - into a tree of plain objects, and `render` builds elements from
// that tree with text nodes only. Nothing in the text becomes markup, so the
// page's rule holds: what a computer sends is words, and nothing in it runs.
//
// Links are drawn as links but do not open, as in the desktop's Markdown
// view: a tap on words an agent wrote takes nobody anywhere.

/// Block tree of `text`: `{ type, ... }` objects, see the cases below.
export function parse(text) {
  const lines = String(text ?? "").replace(/\r\n?/g, "\n").split("\n");
  return blocks(lines, 0);
}

/// How deep quotes and lists nest before the rest is read as a paragraph. A
/// line of thirty thousand `>` was thirty thousand calls deep, past the
/// stack, and no answer worth reading nests a tenth of this.
const MAX_DEPTH = 12;

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^`\s]*)/;
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
const RULE = /^ {0,3}([-*_])(\s*\1){2,}\s*$/;
const QUOTE = /^ {0,3}>\s?(.*)$/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_RULE = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

/// Whether `line`, followed by `rule`, opens a table: both hold a `|`, so a
/// sentence with a pipe in it above a `---` stays a sentence and a rule.
function opensTable(line, rule) {
  return line.includes("|") && rule.includes("|") && rule.includes("-") && TABLE_RULE.test(rule);
}

function blocks(lines, depth) {
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    const fence = line.match(FENCE);
    if (fence) {
      const close = new RegExp(`^ {0,3}${fence[1][0]}{${fence[1].length},}\\s*$`);
      const body = [];
      i++;
      while (i < lines.length && !close.test(lines[i])) body.push(lines[i++]);
      i++;
      out.push({ type: "code", lang: fence[2] || null, text: body.join("\n") });
      continue;
    }
    const heading = line.match(HEADING);
    if (heading) {
      out.push({ type: "heading", level: heading[1].length, inline: inline(heading[2]) });
      i++;
      continue;
    }
    if (RULE.test(line)) {
      out.push({ type: "rule" });
      i++;
      continue;
    }
    if (depth < MAX_DEPTH && QUOTE.test(line)) {
      const body = [];
      while (i < lines.length && QUOTE.test(lines[i])) body.push(lines[i++].match(QUOTE)[1]);
      out.push({ type: "quote", blocks: blocks(body, depth + 1) });
      continue;
    }
    if (depth < MAX_DEPTH && ITEM.test(line)) {
      const [list, next] = listAt(lines, i, depth);
      out.push(list);
      i = next;
      continue;
    }
    if (i + 1 < lines.length && opensTable(line, lines[i + 1])) {
      const head = cells(line);
      const rows = [];
      i += 2;
      while (i < lines.length && lines[i].includes("|") && lines[i].trim()) rows.push(cells(lines[i++]));
      out.push({ type: "table", head: head.map(inline), rows: rows.map((row) => row.map(inline)) });
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !(depth < MAX_DEPTH && startsBlock(lines[i]))) {
      para.push(lines[i++].trim());
    }
    if (!para.length) para.push(lines[i++].trim());
    out.push({ type: "paragraph", inline: inline(para.join("\n")) });
  }
  return out;
}

function startsBlock(line) {
  return FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || ITEM.test(line);
}

/// A list starting at `lines[start]`, and the index after it. An item takes
/// the lines indented under it, so a nested list or a second paragraph stays
/// inside the item it belongs to.
function listAt(lines, start, depth) {
  const first = lines[start].match(ITEM);
  const indent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const items = [];
  let i = start;
  while (i < lines.length) {
    const m = lines[i].match(ITEM);
    if (!m || m[1].length !== indent || /\d/.test(m[2]) !== ordered) break;
    const body = [m[3]];
    i++;
    while (i < lines.length) {
      const next = lines[i];
      if (!next.trim()) {
        // A blank line ends the item unless the item goes on, indented.
        if (i + 1 < lines.length && leading(lines[i + 1]) > indent && lines[i + 1].trim()) {
          body.push("");
          i++;
          continue;
        }
        break;
      }
      const item = next.match(ITEM);
      if (item && item[1].length <= indent) break;
      if (!item && leading(next) <= indent && startsBlock(next)) break;
      body.push(leading(next) > indent ? next.slice(Math.min(leading(next), indent + 2)) : next.trim());
      i++;
    }
    items.push(blocks(body, depth + 1));
  }
  const list = { type: "list", ordered, items };
  if (ordered) list.start = parseInt(first[2], 10);
  return [list, i];
}

function leading(line) {
  return line.length - line.trimStart().length;
}

function cells(line) {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  // A `|` inside a code span is the code's, not a cell boundary.
  const out = [];
  let cell = "";
  let tick = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "\\" && s[i + 1] === "|") {
      cell += "|";
      i++;
    } else if (c === "`") {
      tick = tick ? 0 : 1;
      cell += c;
    } else if (c === "|" && !tick) {
      out.push(cell.trim());
      cell = "";
    } else {
      cell += c;
    }
  }
  out.push(cell.trim());
  return out;
}

/// Inline tree of `text`: strings, and `{ type, ... }` for code, strong,
/// emphasis, links and line breaks.
export function inline(text) {
  const out = [];
  let plain = "";
  const flush = () => {
    if (plain) out.push(plain);
    plain = "";
  };
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === "\\" && i + 1 < text.length && /[\\`*_[\]()#+\-.!|>~]/.test(text[i + 1])) {
      plain += text[i + 1];
      i += 2;
      continue;
    }
    if (c === "\n") {
      flush();
      out.push({ type: "break" });
      i++;
      continue;
    }
    if (c === "`") {
      const run = text.slice(i).match(/^`+/)[0];
      const end = text.indexOf(run, i + run.length);
      if (end > 0) {
        flush();
        out.push({ type: "code", text: text.slice(i + run.length, end).replace(/\n/g, " ").trim() || " " });
        i = end + run.length;
        continue;
      }
      plain += run;
      i += run.length;
      continue;
    }
    if (c === "[") {
      const link = text.slice(i).match(/^\[([^\]\n]+)\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)/);
      if (link) {
        flush();
        out.push({ type: "link", href: link[2], inline: inline(link[1]) });
        i += link[0].length;
        continue;
      }
    }
    if (c === "*" || c === "_") {
      const span = emphasis(text, i);
      if (span) {
        flush();
        out.push(span.node);
        i = span.end;
        continue;
      }
    }
    if (c === "~" && text[i + 1] === "~") {
      const end = text.indexOf("~~", i + 2);
      if (end > i + 2) {
        flush();
        out.push({ type: "strike", inline: inline(text.slice(i + 2, end)) });
        i = end + 2;
        continue;
      }
    }
    plain += c;
    i++;
  }
  flush();
  return out;
}

/// How far a marker looks for its closing one. Without a bound, an answer
/// full of lone asterisks searched to its end from each of them.
const EMPHASIS_REACH = 1000;

/// `**strong**`, `*emphasis*`, and the same with `_`. An underscore inside a
/// word (`snake_case_name`) is a letter, not a marker.
function emphasis(text, at) {
  const mark = text[at];
  const double = text[at + 1] === mark;
  const delim = double ? mark + mark : mark;
  const open = at + delim.length;
  if (open >= text.length || /\s/.test(text[open])) return null;
  if (mark === "_" && at > 0 && /[\p{L}\p{N}]/u.test(text[at - 1])) return null;
  let end = open;
  while ((end = text.indexOf(delim, end + 1)) !== -1 && end - open <= EMPHASIS_REACH) {
    if (/\s/.test(text[end - 1])) continue;
    if (!double && text[end + 1] === mark) {
      end++;
      continue;
    }
    if (mark === "_" && end + delim.length < text.length && /[\p{L}\p{N}]/u.test(text[end + delim.length])) continue;
    const inner = text.slice(open, end);
    if (inner.includes("\n\n")) return null;
    return { node: { type: double ? "strong" : "em", inline: inline(inner) }, end: end + delim.length };
  }
  return null;
}

/// Elements for `tree`, made with `h(tag, props, ...children)`; every string
/// in the tree becomes a text node.
export function render(tree, h) {
  return tree.map((block) => renderBlock(block, h));
}

function renderBlock(block, h) {
  switch (block.type) {
    case "heading":
      return h(`h${Math.min(block.level + 2, 6)}`, { class: "md-h" }, renderInline(block.inline, h));
    case "paragraph":
      return h("p", {}, renderInline(block.inline, h));
    case "code":
      return h("pre", { class: "md-code" }, h("code", {}, block.text));
    case "quote":
      return h("blockquote", {}, render(block.blocks, h));
    case "rule":
      return h("hr", {});
    case "list":
      return h(
        block.ordered ? "ol" : "ul",
        block.ordered && block.start !== 1 ? { start: String(block.start) } : {},
        block.items.map((item) => h("li", {}, render(item, h))),
      );
    case "table":
      return h(
        "div",
        { class: "md-table" },
        h(
          "table",
          {},
          h("thead", {}, h("tr", {}, block.head.map((cell) => h("th", {}, renderInline(cell, h))))),
          h(
            "tbody",
            {},
            block.rows.map((row) => h("tr", {}, row.map((cell) => h("td", {}, renderInline(cell, h))))),
          ),
        ),
      );
    default:
      return null;
  }
}

function renderInline(nodes, h) {
  return nodes.map((node) => {
    if (typeof node === "string") return node;
    switch (node.type) {
      case "break":
        return h("br", {});
      case "code":
        return h("code", {}, node.text);
      case "strong":
        return h("strong", {}, renderInline(node.inline, h));
      case "em":
        return h("em", {}, renderInline(node.inline, h));
      case "strike":
        return h("s", {}, renderInline(node.inline, h));
      case "link":
        return h("span", { class: "md-link", title: node.href }, renderInline(node.inline, h));
      default:
        return null;
    }
  });
}
