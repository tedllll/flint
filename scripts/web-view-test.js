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
/// has to have them. Keeping a real parent pointer is the whole of it; there is still no layout,
/// no styling and no events.
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
    addEventListener() {},
    click() {},
  };
  return node;
}

function loadViewer() {
  const start = html.indexOf("<script>");
  const end = html.lastIndexOf("</script>");
  if (start < 0 || end < 0) throw new Error("web/view.html has no inline script");

  const nodes = new Map();
  const sandbox = {
    module: { exports: {} },
    console,
    FileReader: function FileReader() {},
    document: {
      getElementById(id) {
        if (!nodes.has(id)) nodes.set(id, fakeNode());
        return nodes.get(id);
      },
      // The tag is kept because a check sometimes has to ask what kind of node was drawn -- a report
      // row is a button and a selector row is not -- and there is no other way to tell two stubs
      // apart. The listeners are still not delivered: see the note on `addEventListener` below.
      createElement: (tag) => {
        const node = fakeNode();
        node.tag = tag;
        return node;
      },
      createTextNode: (t) => ({ text: t }),
      addEventListener() {},
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
  return api;
}

const viewer = loadViewer();
const doc = (text) => viewer.applyText(viewer.newDoc(), text);
const kinds = (d) => d.blocks.map((b) => b.kind);

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
  eq(rows[0].children[1].textContent, "bash\nread\nedit", "the answer is the body");
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

// The header's controls are drawn from the `state` frame, which is the page's only read channel:
// it may not read `config.toml` itself, because a second reader of the same state can disagree
// with the process -- the file says one thing while a run does another as soon as `--readonly` or
// `/readonly` is involved. What is pinned here is the half with an answer in it: options in, the
// option in force selected, and the states in which a picker should not be offered at all.
check("a state frame becomes the header's two pickers, set to what is in force", () => {
  const d = viewer.newDoc();
  eq(d.state, null, "a page with no frame has no state");
  viewer.showState(d);
  eq(viewer.__node("controls").hidden, true, "a document with no state offers no controls");

  viewer.applyState(d, JSON.stringify({
    type: "state",
    provider: "stub",
    model: "stub-other",
    readonly: false,
    verbose: "on",
    detail: false,
    providers: [
      { name: "stub", models: ["stub-model", "stub-other"] },
      { name: "other", models: [] },
    ],
  }));

  const providers = viewer.__node("pick-provider");
  const models = viewer.__node("pick-model");
  eq(providers.children.map((o) => o.value), ["stub", "other"], "the providers on offer");
  eq(providers.value, "stub", "the provider in force");
  eq(models.children.map((o) => o.value), ["stub-model", "stub-other"], "the models on offer");
  eq(models.value, "stub-other", "the model in force");
  eq(viewer.__node("controls").hidden, false, "the controls are offered once there is a state");
  eq(providers.disabled, false, "two providers is a choice");
});

// The case that made `fillSelect` more than three lines: `/model` offers a provider's own `model`
// whether or not it is repeated in `models`, so a frame can name a current value that is not among
// the alternatives. A `<select>` whose value is not one of its options keeps the *first* option
// instead, silently -- and then sends it as soon as anything else on the page is touched.
check("a value the frame does not list is still the one shown", () => {
  const d = viewer.newDoc();
  viewer.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "hand-written",
    providers: [{ name: "stub", models: ["stub-a", "stub-b"] }],
  }));
  const models = viewer.__node("pick-model");
  eq(models.children.map((o) => o.value), ["hand-written", "stub-a", "stub-b"], "options");
  eq(models.value, "hand-written", "the value in force");
});

