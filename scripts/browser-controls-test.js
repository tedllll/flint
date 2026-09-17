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
  const click = async (selector) => {
    const aimed = await js(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)});
        if (!el) return null; el.scrollIntoView({ block: "center" });
        const r = el.getBoundingClientRect();
        const x = r.left + r.width / 2, y = r.top + r.height / 2;
        const top = document.elementFromPoint(x, y);
        const name = (n) => n ? n.tagName.toLowerCase() + (n.id ? "#" + n.id : "") +
          (n.className ? "." + String(n.className).split(" ").join(".") : "") : "nothing";
        return { x, y, w: r.width, h: r.height, top: name(top),
                 ours: top === el || (!!top && el.contains(top)), onTopOf: name(document.elementFromPoint(x, y)) };
      })()`
    );
    if (!aimed || typeof aimed !== "object" || typeof aimed.x !== "number") {
      throw new Error(`no element to click: ${selector}`);
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

/// A row of the command panel by the line it would send, which is what the frame put in it.
const ROW = (line) =>
  `(() => { const rows = Array.from(document.querySelectorAll("#command-list button.row code"));
     const found = rows.find((c) => c.textContent.trim() === ${JSON.stringify(line)});
     if (!found) return null;
     found.closest("button").id = "harness-target"; return true; })()`;

async function main() {
  const binary = browserPath();
  if (!binary) {
    console.log("no browser found -- name one with --browser <path>");
    process.exit(2);
  }
  if (!fs.existsSync(flintBinary())) {
    console.log(`build first: cargo build (${flintBinary()} is not there)`);
    process.exit(2);
  }
  // The scripted model: a file is written, read back, and missed, and a grep prints a line number
  // -- the four shapes a path appears in a transcript -- then the run's two kinds of job are
  // started, and prose ends the turn.
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
  const slow = `node -e "console.log('one'); setTimeout(() => console.log('two'), 15000)"`;
  const model = await stubModel([
    toolCall("write", { path: "notes.txt", content: "one\ntwo\nthree\n" }),
    toolCall("read", { path: "notes.txt" }),
    toolCall("read", { path: "gone.txt" }),
    toolCall("grep", { pattern: "two", path: "." }),
    toolCall("bash", { command: slow, background: true }),
    toolCall("task", { prompt: "say hi" }),
    prose("the child's answer"),
    prose("all done"),
  ]);
  const where = scratch(`http://127.0.0.1:${model.port}/v1`);
  const flint = await startFlint(where);
  console.log(`flint: ${flint.url}\n  home: ${where.home}\n  browser: ${binary}\n  model: port ${model.port}\n`);

  const chrome = await startBrowser(binary, flint.url, where.home);
  const page = await attach(chrome.page);
  const before = () => flint.text().length;

  try {
    // The page is only interactive once the state frame has arrived: the controls, the switches and
    // the command list are all drawn from it, and a click before it lands hits nothing.
    await page.waitFor(`document.querySelectorAll("#toggles select").length > 0`, "the switches");
    await page.waitFor(`document.querySelectorAll("#command-list .row").length > 0`, "the command list");

    // ---- the switches ------------------------------------------------------
    const names = await page.js(
      `Array.from(document.querySelectorAll("#toggles .toggle-name")).map((n) => n.textContent)`
    );
    check(
      "the switches are drawn from the run's own state",
      Array.isArray(names) && names.includes("readonly") && names.includes("verbose"),
      `names: ${JSON.stringify(names)}`
    );
    const started = before();
    const was = await page.js(`document.querySelector("#toggles select").value`);
    // Keyboard rather than a synthetic `change`: a native select opened by a pointer is an OS
    // widget the protocol cannot reach into, and an ArrowDown on the focused select is the same
    // path a person's key takes -- a real input event, not a script setting a value.
    await page.js(`document.querySelector("#toggles select").focus(); true`);
    await page.key("ArrowDown", 40);
    const moved = await page
      .waitFor(
        `document.querySelector("#toggles select").value !== ${JSON.stringify(was)} &&
         document.querySelector("#toggles select").value`,
        "the switch to move",
        25
      )
      .catch(() => null);
    // Both halves matter and they are different halves: the switch moved on the page, and the run
    // was told. A page that only moved its own control would leave the run on the old setting, and
    // the run's own output is the only witness to which of those happened.
    check(
      "a switch moves the control and the run together",
      moved !== null && flint.text().length > started,
      `page: ${JSON.stringify(was)} -> ${JSON.stringify(moved)}, run printed: ` +
        JSON.stringify(flint.text().slice(started).slice(0, 200))
    );

    // ---- the command panel -------------------------------------------------
    await page.click("#commands summary");
    const opened = await page.js(`document.getElementById("commands").open`);
    const listed = await page.js(`document.getElementById("command-list").textContent || ""`);
    check("the panel opens with a real click", opened === true, `open: ${JSON.stringify(opened)}`);
    // Named rows from four of the five classes, because "the list is there" is not the claim: the
    // claim is that it is the *run's* list, drawn from the frame -- and a panel with rows in it
    // that had lost a class would still look like a list. Read as the panel's text rather than as
    // `code` elements, because a form row's own line is its submit button, not a label.
    const wanted = ["/config", "/provider key", "/delete <n|id>", "/name", "/sessions"];
    check(
      "the panel lists the run's commands, across the classes",
      wanted.every((line) => String(listed).includes(line)),
      `missing: ${JSON.stringify(wanted.filter((line) => !String(listed).includes(line)))}`
    );
    const actions = await page.js(
      `Array.from(document.querySelectorAll("#actions button")).map((b) => b.textContent.trim())`
    );
    check(
      "the run's actions are buttons in the header",
      Array.isArray(actions) && actions.includes("/reload") && actions.includes("/new"),
      `actions: ${JSON.stringify(actions)}`
    );

    // A report is read *here*: its answer belongs in the panel, and the terminal did not ask.
    const beforeReport = before();
    await page.waitFor(ROW("/config"), "the /config row");
    await page.click("#harness-target");
    const reading = await page.waitFor(
      `(document.querySelector("#command-list .reading") || {}).textContent || ""`,
      "the report's answer"
    );
    await sleep(300);
    check(
      "a report is answered in the panel",
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
    await page.click("#command-list .back"); // back to the list
    await sleep(300);

    // ---- an action button --------------------------------------------------
    // An action is a button in the header, not a row in the panel: it takes no argument, and the
    // panel is a reference. The two are the same `send` string either way, which is the point.
    const beforeAction = before();
    await page.js(`(() => { const b = Array.from(document.querySelectorAll("#actions button"))
      .find((b) => b.textContent.trim() === "/reload");
      if (!b) return null; b.id = "harness-action"; return b.textContent.trim(); })()`);
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

    // ---- the masked credential field --------------------------------------
    const secret = "sk-not-a-real-key-0000";
    const form = `(() => { const f = Array.from(document.querySelectorAll("#command-list form.field"))
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
    const sessionsBefore = fs.readdirSync(path.join(where.home, "sessions")).sort();
    const beforeDanger = before();
    await page.waitFor(ROW("/delete <n|id>"), "the /delete row");
    await page.click("#harness-target");
    const candidates = await page.waitFor(
      `document.querySelectorAll("#command-list button.row.danger").length`,
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
    await page.click("#command-list .back");
    await sleep(300);
    const sessionsAfter = fs.readdirSync(path.join(where.home, "sessions")).sort();
    check(
      "and backing out deletes nothing",
      JSON.stringify(sessionsBefore) === JSON.stringify(sessionsAfter),
      `${JSON.stringify(sessionsBefore)} -> ${JSON.stringify(sessionsAfter)}`
    );

    // ---- the sidebar's own menu --------------------------------------------
    // `⋯` at the end of a conversation's row opens that conversation's actions, drawn from the same
    // frame rows the panel uses with this row's number appended. §11 called this "reasoned rather
    // than seen", so what is checked here is the two-press shape on the row itself: the press that
    // opens the menu sends nothing, the row in it says the whole line before it sends it, and the
    // line that goes out belongs to *that* conversation.
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
        const m = li && li.querySelector(".menu"); if (!m) return null;
        const n = (li.querySelector("span.n") || {}).textContent || "";
        return { number: n,
                 rows: Array.from(m.querySelectorAll("button.row")).map((b) => (b.querySelector("code") || {}).textContent),
                 form: Array.from(m.querySelectorAll("form.field button.send")).map((b) => b.textContent) }; })()`
    );
    check(
      "the menu opens with that conversation's actions and its own number",
      !!menu && Array.isArray(menu.rows) && menu.rows.length > 0 &&
        menu.rows.every((line) => String(line).endsWith(" " + menu.number)),
      `menu: ${JSON.stringify(menu)}`
    );
    check(
      "and opening it sent nothing",
      flint.text().slice(beforeMenu).trim() === "",
      `terminal gained: ${JSON.stringify(flint.text().slice(beforeMenu))}`
    );
    // The rename belongs to the row the run is writing and to no other, which is why it is looked
    // for *here* rather than assumed: a menu that offered it on every row would rename whatever
    // happened to be open.
    check(
      "the conversation being written offers a name field in the same menu",
      !!menu && Array.isArray(menu.form) && menu.form.includes("/name"),
      `menu forms: ${JSON.stringify(menu && menu.form)}`
    );
    const nameInput = `(() => { const li = document.querySelector("#sessions li.current") ||
        document.querySelector("#sessions li");
      const f = li.querySelector(".menu form.field");
      if (!f) return null; f.querySelector("input").id = "harness-name";
      f.querySelector("button.send").id = "harness-name-send"; return true; })()`;
    if (await page.js(nameInput)) {
      const beforeName = before();
      await page.js(`document.getElementById("harness-name").focus(); true`);
      await page.send("Input.insertText", { text: "named from the sidebar" });
      await page.click("#harness-name-send");
      let named = "";
      for (let i = 0; i < 25 && !named.includes("named:"); i += 1) {
        await sleep(200);
        named = flint.text().slice(beforeName);
      }
      check(
        "a name typed into that field reaches the run",
        named.includes("named: named from the sidebar"),
        `terminal gained: ${JSON.stringify(named.slice(0, 200))}`
      );
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
      const victimRow = await page.js(
        `(() => { const li = Array.from(document.querySelectorAll("#sessions li"))
            .find((r) => r.title === ${JSON.stringify(victim)});
          const m = li && li.querySelector(".menu"); if (!m) return null;
          const b = Array.from(m.querySelectorAll("button.row"))
            .find((b) => String((b.querySelector("code") || {}).textContent).startsWith("/delete"));
          if (!b) return null; b.id = "harness-delete-row";
          return (b.querySelector("code") || {}).textContent; })()`
      );
      check(
        "a fixture conversation's menu names the line it would send",
        typeof victimRow === "string" && victimRow.startsWith("/delete "),
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
    // The header's two `<select>`s are drawn from the state frame. A native select's *open list*
    // belongs to the operating system and no protocol can reach into it -- that is the residue §11
    // keeps -- but the keyboard is the path a person takes through it, and it is drivable: focus,
    // ArrowDown, and the value changes as a real input event. What matters is that the change is
    // not merely painted: the run is told, and the model in force is the one pressed for.
    const pickerBefore = await page.js(
      `(() => { const s = document.getElementById("pick-model");
        return { value: s.value, options: Array.from(s.options).map((o) => o.value) }; })()`
    );
    const beforePicker = before();
    await page.js(`document.getElementById("pick-model").focus(); true`);
    await page.key("ArrowDown", 40);
    const pickerAfter = await page
      .waitFor(
        `document.getElementById("pick-model").value !== ${JSON.stringify(pickerBefore.value)} &&
         document.getElementById("pick-model").value`,
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
      "a picker moves from the keyboard and the run is told which model",
      pickerAfter !== null && switched.includes(`ok model ${pickerAfter}`),
      `page: ${JSON.stringify(pickerBefore.value)} -> ${JSON.stringify(pickerAfter)}, ` +
        `terminal: ${JSON.stringify(switched.slice(-200))}`
    );

    // ---- the composer, and whether it is reachable -------------------------
    // The reading, the composer and the hint share the pane's grid rows, so a row whose content
    // outgrows its track paints over the next one. That is the shape of the defect this file's
    // §11 found once already, in the status line, and the way to see it again is to ask what is at
    // the send button's own point rather than to look at the layout and reason about it.
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
                   panelOpen: document.getElementById("commands").open,
                   atSend: at ? at.tagName.toLowerCase() + (at.id ? "#" + at.id : "") : "nothing" }; })()`
      );
    const openPanel = await geometryAt();
    await page.click("#commands summary"); // close it: the panel is a reference, not the conversation
    await sleep(300);
    const closedPanel = await geometryAt();
    check(
      "the send button is reachable with the panel open",
      openPanel.atSend === "button#send" || openPanel.atSend === "textarea#message",
      `at the send button's point: ${JSON.stringify(openPanel)}`
    );
    check(
      "and with it closed",
      closedPanel.atSend === "button#send" || closedPanel.atSend === "textarea#message",
      `at the send button's point: ${JSON.stringify(closedPanel)}`
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
          return { summary: document.getElementById("jobs-summary").textContent,
                   open: p.open,
                   rows: document.querySelectorAll("#job-list .row").length }; })()`,
        "the jobs panel",
        60
      )
      .catch(() => null);
    // The number on the trigger and the rows under it are the same list, so they are one claim: a
    // count that disagrees with what the panel shows is worse than no count at all.
    const counted = panel ? Number((panel.summary.match(/\((\d+)\)/) || [])[1]) : NaN;
    check(
      "a run that started work shows it in the header, with the count of it",
      !!panel && /^jobs \(\d+\)/.test(panel.summary) && counted === panel.rows && panel.rows >= 1,
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

    const wasFolded = await folded("notes.txt");
    const pressed = await aim(called("notes.txt"), "harness-path");
    const readIt = await page
      .waitFor(
        `document.getElementById("preview-text").textContent.includes("three")
           ? { hidden: document.getElementById("preview").hidden,
               path: document.getElementById("preview-path").textContent,
               note: document.getElementById("preview-note").textContent,
               body: document.getElementById("preview-text").textContent,
               url: location.pathname + (location.search.includes("token=") ? "?token=…" : "") }
           : null`,
        "the file's own bytes in the panel",
        30
      )
      .catch(() => null);
    check(
      "pressing a path shows the file the run just wrote",
      pressed && !!readIt && readIt.body === "one\ntwo\nthree\n" && readIt.path === "notes.txt",
      `panel: ${JSON.stringify(readIt)}`
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

    // A file that changed under the reader: the reload button is the one control that re-reads it,
    // and what it must show is the new bytes rather than the ones the page already had.
    fs.writeFileSync(path.join(where.cwd, "notes.txt"), "four\nfive\n");
    await page.click("#preview-reload");
    const reread = await page
      .waitFor(
        `document.getElementById("preview-text").textContent.includes("five")
           ? { body: document.getElementById("preview-text").textContent,
               note: document.getElementById("preview-note").textContent }
           : null`,
        "the file read again",
        30
      )
      .catch(() => null);
    check(
      "reload reads the file again rather than redrawing what it had",
      !!reread && reread.body === "four\nfive\n" && reread.note === "10 bytes",
      `panel: ${JSON.stringify(reread)}`
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
               body: document.getElementById("preview-text").textContent }
           : null`,
        "the line the hit was on",
        30
      )
      .catch(() => null);
    check(
      "a hit's line travels with the path, and the panel opens there",
      !!atLine && /^line \d+/.test(atLine.note) && atLine.body === "four\nfive\n",
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

    // Escape, from wherever the reader is: the panel closes and the transcript is where it was.
    await page.key("Escape", 27);
    const closed = await page.js(
      `({ hidden: document.getElementById("preview").hidden,
          transcript: (document.getElementById("doc") || {}).textContent.length })`
    );
    check(
      "Escape closes the panel and leaves the reading alone",
      !!closed && closed.hidden === true && closed.transcript > 0,
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
    const pressRow = async (kind) => {
      await openList();
      const tagged = await page.js(
        `(() => { const old = document.getElementById("harness-job");
          if (old) old.removeAttribute("id");
          const row = Array.from(document.querySelectorAll("#job-list .row"))
            .find((r) => (r.querySelector(".kind") || {}).textContent === ${JSON.stringify(kind)});
          if (!row) return false; row.id = "harness-job"; return true; })()`
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
            body: document.getElementById("preview-text").textContent,
            listOpen: document.getElementById("jobs").open })`
      );

    // The log of a command that is still running: the reader gets what it has printed *so far*, and
    // the second line is not there yet -- which is the honest answer, and the reason the reload
    // button exists. Read while it runs so the claim is about a live log rather than a finished one.
    await pressRow("command");
    const logShown = await page
      .waitFor(
        `document.getElementById("preview-text").textContent.includes("one")
           ? { body: document.getElementById("preview-text").textContent,
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
    await pressRow("command");
    const whole = await page
      .waitFor(
        `document.getElementById("preview-text").textContent.includes("two")
           ? { body: document.getElementById("preview-text").textContent }
           : null`,
        "the rest of the log",
        30
      )
      .catch(() => null);
    check(
      "and the same row again shows the whole of what it printed",
      !!whole && whole.body === "one\ntwo\n",
      `panel: ${JSON.stringify(whole || (await readPanel()))}`
    );

    // The child, which is the other kind: a run this one started, whose output is a conversation
    // rather than a log. Same row, different thing behind it.
    await pressRow("child");
    const childShown = await page
      .waitFor(
        `(() => { const text = document.getElementById("preview-text").textContent;
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

    // Escape, the same key as the preview's: the list is in the header and the panel is beside the
    // reading, and one key puts away whichever is open.
    await openList();
    const listWasOpen = await page.js(`document.getElementById("jobs").open`);
    await page.key("Escape", 27);
    const listNow = await page.js(`document.getElementById("jobs").open`);
    check(
      "Escape closes the job list too",
      listWasOpen === true && listNow === false,
      `open: ${listWasOpen} -> ${listNow}`
    );
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
  console.log(`harness failed: ${error.message}`);
  process.exit(3);
});
