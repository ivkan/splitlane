// The page: pairing from the link in a QR code, then the agent sessions on
// each paired computer, one card per session.
//
// Everything a computer sends is shown as text (`textContent`), never as
// markup: an agent's answer or question is words it wrote, and nothing in
// it runs here.

import { cleanName, expired, fingerprint, parseLink } from "./protocol.js";
import { RELAY, connect, pair } from "./link.js";
import { startScan } from "./scan.js";
import * as store from "./store.js";
import { PAGE_VERSION } from "./version.js";

const app = document.getElementById("app");
const POLL_MS = 4000;
const RETRY_FIRST_MS = 1000;
const RETRY_MAX_MS = 30000;
/// Keys the computer lets a phone press, in the order they are offered.
const KEYS = [
  ["1", "1"],
  ["2", "2"],
  ["3", "3"],
  ["y", "Y"],
  ["n", "N"],
  ["up", "↑"],
  ["down", "↓"],
  ["enter", "Enter"],
  ["escape", "Esc"],
];

const state = {
  computers: [],
  offer: null,
  badLink: false,
  pairing: null,
  conn: null,
  connFor: null,
  connStatus: "idle",
  connError: null,
  attempt: 0,
  retryMs: RETRY_FIRST_MS,
  retryTimer: null,
  fleet: null,
  fleetError: null,
  answers: new Map(),
  screen: null,
  showScreen: false,
  notice: null,
  armedInterrupt: null,
  refreshing: false,
  drafts: new Map(),
  scanning: false,
  stopScan: null,
  scanNumber: 0,
  inBrowser: false,
  installPrompt: null,
};

// ---- Where the page runs ----------------------------------------------

const ua = navigator.userAgent;
// iPadOS reports itself as a Mac; touch tells them apart.
const IOS = /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
// Every iOS browser can add a page to the home screen, but its Share button
// sits somewhere else; Chrome, Firefox and Edge name themselves in the UA.
const IOS_CHROME = IOS && /CriOS/.test(ua);
const IOS_SAFARI = IOS && !/CriOS|FxiOS|EdgiOS|OPiOS/.test(ua);
const ANDROID = /Android/.test(ua);
const PHONE = IOS || ANDROID || matchMedia("(pointer: coarse)").matches;

/// Opened from the home screen, not in a browser tab.
function installed() {
  return matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;
}

// Chrome on Android offers its own install button; it is kept for the
// moment the person taps ours.
window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault();
  state.installPrompt = event;
  if (!state.computers.length && route().view === "home") render();
});

// ---- Elements ---------------------------------------------------------

function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props ?? {})) {
    if (value == null || value === false) continue;
    if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else if (key === "class") el.className = value;
    else if (key === "value") el.value = value;
    else el.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child == null || child === false) continue;
    el.append(child instanceof Node ? child : String(child));
  }
  return el;
}

function show(...children) {
  app.replaceChildren(...children.filter((c) => c != null));
}

function header(title, back) {
  return h(
    "header",
    { class: "bar" },
    back ? h("a", { class: "back", href: back, "aria-label": "Back" }, "‹") : h("span", { class: "mark" }, "◧"),
    h("h1", {}, title),
    h("a", { class: "gear", href: "#/settings", "aria-label": "Settings" }, "⋯"),
  );
}

// ---- Relay ------------------------------------------------------------

const relay = RELAY;

// ---- Routes -----------------------------------------------------------

function route() {
  const parts = location.hash.replace(/^#\/?/, "").split("/");
  if (parts[0] === "settings") return { view: "settings" };
  if (parts[0] === "c" && parts[1]) {
    const computer = state.computers.find((c) => c.id === parts[1]);
    if (!computer) return { view: "home" };
    if (parts[2] === "s" && /^\d+$/.test(parts[3] ?? "")) {
      return { view: "session", computer, surface: Number(parts[3]) };
    }
    return { view: "computer", computer };
  }
  return { view: "home" };
}

function render() {
  if (state.scanning) return renderScan();
  if (state.offer || state.pairing || state.badLink) return renderPairing();
  const at = route();
  if (at.view === "settings") return renderSettings();
  if (at.view === "home") {
    if (state.computers.length === 1) {
      location.replace(`#/c/${state.computers[0].id}`);
      return;
    }
    return renderHome();
  }
  ensureConnection(at.computer);
  if (at.view === "computer") return renderComputer(at.computer);
  return renderSession(at.computer, at.surface);
}

// ---- Pairing ----------------------------------------------------------

function guessName() {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua)) return "iPad";
  if (/Android/.test(ua)) return "Android phone";
  return "Phone";
}

