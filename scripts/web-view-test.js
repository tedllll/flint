#!/usr/bin/env node
// Run the embedded viewer's renderer over real session and stream lines.
//
// `cargo test` cannot reach into a page: it would need a DOM, and a DOM in the test process
// would be both a dependency and a fiction. So this runs the *embedded* script under Node,
// with a stub DOM small enough to read, and asserts what it does with the two vocabularies
// the page is fed.
//
// It is not a DOM test. Nothing here checks layout, colour, or whether a `details` element
// opens -- that is what `docs/web-mode.md` §9 step 7 means by "measure it in a browser". What
// it checks is the part that has answers: which lines become which blocks, what an unmatched
// tool result does, whether an unknown event is skipped, and where the two vocabularies
// disagree about the shape of the same name.
//
//   node scripts/web-view-test.js

"use strict";

const fs = require("fs");
const path = require("path");
const vm = require("vm");

const root = path.join(__dirname, "..");
const html = fs.readFileSync(path.join(root, "web", "view.html"), "utf8");

let failures = 0;
function check(name, fn) {
  try {
    fn();
    console.log("  PASS  " + name);
  } catch (e) {
    failures++;
    console.log("  FAIL  " + name + "\n        " + e.message);
  }
}
function eq(got, want, what) {
  const a = JSON.stringify(got);
  const b = JSON.stringify(want);
  if (a !== b) throw new Error(`${what}\n        got:  ${a}\n        want: ${b}`);
}
function ok(cond, what) {
  if (!cond) throw new Error(what);
}

// ---------------------------------------------------------------------------
// Run the page's own script, with the smallest DOM that lets it finish.
// ---------------------------------------------------------------------------

/// The smallest node that lets the page's own `paint` finish.
///
/// It grew when `paint` stopped rebuilding the whole transcript: an incremental paint needs the
/// relationships between nodes -- who is whose parent, and how to swap one out -- so the stub
/// has to have them. Keeping a real parent pointer is the whole of it; there is still no layout
/// and no styling.
///
/// It grew again when a *form* was drawn rather than a button: a row that takes several answers is
/// only drawn if the page's submit handler joins them, and the interesting half of that -- that a
/// required field left empty sends nothing at all -- happens *inside* the handler. So listeners are
/// kept now instead of being thrown away, and a check delivers one itself with `fire`. Nothing
/// delivers them on its own: the page still runs only because a check asks it to, which keeps every
/// failure in a check's own line rather than in whatever ran first.
function fakeNode() {
  const node = {
    children: [],
    textContent: "",
    className: "",
    hidden: false,
    files: [],
    parentNode: null,
    open: false,
    scrollTop: 0,
    scrollHeight: 0,
    clientHeight: 0,
    handlers: {},
    focused: false,
    // A real element always has one, and the composer's own `sizeComposer` writes a height into it on
    // every keystroke: measured as `Cannot set properties of undefined (setting 'height')` the moment a
    // check typed into the line through the page's own function rather than by assigning `value`.
    style: {},
    // `focus` is a real browser fact this harness needed the moment the page grew a modal: what makes
    // a dialog a dialog rather than a panel is where the keyboard goes when it opens and where it
    // goes back to when it closes, and neither is visible in the page's own nodes.
    focus() { node.focused = true; },
    classList: { add() {}, remove() {} },
    get firstChild() { return node.children[0] || null; },
    appendChild(child) { child.parentNode = node; node.children.push(child); return child; },
    removeChild(child) {
      const at = node.children.indexOf(child);
      if (at >= 0) node.children.splice(at, 1);
      child.parentNode = null;
      return child;
    },
    replaceChild(fresh, old) {
      const at = node.children.indexOf(old);
      if (at >= 0) node.children[at] = fresh; else node.children.push(fresh);
      fresh.parentNode = node;
      old.parentNode = null;
      return old;
    },
    remove() { if (node.parentNode) node.parentNode.removeChild(node); },
    addEventListener(type, fn) {
      (node.handlers[type] = node.handlers[type] || []).push(fn);
    },
    // The attribute pair, because the page clears a picture by removing `src` rather than by
    // assigning an empty one -- an empty `src` is a request for the page itself. Added when the
    // preview learned to draw a picture and the sandbox threw `removeAttribute is not a function`:
    // the page was right and the stub was incomplete, which is the failure a stub has to be read for.
    setAttribute(name, value) { node[name] = value; },
    getAttribute(name) { return node[name] === undefined ? null : node[name]; },
    removeAttribute(name) { delete node[name]; },
    click() {},
  };
  return node;
}

function loadViewer() {
  const start = html.indexOf("<script>");
  const end = html.lastIndexOf("</script>");
  if (start < 0 || end < 0) throw new Error("web/view.html has no inline script");

  // The ids the document marks `hidden`, read off the real markup.
  //
  // A stub that starts every node visible is a stub that cannot tell a dialog which ships closed from
  // one which ships open -- and the difference is the whole point of the settings overlay, the jobs
  // panel, the preview and the sidebar. This is not a parser: it is the one attribute the page's
  // behaviour depends on at load, matched in the tag that carries the id. A page whose `hidden`
  // attribute is wrong is then wrong here too, which is the honest failure.
  const startsHidden = new Set();
  for (const tag of html.match(/<[a-z]+[^>]*>/g) || []) {
    const id = tag.match(/\sid="([^"]+)"/);
    if (id && /\shidden[\s>]/.test(tag)) startsHidden.add(id[1]);
  }

  const nodes = new Map();
  // What the page sent, in order. The page's whole job is a POST to one of two routes, and which
  // route -- a report read quietly in the panel, or a line typed into the transcript -- is the
  // difference between reading and doing. Nothing outside the browser could see that until this
  // existed: the e2e tests see the *process* answer, which is the same answer either way.
  const sent = [];
  const sandbox = {
    module: { exports: {} },
    console,
    FileReader: function FileReader() {},
    // Answers everything with an empty 200, so a check is about what the page decided to send
    // rather than about a server. `text`/`json` are there because the page calls them on the way
    // through, and a stub that threw where the real thing answers would fail checks for a reason
    // that has nothing to do with what they assert.
    fetch: async (url, init) => {
      sent.push({
        route: String(url),
        body: init && init.body ? String(init.body) : "",
      });
      // `headers` because the file route answers with the cut and the size of what it served, and
      // the page reads them: a stub without one would fail a check for a reason of its own making.
      return {
        ok: true,
        status: 200,
        headers: { get: () => null },
        text: async () => "",
        json: async () => ({}),
      };
    },
    document: {
      getElementById(id) {
        if (!nodes.has(id)) {
          const node = fakeNode();
          if (startsHidden.has(id)) node.hidden = true;
          nodes.set(id, node);
        }
        return nodes.get(id);
      },
      // The tag is kept because a check sometimes has to ask what kind of node was drawn -- a report
      // row is a button and a selector row is not -- and there is no other way to tell two stubs
      // apart.
      createElement: (tag) => {
        const node = fakeNode();
        node.tag = tag;
        return node;
      },
      createTextNode: (t) => ({ text: t }),
      addEventListener() {},
    },
    // A real page always has these two, and the page uses them for one thing: remembering where this
    // tab was reading, so that a reload can say what arrived while it was closed. They were added when
    // a `window.addEventListener("pagehide", ...)` in the page made this harness throw
    // `ReferenceError: window is not defined` -- the page was right and the sandbox was incomplete,
    // which is the failure a stub has to be read for rather than worked around.
    window: {
      handlers: {},
      addEventListener(type, handler) {
        (this.handlers[type] = this.handlers[type] || []).push(handler);
      },
    },
    // Where the page reads its token from, which is the header every request it makes carries. It is
    // `file:` on purpose: the token is here so that a request can be *made* -- a check has to be able
    // to see which route a press asks -- while `servedByFlint` still says this page was dropped rather
    // than served, which is the level the whole harness is written at. Without a `location` at all,
    // every fetch path threw inside its own `try` and reported a failure about nothing.
    location: { protocol: "file:", search: "?token=stub-token" },
    sessionStorage: (() => {
      const store = new Map();
      return {
        getItem: (key) => (store.has(key) ? store.get(key) : null),
        setItem: (key, value) => store.set(key, String(value)),
        removeItem: (key) => store.delete(key),
      };
    })(),
    // The two browser globals the picture half of the preview needs. `createObjectURL` is how the
    // page hands `GET /image`'s bytes to an `<img>` without ever putting its token in a URL, and the
    // *pair* is the reason this is stubbed rather than guarded away: a check has to be able to see
    // that the URL was let go of, because a page that never revokes one holds every picture it has
    // ever opened in memory for as long as the tab lives.
    URL: {
      made: [],
      revoked: [],
      createObjectURL(blob) {
        const url = "blob:http://127.0.0.1:7777/" + (this.made.length + 1);
        this.made.push({ url, size: blob && blob.size });
        return url;
      },
      revokeObjectURL(url) {
        this.revoked.push(url);
      },
    },
    Blob: function Blob(parts, options) {
      this.size = (parts || []).reduce((n, part) => n + (part && part.length ? part.length : 0), 0);
      this.type = (options && options.type) || "";
    },
  };
  vm.createContext(sandbox);
  vm.runInContext(html.slice(start + "<script>".length, end), sandbox, { filename: "view.html" });

  const api = sandbox.module.exports;
  if (!api || typeof api.applyText !== "function") {
    throw new Error("the viewer did not export its renderer for testing");
  }
  // The stub's nodes on demand, so a test can give them the one thing a stub cannot have by
  // itself: a scroll geometry. The page's *view* is otherwise out of reach here -- this harness
  // was built for the model -- and that gap is where a real regression lived: the reading column
  // and the scroll container became two elements and the follow-the-tail code went on reading the
  // one that does not scroll.
  api.__node = (id) => sandbox.document.getElementById(id);
  // What the page sent, and the way to deliver one listener. `fire` is not the browser: it calls
  // the handlers the page registered, synchronously, and hands them the event object a check makes
  // up. That is enough for the one thing worth reaching into -- a submit handler that refuses to
  // send -- without a DOM to lie about everything else.
  api.sent = sent;
  // The page's own store, so a check can clear it between cases and prove what a load does to it: the
  // pair belongs to one tab in the browser, and a stub that could not be reset would make the second
  // check depend on the first.
  api.storage = sandbox.sessionStorage;
  // The blob-URL stub, so a check can see that a picture's URL was *revoked* -- the one fact about
  // the picture half that leaves no trace in the page's own nodes.
  api.urls = sandbox.URL;
  api.fire = (node, type, event) => {
    const handlers = (node && node.handlers && node.handlers[type]) || [];
    for (const handler of handlers) handler(event || { preventDefault() {} });
  };
  return api;
}

const viewer = loadViewer();
const doc = (text) => viewer.applyText(viewer.newDoc(), text);
const kinds = (d) => d.blocks.map((b) => b.kind);

// The dialog's shape, in one place. It has moved twice under these checks -- the settings rows became
// a text block with a control beside them, and a screen's command rows moved inside a group per class
// -- and a check that spells the walk out has to be edited every time, which is how an assertion
// quietly ends up examining nothing. So: a setting row is a name, what changing it means, and the
// control; a screen's rows are the groups in `#row-list-<screen>`, and a group holds its rows in a
// `group-rows` box under a heading it may not have.
const settingsOf = (page, screen) => page.__node("fields-" + screen).children;
const controlOf = (row) => row.children[1];
const namesOf = (page, screen) =>
  settingsOf(page, screen).map((row) => row.children[0].children[0].textContent);
const helpsOf = (page, screen) =>
  settingsOf(page, screen).map((row) => row.children[0].children[1].textContent);
const wordsOf = (control) => control.children.map((choice) => choice.textContent);
const chosenOf = (control) => {
  const on = control.children.find((choice) => choice.getAttribute("aria-pressed") === "true");
  return on ? on.textContent : null;
};
// Everything on a screen's row list. While a reading is open this *is* the reading -- the way back,
// the line that was asked for, and the answer -- because an answer is not a class of row and replaces
// the groups rather than sitting inside one. Otherwise its children are the groups.
const listOf = (page, screen) => page.__node("row-list-" + screen).children;
const groupsOf = (page, screen) => listOf(page, screen);
const rowsBox = (group) =>
  group.children.find((child) => child.className === "group-rows") || { children: [] };
const rowsOf = (page, screen) => groupsOf(page, screen).flatMap((group) => rowsBox(group).children);
const headingsOf = (page, screen) =>
  groupsOf(page, screen).map((group) => {
    const title = group.children.find((child) => child.className === "group-title");
    return title ? String(title.textContent) : "";
  });
const labelsOf = (page, screen) => rowsOf(page, screen).map((row) => String(row.children[0].textContent));

console.log("the viewer over a session file");

// The exact shape `docs/session-format.md` documents, including a name appended later (the
// last one wins), an event from a newer build, a tool call and its result, and the two
// message shapes a tool call produces.
const SESSION = [
  `{"type":"meta","v":2,"id":"1789290356-957","created":"epoch:1789290356","cwd":"C:\\\\work\\\\dsh","provider":"deepseek","model":"deepseek-chat"}`,
  `{"type":"chat","message":{"role":"system","content":"You are flint."}}`,
  `{"type":"chat","message":{"role":"user","content":"why does dsh fail to start"}}`,
  `{"type":"chat","message":{"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"bash","arguments":"{\\"command\\":\\"dsh --version\\"}"}}]}}`,
  `{"type":"chat","message":{"role":"tool","tool_call_id":"call_1","content":"1.2.3"}}`,
  `{"type":"chat","message":{"role":"assistant","content":"It starts fine.","reasoning":"the version is current"}}`,
  `{"type":"title","name":"first name"}`,
  `{"type":"usage","usage":{"prompt_tokens":1204,"completion_tokens":88}}`,
  `{"type":"something.from.the.future","whatever":true}`,
  `{"type":"title","name":"dsh start failure"}`,
].join("\n");

const s = doc(SESSION);

check("reads meta, and the last title is the one in force", () => {
  eq(s.meta.model, "deepseek-chat", "model");
  eq(s.meta.cwd, "C:\\work\\dsh", "cwd, backslashes intact");
  eq(s.title, "dsh start failure", "title");
});

check("usage is read from the wrapper a session file puts it in", () => {
  eq(s.usage.prompt_tokens, 1204, "prompt_tokens");
  eq(s.usage.completion_tokens, 88, "completion_tokens");
});

check("an event from a future build is skipped in silence", () => {
  ok(!JSON.stringify(s.blocks).includes("future"), "an unknown type reached the transcript");
  eq(kinds(s), ["system", "user", "tool", "assistant"], "blocks");
});

check("a tool result is matched back to the call that asked for it", () => {
  const tool = s.blocks.find((b) => b.kind === "tool");
  eq(tool.id, "call_1", "id");
  eq(tool.name, "bash", "name");
  eq(tool.output, "1.2.3", "output");
  eq(tool.done, true, "done");
  eq(tool.ok, true, "ok");
  ok(tool.args.includes("dsh --version"), "the arguments must survive: " + tool.args);
});

check("an assistant message keeps its content and its reasoning apart", () => {
  const answer = s.blocks.find((b) => b.kind === "assistant");
  eq(answer.text, "It starts fine.", "content");
  eq(answer.reasoning, "the version is current", "reasoning");
});

check("the system prompt is a block of its own, not lost and not in the way", () => {
  const sys = s.blocks.find((b) => b.kind === "system");
  eq(sys.text, "You are flint.", "the instructions");
});

console.log("the viewer over the event stream");

// The order the events really arrive in: a round's text, then its tool calls, then the next
// round's text. `message.completed` carries the whole turn, which is not the same thing.
const STREAM = [
  `{"cwd":"/work","model":"deepseek-chat","session":"/home/me/.flint/sessions/1.jsonl","type":"session.started"}`,
  `{"prompt":"is it installed","type":"turn.started"}`,
  `{"text":"Let me look.","type":"message.delta"}`,
  `{"id":"call_1","name":"bash","type":"tool.started"}`,
  `{"arguments":"{\\"command\\":\\"dsh --version\\"}","id":"call_1","name":"bash","type":"tool.args"}`,
  `{"id":"call_1","name":"bash","ok":true,"output":"1.2.3","type":"tool.completed"}`,
  `{"text":"It is 1.2.3.","type":"message.delta"}`,
  `{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"type":"usage"}`,
  // Carries the whole *turn*, which is every round concatenated -- the sink accumulates
  // across a turn. That is not a detail of this fixture: it is what the real stream sends,
  // and it is why the assertion below is worth making.
  `{"text":"Let me look.It is 1.2.3.","type":"message.completed"}`,
  `{"prompt_tokens":10,"completion_tokens":5,"type":"turn.completed"}`,
].join("\n");

const t = doc(STREAM);

check("a turn reads in the order it happened: narration, tool, narration", () => {
  eq(kinds(t), ["user", "assistant", "tool", "assistant"], "blocks");
  eq(t.blocks[1].text, "Let me look.", "the first round's narration");
  eq(t.blocks[3].text, "It is 1.2.3.", "the second round's narration");
  eq(t.blocks[2].output, "1.2.3", "the tool result");
});

check("message.completed does not drag the whole turn into the last block", () => {
  // It carries every round concatenated, so writing it over the last block would move the
  // first round's narration below the tool call that came after it.
  eq(t.blocks[3].text, "It is 1.2.3.", "the last block");
  eq(t.blocks[1].text, "Let me look.", "the first block");
});

check("usage is read from the flat shape the stream uses", () => {
  eq(t.usage.prompt_tokens, 10, "prompt_tokens");
});

check("the tool call arrives in pieces and is one block at the end", () => {
  eq(t.blocks.filter((b) => b.kind === "tool").length, 1, "one tool block");
  eq(t.blocks[2].done, true, "done");
});

console.log("damage, and the status line");

check("a line that is not JSON is shown as damage rather than thrown away", () => {
  const d = doc('{"type":"chat","message":{"role":"user","content":"hi"}}\nnot json at all');
  eq(kinds(d), ["user", "damage"], "blocks");
  ok(d.blocks[1].text.includes("not json"), "the damage must quote the line: " + d.blocks[1].text);
});

check("a known type with a broken shape is damage, not silence", () => {
  const d = doc('{"type":"chat"}');
  eq(kinds(d), ["damage"], "blocks");
});

check("a status event becomes the status line, and a finished turn clears it", () => {
  const running = doc(`{"text":"waiting for the model 12s","type":"status"}`);
  eq(running.status, "waiting for the model 12s", "status while waiting");
  const done = doc(`{"text":"waiting for the model","type":"status"}\n{"prompt_tokens":1,"completion_tokens":1,"type":"turn.completed"}`);
  eq(done.status, "", "status after the turn");
});