check("a picker with one choice says so by being unusable", () => {
  const d = viewer.newDoc();
  viewer.applyState(d, JSON.stringify({
    type: "state", provider: "solo", model: "only",
    providers: [{ name: "solo", models: ["only"] }],
  }));
  eq(viewer.__node("pick-provider").disabled, true, "one provider is not a choice");
  eq(viewer.__node("pick-model").disabled, true, "one model is not a choice");
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
    type: "state", provider: "stub", model: "stub-model", providers: [{ name: "stub", models: [] }],
  }));
  meta.children.length = 0;
  viewer.paint(d);
  eq(meta.children.map((s) => s.textContent), ["cwd C:\\work"], "with a state, the pickers say it");
});

check("a warning and an error read differently", () => {
  const d = doc(`{"message":"careful","type":"warning"}\n{"message":"broke","type":"error"}`);
  eq(d.blocks.map((b) => b.level), ["warning", "error"], "levels");
});

// A toggle is a switch that shows its current value rather than a button that blind-toggles, and
// the value it shows has to be the one the *run* is on -- not the one the file last held, and not
// one this page remembers. So the names, the values a toggle can take and the value it is on all
// come from the frame, and the page adds nothing of its own: `/verbose` takes off|on|full today,
// and a second copy of that list here is how a switch comes to offer a word the command refuses.
check("each toggle in the frame becomes a switch showing what is in force", () => {
  const d = viewer.newDoc();
  viewer.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "stub-model",
    providers: [{ name: "stub", models: ["stub-model"] }],
    toggles: [
      { name: "verbose", values: ["off", "on", "full"], value: "full" },
      { name: "detail", values: ["off", "on"], value: "off" },
    ],
  }));

  const box = viewer.__node("toggles");
  eq(box.children.length, 2, "one switch per toggle");
  eq(box.children.map((l) => l.children[0].textContent), ["verbose", "detail"], "named for the command they send");
  const switches = box.children.map((l) => l.children[1]);
  eq(switches[0].children.map((o) => o.value), ["off", "on", "full"], "the values the command takes");
  eq(switches[0].value, "full", "the value in force");
  eq(switches[1].children.map((o) => o.value), ["off", "on"], "and the two-valued one");
  eq(switches[1].value, "off", "showing off, which a bare on/off button could not");
});

check("a state frame with no toggles in it takes the switches away", () => {
  const d = viewer.newDoc();
  viewer.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "stub-model", providers: [],
    toggles: [{ name: "verbose", values: ["off", "on", "full"], value: "on" }],
  }));
  eq(viewer.__node("toggles").children.length, 1, "a switch while the frame lists one");
  // An older or narrower frame: the switches must go rather than stay behind showing values that
  // nothing is reporting any more.
  viewer.applyState(d, JSON.stringify({ type: "state", provider: "stub", model: "stub-model", providers: [] }));
  eq(viewer.__node("toggles").children.length, 0, "none listed, none shown");
});

console.log("the panel of commands the frame describes");

// §8's read channel, second half: the command list is what a menu — buttons, forms, confirmations —
// is drawn from, and the page must not carry a copy of it. What is pinned here is the arrangement:
// one group per class the page was taught, in a fixed order, with each row saying what to type and
// what it does. The classes are §8's own, so the panel reads as the design does.
check("the frame's command list becomes a panel, grouped by class", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    toggles: [],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel" },
      { label: "/reload", send: "/reload", help: "re-read the config file", class: "button" },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector" },
      { label: "/name [text]", send: "/name", help: "name this conversation", class: "form" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger" },
    ],
  }));
  eq(page.__node("commands").hidden, false, "a frame that lists commands offers the panel");
  const groups = page.__node("command-list").children;
  eq(
    groups.map((g) => g.children[0].textContent),
    ["reports", "actions", "selectors", "forms", "destructive"],
    "one group per class, in the order §8 names them"
  );
  eq(
    groups[0].children.slice(1).map((r) => r.children.map((c) => c.textContent)),
    [["/config", "show shell, steps, proxy"]],
    "a row says what to type and what it does"
  );
  eq(groups[4].children.length, 2, "the destructive group carries its own rows");
});