function renderPairing() {
  if (state.badLink) {
    return show(
      header("Pair a phone"),
      h(
        "section",
        { class: "panel" },
        h("p", {}, "This pairing link is not one this page understands."),
        h("p", { class: "muted" }, "Update Splitlane on the computer, then show a new code there."),
        h("button", { onclick: () => leavePairing() }, "Close"),
      ),
    );
  }
  const p = state.pairing ?? { step: "name" };
  if (p.step === "name") {
    if (expired(state.offer)) {
      return show(
        header("Pair a phone"),
        h(
          "section",
          { class: "panel" },
          h("p", {}, "This pairing code has expired."),
          h("p", { class: "muted" }, "Codes last five minutes. Show a new one on the computer and scan it again."),
          h("button", { onclick: () => leavePairing() }, "Close"),
        ),
      );
    }
    const input = h("input", {
      id: "phone-name",
      value: guessName(),
      maxlength: "40",
      autocomplete: "off",
      enterkeyhint: "go",
    });
    const error = h("p", { class: "error", role: "alert" });
    const start = () => {
      const name = cleanName(input.value);
      if (!name) {
        error.textContent = "Give this phone a short name.";
        return;
      }
      startPairing(name);
    };
    input.addEventListener("keydown", (e) => e.key === "Enter" && start());
    return show(
      header("Pair a phone"),
      h(
        "section",
        { class: "panel" },
        h("p", {}, "Pair this phone with Splitlane on your computer to follow its agent sessions and answer them."),
        IOS && !installed()
          ? h(
              "p",
              { class: "warn" },
              "On iPhone, a pairing made in the browser stays in the browser: Splitlane on the home screen keeps its own data and will not see it. To use it from the home screen, add it there first and scan the code from it.",
            )
          : null,
        h("label", { for: "phone-name" }, "This phone's name, as the computer will show it"),
        input,
        error,
        h("button", { class: "primary", onclick: start }, "Pair"),
        h("button", { class: "quiet", onclick: () => leavePairing() }, "Cancel"),
      ),
    );
  }
  if (p.step === "connecting") {
    return show(header("Pair a phone"), h("section", { class: "panel" }, h("p", { class: "muted" }, "Reaching the computer…")));
  }
  if (p.step === "code") {
    return show(
      header("Pair a phone"),
      h(
        "section",
        { class: "panel" },
        h("p", {}, "Type this code on the computer:"),
        h("p", { class: "code", "aria-label": p.code.split("").join(" ") }, `${p.code.slice(0, 3)} ${p.code.slice(3)}`),
        h("p", { class: "muted" }, "Waiting for the computer…"),
      ),
    );
  }
  if (p.step === "failed") {
    return show(
      header("Pair a phone"),
      h(
        "section",
        { class: "panel" },
        h("p", { class: "error" }, p.error),
        h("p", { class: "muted" }, "Show a new code on the computer and scan it again."),
        h("button", { onclick: () => leavePairing() }, "Close"),
      ),
    );
  }
}

async function startPairing(name) {
  const offer = state.offer;
  if (expired(offer)) {
    state.pairing = { step: "failed", error: "This pairing code has expired." };
    return render();
  }
  state.pairing = { step: "connecting" };
  render();
  try {
    const kept = await pair(relay, offer, name, (code) => {
      state.pairing = { step: "code", code };
      render();
    });
    const id = kept.room;
    const label = `Computer ${await fingerprint(kept.machine)}`;
    await store.keep({ id, label, phoneName: name, pairedAt: Date.now(), ...kept });
    state.computers = await store.computers();
    state.offer = null;
    state.pairing = null;
    state.notice = "Paired.";
    location.replace(`#/c/${id}`);
    render();
  } catch (error) {
    state.pairing = { step: "failed", error: error.message || String(error) };
    render();
  }
}

function leavePairing() {
  state.offer = null;
  state.pairing = null;
  state.badLink = false;
  render();
}

// ---- Connection -------------------------------------------------------