// The stop button is the only way to interrupt a turn from here, and it is offered for exactly
// as long as there is something to interrupt. The two halves are one test because they are one
// fact: the turn's own boundaries are what the button reads, so a turn that ends -- by finishing
// or by being stopped, which is the case the button exists for -- takes the button with it.
check("the stop button is offered while a turn runs, and gone when it ends", () => {
  const d = viewer.newDoc();
  viewer.applyLine(d, `{"type":"turn.started","prompt":"write me an article"}`);
  eq(d.running, true, "the turn is in flight");
  viewer.paint(d);
  eq(viewer.__node("stop").hidden, false, "the stop button is offered while it runs");

  // An interrupted turn never reaches `message.completed`. `turn.completed` is what the server
  // sends for it (and for every other turn end), and without it the page was left holding an
  // answer that still looked like it was arriving.
  viewer.applyLine(d, `{"text":"half an article","type":"message.delta"}`);
  viewer.applyLine(d, `{"prompt_tokens":1,"completion_tokens":1,"type":"turn.completed"}`);
  eq(d.running, false, "the turn is over");
  viewer.paint(d);
  eq(viewer.__node("stop").hidden, true, "and the button goes with it");
  eq(
    d.blocks.filter((b) => b.kind === "assistant").map((b) => b.open),
    [false],
    "no half answer is left looking like it is still arriving"
  );
});

check("a tool result with no matching call is kept, not dropped", () => {
  // A hand-trimmed session file does this, and the format explicitly allows trimming.
  const d = doc(`{"type":"chat","message":{"role":"tool","tool_call_id":"gone","content":"orphan"}}`);
  eq(kinds(d), ["tool"], "blocks");
  eq(d.blocks[0].output, "orphan", "output");
});

console.log("what a command answered");

// §8's other half. A command's answer is not a turn and not a session event, so nothing else on
// this page could carry it -- and a command typed into the composer answered into a terminal the
// reader could not see. What is pinned here is that it lands in the transcript with the line that
// asked for it, because the answer alone ("verbose = on") does not say what was asked, and the
// question alone is what the composer already shows.
check("a command's answer becomes a block, with the line that asked for it", () => {
  const d = doc(
    `{"input":"/config","text":"config: /home/me/.flint/config.toml\\n  verbose          = on","type":"command"}`
  );
  eq(kinds(d), ["command"], "blocks");
  eq(d.blocks[0].input, "/config", "the command");
  ok(
    d.blocks[0].text.includes("verbose          = on"),
    "the answer must keep its lines: " + d.blocks[0].text
  );
});

check("a command's answer is drawn as a transcript block with its own label", () => {
  // A page of its own, because painting is incremental: `painted` remembers the nodes it built for
  // the *previous* document and leaves a block alone when its revision has not changed -- which a
  // one-block fixture shares with the one-block fixture before it. Sharing the viewer across
  // checks is what the rest of this file does with documents it never paints.
  const page = loadViewer();
  const d = page.applyText(page.newDoc(), `{"input":"/tools","text":"bash\\nread\\nedit","type":"command"}`);
  page.paint(d);
  const rows = page.__node("doc").children.filter((n) => n.className === "turn command");
  eq(rows.length, 1, "one command row");
  eq(rows[0].children[0].textContent, "/tools", "the label is the command that was run");
  // The answer is the body's *pieces* rather than one `textContent`, because prose goes through the
  // address splitter: a sentence with a URL or a path in it is several nodes, and a stub cannot
  // join them the way a real DOM does. Joined back here, it is the same answer as before.
  const pieces = [];
  const collect = (node) => {
    if (!node) return;
    if (!node.tag) {
      pieces.push(node.text);
      return;
    }
    if (node.textContent) pieces.push(node.textContent);
    for (const child of node.children || []) collect(child);
  };
  collect(rows[0].children[1]);
  eq(pieces.join(""), "bash\nread\nedit", "the answer is the body");
});

check("a command frame with no text in it paints nothing rather than `undefined`", () => {
  // The server sends nothing at all when a command has nothing to say, so this is the shape of a
  // frame from a build that did -- and the page's job with a field it is not given is to draw
  // nothing, not to draw the word `undefined` into the transcript.
  const page = loadViewer();
  const d = page.applyText(page.newDoc(), `{"input":"/new","type":"command"}`);
  page.paint(d);
  const rows = page.__node("doc").children.filter((n) => n.className === "turn command");
  eq(rows.length, 1, "one command row");
  eq(rows[0].children[1].textContent, "", "an absent answer is empty, not the string undefined");
});

console.log("the settings the frame describes");

// The settings are drawn from the `state` frame, which is the page's only read channel: it may not
// read `config.toml` itself, because a second reader of the same state can disagree with the process
// the moment `--readonly` or `/readonly` is involved. What is pinned here is that the page adds no
// vocabulary of its own: the key, the value in force, the words a choice takes, how to draw it, what
// changing it means and which screen it goes on all come from the frame -- and a screen is a slice
// of that one list rather than a list the page kept.
check("a state frame becomes a control per setting, on the screen the frame names", () => {
  const page = loadViewer();
  const d = page.newDoc();
  eq(d.state, null, "a page with no frame has no state");
  page.showState(d);
  eq(page.__node("fields-model").children.length, 0, "a document with no state draws no settings");
  eq(page.__node("fields-model").hidden, true, "and the empty block is not drawn at all");
  eq(page.__node("fields-run").children.length, 0, "on any screen");

  page.applyState(d, JSON.stringify({
    type: "state",
    settings: [
      { group: "model", key: "provider", kind: "select", value: "stub", choices: ["stub", "other"],
        send: "/provider", help: "switch endpoint" },
      { group: "model", key: "model", kind: "select", value: "stub-other",
        choices: ["stub-model", "stub-other"], send: "/model", help: "switch model" },
      { group: "run", key: "verbose", kind: "select", value: "full", choices: ["off", "on", "full"],
        send: "/verbose", help: "how much to narrate" },
    ],
  }));

  eq(namesOf(page, "model"), ["provider", "model"],
     "the settings the frame filed on that screen, in the frame's order");
  eq(helpsOf(page, "model"), ["switch endpoint", "switch model"],
     "each row says what changing it means");
  // The control is the words themselves: one press per value the frame named, with the one in force
  // filled in. A `<select>` was the same fact behind a control the OS drew, which is why no setting
  // is drawn with one any more.
  const providers = controlOf(settingsOf(page, "model")[0]);
  eq(providers.className, "choices", "a choice is the frame's own words as presses");
  eq(wordsOf(providers), ["stub", "other"], "the words the frame named");
  eq(chosenOf(providers), "stub", "marked as the one in force");
  eq(providers.children[0].disabled, false, "two providers is a choice");
  eq(providers.children[0].tag, "button", "and each word is pressable");
  eq(providers.title, "switch endpoint", "and the control explains itself on hover");
  eq(namesOf(page, "run"), ["verbose"],
     "a setting goes on the screen the frame named, not on the first one");
  eq(chosenOf(controlOf(settingsOf(page, "run")[0])), "full", "showing what is in force");

  // A frame that has gone away takes the settings with it: a control showing a value nothing is
  // reporting any more is the one thing this page must not leave behind.
  page.applyState(d, JSON.stringify({ type: "state" }));
  eq(page.__node("fields-model").children.length, 0, "a frame with no settings draws none");
  eq(page.__node("fields-model").hidden, true, "and hides the block rather than leaving it empty");
  eq(page.__node("fields-run").children.length, 0, "and takes the old ones away");
});

// The other kind of setting: one whose value is a line rather than a choice. Two facts are pinned
// here, and the second is what the frame carries `kind` for -- a number box refuses `many`, and the
// page may not guess which box a key wants from the key's name.
check("a setting whose value is a line is a form, and its kind is the frame's word", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state",
    settings: [
      { group: "limits", key: "max_steps", kind: "number", value: "100",
        send: "/config set max_steps", help: "the runaway guard" },
      { group: "limits", key: "shell", kind: "text", value: "cmd",
        send: "/config set shell", help: "the shell the bash tool runs" },
    ],
  }));
  const rows = settingsOf(page, "limits");
  eq(rows.map((r) => controlOf(r).tag), ["form", "form"], "a line is typed into a form");
  const steps = controlOf(rows[0]);
  const shell = controlOf(rows[1]);
  eq(steps.children.map((n) => n.tag), ["input", "button"], "an input and a press");
  eq(steps.children[0].type, "number", "the frame said number");
  eq(shell.children[0].type, "text", "and the frame said text");
  eq(steps.children[0].value, "100", "the value in force is in the box");
  eq(steps.children[1].textContent, "save", "the press says what it does");
  eq(steps.children[1].disabled, true, "nothing to save until the box says something else");

  // A changed box is a press that can be made, and the enable happens *inside* the page's own
  // listener -- so the check delivers the event rather than assigning the property it sets.
  steps.children[0].value = "200";
  page.fire(steps.children[0], "input");
  eq(steps.children[1].disabled, false, "a changed value can be saved");
  steps.children[0].value = "100";
  page.fire(steps.children[0], "input");
  eq(steps.children[1].disabled, true, "and putting it back takes the press away again");
});

// The case that made the old `fillSelect` more than three lines, and it outlived the select: `/model`
// offers a provider's own `model` whether or not it is repeated in `models`, so a frame can name a
// current value that is not among the alternatives. A control built from `choices` alone would show
// whichever word came first and then *send* it the moment anything else on the page was touched.
check("a value the frame does not list is still the one shown", () => {
  const d = viewer.newDoc();
  viewer.applyState(d, JSON.stringify({
    type: "state",
    settings: [{ group: "model", key: "model", kind: "select", value: "hand-written",
      choices: ["stub-a", "stub-b"], send: "/model", help: "switch model" }],
  }));
  const models = controlOf(settingsOf(viewer, "model")[0]);
  eq(wordsOf(models), ["hand-written", "stub-a", "stub-b"], "the words, with what is in force first");
  eq(chosenOf(models), "hand-written", "the value in force");
});

check("a picker with one choice says so by being unusable", () => {
  const d = viewer.newDoc();
  viewer.applyState(d, JSON.stringify({
    type: "state",
    settings: [{ group: "model", key: "provider", kind: "select", value: "solo",
      choices: ["solo"], send: "/provider", help: "switch endpoint" }],
  }));
  const control = controlOf(settingsOf(viewer, "model")[0]);
  eq(wordsOf(control), ["solo"], "the one word the frame named is still drawn");
  eq(control.children[0].disabled, true, "and it is dead: one provider is not a choice");
});

check("the header says which model wrote this only when there is no state to say it", () => {
  const d = viewer.newDoc();
  d.meta = { model: "deepseek-chat", provider: "deepseek", cwd: "C:\\work" };
  // The stub keeps children across paints (`textContent = ""` is not a real element's), so the
  // span list is emptied here rather than read across two paints.
  const meta = viewer.__node("meta");
  meta.children.length = 0;
  viewer.paint(d);
  eq(meta.children.map((s) => s.textContent), ["model deepseek-chat", "cwd C:\\work", "provider deepseek"], "with no state");

  viewer.applyState(d, JSON.stringify({
    type: "state",
    settings: [{ group: "model", key: "model", kind: "select", value: "stub-model",
      choices: [], send: "/model", help: "switch model" }],
  }));
  meta.children.length = 0;
  viewer.paint(d);
  eq(meta.children.map((s) => s.textContent), ["cwd C:\\work"], "with a state, the screens say it");
});

check("a warning and an error read differently", () => {
  const d = doc(`{"message":"careful","type":"warning"}\n{"message":"broke","type":"error"}`);
  eq(d.blocks.map((b) => b.level), ["warning", "error"], "levels");
});

console.log("the screens of the settings dialog");

// §8's read channel, arranged for a person: the frame says which screen a command row belongs on
// (`group`), so the dialog can be a set of short screens instead of one column of everything. What
// is pinned here is that arrangement: a row is drawn on the screen the frame named, and a screen the
// frame filed nothing on is not offered at all.
check("the frame's rows are drawn on the screens it names, and only there", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel", group: "limits" },
      { label: "/reload", send: "/reload", help: "re-read the config file", class: "button", group: "run" },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector", group: "conversation" },
      { label: "/name [text]", send: "/name", help: "name this conversation", class: "form", group: "conversation" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger", group: "conversation" },
    ],
  }));
  eq(page.__node("row-list-limits").hidden, false, "a screen with rows on it is offered");
  eq(rowsOf(page, "limits").map((r) => r.children[0].textContent), ["/config"],
     "a report row is on the screen the frame filed it on");
  eq(rowsOf(page, "run").map((r) => r.children[0].textContent), ["/reload"],
     "and an action on its own");
  eq(rowsOf(page, "conversation").map((r) => r.children[0].textContent),
     ["/resume <n|id>", "/name [text]", "/delete <n|id>"],
     "the frame's order, kept inside the screen");
  eq(page.__node("row-list-model").hidden, true, "a screen the frame filed nothing on is not offered");
  eq(rowsOf(page, "tools").length, 0, "and holds no row of somebody else's");

  // A frame whose rows carry no screen at all: the dialog is empty rather than showing every row on
  // the first screen, which would be this page deciding where a row belongs -- the one thing the
  // `group` field exists to keep it from doing.
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [{ label: "/config", send: "/config", help: "show shell", class: "panel" }],
  }));
  for (const [key] of page.SETTINGS_PANES) {
    eq(page.__node("row-list-" + key).hidden, true, "a row with no screen is shown on none of them: " + key);
  }
});

// A report is *read* here rather than sent into the transcript, because the terminal is where
// somebody typed `/help` and this page's reader did not ask for a listing there. What is pinned here
// is which rows can be read and what the screen does while one is: the line to ask for comes from the
// frame, and the answer arrives on the feed marked as a panel's. That a press posts to `/report`
// rather than `/message` -- the whole difference between reading and printing -- is asserted over the
// page's bytes in tests/web_view.rs with the other compositions, since the stub DOM delivers no
// events.
check("a report row is pressable on its screen and the other rows are not", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel", group: "limits" },
      { label: "/help", send: "/help", help: "this message", class: "panel", group: "tools" },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector", group: "conversation" },
    ],
  }));
  const reports = rowsOf(page, "limits");
  eq(reports.map((r) => r.tag), ["button"], "a report row is a control");
  eq(reports[0].type, "button", "and not a submit button, which would reload the page");
  eq(reports[0].className, "row", "and is not marked as reference");
  eq(
    reports[0].children.map((c) => c.textContent),
    ["/config", "show shell, steps, proxy"],
    "it still says what to type and what it does"
  );
  eq(
    rowsOf(page, "conversation")[0].tag,
    "div",
    "a selector is still only a row: this page has nothing to choose from yet"
  );
});

check("a row that carries values offers one line per value", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/skills [name]", send: "/skills", help: "list skills", class: "panel",
        group: "tools", values: ["alpha", "beta"] },
      // A frame that carries no values is drawn as one row, so this check also says the value rows come
      // from the frame rather than from the class: the second command here is a panel row too.
      { label: "/config", send: "/config", help: "show shell", class: "panel", group: "tools" },
    ],
  }));
  const reports = rowsOf(page, "tools");
  eq(
    reports.map((r) => r.children[0].textContent),
    ["/skills [name]", "/skills alpha", "/skills beta", "/config"],
    "the row itself, then one line per value, then the next row"
  );
  eq(reports.map((r) => r.tag), ["button", "button", "button", "button"], "all of them are controls");
  eq(reports[1].title, "list skills", "a value row carries the row's own help");
});

check("a row that takes a field gets one, and only the rows the frame marks", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/name [text]", send: "/name", help: "name this conversation", class: "form",
        group: "conversation", fields: [{ field: "text", name: "text", optional: false }] },
      { label: "/provider key <key>", send: "/provider key", help: "set the API key", class: "form",
        group: "model", fields: [{ field: "password", name: "key", optional: false }] },
      { label: "/config edit", send: "/config edit", help: "change shell", class: "form", group: "limits" },
    ],
  }));
  const forms = rowsOf(page, "conversation");
  eq(
    forms.map((n) => n.tag),
    ["form"],
    "a row with answers gets a form"
  );
  const named = rowsOf(page, "model");
  eq(named.map((n) => n.tag), ["form"], "on the screen the frame filed it on");
  eq(
    [forms[0], named[0]].map((f) => f.children[0].tag + ":" + f.children[0].type),
    ["input:text", "input:password"],
    "the frame's word is the input's type, so a credential is masked because the process said so"
  );
  eq(
    [forms[0], named[0]].map((f) => f.children[1].textContent),
    ["/name", "/provider key"],
    "the button sends the row's own `send`, which is also what it says"
  );
  eq(forms[0].children[0].placeholder, "name this conversation", "one answer, so the help is what the field suggests");
  // A form row the frame does *not* mark is a row of reference, which is the half that says the field
  // comes from the frame rather than from the class: `/config edit` asks its questions at the
  // terminal, and a page that drew it a box would send a line nobody there can answer.
  const wizard = rowsOf(page, "limits");
  eq(wizard.map((n) => n.tag), ["div"], "the wizard stays a row of reference");
});

check("a row that takes several answers asks for each of them", () => {
  const page = loadViewer();
  const d = page.newDoc();
  // The frame's own list, kept here as well so the check can ask what the answers become without
  // reaching into the page for something it does not publish.
  const keyFields = [{ field: "password", name: "key", optional: false }];
  const addFields = [
    { field: "text", name: "name", optional: false },
    { field: "text", name: "base_url", optional: false },
    { field: "text", name: "model", optional: true },
  ];
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m",
    commands: [
      { label: "/provider key <key>", send: "/provider key", help: "set the API key for stub", class: "form",
        group: "model", fields: keyFields },
      { label: "/provider add <name> <base_url> [model]", send: "/provider add", help: "set up a new provider", class: "form",
        group: "model", fields: addFields },
    ],
  }));
  const forms = rowsOf(page, "model");
  eq(forms.length, 2, "both rows are drawn");
  const add = forms[1];
  eq(
    add.children.map((n) => n.tag),
    ["input", "input", "input", "button"],
    "one input per answer the frame names, and the button last"
  );
  eq(
    add.children.slice(0, 3).map((n) => n.placeholder),
    ["name", "base_url", "model (optional)"],
    "each input says which answer it wants, and which one may be left empty"
  );
  eq(add.children[3].textContent, "/provider add", "the button says the row's own `send`");

  // What the answers become. The optional one missing is not a hole: the line simply ends.
  eq(
    page.formLine("/provider add", addFields, ["claw", "http://127.0.0.1:8080/v1", ""]),
    "/provider add claw http://127.0.0.1:8080/v1",
    "an empty optional answer is left off"
  );
  eq(
    page.formLine("/provider add", addFields, ["claw", "http://127.0.0.1:8080/v1", "claw-3"]),
    "/provider add claw http://127.0.0.1:8080/v1 claw-3",
    "and a filled one is the last word"
  );
  // The refusal, which is the half a drawing cannot show: the bare command is the wizard in the
  // terminal, and a run being served to a page has nobody there to answer it.
  eq(
    page.formLine("/provider add", addFields, ["claw", "  ", ""]),
    null,
    "a required answer left empty makes no line at all"
  );
  eq(page.formLine("/provider key", keyFields, [""]), null, "and the same for a row with one answer");
});