check("a state frame with no commands in it takes the panel away", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.showState(d);
  eq(page.__node("commands").hidden, true, "a document with no state offers no menu");
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
  }));
  // A frame from a build that did not carry them, or one whose list is empty: either way the panel
  // goes rather than staying up with rows nothing is reporting any more.
  eq(page.__node("commands").hidden, true, "a frame without a command list offers no menu");
});

// §8's second class, and the one with a route of its own. A report is *read* here rather than sent
// into the transcript, because the terminal is where somebody typed `/help` and this page's reader
// did not ask for a listing there. What is pinned here is which rows can be read and what the panel
// does while one is: the line to ask for comes from the frame, and the answer arrives on the feed
// marked as a panel's. That a press posts to `/report` rather than `/message` -- the whole
// difference between reading and printing -- is asserted over the page's bytes in tests/web_view.rs
// with the other compositions, since the stub DOM delivers no events.
check("a report row is pressable and the other rows are not", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel" },
      { label: "/help", send: "/help", help: "this message", class: "panel" },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector" },
    ],
  }));
  const groups = page.__node("command-list").children;
  const reports = groups[0].children.slice(1);
  eq(reports.map((r) => r.tag), ["button", "button"], "a report row is a control");
  eq(reports[0].type, "button", "and not a submit button, which would reload the page");
  eq(
    reports[0].children.map((c) => c.textContent),
    ["/config", "show shell, steps, proxy"],
    "it still says what to type and what it does"
  );
  eq(
    groups[1].children[1].tag,
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
      { label: "/skills [name]", send: "/skills", help: "list skills", class: "panel", values: ["alpha", "beta"] },
      // A frame that carries no values is drawn as one row, so this check also says the value rows come
      // from the frame rather than from the class: the second command here is a panel row too.
      { label: "/config", send: "/config", help: "show shell", class: "panel" },
    ],
  }));
  const reports = page.__node("command-list").children[0].children.slice(1);
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
      { label: "/name [text]", send: "/name", help: "name this conversation", class: "form", field: "text" },
      { label: "/provider key <key>", send: "/provider key", help: "set the API key", class: "form", field: "password" },
      { label: "/config edit", send: "/config edit", help: "change shell", class: "form", field: "text" },
    ],
  }));
  const forms = page.__node("command-list").children[0].children.slice(1);
  eq(
    forms.map((n) => n.tag),
    ["form", "form", "form"],
    "every row the frame marks gets a field, whatever its label looks like"
  );
  eq(
    forms.map((f) => f.children[0].tag + ":" + f.children[0].type),
    ["input:text", "input:password", "input:text"],
    "the frame's word is the input's type, so a credential is masked because the process said so"
  );
  eq(
    forms.map((f) => f.children[1].textContent),
    ["/name", "/provider key", "/config edit"],
    "the button sends the row's own `send`, which is also what it says"
  );
  eq(forms[0].children[0].placeholder, "name this conversation", "the help is what the field suggests");
  // A form row the frame does *not* mark is still a row of reference, which is the half that says
  // the field comes from the frame rather than from the class.
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/name [text]", send: "/name", help: "name this conversation", class: "form", field: "text" },
      { label: "/config edit", send: "/config edit", help: "change shell", class: "form" },
    ],
  }));
  const mixed = page.__node("command-list").children[0].children.slice(1);
  eq(mixed.map((n) => n.tag), ["form", "div"], "the wizard stays a row of reference");
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
      { label: "/provider rm <name>", send: "/provider rm", help: "delete one", class: "danger", from: "providers" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger", from: "sessions" },
    ],
  }));
  const closed = page.__node("command-list").children[0].children.slice(1);
  eq(closed.map((n) => n.tag), ["button", "button"], "both rows are pressable");
  eq(
    closed.map(says),
    ["/provider rm <name>", "/delete <n|id>"],
    "and closing the choices sends nothing: the line is not on any row yet"
  );

  // Opened, on the list this page already holds in `state`. The row being pressed says the whole
  // line, which is the point of two presses rather than one: the second press is the one that can
  // be read before it is made.
  d.confirm = { send: "/provider rm" };
  page.showCommands(d);
  const open = page.__node("command-list").children[0].children.slice(1);
  // The open row's candidates *replace* that row inside the list, and the other destructive row is
  // still there below them: the panel is a list of what the terminal takes, and one row being open
  // is not a reason to hide the rest.
  eq(open.map((n) => n.tag), ["button", "button", "button", "button"], "a way back, two candidates, and the other row");
  eq(
    open.map(says),
    ["\u2039 commands", "/provider rm stub", "/provider rm other", "/delete <n|id>"],
    "each candidate is the line that would be sent"
  );
  eq(open[0].className, "back", "the way back is marked as one");
  eq(open[1].className, "row danger", "and the ones that send are marked as destructive");

  // The conversation row, on a page with no list to draw from -- which is the honest answer rather
  // than an empty list that looks like a list with nothing in it.
  d.confirm = { send: "/delete" };
  page.showCommands(d);
  const empty = page.__node("command-list").children[0].children.slice(1);
  eq(
    empty.map(says),
    ["/provider rm <name>", "\u2039 commands", "nothing to choose from"],
    "an empty list of candidates says so"
  );
});

