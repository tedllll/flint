# Web mode: a window onto a running flint

A browser is a better renderer than a terminal for one thing flint already does: reading a
run. Collapsible tool calls, a transcript you can scroll back through without the scroll
region fighting you, and text you can select and copy whole. This document is the design for
getting that without giving up what makes the terminal version dependable.

The decision at the centre of it: **`--web` is a window onto the running process, not a
second mode.** flint starts exactly as it does now and additionally serves a view of *this*
process on loopback. The terminal keeps working. The browser is a client of the process, not
of a file, and closing the tab loses nothing.

**All of §9 is implemented.** The static viewer, the `--web` listener with `GET /`, the two
data routes `GET /session` and `GET /events`, and step 6 — `GET /sessions` and `POST /message`,
which is what makes the page a **composer** and gives it a sidebar. §11 records what has been
measured in a browser versus what has not, and as of 2026-09-17 that includes every control §8
built: the switches, the panel, the buttons, the forms and the destructive rows have all been
driven with real input events, and driving them found two defects. What is left is not a level but
a polish list, named at the end of §11: a native `<select>`'s open popup, which is the operating
system's, and the page's own long-answer and reconnection measurements. §8's "not doing" is still not
being done.

The token goes in two different places, and the difference is deliberate: `?token=` is accepted
on `/` alone, because that is the URL `--web` prints and the only one a person pastes into an
address bar; every other route wants `x-flint-token` in the header, so a token cannot leak
through a `Referer`, a log or a shared screenshot (`Request::offered_token`, `src/web.rs`). A
hand-written `curl` at `/session` with the token in the query string gets `403` — correct, and
not a missing route. Without the header, `/`, `/session`, `/events` and a path that does not
exist are deliberately indistinguishable.

## How to read the labels

As in [`docs/windows.md`](windows.md), with `DOCUMENTED` naming the kind of knowledge this
file leans on — browser and HTTP behaviour, which is not this repository's source and not a
measured machine:

| Label | Means |
|---|---|
| `VERIFIED` | read in this repository's source, with a `file:line` |
| `DOCUMENTED` | stated by a specification or by MDN, linked |
| `UNVERIFIED` | reasoning that has not been tried in a browser |

**Some of this file has now been run in a browser.** §9 step 2's viewer was rendered in
headless Chrome and looked at, and §11 records what that measured. It is a weaker thing than
a test and a stronger thing than reasoning: it immediately found a defect that no amount of
reading would have. The same has since been done to every control in §8 — driven over the
DevTools protocol by `scripts/browser-controls-test.js`, which found two more defects, one of
them a page with no controls on it at all — and then, in the same harness, to the sidebar's own `⋯`
menu, both drag grips and a picker from the keyboard, which were §11's last "reasoned rather than
seen" residue. What remains unmeasured is one widget and one section, named at the end of §11: a
native `<select>`'s open popup, and the page's long answers and reconnection. The *listener*'s own questions — §4's boundary, §6's routes, backpressure and
reconnection — were measured in the macOS pass this file records, and the phrasing above used to
say they were unmeasured "because the listener does not exist yet", which stopped being true when
it was built.

---

## 1. The decision: a window, not a mode

`flint --web` starts what `flint` starts — REPL or one-shot, same provider, same session file
— and opens a loopback listener that shows the same conversation.

Three consequences follow, and they are the reason for the shape:

- **One writer.** The process owns the session file and the turn. The browser never writes the
  session; it sends a message to the process, which writes it. An append-only file with two
  writers asking "who decides the order" has no good answer, and the ownership question would
  have to be invented at the file level.
- **The terminal stays first-class.** `--web` does not switch anything off. The keys, the
  answer strip, the clock and the interrupt path are all still there, because they are the
  same code.
- **The browser is disposable.** Kill it, close it, sleep the laptop: the transcript is in
  `sessions/*.jsonl` and the state it was rendering is gone with nothing lost. Nothing in the
  view may become the only copy of anything.

What this rules out, on purpose: a web UI that starts its own agent; a headless server that
owns sessions while a terminal attaches to it; a second process appending to a live session;
`--host 0.0.0.0`; and any state that exists only in browser memory.

---

## 2. What already exists, which is why this is small

Four things in the current tree are most of the feature. None of them was written for this.

| What | Where | Why it matters here |
|---|---|---|
| One event funnel, one sink | `Agent::run(&mut self, user_input, sink)` — `src/agent.rs:206` | The TUI, the plain mode and `--json` are already three renderers over one loop. The browser is the fourth. |
| NDJSON with a closed vocabulary | `ndjson::Sink::line` — `src/ndjson.rs:52`; `run_json_turn` — `src/main.rs:1705` | The SSE payload *is* these lines. One `data:` per line, no second serialisation to keep in step. |
| An input channel that already accepts typed-while-thinking messages | `InputMsg::Line` — `src/main.rs:539`; consumed by `run_turn` — `src/main.rs:1770` | The browser's input box is a second producer on a channel that exists, including its steering semantics. |
| An append-only, documented session file | [`session-format.md`](session-format.md) | History is a file read. The view of an old session works with the network down, which is the same argument the session list already makes. |

`VERIFIED`: each of the four was read in the source at the line given.

---

## 3. The levels, and what each one can actually do

| Level | Shape | Needs | Can you type and get an answer? |
|---|---|---|---|
| **L0** | `flint -p "..." --json \| jq` | nothing — **done** | No, but it is already scriptable |
| **L1** | one static HTML file, a `.jsonl` dropped onto it | a text file in the repo — **done** | **No.** It can compose a command for you to paste |
| **L2** | `--web` or `/web`, live view | the listener of §6 — **done** | No — read-only |
| **L3** | `POST /message` into the steering channel | two more routes — **done** | Yes |

**L2 has two ways in, and the second one is the one people use.** `--web` has to be decided
before the run starts, and the moment you want a real renderer is thirty seconds into an answer
that is scrolling past faster than you can read it. Measured from a real session, the first
thing that happened was `--web` typed *at the prompt*: it is the only name for the feature
anyone has met — it is in `--help`, in the README and in flint's own error messages — and
nothing marked it as belonging to the command line rather than to the conversation. It went to
the model, which answered it politely, and the run looked like it had worked.

So `/web [port]` is a command as well as a flag, one `Viewer` backs both, and **a bare flint
flag typed at the prompt is read as the command it names** — `/web` becomes the view, the way
`--provider x` becomes `/provider x`. The first version *refused* the flag with an explanation
and that was the wrong answer, in the way the report made plain: the person had already said
what they wanted, and being told to respell it is not help. What they wanted was the page.

**And the page is opened, not just printed.** Printing a URL and waiting is the behaviour of a
tool that assumes you are already at a browser; the report was "no page opened". The opener is
`open` on macOS, `cmd /C start` on Windows and `xdg-open` elsewhere, launched detached, and
called **only when stdout is a terminal** — a pipe is not a person, and without that check
`cargo test` would open windows on whoever ran it. The test for that is an assertion that a
redirected run does *not* print `(opening it)`, which is red if the check goes away.

Two consequences fall out of the REPL carrying a `Viewer` rather than reading a flag once.
`/web` twice reports where the view already is rather than binding a second listener on a second
port — the second URL would print, and the page a person already had open would be on neither.
And `/new`, `/resume` and `/reload` move the page to the conversation the terminal moved to,
because the session path is shared with the listener instead of copied when the socket is bound.