check("a destructive row opens its choices rather than sending", () => {
  const page = loadViewer();
  // A row is a `code` child and the back button is its own text, so "what this row says" is one or
  // the other.
  const says = (n) => (n.children[0] ? n.children[0].textContent : n.textContent);
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m",
    providers: [{ name: "stub", models: ["m"] }, { name: "other", models: ["x"] }],
    commands: [
      { label: "/provider rm <name>", send: "/provider rm", help: "delete one", class: "danger",
        group: "model", from: "providers" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger",
        group: "conversation", from: "sessions" },
    ],
  }));
  const closed = rowsOf(page, "model");
  eq(closed.map((n) => n.tag), ["button"], "the row is pressable");
  eq(
    closed.map(says),
    ["/provider rm <name>"],
    "and closing the choices sends nothing: the line is not on any row yet"
  );

  // Opened, on the list this page already holds in `state`. The row being pressed says the whole
  // line, which is the point of two presses rather than one: the second press is the one that can
  // be read before it is made. The opened row's candidates *replace* that screen's rows, and the
  // other destructive row is untouched on its own screen -- one row being open is not a reason to
  // disturb the rest of the dialog.
  d.confirm = { send: "/provider rm" };
  page.paintSettings(d);
  const open = rowsOf(page, "model");
  eq(open.map((n) => n.tag), ["button", "button", "button"], "a way back and two candidates");
  eq(
    open.map(says),
    ["\u2039 back", "/provider rm stub", "/provider rm other"],
    "each candidate is the line that would be sent"
  );
  eq(open[0].className, "back", "the way back is marked as one");
  eq(open[1].className, "row danger", "and the ones that send are marked as destructive");
  eq(
    rowsOf(page, "conversation").map(says),
    ["/delete <n|id>"],
    "the other screen still has its own rows"
  );

  // The conversation row, on a page with no list to draw from -- which is the honest answer rather
  // than an empty list that looks like a list with nothing in it.
  d.confirm = { send: "/delete" };
  page.paintSettings(d);
  const empty = rowsOf(page, "conversation");
  eq(
    empty.map(says),
    ["\u2039 back", "nothing to choose from"],
    "an empty list of candidates says so"
  );
});

check("a row this page cannot press says where its control is", () => {
  const page = loadViewer();
  const d = page.newDoc();
  // A row the frame gave `values` is pressable wherever it sits: the rows are offered the names the
  // run already knows, which is the whole point of them -- the reader should not have to read a name
  // off one control and type it into another.
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m",
    commands: [
      { label: "/model <name>", send: "/model", help: "switch to one", class: "selector",
        group: "model", values: ["stub-model", "stub-other"] },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector",
        group: "conversation" },
      { label: "/provider add", send: "/provider add", help: "set up a new provider", class: "form",
        group: "model" },
      { label: "/something <x>", send: "/something", help: "a row of a class this page has not met",
        class: "unheard-of", group: "tools" },
    ],
  }));
  const offered = rowsOf(page, "model");
  eq(offered[0].tag, "button", "a switch with values is a control");
  eq(offered[0].className, "row", "not a reference row");
  eq(offered[0].title, "switch to one", "and keeps the frame's own help");
  eq(offered[0].children.map((n) => n.textContent), ["/model stub-model", "switch to one"], "the first value, as the line it sends");
  eq(offered[1].children.map((n) => n.textContent), ["/model stub-other", "switch to one"], "and the second");

  // Everything else is reference: a row that says what the command is, dressed so that it cannot be
  // mistaken for the control it is not -- and saying, on hover, where that control actually is.
  const conversation = rowsOf(page, "conversation")[0];
  eq(conversation.tag, "div", "the selector with nothing to offer is not pressable");
  eq(conversation.className, "row reference", "it is marked as reference");
  eq(conversation.title, "this one is a conversation on the left", "and names the one home it has");
  eq(rowsOf(page, "model")[2].tag, "div", "a form row with no field is reference too");
  eq(
    rowsOf(page, "model")[2].title,
    "this one is typed in the terminal -- it asks questions",
    "a form the terminal asks about is the terminal's"
  );
  eq(
    rowsOf(page, "tools")[0].title,
    "this one is typed in the terminal",
    "and a class this page has never met is not guessed at"
  );
  eq(
    rowsOf(page, "tools")[0].children.map((n) => n.textContent),
    ["/something <x>", "a row of a class this page has not met"],
    "and it still reads like a row"
  );
});

check("a listing being read replaces its own screen's rows, and the way back restores them", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/config", send: "/config", help: "show shell", class: "panel", group: "limits" },
      { label: "/tools", send: "/tools", help: "list tools", class: "panel", group: "tools" },
    ],
  }));
  d.reading = "/config";
  d.readingGroup = "limits";
  d.readingText = "config: /tmp/config.toml\n  verbose          = on";
  page.paintSettings(d);
  const drawn = listOf(page, "limits");
  eq(drawn[0].tag, "button", "the way back is a control");
  eq(drawn[1].textContent, "/config", "the heading is what was asked for");
  eq(drawn[2].textContent.includes("config.toml"), true, "and the listing is the process's own text");
  // One surface, one reading -- and the answer that arrives later is put there by the frame, not
  // appended to a list nobody is looking at. The other screen is untouched: a reading belongs to the
  // row that asked for it.
  eq(drawn.length, 3, "the rows are replaced rather than added to");
  eq(rowsOf(page, "tools").length, 1, "and another screen keeps its own rows");
  d.reading = null;
  d.readingGroup = "";
  page.paintSettings(d);
  const back = rowsOf(page, "limits");
  eq(back.length, 1, "going back draws the rows again");
  eq(labelsOf(page, "limits"), ["/config"], "with the report row in it, under its class again");
  eq(headingsOf(page, "limits"), ["reports"], "and the grouping is back with it");
});

check("a report frame fills the screen only for what is being read", () => {
  const page = loadViewer();
  const d = page.newDoc();
  d.reading = "/tools";
  d.readingGroup = "tools";
  d.readingText = "";
  page.applyLine(d, JSON.stringify({ type: "command", input: "/config", text: "shell = bash", panel: true }));
  eq(d.readingText, "", "an answer to a listing the reader moved on from is not shown");
  eq(d.blocks.length, 0, "and a report is not a transcript block either");
  page.applyLine(d, JSON.stringify({ type: "command", input: "/tools", text: "read, write", panel: true }));
  eq(d.readingText, "read, write", "the answer to what is being read fills the screen");
  eq(d.blocks.length, 0, "still nothing in the transcript: that is the whole point of the class");
});

check("a command answer without the panel mark is still a transcript block", () => {
  const page = loadViewer();
  const d = page.newDoc();
  // The same frame shape, one field short: this is what a typed `/config` produces, and it belongs
  // in the transcript even while a screen is showing something else.
  d.reading = "/tools";
  page.applyLine(d, JSON.stringify({ type: "command", input: "/config", text: "shell = bash" }));
  eq(d.readingText, "", "a typed answer is not put in the screen");
  eq(d.blocks.length, 1, "it is a block in the transcript");
  eq(d.blocks[0].kind, "command", "of the kind the transcript already drew");
});

console.log("the buttons the frame marks as actions");

// §8's first control. The frame already says which commands are one action with no argument
// (`class: "button"`), so the page's job is only to draw them on the screen the frame named and send
// the row's own line. That the click sends `send` rather than a name put back together here is
// asserted over the page's bytes in tests/web_view.rs, the same way the settings' `send + " " +
// value` is: the stub DOM has no click delivery, and inventing some would be testing the stub.
check("the frame's action rows become buttons on the screen it names", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel", group: "limits" },
      { label: "/new", send: "/new", help: "start a fresh conversation", class: "button", group: "conversation" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger", group: "conversation" },
      { label: "/reload", send: "/reload", help: "re-read the config file", class: "button", group: "run" },
    ],
  }));
  const buttons = rowsOf(page, "run");
  eq(buttons.map((b) => b.children[0].textContent), ["/reload"], "one button per action on that screen");
  eq(buttons[0].title, "re-read the config file", "the help is the tooltip");
  eq(buttons[0].type, "button", "a button that cannot submit anything");
  eq(buttons[0].className, "row action", "marked as an action rather than a report");
  // The class is also the heading, and the words are the `/` menu's own: a launcher and a screen are
  // answering the same question about a row, and two sets of names for five classes is a page
  // teaching a person both. The frame's order is kept *inside* a group -- the groups are the classes,
  // in the menu's order, which is what puts the destructive pair at the bottom of a long screen.
  eq(headingsOf(page, "run"), ["actions"], "under the class's own word");
  eq(labelsOf(page, "conversation"), ["/new", "/delete <n|id>"],
     "a screen of two classes reads as its classes, each in the frame's order");
  eq(headingsOf(page, "conversation"), ["actions", "destructive"],
     "with the rows that destroy work last, under their own word");
});

check("a state frame with no actions in it takes the buttons away", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [],
    commands: [{ label: "/reload", send: "/reload", help: "re-read the config file", class: "button", group: "run" }],
  }));
  eq(rowsOf(page, "run").length, 1, "a button while the frame lists an action");
  eq(page.__node("row-list-run").hidden, false, "and the screen it is on is offered");
  // A frame with only reports in it: nothing to press on that screen. A panel is not a button --
  // pressing one would send a command into the terminal, which is the one place §8 says a report
  // should not go.
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [{ label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel", group: "limits" }],
  }));
  eq(rowsOf(page, "run").length, 0, "no action rows, no buttons");
  eq(page.__node("row-list-run").hidden, true, "and a screen with nothing left on it is not offered");
});

console.log("where the lines come from");

check("the token is read out of the URL that --web printed", () => {
  eq(viewer.tokenFromSearch("?token=deadbeef"), "deadbeef", "the only parameter");
  eq(viewer.tokenFromSearch("?a=1&token=deadbeef&b=2"), "deadbeef", "among others");
  eq(viewer.tokenFromSearch(""), "", "no query at all");
  eq(viewer.tokenFromSearch("?token="), "", "an empty token is no token");
  eq(viewer.tokenFromSearch("?nottoken=deadbeef"), "", "a similarly named parameter");
});

check("a page from disk is the dropped-file level, and a served one is not", () => {
  const at = (protocol, search) => ({ protocol, search });
  ok(!viewer.servedByFlint(at("file:", "")), "file:// has nothing to fetch from");
  ok(!viewer.servedByFlint(at("http:", "")), "served, but with no token to authenticate with");
  ok(viewer.servedByFlint(at("http:", "?token=deadbeef")), "this is the --web case");
  ok(!viewer.servedByFlint(null), "no location at all");
});

console.log("the event stream, as it arrives");

check("a frame that arrives in pieces is not lost", () => {
  // The chunk boundary is the whole risk: a network read ends wherever it ends.
  const whole = "id: 7\ndata: {\"type\":\"message.delta\",\"text\":\"hi\"}\n\n";
  let seen = [];
  let buffer = "";
  for (const piece of [whole.slice(0, 9), whole.slice(9, 30), whole.slice(30)]) {
    buffer += piece;
    const cut = viewer.sseFrames(buffer);
    buffer = cut.rest;
    seen = seen.concat(cut.frames);
  }
  eq(seen.length, 1, "one frame in three pieces");
  eq(seen[0].id, "7", "the cursor");
  eq(JSON.parse(seen[0].data).text, "hi", "the line");
  eq(buffer, "", "nothing left over");
});

check("several frames in one chunk all arrive, in order", () => {
  const cut = viewer.sseFrames(
    "id: 1\ndata: a\n\nid: 2\ndata: b\n\nid: 3\ndata: c\n\npart"
  );
  eq(cut.frames.map((f) => f.id), ["1", "2", "3"], "ids");
  eq(cut.frames.map((f) => f.data), ["a", "b", "c"], "data");
  eq(cut.rest, "part", "the incomplete tail is held back");
});

check("a heartbeat comment is not a frame, and does not end one", () => {
  const cut = viewer.sseFrames(": ping\n\nid: 1\ndata: real\n\n");
  eq(cut.frames.length, 1, "only the real frame");
  eq(cut.frames[0].data, "real", "data");
});

check("a named event is told apart from a run event", () => {
  // `reset` is the transport saying "your document is stale"; every unnamed frame is a
  // fact about the run. The page does different things with the two.
  const cut = viewer.sseFrames("event: reset\ndata: {}\n\nid: 2\ndata: {\"type\":\"status\"}\n\n");
  eq(cut.frames[0].event, "reset", "named");
  eq(cut.frames[1].event, "message", "unnamed is a message");
  eq(cut.frames[1].id, "2", "and still carries its cursor");
});

check("a value with no space after the colon is read the same way", () => {
  const cut = viewer.sseFrames("id:9\ndata:x\n\n");
  eq(cut.frames[0].id, "9", "id");
  eq(cut.frames[0].data, "x", "data");
});

// ---------------------------------------------------------------------------
// the sidebar, and what the composer says
// ---------------------------------------------------------------------------

console.log("\nthe conversation list, and what a click sends");

check("the list is read out of the route's own shape", () => {
  const got = viewer.sessionsFrom(JSON.stringify({
    sessions: [
      { n: 1, id: "200-2", label: "the newer question", current: true },
      { n: 2, id: "100-1", label: "the older one", current: false },
    ],
  }));
  eq(got.length, 2, "two rows");
  eq(got[0].n, 1, "the number /resume takes");
  eq(got[0].current, true, "the open one");
  eq(got[1].label, "the older one", "the label");
});

check("a list that is not a list is empty rather than fatal", () => {
  // The sidebar is not worth taking the page down for: the transcript beside it still reads.
  // Every one of these is a shape a bad day could produce.
  for (const text of ["", "not json", "{}", '{"sessions":null}', '{"sessions":"nope"}', "[]"]) {
    eq(viewer.sessionsFrom(text), [], `for ${JSON.stringify(text)}`);
  }
});

check("a row without a usable number or id is dropped, not rendered broken", () => {
  const got = viewer.sessionsFrom(JSON.stringify({
    sessions: [
      { n: 1, id: "good", label: "kept" },
      { id: "no number", label: "dropped" },
      { n: 2, label: "no id" },
      { n: 3, id: "label missing" },
      null,
    ],
  }));
  eq(got.map((s) => s.id), ["good", "label missing"], "only the usable rows");
  eq(got[1].label, "(empty)", "a missing label gets the same word the terminal uses");
});

check("opening a conversation sends the number, not a path", () => {
  // The page must not know where sessions live. `/resume <n>` is the vocabulary `/sessions`
  // prints, and the process is what turns it into a file -- so a change to where sessions are
  // kept cannot break this, and the page cannot open something the terminal could not.
  eq(viewer.resumeLine(3), "/resume 3", "the line");
});

check("a message is the shape the route reads", () => {
  eq(JSON.parse(viewer.messageBody("hello")), { text: "hello" }, "an object with the text");
  // Including the ones that look like commands: the route must carry them, not interpret them.
  eq(JSON.parse(viewer.messageBody("/resume 3")).text, "/resume 3", "a slash command");
  eq(JSON.parse(viewer.messageBody('line one\nline two')).text, "line one\nline two", "a block");
});

console.log("\nwhat a mid-turn reset must not destroy");

check("the answer being streamed is kept when the file is behind", () => {
  // `/session` is the file, and the file gets the assistant message when the turn *ends* -- so
  // a reset during a turn re-reads a document without the answer in it. Measured: 1,424,691
  // characters on screen became 64,999.
  const d = viewer.newDoc();
  viewer.applyEvent(d, { type: "turn.started", prompt: "ask" });
  viewer.applyEvent(d, { type: "message.delta", text: "the answer so far" });
  const streaming = viewer.streamingAnswer(d);
  eq(streaming.text, "the answer so far", "what was on screen");

  // The file, meanwhile, only knows the question.
  const reloaded = viewer.newDoc();
  viewer.applyEvent(reloaded, { type: "turn.started", prompt: "ask" });
  viewer.carryStreaming(reloaded, streaming);
  eq(reloaded.blocks.length, 2, "the answer is back");
  eq(reloaded.blocks[1].text, "the answer so far", "whole");
  eq(reloaded.blocks[1].open, true, "and still taking deltas");
});

check("the file's own copy is preferred once it has one", () => {
  // The turn ended and wrote the answer; the page's memory of it is then the older copy, and
  // adding it would duplicate the answer instead of rescuing it.
  const d = viewer.newDoc();
  viewer.applyEvent(d, { type: "turn.started", prompt: "ask" });
  viewer.applyEvent(d, { type: "message.delta", text: "half" });
  const streaming = viewer.streamingAnswer(d);

  const reloaded = viewer.newDoc();
  viewer.applyEvent(reloaded, { type: "turn.started", prompt: "ask" });
  viewer.applyEvent(reloaded, { type: "chat", message: { role: "assistant", content: "half and the rest" } });
  viewer.carryStreaming(reloaded, streaming);
  eq(reloaded.blocks.length, 2, "nothing appended");
  eq(reloaded.blocks[1].text, "half and the rest", "the file's copy stands");
});

check("a longer stream wins over a shorter file", () => {
  // The exact race the prefix test is for: the file has a partial answer, the page has more.
  const d = viewer.newDoc();
  viewer.applyEvent(d, { type: "turn.started", prompt: "ask" });
  viewer.applyEvent(d, { type: "message.delta", text: "one two three" });
  const streaming = viewer.streamingAnswer(d);
  const reloaded = viewer.newDoc();
  viewer.applyEvent(reloaded, { type: "turn.started", prompt: "ask" });
  viewer.applyEvent(reloaded, { type: "chat", message: { role: "assistant", content: "one two" } });
  viewer.carryStreaming(reloaded, streaming);
  eq(reloaded.blocks.length, 2, "still one answer");
  eq(reloaded.blocks[1].text, "one two three", "the longer one");
});

check("nothing to carry is nothing to do", () => {
  const d = viewer.newDoc();
  const before = viewer.newDoc();
  viewer.applyEvent(before, { type: "turn.started", prompt: "ask" });
  viewer.carryStreaming(before, viewer.streamingAnswer(d));
  eq(before.blocks.length, 1, "unchanged");
});

check("a delta with nothing in it does not open an answer", () => {
  // Seen on a page reloaded while a turn was waiting on the model: a blank "flint" heading that
  // no text ever arrived to fill, because the delta that opened it was empty.
  const d = viewer.newDoc();
  viewer.applyEvent(d, { type: "message.delta", text: "" });
  eq(d.blocks.length, 0, "no block for an empty delta");
  viewer.applyEvent(d, { type: "reasoning.delta" });
  eq(d.blocks.length, 0, "no block for a delta with no text at all");
  // But the first real piece still opens one.
  viewer.applyEvent(d, { type: "message.delta", text: "hello" });
  eq(d.blocks.length, 1, "the first real piece opens it");
  eq(d.blocks[0].text, "hello", "with the text");
  // And a later empty piece appends nothing without losing the block.
  viewer.applyEvent(d, { type: "message.delta", text: "" });
  eq(d.blocks.length, 1, "still one block");
  eq(d.blocks[0].text, "hello", "unchanged");
});