function ensureConnection(computer) {
  if (state.connFor === computer.id && state.connStatus !== "idle") return;
  dropConnection();
  state.connFor = computer.id;
  if (document.visibilityState !== "visible") return;
  const attempt = ++state.attempt;
  state.connStatus = "connecting";
  connect(relay, computer, (error) => {
    if (attempt !== state.attempt) return;
    state.conn = null;
    state.connStatus = "down";
    state.connError = explain(error);
    retryLater(computer);
    update();
  })
    .then((conn) => {
      if (attempt !== state.attempt) return conn.close();
      state.conn = conn;
      state.connStatus = "open";
      state.connError = null;
      state.retryMs = RETRY_FIRST_MS;
      refresh();
    })
    .catch((error) => {
      if (attempt !== state.attempt) return;
      state.connStatus = "down";
      state.connError = explain(error);
      retryLater(computer);
      update();
    });
}

function retryLater(computer) {
  clearTimeout(state.retryTimer);
  state.retryTimer = setTimeout(() => {
    if (state.connFor !== computer.id) return;
    state.connStatus = "idle";
    ensureConnection(computer);
    update();
  }, state.retryMs);
  state.retryMs = Math.min(state.retryMs * 2, RETRY_MAX_MS);
}

function dropConnection() {
  state.attempt++;
  clearTimeout(state.retryTimer);
  state.conn?.close();
  state.conn = null;
  state.connStatus = "idle";
  state.fleet = null;
  state.answers.clear();
  state.screen = null;
}

function explain(error) {
  const text = error?.message ?? String(error);
  if (text === "timeout") {
    return "The computer does not answer. Splitlane may be closed with no sessions left, the computer asleep or offline, or this phone removed from it.";
  }
  if (text.includes("relay")) return "No connection to the relay. Check this phone's network.";
  return text;
}

// ---- Data -------------------------------------------------------------

async function ask(method, params) {
  if (!state.conn?.open) throw new Error("Not connected to the computer.");
  return state.conn.request(method, params);
}

/// One refresh at a time: an answer can take seconds to read, and polls
/// stacking up behind it would only ask the same again.
async function refresh() {
  if (!state.conn?.open || state.refreshing) return;
  state.refreshing = true;
  try {
    await refreshOnce();
  } finally {
    state.refreshing = false;
  }
}

async function refreshOnce() {
  const at = route();
  const conn = state.conn;
  const hash = location.hash;
  // What comes back after the page moved to another computer, session or
  // connection is not kept.
  const stale = () => state.conn !== conn || location.hash !== hash;
  try {
    const fleet = await ask("list");
    if (stale()) return;
    state.fleet = fleet;
    state.fleetError = null;
  } catch (error) {
    if (stale()) return;
    state.fleetError = sentence(error.message);
  }
  if (at.view === "session") {
    const row = rowFor(at.surface);
    const known = state.answers.get(at.surface);
    const runs = row?.rail?.runs_ended ?? null;
    if (!known || known.runs !== runs) {
      try {
        const answer = await ask("answer", { surface_id: at.surface });
        if (stale()) return;
        state.answers.set(at.surface, { runs, answer });
      } catch (error) {
        if (stale()) return;
        state.answers.set(at.surface, { runs, error: error.message });
      }
    }
    if (state.showScreen) {
      try {
        const screen = await ask("read", { surface_id: at.surface, lines: 120 });
        if (stale()) return;
        state.screen = screen;
      } catch (error) {
        if (stale()) return;
        state.screen = { error: error.message };
      }
    }
  }
  update();
}

setInterval(() => {
  if (document.visibilityState === "visible" && !state.offer) refresh();
}, POLL_MS);

document.addEventListener("visibilitychange", () => {
  // A phone keeps no socket open in the background: it would be cut anyway,
  // and the computer is asked again as soon as the page is back.
  if (document.visibilityState === "hidden") {
    dropConnection();
    // The camera too: a phone shows that it is on.
    if (state.scanning) {
      state.stopScan?.();
      state.stopScan = null;
    }
  } else {
    state.retryMs = RETRY_FIRST_MS;
    render();
  }
});

window.addEventListener("hashchange", () => {
  state.showScreen = false;
  state.screen = null;
  state.armedInterrupt = null;
  render();
  refresh();
});

function rowFor(surface) {
  return state.fleet?.agents?.find((a) => a.surface_id === surface) ?? null;
}

