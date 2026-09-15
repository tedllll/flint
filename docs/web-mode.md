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
measured in a browser versus what has not. What is left is not a level but a polish list: §8's
"not doing" is still not being done.

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
reading would have. Everything about the *listener* — §4's boundary, §6's routes, and every
question about backpressure and reconnection — is still unmeasured, because the listener does
not exist yet.

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

Four routes. No cookies, no HTTP/2, no TLS, no keep-alive, no streaming request bodies.

| Route | Returns |
|---|---|
| `GET /` | the embedded HTML. Requires the token. |
| `GET /session` | the session so far, as the session file's own lines (`NDJSON`) |
| `GET /events` | SSE: replays from `Last-Event-ID`, then live events |
| `POST /message` | one message from the browser, into the steering channel |
| `POST /report` | one command *read* from the browser: same channel, and its answer is captured with the terminal quiet (§11) |
| `GET /sessions` | the conversations `/resume` can reach, numbered the way `/resume` numbers them |
| *`event: sessions`* | not a route but its counterpart: the list has changed, re-read it |
| *`event: state`* | the run's own configuration: provider, model, what each provider offers, the toggles, and the command list with §8's class for each row — a row also carries `values` when the page may read that command with an argument the frame names (§11) |
| `type: command` | what a command answered, on the same stream as the turn's events: `input` and `text` — which is also what a header button's answer arrives on. Carries `panel: true` when the page asked to read it rather than typing it |

**All six are implemented.** `/session` and `/events` read the session path and the event feed
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

**Not yet measured in a browser**, and it is the next thing to look at: the controls were checked as
*behaviour* (which options, which value, what a change sends) and as bytes (the markup starts
hidden, one line per control, the frame applied where it arrives), but nobody has looked at the
header with a real font, or used a picker or a switch from the keyboard. Everything in this section
that says "measured" means a process and a socket; §9 step 7 is still the rule for the rest.

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

**Not yet measured in a browser**: nobody has watched a `/config` typed into the composer land in the
transcript with a real font and a real scroll position. The block is drawn like a user message with
the command as its label, and that is a guess until someone looks at it.

### The command list, and the panel drawn from it — measured, 2026-09-15

The read channel needed one more field before a control could be drawn from it: *which* commands
there are. `/help` printed a hand-written block, dispatch is a `match`, and a third copy for the
frame is exactly the drift this project spends its comments preventing, so the list became one table
in `src/main.rs` (`COMMANDS`), with `/help` printing it and the frame carrying it.

An entry is `{label, send, help, class}`, and later `values` on the rows a page may *read* with an
argument (§11's last section). `label` is what `/help` prints (`/provider key <key>`); `send` is what
the page puts on the wire (`/provider key`) — carried beside the label rather than split out of it,
because the split is not uniform (`add` is part of `/provider add`, `<key>` is not) and a page
re-deriving that would be re-deriving this program's grammar. `class` is §8's own, and two kinds of
row are *not* in the frame: the toggles (the `toggles` field already carries them in the shape a
switch needs) and the terminal's own — `/exit`, `/web`, `!`, `/stop`.

| Claim | How | Result |
|---|---|---|
| The page is told what commands exist | `/events` on connect, read off a real `--web` process | twenty rows, each with `label`, `send`, `help` and `class`; `/exit`, `/web`, `!<command>` and the three toggles absent. `send` is what the page posts, so nothing in the page assembles a command line out of parts |
| The two lists are one list | the same test reads the frame off `/events` *and* `/help` off the run's stdout | every row the page was handed is a row `/help` prints, with the same description. Mutation-checked: dropping the destructive rows from `/help` fails it on `/provider rm <name>` |
| Every offered command exists | the frame's own `send` strings posted to `POST /message`, the answers read off the feed | no `unknown command` for any `panel` or `button` row, and a floor on how many rows were checked. Mutation-checked: `/tools` → `/tool` in the table fails it naming that row |
| The help is generated, and unchanged | the `/help` output captured before the rewrite, diffed after | byte-identical, `/stop`'s hand-made continuation included (a greedy wrap at the width the column leaves reproduces the same break) — with one deliberate change, `/config [edit]` split into `/config` and `/config edit`, because one row cannot carry two classes |
| The panel is drawn from the frame | `showCommands` under Node, over the stub DOM | one group per class the page was taught, in the page's reading order (reports, actions, selectors, forms, destructive), each row showing what to type and what it does; a frame with no command list takes the panel away |
| The page holds no copy of the list | `tests/web_view.rs`, over the page's own bytes | the panel is built from `state.commands`, and `/provider key`, `/delete <n|id>` and `/reload` appear nowhere in the page |

The panel was deliberately not a control at this point: it listed the rows and grouped them, and
clicking one did nothing. Both halves have since been built — the header's buttons send, and a report
row is read — and this paragraph is left as the record of what this commit measured.

**Not yet measured in a browser**: nobody has opened the `commands` panel with a real font and read
it against `/help` in the terminal, or used it while an answer was streaming. It is pinned as
behaviour and as bytes, and that is all it is pinned as.

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

**Not yet measured in a browser**: nobody has pressed one. What a press does is pinned as bytes and
what comes back is pinned end to end, and nothing here says how the button looks or where it lands
under a real cursor.

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

**Not measured**: a report asked for *while a turn runs*. The wait is stashed rather than treated as
an interrupt (`Handover` in `main.rs`), and the assertion that it waits needs a stub turn slow enough
to click during — which the suite does not have yet. And no browser, as ever.

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

**Not measured**: a command whose values the frame does not carry gaining a control. Nothing does
today — `/provider <name>` takes a value and its list comes from `providers`, deliberately, because
running *that* quietly would start a local engine without printing a word — and `values` is where a
second such row would go.