check("the page follows an answer only when the reader was at the bottom", () => {
  // Two elements, and the follow-the-tail code has to use the right one: `#doc` is the reading
  // column and does not scroll, so a `scrollTop` read on it is a constant and a write to it moves
  // nothing. Shipped that way once -- the answer arrived below the fold and the page looked
  // frozen while the terminal had it.
  const scroller = viewer.__node("transcript");
  const doc = viewer.applyText(viewer.newDoc(), SESSION);
  eq(doc.blocks.length > 0, true, "the session renders blocks");

  scroller.scrollHeight = 5000;
  scroller.clientHeight = 600;
  scroller.scrollTop = 4400;
  viewer.paint(doc);
  eq(scroller.scrollTop, 5000, "a reader at the bottom is carried to the new bottom");

  scroller.scrollTop = 100;
  viewer.paint(doc);
  eq(scroller.scrollTop, 100, "a reader further up is left where they were");
});

check("a page that cannot read says which kind of failure it is", () => {
  // The two lines are not interchangeable: a refused token is permanent (it is in the URL the page
  // was opened with, and a restart changes it) while an ended stream is retried. Showing the
  // retrying line for a refused token is what made a dead page look like a broken flint.
  //
  // 403 and not 401: that is what the listener actually answers. Measured against a running flint
  // with `curl /session` and no token header: `403 missing or wrong token`. The first version of
  // the page checked 401, which the listener never sends, so this test would have passed while the
  // page went on saying "reconnecting" -- which is why the measured code is in the test.
  const refused = viewer.feedTrouble("HTTP 403");
  eq(/token/.test(refused), true, "a refused token says so");
  eq(/reconnecting/.test(refused), false, "and does not promise to reconnect");
  eq(viewer.tokenRefused("HTTP 403"), true, "403 is the refused-token code");
  eq(viewer.tokenRefused("HTTP 401"), true, "and 401 from something in front of it counts too");
  eq(viewer.tokenRefused("Failed to fetch"), false, "a network failure is not a refused token");
  const ended = viewer.feedTrouble("the stream ended");
  eq(/reconnecting/.test(ended), true, "an ended stream is retried");
  eq(ended.includes("the stream ended"), true, "and says what ended");
});

check("the feed trace says what the page cannot work out for itself", () => {
  // No frames, skipped frames and frames rendered out of sight look identical from outside, so the
  // `?debug=1` line has to carry all three numbers: what arrived, what it became, and what the last
  // one was. This is the line a bug report can quote.
  const line = viewer.feedTrace(7, 3, "status seq 12");
  eq(line.includes("7 frames"), true, "how many frames arrived");
  eq(line.includes("3 blocks"), true, "how many became blocks");
  eq(line.includes("status seq 12"), true, "which frame was last");
  eq(viewer.debugFeed, false, "off unless the address asks for it");
});

console.log("the addresses in a transcript");

check("an address is cut out of the line around it, and a word that only looks like one is not", () => {
  // The rule in full, on the shapes a tool block actually contains. Every entry here is a case
  // that was argued about rather than an example that happened to pass: `e.g.` and `and/or` are
  // why the rule has an extension length and a segment count at all.
  const parts = viewer.addressParts(
    "wrote C:\\work\\src\\main.rs and tests/say.rs:412:3, see and/or e.g. 4/2 https://x.dev/a/b.rs"
  );
  eq(
    parts.filter((p) => p.path !== undefined),
    [
      { path: "C:\\work\\src\\main.rs", line: 0, written: "C:\\work\\src\\main.rs" },
      { path: "tests/say.rs", line: 412, written: "tests/say.rs:412:3" },
    ],
    "the two real paths, with the line a grep hit was on and the token as it was written"
  );
  // The URL is no longer one of the pieces of text: it is the one candidate in this line with a
  // real address to go to, and the splitter says which kind it is rather than leaving the renderer
  // to guess again from the string.
  eq(
    parts.filter((p) => p.url !== undefined),
    [{ url: "https://x.dev/a/b.rs" }],
    "the address in the line is an address"
  );
  // Joined back together, nothing is lost or doubled. `written` is what makes this lossless: the
  // column of a `:line:column` hit is not part of the path the route opens, but it is part of what
  // the tool said, and the splitter carries both. This assertion is what makes the splitter a
  // splitter rather than a renderer that eats the text between two paths.
  eq(
    parts
      .map((p) =>
        p.path !== undefined ? p.written : p.url !== undefined ? p.url : p.text
      )
      .join(""),
    "wrote C:\\work\\src\\main.rs and tests/say.rs:412:3, see and/or e.g. 4/2 https://x.dev/a/b.rs",
    "the pieces put back together are exactly the line that came in"
  );

  // `asPath` answers with `{path, line, written}`; most of the cases below are about which *path* a
  // token is, so they compare those two fields. What the button prints is `written`, and the check
  // after this one is where that is asserted.
  const bare = (found) => (found ? { path: found.path, line: found.line } : null);
  // Absolute and relative, and the two shapes with no separator at all that are still files.
  eq(bare(viewer.asPath("/tmp/flint/spill/1.txt")), { path: "/tmp/flint/spill/1.txt", line: 0 }, "an absolute path");
  eq(bare(viewer.asPath("./src/bin")), { path: "./src/bin", line: 0 }, "an explicitly relative path");
  eq(viewer.asPath("src/bin"), null, "one separator and no extension is left as text");
  eq(viewer.asPath("and/or"), null, "which is what keeps `and/or` out of it");
  eq(bare(viewer.asPath("a/b/c")), { path: "a/b/c", line: 0 }, "three segments are a path");
  eq(bare(viewer.asPath("Cargo.toml")), { path: "Cargo.toml", line: 0 }, "a name with an extension");
  eq(viewer.asPath("e.g."), null, "a prose abbreviation is not a file");
  eq(viewer.asPath("4/2"), null, "a ratio is not a directory");
  eq(viewer.asPath("2024/09/17"), null, "a date is not a directory");
  eq(viewer.asPath("https://api.github.com/repos/x/y.rs"), null, "a URL is not a path");
  // ...and the narrower rule prose is read with: only a path that is absolute, because a sentence
  // is where `src/bin` and `and/or` and `e.g.` are all just words.
  eq(bare(viewer.asPath("/tmp/flint/spill/1.txt", true)), { path: "/tmp/flint/spill/1.txt", line: 0 }, "an absolute path, in prose");
  eq(bare(viewer.asPath("C:\\work\\main.rs", true)), { path: "C:\\work\\main.rs", line: 0 }, "a Windows path, in prose");
  eq(viewer.asPath("src/main.rs", true), null, "a relative name in a sentence is a name");
  eq(viewer.asPath("Cargo.toml", true), null, "and so is a bare filename in one");

  // A slash-rooted path has to be two segments deep or end in an extension, because this program's
  // own commands are one segment with a slash in front and a conversation about flint is full of
  // them. This was a bug rather than a worry: every `/jobs`, `/name` and `/events` in a transcript
  // was a button onto a file that does not exist.
  eq(viewer.asPath("/stop"), null, "a slash command is not a file");
  eq(viewer.asPath("/jobs"), null, "nor is one this page offers");
  eq(viewer.asPath("/events"), null, "nor is the route behind the live feed");
  eq(bare(viewer.asPath("/etc/hosts")), { path: "/etc/hosts", line: 0 }, "while two real segments are a path");
  eq(bare(viewer.asPath("/notes.md")), { path: "/notes.md", line: 0 }, "and an extension is the same evidence");
  eq(viewer.asPath("/tmp"), null, "one segment with no extension is a directory or a command, and neither is read");
  // The shape is asked of a *slash*-rooted path only: no command word begins with `~` or a drive
  // letter, so those need no second piece of evidence -- and `~/notes` is a file people really mean.
  eq(bare(viewer.asPath("~/.flint/config.toml")), { path: "~/.flint/config.toml", line: 0 }, "a home-relative path");
  eq(bare(viewer.asPath("~/notes")), { path: "~/notes", line: 0 }, "even a bare name under it");
  // ...and a file whose *name* begins with a tilde, which is a relative name and not a home: in a
  // sentence it stays a word, and in a tool result it is a path the run resolves against its own
  // directory -- never against the home directory, which is the rule `config::expand_home` states
  // on the other side of the wire. Two readers, one definition of what a tilde path is.
  eq(viewer.asPath("~notes.txt", true), null, "a file *named* with a leading tilde is a word in prose");
  eq(bare(viewer.asPath("~notes.txt")), { path: "~notes.txt", line: 0 }, "and a relative name in a tool result, not a home");
  eq(bare(viewer.asPath("C:\\notes", true)), { path: "C:\\notes", line: 0 }, "and a drive-rooted path needs none either");
});

check("a web address becomes a link, and only the two web schemes are addresses", () => {
  // The scheme test is the security boundary rather than a cosmetic one: this document holds the
  // run's token, and an `href` built out of a model's words is a way to run script in it --
  // `javascript:` needs no bug, only a click. `data:` and `file:` are refused for the same reason,
  // and a `file:` address is one no page served over http may open in any browser anyway.
  const nodes = viewer.linkNodes(
    "see https://api.github.com/repos/x/y.rs. then javascript:alert(1) and data:text/html,<b>x</b> and file:///C:/notes.txt"
  );
  const links = nodes.filter((n) => n.tag === "a");
  eq(links.length, 1, "one address in the line is a link");
  eq(links[0].href, "https://api.github.com/repos/x/y.rs", "the href is the address, without the full stop after it");
  eq(links[0].textContent, "https://api.github.com/repos/x/y.rs", "the link reads as the address itself");
  eq(links[0].target, "_blank", "a page opens in a new tab: this document is the conversation");
  ok(
    String(links[0].rel).indexOf("noopener") !== -1,
    "and the new tab cannot reach back through window.opener"
  );
  const text = nodes
    .filter((n) => n.tag === undefined)
    .map((n) => n.text)
    .join("");
  ok(text.indexOf("javascript:alert(1)") !== -1, "a scheme that is not the web is left as the words it is");
  // `file:` is still never a *link* -- no browser would follow one from a page served over http,
  // and the page may not navigate this document anywhere at all. It is the page's own path button
  // instead: a different door (`GET /file`, with the run's token, judged by the run), which is what
  // a person pointing at a local file actually wants. The security claim is about the `<a>`, and it
  // is unchanged by that.
  eq(nodes.filter((n) => n.tag === "a" && String(n.href).indexOf("file:") === 0).length, 0, "no anchor goes to a local file");
  eq(
    nodes.filter((n) => n.tag === "button").map((b) => b.textContent),
    ["C:/notes.txt"],
    "and the local address is a path button that reads it through the run"
  );
});

// The line a hit was on, in the three spellings three tools use, and the fourth way a path itself
// gets written. All four were reported from a real transcript: a GitHub-style `#L42` and a
// `file:///C:/…:42` were plain text (so nothing could be pressed), and a `:412` that *was*
// recognised lost its line from the screen -- the button said `src/web.rs` while the tool had said
// `src/web.rs:412`, which is the one detail a hit is worth reading for.
check("a line number is written three ways, and the button keeps it", () => {
  const at = (token, prose) => {
    const found = viewer.asPath(token, prose);
    return found ? { path: found.path, line: found.line } : null;
  };
  eq(at("src/web.rs:412"), { path: "src/web.rs", line: 412 }, "the compiler's and the editor's spelling");
  eq(at("src/web.rs:412:7"), { path: "src/web.rs", line: 412 }, "a grep hit, whose column is dropped");
  eq(at("src/web.rs#L412"), { path: "src/web.rs", line: 412 }, "GitHub's spelling");
  eq(at("src/web.rs#L412-L420"), { path: "src/web.rs", line: 412 }, "a GitHub range keeps its first line");
  eq(at("file:///C:/work/vtscreen.js:42"), { path: "C:/work/vtscreen.js", line: 42 }, "file:// with a line");
  eq(at("file:///C:/work/vtscreen.js#L42"), { path: "C:/work/vtscreen.js", line: 42 }, "and with GitHub's");
  eq(at("file:///home/me/x.js"), { path: "/home/me/x.js", line: 0 }, "on Unix the root is the path's own");
  eq(viewer.asPath("file://server/share/x.js"), null, "a path on another machine stays text");

  // What the button says, and what it opens: `written` is the token as the reader met it, `path` is
  // what the route is asked to read. A path with no line has the two equal.
  const nodes = viewer.linkNodes(
    "see src/web.rs:412:7 and C:\\work\\a\\vtscreen.js#L42 and file:///C:/work/b.js:9 and C:\\work\\plain.rs",
    false
  );
  const buttons = nodes.filter((n) => n.tag === "button");
  eq(
    buttons.map((b) => b.textContent),
    ["src/web.rs:412:7", "C:\\work\\a\\vtscreen.js#L42", "C:/work/b.js:9", "C:\\work\\plain.rs"],
    "the button prints the line, in the words the tool used, and drops only the file:// scheme"
  );
  eq(
    buttons.map((b) => b.title),
    ["src/web.rs:412", "C:\\work\\a\\vtscreen.js:42", "C:/work/b.js:9", "C:\\work\\plain.rs"],
    "and the tooltip names the file and the line the panel will open at"
  );
});

check("prose gets the web addresses and the absolute paths, and a word is left alone", () => {
  const nodes = viewer.linkNodes(
    "look at /etc/hosts, C:\\work\\a.txt, and/or e.g. src/main.rs, then https://x.dev/a for more",
    true
  );
  eq(
    nodes.filter((n) => n.tag === "button").map((b) => b.textContent),
    ["/etc/hosts", "C:\\work\\a.txt"],
    "the two absolute paths in the sentence, and nothing that merely looks like one"
  );
  eq(nodes.filter((n) => n.tag === "a").map((a) => a.href), ["https://x.dev/a"], "and the one address in it");
  const text = nodes
    .filter((n) => n.tag === undefined)
    .map((n) => n.text)
    .join("");
  ok(text.indexOf("src/main.rs") !== -1, "a relative name in a sentence stays a name");
});

check("a turn's own words carry those addresses into the page", () => {
  // The view-level half, and the reason this check exists at all: a splitter nobody calls would
  // pass every check above. This is the path prose actually takes into the transcript.
  const block = viewer.renderBlock({ kind: "assistant", text: "done: https://x.dev/a and /tmp/notes.txt" });
  const found = { links: [], buttons: [] };
  const walk = (node) => {
    if (!node) return;
    if (node.tag === "a") found.links.push(node);
    if (node.tag === "button" && node.className === "path") found.buttons.push(node);
    for (const child of node.children || []) walk(child);
  };
  walk(block);
  eq(found.links.map((a) => a.href), ["https://x.dev/a"], "the address in the answer is a link");
  eq(found.buttons.map((b) => b.textContent), ["/tmp/notes.txt"], "and the file named in it is a button");
});

check("a rendered tool block makes its paths buttons, and pressing one opens the panel", () => {
  // The one check that runs the *view* half: the block is built, walked for the buttons the
  // splitter's rule produced, and pressed. What the press must do is ask the route for the path it
  // named -- encoded, because `?path=` with a raw backslash or space is a different request.
  eq(viewer.fileRoute("src/main.rs"), "/file?path=src%2Fmain.rs", "the path, percent-encoded");
  eq(
    viewer.fileRoute("C:\\work\\a b.txt"),
    "/file?path=C%3A%5Cwork%5Ca%20b.txt",
    "a backslash, a colon and a space all travel escaped"
  );

  const block = viewer.renderBlock({
    kind: "tool", id: "call_1", name: "write", args: '{"path":"src/main.rs"}',
    output: "wrote src/main.rs", done: true, ok: true,
  });
  const buttons = [];
  const walk = (node) => {
    if (!node) return;
    if (node.tag === "button" && node.className === "path") buttons.push(node);
    for (const child of node.children || []) walk(child);
  };
  walk(block);
  // Three: the arguments appear twice in one block by design -- clipped in the summary, whole in
  // the `pre` under it -- and the path in the output is the third.
  eq(buttons.length, 3, "the arguments twice and the output once");
  eq(buttons[1].textContent, "src/main.rs", "the button is the path, not a label for it");

  buttons[1].handlers.click[0]({ preventDefault() {}, stopPropagation() {} });
  // The harness has no run behind it (`canSend` is false, as it is for a dropped session file), so
  // what is asserted here is the panel and the honest sentence. The fetch is the browser harness's
  // to check, against a real listener.
  eq(viewer.__node("preview").hidden, false, "the panel is on screen");
  eq(
    viewer.__node("preview-path").children.map((n) => n.textContent).join(""),
    "src/main.rs",
    "the header shows the path that was pressed"
  );
  eq(
    /no flint behind it/.test(viewer.__node("preview-text").textContent),
    true,
    "and says why it cannot read it: " + viewer.__node("preview-text").textContent
  );
});

check("the open control sends the run's own route, and the frame decides whether it is offered", () => {
  // The body `POST /open` takes, which is a path in JSON -- and the reason it is a function rather
  // than a string built at the call site: a Windows path is full of backslashes, and a quote in a
  // name must not end the JSON string early. The route reads `path` and nothing else.
  eq(viewer.openBody("src/main.rs"), '{"path":"src/main.rs"}', "a plain path");
  eq(
    viewer.openBody("C:\\work\\a b.txt"),
    '{"path":"C:\\\\work\\\\a b.txt"}',
    "a Windows path keeps its backslashes, escaped the way JSON escapes them"
  );
  eq(
    JSON.parse(viewer.openBody('a"b.txt')).path,
    'a"b.txt',
    "a quote in a file's name arrives as itself"
  );

  // And the guard: the page does not launch anything in a run whose own tools may not. The frame is
  // the source -- the same `readonly` toggle the header draws and the terminal prints -- so this is
  // checked against frames rather than against a run with its guard turned on mid-turn.
  // A switch is a setting now, so the guard arrives as one key among the others on the `run` screen:
  // `values: ["off", "on"]`, `value` saying which one is in force.
  const guard = (value) => ({ settings: [{ group: "run", key: "readonly", kind: "select", value,
    choices: ["off", "on"], send: "/readonly", help: "refuse writes" }] });
  eq(viewer.readonlyOn(guard("on")), true, "a run with the guard on");
  eq(viewer.readonlyOn(guard("off")), false, "and one with it off");
  eq(viewer.readonlyOn({ settings: [] }), false, "a frame with no such setting offers it");
  eq(viewer.readonlyOn(null), false, "and so does a page with no state at all");
});

check("the preview says which line, and how much of a cut file is here", () => {
  // The two headers the route adds rather than the transport. Without them the panel would show
  // the first 512 KB of a 40 MB log and say nothing about the other 39.5 MB.
  const headers = (map) => ({ get: (name) => (name in map ? map[name] : null) });
  eq(viewer.previewNote({ headers: headers({}) }, "one\ntwo\n", 0), "8 bytes", "a small file is its size");
  eq(viewer.previewNote({ headers: headers({}) }, "one\ntwo\n", 12), "line 12", "the line it opened at");
  eq(
    viewer.previewNote({ headers: headers({ "X-Flint-Cut": "524288", "X-Flint-Size": "41943040" }) }, "x", 7),
    "line 7 · the first 512 KB of 40 MB",
    "both facts, in the order a reader wants them"
  );
  eq(viewer.bytesLabel(0), "0 bytes", "bytes");
  eq(viewer.bytesLabel(1536), "2 KB", "kilobytes, rounded");
  eq(viewer.bytesLabel(12 * 1024 * 1024), "12 MB", "megabytes");
});