/// The computer's words as a sentence: it writes them as CLI messages.
function sentence(text) {
  const s = String(text ?? "").trim();
  if (!s) return "The computer refused.";
  return s[0].toUpperCase() + s.slice(1) + (/[.!?]$/.test(s) ? "" : ".");
}

// ---- Words for states -------------------------------------------------

function stateOf(row) {
  const rail = row.rail;
  if (rail?.exited) return ["exited", "Exited"];
  const status = rail?.status;
  if (status === "waiting" || row.state === "waiting_for_input") return ["waiting", "Needs you"];
  if (status === "running" || row.state === "thinking") return ["running", "Working"];
  if (status === "starting") return ["running", "Starting"];
  if (status === "failed" || row.state === "errored") return ["failed", "Failed"];
  if (rail?.last_outcome === "finished" || row.state === "finished") return ["done", "Finished"];
  if (rail?.last_outcome === "interrupted") return ["done", "Stopped"];
  return ["idle", "Idle"];
}

function chip(row) {
  const [kind, words] = stateOf(row);
  return h("span", { class: `chip ${kind}` }, words);
}

function projectName(row) {
  const named = state.fleet?.projects?.find((p) => p.index === row.workspace)?.title;
  if (named) return named;
  const cwd = row.rail?.cwd;
  if (cwd) return cwd.split(/[\\/]/).filter(Boolean).pop() ?? cwd;
  return row.workspace != null ? `Project ${row.workspace + 1}` : "Other";
}

/// The rail's name for a running session when the computer sends the rail's
/// list, so the phone and the window call it the same; its pane's name when
/// it does not.
function sessionTitle(row) {
  const listed = railSessions().find((s) => s.surface_id != null && s.surface_id === row.surface_id);
  return listed?.title || row.surface_name || toolName(row.tool);
}

function railSessions() {
  return (state.fleet?.rail?.projects ?? []).flatMap((p) => p.sessions ?? []);
}

function toolName(tool) {
  const names = { claude: "Claude Code", codex: "Codex", opencode: "OpenCode", gemini: "Gemini" };
  return names[tool] ?? tool ?? "Agent";
}

// ---- Views ------------------------------------------------------------

function connectionLine() {
  if (state.connStatus === "open") return null;
  if (state.connStatus === "down") {
    return h(
      "div",
      { class: "status down", role: "status" },
      h("p", {}, state.connError ?? "Not connected."),
      h("button", { class: "small", onclick: () => retryNow() }, "Try again"),
    );
  }
  return h("div", { class: "status", role: "status" }, h("p", { class: "muted" }, "Connecting…"));
}

function retryNow() {
  const at = route();
  if (!at.computer) return;
  state.retryMs = RETRY_FIRST_MS;
  state.connStatus = "idle";
  state.connFor = null;
  render();
}

function noticeLine() {
  if (!state.notice) return null;
  const text = state.notice;
  state.notice = null;
  return h("p", { class: "notice", role: "status" }, text);
}

function renderHome() {
  if (state.computers.length) {
    return show(
      header("Splitlane"),
      noticeLine(),
      h(
        "section",
        { class: "list" },
        state.computers.map((c) => h("a", { class: "row", href: `#/c/${c.id}` }, h("span", { class: "title" }, c.label))),
      ),
    );
  }
  if (!PHONE) return show(header("Splitlane"), noticeLine(), openOnPhone());
  if (!installed() && !state.inBrowser) return show(header("Splitlane"), noticeLine(), installSteps());
  show(header("Splitlane"), noticeLine(), pairSteps(), pasteBox());
}

/// On a computer: this page is for a phone.
function openOnPhone() {
  return h(
    "section",
    { class: "panel center" },
    h("h2", {}, "Splitlane on your phone"),
    h("p", {}, "This page is for your phone. Follow the agent sessions on your computer from it, and answer them."),
    h("p", { class: "muted" }, "Scan this with the phone's camera to open it there:"),
    h("img", { class: "qr", src: "open-on-phone.svg", alt: "QR code of splitlane.dev/app", width: "200", height: "200" }),
    h("p", { class: "muted" }, "splitlane.dev/app"),
  );
}

