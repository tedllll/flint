# Web mode: a window onto a running flint

A browser is a better renderer than a terminal for one thing flint already does: reading a
run. Collapsible tool calls, a transcript you can scroll back through without the scroll
region fighting you, and text you can select and copy whole. This document is the design for
getting that without giving up what makes the terminal version dependable.

The decision at the centre of it: **`--web` is a window onto the running process, not a
second mode.** flint starts exactly as it does now and additionally serves a view of *this*
process on loopback. The terminal keeps working. The browser is a client of the process, not
of a file, and closing the tab loses nothing.

**Steps 2 to 5 of §9 are implemented** — the static viewer, the `--web` listener with `GET /`,
and the two data routes `GET /session` and `GET /events` — and §7 is now decided. Step 6
(`POST /message`) is not: **the page is a viewer and has no input box**, so something typed into
a browser has nowhere to go and the terminal is still the only way in. §11 records what has been
measured in a browser versus what has not. The order at the end is the plan.

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
| **L3** | `POST /message` into the steering channel | one more route | Yes |

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
| `POST /message` | one message from the browser, into the steering channel **(not built)** |

The first three are implemented; the fourth is L3 and is the only route missing. `/session` and
`/events` read the session path and the event feed through a shared handle, which is what lets
`/new` and `/resume` move an open window to the conversation the terminal moved to.

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
   test: a message typed into the browser steers a turn that is already running.
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
| A narrow window | **not measured** | 1100px is what the fixture was taken at; nothing below it has been looked at |
| A long turn, and backpressure | **measured**, and it found the limit of this design — see below | a stub provider on loopback, a real turn, `curl -N /events` |
| Reconnect, replaying exactly once | **measured** against the ring and the reset boundary | a cursor older than the buffer gets `event: reset` and re-reads `/session` |
| A page opened *during* a turn | **measured**, with a defect found and fixed | the status was blank; it is now sent as state on connect |
| The status line during a slow model call | **measured** | the browser shows `no response yet — the network or the endpoint may be stuck`, the terminal's own words |
| A turn on a browser that then reloads mid-turn | **not measured** | |
| What 80 columns looks like next to a terminal | **not measured** | |

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