**L1 cannot be interactive, and the reason is not browser trivia.** At L1 there is no
listener, so there is nothing to talk to: a page cannot start a process, and a page that was
never handed a channel does not have one. The browser's own rules make the gap wider — a
document loaded from `file://` gets an opaque origin, so it cannot read a local file by path
or be same-origin with anything (`DOCUMENTED`:
[file origins](https://developer.mozilla.org/en-US/docs/Web/Security/Same-origin_policy)),
and the File System Access API that would let it write one is Chromium-only and needs a
secure context (`DOCUMENTED`:
[File System API](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API)).
`UNVERIFIED`: how each of those behaves in the browsers on this machine. Neither matters to
the conclusion — the conclusion is that no channel exists at L1, whatever the APIs do. So L1
is a viewer plus an *input composer*: you type a message, it produces the correctly quoted
`flint -p "..."` or `flint --resume <id> -p "..."` line for you to paste. On Windows that is
not a consolation prize — quoting and escaping is exactly the failure mode
[`windows-tooling.md`](windows-tooling.md) is about, and a page that escapes by rule is more
reliable than a model composing its own command line.

The one serverless path to real interaction would be a mailbox: the browser is granted
persistent access to `~/.flint/inbox/<session>.jsonl`, appends a line per message, and the
process polls it. It is rejected: Chromium-only, a permission prompt per session, a polling
watcher in flint, and an invented protocol with no delivery guarantee — all to avoid §6.

L1 is not thrown away when L2 arrives: **it is the same file.** At L1 it is fed by a dropped
`.jsonl`; at L2 and L3 it is fed by `GET /session` and `GET /events`. One renderer, two
sources, and the second is a wire format the first already understands.

---

## 4. The security boundary, which is not optional here

`docs/decisions.md` says flint has no approval prompts, no sandbox and no allow-list, and
that `bash` is not gated at all. A local HTTP listener changes the shape of that risk: it is
a way for *something other than the person at the keyboard* to start a command, on a program
that never asks. Two attacks are well known and both apply:

- **Cross-site request forgery.** Any page you visit can `POST` to `http://127.0.0.1:<port>`
  without reading the response. It only has to guess the port.
- **DNS rebinding.** A hostname the attacker controls can resolve to `127.0.0.1`, which makes
  a request to `http://evil.example:<port>` same-origin as the listener and defeats the
  browser's own protection.

Four constraints, all of them load-bearing:

1. **Bind `127.0.0.1` literally** — not `0.0.0.0`, not `::`, not "localhost" resolved. No
   option to change it. Remote use is not a feature of this program; it is a different program.
2. **A token per run**, generated at startup, printed in the URL that `--web` writes to the
   terminal. `GET /` requires it; everything else requires it in an `X-Flint-Token` header
   rather than a query string, so it cannot leak through a `Referer`.
3. **Check `Host`** on every request: it must be exactly `127.0.0.1:<port>` or
   `localhost:<port>`. This is the DNS-rebinding defence, and it is one string comparison.
4. **Check `Origin` on `POST`**: absent (curl) or exactly this origin. `DOCUMENTED`:
   [Origin](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Origin).

And one rule that follows from the repository rather than from the browser: **the view gets
no approval buttons.** A permission dialog that exists only in the web UI would be a second,
weaker policy for the same action, which is worse than the honest absence flint documents
today.

The page also loads **nothing external** — no CDN, no font, no analytics — which keeps the
loopback listener the only network activity, and keeps the file usable offline.

---

## 5. What the browser renders, and the rule that keeps it honest

**JS renders, Rust decides.** The view has no logic beyond turning events into DOM: no
retries, no queueing, no filtering that loses information, no derived state that the terminal
would disagree with.

- **Unknown event types are ignored**, exactly as the session format requires of its readers.
  A newer flint must not break an older page, and the page must not pretend to understand
  what it does not.
- **Model output goes in through `textContent`, never `innerHTML`.** Tool output is
  attacker-influenced text — a file's contents, a program's stderr, a fetched page — and
  rendering it as markup is a script-injection hole in a program that can run commands. This
  is a policy the tests can check (§10).
- **Plain text, no Markdown renderer.** A renderer is a parser, a parser is a dependency, and
  a parser that emits HTML is the same injection hole with more steps. Code goes in `<pre>`.
- **One file, no build step.** No pnpm, no vite, no TypeScript, no client plugins, no HMR.
  The HTML is a text file in the repository that a person can read and repair, embedded with
  `include_str!` so the binary stays one file. A UI layer that needs a build pipeline would
  end the "state is plain text and hand-editable" rule at the outermost layer, which is
  exactly where it is easiest to lose.

---

## 6. The HTTP surface, small on purpose

Eight routes. No cookies, no HTTP/2, no TLS, no keep-alive, no streaming request bodies.

| Route | Returns |
|---|---|
| `GET /` | the embedded HTML. Requires the token. |
| `GET /session` | the session so far, as the session file's own lines (`NDJSON`) |
| `GET /events` | SSE: replays from `Last-Event-ID`, then live events |
| `POST /message` | one message from the browser, into the steering channel |
| `POST /report` | one command *read* from the browser: same channel, and its answer is captured with the terminal quiet (§11) |
| `GET /sessions` | the conversations `/resume` can reach, numbered the way `/resume` numbers them |
| `GET /file` | a file the transcript named, for the preview drawer (§12). `?path=` takes what the transcript says, including a trailing `:line` or `:line:column`; a relative path is resolved against the run's own working directory. `text/plain` with `X-Flint-Line` when a line was named, and `X-Flint-Cut`/`X-Flint-Size` when the file was longer than the preview cap |
| `GET /jobs` | the `task` children and background commands this run started, as `{"jobs":[…]}` in the run's own order (running first, newest first) — for the header's jobs panel (§13). Each row carries `pid`, `kind`, what was asked, a status word, the exit code in words beside it, absolute epoch seconds for its start and end, and `path`: a child's conversation or a command's log |
| *`event: sessions`* | not a route but its counterpart: the list has changed, re-read it |
| *`event: jobs`* | the same for `GET /jobs`: a job started or ended, re-read it |
| *`event: state`* | the run's own configuration: provider, model, what each provider offers, the toggles, and the command list with §8's class for each row — a row also carries `values` when the page may send that command with an argument the frame names (a *read* on a `panel` row, a line the page types for you on a `selector`), `fields` when the page may collect its answers (one entry per word the line wants, each with the input's kind, the argument's name, and whether the command works without it), and `from` when it destroys something and the argument is one of a list (§11) |
| `type: command` | what a command answered, on the same stream as the turn's events: `input` and `text` — which is also what a header button's answer arrives on. Carries `panel: true` when the page asked to read it rather than typing it, and `input` is the command's own `send` rather than the line in the one case the line carries a credential (§11) |

**All eight are implemented.** `/session` and `/events` read the session path and the event feed
through a shared handle, which is what lets `/new` and `/resume` move an open window to the
conversation the terminal moved to. `/sessions` is the sidebar's source and goes through
`session::list` — the same function `resolve_session` uses — so the numbers in the page *are*
the numbers `/resume` accepts; a second listing that sorted differently would make every row
point at the wrong conversation.

**`POST /message` carries text and does not interpret it.** A line beginning `/` is a slash
command because the REPL says so, `!` is a shell escape because the REPL says so, and neither
fact is known to `web.rs`. So `/resume 3` from the sidebar works without the page knowing that
slash commands exist, and there is exactly one place that decides what a typed line means.

**`POST /report` is the same body and the same channel, and the one field that differs is what the
terminal does with the answer.** A report is a command the *page* reads (§8's panel class), so it is
run with `Term::quiet_start`: recorded for the page, not drawn here. The route still does not
interpret the text, which is why it does not check *which* command a report names — the table that
says a command is a report lives beside the dispatch, and the REPL refuses anything else. A whitelist
in `web.rs` would be a second copy of that table in the one file that is proudest of not having one.

It is also the only route that can make something *happen*, and that is worth stating plainly:
reachable at that port with that token, a caller can run the agent. What keeps it acceptable is
unchanged from §4 — loopback only, the `Host` and `Origin` checks, and a token another origin's
page cannot set, since a cross-origin form post cannot add a header and a `fetch` that could is
refused by `Origin` before it arrives. The body is length-delimited, capped at a megabyte, and
refused rather than half-read when it stops early.

- **SSE framing is the existing format.** Each event goes out as `data: <one ndjson line>`
  followed by a blank line, and each carries `id: <session line number>`. A reconnect sends
  `Last-Event-ID` and the server replays from there, so reconnection needs no invented
  protocol (`DOCUMENTED`:
  [SSE](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)).
- **A heartbeat comment** (`: ping`) every 20 seconds or so, because an idle connection is
  what middleboxes and browsers drop first, and a dropped stream looks exactly like a turn
  that has not started.
- **One request per connection: `Connection: close`.** This is the decision that removes the
  hard parts of HTTP/1.1 — keep-alive, pipelining, request-body framing, chunked encoding.
  SSE is a `Content-Length`-less response that ends when it ends, which is a case we need
  anyway; everything else is a small, complete response.
- **Nothing is served from disk.** No path is taken from a URL, so there is no traversal to
  get wrong.

---

## 7. The one open question: hand-rolled HTTP, or `hyper` — **decided**

**Hand-rolled, and the decision is recorded in [`decisions.md`](decisions.md#dependencies)
where the other dependency decisions live.** It went that way for the reason the
recommendation below gave: the surface is small enough to read in one sitting, `Connection:
close` deletes the corners we do not need, and `tokio`'s `net` feature compiles nothing new
because `mio` is already in the lock file. The counter-argument stands as written and is the
thing to re-read if a route ever needs request framing this file does not have.

The reasoning, kept as it was written before the decision was made:

- **`hyper` is already in the tree.** `Cargo.lock` has `hyper`, `hyper-util`,
  `http-body-util` and `tokio-util` today, pulled in by `reqwest`. A direct dependency would
  compile nothing new — the argument against it is API surface and hand-written glue, not
  build time or binary size. `VERIFIED`: read in `Cargo.lock`.
- **Hand-rolled** is about 250 lines of tokio for four routes with `Connection: close`: read
  the request line and headers to a blank line, compare two strings, write a response. The
  risk is the corners we do not need (keep-alive, chunking, pipelining, request bodies over a
  few hundred bytes) — and `Connection: close` is what makes those corners not exist.
- **Recommendation: hand-roll it**, for the same reason the `SKILL.md` front matter is parsed
  by hand — the surface is small enough to be read in one sitting, and every line of it is
  ours to be wrong about. The counter-argument is real, though: HTTP has more corners than
  YAML front matter, and this is the one place in flint that listens on a socket.

Whichever way it goes, the decision and its reason belong in
[`decisions.md`](decisions.md) when it is made.

---

## 8. Deliberately not doing

- **Approval prompts, a permission UI, a diff-review screen.** §4. The absence is the policy.
- **Remote access.** No `--host`, no reverse-proxy-friendly mode, no TLS. Not this program.
- **Two writers on one session.** The mailbox in §3 is the only serverless alternative, and it
  is rejected there.
- **WebSockets.** Input is request/response; output is one-way. SSE plus `POST` is the whole
  protocol, and it survives reconnection by itself.
- **A build step, a framework, a component library.** §5.
- **Markdown or HTML rendering of model output.** §5.
- **File upload, image attachment, an in-page editor.** Each is a feature with a body of code
  behind it; none of them is what a rescue tool needs at two in the morning.
- **Rendering a session that no process has open** beyond the static viewer: opening an old
  conversation is `flint --resume <id> --web`, which is a window onto a process like any other.

---

## 9. The plan

In order, each one its own commit. Everything before step 4 is useful on its own, and steps
1–3 do not require the HTTP decision of §7 to have been made.

1. **This document**, its row in [`ROADMAP.md`](../ROADMAP.md), and the reference in
   `AGENTS.md`.
2. **The static viewer** — `web/view.html`, one file, no build: drop a `.jsonl` on it, render
   the conversation with collapsible tool calls and no external requests. Plus the policy test
   of §10. Useful immediately, and it is the renderer everything later reuses. — **done**
3. **The window in the CLI** — `--web [--port]`, printing the URL and the token; the listener
   with `GET /` only, plus the token, `Host` and `Origin` checks of §4 and their tests. A run
   without `--web` must be byte-identical to today, which the existing terminal tests already
   assert. — **done**, with two additions worth knowing: the default port is **ephemeral**
   (`--port 0`), because a fixed one collides with whatever else is on loopback — a local model
   server on 11434, 1234 or 8080 — and the URL has to be printed anyway; and the responses
   carry `Content-Security-Policy: default-src 'none'` with `connect-src 'self'`, which makes
   "loads nothing external". §5's rule, something the browser enforces rather than something a
   test hopes for.
4. **History** — `GET /session`, and the viewer switching from a dropped file to a fetch. The
   first commit that needs §7 decided.
5. **Live** — `GET /events`: the same `ndjson::Sink` output, framed as SSE, with `id:` and
   `Last-Event-ID`. The test asserts the frames parse as NDJSON and that the terminal still
   sees the same turn. — **done**, with four things that were not in the plan and are worth
   knowing: `/session` had to report a **cursor** (`X-Flint-Event-Seq`) because the file and
   the stream would otherwise double-count or lose a frame depending on timing; the stream is
   opened *before* the file is read and the server subscribes before it sends headers, so
   nothing slips between them; a reconnect is served from the last 512 frames and gets
   `event: reset` when that is not enough; and the current status is sent once on connect as
   a named event, because a page opened in the middle of a turn has missed every status frame
   and there is no cursor for "now".

   **This step and the `status` event are one commit, and the reason is worth knowing before
   starting.** The browser needs to be told what the turn is waiting for — without it, a
   browser shows nothing at all between `turn.started` and the first delta, which with a
   local model is minutes and reads as a hung program. The phase is decided in `run_turn`,
   which today calls `printer.term().activity_started/named(...)` directly, so sharing one
   source of truth means:

   - a `Live` handle (a broadcast channel plus an `ndjson::Sink`) threaded into `run_turn`;
   - the turn's event closure feeding that sink as well as the terminal, which is also what
     `GET /events` relays — so one channel carries both the status and the stream;
   - `Term::activity_started` and `Term::activity_named` returning whether the clock
     actually restarted, so the event reports what happened rather than what a caller
     guessed. The browser's clock has to start over exactly when the terminal's does, and
     only `Term` knows that (`activity_started` deliberately keeps the original start time
     for a repeated name, and `activity_named` deliberately does nothing when no wait is
     running).
   - a `status` event carrying `restarted`, because a *rename* — `waiting for the model` to
     `writing the answer` thirty seconds in — must not reset the browser's clock to zero.
6. **Input** — `POST /message` into `InputMsg::Line`, through the steering path, with its own
   test: a message typed into the browser steers a turn that is already running. — **done**,
   and the interesting part was not the route. A line from the browser joins the channel the
   keyboard feeds, so a message sent while the model is working steers the turn exactly as
   typing does — and that turned out to be wrong for a *command*: `run_turn` picks a mid-turn
   line up to make it the next prompt, and cannot run anything, so `/resume` from the sidebar
   became a message to the model and the conversation did not change. A line that is a command
   is now handed back to the REPL instead (`for_the_repl`), which is the only place that knows
   what a typed line means.
7. **Measure, and write it down here** — what a long turn does to the browser (backpressure,
   scroll position, a stream that grows to a megabyte), whether a reconnect after a sleep
   replays exactly once, and how the page behaves at 80 columns next to the terminal.

---

## 10. Testing what is not Rust

The page cannot be unit-tested by `cargo test`, so the tests go around it rather than into it:

- **The policy test**, over the embedded HTML as text: no `innerHTML`, no `eval`, no
  `<script src`, no `http://` or `https://` in a fetched position. It is a blunt test, and it
  is the one that catches the two mistakes that matter (injection and an external dependency).
- **The routes**, through the real binary: `GET /` with and without the token, with a foreign
  `Host`, and a `POST` with a foreign `Origin` — the four assertions of §4.
- **The SSE frames**: capture a turn's stream from the route and parse every `data:` line as
  NDJSON, which is the same assertion `tests/json_output.rs` already makes about `--json`.
- **The URL line**: `--web` prints a URL whose token is the one the listener accepts.

What stays uncovered is the DOM, and the honest response is to keep the JS small enough to
read in one sitting and to record what was measured in §9 step 7, in the style of
[`docs/windows.md`](windows.md): labelled, dated, and not confused with a test.

## 11. What has been measured

In the style of [`windows.md`](windows.md): labelled, dated, and not confused with a test.
Everything here was measured on macOS with headless Chrome at 1100px, against the viewer of
§9 step 2 with a fixture session file. The *logic* of the renderer is tested — it is the half
that touches no DOM, and `scripts/web-view-test.js` runs it under Node with fifteen
assertions. What follows is the part no test in this repository reaches.

| Claim | How | Result |
|---|---|---|
| The transcript reads in the order it happened | fixture session file, one screenshot | instructions, user, thinking, the tool call, the answer — in that order |
| The instructions must be collapsed | the first render had them expanded | **a defect, found here:** the system prompt is a few thousand characters and pushed the whole conversation off the first screen. The terminal does not print it for the same reason. Fixed, and re-measured |
| A tool call is legible while collapsed | screenshot | the summary carries the name and the arguments, so a collapsed call still says what it was aimed at |
| Multi-line answers keep their shape | a three-paragraph answer with an indented line | preserved, `pre-wrap`, no reflow of the indentation |
| A narrow window | **measured**, 1200/900/700/520 px | no horizontal overflow at any of them; the composer stays pinned to the bottom edge; the sidebar holds its 250px and the transcript takes the rest, so 520px leaves the transcript a cramped 270px and nothing breaks |
| A long turn, and backpressure | **measured**, and it found the limit of this design — see below | a stub provider on loopback, a real turn, `curl -N /events` |
| Reconnect, replaying exactly once | **measured** against the ring and the reset boundary | a cursor older than the buffer gets `event: reset` and re-reads `/session` |
| A page opened *during* a turn | **measured**, with a defect found and fixed | the status was blank; it is now sent as state on connect |
| The status line during a slow model call | **measured** | the browser shows `no response yet — the network or the endpoint may be stuck`, the terminal's own words |
| A turn on a browser that then reloads mid-turn | **measured** — a defect, fixed | the reloaded page showed the answer from the middle; now it is given the whole of what has streamed. 280,522 characters on screen against the 279,900 the model produced |
| What 80 columns looks like next to a terminal | **measured** | 80 columns is about 640px, which sits between the two widths above: no overflow, nothing clipped, and the transcript is 390px — narrow enough that code wraps, which is what the terminal does too |
| A reconnect after the stream is cut | **measured**, no duplicates | the response is re-read and the stream resumed; every message appears exactly once |

### Reading a run, measured: four defects, and the reason they existed

The §9 step 7 pass, driven over the DevTools protocol against the real page, with a stub provider
whose speed and size are knobs. Everything below was found by measuring, and three of the four
came from the same decision -- `paint` rebuilt the whole transcript on every frame, so every
node on screen was a *new* node.

| Claim | How | Result |
|---|---|---|
| The answer arrives where you are looking | a long answer, scroll sampled every 0.9s | **a defect.** The transcript stayed pinned at the top while the answer grew 55,000px below the fold: you ask something and watch the question. Now the page follows the bottom if it was already there, and does not if the reader has scrolled up |
| An expanded block stays expanded | open the thinking block, then a repaint | **a defect.** Every repaint rebuilt the node, so `open` went back to its default. The collapsible tool call — the reason this page exists — could not be read while anything was happening. Now the node is reused when the block has not changed, and the state lives in the block as well, so even a full rebuild puts it back |
| Text can be selected and copied | select an answer, force a repaint | **a defect.** 102 characters selected, 0 after. This is the promise the whole page is for. Same cause and same fix |
| A `reset` in the middle of a turn | 5000 deltas, watch the length | **a defect, and the worst one.** 1,424,691 characters on screen became **64,999**: the page threw away what it was showing and re-read a file that cannot have the answer yet, because the file gets the assistant message when the turn *ends*. Fixed by carrying the streaming block across the re-read, and only when the file's copy is a prefix of it — anything else means the file has moved on and adding the memory back would duplicate the answer |
| A page *loaded* in the middle of a turn | reload during a turn | **a defect.** A page starting fresh has no memory to carry, so it showed the answer from the middle. Fixed on the server: the answer so far is sent as a named `answer` event — state, like `status` — on connect and after a lagging reset. Verified: 280,522 characters on screen against the 279,900 the model produced |
| A megabyte of answer | 5000 × 310-character deltas | **the limit, and it is a real one.** Painting cost is quadratic: about 85 KB/s rendered while the transcript was small, about 17 KB/s past a megabyte. Node reuse removed the DOM half of it; what is left is the browser laying out one very large text node on every frame. Worth knowing for scale rather than for alarm — a real model emits on the order of 300 **bytes** a second, two orders of magnitude below the slowest figure measured here. The fix, if it is ever needed, is to append to the text node rather than rewrite it, or to split it into chunks |

**And two small ones found by looking at the page rather than by measuring it.** Switching
between conversations was there and *starting* one was not — a page that could only ever show
conversations somebody had already begun. `+ new` sends `/new`, the terminal's own command. And
the composer said only what Enter does, which hid the fact that the whole command vocabulary
works there: the hint now names `/help`. Neither is a design question; both are the kind of gap
that only shows up when somebody uses the thing.

**The fourth defect is the one worth remembering**, because it is the only one that destroyed
something the reader had: the transcript on screen *went backwards*. It took a synthetic producer
to reach it, and it would take a throttled tab or a sleeping laptop to reach it in ordinary use —
but a page that loses the answer it was showing is a page that cannot be trusted to show one.

### L3 in a real browser — measured, and it found three defects

Driven over the Chrome DevTools protocol against the real page and the real binary: fill the
textarea, dispatch the events a person's typing produces, and then look at what flint received.

| Claim | How | Result |
|---|---|---|
| The sidebar lists the conversations | `GET /sessions`, eight real sessions | numbered newest-first, the open one marked, labels ellipsised to one row each |
| Tidying the history in the terminal reaches the sidebar | `/delete 2` in the terminal, a middle conversation | **a defect.** The page kept offering it — and the numbers in the list are *positions*, so each stale row below it pointed at a different conversation. Fixed: a frame named `sessions` re-reads the sidebar and nothing else. The rows went `1=gamma 2=beta 3=alpha` → `1=gamma 2=alpha 3=(empty)`, which is what `/resume` now takes |
| The sidebar can start one | `+ new`, three times over | one click, one `POST /message`, one new conversation each time — the count was watched on disk as well as on the page, after an earlier run suggested a click could make several |
| Enter sends | `keydown` Enter in the textarea | the message reached the transcript; the box cleared |
| Clicking a row switches conversation | click on row 2 | `/resume 2` ran in the terminal, `/session` changed id, `current` moved to row 2 — **for free**, because the sidebar sends text and the REPL decides what text means |
| A message typed into the page appears in the *terminal* | pty, side by side | `> hello from the browser`, then the answer |
| What was asked arrives live on `/events` | a listener attached across a turn | `turn.started` with the prompt, then `message.delta` frames |
| The input is not cleared when the send failed | `fetch` stubbed to 503 | the text stayed and the hint read `not sent: HTTP 503 down` |
| A sidebar click while the model is working | a five-second stub | **a defect.** `/resume` was taken by `run_turn` as the next prompt: the conversation did not switch and the model was asked about it. Fixed — see §9 step 6 |
| Two sends 150 ms apart | send, type again, send | **a defect.** The first one's clear-on-success wiped the second one's text. Fixed by clearing only when the box still holds what was sent |
| The layout | screenshots at 1400×900 and 900×700 | **a defect.** `#status` was `position: fixed` and sat *on top of* the composer and the hint — the bottom two lines were unreadable exactly while a turn was running, which is when the status line shows. It is the second grid row now |

The three defects are the reason this section exists. None of them is visible in the source: the
first two need a turn to be in flight, and the third needs a window with a status line in it.

**A harness trap that reaches outside the terminal, and cost the most time of anything here.**
`--web` and `/web` open a browser — that is what they are for. A harness that runs flint therefore
opens a tab in the browser of whoever is sitting at the machine, once per run, and the person at
the keyboard has no way to know they are looking at a scratch instance with `FLINT_HOME` under
`/tmp` rather than at their own flint. That is exactly what happened: several tabs, someone
clicking around in one of them, and a report of extra conversations that sent me looking for a bug
in `/new` for a while. There was none — the conversations had been made by hand, in a directory the
harness owned.

So: **replace `open` on `PATH` with a script that records its argument** before any run that has a
browser in it. One line of setup, and it is the same trick the opener row above uses to measure
the launcher without a window appearing. Two further traps are already written down — a driver
that stops reading the pty deadlocks the program under test the moment it writes a streamed
answer (HANDOFF), and in raw mode `\n` is Ctrl-J rather than Enter (below) — but this is the only
one whose damage lands on somebody else's screen.

### `/web` from a real session — measured in a pty, on the release binary

The two ways into L2 were driven end to end rather than reasoned about: a pty (a pipe cannot
carry `/web`, because `from_stdin` reads lines and never reaches the event reader), a scratch
`FLINT_HOME`, and `curl` at the port the URL named.

| Step | Result |
|---|---|
| `/web` typed at the prompt | `web: http://127.0.0.1:58962/?token=7fc0…c47` — a live URL |
| `--web` typed at the prompt | `--web is a start-up flag — doing /web instead`, then the URL with `(opening it)` |
| `flint --web` at startup | the same line, and the opener called with the URL |
| The opener actually called | `open` on `PATH` was replaced by a script that records its argument; it received the URL, in both cases above |
| `GET /session` with that token | `{"type":"meta","v":2,"id":"1789309949-903",…}` — the conversation, as it is |
| A `/events` listener attached, then `/new` | `event: reset` followed by an empty `data:` frame, exactly as designed |
| `GET /session` after the `/new` | `id` changed to `1789309957-534` — the *new* file, so the page follows the run |
| `/web` twice | one URL, printed twice; no second listener |

Replacing `open` on `PATH` is the trick worth keeping: it measures the whole path — decision,
program, argument — without a browser window appearing on the machine running the test.

The pty also turned up a trap worth writing down: **in raw mode `\n` is Ctrl-J, not Enter.**
`crossterm` reports the byte as `Char('j')` with a control modifier, so a script that drives
flint through a pty has to send `\r`. The first attempt sent `/web\n` and the transcript read
`> /webj` with the command sitting unsubmitted, which looks like a flint bug and is not one.

### The limit that measuring found — and then closed

**Streamed output used to be buffered per attempt, so a browser got no text until a model
response had finished arriving.** Not a web-mode defect: `provider.rs` buffered a whole
attempt's events so that a retry could discard them, and `HANDOFF.md` carried "streamed output
arrives in one go" as a known limitation for several rounds. What was new here is how visible a
browser made it — a page showing a status line and *nothing else* for twenty seconds, then the
entire answer at once.

Fixed, and the measurement is what says so: 128 `message.delta` frames from a local model
arrived over 5.5 seconds, the first at 1.9s and the last at 7.4s. Buffered, all 128 arrived at
7.4s. A test asserts the same thing against a server that writes one delta and then sits on
the connection for a second and a half — the first delta must arrive before the wait is over,
and with the buffering restored it arrives at 1.504s.

The retry rule changed with it, and that is the part worth knowing: **the ladder stops at the
first drawn character.** Everything before it — a connection that never opened, a 503, a
stream that died during the *reasoning* — is still retried and still leaves no trace. After
it, the failure is reported instead, because a second attempt would be written after the
first: a terminal has scrolled the line, a pipe has emitted it, the browser has rendered it,
and nothing in the chain can take text back.

One defect came out of making this change, and it was the reason the old retry had a hole in
it: **a stream that ended without the provider's completion signal was accepted as an
answer.** `parser.done` was only ever used to leave the read loop early and never asked
afterwards — so a dropped connection, which usually shows up as a body simply ending, produced
a half answer that read as a complete one. It is now a transport failure, which is what puts a
mid-answer drop back on the retry ladder when nothing has been drawn.

### Stopping a turn from the page — measured, and it found the frame that was missing

The stop button was the last of the three complaints in `ROADMAP.md` §9, and the button is the
small part of it. Driven against a stub provider that draws half an answer and then holds the
connection open, with `--web`, its stdout in a file, and `/events` read as it arrives on a socket
with a read timeout — the shape every "the page does not change" report needs, and the reason all
three of these were invisible in the source:

| Claim | How | Result |
|---|---|---|
| The composer can stop a turn | the button, from `turn.started` to `turn.completed` | offered only while a turn is in flight; one click sends one `/stop` down the composer's own route |
| A stopped turn is over *on the page* | `/stop` typed mid-answer, `/events` read live | **a defect.** No `turn.completed` frame is sent by an interactive run at all — the one-shot `-p --json` path writes it, so the page's own handler for it had never run under `--web`. `message.completed` is what closes an answer, and an interrupted turn never reaches it: the half answer stayed open and the button stayed on screen |
| The status line ends with it | the frames after `/stop` | **a defect.** The last frame was `{"restarted":false,"text":"writing the answer","type":"status"}`. `Term::activity_done` returned early when the terminal was not interactive, so a run with a pipe or a file for stdout announced every wait it began and never the end of one |
| A page opened after a stop is not told the half is still arriving | `Live::answer_committed`, then a fresh `/events` connection | **a defect.** `Event::Done` is what drains the accumulator the `answer` snapshot reads, and an interrupted turn never gets one — so the stopped half would have been replayed as an answer still being written, and carried into the next turn's `message.completed` |

The three fixes are in `run_turn` (`turn_over`), `term.rs` and `web.rs` respectively, and the
button reads `doc.running` — the turn's own boundaries — rather than `doc.status`, which also
carries feed trouble. The lesson worth keeping is the one `ROADMAP.md` had written down wrongly:
what settles a page is **the end of the turn**, not a cleared status line. The status is the strip
and the answer is a block, and only the first of those was ever being told.

Four gates, and every one was watched red first: `a_stopped_turn_tells_the_page_it_is_over` (a real
process, a live `/events` read, both frames asserted), `the_composer_can_stop_the_turn_it_is_watching`
(the page's bytes: the word, the route, the state), the in-crate
`a_page_opened_after_a_stop_is_not_told_the_stopped_answer_is_still_arriving`, and the page's own
Node check, which runs the real `paint` over a stub DOM and reads the button back — the first
assertion that failed there was `doc.running` being `undefined`, which is what a page that has never
heard of a turn's end looks like from the inside.

### The read channel, and the controls drawn from it — measured, 2026-09-14

`ROADMAP.md` §8 orders the work: the page sends the command *line*, so what it needs first is a
read channel — the options a control would offer. The page may not read `config.toml` for them, and
this is not a stylistic preference: the file and the run genuinely differ. `/config` prints
`readonly = false (this run; the file says true)` for a run that wrote the file, and a page that
parsed the file would report the other one.

So the event stream grew a `state` frame — provider, model, providers with their models, the
enumerable toggles — and the header grew the first two controls drawn from it: a provider picker and
a model picker, each sending the line it names through the composer's `sendText`. Measured the same
way as everything else in this section, against a real `--web` process:

| Claim | How | Result |
|---|---|---|
| The page is told the state | `/events` read as it arrives, on connect | carried, once, as a named frame — and re-derived then dropped when unchanged, because the REPL asks on every line and a frame per line would rebuild a picker under whoever is choosing from it |
| A page that opens later is told it | a *second* `/events` connection, after the state had already been announced | carried from the snapshot. A client with no cursor is not replayed the ring at all, so on connect is the only chance it gets |
| The page can change it | `POST /message` with `/model stub-other`, the route the picker uses | the feed carried `"model":"stub-other"` a moment later: the command the picker composes is the terminal's own, and the frame describes the run that command made |
| The pickers are drawn right | the page's real `applyState` under Node, over the stub DOM | options in, the value in force selected, a single-option picker disabled. The case that needed care: a current value the frame does not list — a provider's own `model` is offered by `/model` whether or not it is repeated in `models` — where a `<select>` keeps its first option and would then *send* it |
| A setting can be switched from the page | `POST /message` with `/verbose full`, the line the switch composes | the feed carried `"name":"verbose"…"value":"full"` a moment later. The frame carries each toggle as a name, the values that name takes and the value in force, so the page holds no list of its own — not the toggles, not the words, and not which one is on |
| The switches are drawn right | `showToggles` under Node, over the stub DOM | one labelled switch per toggle in the frame, each holding that toggle's values with the value in force selected; a frame with no toggles in it removes them rather than leaving values behind that nothing is reporting |

**Measured in a browser since, 2026-09-17** — see `The later controls, in a real browser` at the end of
this section, which drives the header with a real font and a real window: the switches, the pickers and
the panel are all now looked at rather than only reasoned about, and one of the four switches was moved
by a real key event rather than by a script setting a value. What is still not measured there is the
*picker* (a native `<select>` opened by a pointer is an OS widget no protocol can reach into) and the
sidebar's own menu.

The command list was deliberately not in the frame until something read it — and what reads it is the
panel in the next section, which is why it arrived then rather than here. The other half of that
paragraph — an action's *output* reaching the page — is the section below it.

### What a command answered, and the bug under it — measured, 2026-09-14

`printer.term()` draws in a terminal and exists nowhere else, so a command typed into the composer
answered into a place its reader could not see. The fix is at the funnel rather than at a hundred
call sites: `Term` records what is printed while a command is being handled (`answer_start` /
`answer_take`, called by the REPL around `handle_command`), stripping the printer's colour codes as
it records, and `Live::command` pushes the result as one `{"type":"command","input":…,"text":…}`
line. Scoping the recording by the caller is what keeps a *turn* out of it: a turn's lines come
through the same funnel and are already frames of their own, so recording them here would send every
tool result twice.

| Claim | How | Result |
|---|---|---|
| The answer reaches the page | `POST /message` with `/config`, `/events` read live | `{"type":"command","input":"/config","text":"config: …default_provider = stub…"}`. A *listing* answered a command that used to answer nothing a page could read |
| A command's failure is an answer, not the end of the run | `POST /message` with `/verbose loud`, then `/verbose full` | the frame carried `expected on|off|full`, and the state frame that followed carried `"value":"full"`: the run was still there. Before this it printed `flint: error: …` and exited — one mistyped word took the session, and from the page there was not even a message, because the error went to stderr |
| Colour does not reach the page | `Term::plain`'s two escape forms, in the unit test | `\x1b[2mdim\x1b[0m` records as `dim`, an OSC title records as nothing, and a truncated sequence loses its tail rather than printing junk. The e2e run cannot see this: its stdout is a file, so it has no colour to strip |
| The page draws it as a transcript block | the page's real `paint` under Node, over the stub DOM | one `turn command` row, with `/tools` as its label and `bash\nread\nedit` as its body; a frame with no `text` paints nothing rather than the word `undefined` |

**Measured in a browser since, 2026-09-17**: a line typed into the composer and sent with a real click
on `send` reaches the run and its answer lands in the transcript, read off the process's own output
rather than off the page — see `The later controls, in a real browser`. What that run did *not* do is
scroll: the answer's block is drawn the same way whether the page is at the top or a thousand lines
down, and the scroll position is still the page's own business, reasoned about rather than seen. The
part that *was* only a guess is gone with it: the block is drawn like a user message with the command
as its label, and a real browser has now drawn one.

### The command list, and the panel drawn from it — measured, 2026-09-15

The read channel needed one more field before a control could be drawn from it: *which* commands
there are. `/help` printed a hand-written block, dispatch is a `match`, and a third copy for the
frame is exactly the drift this project spends its comments preventing, so the list became one table
in `src/main.rs` (`COMMANDS`), with `/help` printing it and the frame carrying it.

An entry is `{label, send, help, class}`, and later `values` on the rows a page may send with an
argument — a *read* on a `panel` row, a line the page types for you on a `selector` (§11's two
sections). `label` is what `/help` prints (`/provider key <key>`); `send` is what
the page puts on the wire (`/provider key`) — carried beside the label rather than split out of it,
because the split is not uniform (`add` is part of `/provider add`, `<key>` is not) and a page
re-deriving that would be re-deriving this program's grammar. `class` is §8's own, and two kinds of
row are *not* in the frame: the toggles (the `toggles` field already carries them in the shape a
switch needs) and the terminal's own — `/exit`, `/web`, `!`, `/stop`.

| Claim | How | Result |
|---|---|---|
| The page is told what commands exist | `/events` on connect, read off a real `--web` process | every row the run offers, each with `label`, `send`, `help` and `class`; `/exit`, `/web`, `!<command>` and the toggles absent (`/hear-peers` joined the toggles, so it is drawn as a switch and not as a row). `send` is what the page posts, so nothing in the page assembles a command line out of parts |
| The two lists are one list | the same test reads the frame off `/events` *and* `/help` off the run's stdout | every row the page was handed is a row `/help` prints, with the same description. Mutation-checked: dropping the destructive rows from `/help` fails it on `/provider rm <name>` |
| Every offered command exists | the frame's own `send` strings posted to `POST /message`, the answers read off the feed | no `unknown command` for any `panel` or `button` row, and a floor on how many rows were checked. Mutation-checked: `/tools` → `/tool` in the table fails it naming that row |
| The help is generated, and unchanged | the `/help` output captured before the rewrite, diffed after | byte-identical, `/stop`'s hand-made continuation included (a greedy wrap at the width the column leaves reproduces the same break) — with one deliberate change, `/config [edit]` split into `/config` and `/config edit`, because one row cannot carry two classes |
| The panel is drawn from the frame | `showCommands` under Node, over the stub DOM | one group per class the page was taught, in the page's reading order (reports, actions, selectors, forms, destructive), each row showing what to type and what it does; a frame with no command list takes the panel away |
| The page holds no copy of the list | `tests/web_view.rs`, over the page's own bytes | the panel is built from `state.commands`, and `/provider key`, `/delete <n|id>` and `/reload` appear nowhere in the page |

The panel was deliberately not a control at this point: it listed the rows and grouped them, and
clicking one did nothing. Both halves have since been built — the header's buttons send, and a report
row is read — and this paragraph is left as the record of what this commit measured.

**Measured in a browser since, 2026-09-17**: the panel is opened by a real click on its `summary`, read
against the frame's own rows, and photographed — see `The later controls, in a real browser`, which
also found that it could cover the composer while it was open.

### The first control: a button for the actions — measured, 2026-09-15

§8's first class, and the smallest honest one: an action takes no argument, so there is nothing to
ask for and nothing to confirm, and the whole control is "send this line" — which is what the
composer, the pickers and the switches already do. `showActions` draws one button per `class:
"button"` row into the header's controls row, labels it with the row's `label` and titles it with the
row's `help`, and sends the row's own `send`. Its answer arrives on the `command` line, in the
transcript. Two classes are not drawn: a report is not sent (the panel above is where a report is
read, and §8 keeps those commands off this page's wire), and a destructive command waits for the
confirmation this page does not have yet.

| Claim | How | Result |
|---|---|---|
| The frame's action rows become buttons | `showActions` under Node, over the stub DOM | one button per `class: "button"` row, in the frame's order, labelled with `label` and titled with `help`; a frame whose rows are all reports has none, which is the same assertion that a panel is not a button |
| The press sends the frame's line | `tests/web_view.rs`, over the page's own bytes | `sendText(command.send)` — the line the process composed, not a name the page put back together — with the class filter and the refused-send path (`showState(doc)`) pinned beside it. The stub DOM has no event delivery, so this is pinned as bytes and the Node checks pin what is drawn, exactly as the switches' `/<name> <value>` is |
| An action answers with something | the frame's button `send` strings posted to `POST /message`, the answers read off the feed | non-empty `text` for every button row. A button's whole feedback here is the line it prints, so an empty one would be indistinguishable from a press that never arrived. Mutation-checked: neutering `Live::command`'s push fails it naming `/new` |

**Measured in a browser since, 2026-09-17**: `/reload`'s button is clicked with a real pointer and the
run prints `reloaded`. Being measured at all is worth noting for where the button is — the *header*,
not the panel: an action takes no argument, so the panel keeps it as a row of reference and the button
lives beside the switches (`showActions`). A harness that looked for it in the command list, as this
one did first, finds nothing and learns something.

### A report is read, not printed — measured, 2026-09-15

§8's second class, and the first the composer's route could not carry. A report *is* a command's
answer, so the cheap route was to send it on `POST /message` like a button — but then the listing
prints on the terminal too, and the terminal is where somebody typed `/help`. So the same input
channel carries two kinds of line: `FromPage::Line` (a person typing, which prints here and lands in
the transcript) and `FromPage::Report` (the page reading, which is recorded and not drawn). The
answer goes out as a `command` frame with `panel: true`, because it *is* a command's answer — only
its destination differs. The panel is one reading at a time, with a way back to the list, and nothing
is cached: a listing kept from a moment ago would be a second copy of a fact the process owns.

| Claim | How | Result |
|---|---|---|
| A report is not printed here | a real `--web` process; `/config` sent to `POST /report` **and** to `POST /message` in one run, then the transcript read off disk | `config.toml` appears exactly **once**. The same command by both routes is the assertion: the typed one prints, the read one does not, and a report route that printed would be a listing twice on a screen nobody asked it for |
| The answer is the process's own text | the same run, the feed | a `command` frame with `"input":"/config"`, `"panel":true`, and the whole listing in `text`. Mutation-checked: `Term::quiet_start` not setting the flag gives 2 occurrences and fails with that sentence |
| Only reports may be read | `POST /report` with `/new` (a button), then the feed and the transcript | the frame carries `not a report`, the terminal says `refused:`, and `started a new session` is nowhere in the transcript — a report route that ran what it was handed would delete a conversation on one click that never happened |
| The route carries the kind, not just the text | `src/web.rs` unit tests | `/message` queues `Line` and `/report` queues `Report` for the same body; a report without the token is 403 and never queued |
| The page sends a report to `/report` | `tests/web_view.rs`, over the page's own bytes | `fetch("/report"` with the row's `send` as the body, `doc.reading` set, the panel redrawn — and no `/message` in that function at all |
| A panel's answer fills the panel | Node, over the stub DOM | a `panel` frame fills the reading only when it answers what is on screen; a frame without the mark stays a transcript block. Mutation-checked: `ev.panel === true` → `false` fails the first, dropping the class filter fails the row check |
| The row is a control and the others are not | Node, over the stub DOM | report rows are `button` (type `button`), a selector row is still a `div`, and each still says what to type and what it does |

**Measured since**: a report asked for *while a turn runs*, which is what this note said the suite had no
turn slow enough to check. It has one — a stub provider that draws a delta and then holds the socket
open — and `a_report_asked_for_mid_turn_waits_for_the_turn` uses it against a real `--web` process: the
report is accepted at `/report` mid-turn, the turn is then stopped (a real interrupt, `outcome: stopped`),
and both frames are read off one feed so the *order* is the assertion — `turn.completed` first, the
`panel: true` answer with the config path after it. That is the property the wait exists for: a read that
raced the turn would be a second writer in the transcript. Still open after it: a browser, as ever.

### A read that takes an argument, and the values as the permission — measured, 2026-09-15

§8's selector class, completed. Four of its controls existed already (the two pickers, the switches,
`/resume` from the sidebar). `/skills <name>` had no source for its options, so a command row may now
carry `values`: the argument values the page may ask for, filled from `Agent::skills` — the names the
run's prompt was built with, kept as a field because the state frame is rebuilt after every line and a
directory walk per line is not the comparison its own comment claims. Because the field is on the row
rather than in a `providers`-style list, it is also the *permission*: the route admits a panel row's
bare `send`, or its `send` plus one of its own values, and nothing else.

| Claim | How | Result |
|---|---|---|
| The frame offers the run's skills | a real `--web` process with a scratch `FLINT_HOME` holding `skills/demo/SKILL.md` | the row is `{"label":"/skills [name]","send":"/skills","values":["demo"]}` — mutation-checked: with the values emptied the frame is still a valid menu and the test fails on exactly that fragment |
| Reading one is quiet, like any report | the same run; `/skills demo` to `POST /report` | a `command` frame with `"panel":true` and the body in `text`, and the transcript does not contain the body at all |
| A value the menu did not offer is refused | the same run, with a *second* skill written **after** startup | the frame says `not a report`. This is the discriminating case: the command discovers the directory on every call, so a rule of "any argument after a panel row's `send`" would read the late skill — mutation-checked, and it does |
| The page draws one line per value | Node, over the stub DOM | a row with `values: ["alpha","beta"]` is drawn as the roster line plus `/skills alpha` and `/skills beta`, each a `button` with the row's own `help`; a panel row without values is still one row |
| The page composes no command of its own | `tests/web_view.rs`, over the page's own bytes | the value rows are built as `send + " " + value` from the frame's two strings, next to the `command.values` that named them — the same shape a toggle uses for `/<name> <value>` |

**Not measured here**: a command whose values the frame does not carry gaining a control — see the
next section, which is where the two switches did.

### The switches are offered their values, and a value is a reading only on a report — measured, 2026-09-15

The second thing this round added because somebody used the page: `/provider <name>` and `/model <name>`
take an argument out of a list the run already knows, the page was already *shown* that list (the
header's pickers are filled from `providers`), and the panel still drew them as plain rows — so
switching meant reading a name off one control and typing it into another. Both rows now carry `values`
in the frame, and the page draws them the way it draws every other value row: one pressable line per
value, composed as `send + " " + value`.

That alone would have been a bug, and the near-miss is the interesting half. `values` had meant "a read
the page may ask for", and the report route runs its command **with the terminal quiet** — for
`/provider llamacpp` that is starting a local engine and printing nothing anywhere. So the class is
what routes the line: a value on a `panel` row is a read (`/report`, answered into the panel), and a
value on any other row is a line the page *types* for you (`/message`, answered in the transcript next
to the change). `/model [name]` also had to split into a report (`/model`) and a selector
(`/model <name>`), because one row cannot be both.

| Claim | How | Result |
|---|---|---|
| The switches are offered the names | a real `--web` process, a scratch `FLINT_HOME` with two providers | `{"label":"/provider <name>","send":"/provider","values":["stub","other"]}` and `{"label":"/model <name>","send":"/model","values":["stub-model","stub-other"]}`. Mutation-checked: emptying `/model`'s values fails on exactly that fragment |
| The list is the run's, not the page's | the same frame, and `ProviderConfig::choices()` | the model values are the active provider's, in its own order, active model first — the same function that fills the picker and `/model`'s own listing |
| A switch is not a reading | the same run, `POST /report` with `/provider other` | refused, `not a report`. Mutation-checked the other way: the route would have to admit a non-`panel` row before this could pass |
| The page routes by class | `tests/web_view.rs`, over the page's own bytes | `if (className === "panel") askReport(…) else sendText(…)` in the value loop; Node checks the same rows are drawn as controls either way |
| A selector with nothing to offer still says where its control is | Node | `/resume <n\|id>` is a `div.row.reference` whose hover says "a conversation on the left" — it is the one selector whose list is not in this frame |

**The key row names its provider.** `/provider key <key>`'s help in `/help` reads "set the API key for
the active provider", which is a phrase a browser's reader cannot resolve — and which provider the key
belongs to is the one thing they need before pasting a credential into a box. The frame substitutes the
name (`page_help`), and `tests/cli_output.rs` holds the line at exactly that substitution: a frame's
help must be the table's sentence or that same sentence with the name filled in, never a second
description that can drift from `/help`.

**Not measured**: a browser, as ever. What a reader actually makes of the panel's order — reports,
actions, selectors, forms, destructive — is unknown; the first person to open it asked why some rows
press and others do not, which is why the reference rows are now marked and explain themselves on
hover. Whether that is enough, or whether the groups should be ordered around a *task* (add a provider,
set a key) rather than around §8's classes, is the open question this round leaves behind.

### A form row asks for each answer the frame names — measured, 2026-09-15

§8's form class, for the rows whose answers the page may collect. A row may carry `fields`: a list, one
entry per word the line wants, each with the kind of input (`text` or `password`, passed straight to the
input's `type`), the argument's name for the placeholder, and whether the command works without it. The
process says all three, so the page is not deciding from a command's name that a key is a secret, nor
from its label how many answers it takes. The page composes the line as `send` followed by the answers
**in the frame's order** — a pure function, `formLine`, because the order is the command — and posts it
to `/message`, where a form's answer belongs. A form row with no `fields` stays a row of reference,
which is how `/provider edit` and `/config edit` remain the terminal's.

A row that takes one answer says what the command is *for* in that one field's placeholder, which is
what the help line is good for; a row that takes several has to say which answer each input wants
(`name`, `base_url`, `model (optional)`), because there is no room to explain three of them one at a
time.

**The refusal is the point.** An answer the frame did not mark optional and the page did not get stops
the line: nothing is sent at all. The bare form of a command is a *different* command — `/provider add`
with no arguments is the wizard, which asks a person for five things one at a time — so a page that sent
a partly-filled form would hand a served run to a wizard whose only reader is a terminal nobody is
sitting at. That is a hang, not a wrong answer, which is why it is asserted rather than reasoned about.

| Claim | How | Result |
|---|---|---|
| The frame says which rows take answers, of which kinds, and which are optional | a real `--web` process, the connect-time state frame | `/name [text]` arrives with `"fields":[{"field":"text","name":"text","optional":false}]`, `/provider key <key>` with the same shape carrying `"password"`, `/provider add <name> <base_url> [model]` with three entries whose last is `"optional":true`, and `/config set <key> <value>` with two whose value is optional; `/config edit` and `/provider edit` -- the wizards -- carry no `fields` at all |
| The config file is writable from the page, one setting per line | a real `--web` process, the frame, then `/config set max_steps 7` on `POST /message` | the frame carries the row with both answers as `fields` (the key required, the value optional, because a blank value is how a proxy is cleared); the line the page composes changes the file, and the conversation survives it -- which is the rebuild, since a setting assigned into the running config is one the tools never see |
| Adding a provider from the page writes one | a real `--web` process, the line the page composes on `POST /message` | the config file gains `name = "claw"` with the wizard's default model, the run switches to it (the frame's `provider` becomes `claw`), and the conversation file gains `"type":"switch","provider":"claw"` rather than a second file |
| Adding a name that exists is refused, not overwritten | the same run, the line posted twice | `provider 'claw' already exists — \`/provider edit claw\` changes one`, and the file holds one `name = "claw"` |
| A key typed into a field does not come back | the same run, `/provider key sk-not-a-real-key-0000` on `POST /message`, then the feed and the transcript | the answer says `key saved`, the `command` frame's `input` is `/provider key` and never the line, and the key is nowhere in the transcript — while `config.toml` *does* contain it, which is what stops the other three from passing on a command that never ran. Mutation-checked: echoing the raw line fails this, with the key visible in the frame |
| A refused key-carrying line does not come back either | `POST /report` with the same line | refused, and the refusal quotes `/provider key` rather than the key. Mutation-checked: quoting the refused line fails this |
| The page masks a key and keeps no copy | `tests/web_view.rs`, over the page's own bytes | each input's `type` from the frame's word, the `password` ones emptied on send, and the send going through `sendText` — which is `/message` |
| The answers become one line, and a missing one stops it | Node (`formLine` directly) and `tests/web_view.rs` (the wiring) | `["claw","http://…",""]` is `/provider add claw http://…` with the empty optional answer simply left off, and a required one that is empty is `null` — nothing sent. Mutation-checked both ways: deleting `if (!line) return;` from the page fails the policy test, and both Node and the policy test fail if the drawing stops reading `fields` |
| A form row without the mark is still a row | Node, over the stub DOM | the marked rows are `form` elements holding `input:text` / `input:password` with buttons reading `/name` and `/provider key`, and `/config edit` is a `div` |
| The page does not name a command to build the line | the frame, by construction | the button reads the row's own `send` and the answers are joined to it; the page has no list of commands and no table of which answers they take |

**Measured in a browser since, 2026-09-17**: the field is typed into and its submit button is pressed in
a real browser, with the masking, the emptying and the secret's absence from the page all checked there
— see `The later controls, in a real browser`. What is still not measured is the browser's own
password-manager behaviour, which is the browser's and not this page's.

### A conversation's own actions, one level down — measured, 2026-09-15

The last thing added to the page, and the only part of §8 asked for after using it: archiving or
removing a conversation meant opening the command panel, finding the destructive row and reading the
number off the sidebar by eye. The sidebar row is the conversation, so the row carries a `⋯` button
that opens a short menu — the panel's shape one level down — and the rows in that menu print the whole
line they will send before they send it.

| Claim | How | Result |
|---|---|---|
| The row has its own control, dim until the row is pointed at | Node, over the stub DOM | one `button` per row, reading `⋯`, titled "actions for this conversation"; no menu node while `doc.menu` is unset |
| The menu is the frame's rows, not a list in the page | `tests/web_view.rs` over the page's bytes, and Node | the items are `/archive 3` and `/delete 3` from a frame offering four danger rows — the `from: "providers"` one and the `selector` one are not there. Mutation-checked: asking for `"providers"` instead fails both |
| The second press sends the frame's line | Node, and by construction | `command.send + " " + session.n` — the line is on screen from the moment the menu opens |
| The menu belongs to one conversation | Node | a menu whose `id` is another row's is not drawn on this one |
| An empty menu says so | Node | a frame with no `from: "sessions"` rows draws `nothing to do from here` rather than an empty box |
| It cannot outlive its numbers | `web/view.html`, read | a `reset` clears `doc.menu`, and `readSessions` closes a menu whose conversation is gone — the same argument `doc.confirm` is built on, because the `n` in a menu is a position |

**The open conversation's name is edited in the same menu — added 2026-09-17.** The name is the one
thing the sidebar *shows* that nothing on the page could change: `/name <text>` has been in the frame's
forms class for as long as there has been one, and the panel's row for it can only rename the
conversation that is open — which is also the reason the field is drawn on **one** row rather than on
every row. `/name` names the conversation the run is writing, so a field on another row would either
rename the wrong conversation or would have to switch to it first: a second line whose refusal (the
session archived in the meantime) would leave the rename aimed at whatever was open. Click the row,
then rename it. The field itself is the frame's — drawn only when the frame describes a `/name` that
takes answers, with one input per answer it names — for the reason the destructive rows are: which
commands a run offers is a fact the frame owns, and a control for a command nobody offered sends a line
nobody can answer.

| Claim | How | Result |
|---|---|---|
| The field is on the open conversation, and nowhere else | Node, over the stub DOM | the menu is `[button, form]` on the current row and `[button]` on another, with the same frame |
| The field is the frame's own `/name` row | Node, over the stub DOM | a `/name` row with no `fields` draws no field at all, so a command that never said what it takes gets no control — the rule the destructive rows in this menu already follow |
| It starts on the name in force | Node | `input.value` is the row's label; for a row labelled `(empty)` — this page's placeholder for a conversation with no name — the field is empty, because sending it would make `(empty)` the name |
| A press composes the line the panel's forms compose | `tests/web_view.rs` over the page's bytes | `formLine(command.send, command.fields, …)`, one input per answer the frame names, and `sendText(line)` — the message route, not `/report`. Mutation-checked: deleting the `if (!line) return;` fails it |
| An emptied field sends nothing | Node, by delivering the submit to the page's own handler | the handler returns before it closes the menu, where a non-empty one closes it. Not politeness: `/name` with no text *reports* the name, so a cleared field would ask a question nobody asked |
| Typing in the field does not open the conversation | `tests/web_view.rs`, read | the form stops the click: the row behind it is the conversation, and for this row that is the conversation already open, so the press would send `/resume` for the row the name is being typed on |
| Renaming refreshes the sidebar | `tests/cli_output.rs`, a real `--web` process | a `/name` line posted to `/message` puts `event: sessions` on the feed and the title in the session file; red without the `list_changed` call |

**Not measured**: a browser, as ever. Nobody has opened this menu with a real pointer, so its position
(`absolute`, against a `relative` row), its dismissal (the button toggles it, a `reset` clears it, and
a click elsewhere does **not** close it) and its behaviour while the sidebar scrolls are reasoned
rather than seen. The dismissal is the one worth watching in a real browser: the panel's choice list
has an explicit `‹ commands` row to close it, and this menu has only its own button. The rename field
adds a second thing to watch there: whether the field keeps the focus it was given while the sidebar
re-reads itself (a `sessions` frame arrives the moment a rename is sent), and whether Enter in it is
the submit a person expects rather than the row's own click.

**`/config edit` is still the terminal's, and that is now a choice rather than a gap.** It is a wizard:
it asks four questions and waits for each answer, and a page cannot hold a conversation with a run
whose only reader is a browser. What it was *waiting* for was a one-line form of the same edit, because
"the config file cannot be changed from the page" is a different claim from "/config edit is not a
form". The terminal has that command now — `/config set <key> <value>`, whose keys are the four the
wizard edits and whose values are the only free-form part — so the page draws a field for the key and
one for the value, and the config file is writable from the browser by the same route as everything
else: `/message`, the line a person would have typed. Both front doors are one feature; the page is
still not handed the wizard, on the rule §8 is built on.

### The destructive class, and the two presses — measured, 2026-09-15

§8's last class, and the end of it. `/delete <n|id>`, `/archive <n|id>` and `/provider rm <name>` are
controls now, and the control is shaped like the promise: the row does not send, it opens its
candidates, and each candidate row prints the whole line it will send (`/delete 7`) — so the second
press is one that can be read before it is made. The process is unchanged: the line goes to
`/message`, exactly as if it had been typed, because a confirmation the terminal does not have would be
a second way to run `/delete`.

The frame had to grow one field it is otherwise proud of not having. §8 says a selector's options come
from a frame that already describes them and that the command list should not say where — the control
decides. That holds while the control is one control; it does not hold here, because the choices must
be drawn before anything can be confirmed and the two lists are different ones. So three rows carry
`from`, and a page that could not see it would have to recognise `/delete <n|id>` by name.

| Claim | How | Result |
|---|---|---|
| The frame says which list, and only for the three rows | a real `--web` process, the connect-time state frame | `/delete <n|id>` and `/archive <n|id>` carry `"from":"sessions"`, `/provider rm <name>` carries `"from":"providers"`, and the frame contains exactly three `"from":` — a fourth would have the page offering candidates for a command that reads. Mutation-checked: marking every row `providers` fails it |
| The first press sends nothing | `tests/web_view.rs`, over the page's own bytes | the branch that opens the choices contains no `sendText`, checked at the shape of the branch rather than by eye. Mutation-checked: drawing no choices at all fails the Node check below |
| The choices are the lists the page already holds | Node, over the stub DOM | opened on `from: "providers"`, the panel holds a `‹ commands` back button and one `row danger` per provider, each reading `/provider rm stub` — the frame's `send` plus the frame's name |
| A choice list with nothing in it says so | Node, the same check, opened on `from: "sessions"` with no list | one row reading `nothing to choose from` rather than an empty list that looks like one with nothing in it |
| An open list does not outlive its numbers | `web/view.html`, read | `readSessions` redraws the panel when a choice list is open, and a `reset` clears `doc.confirm` — `/delete` in the terminal shifts every number below the one that went, and the numbers in a list are positions |

**Measured in a browser since, 2026-09-17**: the first press is made with a real pointer on a real
`/delete <n|id>` row, its candidates are read, and the way out is pressed — with the *nothing sent* half
asserted against the run's own output and the sessions still on disk afterwards. See `The later
controls, in a real browser`. The second press is not made **in the panel**, and that is on purpose: it
would delete a real conversation, and what the two-press shape promises is exactly that the first press
is not it. The second press *is* made from the sidebar's own menu, on a conversation the harness put
there for the purpose — which is the one place a deletion is a fair thing to ask a browser to do, and
it is checked against the directory listing rather than against the page.

### The later controls, in a real browser — measured, 2026-09-17

§11's own list of what a browser had never seen, closed one control at a time: the switches, the command
panel, an action button, the masked credential field, and a destructive row's menu. Driven over the
Chrome DevTools protocol from `scripts/browser-controls-test.js` — node's own `WebSocket`, so no package
is installed for it, and headless Chrome on Windows at 1374×800 — with the page's real font, a real
window, and real input events: `Input.dispatchMouseEvent` at each element's own box and
`Input.dispatchKeyEvent` for the switch.

The same harness then closed the residue this section used to carry — the sidebar's own `⋯` menu, both
drag grips, and a picker from the keyboard — so it is 34 claims rather than 20 and no control of the
page's is left asserted-but-undriven. Those are the last six rows of the table below.

**Every claim is checked against the run's stdout, not against the page.** That is the whole method: a
click that sent nothing leaves the page looking exactly like a click that worked, so the witness has to
be the process on the other end of the socket. The harness also keeps the browser's console and network
traffic, which is what turned the first failure from "the controls never appeared" into a 500 on
`/session`.

| Claim | How | Result |
|---|---|---|
| The switches are drawn from the run's own state | the header, read from the page | `verbose`, `detail`, `readonly`, `hear-peers` — the frame's four, in its order |
| A switch moves the control and the run together | `ArrowDown` on the focused `readonly` select | the page's value changed *and* the run printed `/verbose full …` — the same line the keyboard path sends, because a native select opened by a pointer is an OS widget no protocol can reach into |
| The panel opens with a click, and lists the run's commands | a real click on `commands`, then the panel's text | open, with rows from all five classes including `/config`, `/provider key`, `/delete <n|id>` and `/name`; the form rows appear as their submit buttons, which is why the check reads text rather than `code` elements |
| The run's actions are buttons in the header | the same frame, `#actions` | `/new` and `/reload` — not rows in the panel, which is the design and was worth writing down |
| A report is answered in the panel and nowhere else | a real click on `/config`, then the panel and the run's stdout | the listing was drawn in the panel and the terminal gained **not one byte** — the quiet-answer channel, seen from a browser for the first time |
| An action button runs the command | a real click on `/reload` | the run printed `reloaded` |
| The credential field is masked, and the secret does not come back | typing `sk-not-a-real-key-0000` into the `/provider key` field, then its submit | the input is `type=password`, the run printed `key saved`, the field was emptied, the secret is nowhere in the page's markup — and `config.toml` *does* contain it, which is what stops the other three passing on a command that never ran |
| A destructive row opens its candidates instead of sending | a real click on `/delete <n|id>` | candidates drawn, the run's stdout gained nothing, and backing out left the sessions directory byte-identical |
| The composer sends a line the run answers | `/usage` typed into the box, then a real click on `send` | the run printed, and the answer was in the transcript |
| A conversation's row opens its own menu, and the press that opens it sends nothing | a real click on the row's `⋯`, then the terminal | the menu's rows are the frame's destructive commands with *this row's* number appended, the open conversation's menu carries the `/name` field and no other row's does, and the run's stdout gained nothing until a row was pressed |
| A name typed into that field reaches the run | `Input.insertText` into the menu's own field, then its submit | the run printed `named: named from the sidebar` |
| The second press in a row's menu carries that row's number | a real click on the `/delete` row of a conversation the harness created | that session file was gone from `sessions/`, read off the directory rather than the page — the terminal would agree with a menu that had sent the wrong number and been refused |
| The sidebar's hand takes a real drag, and the arrow keys and a double-click belong to the same control | a pointer press, four moves with `buttons: 1`, a release; then `ArrowRight` on the focused hand; then two press/release pairs with `clickCount` 1 and 2 | `--side` grew by the drag, grew by 16 with the arrow, and the double-click removed the property rather than leaving a number — with `body.dragging` asserted *during* the drag, which is the page saying it accepted it. Mutation-checked: neutering the page's `pointermove` handler fails this row and the one below it (32/34) while the arrow-key and double-click claims still pass, so the two halves are independent |
| The reading hand is the same control on the other boundary | the same gestures on `#read-grip`, dragging left | `--read` shrank by the drag, and its double-click reset its own width — and, as the check asserts, nobody else's |
| A picker moves from the keyboard, and the run is told which model | focus `#pick-model`, `ArrowDown` | the page's value changed *and* the run printed `ok model …` for that value. The open *popup* is still the OS's and still unreachable, which is the residue below |

**Two defects, and both were unreachable from the source.** The first: a run with `--web` that has not
been spoken to yet answered **500** on `/session` — the run names its session when it starts, the *file*
is created by the first thing said in it, and `serve_session` treated the missing file as a fault. The
page cannot finish drawing until `/session` has answered, so a fresh `--web` showed a page with no
controls at all, and retried the feed forever. Fixed in `src/web.rs`: a file that is not there yet is an
empty conversation, which is what an empty file would have been; every other read failure is still a
500. Held by `a_session_file_that_does_not_exist_yet_is_an_empty_conversation`, which was watched fail
with the raw `os error 3` first.

The second: **with the `commands` panel open, the `send` button could not be clicked.** The panel is the
one thing in the header that grows without bound — a row per command the run offers — and on a pane
800px tall it took 742 of them and pushed the reading and the composer into the same 54px, where
`elementFromPoint` at the send button's own centre answered `main#transcript`. Two fixes, cause and
guard: the list is now bounded and scrolls inside itself (`.panel[open] .body { max-height: 40vh }`),
and the composer is `position: relative` **without a `z-index`**, because the reading is positioned (the
hand inside it is positioned against it) and a *static* composer paints below it wherever they overlap —
tree order puts the later sibling back on top, and a number would only be a number to escalate against
the sidebar's own menu. The harness's check was red before the fix and is the reason to believe it.

**What this does not cover, and it is one widget and one section now.** A native `<select>`'s open
*popup* belongs to the operating system, so no protocol can reach into it; the picker itself is driven
from the keyboard, which is the path a person takes through it, and what it sends is checked against the
run. Long answers and reconnection, which are §11's own performance section and were measured on macOS
over a different harness, are the other half — and the composer at the *keyboard* was measured in the
earlier pass, with the button driven here.

---

## 12. A file the transcript names, opened beside it

Asked for directly, and the complaint was precise: *"the file paths in the conversation are plain text
and cannot be clicked, which is a nuisance."* Everything a run does leaves paths in the transcript — the
arguments of a `read`, the `notes.txt:2:` lines a `grep` prints, the name at the end of a `write` — and
the only way to see any of them was to leave the browser, find the file by hand and open it in an editor.
DSH's page answers the same complaint its own way (`dsh-client-ui-reference`: an `@` mention, pressed,
opens the file in the right sidebar as a document preview). What follows is that idea, with none of the
machinery: no tab system, no renderer registry, no file tree — one route, one panel.

**A path is a button, and the panel is a column of the layout.** Three decisions carried it:

- **`GET /file?path=`, not a route path.** The route table stays literal and tiny, which is §6's whole
  argument; a path parameter is a *parameter*, not a new shape of route, and the route still takes no
  path from the URL's structure — so §6's "nothing is served from disk" line stays true in the form that
  matters (nothing is served that a *tool result already in the transcript* did not name).
- **Relative paths resolve against the run's working directory**, which is the only reading that can be
  right: the transcript's `notes.txt` is the `notes.txt` of the process that wrote the line. It arrives
  in `Viewer::asked` from `agent.cwd()` — a `--cwd` moves the meaning of every relative path in a
  conversation, and this follows it rather than guessing from the page's own URL.
- **The panel is a grid column, and a browser is what settled that.** The first version was
  `position: fixed` over the right edge, on the argument that a column would reflow the reading. The
  harness rejected it in one press: with the panel open, `elementFromPoint` at the *next* path's centre
  answered `pre#preview-text` — the panel covered the block it had just been opened from, so the second
  path in a turn could not be pressed at all without closing the first. A column narrows the transcript
  instead, which is also what DSH's sidebar does. The check that found it is in the harness (the same
  coverage check every press goes through), and it is the reason this feature has a claim about pressing
  *two* paths and not just one.

**What a cut file says, and where it says it.** The preview cap is 512 KB of a file that may be any
size, so the response carries `X-Flint-Cut` (what was sent) and `X-Flint-Size` (what the file is), and
the *page* draws the note — "the first 512 KB of 40 MB" — beside the path. The alternative, a sentence
inside the body, would be text the file does not contain in a `<pre>` that promises the file's own
bytes, which is the same fault as a Markdown renderer that invents markup. Nothing is refused for
being long: half of a 40 MB log, honestly labelled, is what a reader wants. What *is* refused is a file
this page cannot show as text: not valid UTF-8, a directory, or over 64 MB (the one case where reading
it is the wrong thing to do even to show a little).

**The capability question, answered rather than dodged.** A token holder can already `POST /message` and
run the agent, so serving any file the user can read adds no capability whatsoever — §4's boundary is
unchanged. What was refused on purpose is a cwd jail: it would be a boundary that *looks* like one, and
the honest statement is the one §4 already makes. The page's own rule is unchanged too — every piece of
a path still goes in through `textContent`, because a file called `<script>` is a filename here like any
other, and there is a policy test (`a_path_opens_in_this_page_or_not_at_all`) that forbids the two ways
a path could escape the page: navigating to it, and `window.open`.

**Where the line between a path and prose is drawn, and why it is a rule and not a guess.** A candidate
is a run of characters with no whitespace, quote, bracket, comma or backtick in it; it is a path if it is
absolute, or its last segment has an extension of two to eight characters, or it has three or more
segments. The two edges that decided it: `e.g.` and `i.e.` are why a one-character "extension" is not
one (`foo.c` is the price), and `and/or`, `read/write` and `24/7` are why one separator with no extension
is not one (`src/bin` is the price). A `//` means a URL. A trailing `:line` or `:line:column` is the line
a `grep` hit was on and comes back as a line, not as part of the name. **Only tool blocks are split this
way** — prose is where those abbreviations live, and a linkifier that guesses turns a sentence into a
wrong file. Every one of those edges is a check in `scripts/web-view-test.js`, and one of them was a real
bug caught there: `segments.slice(1).every(…)` is `true` for a name with no separator, which swallowed
every bare filename until `Cargo.toml` was in the test.

**What was measured, in a real browser (2026-09-17).** Ten claims in
`scripts/browser-controls-test.js`, against a run whose turns are scripted by a stub model the harness
starts itself (the same SSE shape `tests/task.rs` serves, so a browser claim can be about a *turn* —
a tool call, its result, and the prose that ends it — rather than about an empty transcript). The
transcript held a `write`, a `read` of that file, a `read` of a file that is not there, and a `grep`,
which is four shapes of path:

| Claim | How | Result |
|---|---|---|
| A tool block's paths are buttons, and a grep hit carries its line | the blocks' `.path` buttons, after a scripted turn | four buttons; `notes.txt`, `gone.txt`, and one whose `title` is `notes.txt:2` — the line, in the tooltip rather than in the label |
| Pressing a path shows the file the run just wrote | a real click, then the panel | `one\ntwo\nthree\n` — the *file's* bytes, read over `GET /file` from the run's cwd, not something the page worked out |
| The page stays where it was | `location` after the press | `/?token=…`, unchanged: the file did not navigate the page |
| The note says the size | the panel's header | `14 bytes` |
| Pressing a path inside a block does not fold the block | the `details`' `open`, before and after | `false` both times: the press is `preventDefault`ed, so the block does not slide away under the file it just opened |
| Reload reads the file again | the file rewritten on disk, then the button | `four\nfive\n` and `10 bytes` — the new bytes, not the ones the page already had |
| A hit inside a block's output is there to press once the block is open | the grep block's `summary`, then the hit | closed → open, and only then was the hit's centre actually on screen — the check that found the fixed-panel defect |
| A hit's line travels with the path | pressing `notes.txt:2` | the note reads `line 2`, and the file's bytes are beside it |
| A path that is not there is refused in the route's own words | pressing `gone.txt` | the panel holds `nothing at gone.txt: …` with `HTTP 404` beside it, rather than an empty panel |
| Escape closes the panel and leaves the reading alone | a real `Escape` | hidden, with the transcript still there |

**Two residues, both deliberate.** A file that is not valid UTF-8 is refused rather than shown as
replacement characters: a lossy conversion would print something no editor would show, and "this is not
text this page can show" is a better answer than a screen of U+FFFD. And there is no highlight on the line a
hit came from — the panel scrolls to it and says which line it is, and a highlight would be a second
render path over the file's own text.

---

## 13. The run's background work, on the page

The complaint was the plainest kind: *"现在看不到子代理和后台任务的情况，在 web 页面上面"* — a run's
children and background commands were invisible in the browser. The terminal has one line for them (the
notice a job leaves when it ends, and `job_op` for whoever remembers to ask); the page had nothing, so a
build started and forgotten, or a child left running, could only be found by reading the session file or
asking the model to interrupt itself.

**The shape is DSH's job popover, and the two things adopted are the two that are about a person.** DSH
puts a jobs action in the session header that appears only when there is at least one job, opens a list
of them on a press, sorts running work first, badges each row with its kind, ticks the duration of the
live ones, and closes on `Escape`. All five are here. Refused: the live-tail of a job's output inside the
list. That is a reader with a scroll position and a growth rate of its own, and the page already has one
place a job's output can be read — the preview column §12 built, which a row opens.

**One record, no second bookkeeping.** `GET /jobs` answers with `tools::jobs_snapshot()`, read off the
same `Job` records `job_op` answers from — the same `finished`, the same `log`/`session`, the same
`started` — so the page and the tool cannot describe one job two ways. Three fields exist because the
page is not the model:

- **`started_secs`/`ended_secs` are absolute epoch seconds**, derived at read time from the job's
  `Instant` (`SystemTime::now() - elapsed`) rather than stored: a stored second field would be the same
  fact recorded twice, and the point of the absolute clock is that a page which has been open for an
  hour still shows the true age of a job it heard about when it opened. Ticking is the page's own
  arithmetic — one `setInterval` that rewrites the `.when` text of the *running* rows, no request.
- **A status word beside the fact.** `running`, `completed`, `killed`, `failed`, from the exit code
  alone (`job_status`), with `exit code 0 (finished)` beside it in the row. A kill and a failure look
  identical from outside, and a row that said "failed" for a kill sends somebody looking for a bug that
  is not there.
- **`path`**, so a row is a door: a command's log or a child's conversation, opened through the same
  `GET /file` a path in the transcript uses. Nothing is drawn pressable when it has nothing behind it —
  a child that has not named its session yet is a row, not a button.

**The frame is a revision, not a list.** `tools.rs` keeps one `AtomicU64` that changes when a job is
registered and when one settles; `web.rs` compares it (`send_jobs_if_changed`) and sends an empty
`event: jobs` when it differs. Three properties fall out of that and each is the reason for the choice:

- **The list is a route.** A frame carrying the jobs would be a second answer that can disagree with
  `GET /jobs`; empty data means "re-read it", exactly like `event: sessions`.
- **A counter, not a flag.** A reader that missed one change must still see the next, and a boolean
  cleared by two readers can lose one. Two changes that arrive together are one frame, which is what the
  page wants anyway.
- **The tool code grows no way to reach a connection.** The counter is the whole interface; nothing in
  `tools.rs` knows a listener exists, which is why this works for `--web` and `/web` alike and why
  nothing had to be threaded through `main.rs`.

The revision is compared on every frame the connection forwards — so a job started by a tool call
appears *during* the turn that started it — and on a **four-second timer** (`JOBS_POLL`), which exists
for the one change with no traffic to ride on: a background command ending while the run is idle. A page
that is opened while jobs are running is told at connect, because the counter starts at zero for that
connection.

**What was measured, and what the measuring found.**

- `tools::tests` — `a_jobs_state_word_is_its_exit_code_read_the_way_a_person_reads_it` (pure), and
  `a_job_is_snapshotted_while_it_runs_and_again_when_it_has_ended`, which starts a real background
  command and asserts the row's kind, its label, the log path, and that the start is on the wall clock;
  then waits for it and asserts the same pid with `completed` and an `ended_secs` after its start. This
  test caught a real design mistake: the first version sent a `tool` field read from `job.label`, which
  for a *command* is the command line — so every command row would have carried the same string twice.
  The field was removed rather than filled in: the kind badge already says `command`, and the label is
  what was asked.
- `tests/cli_output::a_background_command_is_a_job_the_page_can_watch_end` — the real binary, a stub
  model that calls `bash` with `background: true`, a live `/events` connection, `GET /jobs` while it
  runs, then again until it settles, then the log's own bytes. Two `event: jobs` frames are asserted,
  and the second one is load-bearing: with the settle-time `jobs_changed()` removed the test fails with
  "the end of the job was never announced", because the forwarded-frame and timer paths see a revision
  that has not moved.
- `scripts/web-view-test.js` — the route's shape (`jobsFrom`), the two words and the clock
  (`jobWhen`/`durationLabel`: `running for 0s` → `1m 12s`, `took 4s`, and no invented duration for a job
  whose times are missing). Mutation-checked: changing the minute boundary in `durationLabel` fails
  exactly this check.
- `scripts/browser-controls-test.js` — nine claims, against a scripted model that starts **both** kinds
  of job (a fifteen-second `node -e` command and a `task` child). 53/53 held. The claims that could not
  be made any other way: the duration *ticks* (the same row's text changes while nothing is fetched),
  a settled row carries `exit code 0` and `took Ns`, a child's row opens the child's own conversation
  (`…/children/<stamp>.jsonl`, with `say hi` in it), and a command's row opens its log — first with the
  line it had printed while it was still running, then, pressed again after it ended, with both lines.

Two of those claims were written wrong and are worth recording, because each looked like a page bug and
was not. The first looked for *any* settled row, and the child settles seconds before the command does —
so "the job ended" was true, and the log it then read was still half-written. The second marked a row
with an `id` and never cleared it, so the second press found the *first* row by `querySelector` and
re-opened the command's log when the claim was about the child. A harness that asserts on a list needs
to say *which* row it means, in both directions.

**Restraint, and the honest limits.**

- `stopping` is not a status. A stop is a write to a child's stdin or a kill, and neither has a state
  the `Job` records until it has ended; a status word invented to fill that gap would be a claim the run
  cannot back. The row says `running` until it is not.
- `KEEP_FINISHED = 8` is the panel's horizon, and it is the process's, not the page's: a job that fell
  off the end is still in the session file it wrote, and there is no persisted job list to grow into a
  second history — the same rule as everywhere else here.
- A job started by an *earlier* run of flint is not in this list. A child's own conversation is on disk
  and `flint who` names the run, but "the jobs this process started" is what a handle is, and pretending
  otherwise would mean deriving a list from files, which is the thing this repository does not do.
- The panel is in the header, so it is bound by §11's rule: `max-height: 40vh` with the list scrolling
  inside itself, because a reference list must never be able to push the composer off the screen.

### Stopping one, and the word a kill is reported under — measured, 2026-09-17

The listing made the run's work visible; the same complaint had a second half, because seeing a job you
no longer want is only useful if you can end it. Until this round the *only* door onto that was
`job_op`'s `stop`, which is a tool: a person who started a ten-minute build by accident had to ask the
model to stop it, or kill flint and take the child with it. So the round that added the panel and the
listing added the door beside them, in the two places a person already is.

**The terminal gets two commands, and they are the two halves of the record.** `/jobs` prints
`tools::jobs_report` line by line; `/jobs stop <pid>` ends one. Both are the same functions `job_op`
answers from -- `jobs_report(None)` is what `action: "status"` returns, and `stop_job(pid, None)` is
what `action: "stop"` calls -- which is the property worth having rather than three sentences that agree
today: a person and a model cannot come to disagree about what is running or about what happened when
somebody stopped it. The one place the two doors deliberately differ is the argument. `job_op stop`
with no pid acts on the only job in play, because a model that has just started one is unambiguous;
`/jobs stop` with no pid is **refused**, with a sentence pointing at the listing, because a person
typing a kill is naming what to kill and the wrong guess is irreversible. A pid that is not a number or
a third word is refused the same way.

**The page's door is the row that was already there.** `/jobs stop <pid>` is a `destroying` command, so
it renders in the `danger` group and takes two presses -- and the second press offers *candidates*, which
the page takes from `ArgFrom::Jobs` resolving to the jobs panel's own rows. That is the third candidate
list (after the sidebar's session numbers and the state frame's provider names) and it is the one that
shows the pattern was worth having: the pids a person is looking at *are* the values the command takes,
so nothing new had to be sent and the panel did not need a second representation to be aimable. Two
rules come with it, both asserted in `tests/web_view.rs`: only the **live** rows are offered, because a
job that has ended cannot be stopped and a choice whose only outcome is the sentence saying so wastes a
press; and each choice's label is `kind + " " + label` rather than a bare pid, because two numbers in a
menu are a menu nobody can use.

**A kill's exit status is the shell's, not the command's, and the panel was reporting it as a
failure.** This is the part of the round worth reading, because the code that fixed it is three lines
and the reasoning is the whole reason it is three lines. Windows `taskkill /PID … /T /F` leaves the
`cmd.exe` it signalled reporting **1**, so a background command a person stopped deliberately was
listed as `failed` -- the one word that sends somebody looking for a bug that is not there. On Unix the
same stop reports `-1` (a signal) and read correctly, which is exactly the kind of difference a status
word must not depend on. The first version of the e2e test failed on the *reason for the right* answer
being absent: `{"detail":"exit code 1 (failed, cause not classified)","status":"failed"}` where the
claim was `killed`.

The fix records the decision where it is made rather than inferring it from a number afterwards: `Job`
gained `ended_by_us`, set in `Job::kill()` **before** the signal goes out (the supervisor records the
exit status as soon as it has one, so a flag set afterwards would sometimes lose the race it exists to
win) and in the budget-expiry branch, and the supervisor consults it when it records the status. The
budget ending a command is deliberately the same word: the fact a reader is looking for is "did it stop
on its own, or was it stopped", and the note the budget leaves behind is what says why. So `killed` now
means *this run ended it*, and it is true on both platforms for the same reason.

**What was measured.**

- `tools::tests` -- `a_job_a_person_stops_reads_as_stopped_and_says_what_the_model_would_be_told`
  starts a real background command, stops it through `stop_job`, and asserts the sentence ("killed it,
  and it is gone"), the row's `status == "killed"` with `exit code -1` in its detail, and that the text
  the *tool* would have returned is the text `/jobs` prints (`via_tool.trim() ==
  jobs_report(Some(pid)).trim()`), which is the "two doors, one answer" property held to the byte rather
  than asserted in prose.
- `tests/cli_output::a_person_can_read_the_run_s_jobs_and_stop_one` -- the real binary, a stub model
  that starts a background command, and the page's own routes: it reads the opening `state` frame and
  asserts `/jobs` and `/jobs stop` are on it with `panel` and `jobs` as their `from` (parsed out of the
  frame, so a command that stopped being page-reachable fails here and not in a browser), reads
  `GET /jobs` for the pid, reports `/jobs` and asserts the panel names that pid and says `running for`,
  then stops it and polls until the status is no longer `running`.
- `tests/web_view.rs` -- `the_page_stops_a_job_from_the_rows_it_is_already_showing` pins the four
  load-bearing expressions of the candidate list (`command.from === "jobs"`, `listedJobs`,
  `jobIsLive(job)`, `String(job.pid)` and the label) inside the `const choices` body.
- `scripts/browser-controls-test.js` -- three more claims in a real browser against the scripted model,
  which now starts a **second** background command (an endless `node -e` that prints one line and then
  sleeps for a minute, so there is something to stop that no other claim is waiting on): the stop row's
  candidate list is the panel's own rows (`/^\/jobs stop \d+$/`), pressing it leaves a row the page
  classifies as `killed` with `exit code -1` in its detail, and the transcript then says "killed it, and
  it is gone". **56/56 held** (53 before).
- Mutation-checked, both directions. Changing the page's `command.from === "jobs"` branch makes the
  harness fail with "timed out waiting for the /jobs stop row" -- the candidate list is load-bearing,
  without it the row renders as an unpressable reference. Making `was_ended_here()` return false makes
  the lib test fail with `left: String("failed") right: "killed"` -- the flag, not the exit code, is what
  the status word is read from.

**Restraint, and the honest limits.**

- **The panel still has no kill control, on purpose.** The row is a door (it opens the log or the child's
  conversation), and a second gesture on it would have to be a different gesture on the same target --
  exactly the ambiguity that made a browser harness assert on the wrong row in the previous round. The
  person's stop is a command, and the command is reachable from the page through the same command panel
  as everything else.
- `stopping` is still not a status, for §13's original reason. A stop is a write or a signal, and the
  row says `running` until it is not -- which is now also why the *candidate* list is filtered to live
  jobs rather than the row being rewritten.
- **The 20-second window is flint's, and a job that outlasts it says so.** A child asked to stop may
  finish the thought it was on; a command whose kill has not landed yet is reported as exactly that,
  with the job's own budget named as what ends it either way. Nothing waits forever, and nothing claims
  a job is gone because a signal was sent.