/// On a phone, in the browser: add the page to the home screen first. On an
/// iPhone it has to be: the home-screen app keeps its own data, and only it
/// will be able to show notifications.
function installSteps() {
  const share = IOS_CHROME
    ? "Tap the Share button (the square with an arrow) at the right of Chrome's address bar, or open the ⋯ menu and choose Share."
    : IOS_SAFARI
      ? "Tap the Share button (the square with an arrow) in Safari's toolbar."
      : "Open the browser's Share menu.";
  const steps = IOS
    ? [
        share,
        "Scroll down, choose Add to Home Screen, then Add.",
        "Open Splitlane from the home screen and pair it there.",
      ]
    : [
        state.installPrompt ? "Tap Install below." : "Open the browser's menu (⋮) and choose Install app or Add to Home screen.",
        "Open Splitlane from the home screen and pair it there.",
      ];
  const install = state.installPrompt
    ? h(
        "button",
        {
          class: "primary",
          onclick: async () => {
            const prompt = state.installPrompt;
            state.installPrompt = null;
            await prompt.prompt();
            render();
          },
        },
        "Install",
      )
    : null;
  return h(
    "section",
    { class: "panel" },
    h("h2", {}, "First, add Splitlane to the home screen"),
    h("p", {}, "It runs from there like an app, with nothing to download from a store."),
    h("ol", {}, steps.map((step) => h("li", {}, step))),
    install,
    IOS
      ? null
      : h(
          "button",
          {
            class: "quiet",
            onclick: () => {
              state.inBrowser = true;
              render();
            },
          },
          "Use it in the browser instead",
        ),
  );
}

/// Installed, or on a phone that stays in the browser: pair by scanning.
function pairSteps() {
  return h(
    "section",
    { class: "panel" },
    h("h2", {}, "Pair with your computer"),
    h(
      "ol",
      {},
      h("li", {}, "On the computer, open Splitlane: Settings → Phone → Pair a phone."),
      h("li", {}, "Tap Scan code and point the camera at the code on the computer's screen."),
      h("li", {}, "Type the six digits this phone then shows on the computer."),
    ),
    h("button", { class: "primary", onclick: () => openScanner() }, "Scan code"),
  );
}

// ---- Scanning ---------------------------------------------------------

function openScanner() {
  state.scanning = true;
  render();
}

function closeScanner() {
  state.stopScan?.();
  state.stopScan = null;
  state.scanning = false;
  render();
}

function renderScan() {
  const video = h("video", { class: "camera", autoplay: true, muted: true, playsinline: true, "aria-label": "Camera" });
  const hint = h("p", { class: "muted center", role: "status" }, "Point the camera at the code on the computer's screen.");
  show(
    header("Scan code"),
    h("section", { class: "panel scan" }, video, hint, h("button", { onclick: () => closeScanner() }, "Cancel")),
  );
  // Drawn again (back from another app, say): the camera starts over in
  // the new video element. A scan started earlier and still opening is
  // told apart by its number and stopped when it opens.
  state.stopScan?.();
  state.stopScan = null;
  const scan = ++state.scanNumber;
  const current = () => state.scanning && scan === state.scanNumber;
  startScan(video, (text) => {
    if (!current()) return;
    const offer = parseLink(text);
    if (!offer) {
      hint.textContent = "That is not a Splitlane pairing code. Show one on the computer: Settings → Phone → Pair a phone.";
      return;
    }
    state.stopScan?.();
    state.stopScan = null;
    state.scanning = false;
    state.offer = offer;
    render();
  })
    .then((stop) => {
      // Cancelled, or started again, while the camera was opening.
      if (!current()) return stop();
      state.stopScan = stop;
    })
    .catch((error) => {
      if (!current()) return;
      hint.textContent = error.message;
      hint.className = "error center";
    });
}

/// For a page added to the home screen, which a camera's link does not open:
/// the link the computer shows can be pasted instead.
function pasteBox() {
  const input = h("input", { placeholder: "https://splitlane.dev/app#pair=…", autocomplete: "off", "aria-label": "Pairing link" });
  const go = () => {
    const offer = parseLink(input.value.trim());
    if (!offer) {
      state.badLink = true;
    } else {
      state.offer = offer;
    }
    render();
  };
  return h(
    "details",
    { class: "paste" },
    h("summary", {}, "Paste a pairing link"),
    input,
    h("button", { onclick: go }, "Pair"),
  );
}

function renderComputer(computer) {
  const body = h("div", { id: "body" });
  show(header(computer.label, state.computers.length > 1 ? "#/" : null), noticeLine(), body);
  fillComputer(body);
}

