/// The page's later controls, driven in a real browser.
///
/// Run by hand -- `node scripts/browser-controls-test.js` -- and deliberately **not** part of CI:
/// it needs a browser, and a browser is not something a test may assume is installed. What it is
/// for is the half of `docs/web-mode.md` §11 that was left as "asserted as bytes and never driven
/// as clicks": the switches, the command panel, an action button, the masked credential field, and
/// the destructive row's menu. Every claim is checked against the *run's own stdout*, not against
/// the page, because the page cannot be the witness to its own message: a click that sent nothing
/// would leave the page looking exactly like a click that worked.
///
/// The browser is driven over the Chrome DevTools protocol with node's own `WebSocket` (node 22+),
/// so this needs no package installed. Chrome, Edge, Chromium and Brave are looked for where they
/// install on Windows, macOS and Linux; `--browser <path>` names one that is somewhere else.
///
/// Two traps this harness has to respect, both already written down in `docs/web-mode.md`:
/// **`--web` opens a browser tab**, which is why flint is started with its stdout on a file --
/// `announce_view` only opens anything when stdout is a terminal -- and its own browser never runs;
/// and **the run must be alive the whole time**, because every control here is a line into a real
/// REPL, so the process is killed at the end rather than left to a timeout.
///
/// ## What is driven here, and what is not
///
/// The claims in `docs/web-mode.md` are about a page somebody uses, and this is the only thing in the
/// repository that presses its controls -- so "the page's controls are driven in a real browser" has to
/// be a *list*. It is printed at the start of every run rather than only kept here, because a list that
/// exists only in the source is one nobody reads before believing a claim.
///
/// Driven here, each against the run's own stdout: the switches; the command panel (opening it, a report
/// answered in it, an action button, the masked credential field, and that the key really reached
/// `config.toml`); the two-press destructive menu (a sidebar row's, and the jobs panel's stop list); the
/// conversation row's menu, including naming the conversation being written; the sidebar's two drag
/// handles, the arrow keys on the focused one and their double-click reset; the model picker from the
/// keyboard; the composer's send button and a line the run answers; a tool block's path buttons; the
/// preview panel (a grep hit's line, reload, Escape, a refusal in the route's own words, the line numbers
/// of a file too tall and too wide to fit -- measured as geometry -- and the open
/// control -- pressed against a path that is not there, because the press that works opens a viewer on
/// the machine running this); and the jobs panel (a running job's clock, a finished row's exit code, a
/// child's row opening its own conversation, output, and the stop's own two presses).
///
/// Not driven here, with where each is answered instead: a report asked for *mid-turn* -- measured from
/// outside the page by `tests/cli_output.rs::a_report_asked_for_mid_turn_waits_for_the_turn`, which is
/// what `docs/web-mode.md` section 11 now says rather than claiming a press; the `/prompt` row's send
/// button, which section 12 records as not measured; an OS-level open that *succeeds* -- refused here on
/// purpose, since it would start a program on this machine, and held instead by
/// `src/web.rs::tests::each_platform_is_opened_by_the_program_it_has`, which asserts the command line
/// without running it; a paste into the composer, an IME, a screen reader,
/// two tabs on one run, touch, and any phone-sized viewport. None of those is refused or impossible --
/// they are simply not measured, and the honest place to say so is the artifact that measures the rest.
"use strict";

const { spawn } = require("child_process");
const fs = require("fs");
const http = require("http");
const os = require("os");
const path = require("path");

const root = path.join(__dirname, "..");
const results = [];

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/// One claim, and whether it held. Printed as it is decided, because a run that hangs is more
/// readable as a list of what it got through than as a promise that never settled.
function check(claim, ok, detail) {
  results.push({ claim, ok: !!ok, detail });
  console.log(`${ok ? "  PASS" : "  FAIL"}  ${claim}${ok || !detail ? "" : `\n        ${detail}`}`);
}

function browserPath() {
  const named = process.argv.indexOf("--browser");
  if (named >= 0 && process.argv[named + 1]) return process.argv[named + 1];
  const candidates = [
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/usr/bin/brave-browser",
  ];
  return candidates.find((candidate) => fs.existsSync(candidate)) || null;
}

/// The binary this harness drives: the debug build in this checkout, which is what the other checks
/// run and what `cargo build` puts there. The name differs on Windows, and that is the only platform
/// difference in this file -- everything else is the protocol's.
function flintBinary() {
  return path.join(root, "target", "debug", process.platform === "win32" ? "flint.exe" : "flint");
}

/// A flint to drive: a scratch home, a provider that is never called (nothing here needs a model),
/// and two conversations on disk so the sidebar has rows and a destructive row has candidates.
function scratch(baseUrl) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "flint-browser-"));
  const cwd = path.join(home, "work");
  fs.mkdirSync(path.join(home, "sessions"), { recursive: true });
  fs.mkdirSync(cwd, { recursive: true });
  fs.writeFileSync(
    path.join(home, "config.toml"),
    'default_provider = "stub"\n\n[[providers]]\nname = "stub"\n' +
      `base_url = "${baseUrl}"\nmodel = "stub-model"\n` +
      'models = ["stub-model-2", "stub-model-3"]\napi_key = "not-a-real-key"\n'
  );
  for (const [id, prompt] of [["111-1", "the older question"], ["222-1", "the newer question"]]) {
    const meta = JSON.stringify({
      type: "meta", v: 2, id, created: "epoch:1", cwd,
      provider: "stub", model: "stub-model",
    });
    fs.writeFileSync(
      path.join(home, "sessions", `${id}.jsonl`),
      `${meta}\n${JSON.stringify({ type: "chat", message: { role: "user", content: prompt } })}\n`
    );
  }
  // Eight more conversations, because one claim needs a sidebar whose list *scrolls*: a menu that
  // hangs below its row is only drawn outside the list's box when the row it belongs to is at the
  // bottom of a list with more rows than fit, which is the case the sidebar is in after a week of
  // work. They are old (`epoch:1`, like the two above) so nothing that asks which conversation is
  // newest can see them.
  for (let i = 0; i < 8; i += 1) {
    const id = `900-${i}`;
    const meta = JSON.stringify({
      type: "meta", v: 2, id, created: "epoch:1", cwd, provider: "stub", model: "stub-model",
    });
    fs.writeFileSync(
      path.join(home, "sessions", `${id}.jsonl`),
      `${meta}\n${JSON.stringify({ type: "chat", message: { role: "user", content: `filler ${i}` } })}\n`
    );
  }
  return { home, cwd, log: path.join(home, "stdout.txt") };
}