// The directory half of the same press. Reported 2026-09-23: *pressing a directory does not go
// anywhere, and a directory with a space in its name is not recognised at all.* The second half of
// that is not a scanner bug to be fixed in the scanner -- a name with a space is two tokens to
// anything reading text -- so the listing is a route, each entry arrives as one path the run built,
// and the panel draws a control per entry. What is checked here is the decision (which route a path
// is asked of, and the header that corrects it), the mapping from the route's JSON to rows, and what
// the panel draws.
check("a directory is read through its own route, and a listing draws one row per entry", () => {
  // A page of its own: `paintDir` and the press below both write the panel's own state, and a stub
  // node cannot be emptied by assigning `textContent = ""` (it is a plain field there) -- so a check
  // sharing a page with an earlier press would read that press's leftovers.
  const page = loadViewer();
  // The route, encoded the way `/file` and `/image` are.
  eq(page.dirRoute("C:\\work\\My Projects"), "/dir?path=C%3A%5Cwork%5CMy%20Projects",
     "a directory with a space is one encoded path");
  eq(page.dirRoute("src/"), "/dir?path=src%2F", "a trailing separator travels as itself");

  // Which route is asked first. A trailing separator is the one thing a *name* says about being a
  // directory; it is how the run's own `list` prints one, and being wrong costs one request because
  // `/file` says so in a header (`dirHeader`).
  ["src/", "C:\\work\\", "/tmp/", "~/notes/", "  src/  "].forEach((p) => {
    eq(page.dirPath(p), true, p + " ends in a separator, so it is asked for as a directory");
  });
  ["src", "notes.txt", "C:\\work\\a b.txt", "", null, "src//x"].forEach((p) => {
    eq(page.dirPath(p), false, JSON.stringify(p) + " says nothing about being a directory");
  });
  const headers = (map) => ({ get: (name) => (name in map ? map[name] : null) });
  eq(page.dirHeader({ headers: headers({ "X-Flint-Dir": "1" }) }), true, "the header says a directory");
  eq(page.dirHeader({ headers: headers({}) }), false, "and only that header does");
  eq(page.dirHeader(null), false, "with no answer there is nothing to read");

  // The route's JSON to rows. The paths are the ones the run built, in the order the run listed them,
  // with the way up first when there is one -- and the note counts what the route counted rather than
  // what was drawn, because a directory can hold more than one answer carries.
  const listing = page.dirRows({
    path: "C:\\work",
    parent: "C:\\",
    total: 3,
    shown: 3,
    entries: [
      { name: "My Projects", line: "My Projects/", path: "C:\\work\\My Projects", dir: true, size: 0 },
      { name: "notes.txt", line: "notes.txt  (12 bytes)", path: "C:\\work\\notes.txt", dir: false, size: 12 },
      { name: "sub", line: "sub/", path: "C:\\work\\sub", dir: true, size: 0 },
    ],
  });
  eq(listing.rows.map((r) => r.label), ["..", "My Projects/", "notes.txt  (12 bytes)", "sub/"],
     "the way up and then the entries, each labelled the way the run prints it");
  eq(listing.rows.map((r) => r.path),
     ["C:\\", "C:\\work\\My Projects", "C:\\work\\notes.txt", "C:\\work\\sub"],
     "and each row carries the path the run built, not one this page joined");
  eq(listing.rows[0].title, "up to C:\\", "the way up says where it goes");
  eq(listing.note, "3 entries", "the count is the route's own");
  eq(page.dirRows({ entries: [{ name: "only", line: "only/", path: "a/only", dir: true }] }).note,
     "1 entry", "and one entry is not `1 entries`");
  eq(page.dirRows({ total: 5000, shown: 2000, entries: [] }).note, "the first 2000 of 5000 entries",
     "a capped listing says what it is rather than pretending to be the whole directory");
  eq(page.dirRows({}).rows.length, 0, "an answer with nothing in it draws nothing");
  eq(page.dirRows(null).rows.length, 0, "including no answer at all");

  // ...and what the panel draws: a button per row, a directory marked as one, and the label the run
  // gave rather than this page's reassembly of `name` and `/`.
  page.paintDir({
    path: "C:\\work",
    parent: "C:\\",
    total: 3,
    shown: 3,
    entries: [
      { name: "My Projects", line: "My Projects/", path: "C:\\work\\My Projects", dir: true, size: 0 },
      { name: "notes.txt", line: "notes.txt  (12 bytes)", path: "C:\\work\\notes.txt", dir: false, size: 12 },
    ],
  });
  const drawn = page.__node("preview-text").children;
  eq(drawn.map((r) => r.className), ["path dir up", "path dir", "path file"],
     "the way up, the directory and the file, each drawn as what it is");
  eq(drawn.map((r) => r.children.map((n) => n.textContent).join("")),
     ["..", "My Projects/", "notes.txt  (12 bytes)"],
     "labelled the way the run prints them, space and all");
  eq(drawn.map((r) => r.title), ["up to C:\\", "C:\\work\\My Projects", "C:\\work\\notes.txt"],
     "and each one names the whole path a press will read");
  eq(page.__node("preview-note").textContent, "3 entries", "the note is the listing's own");
  eq(page.__node("preview-render").hidden, true, "a listing has no rendered reading");
  eq(page.__node("preview-md").hidden, true, "and no Markdown one either");

  // The press. What it must do is ask about the path the *run* built -- which is the whole reason a
  // directory with a space in its name works here -- and the panel's head is drawn before the read,
  // so it is visible without a fetch behind it.
  page.fire(drawn[1], "click");
  eq(page.__node("preview").hidden, false, "the panel is on screen");
  eq(page.__node("preview-path").children.map((n) => n.textContent).join(""),
     "C:\\work\\My Projects", "and its head shows the whole path, space included");
});

check("a listing in a tool result is read by its own rows, which is where a space survives", () => {
  // The `list` tool's output, which is the shape a person presses paths in. `My Projects/` is two
  // tokens to the splitter and one name to the run, and the line's own mark is the evidence: this is
  // the one place in the page where a bare name with a space can be known to end where it ends.
  const listed = "My Projects/\nnotes.txt  (12 bytes)\nApplication Data  (0 bytes)\n";
  const parts = viewer.addressParts(listed, false);
  eq(parts.filter((p) => p.path !== undefined).map((p) => p.path),
     ["My Projects", "notes.txt", "Application Data"],
     "each row is one path, spaces and all");
  eq(parts.filter((p) => p.path !== undefined).map((p) => p.written),
     ["My Projects", "notes.txt", "Application Data"],
     "and the button says the name, which is the row's own first half");
  eq(
    parts.map((p) => (p.path !== undefined ? p.written : p.text)).join(""),
    listed,
    "nothing is eaten: the directory's `/`, the sizes and the breaks are all still there"
  );
  // A row is only read where relative names are read at all: prose is a sentence, and `My Projects/`
  // in one is two words that happen to end in a slash.
  eq(viewer.addressParts("in My Projects/ we keep it", true).filter((p) => p.path !== undefined).length,
     0, "a sentence is not a listing");
  // And outside a listing, a bare path with a space is asked about rather than guessed at: the run is
  // the only reader here with a filesystem, and its answer is what makes the name one name (see the
  // next check). With no run behind the page the token keeps its own reading, which is the honest
  // residue -- `C:\My` is a link to something that does not exist rather than to nothing.
  eq(viewer.addressParts("wrote C:\\My Projects\\notes.txt", false).filter((p) => p.path !== undefined)
       .map((p) => p.path), ["C:\\My", "Projects\\notes.txt"],
     "with nothing to ask, a bare spaced path is still split -- quoted, it is one");
  eq(viewer.addressParts('"C:\\My Projects\\notes.txt"', false).filter((p) => p.path !== undefined)
       .map((p) => p.path), ["C:\\My Projects\\notes.txt"], "which is what the quotes are for");
});

check("a path the reader could not have seen whole is resolved by the run", () => {
  // Reported directly, 2026-09-23: `C:\Users\zhangzhuo\My Documents` still could not be recognised --
  // a real directory, and two tokens to the splitter. The page asks the run and draws the button over
  // exactly the characters the answer names, so the words after the name stay words.
  const line = "see C:\\Users\\me\\My Documents is where it lives";
  const at = line.indexOf("C:\\");
  const end = at + "C:\\Users\\me\\My".length;
  const asked = viewer.askablePath(line, at, end);
  eq(asked, line.slice(at), "a drive-rooted token with words after it is worth asking about");
  eq(viewer.askablePath("in C:\\work\\x", 3, 12), null, "a token at the end of the line is not");
  eq(viewer.askablePath("in src/My Dir", 3, 9), null, "and a relative one is not: in prose it is words");

  // The run's answer, as `GET /resolve` gives it: how many characters of that text the path took.
  const used = asked.indexOf(" is ");
  viewer.pathEnds.set(asked, { path: "C:\\Users\\me\\My Documents", used });
  const parts = viewer.addressParts(line, true);
  eq(parts.filter((p) => p.path !== undefined).map((p) => p.written),
     ["C:\\Users\\me\\My Documents"], "one button, over the whole name");
  eq(parts.filter((p) => p.path !== undefined).map((p) => p.path),
     ["C:\\Users\\me\\My Documents"], "and a press asks for the name the run found");
  eq(parts.map((p) => (p.path !== undefined ? p.written : p.text)).join(""), line,
     "with every word of the sentence still on the page");
  eq(parts.filter((p) => p.line !== undefined && p.line > 0).length, 0, "no line was invented");

  // A `:N` inside the name the run found is the line, exactly as it is for every other path here.
  viewer.pathEnds.set(asked, { path: "C:\\Users\\me\\My Documents", used: used });
  const numbered = "at " + "C:\\Users\\me\\My Documents\\notes.txt:12 and then";
  const nAt = numbered.indexOf("C:\\");
  const nAsked = viewer.askablePath(numbered, nAt, nAt + "C:\\Users\\me\\My".length);
  viewer.pathEnds.set(nAsked, { path: "C:\\Users\\me\\My Documents\\notes.txt", used: nAsked.indexOf(" and") });
  const numberedParts = viewer.addressParts(numbered, true);
  eq(numberedParts.filter((p) => p.path !== undefined).map((p) => p.written),
     ["C:\\Users\\me\\My Documents\\notes.txt:12"], "the button says the name and the line");
  eq(numberedParts.filter((p) => p.path !== undefined).map((p) => p.line),
     [12], "and the press goes to the line inside the name");

  // A refusal is an answer too -- nothing there -- and it leaves the token exactly as it was: this page
  // does not turn a "no" into a longer name.
  viewer.pathEnds.set(asked, null);
  eq(viewer.addressParts(line, true).filter((p) => p.path !== undefined).map((p) => p.written),
     ["C:\\Users\\me\\My"], "a refusal changes nothing");
  // So is an answer this page cannot read: the button it would have drawn is the one it already draws.
  viewer.pathEnds.set(asked, { path: "C:\\whatever", used: 0 });
  eq(viewer.addressParts(line, true).filter((p) => p.path !== undefined).map((p) => p.written),
     ["C:\\Users\\me\\My"], "and so does one that used nothing");

  viewer.pathEnds.delete(asked);
  viewer.pathEnds.delete(nAsked);
});

check("a directory is opened where it lives, which is what pressing one means", () => {
  // Reported directly, 2026-09-23: *a press should open the directory with the machine's own way of
  // opening one, rather than previewing the files in it.* The route asked is the whole of it -- the
  // harness records what the page sent -- and `/open` is the one that starts a program.
  const before = viewer.sent.length;
  viewer.openPreview("C:\\work\\My Projects\\", 0);
  const sent = viewer.sent.slice(before);
  eq(sent.length, 1, "one press, one request");
  eq(sent[0].route, "/open", "and it is the route that opens a path where it lives");
  eq(JSON.parse(sent[0].body), { path: "C:\\work\\My Projects\\" },
     "carrying the path as it was written, space and separator and all");
  eq(sent.filter((s) => s.route.indexOf("/dir") === 0).length, 0,
     "nothing is listed: the panel is not the place a directory press goes");
  // A path that does not say it is a directory is not opened by a press: it goes to the panel, which
  // is how the run gets to say it is one (`X-Flint-Dir`) before anything is handed to the desktop.
  // The head is drawn before the read, so it is visible with no run behind the page.
  const second = viewer.sent.length;
  // The stub's `textContent = ""` does not clear children, so the head from an earlier check is
  // cleared by hand here: this is about what *this* press drew.
  viewer.__node("preview-path").children.length = 0;
  viewer.openPreview("C:\\work\\notes.txt", 0);
  eq(viewer.sent.length, second, "a press on a file starts nothing");
  eq(viewer.__node("preview").hidden, false, "it goes to the panel, which is the thing that opens");
  eq(viewer.__node("preview-path").children.map((n) => n.textContent).join(""),
     "C:\\work\\notes.txt", "with the path it was pressed for on its head");
});

check("a file is drawn line by line, with the file's own numbers in a gutter", () => {
  const pre = viewer.__node("preview-text");
  // The whole panel, as the browser paints it: what a line's number is, and what a line's text is.
  const drawn = () =>
    pre.children.map((row) => ({
      number: row.children[0].textContent,
      code: row.children[1].textContent,
      hidden: row.children[0]["aria-hidden"],
    }));
  const showed = (body) => {
    viewer.showLines(pre, body);
    return { rows: drawn(), className: pre.className, firstChild: pre.children.length };
  };

  let view = showed("one\ntwo\nthree\n");
  eq(view.rows.length, 3, "a trailing newline ends the last line rather than opening an empty one");
  eq(view.rows.map((r) => r.number), ["1", "2", "3"], "the numbers are the file's own, from one");
  eq(view.rows.map((r) => r.code), ["one", "two", "three"], "and the text is the line, without its break");
  eq(view.rows.every((r) => r.hidden === "true"), true, "a number is hidden from a screen reader");
  eq(view.className, "", "a narrow file needs no wider gutter than the base three characters");

  // A file whose last line has no newline is the same three lines, and a blank line in the middle is
  // a line: it has a number, which is the whole reason a `grep` hit's number can be trusted.
  eq(showed("one\ntwo\nthree").rows.length, 3, "a file that does not end in a break");
  eq(showed("one\n\nthree").rows.map((r) => r.code), ["one", "", "three"], "a blank line is a line");
  eq(showed("one\r\ntwo\r\n").rows.length, 2, "CRLF is one break, like the Markdown reading reads it");
  eq(showed("one\rtwo").rows.length, 2, "and so is a lone CR");
  eq(showed("").rows.length, 0, "no bytes, no lines");
  eq(showed(null).rows.length, 0, "and no body at all is not a line either");

  // The gutter's width comes from how many digits the last line's number has, because a number that
  // does not fit pushes the code right and breaks the column every other row shares.
  eq(showed("x\n".repeat(9)).className, "", "nine lines fit the base width");
  eq(showed("x\n".repeat(1000)).className, "lines-4", "a thousand lines ask for four digits");
  eq(showed("x\n".repeat(100000)).className, "lines-6", "and a hundred thousand for six");
  eq(showed("one\n").className, "", "and a short file again takes the base width back");

  // A sentence where the file would be: no rows, so nothing looks like a numbered line of a file.
  viewer.showPlain(pre, "nothing at C:\\gone.txt");
  eq(pre.children.length, 0, "a refusal has no gutter");
  eq(pre.textContent, "nothing at C:\\gone.txt", "and is the route's sentence, as it is");
  eq(pre.className, "", "with the wide gutter of a long file let go of");
});

check("opening at a line scrolls to that line's own row, whatever is above it", () => {
  const pre = viewer.__node("preview-text");
  viewer.showLines(pre, "one\ntwo\nthree\nfour\n");
  // The geometry a stub has to be given, because it has no layout: the container starts 30px down the
  // page and the four rows start 0, 20, 90 and 200px inside it. The third row is 70px tall rather than
  // one line's worth, which is a *wrapped* line -- and that is the case the old arithmetic got wrong:
  // it scrolled by `(line - 1) * lineHeight` (60px for the fourth line here), which lands short of the
  // target the moment anything above it takes more than one visual line.
  pre.offsetTop = 30;
  pre.children.forEach((row, at) => {
    row.offsetTop = 30 + [0, 20, 90, 200][at];
  });
  pre.scrollTop = 999;
  viewer.scrollToLine(pre, 4);
  eq(pre.scrollTop, 200, "the fourth row's own top, not three line-heights down (that would be 60)");
  viewer.scrollToLine(pre, 1);
  eq(pre.scrollTop, 0, "the first line is the top of the file, never a negative scroll");
  // A path with no line (a plain press) leaves the scroll where it is: `0` is "no line", which is not
  // "line zero". The container is put back to a known position, because the point is that nothing moved.
  pre.scrollTop = 55;
  viewer.scrollToLine(pre, 0);
  eq(pre.scrollTop, 55, "a press with no line does not move the panel");
});

console.log("the run's jobs");

check("a job is read off the route, and a row that is not a job is dropped", () => {
  // The route's shape, in full: what the page reads and what it refuses to guess at. `jobs_listed`
  // in the run sorts running-first and newest-first, and this list keeps that order rather than
  // re-sorting -- two orders for one list is one order too many.
  const listed = viewer.jobsFrom({
    jobs: [
      {
        pid: 41288, kind: "command", label: "cargo build --release",
        status: "running", detail: "", started_secs: 1789290356, ended_secs: null,
        path: "C:\\work\\.flint\\spill\\s\\background-bash-1.log",
      },
      {
        pid: 41290, kind: "child", label: "write a summary of src/web.rs",
        status: "completed", detail: "exit code 0 (finished)", started_secs: 1789290000,
        ended_secs: 1789290042, path: "C:\\work\\.flint\\sessions\\x\\children\\a.jsonl",
      },
    ],
  });
  eq(listed.length, 2, "both jobs, in the order the run listed them");
  eq(listed[0].kind, "command", "a command stays a command");
  eq(listed[0].ended, 0, "a job that has not ended has no end time");
  eq(listed[1].kind, "child", "a child stays a child");
  eq(listed[1].detail, "exit code 0 (finished)", "the fact sentence travels with the word");

  // A kind the page does not know is drawn as a child rather than dropped: the row is still a job,
  // and a job nobody can see is the one thing the list must not do.
  eq(viewer.jobsFrom({ jobs: [{ pid: 1, kind: "martian" }] })[0].kind, "child", "an unknown kind is still a job");
  eq(viewer.jobsFrom({ jobs: [] }).length, 0, "no jobs is an empty list");
  eq(viewer.jobsFrom(null).length, 0, "no answer at all is an empty list");
  eq(viewer.jobsFrom({ jobs: [{ kind: "child" }, null, "x"] }).length, 0, "a row with no pid is not a job");
});