function fillComputer(body) {
  const parts = [connectionLine()];
  if (state.fleetError) parts.push(h("p", { class: "error" }, state.fleetError));
  const agents = state.fleet?.agents;
  const rail = state.fleet?.rail;
  if (rail) {
    parts.push(...railList(rail));
  } else if (agents && agents.length === 0) {
    parts.push(h("p", { class: "muted center" }, "No agent sessions are running on this computer."));
  } else if (agents) {
    const groups = new Map();
    for (const row of agents) {
      const name = projectName(row);
      if (!groups.has(name)) groups.set(name, []);
      groups.get(name).push(row);
    }
    for (const [name, rows] of groups) {
      parts.push(
        h(
          "section",
          { class: "list" },
          h("h2", {}, name),
          rows.map((row) => sessionRow(row)),
        ),
      );
    }
  }
  parts.push(versionLine());
  body.replaceChildren(...parts.filter(Boolean));
}

/// Which page this is and which Splitlane answers it, so "is it updated?" is
/// read off the screen. A computer that does not say is older than the first
/// release that does.
function versionLine() {
  if (!state.fleet) return h("p", { class: "muted center versions" }, `Page ${PAGE_VERSION}`);
  const app = state.fleet.app_version;
  return h(
    "p",
    { class: "muted center versions" },
    `${app ? `Splitlane ${app}` : "Splitlane 0.2.14 or older"} on this computer · page ${PAGE_VERSION}`,
  );
}

/// The rail's list: projects without a group, then each group with its
/// projects, every session the rail shows in the rail's order. A session
/// whose agent is not running is listed too, so the two lists hold the same
/// sessions; it has nothing to open.
function railList(rail) {
  const projects = rail.projects ?? [];
  if (!projects.length && !state.fleet?.agents?.length) {
    return [h("p", { class: "muted center" }, "No agent sessions on this computer.")];
  }
  const block = (project) =>
    h(
      "section",
      { class: "list project" },
      h("h3", {}, project.title),
      project.sessions?.length ? projectSessions(project.sessions) : h("p", { class: "muted" }, "No agent sessions."),
    );
  const groups = rail.groups ?? [];
  const parts = [];
  const ungrouped = projects.filter((p) => p.group == null);
  if (ungrouped.length) {
    if (groups.length) parts.push(h("h2", {}, "Projects"));
    parts.push(...ungrouped.map(block));
  }
  for (const group of groups) {
    const members = projects.filter((p) => p.group === group.id);
    if (!members.length) continue;
    parts.push(h("h2", {}, group.name), ...members.map(block));
  }
  // A running agent with no row in the rail's list is still listed, under
  // its project's name, as the list without the rail lists it.
  const listed = new Set(railSessions().map((s) => s.surface_id).filter((id) => id != null));
  const rest = (state.fleet?.agents ?? []).filter((row) => !listed.has(row.surface_id));
  const restByProject = new Map();
  for (const row of rest) {
    const name = projectName(row);
    if (!restByProject.has(name)) restByProject.set(name, []);
    restByProject.get(name).push(row);
  }
  for (const [name, rows] of restByProject) {
    parts.push(h("section", { class: "list project" }, h("h3", {}, name), rows.map((row) => sessionRow(row))));
  }
  return parts;
}

/// A project's sessions as the rail draws them: a session another opened sits
/// indented under its opener, which says how many it opened, and the sessions
/// of an opener that was closed sit under a line that names it. The order is
/// already the rail's, so an opened session follows its opener.
function projectSessions(sessions) {
  const here = new Set(sessions.map((s) => s.thread_id).filter((id) => id != null));
  const openedBy = (s) => (s.opened_by && s.opened_by.thread_id !== s.thread_id ? s.opened_by : null);
  const count = new Map();
  for (const s of sessions) {
    const by = openedBy(s);
    if (by && here.has(by.thread_id)) count.set(by.thread_id, (count.get(by.thread_id) ?? 0) + 1);
  }
  const out = [];
  let closedOpener = null;
  for (const s of sessions) {
    const by = openedBy(s);
    const closed = by && !here.has(by.thread_id) ? by.thread_id : null;
    if (closed != null && closed !== closedOpener) {
      out.push(h("p", { class: "opened-caption" }, `opened by ${by.title} · closed`));
    }
    closedOpener = closed;
    const n = count.get(s.thread_id);
    const row = railRow(s, n ? (n === 1 ? "Opened 1 session" : `Opened ${n} sessions`) : null);
    out.push(by ? h("div", { class: "opened" }, row) : row);
  }
  return out;
}