/// A model the harness can script, so a browser claim can be made about a *turn*.
///
/// The shape is the Rust stub provider's (`tests/task.rs`): one `data:` line per delta, then a
/// `finish_reason`, then `[DONE]`. Answers by request number with the last one repeating, because
/// a turn with a tool call in it is two requests at least -- the call, then the prose that ends it.
/// Nothing is asserted here about the model: this exists so the transcript has a tool block in it,
/// with real paths, that the page can be asked to open.
function stubModel(bodies) {
  let step = 0;
  const server = http.createServer((request, response) => {
    request.on("data", () => {});
    request.on("end", () => {
      const body = bodies[Math.min(step, bodies.length - 1)] || "";
      step += 1;
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(body);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () =>
      resolve({ server, port: server.address().port })
    );
  });
}

/// One SSE body, as the wire carries it: every line followed by a blank line.
const sse = (...lines) => lines.map((line) => `${line}\n\n`).join("");

const prose = (text) =>
  sse(
    `data: ${JSON.stringify({ choices: [{ delta: { content: text } }] })}`,
    `data: {"choices":[{"delta":{},"finish_reason":"stop"}]}`,
    "data: [DONE]"
  );

const toolCall = (name, args) =>
  sse(
    `data: ${JSON.stringify({
      choices: [{ delta: { tool_calls: [{ index: 0, id: "call_1", function: { name, arguments: JSON.stringify(args) } }] } }],
    })}`,
    `data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}`,
    "data: [DONE]"
  );

/// Start flint with `--web` and hand back the URL it printed.
///
/// stdout goes to a file rather than a pipe for two reasons: the URL is in it, and what every
/// control is checked against is in it. `is_terminal()` is false on a file, which is also what keeps
/// `--web` from opening a tab in the browser of whoever is sitting at this machine.
async function startFlint(where) {
  const out = fs.openSync(where.log, "a");
  const child = spawn(flintBinary(), ["--web"], {
    cwd: where.cwd,
    env: { ...process.env, FLINT_HOME: where.home, NO_COLOR: "1" },
    stdio: ["pipe", out, out],
  });
  const text = () =>
    fs.existsSync(where.log) ? fs.readFileSync(where.log, "utf8").replace(/\u001b\[[0-9;]*m/g, "") : "";
  for (let i = 0; i < 80; i += 1) {
    await sleep(250);
    const found = text().match(/http:\/\/127\.0\.0\.1:\d+\/\?token=[0-9a-f]+/);
    if (found) return { child, url: found[0], text };
  }
  throw new Error(`flint never printed a URL: ${text()}`);
}

async function startBrowser(binary, url, home) {
  const port = 9223 + Math.floor(Math.random() * 200);
  const child = spawn(
    binary,
    [
      "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
      "--window-size=1400,900", `--remote-debugging-port=${port}`,
      `--user-data-dir=${path.join(home, "browser")}`, url,
    ],
    { stdio: "ignore" }
  );
  for (let i = 0; i < 80; i += 1) {
    await sleep(250);
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      const page = list.find((t) => t.type === "page" && t.url.startsWith("http://127.0.0.1"));
      if (page) return { child, page, port };
    } catch {}
  }
  throw new Error("no browser page target appeared on the debugging port");
}

/// A page to talk to, over one WebSocket: `send` for protocol calls, `js` for expressions.
async function attach(target) {
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const waiting = new Map();
  // What the page's own network and console did, kept because "the controls never appeared" has
  // one honest cause and several plausible ones: a route that answered 403, a script that threw, a
  // stream the browser refused. The page's own markup cannot tell those apart.
  const notes = [];
  let next = 0;
  ws.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.method === "Runtime.consoleAPICalled") {
      notes.push(
        `console.${message.params.type}: ` +
          (message.params.args || []).map((a) => a.value ?? a.description ?? "").join(" ")
      );
    }
    if (message.method === "Runtime.exceptionThrown") {
      notes.push(`threw: ${message.params.exceptionDetails.text} ${message.params.exceptionDetails.exception?.description || ""}`);
    }
    if (message.method === "Network.responseReceived") {
      notes.push(`${message.params.response.status} ${message.params.response.url}`);
    }
    if (message.method === "Network.loadingFailed") {
      notes.push(`failed: ${message.params.errorText} (${message.params.type})`);
    }
    if (message.id && waiting.has(message.id)) {
      waiting.get(message.id)(message);
      waiting.delete(message.id);
    }
  });
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve);
    ws.addEventListener("error", reject);
  });
  const send = (method, params) =>
    new Promise((resolve) => {
      const id = ++next;
      waiting.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    });
  await send("Runtime.enable", {});
  await send("Page.enable", {});
  await send("Network.enable", {});

  /// Evaluate in the page and hand back the value. Everything this harness asks the page is a
  /// question about what a person would see, so a thrown exception is an answer too -- and it is
  /// reported rather than swallowed, because a missing element and a wrong element look the same
  /// from the outside. Evaluated as it is rather than wrapped in a function: several of these
  /// expressions are statements (`focus(); value`) and the DevTools protocol already answers with
  /// the thrown error and with the value, JSON and all.
  const js = async (expression) => {
    const answer = await send("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    const result = answer.result || {};
    if (result.exceptionDetails) {
      const text = result.exceptionDetails.exception?.description || result.exceptionDetails.text;
      return `threw: ${String(text).split("\n")[0]}`;
    }
    return result.result ? result.result.value : undefined;
  };
  const waitFor = async (expression, what, tries = 60) => {
    for (let i = 0; i < tries; i += 1) {
      const value = await js(expression);
      if (value) return value;
      await sleep(200);
    }
    // A wait that times out is the harness's most likely failure, and "the page never drew its
    // controls" has several causes that look identical from here -- a page that did not load, a
    // script that threw, a state frame that never arrived. So the page is asked what it is.
    const diagnosis = await js(
      `({
        ready: document.readyState,
        title: document.title,
        url: location.href,
        markup: document.body ? document.body.innerHTML.length : -1,
        hint: (document.getElementById("hint") || {}).textContent || "",
        meta: (document.getElementById("meta") || {}).textContent || "",
        transcript: (document.getElementById("doc") || {}).textContent || "",
      })`
    );
    throw new Error(
      `timed out waiting for ${what} -- page says ${JSON.stringify(diagnosis)}\n        ${notes.join("\n        ")}`
    );
  };
  /// A real click, at the element's own box: `element.click()` would not tell us whether a control
  /// is reachable by a pointer, which is the whole question for a button under a panel or a menu.
  ///
  /// And it is checked before it is sent: what is *on top* at that point is asked for first, and a
  /// control that something else covers is reported as covered rather than as "the click did
  /// nothing". That distinction is the whole reason a real browser is worth the trouble -- a
  /// covering element is invisible in the source and in a stub DOM.
  /// `at` is an offset from the element's own top-left corner, for the one thing that has to be
  /// pressed *off* its centre: the settings mask is a full-screen backdrop with the dialog over the
  /// middle of it, so the only press that reaches the mask is one near an edge. Without it this
  /// helper could only be satisfied by calling `.click()` on the element, which is not a pointer and
  /// never asks what is on top.
  const click = async (selector, at) => {
    const x0 = at ? `r.left + ${at[0]}` : "r.left + r.width / 2";
    const y0 = at ? `r.top + ${at[1]}` : "r.top + r.height / 2";
    const aimed = await js(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)});
        if (!el) return null; el.scrollIntoView({ block: "center" });
        const r = el.getBoundingClientRect();
        const x = ${x0}, y = ${y0};
        const top = document.elementFromPoint(x, y);
        const name = (n) => n ? n.tagName.toLowerCase() + (n.id ? "#" + n.id : "") +
          (n.className ? "." + String(n.className).split(" ").join(".") : "") : "nothing";
        return { x, y, w: r.width, h: r.height, top: name(top),
                 ours: top === el || (!!top && el.contains(top)), onTopOf: name(document.elementFromPoint(x, y)) };
      })()`
    );
    if (!aimed || typeof aimed !== "object" || typeof aimed.x !== "number") {
      // The answer is printed and not just the question: this helper asks the page for a box, and the
      // page answering `null` (no such element), `undefined` (the script threw before the box) and a
      // box at 0,0 (the element is inside something hidden) are three different defects that read
      // identically as "the click did nothing".
      throw new Error(`no element to click: ${selector} -- the page answered ${JSON.stringify(aimed)}`);
    }
    if (!aimed.ours) {
      throw new Error(`${selector} is covered by ${aimed.onTopOf} at ${aimed.x},${aimed.y}`);
    }
    for (const type of ["mousePressed", "mouseReleased"]) {
      await send("Input.dispatchMouseEvent", {
        type, x: aimed.x, y: aimed.y, button: "left", clickCount: 1,
      });
    }
    await sleep(120);
  };
  const key = async (code, keyCode) => {
    for (const type of ["rawKeyDown", "keyUp"]) {
      await send("Input.dispatchKeyEvent", {
        type, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode, key: code,
      });
    }
    await sleep(120);
  };
  /// A drag, as a pointer does it: press on the element, move with the button held, let go. The move
  /// has to be several events rather than one, because the page follows `pointermove` and a single
  /// jump would also be satisfied by a handler that only ever reads the release point. `buttons: 1`
  /// is what makes the moved events come with the button still down -- without it they arrive as
  /// hover, and the page's `pointermove` never fires.
  const drag = async (selector, dx, dy = 0) => {
    const box = await js(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)});
        if (!el) return null; const r = el.getBoundingClientRect();
        return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) }; })()`
    );
    if (!box || typeof box.x !== "number") throw new Error(`no element to drag: ${selector}`);
    await send("Input.dispatchMouseEvent", {
      type: "mousePressed", x: box.x, y: box.y, button: "left", clickCount: 1,
    });
    let held = false;
    for (let step = 1; step <= 4; step += 1) {
      await send("Input.dispatchMouseEvent", {
        type: "mouseMoved",
        x: box.x + Math.round((dx * step) / 4),
        y: box.y + Math.round((dy * step) / 4),
        button: "left",
        buttons: 1,
      });
      await sleep(60);
      // Asked while the button is still down: "the drag was accepted" is a different claim from
      // "the width changed", and the page says so itself by putting `dragging` on the body.
      held = held || (await js(`document.body.classList.contains("dragging")`));
    }
    await send("Input.dispatchMouseEvent", {
      type: "mouseReleased", x: box.x + dx, y: box.y + dy, button: "left", clickCount: 1,
    });
    await sleep(150);
    return held;
  };
  /// A real double-click: two press/release pairs inside the platform's interval, the second
  /// carrying `clickCount: 2`, which is what makes the browser emit `dblclick`. Dispatching a
  /// `dblclick` event by hand would test the handler and not the gesture.
  const doubleClick = async (selector) => {
    const box = await js(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)});
        if (!el) return null; const r = el.getBoundingClientRect();
        return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) }; })()`
    );
    if (!box || typeof box.x !== "number") throw new Error(`no element to double-click: ${selector}`);
    for (const count of [1, 2]) {
      for (const type of ["mousePressed", "mouseReleased"]) {
        await send("Input.dispatchMouseEvent", {
          type, x: box.x, y: box.y, button: "left", clickCount: count,
        });
      }
      await sleep(40);
    }
    await sleep(150);
  };
  return { send, js, waitFor, click, key, drag, doubleClick, close: () => ws.close() };
}

/// A row of one screen of the dialog, by the line it would send -- which is what the frame put in it.
///
/// The screen is the caller's, and it has to be: the dialog is six short screens now rather than one
/// long list, so a row is only in the document while the screen it was filed on is on top. Pressing a
/// row on another screen is not possible (`hidden`), which is why every caller here names the screen
/// it expects the row on -- and a row that moved to a different screen fails the claim rather than
/// being found wherever it went.
const ROW = (line, screen) =>
  `(() => { const rows = Array.from(document.querySelectorAll("#row-list-${screen} button.row code"));
     const found = rows.find((c) => c.textContent.trim() === ${JSON.stringify(line)});
     if (!found) return null;
     found.closest("button").id = "harness-target"; return true; })()`;

/// The rows of one screen, as text: what a claim about "the run's own list" reads.
const ROWS_OF = (screen) =>
  `(document.getElementById("row-list-${screen}") || {}).textContent || ""`;

/// The value of the setting named `key` on the screen `screen`.
///
/// A setting whose values are words is a row of choice buttons and the one in force is the pressed
/// one, so this reads `aria-pressed` rather than a `<select>`'s `value` -- the select is gone, because
/// a native one is the operating system's control (its size, its colours, and a list that opens *over*
/// the dialog) and the page has no business handing a person one when the frame already named every
/// word it takes. By name rather than by id, and that is the point: a change sends a line, the run
/// answers with a new state frame, and `paintSettings` rebuilds the rows -- so the node an id was
/// tagged on is *gone* by the time the answer arrives. Measured the hard way: a claim that read the id
/// back saw `Cannot read properties of null` and read it as "the picker never moved".
const VALUE_OF = (screen, key) =>
  `(() => { const row = Array.from(document.querySelectorAll("#fields-${screen} .setting"))
       .find((r) => (r.querySelector(".setting-name") || {}).textContent === ${JSON.stringify(key)});
     const on = row ? row.querySelector(".choices button[aria-pressed='true']") : null;
     return on ? on.textContent.trim() : null; })()`;

/// The words a setting offers, and which of them is in force; the words are the frame's.
const CHOICES_OF = (screen, key) =>
  `(() => { const row = Array.from(document.querySelectorAll("#fields-${screen} .setting"))
       .find((r) => (r.querySelector(".setting-name") || {}).textContent === ${JSON.stringify(key)});
     if (!row) return null;
     const words = Array.from(row.querySelectorAll(".choices button"));
     return { words: words.map((b) => b.textContent.trim()),
              on: (words.find((b) => b.getAttribute("aria-pressed") === "true") || {}).textContent }; })()`;

/// Tag the choice button *after* the one in force, so a claim can press a real key at it.
///
/// A word rather than a `<select>` plus ArrowDown: the control is five buttons now, and pressing one
/// from the keyboard is a real path a person takes through it -- Tab to the word, Enter. What the
/// claim must not do is set `aria-pressed` or call the page's own handler, because then it would be
/// asserting that the page agrees with itself rather than that the run was told.
const NEXT_CHOICE = (screen, key) =>
  `(() => { const row = Array.from(document.querySelectorAll("#fields-${screen} .setting"))
       .find((r) => (r.querySelector(".setting-name") || {}).textContent === ${JSON.stringify(key)});
     if (!row) return null;
     const words = Array.from(row.querySelectorAll(".choices button"));
     const at = words.findIndex((b) => b.getAttribute("aria-pressed") === "true");
     const target = words[(at + 1) % words.length];
     if (!target) return null; target.id = "harness-choice";
     return { word: target.textContent.trim(), disabled: target.disabled }; })()`;

/// The scope of this harness, printed before anything is pressed.
///
/// Kept as data rather than only as the comment at the top of the file: a claim in `docs/web-mode.md`
/// is about a page somebody uses, and the reader of a run's output is the person deciding whether to
/// believe one. ASCII only, because a Windows console in its own code page turns anything else into
/// noise -- the same reason the rest of this file says `--` rather than an em dash.
function announceScope() {
  console.log("driven in a real browser, each against the run's own stdout:");
  for (const door of [
    "  the settings dialog: the door in the sidebar's seat, the rail, a switch pressed as a word,",
    "  an action button, the masked key field",
    "  a report read in the screen it was asked from, and a screen that holds only its own rows",
    "  the two-press destructive menu: a conversation's row, and the jobs panel's stop list",
    "  a conversation row's menu, including naming the one being written",
    "  the sidebar's drag handles, the arrow keys, and the double-click reset",
    "  the panel's own hand: a drag, an arrow, and the width put back",
    "  the model picker from the keyboard",
    "  the composer's send button, and a line the run answers",
    "  a tool block's path buttons, and the preview panel: a grep hit, reload, Escape, a refusal",
    "  the panel's open control: the route reached, and its refusal, without opening a window",
    "  the addresses in the run's own words: a web address, a path, and a scheme that is not the web",
    "  the jobs panel: a running clock, an exit code, a child's own conversation, output, a stop",
  ]) {
    console.log(door);
  }
  console.log("not driven here, and where each is answered instead:");
  for (const gap of [
    "  a report asked for mid-turn: measured from outside the page, tests/cli_output.rs",
    "  the /prompt row's send button: docs/web-mode.md section 12 says it is not measured",
    "  an OS open that succeeds: src/web.rs asserts the command line without running it",
    "  a paste into the composer, an IME, a screen reader, two tabs, touch, a phone viewport",
  ]) {
    console.log(gap);
  }
}

async function main() {
  announceScope();

  const binary = browserPath();
  if (!binary) {
    console.log("no browser found -- name one with --browser <path>");
    process.exit(2);
  }
  if (!fs.existsSync(flintBinary())) {
    console.log(`build first: cargo build (${flintBinary()} is not there)`);
    process.exit(2);
  }
  // The scripted model: a file is written, read back, and missed, a grep prints a line number, and a
  // `list` prints a directory -- the five shapes a path appears in a transcript -- then the run's two
  // kinds of job are started, and prose ends the turn.
  //
  // The command is `node -e` rather than a shell builtin: it is the one program this harness knows
  // is on `PATH` (it is running under it), it prints two lines fifteen seconds apart on every
  // platform, and those two lines are what the panel has to be able to show afterwards. The `task`
  // child makes a request of its own, which this same server answers -- a child inherits the
  // parent's provider -- so the bodies after the `task` are the child's answer and then the
  // parent's.
  //
  // Fifteen seconds, not three: two of the claims below are about a job that is still running when
  // they are made, and the preview claims in between take several seconds of their own. The log's
  // second line is what proves a row opens the *command's* output rather than an empty file.
  //
  // The second command is the other half of the panel: a job that is *stopped*, which needs one that
  // will not end on its own -- sixty seconds, where the claims that wait for an ending have to say
  // which of the two commands they are waiting for.
  const slow = `node -e "console.log('one'); setTimeout(() => console.log('two'), 15000)"`;
  const endless = `node -e "console.log('alive'); setTimeout(() => console.log('never'), 60000)"`;
  // A file for the two claims about the gutter that no other fixture can carry: 300 lines is taller
  // than the panel, and one of them is wider than it -- which is what a number per line has to
  // survive, and what a single `<pre>` with a column beside it cannot do. Written by the scripted
  // turn rather than by this harness, because the panel's only door is a path in a tool block.
  const tall = Array.from({ length: 300 }, (_, at) => "line " + (at + 1)).join("\n") + "\n";
  const script = [
    toolCall("write", { path: "notes.txt", content: "one\ntwo\nthree\n" }),
    toolCall("write", { path: "long.txt", content: tall.replace("line 2", "x".repeat(600)) }),
    toolCall("read", { path: "notes.txt" }),
    toolCall("read", { path: "gone.txt" }),
    toolCall("grep", { pattern: "two", path: "." }),
    // The directory, listed. Its argument is patched below to the run's own absolute working
    // directory, for the same reason the prose's paths are: the listing's door is a path in a tool
    // block, and the claim is about the run's own listing of a real directory rather than a made-up
    // one. The `list` output is where a *name with a space* appears as the run prints it -- `sub dir/`,
    // a directory this harness makes -- which is the one spelling of that name a reader of the text can
    // already see whole.
    toolCall("list", { path: "." }),
    toolCall("bash", { command: slow, background: true }),
    toolCall("bash", { command: endless, background: true }),
    toolCall("task", { prompt: "say hi" }),
    prose("the child's answer"),
    prose("all done"),
  ];
  const model = await stubModel(script);
  const where = scratch(`http://127.0.0.1:${model.port}/v1`);
  // A real picture for the panel to draw, and one that is *named* like a picture but is text.
  //
  // Written by this harness rather than by the scripted turn, because a tool call's arguments are
  // JSON text and a PNG is not text: the `write` tool could not produce one. Both files are on disk
  // in the run's own working directory before the page is opened, and the turn's prose names them, so
  // the press is a press on a path in the transcript -- which is the whole point of the two claims.
  //
  // The bytes are a 1x1 transparent PNG, and it is *real* rather than a signature and filler: the
  // claim is that the browser draws it, and `naturalWidth` is the browser's own word for that.
  fs.writeFileSync(
    path.join(where.cwd, "cell.png"),
    Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
      "base64"
    )
  );
  fs.writeFileSync(path.join(where.cwd, "not-a-picture.png"), "this is a note with a picture's name\n");
  // A directory whose name has a space in it, and a file inside it. Reported by the person using this
  // build: *a directory with a space in its name is still not recognised*. Nothing about that name is
  // special to a filesystem -- it is two tokens to anything reading the transcript as text, and one
  // name to the run -- so the fixture has to be real: the claims below are that a *bare* path with that
  // space in it becomes one button reading the whole name, and that the run's own listing carries the
  // name it built.
  fs.mkdirSync(path.join(where.cwd, "sub dir"), { recursive: true });
  fs.writeFileSync(path.join(where.cwd, "sub dir", "inside.txt"), "deep\n");
  // A Markdown file for the rendered view, written here for the same reason the PNG is: its bytes are the
  // fixture, and one of them is a tag that would run code if this page ever assigned markup. The heading,
  // the list, the fence and the table are there so the claims below can read a *structure* rather than a
  // string that happens to be on screen.
  fs.writeFileSync(
    path.join(where.cwd, "notes.md"),
    "# The notes\n\n" +
      "Some prose with a <b>tag</b> in it, and `code` left as written.\n\n" +
      "- one\n- two\n  - nested\n\n" +
      "> quoted words\n> on two lines\n\n" +
      "| a | b |\n|---|---|\n| 1 | 2 |\n\n" +
      "```js\nconst a = 1;\n```\n"
  );
  // The turn's last answer, and the three shapes of address a claim below is made against: a web
  // address that must be a link, the absolute path of the file this run wrote that must open in the
  // preview, and a `javascript:` address that must stay the words it is. The last one is the reason
  // the splitter has an allowlist rather than a blocklist.
  //
  // Both of the last two bodies carry it, and that is measured rather than defensive: the parent's
  // request after the `task` and the child's own first request race, and on this machine the parent
  // took the first body and the child the second -- so which one ends up in the parent's transcript
  // is a timing detail, and a claim that depended on it would be a claim that fails on a slower
  // machine. The words are the same either way; the child's own conversation is not what is read.
  // The bodies are read as requests arrive, so the last one can be written *after* the scratch
  // directory exists. That matters for one claim: the prose names a real file by its absolute path,
  // and the page has to be able to open it -- which a made-up path could not show.
  const addresses = prose(
    "all done -- see https://example.com/flint for the page, " +
      `${path.join(where.cwd, "notes.txt")} for the file, ` +
      `${path.join(where.cwd, "cell.png")} for the picture, ` +
      `${path.join(where.cwd, "not-a-picture.png")} for the one that only looks like one, ` +
      `${path.join(where.cwd, "notes.md")} for the notes, ` +
      // Two paths for the directory claims below, both of them *bare* -- no quotes anywhere in this
      // prose -- because that is the shape the report was about: a name with a space in it, and a
      // directory that is not there. The first is one name and two tokens to any reader of the text,
      // which is exactly why the page asks the run where it ends.
      `${path.join(where.cwd, "sub dir")} for the directory with a space in its name, ` +
      `${path.join(where.cwd, "nothere")}${path.sep} for a directory that is gone, ` +
      "and javascript:alert(1) for the scheme that is not the web"
  );
  script[script.length - 1] = addresses;
  script[script.length - 2] = addresses;
  // ...and the `list` argument, now that the scratch directory exists: the run's own working
  // directory, which is the directory it is about to read.
  script[5] = toolCall("list", { path: where.cwd });
  const flint = await startFlint(where);
  console.log(`flint: ${flint.url}\n  home: ${where.home}\n  browser: ${binary}\n  model: port ${model.port}\n`);

  const chrome = await startBrowser(binary, flint.url, where.home);
  const page = await attach(chrome.page);
  const before = () => flint.text().length;

  try {
    // The page is only interactive once the state frame has arrived: the door is offered then, and the
    // dialog's screens are drawn from that same frame. The door is what is waited for here rather than
    // a control, because the screens are *built* when the dialog opens -- there is nothing inside it
    // to look for until somebody opens it.
    await page.waitFor(`document.getElementById("settings-open").hidden === false`, "the settings door");

    // ---- the settings dialog -----------------------------------------------
    // Everything that changes the run is behind one door in the header, so this file opens it the way
    // a person does -- a press on the door, a press on the rail, a press on `close` -- and every claim
    // below is made *inside* it. That is the claim this whole section exists for: a header that had
    // kept the controls would pass each of those claims by accident, which is why the door is checked
    // first, and why the settings and the rows are asserted to be inside the dialog rather than merely
    // present.
    //
    // The screens are named the way the rail names them ("this run", "this conversation"), and the ids
    // below are the page's own for the place behind each name: `pane-<key>`, `fields-<key>` and
    // `row-list-<key>`, with the key the frame files on.
    const openSettings = async (section) => {
      if (await page.js(`document.getElementById("settings").hidden`)) {
        await page.click("#settings-open");
        await page.waitFor(`document.getElementById("settings").hidden === false`, "the settings dialog", 20);
      }
      if (section) {
        // One id per screen rather than one shared tag: the rail is drawn once and stays, so a
        // shared id would be left behind on the button pressed last time and `querySelector` would
        // find *that* one in document order -- which measured as "the commands pane never opened"
        // and a press landing on a hidden field at 0,0. The id is slugged because the rail's names
        // have spaces in them ("this run") and a space is a descendant combinator in a selector.
        const id = "harness-section-" + section.replace(/\s+/g, "-");
        const tagged = await page.js(
          `(() => { const b = Array.from(document.querySelectorAll("#settings-nav button"))
                .find((x) => x.textContent.trim() === ${JSON.stringify(section)});
              if (!b) return false; b.id = ${JSON.stringify(id)}; return true; })()`
        );
        if (tagged) await page.click("#" + id);
        await sleep(150);
      }
    };
    const closeSettings = async () => {
      if ((await page.js(`document.getElementById("settings").hidden`)) === false) {
        await page.click("#settings-close");
        await page.waitFor(`document.getElementById("settings").hidden === true`, "the dialog shut", 20);
      }
    };

    // The door is the sidebar's *bottom seat*, which is where DSH's own settings trigger sits and
    // what was asked for here: it was the last control on the header's line, and a control that
    // changes the process was the one thing there that was not a fact about the conversation. So it
    // is measured where it is -- inside the sidebar, in the seat under the list, at the bottom -- and
    // the header is measured to hold no control at all, which is the state this round arrived at.
    const door = await page.js(
      `(() => { const seat = document.getElementById("side-foot");
         const sidebar = document.getElementById("sidebar");
         const list = document.getElementById("sessions");
         const seatBox = seat.getBoundingClientRect();
         const listBox = list.getBoundingClientRect();
         return { shown: !document.getElementById("settings-open").hidden,
                  dialog: document.getElementById("settings").hidden,
                  mask: document.getElementById("settings-mask").hidden,
                  inSeat: !!document.querySelector("#side-foot #settings-open"),
                  inSidebar: sidebar.contains(seat),
                  below: Math.round(seatBox.top) >= Math.round(listBox.bottom) - 2,
                  header: !!document.querySelector(".head-line #settings-open"),
                  raw: !!document.querySelector(".head-line select, .head-line input, .head-line .setting") }; })()`
    );
    check(
      "the settings door is the sidebar's bottom seat, and what is behind it starts shut",
      !!door && door.shown === true && door.dialog === true && door.mask === true &&
        door.inSeat === true && door.inSidebar === true && door.below === true &&
        door.header === false && door.raw === false,
      `door: ${JSON.stringify(door)}`
    );

    await page.click("#settings-open");
    const opened = await page
      .waitFor(
        `document.getElementById("settings").hidden === false
           ? { mask: document.getElementById("settings-mask").hidden === false,
               focused: document.activeElement ? document.activeElement.id : "",
               first: document.getElementById("pane-model").hidden === false,
               rails: document.querySelectorAll("#settings-nav button").length,
               screens: document.querySelectorAll("#settings-panes .pane").length,
               settings: document.querySelectorAll("#fields-run .setting").length,
               rows: document.querySelectorAll("#row-list-run .row").length }
           : null`,
        "the settings dialog",
        20
      )
      .catch(() => null);
    check(
      "one press opens it, with the mask and the keyboard inside it",
      !!opened && opened.mask === true && opened.first === true && opened.focused === "settings-close",
      `dialog: ${JSON.stringify(opened)}`
    );
    check(
      "and every screen is built, with the settings and the rows the frame named already in them",
      !!opened && opened.rails === 6 && opened.screens === 6 && opened.settings > 0 && opened.rows > 0,
      `dialog: ${JSON.stringify(opened)}`
    );

    await openSettings("this run");
    const inside = await page.js(
      `(() => { const dialog = document.getElementById("settings");
         const box = document.getElementById("fields-run");
         const list = document.getElementById("row-list-run");
         return { settings: dialog.contains(box), rows: dialog.contains(list),
                  drawn: box.hidden === false, up: document.getElementById("pane-run").hidden === false }; })()`
    );
    check(
      "the run's settings are inside the dialog, and the frame has drawn them",
      !!inside && inside.settings === true && inside.rows === true && inside.drawn === true &&
        inside.up === true,
      `inside: ${JSON.stringify(inside)}`
    );

    // ---- a switch, which is one setting among the others ---------------------
    // The five switches are settings now, filed on the `run` screen with the words they take: the
    // claim is the same one it always was -- the names come from the run, not from the page -- and it
    // is made by reading the names off the rows rather than off a container of its own.
    const names = await page.js(
      `Array.from(document.querySelectorAll("#fields-run .setting-name")).map((n) => n.textContent)`
    );
    check(
      "the switches are drawn from the run's own state",
      Array.isArray(names) && names.includes("readonly") && names.includes("verbose"),
      `names: ${JSON.stringify(names)}`
    );
    const started = before();
    // Read before the press, for the claim under it: what the sidebar holds is conversations, and a
    // switch is a decision about how to ask rather than something said.
    const rowsBeforeSwitch = await page.js(`document.querySelectorAll("#sessions li").length`);
    // The switch that is moved is chosen by *name*, not by position: a row added to the run screen
    // above it must not silently change which command this claim presses.
    const was = await page.js(VALUE_OF("run", "verbose"));
    const words = await page.js(CHOICES_OF("run", "verbose"));
    check(
      "a switch is a labelled control on the screen it belongs to, offering the run's own words",
      typeof was === "string" && was.length > 0 && !!words && words.words.length > 1 &&
        words.words.includes(words.on),
      `verbose: ${JSON.stringify(was)} of ${JSON.stringify(words)}`
    );
    // A press rather than a synthetic `change`: the words are real buttons now, so the honest path is
    // the pointer's own -- and a *focused* button is what makes the other one available, which is why
    // the focus is read back before the press. (The keyboard's Enter is not sent here: a `<button>`'s
    // activation on Enter is the browser's own default action and the protocol's raw key event does
    // not carry it, so a claim about it would be a claim about Chrome rather than about this page.
    // What the page has to get right is that the word is a real, focusable, uncovered button.)
    const target = await page.js(NEXT_CHOICE("run", "verbose"));
    check(
      "and the word beside the one in force is pressable, and reachable by Tab",
      !!target && target.disabled === false && target.word !== was,
      `next word: ${JSON.stringify(target)}, in force: ${JSON.stringify(was)}`
    );
    await page.js(`document.getElementById("harness-choice").focus(); true`);
    const focused = await page.js(`document.activeElement && document.activeElement.id`);
    check(
      "and a word takes the keyboard like any other control",
      focused === "harness-choice",
      `focused: ${JSON.stringify(focused)}`
    );
    await page.click("#harness-choice");
    const moved = await page
      .waitFor(
        `${VALUE_OF("run", "verbose")} !== ${JSON.stringify(was)} && ${VALUE_OF("run", "verbose")}`,
        "the switch to move",
        25
      )
      .catch(() => null);
    // Both halves matter and they are different halves: the switch moved on the page, and the run
    // was told. A page that only moved its own control would leave the run on the old setting, and
    // the run's own output is the only witness to which of those happened. A thrown exception is an
    // answer too (`js` reports it as a string), so it is excluded here rather than counted as a value.
    check(
      "a switch moves the control and the run together",
      typeof moved === "string" && !moved.startsWith("threw:") && moved !== was &&
        flint.text().length > started,
      `page: ${JSON.stringify(was)} -> ${JSON.stringify(moved)}, run printed: ` +
        JSON.stringify(flint.text().slice(started).slice(0, 200))
    );
    // ...and what it does *not* do, which is put a conversation in the sidebar. It did: measured on
    // 2026-09-23 in a real home, every door that records how a run asks -- `--thinking`, `--schema`,
    // `/thinking`, `/model`, `/provider`, `/reload`, `/config set`, and these rows -- created an empty
    // session file, so a person who moved one setting and closed the window had a conversation with no
    // messages in it. A run-level decision is written into the conversation's file once there is one;
    // it does not make one. The row the run is *writing* arrives with the first thing it says, which is
    // what the sidebar's own menu below needs and now sets up explicitly.
    const rowsAfterSwitch = await page.js(`document.querySelectorAll("#sessions li").length`);
    check(
      "and a switch alone does not add a conversation to the sidebar",
      rowsAfterSwitch === rowsBeforeSwitch,
      `sidebar rows: ${rowsBeforeSwitch} -> ${rowsAfterSwitch}`
    );

    // ---- the run's own actions ---------------------------------------------
    // An action is a button, not a row of reference: it takes no argument, and the claim is that it is
    // drawn on the screen the frame filed it on and nowhere else. `/reload` is the run's; `/new`
    // belongs to this conversation, and a page that put every action on one screen would pass a claim
    // that only looked for `/reload`.
    const actions = await page.js(
      `Array.from(document.querySelectorAll("#row-list-run button.row.action"))
         .map((b) => ((b.querySelector("code") || {}).textContent || "").trim())`
    );
    const elsewhere = await page.js(`(${ROWS_OF("conversation")}).includes("/reload")`);
    check(
      "the run's actions are buttons on the run's own screen",
      Array.isArray(actions) && actions.includes("/reload"),
      `actions: ${JSON.stringify(actions)}`
    );
    check(
      "and an action is not also drawn on another screen's rows",
      elsewhere === false,
      `the conversation screen carries /reload: ${JSON.stringify(elsewhere)}`
    );

    // ---- the screens: one at a time, and each holds its own rows ------------
    // The rail is how a person reaches them, and one screen at a time is the whole point of the
    // split. The claim has two halves, and the second is the one this round added: the screen that is
    // up holds the rows the frame filed on it, *and* a row filed elsewhere is not in it.
    await openSettings("this conversation");
    const section = await page
      .waitFor(
        `document.getElementById("pane-conversation").hidden === false
           ? { run: document.getElementById("pane-run").hidden,
               marked: document.querySelectorAll("#settings-nav button[aria-current='true']").length,
               rail: document.querySelectorAll("#settings-nav button").length }
           : null`,
        "the conversation screen",
        20
      )
      .catch(() => null);
    check(
      "the rail shows one screen at a time, and says which",
      !!section && section.run === true && section.marked === 1 && section.rail === 6,
      `screens: ${JSON.stringify(section)}`
    );
    const listed = await page.js(ROWS_OF("conversation"));
    const foreign = await page.js(ROWS_OF("limits"));
    // Named rows from four of the classes, because "the rows are there" is not the claim: the claim is
    // that they are the *run's* rows, drawn from the frame -- and a screen with rows in it that had
    // lost a class would still look like a list. Read as the screen's text rather than as `code`
    // elements, because a form row's own line is its submit button, not a label.
    const wanted = ["/delete <n|id>", "/name", "/sessions", "/resume <n|id>"];
    check(
      "the conversation screen lists the run's rows, across the classes",
      wanted.every((line) => String(listed).includes(line)),
      `missing: ${JSON.stringify(wanted.filter((line) => !String(listed).includes(line)))}, ` +
        `screen: ${JSON.stringify(String(listed).slice(0, 200))}`
    );
    check(
      "and the rows filed on another screen are not on this one",
      !String(listed).includes("/config") && String(foreign).includes("/config"),
      `conversation: ${JSON.stringify(String(listed).slice(0, 200))}, ` +
        `limits: ${JSON.stringify(String(foreign).slice(0, 200))}`
    );

    // A report is read *here*: its answer belongs in the screen it was asked from, and the terminal
    // did not ask. `/config` is filed on `limits`, so the rail goes there first.
    await openSettings("limits");
    const beforeReport = before();
    await page.waitFor(ROW("/config", "limits"), "the /config row");
    await page.click("#harness-target");
    const reading = await page.waitFor(
      `(document.querySelector("#row-list-limits .reading") || {}).textContent || ""`,
      "the report's answer"
    );
    await sleep(300);
    check(
      "a report is answered in the screen it was asked from",
      typeof reading === "string" && reading.includes("config"),
      `reading: ${JSON.stringify(String(reading).slice(0, 160))}`
    );
    check(
      "and not printed in the terminal, where nobody asked for it",
      flint.text().slice(beforeReport).trim() === "",
      `terminal gained: ${JSON.stringify(flint.text().slice(beforeReport))}`
    );
    const shot = await page.send("Page.captureScreenshot", { format: "png" });
    if (shot.result && shot.result.data) {
      fs.writeFileSync(path.join(where.home, "panel.png"), Buffer.from(shot.result.data, "base64"));
      console.log(`        screenshot: ${path.join(where.home, "panel.png")}`);
    }
    await page.click("#row-list-limits .back"); // back to the rows
    await sleep(300);

    // ---- an action button --------------------------------------------------
    // It sits on the *run* screen, so the rail goes back there for this press -- a hidden button can be
    // found by a selector and would then swallow the click, which is a failure this harness has
    // already had once.
    await openSettings("this run");
    const beforeAction = before();
    await page.js(`(() => { const b = Array.from(document.querySelectorAll("#row-list-run button.row.action"))
      .find((b) => ((b.querySelector("code") || {}).textContent || "").trim() === "/reload");
      if (!b) return null; b.id = "harness-action"; return true; })()`);
    await page.click("#harness-action");
    // Waited for rather than slept on, like the composer below it: how long a run takes to print a
    // line is the run's business, and a sleep would be a guess about this machine.
    let reloaded = "";
    for (let i = 0; i < 25 && !reloaded; i += 1) {
      await sleep(200);
      reloaded = flint.text().slice(beforeAction);
    }
    check(
      "an action button runs the command",
      reloaded.includes("reloaded"),
      `terminal gained: ${JSON.stringify(reloaded.slice(0, 200))}`
    );
    // The run answered, so a fresh state frame has been drawn; the rail goes to the model screen for
    // the credential row below.
    await openSettings("model");

    // ---- the masked credential field --------------------------------------
    const secret = "sk-not-a-real-key-0000";
    const form = `(() => { const f = Array.from(document.querySelectorAll("#row-list-model form.field"))
        .find((f) => (f.querySelector("button.send") || {}).textContent === "/provider key");
      if (!f) return null; f.querySelector("input").id = "harness-input";
      f.querySelector("button.send").id = "harness-submit"; return f.querySelector("input").type; })()`;
    const kind = await page.waitFor(form, "the credential field");
    check("the credential field is masked", kind === "password", `input type: ${JSON.stringify(kind)}`);
    const beforeKey = before();
    await page.js(`document.getElementById("harness-input").focus(); true`);
    await page.send("Input.insertText", { text: secret });
    await page.click("#harness-submit");
    let saved = "";
    for (let i = 0; i < 25 && !saved.includes("key saved"); i += 1) {
      await sleep(200);
      saved = flint.text().slice(beforeKey);
    }
    const emptied = await page.js(`document.getElementById("harness-input").value`);
    check(
      "a key typed into the field reaches the run",
      saved.includes("key saved"),
      `terminal gained: ${JSON.stringify(saved.slice(0, 200))}`
    );
    check(
      "and the field is cleared, so it does not sit on screen",
      emptied === "",
      `field still holds: ${JSON.stringify(emptied)}`
    );
    check(
      "and the secret is nowhere in the page",
      !(await page.js(`document.documentElement.outerHTML.includes(${JSON.stringify(secret)})`)),
      "the key is in the page's own markup"
    );
    check(
      "and it did reach the config file it was for",
      fs.readFileSync(path.join(where.home, "config.toml"), "utf8").includes(secret),
      "the key never reached config.toml, so the assertions above could pass on a command that never ran"
    );

    // ---- the destructive row asks first ------------------------------------
    // `/delete <n|id>` is filed on *this conversation*, so the rail goes there and the candidates are
    // read out of that screen's rows -- which is also the check that one screen's two-press state does
    // not spill into the others.
    await openSettings("this conversation");
    const sessionsBefore = fs.readdirSync(path.join(where.home, "sessions")).sort();
    const beforeDanger = before();
    await page.waitFor(ROW("/delete <n|id>", "conversation"), "the /delete row");
    await page.click("#harness-target");
    const candidates = await page.waitFor(
      `document.querySelectorAll("#row-list-conversation button.row.danger").length`,
      "the candidates",
      25
    ).catch(() => 0);
    check(
      "a destructive row opens its candidates instead of sending",
      typeof candidates === "number" && candidates >= 1,
      `candidates: ${JSON.stringify(candidates)}`
    );
    // The first press must have sent nothing at all: this is the property that makes the two-press
    // shape worth having, and the terminal is where a sent line would show.
    check(
      "and nothing was sent by that press",
      flint.text().slice(beforeDanger).trim() === "",
      `terminal gained: ${JSON.stringify(flint.text().slice(beforeDanger))}`
    );
    await page.click("#row-list-conversation .back");
    await sleep(300);
    const sessionsAfter = fs.readdirSync(path.join(where.home, "sessions")).sort();
    check(
      "and backing out deletes nothing",
      JSON.stringify(sessionsBefore) === JSON.stringify(sessionsAfter),
      `${JSON.stringify(sessionsBefore)} -> ${JSON.stringify(sessionsAfter)}`
    );

    // Shut again, and by the *second* door: the mask, which is the one a person who has stopped
    // reading settings uses. Everything below is about the page itself -- the sidebar, the hands, the
    // composer, the panel -- and a modal over them would make every press below land on the mask.
    await page.click("#settings-mask", [4, 4]);
    await page.waitFor(`document.getElementById("settings").hidden === true`, "the dialog shut", 20);
    const shut = await page.js(
      `({ dialog: document.getElementById("settings").hidden,
          mask: document.getElementById("settings-mask").hidden,
          focus: document.activeElement ? document.activeElement.id : "" })`
    );
    check(
      "a press outside the dialog shuts it, and the keyboard goes back to the door",
      shut.dialog === true && shut.mask === true && shut.focus === "settings-open",
      `after the mask: ${JSON.stringify(shut)}`
    );

    // ---- the sidebar's own menu --------------------------------------------
    // `⋯` at the end of a conversation's row opens that conversation's actions, drawn from the same
    // frame rows the panel uses with this row's number appended. §11 called this "reasoned rather
    // than seen", so what is checked here is the two-press shape on the row itself: the press that
    // opens the menu sends nothing, the row in it says the whole line before it sends it, and the
    // line that goes out belongs to *that* conversation.
    //
    // The run's own conversation is named first, and that is the setup this phase needs rather than
    // politeness. The two rows in the sidebar belong to other conversations: until this run says
    // something it has no conversation of its own to have a menu on (a settings switch used to create
    // one, which is the defect the claim above now holds). Naming it is the cheapest thing it can say
    // -- it writes the file and asks the model nothing, so the scripted answers below stay on the
    // requests they were written for.
    const beforeRunName = before();
    await page.js(`document.getElementById("message").focus(); true`);
    await page.send("Input.insertText", { text: "/name the conversation being written" });
    await page.click("#send");
    let namedRun = "";
    for (let i = 0; i < 25 && !namedRun.includes("named:"); i += 1) {
      await sleep(200);
      namedRun = flint.text().slice(beforeRunName);
    }
    check(
      "the run's own conversation gets a row of its own once it has said something",
      namedRun.includes("named:"),
      `run printed: ${JSON.stringify(namedRun.slice(0, 200))}`
    );
    // And the row is waited for rather than assumed. A `/name` puts a `sessions` frame on the feed --
    // that is the page's own documented behaviour -- so the sidebar is redrawn one round-trip later,
    // and a menu opened before that redraw is replaced under the pointer. Waiting for the `current`
    // row is waiting for that redraw, because the row is the thing the redraw is what carries.
    const ownRow = await page
      .waitFor(`!!document.querySelector("#sessions li.current")`, "the run's own row", 25)
      .catch(() => null);
    check(
      "and that row is the one marked current, which is what the menu below belongs to",
      ownRow === true,
      `rows: ${JSON.stringify(await page.js(
        `Array.from(document.querySelectorAll("#sessions li")).map((li) => [li.title, li.className])`
      ))}`
    );
    const rowState = () => page.js(
      `(() => {
        const rows = Array.from(document.querySelectorAll("#sessions li"));
        const current = rows.find((li) => li.classList.contains("current")) || rows[0];
        return { n: rows.length, current: current ? current.title : null,
                 hasMore: !!(current && current.querySelector("button.more")) };
      })()`
    );
    const rows = await rowState();
    check(
      "a conversation's row carries its own menu button",
      rows.hasMore === true,
      `rows: ${JSON.stringify(rows)}`
    );
    const beforeMenu = before();
    await page.js(
      `(() => { const li = document.querySelector("#sessions li.current") ||
          document.querySelector("#sessions li");
        li.querySelector("button.more").id = "harness-more"; return true; })()`
    );
    await page.click("#harness-more");
    const menu = await page.js(
      `(() => { const li = document.querySelector("#sessions li.current") ||
          document.querySelector("#sessions li");
        const m = li && li.querySelector(".session-menu"); if (!m) return null;
        const n = (li.querySelector("span.n") || {}).textContent || "";
        const box = (node) => { const r = node.getBoundingClientRect();
          return { top: Math.round(r.top), bottom: Math.round(r.bottom), height: Math.round(r.height) }; };
        return { number: n,
                 rows: Array.from(m.querySelectorAll("button.row")).map((b) => b.textContent),
                 codes: m.querySelectorAll("code").length,
                 form: Array.from(m.querySelectorAll("form.field button.send")).map((b) => b.textContent),
                 box: box(m),
                 rowBoxes: Array.from(m.querySelectorAll("button.row")).map(box) }; })()`
    );
    // What the rows *say* is the frame's own sentence for each action -- "delete one", "file one away,
    // out of the list" -- and never the line the press sends. Reported directly, 2026-09-23: *the
    // conversation's three dots has no commands to choose, and do not write the command out, write
    // what it does.* The claim is made over the drawing rather than over a list of sentences: the
    // run's words are the run's, and a harness that spelled them here would be a second copy that
    // could agree with itself while the page showed a command.
    check(
      "the menu opens with that conversation's actions, written as what they do",
      !!menu && Array.isArray(menu.rows) && menu.rows.length > 0 &&
        menu.codes === 0 &&
        menu.rows.every((line) => String(line).trim().length > 0 && !String(line).includes("/")),
      `menu: ${JSON.stringify(menu)}`
    );
    check(
      "and opening it sent nothing",
      flint.text().slice(beforeMenu).trim() === "",
      `terminal gained: ${JSON.stringify(flint.text().slice(beforeMenu))}`
    );
    // That the rows *exist* is not that they can be seen, and the difference is the whole of this
    // claim. Reported directly, 2026-09-23 -- twice, the second time after the rows were already right
    // -- the three dots opened "an empty bar with nothing in it": the menu wore the class the
    // composer's `/` menu is styled by, whose rule pins `left`, `right` *and* `bottom`, so together
    // with the sidebar's `top: 100%` the box had both ends pinned against the row and collapsed to
    // its own border and padding -- 10px, with its rows in the scrollable overflow of a box with no
    // height. Every claim above this one was reading `textContent` and passing. So: the box the rows
    // are drawn in has to hold them.
    check(
      "the menu is a box its rows fit in",
      !!menu && menu.rows.length > 0 && menu.rowBoxes.length === menu.rows.length &&
        menu.rowBoxes.every((row) => row.bottom <= menu.box.bottom + 1 && row.top >= menu.box.top - 1),
      `menu: ${JSON.stringify(menu && { box: menu.box, rows: menu.rowBoxes })}`
    );
    // The rename belongs to the row the run is writing and to no other, which is why it is looked
    // for *here* rather than assumed: a menu that offered it on every row would rename whatever
    // happened to be open.
    check(
      "the conversation being written offers a name field in the same menu",
      !!menu && Array.isArray(menu.form) && menu.form.length > 0 &&
        menu.form.every((word) => String(word).length > 0 && !String(word).includes("/")),
      `menu forms: ${JSON.stringify(menu && menu.form)}`
    );
    const nameInput = `(() => { const li = document.querySelector("#sessions li.current") ||
        document.querySelector("#sessions li");
      const f = li.querySelector(".session-menu form.field");
      if (!f) return null; f.querySelector("input").id = "harness-name";
      f.querySelector("button.send").id = "harness-name-send"; return true; })()`;
    if (await page.js(nameInput)) {
      const beforeName = before();
      const wasNamed = await page.js(`document.getElementById("harness-name").value`);
      await page.js(`document.getElementById("harness-name").focus(); true`);
      await page.send("Input.insertText", { text: "named from the sidebar" });
      await page.click("#harness-name-send");
      let named = "";
      for (let i = 0; i < 25 && !named.includes("named:"); i += 1) {
        await sleep(200);
        named = flint.text().slice(beforeName);
      }
      // The field arrives holding the conversation's own name -- it is the row `current` that offers
      // it, and a rename is an edit -- so what is typed lands *after* that name and the line the run
      // prints carries both. Asserting the whole string would be asserting that the page clears a
      // field a person may well be editing; what the claim is about is that what was typed reaches
      // the run at all, which is why the name is read back off the field rather than assumed.
      check(
        "a name typed into that field reaches the run",
        named.includes("named from the sidebar") &&
          (wasNamed === "" || named.includes(`named: ${wasNamed}`)),
        `field held ${JSON.stringify(wasNamed)}, terminal gained: ` +
          JSON.stringify(named.slice(0, 200))
      );
    }

    // A menu on the last row of a full sidebar is still a menu. The list scrolls, and a menu that
    // hangs *below* its row can be drawn past the bottom of the box the browser lets the list paint
    // in -- reachable only by scrolling a sidebar nobody thought to scroll. The window is shortened
    // here rather than the list grown, and the list is scrolled to its bottom, which is where it sits
    // when somebody is looking at what they were just doing. Measured with the flip removed from the
    // page: the menu is drawn at y=243 with the list's own box ending at y=243 -- entirely below it,
    // which is the second half of the report this round began with.
    await page.send("Emulation.setDeviceMetricsOverride", {
      width: 1400, height: 300, deviceScaleFactor: 1, mobile: false,
    });
    await sleep(400);
    const bottomRow = await page.js(
      `(() => { const list = document.getElementById("sessions"); if (!list) return null;
        const items = () => Array.from(list.querySelectorAll("li"));
        // Scrolled to the bottom, which is where a long list sits when somebody is looking at their
        // most recent conversations -- and the only state in which the last row is against the box.
        list.scrollTop = list.scrollHeight;
        const last = items()[items().length - 1]; if (!last) return null;
        const title = last.title;
        const more = last.querySelector("button.more"); if (!more) return null;
        // The press repaints the list, so the row it was on is a new node afterwards: the row is found
        // again by title rather than held across the click, which is what "the list is a function of
        // the state" means for anything driving it.
        more.click();
        const li = items().find((r) => r.title === title);
        const m = li && li.querySelector(".session-menu"); if (!m) return null;
        const box = (node) => { const r = node.getBoundingClientRect();
          return { top: Math.round(r.top), bottom: Math.round(r.bottom) }; };
        const room = box(list);
        const readable = () => {
          const r = box(m);
          return r.top >= room.top - 1 && r.bottom <= room.bottom + 1;
        };
        const flipped = readable();
        const up = m.classList.contains("up");
        // The control: the same menu with the flip taken off it, which is where it would have been
        // drawn. A claim that only checked the page as it stands could pass over a menu that happened
        // to fit; this one cannot, because it also requires that *not* flipping it does not.
        m.classList.remove("up");
        m.getBoundingClientRect();
        const unflipped = readable();
        return { title: title, rows: items().length, up: up, flipped: flipped,
                 unflipped: unflipped, menu: box(m), list: room }; })()`
    );
    check(
      "a menu on the last conversation opens where it can be read, and only the flip puts it there",
      !!bottomRow && bottomRow.up === true && bottomRow.flipped === true &&
        bottomRow.unflipped === false,
      `bottom row: ${JSON.stringify(bottomRow)}`
    );
    // Closed again by the same press that opened it, so the phase below starts with no menu open: its
    // own press is a toggle, and a menu already open on the row it means to open would close.
    await page.js(
      `(() => { const items = Array.from(document.querySelectorAll("#sessions li"));
        const li = items[items.length - 1]; if (!li) return null;
        const more = li.querySelector("button.more"); if (more) more.click(); return true; })()`
    );
    await page.send("Emulation.clearDeviceMetricsOverride");
    await sleep(400);

    // A menu is a box the *row* holds, sideways as well as downward. The sidebar can be dragged down
    // to 160px, and this box carried `min-width: 150px` against `max-width: 100%` -- a used width is
    // never below its own floor, so on a sidebar dragged to the narrow end the box grew past the row's
    // left edge and out of the window. Reported directly, 2026-09-23: *the three dots' little window is
    // too close to the left, it is off the screen*. The width is made with the grip, which is the door
    // a person has for it, and the geometry is measured against the sidebar and the viewport rather
    // than against what the page meant to do.
    {
      const dragged = await page.drag("#grip", -120);
      const side = await page.js(
        `(() => { const r = document.getElementById("sidebar").getBoundingClientRect();
          return { left: Math.round(r.left), right: Math.round(r.right), width: Math.round(r.width),
                   win: window.innerWidth }; })()`
      );
      const narrow = await page.js(
        `(() => { const items = () => Array.from(document.querySelectorAll("#sessions li"));
          const first = items()[0]; if (!first) return null;
          const title = first.title;
          // Found again by title after the press, because the press repaints the list and the row it
          // was on is a new node afterwards -- the same reason the flip claim above re-finds its row.
          first.querySelector("button.more").click();
          const li = items().find((r) => r.title === title);
          const m = li && li.querySelector(".session-menu"); if (!m) return null;
          const box = m.getBoundingClientRect();
          const rows = Array.from(m.querySelectorAll("button.row, form.field"));
          return { left: Math.round(box.left), right: Math.round(box.right),
                   width: Math.round(box.width), height: Math.round(box.height),
                   rows: rows.length,
                   rowsHaveWidth: rows.every((r) => r.getBoundingClientRect().width > 0),
                   win: window.innerWidth }; })()`
      );
      check(
        "a menu on a sidebar dragged narrow stays on the screen, inside the row it belongs to",
        dragged === true && !!narrow && narrow.rows > 0 && narrow.rowsHaveWidth === true &&
          narrow.width > 40 && narrow.height > 10 &&
          narrow.left >= side.left - 1 && narrow.right <= side.right + 1 &&
          narrow.left >= 0 && narrow.right <= narrow.win,
        `sidebar: ${JSON.stringify(side)}, menu: ${JSON.stringify(narrow)}`
      );
      // The control: the rule put back on the same box, which is where it was being drawn. Both halves
      // of it, because the floor alone no longer decides anything -- `max-width: calc(100% - 8px)` caps
      // the box whatever `min-width` says, which *is* the fix, so a control that restored only the
      // floor would be measuring the fix. A claim that only measured the page as it stands could pass
      // on a sidebar that happened to be wide enough; this one cannot, because it also requires that
      // the old rule does *not* fit.
      const withFloor = await page.js(
        `(() => { const m = document.querySelector("#sessions li .session-menu"); if (!m) return null;
          m.style.minWidth = "150px"; m.style.maxWidth = "100%";
          const r = m.getBoundingClientRect();
          return { left: Math.round(r.left), right: Math.round(r.right), win: window.innerWidth }; })()`
      );
      check(
        "and the rule it used to carry is what put it off the screen",
        !!withFloor && withFloor.left < 0,
        `menu with the old rule: ${JSON.stringify(withFloor)}`
      );
      // Put back the way a person puts it back, and the menu closed by the press that opened it.
      await page.js(
        `(() => { const li = document.querySelector("#sessions li");
          const m = li && li.querySelector(".session-menu");
          if (m) { m.style.minWidth = ""; m.style.maxWidth = ""; }
          const b = li && li.querySelector("button.more"); if (b) b.click(); return true; })()`
      );
      await page.doubleClick("#grip");
      await sleep(300);
    }

    // The second press is the one that sends, and it sends *this row's* number: the fixture session
    // is removed by the menu on its own row, and the file is the witness (the terminal would agree
    // with a menu that had sent the wrong conversation's number and been refused).
    const victim = "111-1";
    const victimThere = fs.existsSync(path.join(where.home, "sessions", `${victim}.jsonl`));
    const beforeDelete = before();
    const aimed = await page.js(
      `(() => { const li = Array.from(document.querySelectorAll("#sessions li"))
          .find((r) => r.title === ${JSON.stringify(victim)});
        if (!li) return null; const b = li.querySelector("button.more");
        if (!b) return null; b.id = "harness-victim"; return true; })()`
    );
    if (aimed) {
      await page.click("#harness-victim");
      // The row that deletes, found by the run's own sentence for it rather than by the line it
      // sends: the menu no longer draws the command at all (reported directly, 2026-09-23), so the
      // words on the row are the frame's help for `/delete`, and the file disappearing below is what
      // says the press carried *this* conversation's number.
      const victimRow = await page.js(
        `(() => { const li = Array.from(document.querySelectorAll("#sessions li"))
            .find((r) => r.title === ${JSON.stringify(victim)});
          const m = li && li.querySelector(".session-menu"); if (!m) return null;
          const b = Array.from(m.querySelectorAll("button.row"))
            .find((b) => String(b.textContent).trim() === "delete one");
          if (!b) return null; b.id = "harness-delete-row";
          return b.textContent; })()`
      );
      check(
        "a fixture conversation's menu offers the same actions, in its own words",
        victimRow === "delete one",
        `row: ${JSON.stringify(victimRow)}`
      );
      check(
        "and opening that menu sent nothing either",
        flint.text().slice(beforeDelete).trim() === "",
        `terminal gained: ${JSON.stringify(flint.text().slice(beforeDelete))}`
      );
      await page.click("#harness-delete-row");
      let removed = "";
      for (let i = 0; i < 25 && !removed.includes("deleted"); i += 1) {
        await sleep(200);
        removed = flint.text().slice(beforeDelete);
      }
      check(
        "the second press sends the line, and the run removes that conversation",
        victimThere && !fs.existsSync(path.join(where.home, "sessions", `${victim}.jsonl`)),
        `run printed: ${JSON.stringify(removed.slice(0, 200))}, still there: ` +
          `${fs.existsSync(path.join(where.home, "sessions", `${victim}.jsonl`))}`
      );
    }

    // ---- the two hands -----------------------------------------------------
    // The grips are the one control whose state is not in a frame: they set `--side` and `--read` on
    // the app element and nothing else, so every claim here is read off the style and the run is not
    // involved at all. Both are dragged the way a pointer drags them (press, move, release, with the
    // button held), nudged with the arrow keys, and put back with a double-click -- the three
    // gestures the page documents.
    const widthOf = (property) =>
      page.js(
        `(() => { const app = document.getElementById("app");
          const raw = app.style.getPropertyValue(${JSON.stringify(property)});
          return { set: raw, px: parseFloat(raw) || null };
        })()`
      );
    const sideBefore = await widthOf("--side");
    const heldLeft = await page.drag("#grip", 72);
    const sideAfter = await widthOf("--side");
    check(
      "the sidebar's hand takes a real drag",
      heldLeft === true && sideAfter.px !== null && sideAfter.px > (sideBefore.px || 0) + 40,
      `dragging: ${heldLeft}, --side: ${JSON.stringify(sideBefore)} -> ${JSON.stringify(sideAfter)}`
    );
    const sideNudged = await (async () => {
      await page.js(`document.getElementById("grip").focus(); true`);
      await page.key("ArrowRight", 39);
      return widthOf("--side");
    })();
    check(
      "and the arrow keys move the boundary it belongs to",
      sideNudged.px !== null && sideNudged.px > (sideAfter.px || 0) + 8,
      `--side: ${JSON.stringify(sideAfter)} -> ${JSON.stringify(sideNudged)}`
    );
    await page.doubleClick("#grip");
    const sideReset = await widthOf("--side");
    check(
      "and a double-click puts the width back, rather than leaving a number behind",
      sideReset.set === "",
      `--side after the double-click: ${JSON.stringify(sideReset)}`
    );

    const readBefore = await widthOf("--read");
    const heldRight = await page.drag("#read-grip", -64);
    const readAfter = await widthOf("--read");
    check(
      "the reading hand takes a real drag, on the other side of the same control",
      heldRight === true && readAfter.px !== null && readAfter.px < (readBefore.px || 9999) - 32,
      `dragging: ${heldRight}, --read: ${JSON.stringify(readBefore)} -> ${JSON.stringify(readAfter)}`
    );
    await page.doubleClick("#read-grip");
    const readReset = await widthOf("--read");
    check(
      "and its own double-click resets only its width",
      readReset.set === "" && (await widthOf("--side")).set === "",
      `--read: ${JSON.stringify(readReset)}, --side: ${JSON.stringify(await widthOf("--side"))}`
    );

    // ---- a picker, from the keyboard ---------------------------------------
    // The `model` screen's settings are drawn from the state frame as the run's own words, one button
    // per model. A native `<select>` used to be here and its *open list* belonged to the operating
    // system, which no protocol can reach into -- that residue is gone with the select: every word the
    // run named is a button on the page, so the whole path is drivable. What matters is that the
    // change is not merely painted: the run is told, and the model in force is the one pressed for.
    // The dialog is opened for it and shut after, because a picker behind a mask is a picker nobody
    // can press -- which is the whole trade the dialog makes.
    await openSettings("model");
    const pickerBefore = await page.js(CHOICES_OF("model", "model"));
    const beforePicker = before();
    const pickerWord = await page.js(NEXT_CHOICE("model", "model"));
    check(
      "the model picker offers the run's own words, with one of them in force",
      !!pickerBefore && pickerBefore.words.length > 1 && !!pickerWord && pickerWord.disabled === false,
      `words: ${JSON.stringify(pickerBefore)}, next: ${JSON.stringify(pickerWord)}`
    );
    await page.click("#harness-choice");
    const pickerAfter = await page
      .waitFor(
        `${VALUE_OF("model", "model")} !== ${JSON.stringify(pickerBefore && pickerBefore.on)} &&
         ${VALUE_OF("model", "model")}`,
        "the model picker to move",
        25
      )
      .catch(() => null);
    let switched = "";
    for (let i = 0; i < 25 && !switched.includes("ok model"); i += 1) {
      await sleep(200);
      switched = flint.text().slice(beforePicker);
    }
    check(
      "pressing a word moves the picker, and the run is told which model",
      !!pickerBefore && typeof pickerAfter === "string" && !pickerAfter.startsWith("threw:") &&
        switched.includes(`ok model ${pickerAfter}`),
      `page: ${JSON.stringify(pickerBefore && pickerBefore.on)} -> ${JSON.stringify(pickerAfter)}, ` +
        `terminal: ${JSON.stringify(switched.slice(-200))}`
    );
    await closeSettings();

    // ---- the composer, and whether it is reachable -------------------------
    // The reading, the composer and the hint share the pane's grid rows, so a row whose content
    // outgrows its track paints over the next one. That is the shape of the defect this file's
    // §11 found once already, in the status line, and the way to see it again is to ask what is at
    // the send button's own point rather than to look at the layout and reason about it.
    //
    // The dialog is the *other* half of the same question, and it is the reason it is an overlay
    // rather than a row in the header: opening it must not move the page behind it by a pixel, and
    // while it is open it must cover the page -- that is what a modal is. Both are measured, because
    // a dialog that pushed the reading down would be the very defect this file exists for, and a
    // dialog that did *not* cover the page would let a stray press reach a control behind it.
    const geometryAt = () =>
      page.js(
        `(() => { const box = (id) => { const r = document.getElementById(id).getBoundingClientRect();
            return [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)]; };
          const send = document.getElementById("send").getBoundingClientRect();
          const at = document.elementFromPoint(send.left + send.width / 2, send.top + send.height / 2);
          return { window: [window.innerWidth, window.innerHeight],
                   pane: box("transcript").length && (() => { const r = document.querySelector(".pane").getBoundingClientRect();
                     return [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)]; })(),
                   reading: box("transcript"), composer: box("composer"), send: box("send"),
                   dialog: document.getElementById("settings").hidden === false,
                   atSend: at ? at.tagName.toLowerCase() + (at.id ? "#" + at.id : "") : "nothing" }; })()`
      );
    const beforeDialog = await geometryAt();
    await page.click("#settings-open");
    await sleep(300);
    const withDialog = await geometryAt();
    check(
      "the dialog is an overlay: the page behind it does not move",
      withDialog.dialog === true &&
        JSON.stringify(withDialog.reading) === JSON.stringify(beforeDialog.reading) &&
        JSON.stringify(withDialog.composer) === JSON.stringify(beforeDialog.composer) &&
        JSON.stringify(withDialog.pane) === JSON.stringify(beforeDialog.pane),
      `with: ${JSON.stringify(withDialog)}, without: ${JSON.stringify(beforeDialog)}`
    );
    check(
      "and it covers the page, which is what makes it modal",
      withDialog.atSend === "div#settings-mask",
      `at the send button's point: ${JSON.stringify(withDialog.atSend)}`
    );
    await page.click("#settings-mask", [4, 4]);
    await sleep(250);
    const closedDialog = await geometryAt();
    check(
      "and with it shut the send button is its own again",
      closedDialog.dialog === false &&
        (closedDialog.atSend === "button#send" || closedDialog.atSend === "textarea#message"),
      `at the send button's point: ${JSON.stringify(closedDialog)}`
    );

    // ---- the composer, on the page rather than in a stub DOM ---------------
    // Focused through the page rather than by clicking it: what is being measured here is the send,
    // and the typing is a real `insertText` either way.
    const beforeComposer = before();
    await page.js(`document.getElementById("message").focus(); true`);
    await page.send("Input.insertText", { text: "/usage" });
    const typed = await page.js(`document.getElementById("message").value`);
    await page.click("#send");
    let gained = "";
    for (let i = 0; i < 25 && !gained; i += 1) {
      await sleep(200);
      gained = flint.text().slice(beforeComposer);
    }
    check("what is typed reaches the box", typed === "/usage", `box holds: ${JSON.stringify(typed)}`);
    check(
      "the composer sends a line the run answers",
      gained.length > 0,
      `run printed: ${JSON.stringify(gained.slice(0, 200))} | transcript: ` +
        JSON.stringify(await page.js(`(document.getElementById("doc") || {}).textContent || ""`))
    );

    // ---- a path in the transcript ------------------------------------------
    // The turn here is scripted: a write, a read, a read of a file that is not there, and a grep
    // that prints a line number. Four tool blocks, therefore four shapes a path takes, and every
    // claim below is about pressing one. What the panel must show is the *file* -- read by the run
    // over `GET /file`, from the directory the run is working in -- and not something the page
    // worked out for itself.
    const beforeTurn = before();
    await page.js(`document.getElementById("message").focus(); true`);
    await page.send("Input.insertText", { text: "leave me a note" });
    await page.click("#send");
    const drew = await page
      .waitFor(
        `document.querySelectorAll("details.tool button.path").length >= 4`,
        "the paths in the tool blocks",
        60
      )
      .catch(() => null);
    const labels = await page.js(
      `Array.from(document.querySelectorAll("details.tool button.path"))
         .map((b) => ({ text: b.textContent, title: b.title }))`
    );
    check(
      "a tool block's paths are buttons, and a grep hit carries its line",
      drew !== null &&
        Array.isArray(labels) &&
        labels.some((b) => b.text === "notes.txt") &&
        labels.some((b) => b.text === "gone.txt") &&
        labels.some((b) => /^notes\.txt:\d+$/.test(b.title)),
      `terminal: ${JSON.stringify(flint.text().slice(beforeTurn).slice(0, 120))}, ` +
        `paths: ${JSON.stringify(labels)}`
    );

    // ---- the run's background work -----------------------------------------
    // The turn above started a command nobody waits for and a `task` child. Both are in the
    // transcript as a one-line handle, which is what the model reads; what a *person* reads is this
    // panel, and none of these claims can be made from the terminal's own text -- a job that ends
    // while the run is busy is in no transcript line at all.
    //
    // Read here, at the start rather than at the end, because two of the claims are about a job that
    // is still *running*: the command below lives fifteen seconds, and the preview claims after this
    // take long enough that it would have ended by the time they were done.
    const panel = await page
      .waitFor(
        `(() => { const p = document.getElementById("jobs");
          if (!p || p.hidden) return null;
          const chips = Array.from(document.querySelectorAll("#jobs-summary .chip"))
            .map((c) => c.textContent);
          const rows = Array.from(document.querySelectorAll("#job-list .row")).map((r) => ({
            kind: (r.querySelector(".kind") || {}).textContent || "",
            status: r.className }));
          return { chips: chips, open: p.open, rows: rows }; })()`,
        "the jobs panel",
        60
      )
      .catch(() => null);
    // The chips are counts of *kinds*, so the claim is not a fixed set of words -- which jobs exist
    // at this instant is the scripted turn's business, not this test's -- but that the chips and the
    // rows under them are the same list counted two ways. A count that disagrees with what the panel
    // shows is worse than no count at all, which is the rule the old `jobs (N)` trigger was held to
    // and this keeps: the three partition chips must add up to the rows, and the subagent chip is a
    // cross-cut of the live ones rather than a fourth part.
    const jobChips = panel ? panel.chips : [];
    const numbers = (re) => jobChips.filter((c) => re.test(c)).map((c) => Number((c.match(/\d+/) || [])[0]));
    const liveChip = numbers(/ (running|stopping)$/).reduce((a, b) => a + b, 0);
    const failedChip = numbers(/ failed$/).reduce((a, b) => a + b, 0);
    const doneChip = numbers(/ done$/).reduce((a, b) => a + b, 0);
    const childChip = numbers(/ subagent/).reduce((a, b) => a + b, 0);
    const jobRows = panel ? panel.rows : [];
    const isLive = (r) => /(^|\s)(running|stopping)(\s|$)/.test(r.status);
    check(
      "a run that started work shows it in the header, counted by kind",
      !!panel &&
        jobChips.length >= 1 &&
        liveChip === jobRows.filter(isLive).length &&
        failedChip === jobRows.filter((r) => /(^|\s)(failed|killed)(\s|$)/.test(r.status)).length &&
        childChip === jobRows.filter((r) => isLive(r) && r.kind === "child").length &&
        liveChip + failedChip + doneChip === jobRows.length &&
        jobRows.length >= 1,
      `panel: ${JSON.stringify(panel)}`
    );

    const running = await page.js(
      `(() => { const row = document.querySelector("#job-list .row.running");
        if (!row) return null;
        const label = row.querySelector("code.label");
        return { kind: row.querySelector(".kind").textContent,
                 label: label.textContent, title: label.title,
                 when: row.querySelector(".when").textContent,
                 path: row.title, isButton: row.tagName === "BUTTON" }; })()`
    );
    check(
      "a running job's row names its kind and what was asked, and says how long it has been going",
      !!running &&
        running.kind === "command" &&
        running.title.includes("console.log") &&
        /^running for \d+s$/.test(running.when) &&
        running.isButton === true,
      `row: ${JSON.stringify(running)}`
    );

    // The clock, which is the one thing in this panel that is not a frame from the run: the row was
    // read once, so the claim is that the *same* row's text changed while nothing was fetched. The
    // list is opened first, and that is not a convenience -- the tick repaints rows that are on
    // screen, and a closed list has none.
    //
    // Polled rather than read after a sleep: the tick lands on the interval's own second, and a row
    // painted at 0.9s and repainted at 1.9s reads "0s" twice. Waiting for the *change* is the claim;
    // how many ticks it took is not.
    await page.click("summary#jobs-summary");
    const ticked = await page
      .waitFor(
        `(() => { const row = document.querySelector("#job-list .row.running");
          const when = row && row.querySelector(".when");
          if (!when || when.textContent === ${JSON.stringify(running && running.when)}) return null;
          return { when: when.textContent, open: document.getElementById("jobs").open }; })()`,
        "the duration to tick",
        20
      )
      .catch(() => null);
    check(
      "the duration ticks once a second while the job runs, and the list is open to see it",
      typeof running === "object" &&
        !!ticked &&
        ticked.open === true &&
        /^running for \d+s$/.test(ticked.when),
      `was ${running && running.when}, now ${JSON.stringify(ticked)}`
    );

    // Kept outside the scratch home, which a green run deletes: what a list of running work looks
    // like in this header is a judgement no claim here makes, and a picture is how it gets made.
    const shotOfJobs = await page.send("Page.captureScreenshot", { format: "png" });
    if (shotOfJobs.result && shotOfJobs.result.data) {
      const file = path.join(os.tmpdir(), "flint-jobs.png");
      fs.writeFileSync(file, Buffer.from(shotOfJobs.result.data, "base64"));
      console.log(`        screenshot: ${file}`);
    }

    // The block a path sits in is a `summary`, and pressing a path must not also fold the block
    // open: the file would appear and the line that named it would slide away under it.
    const folded = (text, nth = 0) =>
      page.js(
        `(() => { const all = Array.from(document.querySelectorAll("details.tool button.path"))
            .filter((b) => b.textContent === ${JSON.stringify(text)});
          const b = all[${nth}]; return b ? b.closest("details").open : null; })()`
      );
    // Aimed by a predicate rather than by position, because the same path appears in more than one
    // block -- its own arguments twice, the line a grep printed once -- and the claim is about a
    // *particular* one of them.
    const aim = async (find, id) => {
      const tagged = await page.js(
        `(() => { const b = Array.from(document.querySelectorAll("details.tool button.path"))
            .find((b) => ${find});
          if (!b) return null; b.id = ${JSON.stringify(id)}; return true; })()`
      );
      if (tagged) await page.click(`#${id}`);
      return tagged === true;
    };
    const called = (text) => `b.textContent === ${JSON.stringify(text)}`;
    const theHit = `/^notes\\.txt:\\d+$/.test(b.title)`;
    // The file's bytes are read off the *code* spans rather than off the container: the panel draws a
    // number per line, so the container's own `textContent` is the numbers run together with the text,
    // and what a person copies out is the code. That split is the claim, not an inconvenience.
    const bodyOf = `Array.from(document.querySelectorAll("#preview-text .code")).map((c) => c.textContent).join("\\n")`;
    const numbersOf = `Array.from(document.querySelectorAll("#preview-text .ln")).map((n) => n.textContent)`;

    const wasFolded = await folded("notes.txt");
    const pressed = await aim(called("notes.txt"), "harness-path");
    const readIt = await page
      .waitFor(
        `${bodyOf}.includes("three")
           ? { hidden: document.getElementById("preview").hidden,
               path: document.getElementById("preview-path").textContent,
               note: document.getElementById("preview-note").textContent,
               body: ${bodyOf},
               numbers: ${numbersOf},
               url: location.pathname + (location.search.includes("token=") ? "?token=…" : "") }
           : null`,
        "the file's own bytes in the panel",
        30
      )
      .catch(() => null);
    check(
      "pressing a path shows the file the run just wrote",
      pressed && !!readIt && readIt.body === "one\ntwo\nthree" && readIt.path === "notes.txt",
      `panel: ${JSON.stringify(readIt)}`
    );
    check(
      "and the lines are numbered with the file's own numbers, in a gutter beside them",
      !!readIt && JSON.stringify(readIt.numbers) === JSON.stringify(["1", "2", "3"]),
      `numbers: ${JSON.stringify(readIt && readIt.numbers)}`
    );
    check(
      "and the page stayed where it was: the file did not navigate it away",
      !!readIt && readIt.url === "/?token=…",
      `address: ${JSON.stringify(readIt && readIt.url)}`
    );
    check(
      "the panel says how big the file is, and where a grep hit was",
      !!readIt && readIt.note === "14 bytes",
      `note: ${JSON.stringify(readIt && readIt.note)}`
    );
    check(
      "pressing a path inside a tool block does not fold the block",
      wasFolded === false && (await folded("notes.txt")) === false,
      `open before: ${wasFolded}, after: ${await folded("notes.txt")}`
    );
    // Kept outside the scratch home, which a green run deletes: what the panel looks like beside
    // the conversation is a judgement no claim here makes, and a picture is how it gets made.
    const shotOfPanel = await page.send("Page.captureScreenshot", { format: "png" });
    if (shotOfPanel.result && shotOfPanel.result.data) {
      const file = path.join(os.tmpdir(), "flint-preview.png");
      fs.writeFileSync(file, Buffer.from(shotOfPanel.result.data, "base64"));
      console.log(`        screenshot: ${file}`);
    }

    // ---- the panel's own hand ----------------------------------------------
    // The panel was the one boundary with nothing to pull: a fixed `min(720px, 45vw)` beside a
    // transcript that can be a long line of prose or a wide file, and the width a reader wants is the
    // one thing about it that cannot be guessed from here. So it gets the same hand the other two
    // boundaries have, and the claims are the same three gestures -- a real pointer drag, the arrow
    // keys, and a double-click to put it back -- made the same way, against the style rather than
    // against a frame, because a width is the page's own and the run is not told about it.
    //
    // The hand's *presence* is part of the claim: a grip drawn over a closed panel would be a control
    // onto nothing, which is why `openPreview` unhides it and `closePreview` hides it again.
    const gripState = () =>
      page.js(
        `(() => { const hand = document.getElementById("preview-grip");
           const panel = document.getElementById("preview");
           const h = hand.getBoundingClientRect(), p = panel.getBoundingClientRect();
           const raw = document.getElementById("app").style.getPropertyValue("--preview");
           return { hidden: hand.hidden, width: Math.round(h.width), gap: Math.round(p.left - h.left),
                    now: hand.getAttribute("aria-valuenow"), set: raw, px: parseFloat(raw) || null,
                    panel: Math.round(p.width) }; })()`
      );
    const gripBefore = await gripState();
    check(
      "an open panel has a hand of its own, in the layout between the reading and the file",
      !!gripBefore && gripBefore.hidden === false && gripBefore.width >= 3 &&
        gripBefore.gap >= 0 && gripBefore.gap <= 12 && gripBefore.panel > 100,
      `hand: ${JSON.stringify(gripBefore)}`
    );
    const previewHeld = await page.drag("#preview-grip", -80);
    const gripAfter = await gripState();
    check(
      "and dragging it left widens the panel, so a wide file stops wrapping",
      previewHeld === true && gripAfter.hidden === false && gripAfter.px !== null &&
        gripAfter.px > (gripBefore.px || 0) + 40 && gripAfter.panel > gripBefore.panel,
      `dragging: ${previewHeld}, --preview: ${JSON.stringify(gripBefore)} -> ${JSON.stringify(gripAfter)}`
    );
    check(
      "and the hand says how wide the panel is, for a reader who cannot see the drag",
      gripAfter.now !== null && Math.abs(Number(gripAfter.now) - gripAfter.panel) <= 2,
      `aria-valuenow: ${JSON.stringify(gripAfter.now)}, measured: ${gripAfter.panel}`
    );
    await page.js(`document.getElementById("preview-grip").focus(); true`);
    await page.key("ArrowLeft", 37);
    const gripNudged = await gripState();
    check(
      "the arrow keys move this boundary too, the same rule as the other two",
      gripNudged.px !== null && gripNudged.px > (gripAfter.px || 0) + 8 &&
        gripNudged.panel > gripAfter.panel,
      `--preview: ${JSON.stringify(gripAfter)} -> ${JSON.stringify(gripNudged)}`
    );
    await page.doubleClick("#preview-grip");
    const gripReset = await gripState();
    check(
      "and a double-click puts the panel's width back rather than leaving a number behind",
      gripReset.set === "" && gripReset.panel !== gripNudged.panel,
      `--preview after the double-click: ${JSON.stringify(gripReset)}`
    );

    // A file that changed under the reader: the reload button is the one control that re-reads it,
    // and what it must show is the new bytes rather than the ones the page already had.

    fs.writeFileSync(path.join(where.cwd, "notes.txt"), "four\nfive\n");
    await page.click("#preview-reload");
    const reread = await page
      .waitFor(
        `${bodyOf}.includes("five")
           ? { body: ${bodyOf}, numbers: ${numbersOf},
               note: document.getElementById("preview-note").textContent }
           : null`,
        "the file read again",
        30
      )
      .catch(() => null);
    check(
      "reload reads the file again rather than redrawing what it had",
      !!reread && reread.body === "four\nfive" && reread.note === "10 bytes",
      `panel: ${JSON.stringify(reread)}`
    );
    check(
      "and the numbers follow the bytes it read again, rather than the file it had",
      !!reread && JSON.stringify(reread.numbers) === JSON.stringify(["1", "2"]),
      `numbers: ${JSON.stringify(reread && reread.numbers)}`
    );

    // The line a grep printed: the same file, opened where the hit was. It sits in the block's
    // *output*, and an output inside a closed block is not on screen at all -- so the block is
    // opened first, which is what a reader does and also what proves a hidden button is not
    // quietly pressable.
    const grepClosed = await page.js(
      `(() => { const block = Array.from(document.querySelectorAll("details.tool"))
          .find((d) => (d.querySelector("summary") || {}).textContent.includes("grep"));
        if (!block) return null; block.querySelector("summary").id = "harness-grep";
        return block.open; })()`
    );
    await page.click("#harness-grep");
    const grepOpen = await page.js(`document.getElementById("harness-grep").closest("details").open`);
    check(
      "a hit inside a block's output is there to press once the block is open",
      grepClosed === false && grepOpen === true,
      `open: ${grepClosed} -> ${grepOpen}`
    );
    await aim(theHit, "harness-line");
    const atLine = await page
      .waitFor(
        `document.getElementById("preview-note").textContent.startsWith("line")
           ? { note: document.getElementById("preview-note").textContent,
               body: ${bodyOf}, numbers: ${numbersOf} }
           : null`,
        "the line the hit was on",
        30
      )
      .catch(() => null);
    check(
      "a hit's line travels with the path, and the panel opens there",
      !!atLine && /^line \d+/.test(atLine.note) && atLine.body === "four\nfive",
      `panel: ${JSON.stringify(atLine)}`
    );

    // A file that is not there: the route's sentence, in the panel, where the file would have been.
    await aim(called("gone.txt"), "harness-gone");
    const refused = await page
      .waitFor(
        `document.getElementById("preview-note").textContent.startsWith("HTTP 4")
           ? { note: document.getElementById("preview-note").textContent,
               body: document.getElementById("preview-text").textContent }
           : null`,
        "the refusal",
        30
      )
      .catch(() => null);
    check(
      "a path that is not there is refused in the route's own words, not with an empty panel",
      !!refused && /nothing at/.test(refused.body) && refused.body.includes("gone.txt"),
      `panel: ${JSON.stringify(refused)}`
    );

    // The panel's own route, pressed for real: `POST /open` hands the path to the program this
    // machine uses for it, and this is the one press in the whole harness that is deliberately made
    // against a path that does not exist -- a real one would open a viewer or a file manager on the
    // machine running this, which is a side effect a test may not have. What it proves is the half
    // that matters: the control is the panel's, it is enabled in a run that is not readonly, the
    // route is reached, and the route's own sentence is what the page shows.
    const openable = await page.js(
      `({ disabled: document.getElementById("preview-open").disabled,
          title: document.getElementById("preview-open").title })`
    );
    check(
      "the panel offers to open the path where it lives, in a run that may",
      !!openable && openable.disabled === false && /where it lives/.test(openable.title),
      `the open control: ${JSON.stringify(openable)}`
    );
    await page.click("#preview-open");
    const notOpened = await page
      .waitFor(
        `/not opened:/.test(document.getElementById("hint").textContent || "")
           ? document.getElementById("hint").textContent
           : null`,
        "the route's refusal in the hint",
        30
      )
      .catch(() => null);
    check(
      "pressing it asks the run, and a path that is not there comes back in the route's words",
      typeof notOpened === "string" &&
        notOpened.includes("not opened:") &&
        notOpened.includes("nothing at") &&
        notOpened.includes("gone.txt"),
      `hint: ${JSON.stringify(notOpened)}`
    );

    // Escape, from wherever the reader is: the panel closes and the transcript is where it was. The
    // hand goes with it -- a grip left behind over a shut panel is a control onto nothing, and it
    // would take the pointer events of whatever ends up under it.
    await page.key("Escape", 27);
    const closed = await page.js(
      `({ hidden: document.getElementById("preview").hidden,
          hand: document.getElementById("preview-grip").hidden,
          transcript: (document.getElementById("doc") || {}).textContent.length })`
    );
    check(
      "Escape closes the panel and leaves the reading alone",
      !!closed && closed.hidden === true && closed.hand === true && closed.transcript > 0,
      `after Escape: ${JSON.stringify(closed)}`
    );

    // ---- what the jobs panel does with them --------------------------------
    // Then the command ends, and the ending is the fact the whole panel exists for: a job nobody
    // waited for has its exit code nowhere else a person can look. The *command's* row is waited
    // for, not whichever row settles first -- the child ends long before it, and a claim that the
    // log below is complete has to be made about the job that wrote it.
    const settled = await page
      .waitFor(
        `(() => { const row = Array.from(document.querySelectorAll("#job-list .row")).find((r) =>
            /(completed|failed|killed)/.test(r.className) &&
            (r.querySelector(".kind") || {}).textContent === "command");
          if (!row) return null;
          const detail = row.querySelector(".detail");
          return { status: String(row.className).split(" ").pop(),
                   detail: detail ? detail.textContent : null,
                   when: row.querySelector(".when").textContent }; })()`,
        "the command to end",
        110
      )
      .catch(() => null);
    check(
      "a job that ended carries its exit code and how long it took",
      !!settled &&
        settled.status === "completed" &&
        /exit code 0/.test(String(settled.detail)) &&
        /^took \d+s/.test(settled.when),
      `row: ${JSON.stringify(settled)}`
    );

    // The child, which is the other kind: a run this one started, whose output is a conversation
    // rather than a log. Same row shape, different thing behind it.
    const child = await page.js(
      `(() => { const rows = Array.from(document.querySelectorAll("#job-list .row"));
        const row = rows.find((r) => (r.querySelector(".kind") || {}).textContent === "child");
        if (!row) return null;
        const label = row.querySelector("code.label");
        return { label: label.textContent, path: row.title, isButton: row.tagName === "BUTTON" }; })()`
    );
    check(
      "and a child is the same row, opening the conversation it had instead of a log",
      !!child && child.label === "say hi" && /children/.test(child.path) && child.isButton === true,
      `row: ${JSON.stringify(child)}`
    );

    // Pressing a row is what the panel is for, and the list closes behind it: the panel opens beside
    // the reading, and a list left open over it would be the same defect the preview's own column
    // fixed. The id is cleared before it is set, because a row that kept it would be the one every
    // later press found -- the same id twice, and `querySelector` answers with the first.
    const openList = async () => {
      if ((await page.js(`document.getElementById("jobs").open`)) !== true) {
        await page.click("summary#jobs-summary");
      }
    };
    // `needle` is what tells two rows of the same kind apart, and this turn starts two commands on
    // purpose: the one whose log is read below, and the one the stop at the end of this section ends.
    const pressRow = async (kind, needle) => {
      await openList();
      const tagged = await page.js(
        `(() => { const old = document.getElementById("harness-job");
          if (old) old.removeAttribute("id");
          const row = Array.from(document.querySelectorAll("#job-list .row"))
            .find((r) => ((r.querySelector(".kind") || {}).textContent === ${JSON.stringify(kind)}) &&
                         (!${JSON.stringify(needle || "")} ||
                          ((r.querySelector("code.label") || {}).textContent || "")
                            .includes(${JSON.stringify(needle || "")})));
          if (!row) return false; row.id = "harness-job";
          row.scrollIntoView({ block: "center" }); return true; })()`
      );
      if (tagged !== true) return false;
      await page.click("#harness-job");
      return true;
    };
    const readPanel = () =>
      page.js(
        `({ hidden: document.getElementById("preview").hidden,
            path: document.getElementById("preview-path").textContent,
            note: document.getElementById("preview-note").textContent,
            body: ${bodyOf},
            listOpen: document.getElementById("jobs").open })`
      );

    // The log of a command that is still running: the reader gets what it has printed *so far*, and
    // the second line is not there yet -- which is the honest answer, and the reason the reload
    // button exists. Read while it runs so the claim is about a live log rather than a finished one.
    await pressRow("command", "15000");
    const logShown = await page
      .waitFor(
        `${bodyOf}.includes("one")
           ? { body: ${bodyOf},
               path: document.getElementById("preview-path").textContent,
               listOpen: document.getElementById("jobs").open }
           : null`,
        "the command's log in the panel",
        30
      )
      .catch(() => null);
    check(
      "pressing a job's row opens what it has printed",
      !!logShown &&
        logShown.body.includes("one") &&
        /background-bash/.test(logShown.path) &&
        logShown.listOpen === false,
      `panel: ${JSON.stringify(logShown || (await readPanel()))}`
    );

    // Then the same row again once the command has ended: the whole of the log, both lines, through
    // the same press -- which is what "the path is where the output is" means for a finished job.
    await pressRow("command", "15000");
    const whole = await page
      .waitFor(
        `${bodyOf}.includes("two") ? { body: ${bodyOf} } : null`,
        "the rest of the log",
        30
      )
      .catch(() => null);
    check(
      "and the same row again shows the whole of what it printed",
      !!whole && whole.body === "one\ntwo",
      `panel: ${JSON.stringify(whole || (await readPanel()))}`
    );

    // The child, which is the other kind: a run this one started, whose output is a conversation
    // rather than a log. Same row, different thing behind it.
    await pressRow("child");
    const childShown = await page
      .waitFor(
        `(() => { const text = ${bodyOf};
          if (!text.includes("say hi")) return null;
          return { path: document.getElementById("preview-path").textContent,
                   lines: text.split("\\n").length,
                   head: text.slice(0, 80) }; })()`,
        "the child's conversation in the panel",
        30
      )
      .catch(() => null);
    check(
      "and pressing a child's row opens the child's own conversation",
      !!childShown && /children/.test(childShown.path) && childShown.lines >= 2,
      `panel: ${JSON.stringify(childShown || (await readPanel()))}`
    );

    // ---- stopping a job from the page --------------------------------------
    // The panel above is a reading, and that is deliberate: a misclick in a panel is a misclick, and
    // DSH's own jobs panel has no kill control either. What a person gets instead is a *command* --
    // `/jobs stop <pid>` -- and the two-press shape every destructive row already has, with the pids
    // the panel is showing as its candidates. So this is the whole path checked at once: the row
    // offers only jobs that are still running, the line that goes out names the pid of the job that
    // was still running, that job reads `killed` afterwards rather than `failed`, and the run says
    // what it did -- which is the only witness that a real process ended rather than a row repainted.
    // The stop's candidates are the pids the jobs panel is showing, and the row that sends is on the
    // *background work* screen of the dialog -- opened here the way a person would, since the dialog
    // was shut after the geometry above.
    await openSettings("background work");
    await page.waitFor(ROW("/jobs stop <pid>", "work"), "the /jobs stop row");
    await page.click("#harness-target");
    const choice = await page
      .waitFor(
        `(() => { const rows = Array.from(document.querySelectorAll("#row-list-work button.row.danger"));
           const row = rows.find((r) => /60000/.test((r.querySelector("span") || {}).textContent || ""));
           if (!row) return null;
           row.id = "harness-stop";
           row.scrollIntoView({ block: "center" });
           return row.querySelector("code").textContent; })()`,
        "the running job's pid",
        25
      )
      .catch(() => null);
    check(
      "the stop's candidates are the pids the jobs panel is showing",
      typeof choice === "string" && /^\/jobs stop \d+$/.test(choice),
      `choice: ${JSON.stringify(choice)}`
    );
    const beforeStop = before();
    if (typeof choice === "string") await page.click("#harness-stop");
    const stoppedRow = await page
      .waitFor(
        `(() => { const row = Array.from(document.querySelectorAll("#job-list .row")).find((r) =>
             /(^| )killed( |$)/.test(String(r.className)));
           if (!row) return null;
           return { status: String(row.className).split(" ").pop(),
                    detail: (row.querySelector(".detail") || {}).textContent,
                    label: (row.querySelector("code.label") || {}).textContent }; })()`,
        "the job to read as killed",
        40
      )
      .catch(() => null);
    check(
      "pressing it twice ends that job, and the panel says killed rather than failed",
      !!stoppedRow &&
        stoppedRow.status === "killed" &&
        /60000/.test(String(stoppedRow.label)) &&
        /exit code -1/.test(String(stoppedRow.detail)),
      `row: ${JSON.stringify(stoppedRow)}`
    );
    // The terminal is polled rather than read once: the row above is the frame arriving, and the
    // run's own line is written by the REPL as it handles the message, which is a moment later.
    let said = "";
    for (let tries = 0; tries < 30 && !/killed it, and it is gone/.test(said); tries += 1) {
      await sleep(100);
      said = flint.text().slice(beforeStop);
    }
    check(
      "and the run says what it did, in the terminal the page is a window on",
      /killed it, and it is gone/.test(said),
      `terminal gained: ${JSON.stringify(said)}`
    );
    // Shut again, because the claim below is about the *jobs* list's own Escape: with a modal open,
    // Escape closes the modal and nothing else (that order is checked in `scripts/web-view-test.js`).
    await closeSettings();

    // Escape, the same key as the preview's, and now with an *order* rather than "whichever is open".
    // The page puts away one thing per press, front to back: the settings dialog, then the file panel,
    // then the job list. The panel is open here -- a job's own output was just read in it -- so the
    // first press is the panel's and the list stays open, which is the change the overlay made
    // explicit (before it, one press closed both, because they were three listeners on one key). The
    // dialog's place in that order is checked above, at the mask.
    await openList();
    const listWasOpen = await page.js(
      `({ jobs: document.getElementById("jobs").open,
          panel: document.getElementById("preview").hidden === false })`
    );
    await page.key("Escape", 27);
    const afterOne = await page.js(
      `({ jobs: document.getElementById("jobs").open,
          panel: document.getElementById("preview").hidden === false })`
    );
    check(
      "one Escape puts away what is in front, and the job list is behind the panel",
      listWasOpen.jobs === true && listWasOpen.panel === true &&
        afterOne.panel === false && afterOne.jobs === true,
      `open: ${JSON.stringify(listWasOpen)} -> ${JSON.stringify(afterOne)}`
    );
    await page.key("Escape", 27);
    const listNow = await page.js(`document.getElementById("jobs").open`);
    check(
      "and the press that follows closes the job list",
      listWasOpen.jobs === true && listNow === false,
      `list open: ${listWasOpen.jobs} -> ${listNow}`
    );

    // ---- the addresses in the run's own words ------------------------------
    // The turn's last answer carries three addresses of three kinds, and what the page must do with
    // each is the claim: a link, a path that opens the panel, and words. They are read off the *same*
    // row, so a page that turned everything into a link could not pass by accident -- and the count
    // of links is asserted, which is the half that catches a `javascript:` address becoming one.
    const spoken = await page.js(
      `(() => { const row = Array.from(document.querySelectorAll(".turn.assistant"))
            .filter((t) => t.textContent.includes("all done")).pop();
          if (!row) return null;
          return {
            links: Array.from(row.querySelectorAll("a.link")).map((a) =>
              ({ text: a.textContent, href: a.href, target: a.target, rel: a.rel })),
            buttons: Array.from(row.querySelectorAll("button.path")).map((b) => b.textContent),
            text: row.textContent,
          }; })()`
    );
    check(
      "an address in the run's own words is a link to the real page, in a new tab",
      !!spoken &&
        spoken.links.length === 1 &&
        spoken.links[0].href === "https://example.com/flint" &&
        spoken.links[0].text === "https://example.com/flint" &&
        spoken.links[0].target === "_blank" &&
        /noopener/.test(String(spoken.links[0].rel)),
      `links: ${JSON.stringify(spoken && spoken.links)}`
    );
    check(
      "and an address that is not the web stays the words it is",
      !!spoken && spoken.links.length === 1 && /javascript:alert\(1\)/.test(String(spoken.text)),
      `text: ${JSON.stringify(spoken && spoken.text)}`
    );

    const prosePath = path.join(where.cwd, "notes.txt");
    const taggedProse = await page.js(
      `(() => { const row = Array.from(document.querySelectorAll(".turn.assistant"))
            .filter((t) => t.textContent.includes("all done")).pop();
          if (!row) return false;
          const b = Array.from(row.querySelectorAll("button.path"))
            .find((x) => x.textContent === ${JSON.stringify(prosePath)});
          if (!b) return false; b.id = "harness-prose-path"; return true; })()`
    );
    if (taggedProse) await page.click("#harness-prose-path");
    const readByProse = await page
      .waitFor(
        `document.getElementById("preview-path").textContent === ${JSON.stringify(prosePath)}
           ? { body: ${bodyOf},
               note: document.getElementById("preview-note").textContent }
           : null`,
        "the file the run's words named",
        30
      )
      .catch(() => null);
    check(
      "a path in the run's own words opens that file beside the conversation",
      taggedProse === true && !!readByProse && readByProse.body === "four\nfive",
      `tagged: ${taggedProse}, panel: ${JSON.stringify(readByProse)}`
    );

    // The picture half of the panel, pressed the same way and in the same prose: a real PNG this
    // harness wrote into the run's working directory before the page opened, and a note that merely
    // has a picture's name. What differs is which route answers -- `/image` hands over bytes with
    // their own type, and a refusal falls through to `/file`, which reads the note as the note.
    //
    // `naturalWidth` is the browser's own word for "this decoded", which is the claim that matters: a
    // route that served the wrong bytes, or a page that drew the blob URL wrong, would leave an image
    // element with a `src` and no pixels -- and nothing read off the page's own nodes could tell that
    // from a picture. The bytes are written by this harness rather than by the scripted turn because a
    // tool call's arguments are JSON text and a PNG is not text.
    const pressProse = async (name, id) => {
      const full = path.join(where.cwd, name);
      const tagged = await page.js(
        `(() => { const row = Array.from(document.querySelectorAll(".turn.assistant"))
              .filter((t) => t.textContent.includes("all done")).pop();
            if (!row) return false;
            const b = Array.from(row.querySelectorAll("button.path"))
              .find((x) => x.textContent === ${JSON.stringify(full)});
            if (!b) return false; b.id = ${JSON.stringify(id)}; return true; })()`
      );
      if (tagged) await page.click(`#${id}`);
      return tagged === true;
    };

    const pressedPicture = await pressProse("cell.png", "harness-picture");
    const picture = await page
      .waitFor(
        `(() => { const img = document.getElementById("preview-image");
           if (!img || !img.naturalWidth) return null;
           return { width: img.naturalWidth, height: img.naturalHeight,
                    src: img.src.slice(0, 5),
                    textHidden: document.getElementById("preview-text").hidden,
                    controlShown: !document.getElementById("preview-image-button").hidden,
                    note: document.getElementById("preview-note").textContent }; })()`,
        "the picture in the panel",
        30
      )
      .catch(() => null);
    check(
      "pressing a picture's path draws the picture, and the text pane gets out of its way",
      pressedPicture &&
        !!picture &&
        picture.width === 1 &&
        picture.height === 1 &&
        picture.controlShown &&
        picture.textHidden,
      `picture: ${JSON.stringify(picture)}`
    );
    check(
      "the picture is drawn from a blob URL the page made, not from a route carrying the token",
      !!picture && picture.src === "blob:",
      `src: ${JSON.stringify(picture && picture.src)}`
    );
    check(
      "and the note names the type the route served, not the name the file has",
      !!picture && /^image\/png · \d+ bytes$/.test(picture.note),
      `note: ${JSON.stringify(picture && picture.note)}`
    );

    // The fall-through: the picture route refuses bytes that are not a picture, and the panel asks
    // `/file` rather than failing twice -- which is why the answer a reader meets is the text route's
    // own rather than "not a picture".
    const pressedTheNote = await pressProse("not-a-picture.png", "harness-not-picture");
    const fellThrough = await page
      .waitFor(
        `document.getElementById("preview-text").textContent.includes("with a picture's name")
           ? { textHidden: document.getElementById("preview-text").hidden,
               pictureHidden: document.getElementById("preview-image-button").hidden }
           : null`,
        "the note read as text",
        30
      )
      .catch(() => null);
    check(
      "a file with a picture's name and a note's bytes reads as the note",
      pressedTheNote && !!fellThrough && fellThrough.textHidden === false && fellThrough.pictureHidden === true,
      `panel: ${JSON.stringify(fellThrough)}`
    );

    // And Escape closes it as it closes a file, picture and all: a hidden panel still holding an
    // `<img src="blob:…">` would keep those bytes alive for the life of the tab.
    await page.js(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); true`);
    const pictureClosed = await page
      .waitFor(
        `document.getElementById("preview").hidden
           ? { hidden: document.getElementById("preview-image-button").hidden,
               text: document.getElementById("preview-text").hidden }
           : null`,
        "the panel closed with a picture in it",
        20
      )
      .catch(() => null);
    check(
      "Escape closes a picture the same way it closes a file",
      !!pictureClosed && pictureClosed.hidden === true && pictureClosed.text === false,
      `panel: ${JSON.stringify(pictureClosed)}`
    );
    // ---- the rendered view of a Markdown file ------------------------------
    // The one thing here the stub DOM cannot show: that a *real* browser lays out the nodes this page
    // built -- an `h1` that is a heading, a `ul` that is a list, a `table` that is a table -- and that
    // the switch between the reading and the file's own bytes changes what is on screen. What the
    // parser produces is checked without a browser (`scripts/web-view-test.js`); what is checked here is
    // that the reading is *drawn*, and that the honest half (the source) is one press away.
    const pressedNotes = await pressProse("notes.md", "harness-notes");
    const drawnNotes = pressedNotes
      ? await page
          .waitFor(
            `(() => { const md = document.getElementById("preview-md");
               if (!md || md.hidden) return null;
               const kinds = Array.from(md.children).map((n) => n.tagName.toLowerCase());
               const heading = md.querySelector("h1");
               const nested = md.querySelectorAll("ul ul").length;
               return { kinds: kinds,
                        title: heading ? heading.textContent : "",
                        nested: nested,
                        // A tag in the file is *text*: this page never assigns markup, so an
                        // angle-bracket tag in a document is four characters on screen, not bold words.
                        // (No backticks in here: this whole expression is inside one, and an inner one
                        // closes it -- which is how this cost a browser run to find.)
                        prose: (md.querySelector("p") || {}).textContent || "",
                        cells: md.querySelectorAll("td").length,
                        code: (md.querySelector("pre code") || {}).textContent || "",
                        note: document.getElementById("preview-note").textContent,
                        button: document.getElementById("preview-render").textContent }; })()`,
            "the Markdown file to be rendered",
            25
          )
          .catch(() => null)
      : null;
    check(
      "a Markdown path from the transcript is read rather than shown as its own source",
      !!drawnNotes &&
        drawnNotes.kinds.join(",") === "h1,p,ul,blockquote,table,pre" &&
        drawnNotes.title === "The notes" &&
        drawnNotes.nested === 1 &&
        drawnNotes.cells === 2 &&
        drawnNotes.code === "const a = 1;" &&
        drawnNotes.button === "source",
      `rendered: ${JSON.stringify(drawnNotes)}`
    );
    check(
      "and a tag inside it is text, because this page never assigns markup",
      !!drawnNotes && drawnNotes.prose.includes("<b>tag</b>") && drawnNotes.prose.includes("`code` left as written"),
      `prose: ${JSON.stringify((drawnNotes || {}).prose)}`
    );
    check(
      "the note says which reading is on screen",
      !!drawnNotes && /rendered$/.test(String(drawnNotes.note).trim()) &&
        /\bbytes\b/.test(String(drawnNotes.note)),
      `note: ${JSON.stringify((drawnNotes || {}).note)}`
    );

    // The switch: one press, and the file's own bytes are what is on screen -- with the numbering a line
    // would be found by, which is the whole reason a rendered view may not be the only view.
    await page.click("#preview-render");
    const source = await page
      .waitFor(
        `(() => { const text = document.getElementById("preview-text");
           const md = document.getElementById("preview-md");
           if (!text || text.hidden) return null;
           return { first: (${bodyOf}).split("\\n")[0],
                    numbers: ${numbersOf}.slice(0, 2),
                    rendered: md ? md.hidden : null,
                    button: document.getElementById("preview-render").textContent,
                    note: document.getElementById("preview-note").textContent }; })()`,
        "the file's own bytes",
        20
      )
      .catch(() => null);
    check(
      "the switch shows the source, and says how to get the reading back",
      !!source && source.first === "# The notes" && source.rendered === true &&
        JSON.stringify(source.numbers) === JSON.stringify(["1", "2"]) &&
        source.button === "rendered" && !String(source.note).includes("rendered"),
      `source: ${JSON.stringify(source)}`
    );
    await page.click("#preview-render");
    const back = await page
      .waitFor(`document.getElementById("preview-md").hidden === false ? "rendered" : null`,
        "the reading again", 20)
      .catch(() => null);
    check("and pressing it again reads the file once more", back === "rendered", `view: ${JSON.stringify(back)}`);

    // A path with a line on it opens raw: the line is why the panel was opened at all, and "line 412" has
    // no meaning in a rendered view. The press is the transcript's own `notes.md:6`, from the grep the
    // scripted turn ran over the file this harness wrote -- which is the shape a `grep` hit and a
    // compiler error both have. The claim fails rather than vanishing when the button is not there: a
    // skipped claim that reads as a pass is the one thing a harness may not do.
    const lineButton = await page.js(
      `(() => { const buttons = Array.from(document.querySelectorAll("details.tool button.path"));
         const one = buttons.find((b) => /notes\\.md:\\d+$/.test(b.title));
         if (!one) return null;
         one.id = "harness-notes-line";
         return one.title; })()`
    );
    if (typeof lineButton === "string") await page.click("#harness-notes-line");
    const notesAtLine = typeof lineButton === "string"
      ? await page
          .waitFor(
            `(() => { const text = document.getElementById("preview-text");
               if (!text || text.hidden) return null;
               return { note: document.getElementById("preview-note").textContent,
                        rendered: document.getElementById("preview-md").hidden,
                        button: document.getElementById("preview-render").textContent }; })()`,
            "the file opened at a line",
            20
          )
          .catch(() => null)
      : null;
    check(
      "a path that names a line opens the source, not the reading",
      typeof lineButton === "string" && /^notes\.md:\d+$/.test(lineButton) && !!notesAtLine &&
        notesAtLine.rendered === true && notesAtLine.button === "rendered" &&
        /^line \d+/.test(String(notesAtLine.note)),
      `button: ${JSON.stringify(lineButton)}, at line: ${JSON.stringify(notesAtLine)}`
    );

    // The gutter, measured rather than read: a file of three hundred lines, one of them six hundred
    // characters wide. What the layout has to do -- and the reason a line is a row of its own with two
    // children rather than one `<pre>` with a column of numbers beside it -- is keep the code column at
    // one x even though the numbers differ in length, keep the numbers to its left, put a wrapped line's
    // continuation in the code column rather than under the number, and leave that line's number beside
    // its *first* visual line. Where a `grep` hit's line puts the scroll is the other half of this, and
    // it is checked in `scripts/web-view-test.js` against injected geometry: the one fixture here that is
    // taller than the panel has no line number to open it at, and a claim that read `scrollTop === 0`
    // would be a claim about nothing.
    await aim(called("long.txt"), "harness-long");
    const tallFile = await page
      .waitFor(`${bodyOf}.includes("line 300") ? true : null`, "the tall file in the panel", 30)
      .catch(() => null);
    const geometry = tallFile
      ? await page.js(
          `(() => {
             const rows = Array.from(document.querySelectorAll("#preview-text .line"));
             const box = (row, part) => row.querySelector(part).getBoundingClientRect();
             return {
               rows: rows.length,
               numbers: [rows[0], rows[299]].map((r) => r.querySelector(".ln").textContent),
               wideText: rows[1].querySelector(".code").textContent.length,
               codeX: [box(rows[0], ".code").left, box(rows[1], ".code").left, box(rows[299], ".code").left],
               numberLeft: box(rows[1], ".ln").left,
               codeLeft: box(rows[1], ".code").left,
               topGap: box(rows[1], ".ln").top - box(rows[1], ".code").top,
               heights: [box(rows[0], ".code").height, box(rows[1], ".code").height],
               selectable: getComputedStyle(rows[0].querySelector(".ln")).userSelect };
           })()`
        )
      : null;
    const spread = (xs) => Math.max(...xs) - Math.min(...xs);
    check(
      "a long file is numbered to its last line, and a number is not part of the text it numbers",
      !!geometry &&
        geometry.rows === 300 &&
        JSON.stringify(geometry.numbers) === JSON.stringify(["1", "300"]) &&
        geometry.wideText === 600 &&
        geometry.selectable === "none",
      `panel: ${JSON.stringify(geometry)}`
    );
    check(
      "the code column is one column: a wrapped line continues in it, not under the number",
      !!geometry &&
        spread(geometry.codeX) < 1 &&
        geometry.numberLeft < geometry.codeLeft &&
        Math.abs(geometry.topGap) < 1 &&
        geometry.heights[1] > geometry.heights[0] * 2,
      `geometry: ${JSON.stringify(geometry)}`
    );
    await page.js(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); true`);

    // ---- a directory, opened where it lives --------------------------------
    // Reported by the person using this build, 2026-09-23, both halves in one sentence: *`C:\Users\
    // zhangzhuo\My Documents` still cannot be recognised*, and *a press should open the directory with
    // the machine's own way of opening one, rather than previewing the files in it.*
    //
    // The *name* is held here end to end, because nothing smaller can hold it: the model's own prose
    // names a real directory whose name has a space in it, the page asks the run where that path ends
    // (`GET /resolve`), and the button that appears reads the whole name. A page that guessed -- which
    // is the page a person was looking at -- drew `...\sub`, a path that is not there.
    //
    // The *press* is held against a directory that is gone, for the reason the panel's own open control
    // already is, above: a real one would put a file manager on the screen of whoever ran this. What is
    // checked is where the press goes and what it does *not* do -- `POST /open`, the route's own
    // sentence on the hint line, and no listing drawn, which a page that read the directory into the
    // panel could not have produced. That the run would open a real directory is `open_plan` in
    // `src/web.rs`, and which route a press sends is recorded in `scripts/web-view-test.js`; no test on
    // this machine launches anything.
    //
    // Every claim here is *behavioural* -- press a button, read what the page then holds -- rather than
    // a comparison against a path this file computed. That is deliberate: a path in a tool call's
    // arguments is shown as the JSON that carried it, so on Windows its separators read doubled, and a
    // harness deriving the expected spelling would be testing its own arithmetic.
    const pressIn = async (scope, text, id) => {
      const tagged = await page.js(
        `(() => { const row = Array.from(document.querySelectorAll(${JSON.stringify(scope)}))
              .find((x) => x.textContent === ${JSON.stringify(text)});
            if (!row) return false; row.id = ${JSON.stringify(id)}; return true; })()`
      );
      if (tagged) await page.click(`#${id}`);
      return tagged === true;
    };

    // The prose's bare path, with its space: the button must read the whole name the *run* found, and
    // the words after it must still be the sentence they were.
    const spacedName = path.join(where.cwd, "sub dir");
    const spacedDrawn = await page
      .waitFor(
        `Array.from(document.querySelectorAll("#doc button.path"))
           .some((b) => b.textContent === ${JSON.stringify(spacedName)}) ? true : null`,
        "the spaced path, resolved by the run and drawn as one button",
        30
      )
      .catch(() => null);
    const drawn = await page.js(
      `Array.from(document.querySelectorAll("#doc button.path")).map((b) => b.textContent)`
    );
    check(
      "a bare path with a space in it is one button, reading the whole name the run found",
      spacedDrawn === true && !drawn.includes(path.join(where.cwd, "sub")),
      `drawn: ${JSON.stringify(drawn.filter((label) => /sub|nothere/.test(label)))}, resolved: ${spacedDrawn}`
    );
    const sentence = await page.js(`document.getElementById("doc").textContent`);
    check(
      "and the words after the name are still the sentence they were",
      typeof sentence === "string" && sentence.includes("for the directory with a space in its name"),
      `transcript: ${JSON.stringify(String(sentence).slice(-200))}`
    );

    // The run's own listing, where the *process* is what says where the name ends: it prints one entry
    // per line (`sub dir/`), so a name with a space in it is already one name there -- the other half of
    // the same report, and the half no scanner can get wrong. The press is not made -- it would open a
    // real directory -- so this is about the row, not about what it does.
    const listedName = await page.js(
      `Array.from(document.querySelectorAll("#doc details.tool button.path"))
         .filter((b) => b.textContent === "sub dir").length`
    );
    check(
      "the run's own listing draws that name as one row, the way the process printed it",
      listedName >= 1,
      `rows in tool blocks reading "sub dir": ${listedName}`
    );

    // The press, on a directory that is gone: `/open` is asked, its refusal is what the page says, and
    // nothing is listed -- the listing is not where a directory press goes any more.
    const gone = path.join(where.cwd, "nothere") + path.sep;
    const pressedGone = await pressIn("#doc button.path", gone, "harness-gone-dir");
    const goneSaid = pressedGone
      ? await page
          .waitFor(
            `/not opened:/.test(document.getElementById("hint").textContent || "")
               ? document.getElementById("hint").textContent
               : null`,
            "the open route's refusal in the hint",
            30
          )
          .catch(() => null)
      : null;
    const listingRows = await page.js(`document.querySelectorAll("#preview-text .path").length`);
    check(
      "a press on a directory asks the run to open it, in the route's own words",
      pressedGone === true &&
        typeof goneSaid === "string" &&
        goneSaid.includes("not opened:") &&
        goneSaid.includes("nothing at") &&
        goneSaid.includes("nothere"),
      `pressed: ${pressedGone}, hint: ${JSON.stringify(goneSaid)}`
    );
    check(
      "and the panel lists nothing: a directory press is not a reading",
      listingRows === 0,
      `listing rows in the panel: ${listingRows}`
    );
    // ---- the `/` menu in the composer --------------------------------------
    // The one surface that needs a real browser *and* a real run: a menu drawn from the frame the
    // process sent, opened by a keystroke, driven by the arrow keys, and committed by Enter. The stub
    // DOM can check what a row *does* (and does); what it cannot check is that a real `keydown`
    // reaches the handler at all, that the rows are the real binary's commands, or that Enter on a
    // report goes out on the route that keeps it off the terminal.
    const menuState = () =>
      page.js(`(() => { const box = document.getElementById("menu");
        const rows = box ? Array.from(box.querySelectorAll("button.row")) : [];
        return { open: !!box && !box.hidden, count: rows.length,
                 first: rows.length ? rows[0].textContent : "",
                 marked: rows.filter((r) => r.getAttribute("aria-selected") === "true")
                             .map((r) => r.textContent),
                 text: document.getElementById("message").value,
                 settings: !document.getElementById("settings").hidden }; })()`);

    const typedMenu = async (text) => {
      // The pointer is parked in the corner first, and that is not tidiness: a row under the pointer is
      // marked on `mouseenter`, so a menu that opens *where the last press left the mouse* can be marked
      // on a row nobody's keyboard chose. The claims below are about the keyboard, and the transcript's
      // last row is one press away from where the menu appears. Measured: without this, the bare `/`
      // came back marked on the fifth row while the first row on screen was unmarked, and the press
      // before it was a path button in the last turn.
      await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 2, y: 2 });
      await page.js(`document.getElementById("message").focus(); true`);
      await page.send("Input.insertText", { text });
      await sleep(150);
      return menuState();
    };

    // A bare slash opens it on the frame's own order. The names are the real binary's: `/config` and
    // `/help` are the first two commands this build offers, which is a claim about *this* run rather
    // than about the page -- exactly what a live check is for.
    const bare = await typedMenu("/");
    check(
      "a slash in the composer offers the run's own commands",
      bare.open && bare.count > 0 && /^\/\w/.test(bare.first),
      `menu: ${JSON.stringify({ open: bare.open, count: bare.count, first: bare.first })}`
    );
    check(
      "and the dialog is not what opened",
      bare.settings === false,
      `settings: ${JSON.stringify(bare.settings)}`
    );
    check(
      "the keyboard starts on the first row",
      bare.marked.length === 1 && bare.marked[0] === bare.first,
      `marked: ${JSON.stringify(bare.marked)} first: ${JSON.stringify(bare.first)} of ${bare.count}`
    );

    // Typing after the slash filters, and a space closes it: the line is being written, and a menu
    // over it would be covering the words. Both halves matter -- a menu that never closed would sit
    // on top of a sentence.
    const filtered = await typedMenu("usage");
    check(
      "letters after the slash narrow the list",
      filtered.open && filtered.count >= 1 && filtered.first.startsWith("/usage"),
      `menu: ${JSON.stringify({ count: filtered.count, first: filtered.first })}`
    );
    const spaced = await typedMenu(" now");
    check(
      "a space closes it, because the line is a line again",
      spaced.open === false && spaced.text === "/usage now",
      `menu: ${JSON.stringify({ open: spaced.open, text: spaced.text })}`
    );

    // The arrow keys move the mark, one row at a time, in a real browser: the stub can call
    // `menuStep`, and only this can show that `preventDefault` on a real `keydown` keeps the caret
    // from moving to the start of the line while the list is driven.
    await page.js(`document.getElementById("message").value = ""; true`);
    await typedMenu("/");
    const beforeArrow = await menuState();
    await page.key(`ArrowDown`, 40);
    const afterArrow = await menuState();
    check(
      "ArrowDown moves the mark down one row, and the caret is not what moved",
      afterArrow.marked.length === 1 &&
        afterArrow.marked[0] !== beforeArrow.marked[0] &&
        afterArrow.text === "/",
      `before: ${JSON.stringify(beforeArrow.marked)}, after: ${JSON.stringify(afterArrow.marked)}, ` +
        `box: ${JSON.stringify(afterArrow.text)}`
    );
    await page.key(`ArrowUp`, 38);
    const backUp = await menuState();
    check(
      "ArrowUp brings it back",
      backUp.marked[0] === beforeArrow.marked[0],
      `marked: ${JSON.stringify(backUp.marked)}`
    );

    // Escape closes the menu and leaves the line alone: a list put away is not a line cleared.
    await page.key(`Escape`, 27);
    const escaped = await menuState();
    check(
      "Escape puts the menu away and keeps what was typed",
      escaped.open === false && escaped.text === "/",
      `menu: ${JSON.stringify({ open: escaped.open, text: escaped.text })}`
    );

    // Enter on a report row: the answer is a *reading*, drawn in the dialog's commands section, and
    // nothing of it is typed or sent into the transcript. This is the claim the page's whole reason
    // for the `/report` route rests on, made against a real run: the terminal must gain nothing.
    const beforeSlashReport = before();
    await page.js(`document.getElementById("message").value = ""; true`);
    await typedMenu("/help");
    const helpRow = await menuState();
    // The *marked* row is what Enter takes, and the mark is where the keyboard is -- so the claim is
    // that a filter's best answer is first and selected, not that `/help` is the only row whose help
    // text mentions helping.
    check(
      "the filter puts the report row it matches first, and marks it",
      helpRow.first.startsWith("/help") && helpRow.marked.length === 1 &&
        helpRow.marked[0] === helpRow.first,
      `menu: ${JSON.stringify({ count: helpRow.count, first: helpRow.first, marked: helpRow.marked })}`
    );
    await page.key(`Enter`, 13);
    let slashReading = null;
    for (let i = 0; i < 25 && !slashReading; i += 1) {
      await sleep(200);
      // The reading is drawn where the rows were, on the screen the frame filed the row on: a back
      // button and the answer, which is the shape the dialog uses for a report (a name from the
      // question and the text from the process). `/help` is a palette row with no screen of its own
      // -- it is read rather than kept -- so its reading lands on the first screen, which is the
      // documented fallback `screenOf` implements.
      slashReading = await page.js(`(() => { const list = document.getElementById("row-list-model");
        if (!list || document.getElementById("settings").hidden) return null;
        const back = list.querySelector("button.back");
        const text = (list.querySelector(".reading") || {}).textContent || "";
        return back && text ? { answer: text.slice(0, 300) } : null; })()`);
    }
    check(
      "Enter on a report row reads it in the dialog rather than sending it",
      slashReading !== null && flint.text().slice(beforeSlashReport).length === 0,
      `reading: ${JSON.stringify(slashReading)}, ` +
        `terminal gained: ${JSON.stringify(flint.text().slice(beforeSlashReport).slice(0, 120))}`
    );
    check(
      "and the line it came from was not sent either",
      (await page.js(`document.getElementById("message").value`)) === "",
      `box: ${JSON.stringify(await page.js(`document.getElementById("message").value`))}`
    );
    await page.key(`Escape`, 27);
    const closedAgain = await menuState();
    check(
      "one Escape closes the dialog the reading was shown in",
      closedAgain.settings === false,
      `settings: ${JSON.stringify(closedAgain.settings)}`
    );

    // A form row: the dialog, at the row, and the line untouched. This is the claim that a credential
    // cannot be typed into the transcript by a keystroke in a menu -- the value is not in the box, so
    // it cannot be sent, and the masked field is the one that takes it. `/name` is filed on *this
    // conversation*, which is where the dialog opens and where the mark has to be.
    await page.js(`document.getElementById("message").value = ""; true`);
    await typedMenu("/name");
    await page.key(`Enter`, 13);
    let pointed = null;
    for (let i = 0; i < 20 && !pointed; i += 1) {
      await sleep(200);
      pointed = await page.js(`(() => { const list = document.getElementById("row-list-conversation");
        if (!list || document.getElementById("settings").hidden) return null;
        const marks = Array.from(list.querySelectorAll(".pointed"));
        return marks.length ? { marks: marks.length, text: marks[0].textContent } : null; })()`);
    }
    check(
      "Enter on a form row opens the dialog at that row and writes nothing into the line",
      pointed !== null && pointed.marks === 1 &&
        (await page.js(`document.getElementById("message").value`)) === "",
      `pointed: ${JSON.stringify(pointed)}, ` +
        `box: ${JSON.stringify(await page.js(`document.getElementById("message").value`))}`
    );
    await page.key(`Escape`, 27);

    // An action row completes the line and sends nothing: a keystroke in a menu must not decide
    // anything, and the person's own Enter on the *line* is what sends it. `/reload` is the action
    // here because its answer is one line the terminal prints either way.
    const beforeSlashAction = before();
    await page.js(`document.getElementById("message").value = ""; true`);
    await typedMenu("/reload");
    await page.key(`Enter`, 13);
    await sleep(300);
    const completed = await menuState();
    check(
      "Enter on an action row completes the line and sends nothing",
      completed.text === "/reload" && completed.open === false &&
        flint.text().slice(beforeSlashAction).length === 0,
      `box: ${JSON.stringify(completed.text)}, open: ${JSON.stringify(completed.open)}, ` +
        `terminal gained: ${JSON.stringify(flint.text().slice(beforeSlashAction).slice(0, 120))}`
    );
    await page.click("#send");
    let slashReloaded = "";
    for (let i = 0; i < 25 && !slashReloaded; i += 1) {
      await sleep(200);
      slashReloaded = flint.text().slice(beforeSlashAction);
    }
    check(
      "and the person's own Enter on that line is what sends it",
      slashReloaded.length > 0 && (await page.js(`document.getElementById("message").value`)) === "",
      `terminal gained: ${JSON.stringify(slashReloaded.slice(0, 160))}`
    );

    // ---- cutting a branch from an answer ------------------------------------
    // The one command whose argument is a question *in this conversation*, and the one thing the page
    // cannot count for itself: `/fork n` counts questions in the run's history, while the page has drawn
    // `chat` lines out of a file. A `/compact` makes those two lists different lengths -- it drops a
    // prefix of the questions and never the newest one -- so the page pairs its own turns with the
    // run's questions *from the bottom*, where the two agree, and believes a pairing only when the
    // turn's own first line is the question the frame named (`branchPoints`, and `labels` in the frame).
    //
    // The claim is made against the run's stdout and then against the transcript the page is drawing,
    // not against the button: a button that cut somewhere else would look exactly the same.
    //
    // One question is not a choice, and this is the state the run has been in since the first prompt:
    // the frame offers `/fork 1`, which cuts in front of everything the run holds, so there is nothing
    // for a button to sit on. That is the negative half, and it is what makes the button below mean
    // something rather than being drawn for every turn.
    const barsNow = () =>
      page.js(
        `Array.from(document.querySelectorAll("#doc .turn")).map((turn) => ({
           kind: turn.className,
           branch: (turn.querySelector("button.branch") || {}).title || "",
         }))`
      );
    check(
      "an answer with nothing after it that could be cut keeps no button",
      (await barsNow()).every((turn) => !turn.branch),
      `turns: ${JSON.stringify(await barsNow())}`
    );

    const beforeSecond = before();
    await page.js(`document.getElementById("message").focus(); true`);
    await page.send("Input.insertText", { text: "and the tests" });
    await page.click("#send");
    const drewBranch = await page
      .waitFor(
        `document.querySelectorAll("#doc .turn.assistant button.branch").length === 1`,
        "the branch button under the answer a cut would keep",
        60
      )
      .catch(() => null);
    const bars = await barsNow();
    const marked = bars.filter((turn) => turn.branch);
    check(
      "the button lands under the answer the cut keeps, once there is a question to cut in front of",
      drewBranch !== null && marked.length === 1 &&
        marked[0].kind.includes("assistant") && marked[0].branch.includes("and the tests"),
      `turns: ${JSON.stringify(bars)} | terminal gained: ` +
        JSON.stringify(flint.text().slice(beforeSecond).slice(0, 200))
    );

    const beforeFork = before();
    await page.click("#doc .turn.assistant button.branch");
    let forked = "";
    for (let i = 0; i < 30 && !forked.includes("forked:"); i += 1) {
      await sleep(200);
      forked = flint.text().slice(beforeFork);
    }
    check(
      "pressing it cuts the conversation where the button said it would",
      forked.includes("forked:") && forked.includes("cut at question 2 of 2: and the tests"),
      `run printed: ${JSON.stringify(forked.slice(0, 400))}`
    );

    // And the page is now reading the branch: the second question is gone from the transcript, which
    // is what a `reset` frame plus `GET /session` is for. Read as the *transcript* rather than as the
    // run's stdout, because the half a person sees is the page's.
    const branched = await page
      .waitFor(
        `!document.getElementById("doc").textContent.includes("and the tests")`,
        "the page drawing the branch",
        60
      )
      .catch(() => null);
    check(
      "and the page is left reading the branch rather than the conversation it was cut from",
      branched !== null,
      `transcript: ` +
        JSON.stringify((await page.js(`document.getElementById("doc").textContent`)).slice(0, 300))
    );

    // ---- the conversation you are *in* can be deleted, and the right side goes back to its page ---
    //
    // Everything above deletes a conversation that is not the run's own, which is the half the
    // terminal always allowed. This is the other half, and it is the half it used to refuse: the row
    // under the `...` of the conversation being *written*. The command closes that conversation first
    // -- a fresh one is started, then the file is moved or removed -- so the sidebar loses the row and
    // the pane has nothing to draw. That last state is the one asked for by name on 2026-09-23, with
    // DSH as the reference: a run holding a conversation nobody has said anything in is still a page,
    // not an empty pane. It is the last phase here on purpose, because it clears the transcript the
    // claims above read.
    //
    // The list is scrolled to its bottom by the claim above (that is the state the upward flip is
    // about), so the row is brought back into view by the press itself -- `click` does that -- rather
    // than left to wherever the list happened to be.
    //
    // In a block of its own: `held` and `closed` are names the phases above already use for other
    // things, and one `main` is one scope.
    {
    // The level a person sets in the settings screen belongs to the run, and this phase's move -- a
    // fresh conversation started, then the old file thrown away -- is where it used to be dropped: the
    // new agent was built at the provider's starting word rather than at the level the run was asking
    // for, so the screen somebody had just used to set `high` read `off` again. Reported directly on
    // 2026-09-23, in this sequence and through this door, which is why the claim is made here rather
    // than beside the settings screen's own claims above.
    await openSettings("model");
    const levelBefore = await page.js(CHOICES_OF("model", "thinking"));
    const pressLevel = await page.js(
      `(() => { const row = Array.from(document.querySelectorAll("#fields-model .setting"))
          .find((s) => { const n = s.querySelector(".setting-name");
            return n && n.textContent.trim() === "thinking"; });
        if (!row) return null;
        const b = Array.from(row.querySelectorAll(".choices button"))
          .find((x) => x.textContent.trim() === "high");
        if (!b) return null; b.id = "harness-level"; return b.textContent; })()`
    );
    if (pressLevel === "high") await page.click("#harness-level");
    const levelSet = await page
      .waitFor(`${VALUE_OF("model", "thinking")} === "high" && "high"`, "the level to be set", 25)
      .catch(() => null);
    check(
      "the reasoning level is a word a person presses, on the screen the run's own state draws it on",
      !!levelBefore && Array.isArray(levelBefore.words) && levelBefore.words.includes("high") &&
        pressLevel === "high" && levelSet === "high",
      `thinking: ${JSON.stringify(levelBefore)} -> ${JSON.stringify(levelSet)}`
    );
    await closeSettings();

    const held = await page.js(
      `(() => { const li = document.querySelector("#sessions li.current");
        if (!li) return null;
        const label = li.querySelector("span.label");
        return { id: li.title, label: label ? label.textContent : "" }; })()`
    );
    check(
      "the run is holding a conversation, so deleting the one you are in has a subject",
      !!held && !!held.id && !!held.label,
      `current row: ${JSON.stringify(held)}`
    );

    const beforeOwn = before();
    let ownRow = null;
    if (held) {
      await page.js(
        `(() => { const li = document.querySelector("#sessions li.current");
          if (!li) return null; const b = li.querySelector("button.more");
          if (!b) return null; b.id = "harness-own"; return true; })()`
      );
      await page.click("#harness-own");
      // Found by the run's own sentence for it, the way the fixture's row above is: the menu draws
      // what a row *does*, so there is no command in the page to look for.
      ownRow = await page.js(
        `(() => { const li = document.querySelector("#sessions li.current");
          const m = li && li.querySelector(".session-menu"); if (!m) return null;
          const b = Array.from(m.querySelectorAll("button.row"))
            .find((b) => String(b.textContent).trim() === "delete one");
          if (!b) return null; b.id = "harness-own-delete"; return b.textContent; })()`
      );
      check(
        "the conversation the run is writing offers the same rows in its own menu",
        ownRow === "delete one",
        `row: ${JSON.stringify(ownRow)}`
      );
      check(
        "and opening that menu sent nothing",
        flint.text().slice(beforeOwn).trim() === "",
        `terminal gained: ${JSON.stringify(flint.text().slice(beforeOwn))}`
      );
      await page.click("#harness-own-delete");
    }
    let closed = "";
    for (let i = 0; i < 40 && !(closed.includes("deleted ") && closed.includes("started a new session")); i += 1) {
      await sleep(200);
      closed = flint.text().slice(beforeOwn);
    }
    check(
      "the second press deletes the conversation the run is in and starts a fresh one",
      !!held && closed.includes("deleted ") && closed.includes("started a new session"),
      `run printed: ${JSON.stringify(closed.slice(0, 300))}`
    );

    // ...and what the run was asking with is not one of the things the move threw away. Read back off
    // the same screen it was set on, because that screen is where the loss was reported: a fresh
    // conversation is a new file and a new agent, and the level belongs to the run rather than to
    // either. The provider starts at `off` in `Provider::new`, so this is the claim that says the run's
    // decision is handed over rather than lost with the file.
    await openSettings("model");
    const levelAfter = await page.js(CHOICES_OF("model", "thinking"));
    await closeSettings();
    check(
      "and the level the run was at survives the conversation it was set in being thrown away",
      !!levelAfter && levelAfter.on === "high",
      `thinking after the move: ${JSON.stringify(levelAfter)}`
    );

    const swept = await page
      .waitFor(
        `!document.querySelector("#sessions li.current") &&
         !Array.from(document.querySelectorAll("#sessions li"))
            .some((li) => li.title === ${JSON.stringify(held && held.id)})`,
        "the sidebar dropping the conversation the run was in",
        30
      )
      .catch(() => null);
    check(
      "the sidebar stops showing it, and no row is marked as the run's own",
      swept === true,
      `rows: ${JSON.stringify(await page.js(
        `Array.from(document.querySelectorAll("#sessions li")).map((li) => [li.title, li.className])`
      ))}`
    );

    const resting = await page
      .waitFor(
        `document.querySelectorAll("#doc .turn").length === 0 &&
         /Nothing loaded yet/.test(document.getElementById("doc").textContent)`,
        "the pane going back to the page the viewer opens with",
        30
      )
      .catch(() => null);
    check(
      "and the right side is back on the page the viewer opens with rather than an empty pane",
      resting === true,
      `transcript: ` +
        JSON.stringify((await page.js(`document.getElementById("doc").textContent`)).slice(0, 200))
    );

    // And it is a page and not a picture of one: the run is in a conversation nothing has been said
    // in, so the next thing said has to start one -- which is what "the default state" means for a
    // run that keeps a file per conversation, and what makes the row's absence honest rather than a
    // sidebar with a hole in it.
    await page.js(`document.getElementById("message").focus(); true`);
    await page.send("Input.insertText", { text: "after the delete" });
    await page.click("#send");
    const restarted = await page
      .waitFor(
        `document.getElementById("doc").textContent.includes("after the delete") &&
         Array.from(document.querySelectorAll("#sessions li"))
           .some((li) => li.textContent.includes("after the delete"))`,
        "the page asking its first question in the conversation it started",
        60
      )
      .catch(() => null);
    check(
      "and the page is one you can talk in: the next question starts a conversation of its own",
      restarted === true,
      `transcript: ` +
        JSON.stringify((await page.js(`document.getElementById("doc").textContent`)).slice(0, 200))
    );
    }

  } finally {
    page.close();
    chrome.child.kill();
    flint.child.stdin.end();
    model.server.close();
    await sleep(400);
    flint.child.kill();
  }

  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} claims held`);
  if (failed.length) {
    console.log("failed:");
    for (const one of failed) console.log(`  - ${one.claim}`);
    // Left behind on purpose, and only then: the home holds the run's own output, the config a form
    // wrote and the screenshot, which is what a failing claim has to be diagnosed from -- and it is
    // the same rule the Rust tests follow. A green run leaves nothing, because a harness that keeps
    // its scratch every time fills somebody's temp directory.
    console.log(`kept for inspection: ${where.home}`);
  } else {
    fs.rmSync(where.home, { recursive: true, force: true });
  }
  process.exit(failed.length ? 1 : 0);
}

main().catch((error) => {
  // The stack as well as the message: a failure in a helper (`pressProse`, `click`, `waitFor`) is one
  // this file's own line numbers answer, and a message alone has twice sent a reader looking in the
  // wrong block.
  console.log(`harness failed: ${error.message}`);
  if (error.stack) console.log(error.stack);
  process.exit(3);
});