check("a job's line says whether it is still going, and how long for", () => {
  // The two words, and the clock. `nowSecs` is handed in, so "three minutes" means three minutes
  // here without this check waiting for three minutes.
  const running = { status: "running", started: 1000, ended: 0 };
  eq(viewer.jobWhen(running, 1000), "running for 0s", "just started");
  eq(viewer.jobWhen(running, 1003), "running for 3s", "and it counts");
  eq(viewer.jobWhen({ status: "running", started: 1000, ended: 0 }, 1072), "running for 1m 12s", "past a minute");
  eq(
    viewer.jobWhen({ status: "completed", started: 1000, ended: 1004 }, 9999),
    "took 4s",
    "a duration that has stopped says so, and does not grow with the clock"
  );
  eq(
    viewer.jobWhen({ status: "failed", started: 1000, ended: 1003 }, 9999),
    "took 3s",
    "how long a failure took is worth knowing too"
  );
  // A job whose times are missing is not given an invented duration: it says the status word it
  // came with. A page that guessed would be the second answer to a question the route answered.
  eq(viewer.jobWhen({ status: "killed", started: 0, ended: 0 }, 9999), "killed", "no times, no number");
  eq(viewer.durationLabel(0), "0s", "seconds");
  eq(viewer.durationLabel(59), "59s", "the last second before a minute");
  eq(viewer.durationLabel(60), "1m 00s", "a minute, with its seconds padded");
  eq(viewer.durationLabel(3599), "59m 59s", "the last second before an hour");
  eq(viewer.durationLabel(3600), "1h 00m", "an hour");
  eq(viewer.durationLabel(86399), "23h 59m", "and a long build's duration");
  eq(viewer.durationLabel(-5), "0s", "a clock that is behind is not a negative duration");
  eq(viewer.jobIsLive({ status: "running" }), true, "running is live");
  eq(viewer.jobIsLive({ status: "completed" }), false, "and nothing else is");
});

// What the header says while the run is working, which is not one number. Three questions get asked
// of a run that has started something -- is any of it still moving, is any of it a *subagent* rather
// than a command, and has anything already ended badly -- and a total answers none of them. The
// failure chip is the one that matters most: a job that failed and a job that finished are the same
// count in a total, and the row behind the chip is the only place the exit code is legible.
check("the header's chips say what kind of work is running, and never fold a failure into done", () => {
  const chips = () => (viewer.__node("jobs-summary").children || []);
  const words = () => chips().map((c) => c.children[1].textContent);
  const kinds = () => chips().map((c) => c.className);

  viewer.paintJobs([
    { pid: 1, kind: "command", label: "cargo build --release", status: "running", started: 1000, ended: 0 },
    { pid: 2, kind: "child", label: "write a summary", status: "running", started: 1001, ended: 0 },
    { pid: 3, kind: "child", label: "reading src", status: "running", started: 1002, ended: 0 },
    { pid: 4, kind: "command", label: "the one that worked", status: "completed", started: 900, ended: 902 },
    { pid: 5, kind: "child", label: "the one that did not", status: "failed", started: 800, ended: 801 },
    { pid: 6, kind: "command", label: "stopped by the person", status: "killed", started: 700, ended: 701 },
  ]);
  eq(viewer.__node("jobs").hidden, false, "there is work, so the control is there");
  eq(words().join(" · "), "3 running · 2 subagents · 2 failed · 1 done", "the kinds, and the failure kept out of done");
  eq(kinds()[1], "chip child", "a subagent chip is its own kind, not a job count");
  eq(kinds()[2], "chip failed", "and a failure is its own chip");
  // The dot and the count are one control, and the sentence behind it is the long form: a chip is
  // read at a glance, and "1 failed" is not enough to say what to do about it.
  eq(chips()[2].title.indexOf("exit codes") >= 0, true, "the failed chip says where the reason is");

  // All of it stopping is a different fact from all of it running, and the chip says which.
  viewer.paintJobs([
    { pid: 1, kind: "command", label: "cargo build", status: "stopping", started: 1000, ended: 0 },
    { pid: 2, kind: "command", label: "the other one", status: "stopping", started: 1001, ended: 0 },
  ]);
  eq(words().join(" · "), "2 stopping", "nothing is running once everything is on its way out");

  // Nothing left, and the control goes with it: a chip that says "0" is a control nobody presses.
  viewer.paintJobs([]);
  eq(viewer.__node("jobs").hidden, true, "no jobs, no control");
  eq(words().length, 0, "and no chips behind it");
});

// The name in the header. Three sources and one order, and each of the three is a case a person will
// actually meet: a conversation they named, one they did not (which should show its own opening words
// rather than the product's name), and a page with nothing behind it at all.
check("the header's name is the conversation's, not the product's", () => {
  eq(viewer.titleWords({ title: "the crash in web.rs" }, "whatever the list said"), "the crash in web.rs",
     "a name in force wins over the list");
  eq(viewer.titleWords({}, "why does dsh fail to start"), "why does dsh fail to start",
     "an unnamed conversation shows what it opened with, not `flint`");
  eq(viewer.titleWords({}, ""), "flint", "and a page with neither says what it is");
  eq(viewer.titleWords(null, null), "flint", "including one that has not been loaded yet");
});

// The picture half of the preview panel. The route is a second read of the same path -- the bytes
// instead of the text -- and the panel decides which to ask for from the name alone, because it
// cannot sniff a file it has not fetched. So the two things worth holding here are that decision and
// the drawing: which names make `/image` worth asking, and what the panel does with the answer.
check("a picture is asked for by name, and let go of when it is replaced", () => {
  // The extension decision: a guess the route then confirms or refuses, so the cost of being wrong in
  // either direction is one wasted request rather than a wrong answer.
  ["cell.png", "photo.JPEG", "anim.gif", "modern.webp", "old.bmp", "favicon.ico", "scan.tiff",
   "phone.avif", "phone.heic", "logo.svg", "a/b/c/name.PNG"].forEach((name) => {
    eq(viewer.imageExt(name), true, name + " is worth asking about");
  });
  ["notes.txt", "README.md", "main.rs", "no-extension", "archive.tar.gz", "", "dot."].forEach((name) => {
    eq(viewer.imageExt(name), false, name + " is not a picture by its name");
  });
  eq(viewer.imageExt("~/shots/one.png"), true, "a tilde path's name is still its name");

  // The route: the same encoding rule as the text one, because a path is a path.
  eq(viewer.imageRoute("C:\\shots\\one two.png"), "/image?path=C%3A%5Cshots%5Cone%20two.png",
     "the picture route encodes the path the way the route decodes it");

  // Drawing it. The note is the *route's* type rather than the extension, for the reason the route
  // sniffs: a `.png` that is really a JPEG must not be described as a PNG here either.
  const response = { headers: { get: (name) => (name === "Content-Type" ? "image/png" : name === "Content-Length" ? "2411724" : null) } };
  viewer.showPicture("blob:http://127.0.0.1:7777/1", response);
  eq(viewer.__node("preview-image").src, "blob:http://127.0.0.1:7777/1", "the image is the blob URL");
  eq(viewer.__node("preview-image-button").hidden, false, "the picture's own control is shown");
  eq(viewer.__node("preview-text").hidden, true, "and the text pane steps out of its way");
  eq(viewer.__node("preview-note").textContent, "image/png · 2 MB", "the note is the route's own type and size");

  // Replaced: the URL that was on screen is revoked exactly once, which is the page's own memory
  // rather than the run's -- and is the one thing here that would leak without being said out loud.
  viewer.showPicture("blob:http://127.0.0.1:7777/2", response);
  eq(viewer.__node("preview-image").src, "blob:http://127.0.0.1:7777/2", "the second picture is drawn");
  eq(
    viewer.urls.revoked.join(","),
    "blob:http://127.0.0.1:7777/1",
    "the first picture's URL was let go of when the second arrived"
  );

  // Cleared, which is what closing the panel does: no picture, no URL, and the text pane back.
  viewer.showPicture(null);
  eq(viewer.__node("preview-image-button").hidden, true, "no picture, no control");
  eq(viewer.__node("preview-text").hidden, false, "the text pane is where a path reads again");
  eq(
    viewer.urls.revoked.join(","),
    "blob:http://127.0.0.1:7777/1,blob:http://127.0.0.1:7777/2",
    "and closing lets go of the last one"
  );
});

console.log("the settings dialog");

// The dialog is the one overlay on this page that is *modal*, and everything asserted here is the
// difference between a modal and a panel that happens to be on screen: it says it is one, it takes
// the keyboard when it opens, it gives the keyboard back when it closes, one section shows at a time,
// and one Escape closes the thing in front rather than everything at once.
check("the dialog opens and closes as a dialog, and the keyboard goes with it", () => {
  const settings = viewer.__node("settings");
  const mask = viewer.__node("settings-mask");
  const close = viewer.__node("settings-close");
  const door = viewer.__node("settings-open");

  // As the document ships it: shut, with nothing behind it. A dialog that is open on load is a page
  // that greets you with its settings.
  eq(settings.hidden, true, "settings start closed");
  eq(mask.hidden, true, "and so does the mask");
  eq(viewer.settingsOpen(), false, "the page agrees it is closed");

  viewer.openSettings();
  eq(settings.hidden, false, "one press opens it");
  eq(mask.hidden, false, "and the mask behind it, so a press outside can close it");
  eq(viewer.settingsOpen(), true, "the page agrees it is open");
  eq(close.focused, true, "the keyboard lands on the way out, not on the page underneath");

  close.focused = false;
  door.focused = false;
  viewer.closeSettings();
  eq(settings.hidden, true, "closing hides it");
  eq(mask.hidden, true, "and the mask");
  eq(door.focused, true, "and the keyboard goes back to the door it came from");
  eq(viewer.settingsOpen(), false, "the page agrees again");
});

// One screen at a time, and the rail is where the choice is made. The names and the order are the
// page's own -- a screen is a place this page put things -- while what is *inside* a screen comes
// from the frame. Which is why the six are pinned here: a screen the page names and the frame never
// files anything on is a heading over an empty pane, and `tests/web_view.rs` holds the other side of
// that pair.
check("the rail offers one screen per place, and shows one at a time", () => {
  // The dialog's own name is the rail's first child, so the cells live in the list under it: the
  // checks below walk the list and not the nav, which is what the page does too (`showSettingsPane`).
  const rail = viewer.__node("settings-list");
  eq(
    viewer.SETTINGS_PANES.map(([key]) => key),
    ["model", "run", "limits", "tools", "conversation", "work"],
    "the screens, in the order a person reads them: what answers, then how it works"
  );
  for (const [, label, note] of viewer.SETTINGS_PANES) {
    ok(label && note, "every screen has a name and a line saying what is on it: " + label);
  }

  viewer.openSettings();
  const first = viewer.SETTINGS_PANES[0][0];
  eq(rail.children.length, viewer.SETTINGS_PANES.length, "one button per screen");
  eq(rail.children.map((b) => b.textContent), viewer.SETTINGS_PANES.map(([, label]) => label),
     "named in the page's own words");
  eq(viewer.__node("pane-" + first).hidden, false, "the first screen is the one shown");
  eq(viewer.__node("pane-run").hidden, true, "and the others are not");

  viewer.showSettingsPane("work");
  eq(viewer.__node("pane-work").hidden, false, "another screen opens");
  eq(viewer.__node("pane-" + first).hidden, true, "and the first closes -- one at a time, not a column");
  eq(rail.children[5].getAttribute("aria-current"), "true", "the rail marks the one in force");
  eq(rail.children[0].getAttribute("aria-current"), "false", "and unmarks the one that was");

  // A name the rail does not offer leaves the dialog as it was: showing nothing at all would be a
  // blank settings pane, which reads as a page that has lost its settings.
  viewer.showSettingsPane("a-screen-this-page-never-offered");
  eq(viewer.__node("pane-work").hidden, false, "an unknown screen changes nothing");
  viewer.closeSettings();
});

// The Escape order, which is the one thing a second overlay made ambiguous: with the dialog over the
// preview, one press must put away the dialog and leave the panel alone.
check("one Escape closes the thing in front, and the dialog is in front of the panel", () => {
  const settings = viewer.__node("settings");
  const preview = viewer.__node("preview");

  // Both open: the dialog is the one being used, so it is the one that goes. The panel is opened the
  // way the page opens it -- `openPreview` sets its path and unhides it before it awaits the route,
  // which is all this check needs from it.
  viewer.openSettings();
  viewer.openPreview("C:\\notes.txt", 0);
  eq(preview.hidden, false, "the panel is open to begin with");
  eq(viewer.dismissTopmost(), true, "a press with something open does something");
  eq(settings.hidden, true, "the dialog closed");
  eq(preview.hidden, false, "and the panel behind it did not");

  // Now the panel is the only thing left, and the next press takes it.
  eq(viewer.dismissTopmost(), true, "the press still does something");
  eq(preview.hidden, true, "the panel closed on its own press");

  // Nothing open: Escape is not a control that pretends to work.
  eq(viewer.dismissTopmost(), false, "with nothing open, Escape is not an action");
});

console.log("the / menu in the composer");

// A frame with one row of each class, so that every branch of the dispatch is reachable -- the menu's
// whole job is to treat five classes differently, and a check with four of them would leave the one
// that matters (a form, which must never fill the line) unexercised.
// The frame the menu is built from, as the process sends it: every row carries the screen the frame
// filed it on, because that is what the menu's hand-offs read to open the dialog at the right place.
// `target` is for the one check that has to drive the page's *own* document -- the dialog's painters
// read it, and a detached one would draw into the same nodes and then be painted over.
const menuFrame = (page, target) => {
  const d = target || page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel", group: "limits" },
      { label: "/reload", send: "/reload", help: "re-read the config file", class: "button", group: "run" },
      { label: "/provider <name>", send: "/provider", help: "switch endpoint", class: "selector",
        group: "model", values: ["stub", "other"] },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector",
        group: "conversation" },
      { label: "/provider key <key>", send: "/provider key", help: "save a key for one",
        class: "form", group: "model", fields: [{ field: "password" }] },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger",
        group: "conversation" },
    ],
  }));
  return d;
};

// When the menu is open at all. The rule is strict on purpose: a slash in the middle of a sentence is
// not a command, and a menu that stayed open through `/config set a b` would cover the line being
// written.
check("the menu opens on a slash that starts the line, and on nothing else", () => {
  eq(viewer.menuQuery("/"), "", "a bare slash asks about everything");
  eq(viewer.menuQuery("/pro"), "pro", "and letters after it are the query");
  eq(viewer.menuQuery("  /skill"), "skill", "leading space is not part of the line yet");
  eq(viewer.menuQuery("/config set"), null, "a space means the line is being written");
  eq(viewer.menuQuery("see src/main.rs and/or docs"), null, "a slash mid-sentence is not a command");
  eq(viewer.menuQuery("hello"), null, "and neither is text");
  eq(viewer.menuQuery(""), null, "an empty box asks nothing");
  eq(viewer.menuQuery(null), null, "and a missing value does not throw");
});

// The order, which is what makes a menu learnable: a prefix first, then a subsequence of the command
// itself, then a word in its help -- and the frame's own order as the tie-break.
check("the menu filters by prefix, then by subsequence, then by the help text", () => {
  const page = loadViewer();
  const d = menuFrame(page);
  const names = (query) => page.menuRows(d, query).map((c) => c.send);

  eq(names(""), ["/config", "/reload", "/provider", "/resume", "/provider key", "/delete"],
     "a bare slash is the frame's own order");
  eq(names("re")[0], "/reload", "a prefix beats everything: /re wins over /provider and /resume");
  eq(names("pkey"), ["/provider key"], "a subsequence finds a two-word command");
  eq(names("delete"), ["/delete"], "and an exact name is a match");
  eq(names("proxy"), ["/config"], "the help text is the last chance: 'show shell, steps, proxy'");
  eq(names("zzz"), [], "nothing matches nothing");
  ok(names("p").includes("/provider") && names("p").includes("/provider key"),
     "both commands a letter matches are offered: " + names("p").join(", "));
  // A subsequence over the help would match almost anything a person types; that is why the help is a
  // substring and the *name* is the subsequence. "rmv" is a subsequence of "remove one of them" and
  // must not reach `/delete`.
  eq(page.menuScore({ send: "/delete", help: "remove one of them" }, "rmv"), 0, "help is not fuzzy");
  ok(page.menuScore({ send: "/delete", help: "remove one of them" }, "remove") > 0,
     "but a substring of the help does match");
});

// The tie-break, which the bare `/` is made of entirely: every row scores the same, so what decides is
// the order the menu *draws* in -- by class, this page's own reading order -- and not the frame's. The
// fixture above is already grouped by class, which is why this needs a frame of its own: the process's
// table is not, and with the frame's order as the tie-break the marked row is not the top row on screen.
check("a tie is broken by the order the menu draws in, not by the frame's", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel", group: "limits" },
      { label: "/config set <key> <value>", send: "/config set", help: "change one setting",
        class: "form", group: "limits" },
      { label: "/reload", send: "/reload", help: "re-read the config file", class: "button", group: "run" },
    ],
  }));
  const names = (query) => page.menuRows(d, query).map((c) => c.send);
  eq(names(""), ["/config", "/reload", "/config set"], "a bare slash reads in the drawn order");
  // And the score still wins outright over it: `/config set` is drawn last and comes back first, which
  // is the palette's promise and the reason the class is only the tie-break.
  eq(names("config set")[0], "/config set", "a query that names a row beats the drawing order");
});

check("the menu draws the frame's rows, marked, and says when nothing matches", () => {
  const page = loadViewer();
  const d = menuFrame(page);
  page.showMenu(d, "");
  const box = page.__node("menu");
  eq(box.hidden, false, "the menu is shown");
  const headings = box.children.filter((c) => c.className === "group-name").map((c) => c.textContent);
  eq(headings, ["reports", "actions", "selectors", "forms", "destructive"],
     "the same reading order as the dialog's list");
  const rows = box.children.filter((c) => String(c.className).includes("row"));
  eq(rows.length, 6, "one row per command");
  eq(rows[0].getAttribute("aria-selected"), "true", "the keyboard starts on the first row");
  eq(rows[1].getAttribute("aria-selected"), "false", "and only one row is marked");

  // The arrows move one row at a time and wrap, which is the whole reason this is a menu rather than a
  // list of buttons: `menu.at` counts rows, so the group headings between them are skipped.
  page.menuStep(d, 1);
  eq(rows[1].getAttribute("aria-selected"), "true", "ArrowDown moves the mark");
  page.menuStep(d, -1);
  eq(rows[0].getAttribute("aria-selected"), "true", "ArrowUp moves it back");
  page.menuStep(d, -1);
  eq(rows[5].getAttribute("aria-selected"), "true", "and it wraps rather than sticking");

  page.showMenu(d, "zzz");
  eq(page.__node("menu").children[0].textContent, "no command matches /zzz",
     "a query with no answer says so rather than drawing an empty box");
  eq(page.menuOpen(), true, "and the menu stays open, so a backspace brings the list back");

  page.showMenu(d, "");
  page.hideMenu();
  eq(page.__node("menu").hidden, true, "hiding it closes the box");
  eq(page.menuOpen(), false, "and the page agrees it is closed");
});