function railRow(session, note) {
  const row = session.surface_id != null ? rowFor(session.surface_id) : null;
  if (row) return sessionRow(row, note);
  return h(
    "div",
    { class: "row inert" },
    h(
      "span",
      { class: "line" },
      h("span", { class: "title" }, session.title || toolName(session.agent)),
      h("span", { class: "chip" }, "Not running"),
    ),
    h("span", { class: "sub" }, [toolName(session.agent), note].filter(Boolean).join(" · ")),
  );
}

function sessionRow(row, note) {
  const at = route();
  const [kind] = stateOf(row);
  const message = kind === "waiting" ? row.rail?.message ?? row.message : null;
  const sub = [sessionTitle(row) !== toolName(row.tool) ? toolName(row.tool) : null, note].filter(Boolean).join(" · ");
  const inner = [
    h("span", { class: "line" }, h("span", { class: "title" }, sessionTitle(row)), chip(row)),
    sub ? h("span", { class: "sub" }, sub) : null,
    message ? h("span", { class: "sub question" }, message) : null,
  ];
  if (row.surface_id == null) return h("div", { class: "row inert" }, inner);
  return h("a", { class: `row ${kind}`, href: `#/c/${at.computer.id}/s/${row.surface_id}` }, inner);
}

function renderSession(computer, surface) {
  const head = h("div", { id: "head" });
  const body = h("div", { id: "body" });
  // Kept in the state, so that a page drawn again (back from another app,
  // "Try again") keeps what was being typed.
  const draftKey = `${computer.id}/${surface}`;
  const draft = h("textarea", {
    id: "reply",
    rows: "3",
    placeholder: "Reply to the agent",
    "aria-label": "Reply",
    value: state.drafts.get(draftKey) ?? "",
    oninput: (e) => state.drafts.set(draftKey, e.target.value),
  });
  const result = h("p", { class: "result", role: "status" });
  const say = (text, bad) => {
    result.textContent = text;
    result.className = bad ? "result error" : "result";
  };
  const write = async (method, params, done) => {
    try {
      await ask(method, { surface_id: surface, ...params });
      say(done);
      setTimeout(refresh, 600);
      return true;
    } catch (error) {
      say(sentence(error.message), true);
      return false;
    }
  };
  const send = async () => {
    const text = draft.value;
    if (!text.trim()) return;
    if (await write("send", { text, submit: true }, "Sent.")) {
      draft.value = "";
      state.drafts.delete(draftKey);
    }
  };
  const keys = h(
    "div",
    { class: "keys" },
    KEYS.map(([key, label]) => h("button", { class: "key", onclick: () => write("key", { keystroke: key }, `Pressed ${label}.`) }, label)),
  );
  const stop = h("button", { class: "danger", id: "interrupt" }, "Interrupt");
  stop.addEventListener("click", () => {
    // A second tap within a few seconds, not a dialog: stopping a turn
    // cannot be taken back.
    if (state.armedInterrupt && Date.now() - state.armedInterrupt < 4000) {
      state.armedInterrupt = null;
      stop.textContent = "Interrupt";
      write("interrupt", {}, "Interrupted.");
    } else {
      state.armedInterrupt = Date.now();
      stop.textContent = "Tap again to interrupt";
      setTimeout(() => {
        if (state.armedInterrupt && Date.now() - state.armedInterrupt >= 4000) {
          state.armedInterrupt = null;
          stop.textContent = "Interrupt";
        }
      }, 4100);
    }
  });
  const compose = h(
    "section",
    { class: "compose" },
    draft,
    h("div", { class: "actions" }, h("button", { class: "primary", onclick: send }, "Send"), stop),
    keys,
    result,
  );
  const said = h("div", { id: "said" });
  show(header("Session", `#/c/${computer.id}`), head, said, compose, body);
  fillSession(head, said, body, surface);
}