check("a conversation's row carries its own actions behind one button", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m",
    providers: [{ name: "stub", models: ["m"] }],
    commands: [
      { label: "/archive <n|id>", send: "/archive", help: "file it away", class: "danger", from: "sessions" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger", from: "sessions" },
      { label: "/provider rm <name>", send: "/provider rm", help: "delete one", class: "danger", from: "providers" },
      { label: "/model <name>", send: "/model", help: "pick one", class: "selector" },
    ],
  }));
  const session = { n: 3, id: "173-9", label: "the branch", current: false };
  const says = (n) => (n.children[0] ? n.children[0].textContent : n.textContent);
  const menuOf = (row) => row.children.find((n) => n.className === "menu");

  const closed = page.sessionRow(d, session);
  eq(closed.tag, "li", "a conversation is a list item");
  eq(closed.children.map((n) => n.tag), ["span", "span", "button"], "the number, the label, and one button");
  eq(closed.children[2].textContent, "\u22ef", "the button is the three dots, without a word");
  eq(closed.children[2].title, "actions for this conversation", "and explains itself on hover");
  eq(menuOf(closed), undefined, "closed: no menu is drawn");

  // Opened, on the row the document names -- by id, because the numbers under a menu are positions
  // and a list that shifted under it would aim the next press at the wrong conversation.
  d.menu = { n: 3, id: "173-9" };
  const open = page.sessionRow(d, session);
  const menu = menuOf(open);
  if (!menu) throw new Error("the open row has no menu");
  eq(menu.children.map((n) => n.tag), ["button", "button"], "one row per action the frame offers");
  eq(menu.children.map(says), ["/archive 3", "/delete 3"], "each row is the line it will send");
  eq(menu.children.map((n) => n.className), ["row danger", "row danger"], "both of them destroy something");
  eq(
    menu.children.map((n) => n.children[1].textContent),
    ["file it away", "delete one"],
    "the description is the frame's own, not a word this page made up"
  );

  d.menu = { n: 3, id: "someone-else" };
  eq(menuOf(page.sessionRow(d, session)), undefined, "a menu belongs to the row it names");

  // The conversation you are in is offered them too, and the *terminal* is what refuses: its
  // refusal explains itself ("/new starts a fresh one; then this one can be filed away"), and a
  // page that hid the row would be deciding a rule it does not own.
  d.menu = { n: 1, id: "111-1" };
  const current = menuOf(page.sessionRow(d, { n: 1, id: "111-1", label: "this one", current: true }));
  eq(current.children.length, 2, "the open conversation is offered them as well");

  // A frame that offers no way to act on another conversation says so rather than drawing nothing:
  // an empty menu is indistinguishable from a menu that failed to draw.
  d.menu = { n: 3, id: "173-9" };
  page.applyState(d, JSON.stringify({ type: "state", provider: "stub", model: "m", commands: [] }));
  eq(menuOf(page.sessionRow(d, session)).children.map(says), ["nothing to do from here"], "an empty menu says so");
});