// A page with no run has no commands, and a menu drawn from nothing is a promise the page cannot keep
// -- the same rule the settings door follows.
check("a page with no frame offers no menu", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.showMenu(d, "");
  eq(page.__node("menu").hidden, true, "no state, no menu");
  eq(page.menuOpen(), false, "and nothing is open");
});

// The dispatch, one class at a time, and it is checked as a *decision* first: which of the five things
// a row does is a function of the row's class, and the rule that matters lives here -- a `form` row
// never completes the line, because the composer's text is sent to the run and written into the
// session file. A credential typed there is a credential on disk.
check("what a row commits to is decided by its class, and only its class", () => {
  const page = loadViewer();
  const d = menuFrame(page);
  const dispatch = (query) => page.menuDispatch(page.menuRows(d, query)[0]);
  eq(dispatch("config"), "report", "a report is read");
  eq(dispatch("reload"), "line", "an action completes the line");
  eq(dispatch("resume"), "line", "a selector whose list is elsewhere completes the line");
  eq(dispatch("provider key"), "dialog", "a form opens the dialog and never the line");
  eq(dispatch("delete"), "line", "a destructive row completes the line");
  eq(page.menuDispatch({ class: "value", send: "3" }), "value", "and a value completes the line it was asked for");
  eq(page.menuDispatch(null), "none", "nothing to take is not a dispatch");
  const withValues = page.menuRows(d, "provider").find((c) => c.class === "selector" && c.values);
  eq(page.menuDispatch(withValues), "values", "a selector the frame gave values for offers them");
  // The order matters as much as the word: a form that also carried `values` must still be a dialog,
  // because the class is what the process said the command *is*.
  eq(page.menuDispatch({ class: "form", send: "/x", values: ["a"] }), "dialog",
     "a form with values is still a form");
});

// The doing, and every observable half of it happens before the first `await` -- which is why this
// check is synchronous and still sees the request on the wire, the dialog open and the line written.
check("taking a row does exactly what its dispatch says", () => {
  const page = loadViewer();
  const d = menuFrame(page, page.doc);
  const take = (query, row) => page.takeMenuRow(d, row || page.menuRows(d, query)[0]);
  // The composer starts empty, and saying so is not a formality: "a form row does not touch the line"
  // is an assertion about the *absence* of a value, and a stub box whose `value` was never written is
  // `undefined`, which would pass for the wrong reason.
  page.setComposerText("");
  eq(page.__node("message").value, "", "the line starts where the person left it");

  // A report: the reading is drawn in the dialog's commands section, and the line is untouched. The
  // *route* is not asserted here -- `canSend` is false in this stub, as it is for a dropped session
  // file, so the page never gets as far as a fetch -- which is why `menuDispatch` above is the claim
  // made here and a real listener is where the request is checked (`scripts/browser-controls-test.js`).
  //
  // The box is filled with the *query* first, because that is the state a real press happens in -- and
  // it is what caught the defect in a real browser: the query was left behind, so the next Enter would
  // have sent the very line the report route exists to keep out of the transcript.
  page.setComposerText("/config");
  page.showMenu(d, "config");
  take("config");
  eq(page.__node("message").value, "", "the query the menu was built from is cleared, not sent");
  eq(page.settingsOpen(), true, "the reading is shown in the dialog");
  eq(page.__node("pane-limits").hidden, false, "on the screen the frame filed that row on");
  page.closeSettings();

  // An action: the line, completed, and nothing sent. A keystroke in a menu should not decide anything.
  page.sent.length = 0;
  page.showMenu(d, "reload");
  take("reload");
  eq(page.__node("message").value, "/reload", "an action completes the line");
  eq(page.sent.length, 0, "and sends nothing");
  eq(page.menuOpen(), false, "the menu closes behind it");

  // A selector with values: the line so far, then the frame's own values as a second list.
  page.showMenu(d, "provider");
  take("provider", page.menuRows(d, "provider").find((c) => c.class === "selector" && c.values));
  eq(page.__node("message").value, "/provider ", "a selector is completed with a space for its argument");
  const values = page.__node("menu").children.filter((c) => c.getAttribute("data-row") !== null);
  eq(values.map((r) => r.children[0].textContent), ["stub", "other"], "and the values are offered");
  take(null, { class: "value", send: "other", label: "other" });
  eq(page.__node("message").value, "/provider other", "taking one completes the whole line");
  eq(page.sent.length, 0, "...and still sends nothing");

  // A selector whose list lives elsewhere on the page: the line, with room for the argument.
  page.showMenu(d, "resume");
  take("resume");
  eq(page.__node("message").value, "/resume ", "a selector with no values still leaves room");

  // The form: the dialog, at the row, with the line left empty. Asserted as the *absence* of the
  // credential rather than as the presence of a dialog, because the absence is the property -- and the
  // box is filled with the query first, which is the state the press really happens in.
  page.setComposerText("/provider key");
  page.showMenu(d, "provider key");
  const form = page.menuRows(d, "provider key")[0];
  eq(form.class, "form", "the row under test is a form");
  take(null, form);
  eq(page.__node("message").value, "", "a form row leaves the line empty, not holding the query");
  eq(page.settingsOpen(), true, "it opens the dialog");
  eq(page.__node("pane-model").hidden, false, "at the screen the row's subject belongs to");
  const marked = rowsOf(page, "model")
    .filter((r) => String(r.className).includes("pointed"))
    .map((r) => r.textContent);
  eq(marked.length, 1, "and the row it was about is marked on that screen: " + JSON.stringify(marked));
  page.closeSettings();

  // A destructive row: the line, completed, one press short of doing anything.
  page.sent.length = 0;
  page.showMenu(d, "delete");
  take("delete");
  eq(page.__node("message").value, "/delete ", "a destructive row completes the line");
  eq(page.sent.length, 0, "and sends nothing at all");
});

// One Escape, and the menu is in front of everything: it lives in the composer, which is where the
// keyboard already is.
check("Escape puts the menu away before anything else", () => {
  const page = loadViewer();
  const d = menuFrame(page);
  page.showMenu(d, "");
  page.openSettings();
  eq(page.dismissTopmost(), true, "the press does something");
  eq(page.menuOpen(), false, "the menu closed");
  eq(page.settingsOpen(), true, "and the dialog behind it did not");
  eq(page.dismissTopmost(), true, "the next press");
  eq(page.settingsOpen(), false, "closes the dialog");
});

console.log("the Markdown reading in the preview");

// Which files are read as Markdown, and which are not. By name, like the picture route: a `.md` that is
// really a log should read as a log, and the switch is one press away in both directions.
check("a file is Markdown by its own name", () => {
  for (const name of ["AGENTS.md", "C:\\x\\HANDOFF.MD", "docs/a.markdown", "b.mkd", "c.mdx"]) {
    eq(viewer.isMarkdown(name), true, name + " is Markdown");
  }
  for (const name of ["notes.txt", "a.png", "readme", "md", "a.md.txt", ""]) {
    eq(viewer.isMarkdown(name), false, JSON.stringify(name) + " is not Markdown");
  }
  eq(viewer.isMarkdown(null), false, "and a missing path is not Markdown either");
});

// The reading itself: lines in, blocks out. No DOM, which is the whole reason this half can be checked
// here rather than in a browser.
check("lines become blocks: headings, fences, lists, quotes, rules and paragraphs", () => {
  const blocks = viewer.markdownBlocks([
    "# The title",
    "",
    "Some prose that is",
    "wrapped over two lines.",
    "",
    "## A section ##",
    "",
    "- one",
    "- two",
    "  - nested",
    "",
    "1. first",
    "2. second",
    "",
    "> quoted words",
    "> on two lines",
    "",
    "---",
    "",
    "```js",
    "const a = 1;",
    "```",
  ].join("\n"));

  eq(blocks.map((b) => b.kind),
     ["heading", "paragraph", "heading", "list", "list", "quote", "rule", "code"],
     "every block, in order");
  eq(blocks[0], { kind: "heading", level: 1, text: "The title" }, "an ATX heading keeps its level");
  eq(blocks[1].text, "Some prose that is wrapped over two lines.",
     "a paragraph joins its wrapped lines with a space");
  eq(blocks[2].level, 2, "and a closing run of hashes is not part of the text");
  eq(blocks[3].ordered, false, "a bulleted list");
  eq(blocks[3].items.map((i) => i.text), ["one", "two"], "with one item per marker");
  eq(blocks[3].items[1].children.map((c) => c.kind), ["list"], "and an indented item holds a nested list");
  eq(blocks[3].items[1].children[0].items[0].text, "nested", "which is a list of its own");
  eq(blocks[4].ordered, true, "a numbered list is its own kind");
  eq(blocks[5].blocks.map((b) => b.text), ["quoted words on two lines"],
     "a quote is a block, and its lines are joined like a paragraph's");
  eq(blocks[7].info, "js", "a fence keeps the language it named");
  eq(blocks[7].lines, ["const a = 1;"], "and its body verbatim");
});

// The two shapes that are easy to get wrong and that these files are full of: a numbered list under a
// bulleted one is a *new* list, and a fence inside an item belongs to the item.
check("a list ends where a reader would say it ends", () => {
  const sep = viewer.markdownBlocks("- bullet\n1. numbered\n- bullet again");
  eq(sep.map((b) => b.kind + (b.ordered ? "(ol)" : "(ul)")), ["list(ul)", "list(ol)", "list(ul)"],
     "a change of marker kind starts a new list");

  const loose = viewer.markdownBlocks("- one\n\n- two");
  eq(loose.length, 1, "a blank line between two items is one loose list, not two");
  eq(loose[0].items.length, 2, "and both items are in it");

  const split = viewer.markdownBlocks("- one\n\nafter the list");
  eq(split.map((b) => b.kind), ["list", "paragraph"],
     "but a blank line before prose ends the list");

  const item = viewer.markdownBlocks("- item\n\n  ```\n  code\n  ```");
  eq(item.length, 1, "the fence is part of the item, not a block after the list");
  eq(item[0].items[0].children.map((c) => c.kind), ["code"], "and it is read as code");
});

// A pipe table, which is what `docs/features.md` and `AGENTS.md` are made of.
check("a pipe table becomes a table, with the alignment its divider asks for", () => {
  const blocks = viewer.markdownBlocks([
    "| Area | Control | What |",
    "|---|---:|:---:|",
    "| header | the name | not a control |",
    "| settings | a picker | sends a line |",
  ].join("\n"));
  eq(blocks.length, 1, "one table, not four paragraphs");
  eq(blocks[0].kind, "table", "and it is a table");
  eq(blocks[0].head.map((c) => c.text), ["Area", "Control", "What"], "with its head cells");
  eq(blocks[0].align, ["", "right", "center"], "and the alignment its divider named");
  eq(blocks[0].rows.length, 2, "two body rows");
  eq(blocks[0].rows[0], ["header", "the name", "not a control"], "each split on its pipes");

  // A line with a pipe and no divider under it is prose: a table is only a table when it says so, and
  // guessing is how a paragraph about `a | b` would lose its words.
  const prose = viewer.markdownBlocks("this | that\nnot a table");
  eq(prose.length, 1, "a pipe in a paragraph is not a table");
  eq(prose[0].kind, "paragraph", "it is a paragraph");
});

// Nothing is guessed and nothing is dropped: setext headings, indented code, raw HTML and a fence that
// was never closed all come out as *something*, and the fallback is always the words themselves.
check("what the renderer does not know, it does not eat", () => {
  const setext = viewer.markdownBlocks("A title\n=======\n\nAnother\n-------");
  eq(setext.map((b) => b.kind + b.level), ["heading1", "heading2"], "setext headings are read");
  eq(setext[0].text, "A title", "with the line above as the text");

  const html = viewer.markdownBlocks("<script>alert(1)</script>\n\n<img src=x onerror=y>");
  eq(html.length, 2, "raw HTML is left as its own paragraphs");
  eq(html[0].text, "<script>alert(1)</script>", "and the characters are the text");
  eq(html[1].text, "<img src=x onerror=y>", "for a tag that would have run code in another page");

  const unclosed = viewer.markdownBlocks("```\nno closing fence");
  eq(unclosed.length, 1, "a fence that never closed is still a code block");
  eq(unclosed[0].lines, ["no closing fence"], "with everything that followed it inside");

  const indent = viewer.markdownBlocks("    four spaces");
  eq(indent[0].kind, "paragraph", "indented text is read as text rather than dropped");
  eq(viewer.markdownBlocks("").length, 0, "an empty file has no blocks");
  eq(viewer.markdownBlocks(null).length, 0, "and neither has a missing one");
});

// The painter: what the blocks become as nodes, read through the same stub DOM every other check uses.
check("the blocks become nodes, and every string arrives as text", () => {
  const holder = viewer.__node("preview-md");
  viewer.paintMarkdown(holder, viewer.markdownBlocks([
    "# Title",
    "",
    "**bold** and `code` stay as written",
    "",
    "- a",
    "  - b",
    "",
    "```py",
    "x = 1",
    "```",
    "",
    "| a | b |",
    "|---|---|",
    "| 1 | 2 |",
  ].join("\n")));
  const tags = holder.children.map((n) => n.tag);
  eq(tags, ["h1", "p", "ul", "pre", "table"], "one node per block, of the right kind");
  eq(holder.children[0].textContent, "Title", "a heading's text is its text");
  // The decision that inline syntax is not parsed shows up here as a fact rather than as a promise: the
  // emphasis and the backticks are the characters the file holds.
  eq(holder.children[1].textContent, "**bold** and `code` stay as written",
     "inline syntax is not parsed, so it stays readable");
  eq(holder.children[2].children.length, 1, "one list item");
  eq(holder.children[2].children[0].children.map((n) => n.tag), ["ul"],
     "with the nested list inside it");
  eq(holder.children[3].children.map((n) => n.tag + ":" + n.textContent),
     ["div:py", "code:x = 1"], "a fence keeps its language and its body");
  eq(holder.children[4].children.map((n) => n.tag), ["thead", "tbody"], "a table has a head and a body");
  // Nothing the page draws can become markup: `el` sets `textContent`, and the policy test forbids the
  // assignment functions outright, so this is the shape of the guarantee rather than a second check of it.
  eq(holder.children[0].children.length, 0, "and a heading is a node with text, not markup");
});

console.log("the preview's two views");

// The raw/rendered switch: which view a file opens in, what the button says, and which container is
// showing. `line` is the whole rule -- a file opened at a line opens raw, because the line is why it was
// opened at all and "line 412" has no meaning in a rendered view.
check("a Markdown file opens rendered, and a line opens raw", () => {
  eq(viewer.previewView("C:\\notes.md", 0), "rendered", "a Markdown file with no line opens rendered");
  eq(viewer.previewView("C:\\notes.md", 412), "source", "the same file opened at a line opens raw");
  eq(viewer.previewView("C:\\notes.txt", 0), "source", "a file that is not Markdown opens raw");
  eq(viewer.previewView("C:\\notes.txt", 3), "source", "and stays raw at a line");
  eq(viewer.previewView("C:\\shot.png", 0), "source", "a picture has no rendered view to open in");
  eq(viewer.previewView(null, 0), "source", "and a missing path is read as text");
});

// The header's own headers, as the route really sends them: `previewNote` reads the cut and the size out
// of them, so a fake response says there was no cut.
const noCut = { headers: { get: () => null } };

check("the switch is offered only where there is a choice, and it says what it does", () => {
  const button = viewer.__node("preview-render");
  const text = viewer.__node("preview-text");
  const rendered = viewer.__node("preview-md");
  const note = viewer.__node("preview-note");

  viewer.paintPreviewView("# Title\n\n- one\n", noCut, "rendered", "C:\\notes.md", 0);
  eq(button.hidden, false, "a Markdown file gets the switch");
  eq(rendered.hidden, false, "the rendered view is showing");
  eq(text.hidden, true, "and the raw one is not");
  eq(button.textContent, "source", "the button says what pressing it does: show the source");
  eq(button.getAttribute("aria-pressed"), "true", "and that the rendered view is in force");
  ok(note.textContent.includes("rendered"), "the note says which view is showing: " + note.textContent);
  eq(rendered.children.length, 2, "and the blocks are in it");

  viewer.paintPreviewView("# Title\n", noCut, "source", "C:\\notes.md", 412);
  eq(button.textContent, "rendered", "opened at a line, the button offers the rendering");
  eq(button.getAttribute("aria-pressed"), "false", "and nothing rendered is in force");
  eq(text.hidden, false, "the raw text is showing");
  // The bytes, read off the lines rather than off the container: what is between the code and the
  // gutter is the file, and the container's own `textContent` is the numbers run together with it --
  // which is exactly what the gutter must *not* be part of when a block is copied out of the panel.
  eq(text.children.map((row) => row.children[1].textContent).join("\n") + "\n", "# Title\n", "and it is the file's own bytes");
  eq(text.children.map((row) => row.children[0].textContent), ["1"], "with the file's own numbering");
  eq(rendered.hidden, true, "with the rendered view shut");
  eq(note.textContent, "line 412", "the note still says the line");

  viewer.paintPreviewView("plain words\n", noCut, "source", "C:\\notes.txt", 0);
  eq(button.hidden, true, "a file that is not Markdown is offered no switch");
  eq(rendered.hidden, true, "and nothing rendered");
  eq(text.hidden, false, "the text is what shows");
});

// A refusal is not a file either: the route's own sentence is the whole answer, and the switch belongs to
// a reading that happened.
check("a refusal takes the switch away with it", () => {
  const button = viewer.__node("preview-render");
  viewer.paintPreviewView("# Title\n", noCut, "rendered", "C:\\notes.md", 0);
  eq(button.hidden, false, "the switch is there for the file");
  viewer.paintPreviewRefusal("cannot read it: not there", 404);
  eq(button.hidden, true, "and goes when the route refused");
  eq(viewer.__node("preview-md").hidden, true, "with nothing rendered behind it");
  eq(viewer.__node("preview-note").textContent, "HTTP 404", "the refusal is the note");
  eq(viewer.__node("preview-text").textContent, "cannot read it: not there",
     "and the route's sentence is what is shown");
});

// An exported page carries its conversation in a JSON island, and this is the one function that reads
// it. The island is a list of the session file's own lines, so what comes out of it goes straight into
// `applyText` -- which is why an export draws like a session somebody dropped on the page, and why
// there is nothing here to test about drawing. What is worth pinning is the two ways it can say
// nothing: no island at all (the page as it has always been, empty and waiting for a drop), and an
// island that cannot be read (a broken export, which the boot says out loud rather than drawing as an
// empty conversation).
check("the island an exported page carries", () => {
  const lines = [`{"type":"meta","id":"1"}`, `{"type":"chat","message":{"role":"user","content":"hi"}}`];
  eq(
    viewer.islandLines({ textContent: JSON.stringify({ lines }) }),
    lines.join("\n"),
    "the conversation comes back as the lines applyText reads"
  );
  eq(viewer.islandLines(null), "", "a page with no island carries no conversation");
  eq(viewer.islandLines({ textContent: "" }), "", "and neither does an empty one");
  eq(viewer.islandLines({ textContent: "not json at all" }), "", "damage is not thrown, it is refused");
  eq(viewer.islandLines({ textContent: `{"lines":"a string"}` }), "", "lines that are not a list are refused");
  // The escape the export writes: a conversation cannot close the element it is written into, and the
  // text that comes back is the conversation, not the escape.
  const nasty = `</script><script>alert(1)</script>`;
  const escaped = JSON.stringify({ lines: [nasty] }).replace(/</g, "\\u003c").replace(/>/g, "\\u003e");
  eq(viewer.islandLines({ textContent: escaped }), nasty, "an escaped island still reads exactly");
});