function fillSession(head, said, body, surface) {
  const row = rowFor(surface);
  const parts = [];
  const top = [connectionLine()];
  if (!row && state.fleet) {
    top.push(h("p", { class: "muted" }, "This session is no longer running."));
  }
  if (row) {
    const [kind] = stateOf(row);
    head.replaceChildren(
      ...top.filter(Boolean),
      h(
        "section",
        { class: "card-head" },
        h("div", { class: "line" }, h("h2", {}, sessionTitle(row)), chip(row)),
        h("p", { class: "sub" }, [toolName(row.tool), projectName(row)].join(" · ")),
        kind === "waiting" && (row.rail?.message ?? row.message)
          ? h("p", { class: "question" }, row.rail?.message ?? row.message)
          : null,
      ),
    );
  } else {
    head.replaceChildren(...top.filter(Boolean));
  }
  const known = state.answers.get(surface);
  said.replaceChildren();
  if (known) {
    const answer = known.answer;
    said.replaceChildren(
      h(
        "section",
        { class: "answer" },
        h("h3", {}, "Last answer"),
        known.error
          ? h("p", { class: "error" }, sentence(known.error))
          : answer?.text
            ? h("div", { class: "text" }, answer.text)
            : h("p", { class: "muted" }, answer?.hint ?? "No answer yet."),
        answer?.truncated ? h("p", { class: "muted" }, "Cut short; the rest is on the computer.") : null,
      ),
    );
  }
  const toggle = h(
    "button",
    {
      class: "small",
      onclick: () => {
        state.showScreen = !state.showScreen;
        if (!state.showScreen) state.screen = null;
        update();
        if (state.showScreen) refresh();
      },
    },
    state.showScreen ? "Hide screen" : "Show screen",
  );
  const screen = state.showScreen
    ? state.screen?.error
      ? h("p", { class: "error" }, sentence(state.screen.error))
      : h("pre", { class: "screen" }, (state.screen?.text ?? "…").replace(/\s+$/, ""))
    : null;
  parts.push(h("section", { class: "screen-box" }, toggle, screen));
  body.replaceChildren(...parts.filter(Boolean));
  if (screen?.tagName === "PRE") screen.scrollTop = screen.scrollHeight;
}

/// Refill what changes in the current view, keeping what is being typed.
function update() {
  if (state.offer || state.pairing || state.badLink) return;
  const at = route();
  const head = document.getElementById("head");
  const said = document.getElementById("said");
  const body = document.getElementById("body");
  if (at.view === "computer" && body) return fillComputer(body);
  if (at.view === "session" && body && head && said) return fillSession(head, said, body, at.surface);
}

function renderSettings() {
  dropConnection();
  show(
    header("Settings", "#/"),
    h(
      "section",
      { class: "list" },
      h("h2", {}, "Paired computers"),
      state.computers.length
        ? state.computers.map((c) => computerRow(c))
        : h("p", { class: "muted" }, "None yet."),
    ),
    h("button", { onclick: () => openScanner() }, "Pair another computer"),
    pasteBox(),
    h(
      "section",
      { class: "panel" },
      h(
        "p",
        { class: "muted" },
        "Everything between this phone and your computer is encrypted end to end. The relay in between sees only that a message passed, how long it was and when.",
      ),
    ),
    h("p", { class: "muted center versions" }, `Page ${PAGE_VERSION}`),
  );
}

function computerRow(computer) {
  const forget = h("button", { class: "small danger" }, "Forget");
  forget.addEventListener("click", async () => {
    if (forget.dataset.armed) {
      await store.forget(computer.id);
      state.computers = await store.computers();
      renderSettings();
    } else {
      forget.dataset.armed = "1";
      forget.textContent = "Tap again to forget";
    }
  });
  return h(
    "div",
    { class: "row" },
    h("span", { class: "line" }, h("span", { class: "title" }, computer.label), forget),
    h(
      "span",
      { class: "sub" },
      `Paired ${new Date(computer.pairedAt).toLocaleDateString()} as “${computer.phoneName}”. Remove this phone on the computer too, in Settings → Phone.`,
    ),
  );
}

// ---- Start ------------------------------------------------------------

async function start() {
  // The link's secret is in the fragment, which never leaves the browser;
  // it is taken out of the address at once so that it is not kept in the
  // history either.
  if (location.hash.startsWith("#pair=")) {
    state.offer = parseLink(location.href);
    state.badLink = !state.offer;
    history.replaceState(null, "", location.pathname + location.search);
  }
  try {
    state.computers = await store.computers();
  } catch (error) {
    state.computers = [];
    state.notice = `This browser does not let the page keep data (${error?.name ?? "error"}), so it cannot stay paired. Private browsing is the usual reason.`;
  }
  render();
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("sw.js").catch(() => {});
}

start();