check("a row the panel cannot press says where its control is", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m",
    commands: [
      { label: "/tools", send: "/tools", help: "list available tools", class: "panel" },
      { label: "/new", send: "/new", help: "start a fresh conversation", class: "button" },
      { label: "/model <name>", send: "/model", help: "switch to one", class: "selector", values: ["stub-model", "stub-other"] },
      { label: "/resume <n|id>", send: "/resume", help: "switch to one of them", class: "selector" },
      { label: "/provider add", send: "/provider add", help: "set up a new provider", class: "form" },
    ],
  }));
  page.showCommands(d);
  const drawn = page.__node("command-list").children;
  // One group per class the frame sends, with the rows in the order the frame gave them. The
  // switches are not here because they are not in the frame at all: their value is already a field
  // of it, and one fact in two places is how the two come to disagree.
  const rows = drawn.filter((n) => n.className === "group").map((g) => g.children.slice(1));
  eq(rows.length, 4, "four groups, one per class the frame sends");
  const [reports, actions, selectors, forms] = rows;

  // The report row is pressable, and carries no "reference" mark.
  eq(reports[0].tag, "button", "a report is a control");
  eq(reports[0].className, "row", "and is not marked as reference");
  eq(reports[0].title, "list available tools", "and explains itself with the frame's own help, not with a home");

  // A row the frame gave `values` is pressable wherever it sits: the switch rows are offered the
  // names the run already knows, which is the whole point of them -- the reader should not have to
  // read a name off one control and type it into another.
  eq(selectors[0].tag, "button", "a switch with values is a control");
  eq(selectors[0].className, "row", "not a reference row");
  eq(selectors[0].title, "switch to one", "and keeps the frame's own help");
  eq(selectors[0].children.map((n) => n.textContent), ["/model stub-model", "switch to one"], "the first value, as the line it sends");
  eq(selectors[1].children.map((n) => n.textContent), ["/model stub-other", "switch to one"], "and the second");
  // ...while a selector the frame gave no values keeps saying where its own control is.
  eq(selectors[2].tag, "div", "the selector with nothing to offer is not pressable");
  eq(selectors[2].className, "row reference", "it is marked as reference");
  eq(selectors[2].title, "this one is a conversation on the left", "and names the one home it has");

  // Everything else is reference: a row that says what the command is, dressed so that it cannot be
  // mistaken for the control it is not -- and saying, on hover, where that control actually is.
  eq(actions[0].tag, "div", "an action is not pressable in the panel");
  eq(actions[0].className, "row reference", "it is marked as reference");
  eq(actions[0].title, "this one is a button in the header", "and says where its control is");
  eq(forms[0].title, "this one is typed in the terminal -- it asks questions", "a form without a field is the terminal's");
  eq(forms[0].children.map((n) => n.textContent), ["/provider add", "set up a new provider"], "and it still reads like a row");
});