console.log("the file's own lines, sent to catch a page up");

// A reconnect is answered from the session file when the ring cannot cover the cursor, and those
// lines describe moments the page may already have drawn from the stream. Both guards below are
// about that overlap: without them a caught-up page shows a question twice and an answer twice.
check("a question the stream already drew is not drawn again from the file", () => {
  const d = viewer.applyText(
    viewer.newDoc(),
    `{"type":"chat","message":{"role":"user","content":"the same question"}}`
  );
  viewer.applyLine(
    d,
    `{"type":"chat","message":{"role":"user","content":"the same question"}}`,
    true
  );
  eq(kinds(d).filter((k) => k === "user").length, 1, "one question, not two");
  // The same line from the *stream* is not deduped: a person asking twice asked twice.
  viewer.applyLine(d, `{"type":"chat","message":{"role":"user","content":"the same question"}}`);
  eq(kinds(d).filter((k) => k === "user").length, 2, "a stream line is never treated as a repeat");
});

check("an answer built from deltas is filled in from the file, not opened twice", () => {
  const d = viewer.newDoc();
  viewer.applyLine(d, `{"type":"turn.started","prompt":"a question"}`);
  viewer.applyLine(d, `{"type":"message.delta","text":"half an ans"}`);
  viewer.applyLine(
    d,
    `{"type":"chat","message":{"role":"assistant","content":"half an answer, whole","reasoning":"why"}}`,
    true
  );
  eq(kinds(d).filter((k) => k === "assistant").length, 1, "one answer block");
  const last = d.blocks[d.blocks.length - 1];
  eq(last.text, "half an answer, whole", "the file's copy is the one that stands");
  eq(last.reasoning, "why", "and its reasoning with it");
  eq(last.open, false, "the block is finished");
});

// ROADMAP.md section 11 item 8: a reload must rebuild from the file, so what the page can hold
// honestly is where this tab was reading -- and what it can do with that is say what arrived while it
// was closed, or that the position does not fit the file it is looking at. The cases are checked
// against the page's own functions rather than against its text, because the text said the right
// words in a version of this that also said the wrong thing when nothing had been missed.
const hint = () => viewer.__node("hint").textContent || "";

check("closing the page remembers the conversation and the position it had drawn to", () => {
  const store = viewer.storage;
  store.removeItem(viewer.SEEN_KEY);
  eq(viewer.remembered(), null, "nothing remembered before there is anything to remember");
  viewer.remember("1789290356-957", 4096);
  eq(viewer.remembered().id, "1789290356-957", "the conversation");
  eq(viewer.remembered().at, 4096, "the position");
  // A conversation the page does not know is not worth a pair: a pair with no id could be compared
  // against the next conversation's file, which is the one thing this must never do.
  viewer.remember("", 4096);
  eq(viewer.remembered().id, "1789290356-957", "an id-less write leaves the pair alone");
  store.removeItem(viewer.SEEN_KEY);
});

check("a page that comes back to the same conversation says what arrived while it was closed", () => {
  viewer.storage.removeItem(viewer.SEEN_KEY);
  viewer.notePosition({ id: "s-1", at: 1000 }, 4000, "s-1");
  ok(hint().includes("3000 bytes arrived while this page was closed"), `the gap is not said: ${hint()}`);
  // Nothing arrived, so there is nothing to say -- and the check is that the page said *nothing new*,
  // not that the hint is empty: a hint belongs to whatever spoke last, and this is not its turn.
  const said = hint();
  viewer.notePosition({ id: "s-1", at: 4000 }, 4000, "s-1");
  eq(hint(), said, "a page that missed nothing said something");
  // A different conversation is not a loss, so it is not an occasion for a message either.
  viewer.notePosition({ id: "s-1", at: 1000 }, 4000, "s-2");
  eq(hint(), said, "another conversation was reported as a gap");
});

check("a position past the end of the file is a stale read, said and not thrown", () => {
  viewer.notePosition({ id: "s-1", at: 9000 }, 4000, "s-1");
  ok(hint().includes("is not this one"), `a stale position was not reported: ${hint()}`);
  ok(hint().includes("9000") && hint().includes("4000"), `the two numbers are the fact: ${hint()}`);
  // The page carries on: the conversation it just read from the file is on screen either way.
  const d = viewer.newDoc();
  viewer.applyText(d, SESSION);
  eq(kinds(d), ["system", "user", "tool", "assistant"], "a stale pair does not cost the conversation");
});

// ROADMAP.md section 11 item 9(ii): the page's `/say` could not address one run because a pid typed
// into a text field is prose. What makes the picker possible is two things the frame says -- the flag
// an answer follows, and the list its choices come from -- and both are checked here against the page's
// own functions rather than against its text.
const SAY_FIELDS = [
  { field: "text", flag: "--to", from: "peers", name: "pid", optional: true },
  { field: "text", name: "text", optional: false },
];

check("a flagged answer is written after its flag, and left out whole", () => {
  eq(
    viewer.formLine("/say", SAY_FIELDS, ["41288", "the words"]),
    "/say --to 41288 the words",
    "the addressed line"
  );
  // The address is optional *and first*, which a positional answer cannot be: `--to 41288` is a
  // phrase, so leaving it out leaves no hole for the words to fall into.
  eq(viewer.formLine("/say", SAY_FIELDS, ["", "the words"]), "/say the words", "the broadcast line");
  eq(viewer.formLine("/say", SAY_FIELDS, ["   ", "the words"]), "/say the words", "whitespace is empty");
  // And a required answer still refuses: the bare command is a different thing -- for `/say` it is the
  // usage line -- so the page must not send it by accident.
  eq(viewer.formLine("/say", SAY_FIELDS, ["41288", ""]), null, "no words");
  // The positional rows are unchanged, including the optional one that has to stay last.
  const addFields = [
    { field: "text", name: "name", optional: false },
    { field: "text", name: "base_url", optional: false },
    { field: "text", name: "model", optional: true },
  ];
  eq(
    viewer.formLine("/provider add", addFields, ["claw", "http://127.0.0.1:8080/v1", ""]),
    "/provider add claw http://127.0.0.1:8080/v1",
    "an optional trailing answer"
  );
});

check("the peer picker sends a pid and shows who it is", () => {
  const select = viewer.__node("say-peer-probe");
  viewer.fillPeerSelect(
    select,
    [
      { pid: 41288, model: "deepseek-chat", age_secs: 3 },
      { pid: 41999, model: "qwen3", readonly: true, age_secs: 0 },
    ],
    "(everyone here)"
  );
  const options = select.children.slice();
  eq(options.length, 3, "the broadcast and two runs");
  // The first option is the default and it means the broadcast: the row did that before it could
  // address one, and losing it would be a regression wearing a feature's clothes.
  eq(options[0].value, "", "the broadcast is what is sent when nothing is chosen");
  eq(options[0].selected, true, "and it is chosen");
  eq(options[1].value, "41288", "the pid is what is sent");
  eq(options[1].textContent, "pid 41288 · deepseek-chat · last said so 3s ago", "and who that is");
  eq(options[2].textContent, "pid 41999 · qwen3 · read-only · here now", "a readonly run says so");
  eq(select.value, "", "the picker starts on the broadcast");
  // Nobody else here is an answer, not an empty control: the message waits in the file either way.
  viewer.fillPeerSelect(select, [], "(nobody else is here — it waits in the file for the next run)");
  eq(select.firstChild.textContent, "(nobody else is here — it waits in the file for the next run)", "the empty case");
  eq(select.firstChild.value, "", "and it is still the broadcast");
});

console.log("cutting a branch from an answer");

// A conversation the page drew, and the frame that names its questions. Both are needed for a branch
// button: the value to send comes from the frame (`/fork`'s `values`), and *where* to draw it comes
// from the page's own transcript, tied to the value by the question's first line (`labels`).
//
// The fold is why the tie is by text and not by counting: after `/compact` the run holds fewer
// questions than the file has `chat` lines, so the page's Nth question and the run's Nth question are
// not the same question -- and the page cannot know how many were folded away without reading the
// run's history, which it may not do. The lists are paired from the end, where they agree, and each
// pairing is believed only when the turn's own text is the question the label names.
const forkFrame = (page, target, labels, values) => {
  const d = target || page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/fork [n]", send: "/fork", help: "start a new conversation cut at question n",
        class: "selector", group: "conversation", values: values || labels.map((_, i) => String(i + 1)),
        labels },
    ],
  }));
  return d;
};
const chat = (role, content) =>
  `{"type":"chat","message":{"role":"${role}","content":${JSON.stringify(content)}}}`;
// Two questions asked and answered. The transcript the page has when there is nothing else in the
// file: no fold, no import, and the newest question is the one at the bottom.
const TWO_TURNS = [
  chat("user", "why does the socket close early"),
  chat("assistant", "because the peer half-closes"),
  chat("user", "what about the retry path"),
  chat("assistant", "it retries twice, then gives up"),
].join("\n");

check("a question's number is labelled with the question, in the settings list too", () => {
  const page = loadViewer();
  const d = forkFrame(page, null, ["why does the socket close early", "what about the retry path"]);
  page.showState(d);
  const rows = rowsOf(page, "conversation");
  eq(labelsOf(page, "conversation"), ["/fork 1", "/fork 2"], "one row per question, by number");
  // The second half of the row is the question itself rather than the help printed twice: "start a
  // new conversation cut at question n" under `/fork 1` and again under `/fork 2` says nothing about
  // which question is which, which is the whole of what this row is for.
  eq(
    rows.map((row) => String(row.children[1].textContent)),
    ["why does the socket close early", "what about the retry path"],
    "each value says what it is of"
  );
});

check("the answers a value cuts from carry the button, and the rest do not", () => {
  const page = loadViewer();
  const d = forkFrame(page, page.applyText(page.newDoc(), TWO_TURNS),
    ["why does the socket close early", "what about the retry path"]);
  page.paint(d);
  const turns = page.__node("doc").children.filter((n) => n.className.indexOf("turn ") === 0);
  eq(turns.length, 4, "two questions and two answers");
  const tools = turns.map((turn) => turn.children.find((child) => child.className === "turn-tools"));
  // The button is under the *first* answer: `/fork 2` cuts in front of the second question, so the
  // branch keeps everything up to and including the first answer. The newest answer has none --
  // cutting in front of nothing is not a cut at all -- and neither has the first question's, because
  // the value that would name it (`/fork 1`) keeps no answer: the run has held nothing before it.
  eq(tools.map((bar) => (bar ? bar.children[0].textContent : null)),
    [null, "fork from here", null, null], "one button, under the answer it keeps");
  const button = tools[1].children[0];
  eq(button.title.indexOf("keeps everything up to and including this answer") > 0, true, "what it does");
  eq(button.title.indexOf("what about the retry path") > 0, true, "and which question it cuts in front of");
  // The line the mark composes: the frame's own `send` and one of the values it offered, like every
  // other control on this page. The press itself is checked in a browser
  // (`scripts/browser-controls-test.js`), where a click is real.
  eq(page.branchPoints(d).get(1).line, "/fork 2", "the line a press would send");
});

check("a state frame is enough on its own to draw the cut it carries", () => {
  // The frame is what names the questions, so it is what draws the buttons -- and a page that has
  // just loaded reads the conversation and then waits for exactly this frame. Drawing the buttons
  // only on the next transcript event would mean a freshly opened page showed no way to cut from an
  // answer until an answer arrived, which is the one moment the buttons are wanted.
  const page = loadViewer();
  const d = page.applyText(page.newDoc(), TWO_TURNS);
  page.paint(d);
  const bar = () =>
    page.__node("doc").children[1].children.find((child) => child.className === "turn-tools");
  eq(bar(), undefined, "with no frame yet there is nothing to cut");
  forkFrame(page, d, ["why does the socket close early", "what about the retry path"]);
  ok(bar() !== undefined, "the frame that names the questions draws the button by itself");
  eq(bar().children[0].title.indexOf("what about the retry path") > 0, true, "and it names one");
});

check("a frame that offers no cut draws no buttons at all", () => {
  const page = loadViewer();
  const d = page.applyText(page.newDoc(), TWO_TURNS);
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [], commands: [],
  }));
  page.paint(d);
  eq(page.branchPoints(d).size, 0, "nothing to cut in front of");
  const tools = page.__node("doc").children.map((turn) =>
    turn.children.find((child) => child.className === "turn-tools"));
  eq(tools.filter(Boolean).length, 0, "and nothing is drawn");
});

check("a question the run no longer holds is not marked with the button above it", () => {
  // What `/compact` leaves: the file still has the folded question and its answer, and the run holds
  // the two after it plus the digest standing in for the first. The page cannot tell how much was
  // folded -- that is a fact about the run's history, not about the text on screen -- so it pairs the
  // question list from the end and believes a pairing only when the turn's text is the question the
  // label names. The button then lands on the answer the cut keeps, and the folded answer above it,
  // which the branch does not contain, is left alone: a button there would promise a branch holding
  // an answer it dropped.
  const page = loadViewer();
  const d = forkFrame(page, page.applyText(page.newDoc(), [
    chat("user", "the folded question"),
    chat("assistant", "the folded answer"),
    chat("user", "what about the retry path"),
    chat("assistant", "it retries twice, then gives up"),
    chat("user", "and the timeout"),
    chat("assistant", "thirty seconds"),
  ].join("\n")), ["what about the retry path", "and the timeout"]);
  page.paint(d);
  const turns = page.__node("doc").children;
  const marked = turns
    .map((turn, at) => {
      const bar = turn.children.find((child) => child.className === "turn-tools");
      return bar ? at + " " + bar.children[0].title : null;
    })
    .filter(Boolean);
  eq(marked.length, 1, "one cut, over a fold");
  ok(marked[0].indexOf("3 ") === 0, "the button is on the held answer, not the folded one");
  ok(marked[0].indexOf("and the timeout") > 0, "and it names the question it cuts in front of");
});

check("a turn in flight takes the buttons away rather than moving them", () => {
  const page = loadViewer();
  const d = forkFrame(page, page.applyText(page.newDoc(), TWO_TURNS),
    ["why does the socket close early", "what about the retry path"]);
  page.paint(d);
  page.applyLine(d, `{"type":"turn.started","prompt":"and the timeout"}`);
  page.paint(d);
  const tools = page.__node("doc").children.map((turn) =>
    turn.children.find((child) => child.className === "turn-tools"));
  // The frame is built between turns, so the run does not know the third question yet: its list is
  // one question behind the page's drawing, and pairing it from the end would put `/fork 2` on the
  // answer to the *first* question. Refusing every pairing is the honest drawing; the settings list
  // still names all three questions and works.
  eq(tools.filter(Boolean).length, 0, "no button is left pointing one question off");
});

check("two questions that read alike are not pointed at, and the others still are", () => {
  const page = loadViewer();
  const repeated = [
    chat("user", "carry on"),
    chat("assistant", "the first answer"),
    chat("user", "carry on"),
    chat("assistant", "the second answer"),
    chat("user", "and the tests"),
    chat("assistant", "they pass"),
  ].join("\n");
  const d = forkFrame(page, page.applyText(page.newDoc(), repeated),
    ["carry on", "carry on", "and the tests"]);
  page.paint(d);
  const turns = page.__node("doc").children;
  const marked = turns
    .map((turn, at) => (turn.children.find((c) => c.className === "turn-tools") ? at : -1))
    .filter((at) => at >= 0);
  // Only the third question's button: "carry on" appears twice, so a press cannot say which answer
  // the person meant, and the page says nothing rather than guessing. The value is still offered in
  // the dialog, where the numbers tell the two apart.
  eq(marked, [3], "the ambiguous question is skipped, the rest are marked");
  eq(turns[3].children.find((c) => c.className === "turn-tools").children[0].title.indexOf("and the tests") > 0,
    true, "and the button that is drawn names the right question");
});

check("a repaint leaves the buttons alone until the questions move", () => {
  const page = loadViewer();
  const d = forkFrame(page, page.applyText(page.newDoc(), TWO_TURNS),
    ["why does the socket close early", "what about the retry path"]);
  page.paint(d);
  const first = page.__node("doc").children[1];
  // Painting is incremental, and the mark is part of what a node was built from: a repaint that
  // rebuilt every answer would throw away a selection and an open reasoning box on each delta.
  page.paint(d);
  ok(page.__node("doc").children[1] === first, "an unchanged answer keeps its node");
  // A question asked since: `/fork 3` cuts in front of the new one, so the button under the second
  // answer is new, and the one under the first still means what it meant (`/fork 2`) and is left
  // alone -- the same node, not a rebuilt copy of it.
  page.applyLine(d, chat("user", "and the tests"));
  page.applyLine(d, chat("assistant", "they pass"));
  forkFrame(page, d, ["why does the socket close early", "what about the retry path", "and the tests"]);
  page.paint(d);
  const turns = page.__node("doc").children;
  const bar = (at) => turns[at].children.find((c) => c.className === "turn-tools");
  ok(turns[1] === first, "an answer whose cut did not move keeps its node");
  eq(bar(1).children[0].title.indexOf("what about the retry path") > 0, true, "and the same question");
  eq(bar(3).children[0].title.indexOf("and the tests") > 0, true, "the new cut names the new question");
  eq(bar(5), undefined, "the newest answer is never cut from");
});

check("a label the page cannot match is not a button", () => {
  eq(viewer.isTheQuestion("why does the socket close early", "why does the socket close early"), true,
    "the same question");
  eq(viewer.isTheQuestion("why does the socket close early?", "why does the socket close early"), false,
    "a longer question is a different question");
  eq(viewer.isTheQuestion("  why does the socket close early\n", "why does the socket close early"), true,
    "the label is trimmed, like the run's own preview");
  const long = "x".repeat(80);
  eq(viewer.isTheQuestion(long, "x".repeat(64) + " ..."), true, "a clipped first line");
  eq(viewer.isTheQuestion("x".repeat(64), "x".repeat(64) + " ..."), false,
    "the ellipsis means the question is longer than the label");
  eq(viewer.isTheQuestion("first line\nsecond line", "first line ..."), true,
    "more lines than the label shows is what the ellipsis says");
  eq(viewer.isTheQuestion("first line", "first line ..."), false,
    "a single short line is not a clipped one");
});

if (failures) {
  console.log(`\n${failures} failed`);
  process.exit(1);
}
console.log("\nall passed");