check("a listing being read replaces the list, and the way back restores it", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [{ label: "/config", send: "/config", help: "show shell", class: "panel" }],
  }));
  d.reading = "/config";
  d.readingText = "config: /tmp/config.toml\n  verbose          = on";
  page.showCommands(d);
  const drawn = page.__node("command-list").children;
  eq(drawn[0].tag, "button", "the way back is a control");
  eq(drawn[1].textContent, "/config", "the heading is what was asked for");
  eq(drawn[2].textContent.includes("config.toml"), true, "and the listing is the process's own text");
  // The list is gone while a listing is on screen: one surface, one reading -- and the answer that
  // arrives later is put there by the frame, not appended to a list nobody is looking at.
  eq(drawn.length, 3, "the list is replaced rather than added to");
  d.reading = null;
  page.showCommands(d);
  const back = page.__node("command-list").children;
  eq(back.length, 1, "going back draws the groups again");
  eq(back[0].children.length, 2, "with the report row in it");
});

check("a report frame fills the panel only for what is being read", () => {
  const page = loadViewer();
  const d = page.newDoc();
  d.reading = "/tools";
  d.readingText = "";
  page.applyLine(d, JSON.stringify({ type: "command", input: "/config", text: "shell = bash", panel: true }));
  eq(d.readingText, "", "an answer to a listing the reader moved on from is not shown");
  eq(d.blocks.length, 0, "and a report is not a transcript block either");
  page.applyLine(d, JSON.stringify({ type: "command", input: "/tools", text: "read, write", panel: true }));
  eq(d.readingText, "read, write", "the answer to what is being read fills the panel");
  eq(d.blocks.length, 0, "still nothing in the transcript: that is the whole point of the class");
});

check("a command answer without the panel mark is still a transcript block", () => {
  const page = loadViewer();
  const d = page.newDoc();
  // The same frame shape, one field short: this is what a typed `/config` produces, and it belongs
  // in the transcript even while a panel is showing something else.
  d.reading = "/tools";
  page.applyLine(d, JSON.stringify({ type: "command", input: "/config", text: "shell = bash" }));
  eq(d.readingText, "", "a typed answer is not put in the panel");
  eq(d.blocks.length, 1, "it is a block in the transcript");
  eq(d.blocks[0].kind, "command", "of the kind the transcript already drew");
});

console.log("the buttons the frame marks as actions");

// §8's first control. The frame already says which commands are one action with no argument
// (`class: "button"`), so the page's job is only to draw them where controls go and send the row's
// own line. That the click sends `send` rather than a name put back together here is asserted over
// the page's bytes in tests/web_view.rs, the same way the switches' `/<name> <value>` is: the stub
// DOM has no event delivery, and inventing some would be testing the stub.
check("the frame's action rows become buttons in the header", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [], toggles: [],
    commands: [
      { label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel" },
      { label: "/new", send: "/new", help: "start a fresh conversation", class: "button" },
      { label: "/delete <n|id>", send: "/delete", help: "delete one", class: "danger" },
      { label: "/reload", send: "/reload", help: "re-read the config file", class: "button" },
    ],
  }));
  const box = page.__node("actions");
  eq(box.children.map((b) => b.textContent), ["/new", "/reload"], "one button per action, in the frame's order");
  eq(box.children[0].title, "start a fresh conversation", "the help is the tooltip");
  eq(box.children[0].type, "button", "a button that cannot submit anything");
});

check("a state frame with no actions in it takes the buttons away", () => {
  const page = loadViewer();
  const d = page.newDoc();
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [],
    commands: [{ label: "/reload", send: "/reload", help: "re-read the config file", class: "button" }],
  }));
  eq(page.__node("actions").children.length, 1, "a button while the frame lists an action");
  // A frame with only reports in it: nothing to press. A panel is not a button -- pressing one
  // would send a command into the terminal, which is the one place §8 says a report should not go.
  page.applyState(d, JSON.stringify({
    type: "state", provider: "stub", model: "m", providers: [{ name: "stub", models: ["m"] }],
    commands: [{ label: "/config", send: "/config", help: "show shell, steps, proxy", class: "panel" }],
  }));
  eq(page.__node("actions").children.length, 0, "no action rows, no buttons");
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

if (failures) {
  console.log(`\n${failures} failed`);
  process.exit(1);
}
console.log("\nall passed");
