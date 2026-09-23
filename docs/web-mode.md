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
a polish list, named at the end of §11: the page's own long-answer and reconnection measurements —
and the one widget that *was* on that list, a native `<select>`'s open popup, is gone with §25, which
replaced the last `<select>` on the settings surface with the frame's own words as buttons. §8's
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
reading would have. The same has since been done to every control in §8 — driven over the
DevTools protocol by `scripts/browser-controls-test.js`, which found two more defects, one of
them a page with no controls on it at all — and then, in the same harness, to the sidebar's own `⋯`
menu, both drag grips and a picker from the keyboard, which were §11's last "reasoned rather than
seen" residue — and, in §25, the settings door's own seat and the preview's third hand. What remains
unmeasured is one section, named at the end of §11: the page's long answers and reconnection. The *listener*'s own questions — §4's boundary, §6's routes, backpressure and
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
- **An address is the one thing that is pressable, and the two kinds are different on purpose.** A
  *web address* becomes a link in a new tab (`target="_blank"`, `rel="noopener noreferrer"`): this
  document is the conversation — its token, its stream, the reader's place — so following a link
  inside it would throw all three away, and a new tab with the opener severed is the one way to open
  a page without doing that. A *path* stays a button that opens the preview panel, because a file has
  no address a browser may open from a page served over http; `GET /file` is that door (§12). The
  scheme test is an **allowlist** (`asUrl`), and it is the security boundary rather than a nicety:
  the `href` is set as a property from a model's words, in a document holding the run's token, so
  `javascript:` would be script here needing no bug — only the click the reader was going to make.
  `http` and `https` are links; `javascript:`, `data:` and `file:` stay the words they are. Prose is
  read with the narrower path rule — absolute paths only — because a sentence is where `src/bin`,
  `and/or` and `e.g.` are all just words (§11, measured).
- **One file, no build step.** No pnpm, no vite, no TypeScript, no client plugins, no HMR.
  The HTML is a text file in the repository that a person can read and repair, embedded with
  `include_str!` so the binary stays one file. A UI layer that needs a build pipeline would
  end the "state is plain text and hand-editable" rule at the outermost layer, which is
  exactly where it is easiest to lose.

---

## 6. The HTTP surface, small on purpose

Fourteen routes. No cookies, no HTTP/2, no TLS, no keep-alive, no streaming request bodies.

| Route | Returns |
|---|---|
| `GET /` | the embedded HTML. Requires the token. |
| `GET /session` | the session so far, as the session file's own lines (`NDJSON`) |
| `GET /events` | SSE: replays from the cursor (`Last-Event-ID`, or `?last=` with `?session=`), then live events |
| `POST /message` | one message from the browser, into the steering channel |
| `POST /report` | one command *read* from the browser: same channel, and its answer is captured with the terminal quiet (§11) |
| `POST /log` | the page's own account of itself, appended where it can be read afterwards (§25) |
| `GET /sessions` | the conversations `/resume` can reach, numbered the way `/resume` numbers them |
| `GET /file` | a file the transcript named, for the preview drawer (§12). `?path=` takes what the transcript says, including a trailing `:line` or `:line:column`; a relative path is resolved against the run's own working directory. `text/plain` with `X-Flint-Line` when a line was named, and `X-Flint-Cut`/`X-Flint-Size` when the file was longer than the preview cap. A *directory* is refused by this route's own rule, and the refusal carries `X-Flint-Dir: 1` — the sentence is for the person and the header is for the page, which then opens it where it lives (§28) |
| `GET /dir` | what is in a directory: `{"path", "parent", "entries":[{"name","line","path","dir","size"}], "total", "shown"}`, the entries in the order the `list` tool prints them and each `path` joined by this process (§27). A file is refused — "is a file, not a directory" — because drawing one as an empty listing would be the panel lying about where the reader is. The route is what the panel draws when a **readonly** run may not open a directory, and the two readings are one body (§27, §28) |
| `GET /resolve` | where, in a line of text, a path ends: `?text=` is the line *from the first character of the path*, and the answer is `{"path": "…", "used": N}` — the longest prefix of that text which exists, and how many UTF-16 code units of it that was. A text with no path in it is 404, in a sentence. This is how a bare path with a space in it becomes one button: no rule about the text can tell `C:\work\My Notes` from a path followed by a word, and the run is the only reader here with a filesystem (§28) |
| `GET /image` | the other half of the same press: a picture's own bytes, with the type they really are rather than the one its name suggests (§21) |
| `POST /open` | the *other* half of that answer: `{"path": "…"}` handed to the program this machine uses for it, for what the panel cannot show — a file past the cap, anything that is not text — **and for a directory, which is what a press on one now does** (§28): `open` is how a directory is opened where it lives, from the transcript or from a listing. `{"opened": …, "with": …}`, or the refusal that says why not. The second route here that starts something, and the only one that starts a **process**: refused outright when the run is `readonly` (409), which is the one refusal the panel answers with a listing |
| `GET /jobs` | the `task` children and background commands this run started, as `{"jobs":[…]}` in the run's own order (running first, newest first) — for the header's jobs panel (§13). Each row carries `pid`, `kind`, what was asked, a status word, the exit code in words beside it, absolute epoch seconds for its start and end, and `path`: a child's conversation or a command's log |
| `GET /peers` | who else is working in this directory, read off the presence records — the same list `/say --to` addresses (§8) |
| *`event: sessions`* | not a route but its counterpart: the list has changed, re-read it |
| *`event: jobs`* | the same for `GET /jobs`: a job started or ended, re-read it |
| *`event: state`* | the run's own configuration: provider, model, what each provider offers, every setting with the words that change it and the screen it belongs on (`settings`, one list for the switches and the values alike), and the command rows with §8's class and the screen each is filed on (`group`) — a row also carries `values` when the page may send that command with an argument the frame names (a *read* on a `panel` row, a line the page types for you on a `selector`), `fields` when the page may collect its answers (one entry per word the line wants, each with the input's kind, the argument's name, and whether the command works without it), and `from` when it destroys something and the argument is one of a list (§11). There is no `toggles` field: `settings` says what the switches are, what they take and which one is on, and a second copy of that is one fact in two places (§22) |
| `type: command` | what a command answered, on the same stream as the turn's events: `input` and `text` — which is also what a header button's answer arrives on. Carries `panel: true` when the page asked to read it rather than typing it, and `input` is the command's own `send` rather than the line in the one case the line carries a credential (§11) |

**All thirteen are implemented.** `/session` and `/events` read the session path and the event feed
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

`POST /message` is also the only route that can make the *agent* do something, and that is worth stating
plainly:
reachable at that port with that token, a caller can run the agent. What keeps it acceptable is
unchanged from §4 — loopback only, the `Host` and `Origin` checks, and a token another origin's
page cannot set, since a cross-origin form post cannot add a header and a `fetch` that could is
refused by `Origin` before it arrives. The body is length-delimited, capped at a megabyte, and
refused rather than half-read when it stops early.

**`POST /open` is the one that starts a process**, which is a different kind of thing from running the
agent — the agent runs *commands*, and this makes the operating system launch *a program* — and it is
worth its own paragraph for the same reason. It asks the machine, through that machine's own launcher,
to open one path; it refuses when the run is `readonly`, holding the same all-or-nothing switch the
tools are held to; and the one thing that decides it is a pure function whose output is the command
line, so a test can assert what would be run without running it. §16 is the record.

- **SSE framing is the existing format.** Each event goes out as `data: <one ndjson line>`
  followed by a blank line, and each carries `id: <cursor>` — **a position in the session file**,
  the file's length when the frame was pushed, so that the id means the same thing to any process
  serving that file (§15). A reconnect sends `Last-Event-ID`, or `?last=` with `?session=<id>`
  beside it, and the server replays from there, so reconnection needs no invented
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
   knowing: `/session` had to report a **cursor** because the file and the stream would otherwise
   double-count or lose a frame depending on timing; the stream is opened *before* the file is read
   and the server subscribes before it sends headers, so nothing slips between them; a reconnect is
   served from the last 512 frames and gets `event: reset` when that is not enough; and the current
   status is sent once on connect as a named event, because a page opened in the middle of a turn has
   missed every status frame and there is no cursor for "now". **The cursor changed shape later, and
   §15 is the record**: it is a position in the session file (`X-Flint-At`) rather than a count of
   frames, which is what lets a process answer a cursor it did not mint — and a page that reconnects
   is caught up instead of rebuilt.

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
| The settings are drawn right | the page's real `applyState` under Node, over the stub DOM | one row per setting the frame sent, on the screen it named, each row a name, what changing it means and a control: a choice is one button per word the frame listed with the one in force pressed (and a single word means a disabled button), a typed value is a form with the frame's own `kind` as the input's type. The case that needed care: a current value the frame does not list — a provider's own `model` is offered by `/model` whether or not it is repeated in `models` — which is drawn as one more word and *pressed*, rather than quietly becoming the first of the others and then being sent |
| A setting can be switched from the page | `POST /message` with `/verbose full`, the line the control composes | the feed carried `"name":"verbose"…"value":"full"` a moment later. The frame carries each setting as a key, the values that key takes and the value in force, so the page holds no list of its own — not the settings, not the words, and not which one is on |
| A frame with no settings in it takes them away | the same function, over the stub DOM | the rows are removed and the block is `hidden`, rather than left holding values nothing is reporting any more |

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
raced the turn would be a second writer in the transcript. **What that measurement is, and is not**: it is
a request to the route through the page's own door, not a *press*. Pressing a panel row while the composer
is busy is not driven by `scripts/browser-controls-test.js`, and that harness prints what it does drive
before it presses anything — this case is on its "not driven here" list, with this test named as where the
behaviour is actually measured. See the end of §11.

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

**Answered, 2026-09-22, and the answer was the second half of the question.** Grouping around §8's
classes was right for a *list* and wrong for a *place to work*: the classes say what a row does (read,
send, ask, destroy) and a person arriving at the page knows what they came to change (the model, this
run, this conversation). §22 keeps the classes — they are what the page draws a control *from* — and
adds a screen per place, with `MENU_GROUPS` left holding the classes for the `/` menu, where "what does
this row do?" is exactly the question a launcher has to answer.

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

**Partly measured since, 2026-09-18.** The menu is opened with a real press on both kinds of row now: the
run's own, whose rename field is typed into and whose send button carries the name to the run, and a
fixture row's, whose *second* press carries that row's number and removes that file — which the directory
listing witnesses, and which the terminal would have agreed with even if the wrong number had been sent.
What is still reasoned rather than seen is the three things this paragraph named: the menu's position
(`absolute`, against a `relative` row), its dismissal (the button toggles it, a `reset` clears it, and
a click elsewhere does **not** close it) and its behaviour while the sidebar scrolls. The dismissal is the
one worth watching in a real browser: the panel's choice list
has an explicit `‹ commands` row to close it, and this menu has only its own button. The rename field
adds two more halves that are still unmeasured, and for the same reason the harness presses the field's own
send button rather than `Enter`: whether the field keeps the focus it was given while the sidebar
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
page's is left asserted-but-undriven. Those are the last six rows of the table below. §22 then made the
switches and the pickers *settings* rather than header controls and §25 made them words rather than
`<select>`s, so those rows have been retargeted twice; the harness is **109 claims** as §25 leaves it,
and the number is a date rather than a promise.

**Every claim is checked against the run's stdout, not against the page.** That is the whole method: a
click that sent nothing leaves the page looking exactly like a click that worked, so the witness has to
be the process on the other end of the socket. The harness also keeps the browser's console and network
traffic, which is what turned the first failure from "the controls never appeared" into a 500 on
`/session`.

| Claim | How | Result |
|---|---|---|
| The switches are drawn from the run's own state | the header, read from the page | `verbose`, `detail`, `readonly`, `hear-peers` — the frame's four, in its order |
| A switch moves the control and the run together | a real click on the word beside the one in force, on the `this run` screen | the page's pressed word changed *and* the run printed the line that word composes. The control is a row of the frame's own words since §25, so the press is the pointer's — a `<button>`'s activation on Enter is the browser's own default action and a raw protocol key event does not carry it, so a claim about that would be a claim about Chrome; what *is* asserted is that the word is a real, focusable, uncovered button and that the run was told |
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
| The panel's own hand is the third boundary, and §25 added it | the same gestures on `#preview-grip`, dragging left, then the arrow keys, then two clicks | `--preview` grew by the drag *and the panel with it*, grew by 16 with the arrow, and the double-click removed the property; `aria-valuenow` is asserted to agree with the panel's measured width, the hand is asserted to be **in the layout** (width 5px, 0–12px from the panel's left edge), and it is asserted **hidden again** once `Escape` closes the panel. Measured the hard way first: an `auto` grid track with an empty div in it is a hand of width **0**, which no pointer can land on — both the drag and the double-click claims failed against it while the fault was read as "the hand does not work" |
| A picker is pressed, and the run is told which model | a real click on the word beside the one in force, on the `model` screen | the page's pressed word changed *and* the run printed `ok model …` for that value. The open *popup* is no longer a residue at all: the words are buttons on the page since §25, so there is no OS widget left in this claim |

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

**What this does not cover — and the widget it used to name is gone.** A native `<select>`'s open *popup*
belonged to the operating system, so no protocol could reach into it; §25 removed the last `<select>`
from the settings surface (the words are buttons now), so there is no widget left here that a claim has
to reason about rather than drive. What is left is scope rather than a gap: long
answers and reconnection, which are §11's own performance section and were measured on macOS
over a different harness, and the composer at the *keyboard*, which was measured in the
earlier pass, with the button driven here.

### A saved prompt, readable and sendable — measured, 2026-09-17

A template is the one command a page cannot type: its invocation is `/<name>`, and a row a page can
press sends a fixed `send` plus one value. So a run's saved prompts are offered twice, and the pair is
the whole design rather than a convenience. `/prompts <name>` is a `panel` row whose value is a
**reading** — the body, into the panel, with the terminal quiet, exactly like `/skills <name>`. `/prompt
<name>` is a `selector` whose value is the command itself: the page types the line, the turn runs, and
the answer lands in the transcript where the work is. A page that could read through the second one
would be starting a model turn through the report route.

| Claim | How | Result |
|---|---|---|
| Both rows carry the run's own templates | a real `--web` process with a scratch `FLINT_HOME` holding `prompts/demo.md` | `{"label":"/prompts [name]","send":"/prompts","values":["demo"]}` and `{"label":"/prompt <name> [args]","send":"/prompt","values":["demo"]}` — mutation-checked: with the values emptied the frame is still a valid menu and the test fails on exactly those fragments |
| Reading one is quiet, like any report | the same run; `/prompts demo` to `POST /report` | a `command` frame with `"panel":true` and the body in `text`, and the transcript does not contain the body at all |
| Sending one is not a reading | the same run; `/prompt demo` to `POST /report` | refused, `not a report`. The values are the permission and the class is the route — the same boundary `/provider other` is held to one section above |
| The list is the run's, and it costs a run nothing until it is used | the same frame, and `Agent::prompts` | the names come from the run's own discovery walk, like `Agent::skills` — and unlike it, nothing about a template appears in the system prompt or a tool schema |

**Not measured here**: pressing the button in a real browser. The frame and the route are held at the
bytes. The press belongs to the harness in §11, which drives the page's controls — and that harness now
*prints what it drives* before it presses anything, with these two rows on its "not driven here" list
rather than in a promise that the list could grow to include them. So this is a bounded claim with a
stated boundary, which is what it was always meant to be.

### An address in a turn's own words — measured, 2026-09-18

Asked for directly, one day after §12's own complaint was settled: *"addresses on the page should be
hyperlinks to the real thing — a web page, or an address on this computer."* §12 had made a **path**
pressable and only inside a tool block, because the splitter was written for tool output; prose was left
alone on purpose, since a sentence is where `and/or` and `e.g.` live. What was missing was the other kind
of address, and prose.

Two kinds, two doors, and the difference is which one has somewhere real to go. A web address is a link in
a new tab with the opener severed; a path stays a button into the preview panel, because a file has no
address a browser may open from a page served over http. The scheme test is an **allowlist** in one
function, and that is the security boundary: the `href` is a property set from a model's words in a
document that holds the run's token, so `javascript:` would be script here needing no bug — only the click
the reader was about to make. Prose is read with the narrower path rule (absolute only), tool output with
the wide one.

| Claim | How | Result |
|---|---|---|
| The address in the run's own words is a link to the real page | the harness's scripted turn, read out of the DOM | one `a.link`, `href="https://example.com/flint"`, `target="_blank"`, `rel` carrying `noopener` |
| An address that is not the web stays the words it is | the same sentence, carrying `javascript:alert(1)` | still exactly **one** link in that row, and the text is intact — the count is the assertion that catches a permissive allowlist |
| A path in the run's own words opens that file | the same sentence, naming the file the turn wrote by absolute path | a `button.path`, and pressing it puts `four\nfive\n` in the panel with that path in its header |
| The splitter's rules | `scripts/web-view-test.js`, no browser | the four schemes in one line (one link), prose's absolute-only rule (`src/main.rs` stays a word), and the view *calling* it — a splitter nobody calls would pass the first three |
| The allowlist is the only place a scheme is accepted | the page's own bytes | `asUrl` is a function, and the mutation that widens it to "any scheme" makes the Node check fail |

Two page-policy tests changed, and that is the part worth reading rather than the code:
`the_view_requests_nothing_external` used to mean "no absolute URL appears in the page at all", which the
new link could only satisfy by hiding the string from the scanner; it now means "the page itself requests
nothing off this machine", keeps every forbid it had, and gained sharper ones (`fetch("http`, `src="http`).
`a_path_opens_in_this_page_or_not_at_all` no longer forbids `target="_blank"` in general — that rule was
about paths, and the address half is asserted in its own test, with `window.open(` still forbidden because
a link is the safer form.

**Not done then, and built one session later**: the OS-level open this section named rather than
assumed. It got the decision of its own that this paragraph asked for — asked for directly, in those
words — and §16 is the record: a `POST /open` route, the panel's fourth control, and the `readonly`
guard over both. What is still true is the second half: the **preview panel's own contents are plain
text**, so a URL inside a file a person is reading is not pressable yet.

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
| Pressing a path shows the file the run just wrote | a real click, then the panel | `one\ntwo\nthree` — the *file's* bytes, read over `GET /file` from the run's cwd, not something the page worked out (read as the code cells, the numbers being furniture) |
| The page stays where it was | `location` after the press | `/?token=…`, unchanged: the file did not navigate the page |
| The note says the size | the panel's header | `14 bytes` |
| Pressing a path inside a block does not fold the block | the `details`' `open`, before and after | `false` both times: the press is `preventDefault`ed, so the block does not slide away under the file it just opened |
| Reload reads the file again | the file rewritten on disk, then the button | `four\nfive` and `10 bytes` — the new bytes, not the ones the page already had, and the numbers follow them |
| A hit inside a block's output is there to press once the block is open | the grep block's `summary`, then the hit | closed → open, and only then was the hit's centre actually on screen — the check that found the fixed-panel defect |
| A hit's line travels with the path | pressing `notes.txt:2` | the note reads `line 2`, and the file's bytes are beside it |
| A path that is not there is refused in the route's own words | pressing `gone.txt` | the panel holds `nothing at gone.txt: …` with `HTTP 404` beside it, rather than an empty panel |
| Escape closes the panel and leaves the reading alone | a real `Escape` | hidden, with the transcript still there |

The panel's head has a fourth control since §16 — `open` — and the two claims about it are made in the same
run: the control is enabled and titled "open it where it lives" in a run that may, and pressing it against a
path that is not there puts the *route's* refusal in the hint. The successful press is deliberately not
driven: it would open a viewer on the machine running the harness.

**The lines are numbered, and a number is a gutter rather than text.** Asked for directly after the first
day of using the panel: *"代码文件在右侧的监视，现在没有在左边标注行号"* — a code file opened beside the
conversation had no line numbers, so the one thing a `grep` hit's line number is *for* (finding that line by
eye) was the one thing the panel did not offer. Four decisions carried it:

- **Every text file, not only code.** The page does not ask whether a file is "code": it already refuses
  that kind of guess for pictures (§21) and for Markdown (§24), and a third guess by extension would be a
  third rule to be wrong about. A `.md` opened *rendered* has no numbers — a reading is not a listing — and
  the raw view one press away has them, which is the split §24 already draws.
- **A number is furniture, not text.** It is `aria-hidden` (a screen reader should read the code) and
  `user-select: none` (a block copied out of the panel does not come out with `12\t` in front of it). The
  bytes are in a `.code` node per line, so the container's own `textContent` is the numbers run together
  with the file — which is why both harnesses read the code nodes. That is the claim, not an
  inconvenience: what a person copies is the code.
- **A line is a row, and the gutter is a `width`.** Each line is a flex row of `.ln` and `.code`, and the
  gutter is `4ch` (the page is `border-box`, so that is three digits and a space) with the container
  carrying `lines-4`/`lines-5`/`lines-6` when the file's last number needs the room. A `min-width` gutter
  looks equivalent and is wrong in a way only a long file shows: the box grows with its own digits, so line
  300 of a three-hundred-line file pushes its own code right while every other line stays put. And the row
  is what puts a *wrapped* line's continuation in the code column: one `<pre>` with a column of numbers
  beside it cannot do that at all.
- **The scroll is the row's own position.** `scrollToLine` scrolled by `(line - 1) * line-height`, which is
  right only while every line above the target is one visual line tall. With the numbers drawn, the reader
  compares the number at the top of the panel against the one in the transcript, so a wrapped line above
  the target made that comparison fail — and it now reads the row and subtracts the container's own top,
  keeping the arithmetic as the fallback for a panel with no layout to measure (the Node harness, and a
  browser before the first paint).

Measured 2026-09-22, and the fixture is the claim: a file of three hundred lines, one of them six hundred
characters wide, written by the scripted turn so that it arrives as a path in a tool block.

| Claim | How | Result |
|---|---|---|
| A long file is numbered to its last line | the `.ln` nodes after pressing `long.txt` | 300 rows, `1` … `300`, and the 600-character line intact in its `.code` node |
| A number is not part of the text it numbers | `getComputedStyle` on a gutter cell | `user-select: none` |
| The code column is one column | the left edge of three code cells, and the two heights | within one pixel of each other; the wide line's box more than twice a single line's height; the number's right edge left of the code's left edge |
| Opening at a line scrolls to that line's own row | injected geometry in `scripts/web-view-test.js`, where a wrapped row makes the two formulas disagree | the row's own position (`200`) rather than three line-heights (`60`) |

The `min-width` mistake above was found by that measurement rather than by reading the CSS, which is the
argument for measuring geometry at all: `spread(codeX) < 1` is a claim no byte-scan of the page can make.

**Two residues, both deliberate.** A file that is not valid UTF-8 is refused rather than shown as
replacement characters: a lossy conversion would print something no editor would show, and "this is not
text this page can show" is a better answer than a screen of U+FFFD. And there is no highlight on the line a
hit came from — the panel scrolls to it and says which line it is, and a highlight would be a second
render path over the file's own text. A third residue was named here for one session and is now built:
the *directory* and the too-large file were refusals with nowhere to go, which is what §16 answers.

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

- **`started_secs`/`ended_secs` are absolute epoch seconds**, recorded once in the job's own moment
  (`JobMoment`: an `Instant` for "how long ago", a `SystemTime` for the time a person reads) rather than
  computed at read time. They *were* computed — `SystemTime::now() - job.started.elapsed()` — on the
  argument that a stored second field would be the same fact recorded twice, and the argument was wrong
  in a way Windows CI caught: both ends of that subtraction are truncated to whole seconds, so where the
  fractional part of the clock falls decides which second comes back, and two looks at one running job
  could report it starting a second apart. A start that moves is not a start. The point of the absolute
  clock is unchanged — a page open for an hour still shows the true age of a job it heard about when it
  opened — and ticking is still the page's own arithmetic: one `setInterval` that rewrites the `.when`
  text of the *running* rows, no request.
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

- `stopping` was not a status when this was measured, and it is one now. A stop is a write to a child's
  stdin or a kill, and the `Job` recorded nothing between the ask and the exit, so a status word invented
  to fill that gap would have been a claim the run could not back: the row said `running` until it was
  not. **Built 2026-09-18** (`ROADMAP.md` §11 item 7): the run sets a flag where the stop is asked for
  (`Job::ask_to_stop`, `Job::kill`) and reads it against `finished` (`Job::is_stopping`), so a row can
  say `stopping` while that is true and is described as ended the moment it is not.
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
- `stopping` was not a status for §13's original reason, and **it is one since 2026-09-18** -- see that
  bullet. The *candidate* list is still filtered to live jobs rather than the row being rewritten, which
  was never the same decision: that filter is about which job a second press may name, not about what a
  row may say.
- **The 20-second window is flint's, and a job that outlasts it says so.** A child asked to stop may
  finish the thought it was on; a command whose kill has not landed yet is reported as exactly that,
  with the job's own budget named as what ends it either way. Nothing waits forever, and nothing claims
  a job is gone because a signal was sent.

## 14. The same page with nothing behind it: `flint export`

Every level in §3 needs something: a run to serve the page, a file to drop on it, a terminal. The
export is the page as an **artifact** — `flint export <n|id|path> [--out <file>]` writes one HTML file
holding a finished conversation, and that file is opened from `file://` with no flint, no listener and
no network. It is the shape Pi's own export has (§3.5 of `docs/pi-agent-harness.md`), and the interesting
part is that flint already had both halves of it and had never put them together: the page draws a
session file dropped on it, and the session file is the record.

**One renderer, driven from a static frame.** The export is `web/view.html` with the conversation welded
into it as a JSON island — `<script id="session" type="application/json">{"lines":[…]}</script>` —
placed immediately before the page's own script. The lines are the session file's *own* lines, because
`applyText` already parses them (`meta`, `chat`, `usage`, `title`, the tool-call shapes); the alternative,
rendering the conversation to HTML in Rust, was refused for the reason this repository gives everywhere:
it would be a second place where "what a tool call looks like" is decided, and the page is the one
somebody looks at. The page's boot reads the island in the branch it already had for `file:`
(`servedByFlint()` is false there, so not one live route is touched), and an island that cannot be read
is drawn as damage with a hint saying so — a broken export and an empty conversation must not look alike
when one of them is a file to send back.

**What an export may not carry is the whole design**, and each of the three is a decision rather than a
detail:

- **The directory the conversation was held in.** `cwd` is removed from the `meta` line, and it is the
  only field in a session file that names the machine rather than the conversation. An export exists to
  leave that machine, which no other file in this project does. Everything else is kept, including the
  provider and the model, which are facts about the conversation.
- **A byte-order mark.** `notepad` writes one, and so does PowerShell's `Set-Content -Encoding utf8`; a
  JSON parser stops at it, so the `meta` line would be carried as damage — *with its `cwd` still in it*,
  which is the one thing this function exists to take out. The mark is the encoding's business rather
  than the content's, which is the same sentence `attach.rs` already has for `@file`, and the export
  writes a fresh utf-8 document with its own `<meta charset>`.
- **The ability to become script.** The island sits inside a `<script>` element, and such an element ends
  at the first `</script` in its text — and that text is a conversation: a tool result quoting a file, or
  a model asked about this very page. So `<`, `>` and `&` are escaped (`\u003c` and friends), which
  `JSON.parse` reads back as the same characters and the HTML parser does not see at all. Without it, one
  conversation line closes the island early and the rest of the conversation is parsed as HTML, where
  `<script>` is a script — in a file whose whole purpose is to be opened by somebody else.

**Where it goes, and what it refuses.** With `--out` the page goes to that path and stdout keeps one line
naming it; without one, stdout *is* the page and nothing else, so `flint export 3 > page.html` is the
whole invocation. That is `--json`'s rule — a mode either owns stdout or owns none of it — because a
caller who has to strip a banner out of an artifact is a caller who will strip the wrong line one day. It
needs no key and no reachable endpoint, like `/import` and `--archive`: it reads one file and writes
another, and the machine where the provider is in doubt is exactly where somebody wants to hand a
conversation to a colleague. A conversation with no messages is refused, for `/import`'s reason: a page
that looks like a conversation and holds nothing cannot be told from a page that failed to load.

**Measured, in a real browser rather than in bytes.** Chrome, `--headless=new --dump-dom`, over
`file://` on an exported page: the question, the answer, a tool call and its result, and the model's
reasoning are all in the dumped DOM; the directory the session was held in is nowhere in it; the browser
tab carries the conversation's name; and a session whose message is
`</script><script>document.body.setAttribute("data-pwned","1")</script>` produces exactly two
`</script>` closers (the page's and the island's), keeps the text as text, and sets no attribute. The
bytes are held by six tests in `tests/cli_output.rs` and the island's reader by
`scripts/web-view-test.js`; the drawing needs no new test, because it is `applyText`, which the page's own
suite already covers line by line.

**Built since, 2026-09-18: the same door from inside a running conversation.** `/export <file>` writes this
conversation out through the same `page_for_session` the CLI uses, so a page exported mid-conversation and
one exported afterwards cannot drift — a test drives both for one conversation and asserts the bytes are
*equal*. Where the page goes is the person's word and never a guess: a bare `/export` says what it needs
rather than inventing a name in whatever directory the run happens to be in, because the file it landed on
might be one somebody already had. This paragraph used to say the door was not built, and named the run's
file as the wrong thing to guess at half way through a turn — which is exactly why the path is an argument
here while the CLI's default is stdout. Three refusals are held with it: a `--no-session` run, a
conversation nobody has spoken in yet, and a write that fails (reported, and the run carries on).
`ROADMAP.md` §11 item 10 is the change.


## 15. The cursor is a position in the session file

**Built 2026-09-17.** §6 said a frame carries `id: <session line number>`; what it carried was this
process's **frame count**, and the difference turned out to matter.

The count is minted by `Live::next`, starts at 1 in every process, and means nothing in the next one.
So a client whose cursor predated the ring — a gap of more than 512 frames, or a `--web` that had been
restarted — could not be answered at all: `Catch::Reset`, and the page re-read the whole conversation
from `/session` to find out what it had missed. That is what a reconnect was, and it was the *common*
case, because the page called `readSession()` on every successful connection. A two-second network
blip therefore threw away the reader's place in the transcript and any answer still streaming into it.

What a frame carries now is the **length of the session file** at the moment it was pushed — a position
in the one thing here that outlives a process. Three consequences, and each is a decision:

- **The ring is still asked first.** It is the only source that has the *deltas* of an answer that is
  streaming right now, because those are in no file yet. The ring answers whenever it still holds every
  frame written at or after the cursor (its oldest frame is behind the cursor and its newest is ahead of
  it), and it replays those frames. This is the case that matters most often and it is unchanged.
- **The file answers when the ring cannot.** The entries after that position are sent as `event: file`
  frames — the session's own lines, in order, and drawn as the file draws them rather than as the stream
  does. A gap the ring dropped and a process that has only just started are the same request from the
  client's side, and the file answers both.
- **`reset` is left for what neither can place.** A cursor past the end of the file, a file that cannot
  be read, or — the case worth naming — a client showing a different conversation. That last one is why
  the page sends `?session=<id>` beside the cursor: a position in a file and a position in *another*
  file are the same number, and `/resume` moves the run between files while a page may be disconnected
  and therefore never see the `reset` frame that would have told it. Without the id it would be handed
  lines from the middle of a conversation it is not reading.

**Two page-side rules came with it, and both are guarded by tests in `scripts/web-view-test.js`.** The
same moment reaches the page twice — once as the stream's event and once as the file's entry — and the
two are drawn differently:

- a question the stream already drew from `turn.started` is not drawn again from the file's `chat` line
  (only when the line *is* the file's, and only when it is the last block: a person asking the same
  question twice asked twice);
- an answer the page built from deltas is **filled in** by the file's line rather than opened beside it,
  which is the same principle `carryStreaming` already applied in the other direction.

**What was measured.** `cargo test` went from 611 to 615 passing: four new lib tests over a real
scratch session file — the file answering a cursor no ring could place, the ring being preferred when it
can cover one, a cursor naming another conversation being refused, and a cursor past the end of the file
being refused. The first was watched failing with "a restarted process must answer from the file, not
reload" before `entries_after` was written, and the two page rules were watched failing ("a question the
stream already drew is not drawn again from the file") with the `fromFile` distinction removed. The ring
tests in `src/web.rs` still hold, unchanged in substance, because a run with no session file stamps its
frames with their own count — a run that keeps no conversation has no file for a cursor to be durable
*in*, and says so rather than pretending.

**The honest limit, which is the reason this is not the whole of ROADMAP's item.** A page still cannot
get back to a restarted flint: the new process listens on a new port with a new token, so the page's
`EventSource` has nowhere to go, and the page persists no cursor of its own. The cursor is now the kind
of thing that *could* be carried across — a client that remembered `(session id, position)` and came
back to a run willing to accept a cursor it did not mint — and no such client exists. What was built is
the server half of that, plus a page that no longer rebuilds itself for a dropped connection; the client
half is named here as the next step rather than implied by the feature's name.

**The client half, as far as it honestly goes — built 2026-09-18.** Read against the code, "the page
should carry the cursor across a restart" splits in three, and two of the three are not the page's to
do. Following a restart needs both the origin and the token: `--port` can hold the origin, and the token
is minted fresh in every process and required on every route by §4.2, so a page that stored it to follow
a restart would be putting a credential that can drive the composer where any document on that loopback
origin can read it — a worse defect than the one being fixed. A reload must rebuild, because the DOM is
not persisted and the file is the record; only a dropped *connection* keeps the in-memory cursor, and
that path is already a delta. What is left is real and is built: the page writes `(session id, byte
position)` to **`sessionStorage`** — per tab, so no other local document reads it, surviving exactly the
reload in question, and never holding the token — on `pagehide`, where the position is final rather than
as frames arrive (a load-time cursor would report a whole session as "arrived while you were away"). On
load, before the conversation is read, it compares that pair against the file: the same conversation and
a smaller position means the difference **arrived while this page was closed**, and it says so by
count; a different id replaces the pair in silence, because another conversation is not a loss; and a
position **past the end** of the file is a *stale read* — reachable by hand-editing or replacing a
session — reported as a fact with the page carrying on and drawing what it read. That last case is the
one the item said needed deciding before any code existed. Held by `tests/web_view.rs` (the wiring, the
three wordings, and the refusal of the origin-wide store) and by three checks in
`scripts/web-view-test.js` that call `notePosition` in the sandbox — the behavioural half, because a
text assertion passes over an unreachable branch, which was measured: disabling the stale branch left
the Rust test green and the harness check red.

---

## 16. A path opened where it lives, in the program this machine uses for it

Asked for directly, and in the same breath as §11's addresses: *"addresses on the page should be
hyperlinks to the real thing — a web page, or an address on this computer."* The web half was built; the
address **on this computer** was left as a residue, with a sentence saying why rather than a promise:
a route that launches a program on the strength of text a model wrote is a door worth a decision of its
own. The decision came one session later, in four words — *"要做 os 级打开"* — and this is what it built.

**What the panel could not do, and what the refusals already said.** `GET /file` (§12) serves *text*, and
its own refusals name the three cases it cannot: a directory, a file past the 64 MB ceiling, a file that
is not valid UTF-8. The second one already ended with the words *"open it where it lives"* — an
instruction with no door behind it. So the shape was not invented here; it was the missing half of a
sentence the page had been printing for a session.

**A path is still a button, and this is a second, named press.** The control is in the preview panel's
head, beside `reload` and `close`, and it is deliberately *not* the path itself. The text a transcript
carries is model-written; a plain click on it must never be the thing that starts a process, or reading
an answer becomes a way to run what the answer names. Two presses, two meanings: the path reads into this
page's own panel, and `open` hands the path to the machine.

**The platform answers, and they are three different programs on purpose.** `explorer`'s own launcher on
Windows — `cmd /C start "" <path>`, where the empty title is what `start` needs so it does not take the
quoted path for a window title — `open` on macOS, `xdg-open` elsewhere. A directory and a file take the
**same** command on all three, because the launcher asks the operating system, which is the only thing
that knows what a `.pdf` is registered to; a table of file types in flint would be a copy of the registry
that goes wrong quietly. The decision is a **pure function** (`open_plan`) returning the program and its
arguments, and the effect is one `Command::spawn` — so all three command lines are asserted on whichever
machine runs the suite, and no test opens a window.

**The guard, and it is the run's own.** A `readonly` run refuses `POST /open` with `409`, before the path
is even looked at: in such a run the model may not start a program, and a button that started one because
a *person* clicked text the model wrote would be that program running anyway, one click removed. The page
does not offer the control there either, and it decides that from the state frame's own `readonly` toggle
rather than from an opinion — the route refuses regardless, because the page is not the authority.

The plumbing that keeps the two from drifting is one line, and its *place* is the point: the agent is
rebuilt in exactly one arm of the REPL (`Flow::NewAgent`, which `/readonly`, `/model`, `/provider`,
`/reload` and `/new` all return through), so the page's copy of the guard is set there from the new agent
rather than in each command that might change it. A command cannot forget because it never has to
remember.

**What is honest about the capability.** In a run that is not `readonly`, the model can already run that
same program itself through `bash`; what this adds is a *person's click*, not a reach. The residue is the
one a click can never be rid of: a person who clicks a path whose name ends in something executable is
the one deciding to run it. That is why the control is not the path, why its tooltip says what it will
do, and why the guard is checked at the route.

| Claim | How | Result |
|---|---|---|
| Each platform is opened by the program it has | `src/web.rs`, the pure `open_plan` over all three `Platform` values | `cmd /C start "" <path>` / `open <path>` / `xdg-open <path>`, and a directory takes the same one. Mutation-checked: changing the Windows `with` label fails it |
| A readonly run refuses, whatever the path | the same function, twice: a file that exists and one that does not | the *same* `409` sentence both times — the answer is about the run, not about the file |
| The route reads the guard the run is holding **now** | `respond` against one `State`, with the shared flag flipped on and back off | `404` (nothing there) → `409` (readonly) → `404`: a mirror that latches would fail the last step |
| A path that is not there is refused before anything is spawned | `respond` with a missing path | `nothing at …` with the operating system's own words, and the launch is the only line after that check |
| The body is asked for in its own words | four bodies: not JSON, no `path`, an empty one, spaces | the four sentences, each with its status |
| The page posts the path as JSON | `scripts/web-view-test.js`, calling `openBody` | a Windows path keeps its backslashes, and a quote in a name arrives as itself |
| The frame decides whether the button is offered | the same harness, calling `readonlyOn` with four frames | `on` → true, `off`/absent/`null` → false |
| The panel offers it, and the route's refusal is what comes back | the browser harness, driving the control for real | the button is enabled and titled "open it where it lives"; the press puts `not opened: nothing at gone.txt: …` in the hint |

**Not driven here, and where each is answered instead.** A press that *succeeds*: it would start a
viewer or a file manager on the machine running the harness, so the harness presses the control against a
path that is not there and the command lines are held by the Rust test instead. The harness prints that on
its "not driven here" list, beside `src/web.rs::tests::each_platform_is_opened_by_the_program_it_has`.
And a `readonly` run's page is not driven either: turning the guard on mid-turn would change the state
every later claim in that run is made against, so the page's half is the Node check above and the route's
half is the Rust one.

**Where this leaves the boundary.** Not moved. §4's argument is that the token holder can already drive
the agent; `POST /open` is a *narrower* door than `POST /message` is, and the one thing it could have
added — a way for a `readonly` run to launch something — is the thing it refuses. `docs/features.md` §12
carries the door-by-door version, and §15 of that file carries the "deliberately not built" line it
replaces: the preview panel's own contents are still plain text, which remains the honest answer to a URL
inside a file.

## 17. The header says whose conversation this is, and what the run is doing

Asked for directly, 2026-09-18: *the header is weak — the conversation's name should be up there, with
the background jobs and the subagents*. Two complaints in one sentence, and both were true. The line
above the conversation said `flint` — the product's name — for every conversation, named or not; and a
run with three subagents and two background commands working showed a single collapsed panel reading
`jobs (5) · 3 running`, which is a total that cannot answer any of the three questions a person watching
work actually has: is it still moving, is any of it a *subagent* rather than a command, has anything
already ended badly.

The line is now the conversation's name with the counts beside it:

    why does dsh fail to start   ● 2 running  ● 1 subagent  ● 1 failed  ● 3 done

**The name is read, not invented, and the one decision in it is the order of two real sources.** The
session file's own newest `title` event arrives on the stream and moves the moment a rename happens; the
label comes with the conversation list, where `session::list` chose it: the newest name, else the first
thing that was said, else `(empty)`. The title wins, and the label is the fallback rather than the first
choice — even though the label is the *fuller* answer, because it is what makes an unnamed conversation
show its own opening words instead of `flint`. The reason for that order is the reverse direction: a page
that preferred a list it read a moment ago would go backwards after a rename, and `doc.title` is the fact
this run is holding. `titleWords(doc, label)` is the whole rule as a pure function, and the tooltip
carries the label unclipped because the line clips with an ellipsis at 64 characters.

**The counts come from `GET /jobs`, counted by kind rather than summed**, and the wording is the
vocabulary §13 already established (`running`/`stopping`/`completed`/`killed`/`failed`). Four rules are
the design:

- **A failure is never folded into `done`.** A job that failed and one that finished are the same number
  in a total, and the row behind the chip is the only place an exit code is legible. `killed` counts as a
  failure there, which is what the row's own dot already said.
- **A subagent is its own chip**, because "2 running" and "2 subagents reading your code" are different
  facts about where the money is going.
- **The whole control is hidden when there is no work.** A chip that says `0` is a control nobody presses,
  and it was DSH's own header that made the case: its job badge renders nothing at all rather than `0`.
- **Settled work stays visible** (`3 done`), de-emphasised rather than tidied away, so a run that has just
  finished five things does not look like a run that never started any.

**The work found a bug in what "live" meant.** `jobIsLive` was `status === "running"`, so a job the run
had asked to stop — `stopping`, the word §11.7 of `HANDOFF.md` added — fell on the settled side and was
counted as `done`: a run would show itself finished while the editor it started was still holding a file.
A stopping job has been *asked* to stop and has not stopped, so it is still spending time, and it now
counts as live for the chips and for the clock that keeps a duration honest. It is *not* live for the
`/jobs stop` menu, which asks the new `jobIsStoppable` instead: a job already on its way out is not
something a second press can do anything about, and that menu's own rule is that a choice whose only
outcome is the sentence it already has is a press wasted.

**Measured, and how.**

| Claim | Where it was measured | What came back |
|---|---|---|
| The name is the conversation's, in three cases | `scripts/web-view-test.js`, calling `titleWords` | `flint` only for a page with neither a title nor a list |
| The chips are counted by kind | the same harness, calling `paintJobs` and reading the summary's children | `3 running · 2 subagents · 2 failed · 1 done` for six jobs, and `2 stopping` when everything is on its way out |
| All of it goes away with the last job | the same | the control is `hidden` and the summary is empty |
| The failure chip says where the reason is | the same | its tooltip names the exit codes |
| The header's shape is a policy, not a style | `tests/web_view.rs::the_header_names_the_conversation_and_what_the_run_is_doing` | the name and the work are on one line, the name's order is read, and the chips read `jobIsLive`, the child kind, the failures and the kills |
| The chips and the rows are the same list, in a real browser | `scripts/browser-controls-test.js`, against a real run's own stdout | **61/61 claims held**, including `3 running` / `1 subagent` for a turn that started two background commands and one `task` child, with the three partition chips adding up to the rows and the subagent chip a cross-cut of the live ones |
| The name on the header is the name the sidebar shows, in a real browser | the same run, with `/name` sent from the sidebar's own field | the line above the conversation read `named from the sidebar`, which is the row the sidebar had just named — and not `flint` |

**The stub DOM found a real defect while this was being tested**, which is worth recording because it is
the second time that harness has caught something the browser would not have. `summary.textContent = ""`
clears the children in a browser and does *not* in the harness, so the chips of the previous paint were
still there and the counts were wrong — `3 running … 2 done` for a list with one done job in it. The page
now removes children one at a time, which is what `fillSelect` already did and says why.

**What this deliberately does not do.** It does not show the model, the provider or the token — those are
the *run's*, not the conversation's, and the controls that change them belong where the rest of the
configuration is (the settings surface is §19). It does not animate anything while a turn is running: the
header describes *work*, and a spinner for the main turn is the transcript's job, whose last block is
already the answer being written. And it does not rename anything: `/name`, the sidebar's field and the
file's own `title` event are the three doors onto one fact, and the header is a fourth reader of it, not a
fourth writer.

## 18. The paths in a transcript, and where a tilde leads

Four reports from one real transcript, 2026-09-18, and they are one subject: what the page thinks a path
is, what it shows of one, and where the path it shows actually leads.

**1. Flint's own commands were files.** §11 states the rule for prose: a path is a token that is
absolute, or that has an extension, or that has three segments. `Path` is absolute. So `/stop`, `/name`,
`/jobs`, `/web`, `/events` — every one of them one segment with a slash in front — became a button onto a
file that does not exist, and a press filled the preview panel with `nothing at /events: …`. A
conversation *about flint* is full of those, which is how it was noticed.

The rule now asks the shape only of a **slash-rooted** path: two segments deep, or an extension at the
end. `/etc/hosts` passes, `/notes.md` passes on its extension, `/stop` and `/tmp` do not. `~`-rooted and
drive-rooted paths are exempt, and the exemption is the point: the extra evidence exists to tell a
one-segment `/word` apart from a command word, and no command word begins with `~` or a drive letter — so
`~/notes` and `C:\notes` are paths on their own evidence. Dropping `/tmp` costs nothing **today**,
because a directory is the one thing `GET /file` refuses by its own rule — and that reason was only
true while a directory had no reading at all: since §27, a directory *is* readable, and a one-segment
`/word` is still dropped for the reason it always was (this program's own commands are written that
way). What §27 adds is the other direction: a name that *ends* in a separator is evidence of its own,
and the run's own listing rows are read as names rather than as tokens.

**2. A path with a space in it was cut in the middle.** `read C:\Program Files\flint\config.toml` is a
path and a word, and which is which cannot be recovered from the line — so the scanner cut at the space
and drew a link to `C:\Program`, a name that exists nowhere, with the rest of the path as plain text.
Two halves, one rule:

- the scanner reads a **quoted** run (`"…"`, `'…'`, `` `…` ``) as one candidate, and draws the link
  *inside* the quotes rather than eating them;
- flint **quotes a path with a space where it prints one** (`display::quote_if_spaced`), because the line
  it writes is the input to both of this path's readers — a person, and this page. That is the one place
  in this design where the *printer* had to change for the page's sake, and it is a change a person
  wanted anyway: `read C:\Program Files\flint\config.toml` is ambiguous in a terminal too.

The residue is stated rather than hidden: a *bare* path with a space, written by a model that did not
quote it, stays split. The first fragment is no longer a wrong link (the new shape rule drops
`C:\Program`), and nothing else can be done — no reader can tell `C:\Program Files\x` from a path and a
word. **§28 answers this one, a round later, and by asking somebody who can**: the *page* cannot tell,
but the run has a filesystem, so the page asks it where the path ends and draws the button over the
answer. What is left of the residue after that is written down there.

While this was being fixed, one layer down, the shortening step moved in front of the read-range suffix.
It used to find the path again by splitting the printed line at its **first space**, which is the same bug
in the other direction: for the file above it handed the shortener `C:\Program`, and the range suffix
ended up glued to the wrong half.

**3. The line number was not on the button.** `asPath` has always turned `src/web.rs:412` into
`{path, line}` — and the renderer drew `part.path`, so `:412` existed in the tooltip and nowhere else.
The one detail a `grep` hit is worth reading for was invisible. The result is now
`{path, line, written}`: `path` is what `GET /file` is asked for, `line` is where the panel scrolls to,
and `written` is the token as the reader met it, which is what the button says.

Two spellings came with it, both from the same report:

| Written | Opens | Prints |
|---|---|---|
| `src/web.rs:412` | that file at line 412 | `src/web.rs:412` |
| `src/web.rs:412:7` | line 412, column dropped | `src/web.rs:412:7` |
| `src/web.rs#L412`, `#L412-L420` | line 412 (a range keeps its first) | as written |
| `file:///C:/work/x.js:42` | `C:/work/x.js` at line 42 | `C:/work/x.js:42` |

`file://` is deliberately a **path button and not a link**: no anchor in this page goes to a local file
(§4's allowlist refuses the scheme, and a browser served over http would not follow one), while the file
*is* the thing the reader pointed at and `GET /file` is the door that reads it. The scheme is dropped on
the way to the button because it is the one part of the token the panel does not need to repeat; a `/`
in front of a drive letter goes with it, since `file:///C:/x.js` is what a browser's own copy-link
produces. `file://host/share/x` is another machine's path: it stays text.

**4. A tilde means the home directory, in one place.** The page has accepted `~/…` since §11, and the
route read the tilde literally — so the button opened nothing, because the path resolved against the
working directory as `…/~/.flint/skills`. The rule is `config::expand_home`: `~` followed by a separator,
and nothing else. `~user/x` is another user's home and `~notes.txt` is a file *named* that, and both stay
as written so a reader reports what it looked for instead of reading a file that was not the one named; a
machine that will not say where home is leaves the path alone rather than resolving it against whatever
directory the process happens to be sitting in.

`config::resolve_path` is that resolution applied at all four doors a path comes in through — a model's
tool argument, the page's two routes, a person's `@name`, and a directory named in `config.toml` — because
four copies of "absolute, else against the working directory" is how the tilde came to work in *none* of
them and how they could have come to disagree about the rest. The page's own scanner states the same rule
(`~` plus a separator, so `~notes.txt` is a word in prose and a relative name in a tool result), which is
two readers and one definition.

**Measured.**

| Claim | Where it was measured | What came back |
|---|---|---|
| Flint's commands are not files | `scripts/web-view-test.js`, calling `asPath` | `/stop`, `/jobs`, `/events`, `/tmp` → null; `/etc/hosts`, `/notes.md` → paths |
| A quoted path survives its space | the same, through `addressParts` | one path plus text either side, quotes intact |
| The button prints the line | the same, through `linkNodes` | four tokens, four texts, four tooltips naming file and line |
| `file://` is not a link | the same | no anchor whose href begins `file:`, one path button |
| A tool line quotes the ambiguous path | `src/display.rs::a_path_with_a_space_in_it_is_quoted` | `"C:\Program Files\flint\config.toml"`, range outside the quotes |
| The shortener sees the whole path | `src/display.rs::shortening_happens_before_the_range_is_appended` | the shortener's own marker around the whole path |
| The tilde rule, and the two shapes that are not homes | `src/config.rs::a_leading_tilde_is_the_home_directory_and_nothing_else_is` | `~/x` expands, `~user/x`, `~notes.txt`, `a/~/b` and no-home do not |
| A tool argument resolves the same way | `src/tools.rs::resolve_path_joins_relative_and_keeps_absolute` | `~/notes.txt` → the real home; `~notes.txt` → the working directory |

**What this does not do.** It does not open a path that is not on this machine, and it does not expand
anything else a shell would: `$VAR`, a glob, a `~` in the middle of a path and a `~user` all stay as
written, because each would be a guess about what somebody meant and the refusal is a sentence that says
what was looked for. And it does not touch the *tools'* own arguments beyond resolution: a `read` given
`~/notes.txt` now reads the same file the page's button opens, which is the whole point — one name, one
file, whether the model, the page or a person is doing the reading.

## 19. The plan for the command surface: settings, and commands behind a `/` — **not built**

Asked for directly, 2026-09-18, in the same breath as §17: *the page is too raw — put things in settings
rather than spreading them across the page, and it can be redesigned*. DSH (the harness this session runs
in) was named as the model, so its own surfaces were read rather than remembered, and this is the plan of
record for the half of that request that is not built yet. It is written before the code, in the style of
§9, so the decisions are reviewable on their own.

**What is wrong now.** The header is a row of controls: two `<select>`s, five toggle `<select>`s, an action
button each, a `commands` panel listing every command in five groups, and a jobs panel. Every one of them
is a *setting* or a *command* wearing the same clothes, on the page, always. The concrete costs are that
the reading column is pushed around by controls that are not part of the conversation, a settings change
(`readonly`, `verbose`, the model) is drawn exactly like a command (`/new`, `/export`), and the command
list is a wall rather than something you can ask a question of.

**Where each thing goes.**

| What is on the page now | Where it goes | Why |
|---|---|---|
| `#pick-provider`, `#pick-model` | **settings**, as rows that apply immediately | one endpoint per run is state, not an action; changing it is a decision about *this session*, and the answer is visible in the row |
| the five toggle `<select>`s (`verbose`, `detail`, `readonly`, `hear-peers`, `thinking`) | **settings**, same | each is a standing property of the run, drawn from the state frame, sent as `/<toggle> <value>` exactly as now |
| `form` rows (`/provider add`, `/provider key`, `/config set`, `/import`, `/export`, `/name`, `/queue`) | **settings** | they are fields, not sentences; `/provider key` is a *secret* and belongs in a masked field inside a dialog rather than in a row of the transcript |
| `button` rows (`/new`, `/reload`) | **settings**, under a plain heading | one press, no argument, and a lifetime decision about the run |
| the five command groups, `reports`/`actions`/`selectors`/`destructive` | the composer's **`/` menu**, kept in the same five classes | see below: a command is something you *type*, and the frame already says which is which |
| the jobs panel | stays where §17 put it, chips in the header | it is not a command and not a setting: it is the run's work |
| the sidebar, the preview, the composer and the transcript | unchanged | nothing here is a control that was in the way |

**The settings surface.** A centered **overlay**: a fixed panel with a mask over the page, opened by one
`settings` button that sits with the header's identity line, and closed by its own close button, a press on
the mask, or `Escape`. It carries `role="dialog"` and `aria-modal="true"` with an accessible name, moves
focus to the close button when it opens and back to the button that opened it when it closes, and only the
active section is mounted (a nav rail of sections down the left, one detail pane on the right, DSH's own
shape). The stub DOM in `scripts/web-view-test.js` has no `<dialog>` and no `showModal` — and
`document.addEventListener` there is a no-op — so this is a `hidden`-toggled `div` whose close function
the harness calls directly, which is the same constraint §10 hit and the same answer. Rows that are
settings apply **immediately** (there is no Apply in DSH's General section either, and a settings dialog
with a Save button is a second source of truth for state the run already holds). A section with a
destructive row inside it (`/archive`, `/delete` of a conversation) keeps the two-press rule and names the
object it is about to destroy.

**The `/` menu, in the composer — built, and §23 is the record.** DSH has no command palette — this was
looked for and is not there — and what it has instead is a **trigger menu** in the composer: typing `/`
opens a list of the commands, each row an icon, a title, the raw name as an alias and one line of
description, with `Tab` to complete and `Enter` to take. flint's version is drawn from the same `state`
frame the `commands` panel is drawn from today, so the page still contains no command name of its own,
and it keeps the frame's own five classes because those already say what a press would do:

| Class | What `Enter` does | Why |
|---|---|---|
| `reports` | sends nothing: it asks for the **reading** and shows it where the list was | a report is read, not sent — flint's own rule, and the panel already does this |
| `actions` | **completes the line** in the composer rather than sending it | `/new` mid-sentence is a decision, and one keystroke should not make it |
| `selectors` | completes the line with the command, then offers its arguments | the frame knows the list (`providers`, `sessions`, `jobs`); the page does not |
| `forms` | **opens settings at the row**, never types a secret into the transcript | a key typed into a conversation is a key in the session file |
| `destructive` | completes the line and leaves it there | never one press from destroying something |

Matching is subsequence-with-priorities (prefix first), filtered as you type after the `/`, closed by
`Escape`, and the list is drawn from the frame's `label`/`send`/`help` fields, which is what the panel
uses today. What this deletes: `details#commands` and its whole group renderer — which the settings slice
had already done, so in the end this slice deleted nothing: it added a second *reader* of the same
frame, and the panel it once meant to replace is where a report's answer is drawn.

**Tests this plan already knows it needs**, because each is where this kind of change goes wrong: the
settings overlay opens and closes by all three doors and returns focus (§10's harness can call the
page's own functions); a `form` row with a `password` field still never reaches the transcript; the `/`
menu's filtered list is the frame's commands and nothing else; and `readonly` still cannot be turned on
and off by a control that reaches a route rather than the run.

## 20. Markdown in the preview panel: an answer, not a plan — **the scope was a person's choice, and the choice was made — see §24**

Asked 2026-09-18, with the honest worry attached: *the preview should understand Markdown, but is that too
much complexity?* It is a real question, and the numbers are what answer it.

**What is actually being asked for.** The panel already reads a file and draws it as text with line
numbers, a scroll to a line, a byte count and a refusal of its own for each reason it cannot be shown
(§12). A `.md` file drawn as its own source is legible but not *read*: `AGENTS.md`, `HANDOFF.md` and every
`docs/*.md` in this repository are the files a person previews most, and they are the ones whose structure
the raw text hides.

**The three scopes, with what each costs.**

| Scope | What it does | Size | Risk |
|---|---|---|---|
| **A. line-level styling** | headings, fences, list bullets and quotes get *style* — a proportional face, a measure, a bolder `#` line, a monospaced fence — with no parsing of inline syntax | ~50 lines of JS, 2–3 harness checks | low: a wrong style is a style |
| **B. block rendering** | A plus real blocks: headings at their levels, fenced code, lists (nested), blockquotes, thematic breaks, paragraphs, and a **raw/rendered toggle** | ~250 lines + ~10 checks | medium: the failure mode is a wrong render, contained to the panel and one press from the truth |
| **C. B plus inline** | B plus `**bold**`, `*italic*`, `` `code` ``, and `[text](url)` whose URL goes through the page's existing allowlist | +80 lines + 5 checks | medium: the parser is the new surface, and the link half is where a security rule already exists to reuse |

**What makes it cheaper than it looks.** Three of flint's own rules do most of the work:

- **the page never assigns markup** (`the_view_never_assigns_markup`, a policy test): a renderer here
  *builds nodes* and sets `textContent`, so a file containing `<script>` or `<img onerror>` renders as
  those characters. Raw HTML in Markdown is therefore not a decision to make — it is impossible by
  construction, which is the one thing every hand-rolled Markdown implementation normally gets wrong;
- **the URL allowlist already exists** (`asUrl`: `http`/`https` only), so `[x](javascript:…)` becomes
  text without a second rule;
- **there is no dependency to add**: the page is one hand-written file, and the parser would be too.

**What makes it more expensive than it looks.** A Markdown parser is a dialect claim, and a half-parser is
worse than raw text for a document that is a *source of truth*: tables, setext headings, reference links,
task lists, footnotes, nested emphasis and HTML blocks are all things a reader will eventually type and
this would render wrong, silently. Two interactions with what is already built also have to be answered
rather than discovered:

- **line numbers.** The panel scrolls to a line because a `grep` hit asked for one. In a rendered view,
  "line 412" has no obvious meaning. The rule this plan proposes: a preview opened **at a line** opens
  **raw** (the line is why it was opened at all), and rendering is what a file opened without one gets;
- **the toggle is not optional.** Raw must stay one press away, and the panel's note must say which view
  is showing, because the honest answer to "the renderer got it wrong" is the file's own bytes.

**The recommendation.** A, then B only if A still feels thin — and B **behind the toggle**, with the
dialect documented in this file as the subset it is, and C only when a link in a previewed document is
actually missed. That order is not a compromise: A is where most of the readability is, B is where the
parser starts, and the toggle is what keeps the parser's mistakes from being the *only* thing the panel
can show. Doing all three at once would be the largest single piece of page-only logic in the tree
(comparable to the whole jobs panel, §13) for a panel that is a side window — which is a fair thing to
build, but it should be chosen rather than assumed.

**Built, and the two steps became one (§24).** B was built with A's styling folded into it rather than
A first, because A's code would have been deleted by B's: styling a line by the shape of its first
character and then reading that line as a block are the same loop with different bodies, and doing them
in order would have meant writing the loop twice. C was **not** built, which is what this
recommendation asked for — a link in a previewed document has not been missed by anyone, and the honest
answer to "the renderer got it wrong" is still the file's own bytes, one press away. §24 has the
measurements, the four defects the reading had, and what a person gives up by having it.

## 21. A picture in the panel, and the two rules it had to respect

Asked for directly, 2026-09-18, after §17 and §18: *the panel should probably support images too — the
mainstream ones — and how is up to you; clicking it could also go through the system's own open.* It
is the third answer to the same press §12 built, and it cost less than the two before it because the
two rules it had to respect were already written down rather than invented here.

**The route is new and the panel is not.** `GET /image?path=…` serves one file as **bytes** with the
type its own first bytes declare, and the panel draws it. The alternative — teaching `GET /file` to
answer with a picture — was refused on a fact rather than a preference: `Response`'s body is a
`String`, every one of its twenty-four construction sites builds text, and fifty-eight assertions in
`src/web.rs` read that text back. Turning the body into an enum to serve a photograph would have
touched all of them. So `Answer` gained a third shape (`Raw { content_type, body, extra }`), the header
block moved into one `head(…)` both shapes call — and the four security headers, which is where a
second copy has already gone wrong once, are still written in exactly one place.

**The type is the bytes, not the name.** A `Content-Type` is a claim a browser acts on: it hands the
bytes to a decoder, and for `image/svg+xml` into a document context. So `serve_image` sniffs:
PNG, JPEG, GIF (87a and 89a), WebP (`RIFF` **and** `WEBP` at 8), BMP, ICO/CUR, TIFF (both byte
orders), AVIF and HEIC/HEIF (`ftyp` plus the brand), and SVG — which is text, so it is recognised by
what it *says* (`<?xml`/`<svg` in the first kilobyte) rather than by how it starts. A JPEG named
`photo.png` is served as `image/jpeg`; a note named `logo.png` is refused with a sentence that lists
what this route would draw.

**What the page may ask, and in which order.** The page cannot sniff bytes it has not fetched, so it
decides from the *name* — one set of extensions — and asks `/image` first for a name that looks like a
picture. A refusal is **not** shown: the panel falls through to `/file` and shows what that says. That
one rule buys three behaviours a reader will actually meet: a `.png` that is really a note reads as the
note; a directory or a missing file gets the text route's own sentence; and a picture whose route
refused it cannot produce "not a picture this page can draw" as the *only* thing on screen.

**The click goes through the system, which is what the person asked for.** Pressing the picture calls
the same `openOutside()` the header's `open` button calls, so it is the same route (`POST /open`), the
same `readonly` refusal, and the same two-press discipline that §16 built — the preview press opens the
panel, and the second press hands the file to the OS. Text keeps its separate `open` button because
there is nothing in a text panel to press that would mean *this file*.

**Measured, and how.**

| Claim | Where it was measured | What came back |
|---|---|---|
| A picture is served as its own bytes with the type they declare | `src/web.rs::an_image_is_served_as_its_own_bytes_with_the_type_they_declare` | the body equals the file, `Content-Type: image/png`, `Content-Length` right, and all four security headers present on a bytes body |
| The name is not evidence | `src/web.rs::an_images_type_is_its_bytes_and_not_its_name` | a JPEG named `.png` → `image/jpeg`; a note named `.png` → the refusal, and `GET /file` reads it |
| Every refusal has a sentence | `src/web.rs::a_missing_directory_and_oversized_image_each_say_what_they_are` | missing → 404, directory → 400, 24 MB + 1 → "too big to show here; open it where it lives", no path → 400, empty path → 400, **no token → 403** |
| The sniffing table, format by format | `src/web.rs::image_kind_knows_the_formats_a_browser_draws` | fifteen signatures answer with their type; text, an empty file, a RIFF that is sound, an `ftypmp42` video and a short PNG answer with none |
| The panel asks for a picture by name and lets go of the last one | `scripts/web-view-test.js`, calling `imageExt`/`imageRoute`/`showPicture` | the extension set both ways, the route's encoding, the blob URL drawn, the note from the route's headers, and `revokeObjectURL` called exactly once per replaced picture |
| A token never appears in a URL | `tests/web_view.rs::a_picture_is_read_with_the_pages_own_auth_and_never_a_token_in_a_url` | the fetch carries `authHeader()`, the `<img>` gets a blob URL, and `&token=`/`?token=` appear nowhere but `/`'s own address |
| A real picture decodes in a real browser | `scripts/browser-controls-test.js` against a live run | every claim in that block held, including a 1×1 PNG with `naturalWidth === 1`, the note `image/png · 68 bytes`, the misnamed note falling through to text, and Escape releasing the picture (the harness's own total is printed by the run rather than quoted here, because it grows with each slice) |

**What was refused.** Base64 in a JSON body (`data:` URLs are already allowed by the CSP) — refused
because it inflates a photograph by a third and makes the page's own script decode what the browser's
decoder does better. A token in the image URL — refused because that is the rule §4 already states, and
the fact that an `<img>` cannot send a header is exactly why the page fetches the bytes itself. A PDF
renderer, an image *editor*, EXIF, thumbnails and a file tree — none of them were asked for, and a
preview panel is a side window rather than a viewer. And the size cap is **24 MB** rather than `/file`'s
64: a text file can be cut at 512 KB and still be honest, while a picture has to arrive whole to be a
picture, so that number is what this process and the tab hold in memory at once.

**The one CSP widening this needed**, said out loud because a policy is not a detail: `img-src` gained
`blob:`. The page makes that URL itself, from bytes it fetched with a header, so it is not a second way
in — and the alternative that needed no widening (`data:`) was refused above for a reason of its own.

## 22. The settings dialog: six screens — **built; §25 rebuilds its chrome and moves the door to the sidebar**

The second half of the same request §17 and §18 came from, and the half that was a complaint rather than
a request: *the page is weak, the controls are laid out raw on the surface, and some of it should be in
settings* — with DSH named as the model. It is the three-word version of a real fault: a header that
holds a picker per provider, a switch per toggle, a button per action and a reference panel of forty
commands has stopped being a header and become the top of the reading column. The reading is what the
page is for; everything else is furniture.

> **What in this section has moved since.** The screens, the `group` field and the page's ownership of
> the *names* are as written here. The **chrome** — the rail, the rows, and the controls on them — was
> rebuilt on DSH's own shape in §25, and the door is no longer on the header's line: it is the sidebar's
> bottom seat. Read both; where they disagree about furniture, §25 is the later measurement.

**What moved, and what stayed.** The header's line was the conversation's name, the jobs chips, and
one `settings` button (since §25 it is the name and the chips, and the button is the sidebar's bottom
seat). Behind the button: the two pickers, the five switches, the run's own action
buttons, its `cwd`/id/creation line, and the command list. The chips stayed because they are *status* —
"2 running" is what the header is for — and the name stayed because it is the one thing that says whose
conversation this is. The line between the two is not "important versus unimportant": it is **state you
are watching** versus **state you are changing**, and only the second half belongs behind a door.

**Screens, not a pile — and the second half of the same complaint.** The first version of the dialog
had a rail with two sections, `run` and `commands`, which was the *page's* split: everything about the
run on one side, everything the run takes on the other. A person using it said what was wrong with
that in one sentence — the settings should be split the way the page is *used*, like DSH's, rather than
wrapped up in one box. Two sections is not a design; it is the old header with a lid on it. So the
screens are now the places a person actually goes, and they are cut by *what somebody came to do*:

| Screen | What is on it |
|---|---|
| `model` | the endpoint and the model this run asks: the provider and model pickers, and every way to change either (`/provider add`, `/provider key`, `/provider rm`) |
| `this run` | how it behaves while it works: the five switches, and the actions that affect this run (`/reload`, `/say`) |
| `limits` | what it may spend, and how it runs a command: `/config` and `/config edit` |
| `tools` | what it can use, and what it knows: `/tools`, `/skills`, `/prompts`, `/agents` |
| `this conversation` | its name, its size, and the other conversations: `/name`, `/usage`, `/compact`, `/export`, `/import`, `/sessions`, `/resume`, `/fork`, `/new`, `/archive`, `/delete` |
| `background work` | what this run left running: `/jobs`, `/jobs stop <pid>` |

**The process files the rows; the page names its own furniture.** Six screens is not six names the page
invented — that would be §8's rule broken at exactly the place it matters most, because a page that
decided which screen a command belongs on would be a page that has to be edited every time a command is
added. Each row the frame sends carries a `group`, and `page_group` in `src/main.rs` is the one place
that decides it, beside the command table and for the same reason (the third copy of a fact is the copy
that drifts). The page owns the *screen names, their order and their one-line notes* — its own words for
places it put things, like a group heading — and it drops any row whose `group` names a screen it does
not have, rather than inventing a home for it. Three lists are therefore tied together by construction
and by tests: `page_group`'s vocabulary, the page's `SETTINGS_PANES`, and the frame the tests read.

**The rail is a launcher, so it is the page's, and the reading follows the row.** Two things had to
change with the split, and both are about a press landing where the person is looking. A report is read
on the screen the row was asked from — the answer replaces *that* screen's rows, with a `‹ back` above
it — rather than in a single panel that a press from any screen would fill. And the `/` menu hands a row
off to the screen the frame filed it on, so a form row opens the dialog *at that row* rather than at
whichever screen happens to be first. A row the page does not group — `/help`, `/queue`, `/config set`,
and the two picker lines that are already controls — is read on the first screen, which is the honest
fallback rather than a silent nothing.

**`hidden`, not `<dialog>`/`showModal`.** §10 already recorded why the page has no `<dialog>`: the stub
DOM the Node harness runs cannot express `showModal`, and a control whose behaviour is only checkable in
a browser is a control most of whose behaviour goes unchecked. The dialog is a `hidden`-toggled div with
`role="dialog" aria-modal="true"` and a sibling mask, so the *three* doors a modal needs — its own close
button, a press on the mask, `Escape` — are three ordinary handlers that the Node harness can call and
the browser harness can press.

**The screens are built, not written.** The markup holds the rail and one empty container; the six
screens (`pane-<key>`, `fields-<key>`, `row-list-<key>`) are built when the dialog opens, because the
screens are the page's own list and a second copy of it in the markup is the drift this repository
spends its comments preventing. The cost is a real one and worth naming: nothing inside the dialog is in
the document until somebody opens it, which is why the browser harness waits for the *door* rather than
for a control, and why `openSettings` paints as well as builds.

**Focus is the whole difference between a modal and a panel that happens to be on screen.** Opening moves
the keyboard to the close button, and closing puts it back on the door it came from. Neither is visible
in the page's own nodes, which is why the Node stub grew a `focus()` that records where the keyboard
went: an assertion that a dialog "opened" without one would pass for a div that merely became visible.

**One `Escape`, and an order.** Before this, `Escape` was two anonymous listeners on the document, one
for the preview and one for the jobs list, and `docs/features.md` recorded the consequence as a feature:
one press closed both. With a modal in the tree that is no longer an honest answer — a dialog over the
page is the thing being used, so a press must close it and *nothing else* — and the two listeners became
one function, `dismissTopmost`, ordered settings → preview → jobs list. The ordering is the design; the
fact that it is one function is what makes "which one does this close?" a question with a readable
answer.

**The page's controls got a measurement they did not have.** The old claim was that the reading
column's geometry was right with the reference panel open and shut. The new one is stronger and is the
reason the dialog is an overlay: with the dialog open, the transcript, the pane and the composer have
**identical** rectangles to the pixel, and the point at the centre of the send button belongs to the
mask — an overlay that moved the page would be the very defect §11 found in the status line, and one
that did *not* cover the page would let a stray press reach a control behind it. Both halves are asserted
in a real browser.

**What this cost, and where.** One `hidden` id-aware stub change: the Node harness now reads the
document's own `hidden` attributes at load, because a stub that starts every node visible cannot tell a
dialog that ships closed from one that ships open — and every overlay on this page (the sidebar, the
jobs panel, the preview, the dialog, the mask) ships closed. Three policy tests that grepped the old
header markup were retargeted at the screens they now live in, one of them (`the_command_panel...`)
gained a *better* needle — the screen, rather than a window wide enough to span two siblings — and the
split cost one more test that is worth its keep: `the_page_files_every_settings_screen_the_process_hands_it`
reads `page_group`'s arms out of `src/main.rs` and compares them with the page's own `SETTINGS_PANES`,
so a screen the process files rows on and the page does not draw (or the reverse) fails rather than
silently losing rows.

**The second round, measured.** Splitting the dialog into the six screens above changed nothing about
what a *control* does, which is why the round is mostly a tightening: a screen that has been built and
is not the one on top is `hidden`, so a row on another screen cannot be pressed at all — and the browser
harness now presses every row through the screen it expects it on (`ROW(line, screen)`), which turns "the
row is somewhere in the dialog" into "the row is where the frame filed it". Two claims are new rather
than retargeted: a row filed on `limits` is asserted **absent** from the conversation screen while the
conversation screen is up, and `/reload` is asserted to be a button on `this run` and *not* a row on
`this conversation` — a page that put every action on one screen passed the old single-screen claim by
accident.

| Claim | Where it was measured | What came back |
|---|---|---|
| The header holds what identifies the conversation and nothing else | `tests/web_view.rs::the_runs_controls_live_in_a_dialog_and_the_header_keeps_no_door` | the header's own bytes carry the name and the jobs panel; the pickers, switches, actions, `meta`, command rows **and the door** are asserted **absent** from it, and the door is asserted to be inside the sidebar's own foot (see §25) |
| It is a modal, and everything that changes state is inside it | the same test | `role="dialog"`, `aria-modal="true"`, `aria-labelledby` pointing at the dialog's own heading, and the rail, the container, the close button and `meta` asserted to be past the dialog's own offset in the file |
| Three doors, and one Escape order | the same test | the close button, the mask and `dismissTopmost` are all wired, and the modal is asked about *before* the preview in that function |
| One screen at a time | the same test | six named screens in the rail, and `showSettingsPane` shuts the others; a screen the rail does not offer changes nothing |
| The process decides which screen a row is on, and the page draws exactly those | `tests/web_view.rs::the_page_files_every_settings_screen_the_process_hands_it` | `page_group`'s vocabulary (parsed out of `src/main.rs`) is the page's `SETTINGS_PANES`, name for name |
| A report is read on the screen it was asked from | `scripts/web-view-test.js`, and a real browser | the reading replaces that screen's rows with a `‹ back` above it; the ask carries the screen, so a frame arriving in between cannot move it under another heading |
| The menu hands a row off to its own screen | the same two | a `form` row opens the dialog at the row's screen and marks the row there; a report row's reading lands on the row's screen, or the first screen for a row the frame does not file |
| The dialog opens and closes with the keyboard | `scripts/web-view-test.js` | shut on load, `openSettings` builds the screens, unhides it with the mask and focuses `close`; `closeSettings` refocuses the door |
| One `Escape` puts away the thing in front | `scripts/web-view-test.js` | with the dialog and the panel both open, the first press closes the dialog and leaves the panel; the second closes the panel; with nothing open it does nothing |
| The screens are really behind the door, and the page is untouched | `scripts/browser-controls-test.js` against a live run | the door ships hidden then visible **in the sidebar's seat, under the conversation list**, the dialog opens with the mask and the focus, six screens are built with settings and rows already in them, the run's rows are asserted *inside* the dialog, one screen shows at a time, a row is pressed on the screen it belongs to — and the geometry of the page behind is identical to the pixel |

**Not built, and named where it is.** Nothing else from §19's plan is missing: the `/` trigger menu it
designed is built and recorded in §23, and the `form` rows are already in the right place for it — a
secret typed into a *dialog* is not typed into the transcript, which is the property that decision was
made for. One thing this round deliberately did **not** do: the two picker rows (`/provider <name>`,
`/model <name>`) are still settings *and* still rows in the `/` menu, because the menu is a launcher for
a line somebody types and the picker is the same line with the frame's own values offered — one act,
two doors, and the settings control is the one that can be pressed without knowing the vocabulary.


## 23. The `/` menu: a launcher in the composer — **built**

The last piece of §19's plan, and the one that had to be written *after* the settings dialog rather than
before it, because two of its five class decisions are about the dialog: a `form` row opens it at the
row, and a report's reading is drawn in it.

**What it is, in one sentence.** Typing `/` as the first character of the composer opens a list of the
run's own commands above the box; letters after the slash filter it, the arrow keys move the mark,
`Enter` takes the row, `Escape` puts it away, and what `Enter` *does* is decided by the class the frame
gave the command — which is the same table §19 wrote, now held to by a test per class.

**Why it goes above the box.** The composer grows downward as a person types (up to `30vh`) and the
menu has to be anchored to something that does not move. Above the box, `left`/`right` set to the
composer's own padding, so the menu starts where the text does; the box's growth takes space from the
transcript rather than pushing the menu around under the pointer.

**The trigger rule is strict, and that is the design.** The menu is open only while the value *starts*
with a slash and has **no whitespace yet**. Three defects are refused by that one rule: a menu covering
the line it is helping with (`/config set key value`), a menu opened by a slash inside a sentence
(`see src/main.rs and/or docs` offering `/or`), and a menu that has to be dismissed before a line can be
sent. The cost is that the menu is gone the moment an argument begins — which is correct, because at that
point the person is writing the argument, not choosing a command.

**Filtering is scored, not `includes`.** A prefix of the command name scores highest, an appearance
inside it next, a subsequence of it after that (so a two-word command is reachable by its initials),
and a **substring of the help text** last. The help is deliberately *not* fuzzy: a subsequence over a
sentence matches nearly anything a person types, and `/delete` answering to "remove one of them" would
be a list that cannot be learned. Ties keep a stable order, so the list does not reshuffle as somebody
types — and that order is the one the menu **draws** in, which the browser run of §28's round had to
be told twice: see below.

**The dispatch is one pure function and one `async` one, on purpose.** `menuDispatch(row)` answers with
one word (`report`/`dialog`/`values`/`line`/`value`/`none`) and `takeMenuRow` does it — because the rule
that matters lives in the decision and would otherwise be checkable only in a browser: **a `form` row
may not complete the line.** The composer's text is what gets sent to the run and written into the
session file, so a `/provider key` completed into the box would be a credential on disk. The form row
opens the dialog at the row instead, where the field is masked, emptied after a send, and never reaches
the conversation. A policy test slices the branch and asserts there is no `setComposerText` in it, and
the Node harness asserts the line is unchanged after the row is taken.

**Everything observable happens before the first `await`.** The report branch opens the dialog *as* it
asks for the reading (the panel already draws "reading…" where the listing will go) rather than after
the answer arrives, which is both better behaviour — a round trip with nothing on screen is the one way
this could look broken — and the reason the Node harness can check the whole dispatch synchronously in a
stub DOM with no network.

**The first browser run found the bug this slice would have shipped, and the stub could not have.** Two
of the new claims came back red with `box: "/help"` and `box: "/name"`: taking a report or a form row put
the dialog up and left the *query* in the composer. That is not untidiness — the query is a command name,
so the next `Enter` would have sent `/help` (or a form's own line) into the transcript, which is the
exact thing the report route and the masked field exist to prevent. The fix is two `setComposerText("")`
calls, and the reason the Node harness had passed is worth keeping: its check took the row while the box
was already empty, so "the line is untouched" was true and meaningless. Both checks now fill the box with
the query first, which is the state a real press happens in — a claim about an abs*ence* has to establish
what it is the absence of.

**Two things were refused from DSH's version.** `Tab` to complete: the composer's row has `stop` and
`send` in it, so `Tab` is the browser's focus key here and a menu that swallowed it would trap the
keyboard in a list. And a completion that *rewrites* the line as you move the mark: flint's page lets a
keystroke in a menu decide nothing that can be undone by typing again, which is why only the report
class sends anything at all.

**The mark and the screen disagree about "first", and a real browser is what said so** (2026-09-23,
while §28's round was being measured — the two claims about the bare `/` came back red twice in a
harness that had been green, which is the difference between a flake and a defect). Chasing it found
**two** faults, one in the page and one in the harness, and they are worth keeping apart.

The page's: the menu has two orders and only one of them decided a tie. `menuRows` sorted by score and
then by the **frame's** order, while `showMenu` *draws* by class (`MENU_GROUPS`: reports, actions,
selectors, forms, destructive) — and the frame's order is the process's command table, which is not
grouped that way. A typed query never shows it, because the score decides. A bare `/` shows it every
time: every row ties at 1, so the tie-break was the frame's order and the mark landed on a row that was
not the top one on screen — the reader sees the top row unmarked and `Enter` taking a different one.
The fix is one comparator: **score, then the order the menu draws in, then the frame's.** The score
stays first, because that is the palette's promise and what a query is asking about — a query that
names a row still comes back with that row first even when it is drawn in the last group; the class
only decides a tie, which is exactly what the bare `/` is made of. It is held twice, both watched red
first against the old comparator: a Node claim over a deliberately *interleaved* frame (the fixture
beside it is grouped by class, which is why it could never have caught this) and an assertion in
`tests/web_view.rs` that slices `menuRows` and names both halves of the sort.

The harness's, and it is the one that was actually failing: **`Input.dispatchMouseEvent` leaves the
pointer where the last press put it, and a row under the pointer is marked on `mouseenter`.** The claim
before the menu presses a path button in the transcript's last turn — one press away from where the menu
appears — so the menu opened *under the pointer*, hover marked a row nobody's keyboard had chosen, and
"the keyboard starts on the first row" read a mark five rows down. Two rounds of measurement went into
the page before the pointer was suspected, which is the lesson worth keeping: the claim said *keyboard*
and the cause was the *mouse*, and the second symptom (`ArrowUp` "not bringing it back") was the same
arithmetic from the same wrong starting row. The fix is one `mouseMoved` to the corner before typing, in
the helper every menu claim goes through, and the claim now prints `first` as well as `marked` so the
next reader of a failure sees both halves.

**One `Escape` order gained a step.** `dismissTopmost` now asks about the menu first, before the
settings dialog: the menu lives inside the composer, where the keyboard already is, so it is the thing
in front of everything. That is a change to a claim `docs/features.md` already carried (the order was
settings → preview → jobs), and it is recorded there with the new count of keys the page handles itself.

| Claim | Where it was measured | What came back |
|---|---|---|
| It opens on a slash and on nothing else | `scripts/web-view-test.js`, calling `menuQuery` | `/` → the empty query, `/pro` → `pro`, a leading space is allowed, a space after the command is not, a slash mid-sentence is not, and a missing value does not throw |
| Prefix beats subsequence beats help | `scripts/web-view-test.js`, calling `menuScore`/`menuRows` | `/re` finds `/reload` first, `pkey` finds a two-word command, `proxy` finds the row whose help says proxy, `zzz` finds nothing, ties keep the frame's order, and a subsequence of a *help* string does not match |
| The rows are the frame's, marked one at a time | `scripts/web-view-test.js`, calling `showMenu`/`menuStep` | the group headings are the page's own and the rows are the frame's; the mark starts on the first row, moves one row at a time past the headings, and wraps; `zzz` says `no command matches /zzz` and leaves the menu open for a backspace |
| A page with no run offers no menu | `scripts/web-view-test.js` | with no `state` frame, `showMenu` leaves the box hidden |
| What a row commits to is its class's answer | `scripts/web-view-test.js`, calling `menuDispatch` | report → `report`, action → `line`, selector → `line` or `values`, form → `dialog` **even when the row also carries values**, destructive → `line`, and nothing → `none` |
| A form row never touches the line | `scripts/web-view-test.js` and `tests/web_view.rs` | taking it clears the query it was built from, opens the dialog on **the row's own screen** (`screenOf(command)`), and marks that one row; the branch itself contains no `setComposerText` *completion* — the one call in it is the clearing one, which is why the Node check fills the box first |
| The menu page carries no command name | `tests/web_view.rs::the_slash_menu_is_a_launcher_drawn_from_the_frame` | the menu is inside the composer form and ships `hidden` with `role="listbox"`; its rows are built from `row.send`/`label`/`help`; and the four strings that would be a leaked list (`/provider key`, `/delete <n\|id>`, `/reload`, `inspect the config`) appear nowhere in the page — twice over, since the list test already checked them |
| The menu never sends anything but a report | the same test | `takeMenuRow` holds no `fetch` of its own; the one request it can cause goes through `askReport`, and the dialog is opened before that call |
| One `Escape`, and the menu is in front | `scripts/web-view-test.js` | with the menu and the dialog both open, the first press closes the menu and leaves the dialog; the next closes the dialog |
| A real keystroke opens it, filters it, and a space closes it | `scripts/browser-controls-test.js` against a live run | `/` opens it on the real binary's own command list with one row marked; `usage` narrows it to `/usage` first; ` now` closes it and the box still holds `/usage now` |
| The arrows move the mark, and not the caret | the same harness | `ArrowDown` moves the mark one row down while the box still reads `/` (which is what `preventDefault` buys), and `ArrowUp` brings it back |
| `Escape` keeps what was typed | the same harness | the menu is shut and the box still holds `/` |
| `Enter` on a report row reads it instead of sending it | the same harness | the reading is drawn where the rows were, on the screen the row's own `group` names (a `‹ back` button and the answer), the box is empty, and **the terminal gained nothing** — the claim the `/report` route exists for |
| `Enter` on a form row writes nothing | the same harness | the dialog opens at that row's screen with exactly one row marked, and the box is still empty |
| `Enter` on an action row completes the line and sends nothing | the same harness | the box reads `/reload`, the menu is shut, the terminal gained nothing — and the person's own press of `send` is what makes it print |
## 24. Markdown in the preview: a reading, with the file one press away — **built**

§20 answered the question a person asked (*"the preview should understand Markdown, but is that too much
complexity?"*) with three scopes and a recommendation: **B behind a raw/rendered toggle**, and C only when
a link in a previewed document is actually missed. Built on 2026-09-18, in the same batch as §21–§23.

**The two steps became one, and that is the one place the recommendation was not followed literally.**
A (line-level styling) would have been a loop over lines deciding what to style from the first character;
B is the same loop deciding what *block* the line starts. Nine tenths of A's code would have been deleted
by B's, so A was folded into B rather than built first: the styling A promised -- a proportional face, a
measure, a bolder heading, a monospaced fence -- is all there, it is just driven by a real reading rather
than by the shape of a line. C was **not** built, which is exactly what §20 recommended, and the reason is
still true: nobody has missed a link in a previewed document, and `**bold**` and `` `code` `` sitting there
as they were written is legible in a way that a wrong render is not.

**What it is.** A `.md`/`.markdown`/`.mdown`/`.mkd`/`.mdx` file read through `GET /file` is drawn by
`markdownBlocks` (lines in, blocks out, no DOM) and `paintMarkdown` (blocks in, nodes out) into
`#preview-md`, and the `pre` beside it keeps the file's own bytes. One button in the panel's head switches
between them, its label naming what a press would *show* (`source` while a reading is on screen,
`rendered` while the bytes are), its `aria-pressed` naming what is in force, and the note beside the path
ending in `· rendered` — because the honest answer to "the renderer got it wrong" has to be visible
without pressing anything.

**The dialect is the whole honesty of this feature, so it is written down.** Blocks: ATX headings
(`#`–`######`, with a closing run of hashes ignored), setext headings (`===`/`---` under a line), fenced
code (three backticks or three tildes, with the info string kept as a caption), bulleted and numbered lists nested by
indentation and split when the marker *kind* changes, blockquotes, thematic breaks, pipe tables with the
alignment their divider asks for, and paragraphs whose wrapped lines are **joined with a space** — which is
what these files' 100-column prose needs and what Markdown does. **Inline syntax is not parsed at all**:
`**bold**`, `` `code` `` and `[text](url)` come out as the characters the file holds. Raw HTML is not
"refused" — it is *text*, because the page never assigns markup, which is a property of the whole page
rather than a rule of this function (the same test that has held since §1). And anything the reader does
not recognise is a **paragraph**, never a guess: a line that looks like nothing else becomes words, which
is exactly what the raw view would have shown.

**Two rules the panel already had decided the shape of this.**

- **A line opens the source.** `previewView(path, line)` is the whole of it: a Markdown file **without** a
  line opens rendered, one **with** a line opens raw, and everything else opens raw. A `grep` hit or a
  compiler error names a line, and "line 412" has no meaning in a reading — so asking for a line is asking
  for the file's own lines. This is the rule that keeps a rendering from ever *replacing* the file.
- **The bytes are read, not kept.** The switch flips `preview.view` and calls `readPreview()` again rather
  than holding the last body: a second copy of a file in the page would be exactly the derived state this
  project does not keep, and the route is one round trip away. It is the same reason `reload` exists as a
  control rather than as a cache.

**Four defects were found while building it, and three of them were the *point* of the design.**

1. **An item's own words were lost when it had a nested list.** `markdownBlocks` returns an item's lines
   as blocks, and the first version pushed *all* of them into the item's children — so `- two` followed by
   `  - nested` became a bullet with an empty label and a list under it. Three Node checks went red at
   once, which is the argument for making the reading a pure function: the failure was in the reading, it
   was reported as the reading, and a browser never had to be opened to find it. The fix is that a leading
   paragraph stays on the item as its text and everything after it is a child.
2. **A `<script>` in a page *comment* broke the harness's own extractor**, which finds the first
   `<script>` in the file to eval the page's script — so the markup comment that said "a file containing
   `<script>` renders as text" made the harness parse markup as JavaScript. **And the same trap bit again
   in the same block**: the harness's page-eval expressions are backtick-delimited, so a backtick inside
   one (in a comment about a tag) closed the string and turned prose into code, which reads as
   `b is not defined` from a line that has no `b` in it. Both are recorded because a page-eval string is a
   program written inside a string: the extractor and the delimiter are two ways the *enclosing* file
   decides what the page's code is.
3. **A duplicate `const` in the harness** (`reading`, already bound by the settings block) — the same
   shadowing mistake §23 hit with four names, and the reason the block was renamed to `drawnNotes` rather
   than the older binding: the older one is a claim about the dialog.
4. **The harness printed only an error's message, not its stack**, which cost a full browser run to locate
   defect 3. It prints the stack now.

**One claim was seen red once, and it was not this slice's.** `a hit's line travels with the path` — a
§12 claim about pressing a `grep` hit — failed on one run of the harness and passed on the runs either
side of it, with nothing in the tree changed in between; the detail line was lost to a `tail` on the way
out, so what it said is not recorded. It is the same unreproduced class as the two CI flakes `HANDOFF.md`
already carries, and it is written down rather than rerun-until-green because a claim that has been red
once is a claim the next reader should re-run before believing: **93/93 held** on the runs that matter.

| Claim | Where it was measured | What came back |
|---|---|---|
| Markdown is a name, not a guess | `scripts/web-view-test.js`, calling `isMarkdown` | `.md`, `.markdown`, `.mkd`, `.mdx` and an uppercase name are Markdown; `.txt`, `.png`, `a.md.txt`, `md` and a missing path are not |
| Lines become blocks | the same harness, calling `markdownBlocks` | headings at their level (a closing `##` run dropped), paragraphs joined, a bulleted list with a nested list inside its second item, a numbered list as its own kind, a quote whose lines join, a rule, and a fence with its info string and its body verbatim |
| A list ends where a reader would say it ends | the same harness | a change of marker kind starts a new list; a blank line between two items is one loose list; a blank line before prose ends it; a fence indented under an item belongs to the item |
| A pipe table is a table | the same harness | head cells, two body rows, alignment `["", "right", "center"]` from the divider, and **a pipe with no divider under it stays a paragraph** |
| What it does not know, it does not eat | the same harness | setext headings read; `<script>alert(1)</script>` and `<img src=x onerror=y>` are text; an unclosed fence is still a code block; indented text is a paragraph; an empty or missing body has no blocks |
| Blocks become nodes, and every string arrives as text | the same harness, calling `paintMarkdown` | one node per block (`h1`, `p`, `ul`, `pre`, `table`), one `li` holding the nested `ul`, a fence as a `.lang` div plus `code`, a table with `thead`/`tbody` — and `**bold** and \`code\` stay as written`, which is the inline decision stated as a fact rather than a promise |
| A Markdown file opens rendered, a line opens raw | the same harness, calling `previewView` | `.md` → rendered; `.md` at line 412 → source; `.txt` → source; `.png` → source; a missing path → source |
| The switch is offered only where there is a choice | the same harness, calling `paintPreviewView` | the button un-hides and reads `source` with `aria-pressed="true"`, the note ends in `rendered` and the rendered container has the blocks; opened at a line the button reads `rendered`, the `pre` holds the bytes and the note says `line 412`; a `.txt` gets no button at all |
| A refusal takes the switch away | the same harness, calling `paintPreviewRefusal` | the switch and the rendered container are hidden, the route's sentence is the text and `HTTP 404` is the note |
| The page carries the reading and nothing else | `tests/web_view.rs::a_markdown_file_is_read_not_just_shown` | the rendered container ships `hidden`; `markdownBlocks` contains no `document.` (the reading is a function of its text); the painter builds through `el(` and names no markup assignment; `previewView` is the `isMarkdown(path) && !line` ternary; the switch flips the view and re-reads; a refusal takes both away |
| A real browser draws the reading | `scripts/browser-controls-test.js`, against a live run | a `notes.md` path from the transcript draws `h1,p,ul,blockquote,table,pre`, the heading's text is `The notes`, one nested `ul`, two `td`, the fence's `const a = 1;`, and the button reads `source` |
| A tag in the file is text on screen | the same harness | the paragraph's `textContent` contains `<b>tag</b>` and `` `code` left as written `` — read out of the *live* DOM, which is the only place this claim could be made |
| The note says which reading is on screen | the same harness | the note ends in `rendered` and still carries the byte count |
| `source` shows the bytes, `rendered` reads again | the same harness | the `pre`'s first line is `# The notes`, the rendered container is hidden, the button reads `rendered` and the note no longer says so; a second press reads it again |
| A path that names a line opens the source | the same harness | pressing the transcript's own `notes.md:2` shows the `pre` with a note starting `line 2` and the button offering the reading |

**What a person gives up by having this, said plainly.** A rendered view is *the page's* reading of the
file, so a defect in it is a defect in what somebody is looking at — which is why the source is one press
away, why the note names the mode, and why the dialect is written down here rather than described as "a
Markdown renderer". Nothing about the file changes: `GET /file` still serves the same bytes, `/file` still
refuses what it refused, and the reading exists only in the panel. §20's closing sentence — that the
preview's contents are the file and not a transformation of it — is now true of *one* of two views rather
than of the only one, and the toggle is the sentence.
## 25. The dialog in DSH's own shape, and a hand on the preview — **built**

Asked for directly, after using the page: *the settings are still bad — read DSH's own for reference —
and the preview on the right cannot be dragged to resize it*, plus one more sentence about where the door
belongs: *put the settings at the bottom left*. Three faults, one round, and each is a different kind:
the first is a page that looked like settings without being one, the second is a control that was simply
missing, and the third is a seat.

**What "still bad" meant, read off DSH's own panel rather than remembered.** The dialog already had
DSH's *geometry* from §22 — a centred modal, a rail down the left, one screen at a time — and none of its
*quality*. It was a rail of six buttons over a column of rows, each row a label, a sentence, and a native
`<select>` or a one-line form; the command rows were a flat list under the settings with no headings at
all. DSH's own surface was read for this (its `settings-general` client bundle: the panel is
`800px` wide, `height: min(800px, 100vh - 48px)`, `border-radius: 32px` with a prominent shadow; the nav
is a `188px` column with a `16px/500` title and `40px` cells at `12px` radius; the content is a `54px`
header with a `28px` round close and one scrolling options region; its trigger is a `42px` cell in the
sidebar's **bottom seat**). flint keeps its own palette, its own lowercase vocabulary and its own tokens —
what it took is the *shape*: a real rail cell set, a heading over each group, and a control that is the
page's rather than the operating system's.

| What changed | What it was | What it is now |
|---|---|---|
| The door | the last control on the header's line | the **sidebar's bottom seat**, its own row under the conversation list, and the seat is `hidden` with the door (an empty strip with a hairline over it is worse than nothing) |
| The rail | six buttons in a bare `div` | a titled, `190px` column: `settings` at `15px/600` over a list of `40px` cells at `12px` radius, the one in force filled, the whole set inside a bordered column |
| A setting's control | a native `<select>`, or a form | the frame's own words as **buttons**, the one in force pressed (`aria-pressed`), and a single word is one disabled button; a typed value is still a form, with the frame's `kind` as the input's type |
| The command rows | one flat list of rows per screen | a **group per class**, in the `/` menu's own order, each under its class's word (`reports`, `actions`, `selectors`, `forms`, `destructive`) |
| The rows themselves | a control with two bare spans | a text block (name at `13.5px`, help at `12px` dim) and the control beside it, separated by a hairline that the first row of a group does not get |
| The panel's width | `min(720px, 45vw)`, fixed | `var(--preview)`, set by a **hand** between the reading and the file, with the same gestures the other two boundaries have |

**The class is the group, and the words are the menu's.** §11's rule for the `/` menu is that the frame's
five classes already say what a press would do; the same five say how a *screen* should be read, so the
groups are `MENU_GROUPS` — one vocabulary, not two. A page that invented its own headings ("commands",
"reports and tools") would be teaching a person a second set of names for the same five things, which is
exactly the drift this repository spends its comments preventing. The order is the menu's order too, which
is what puts the destructive rows at the bottom of a long screen without anybody deciding to.

**An unfamiliar class is drawn, not dropped — and the Node harness is what found it.** The first version
of the grouping filtered rows by the classes the page knows and drew one untitled group for rows with
*no* class; a row whose class the page has never heard of (a command added to the process after this page
was written) went into neither and **vanished from the screen**. The harness's `a row this page cannot
press says where its control is` failed with `Cannot read properties of undefined`, and the fix is a
group per unknown class, untitled, in the frame's order. It is the same rule the report fallback follows
one level up: what the frame offers is not this page's to lose, and a page that hides a command it does
not recognise is the worst version of a page that knows a command's name.

**Why the `<select>`s went, and the one that stayed.** A native `<select>` is the operating system's
control: its own size, its own colours in a dark page, and a list that opens *over* the dialog it is in.
It also cannot be driven by any protocol, which is the residue §11 carried for three rounds as "a picker
from the keyboard" — with the words already in the frame, the honest control is the words themselves, one
press each, and the residue disappears with the widget. The dialog has exactly one `<select>` left and it
is not a setting: the peer picker in `/say`'s form, which is *an answer being given* rather than a value
being set, and where the OS list is the right shape.

**The preview's hand, and the defect only a browser could find it had.** The panel is a grid column
(`§12`), so the boundary between the reading and the file is a boundary like the sidebar's, and it takes
the same three gestures through the same `dragBoundary` helper the other two use: a pointer drag, the
arrow keys (`16px`, `48px` with `Shift`), and a double-click to put the width back. It is clamped to
`280px … min(1100px, window.innerWidth - 420px)` so that neither hand can squeeze the transcript to
nothing — the same argument as `min(var(--side), 60vw)`, in JS because the number depends on the window
and on where the panel's own right edge is. `openPreview` unhides the hand and writes its `aria-valuenow`
from the panel's measured width; `closePreview` hides it again, because a grip left over a shut panel is
a control onto nothing that would eat the pointer events of whatever ends up under it.

The defect is worth recording as a type, not an incident. The hand's grid track is `auto` (so that both
preview columns collapse to nothing when no panel is open, which is what keeps a reading page from losing
5px to a control that is not there) — and **an empty `div` in an `auto` track is 0px wide**. The page
looked right, the claims read right, and the harness said `width: 0`: the drag returned `false` because
the pointer press landed on the transcript beside it, and the double-click never reached the hand at all.
The fix is one line in the CSS (`width: 5px`), and the reason it took a browser to see is that *nothing
in the page's own state is wrong* — there is no frame, no value, no node that says "the hand is zero
wide"; only a laid-out box does.

**What the harnesses found, in order.** The Node harness found the dropped class (above). The Rust policy
tests found the three places the markup is grepped and had to be retargeted at the new chrome — one of
them *better* than it was: `the_settings_are_the_runs_own_lines_and_nothing_else` now pins
`sendText(send + " " + word)` in `choiceControl` and `setting.choices` in `settingRow`, which is the
composition rule stated where it is composed rather than in a window that spans two functions. The
browser harness found the width-0 hand, and one thing about *itself*: a claim that a `<button>` can be
pressed by a raw `Input.dispatchKeyEvent` for Enter does not hold — button activation on Enter is the
browser's own default action and the protocol's raw key event does not carry it — so the two claims that
used to press a `<select>` with `ArrowDown` now press the word with a real click and assert that it is
focusable, which is the half of the keyboard path this page actually owns.

| Claim | Where it was measured | What came back |
|---|---|---|
| The door is the sidebar's bottom seat | `tests/web_view.rs::the_runs_controls_live_in_a_dialog_and_the_header_keeps_no_door`, and the browser harness | the door's own bytes are inside the sidebar's foot, the header still carries the name and the jobs panel and **not** the door, the seat and the door ship `hidden` and are shown together when the frame describes a run — and in a browser the seat's top is at or below the bottom of the list above it |
| A choice sends the frame's line and nothing else | `tests/web_view.rs`, over the page's bytes | `choiceControl` contains exactly one `sendText(send + " " + word)`, and the words come from `setting.choices` |
| The words are the frame's, including the value in force | `scripts/web-view-test.js` | the words in order, the one in force pressed, a single word a disabled button, and a value the frame did not list drawn *first* and pressed |
| A screen's rows are grouped by their class, in the menu's order | the same harness | the group headings are `reports`/`actions`/`destructive` for the classes present, the frame's order is kept inside a group, and a class the page has never seen is drawn untitled rather than dropped |
| A reading still replaces its own screen's rows | the same harness | the way back, the heading and the answer are the screen's row list *instead of* the groups, and going back draws the groups again under their headings |
| The panel's hand is in the layout, and works | `scripts/browser-controls-test.js` against a live run | width `5px` and 0–12px from the panel's left edge, `--preview` grows by the drag *and the panel with it*, grows by `16` with the arrow, `aria-valuenow` agrees with the measured width, the double-click removes the property, and `Escape` hides the hand with the panel |

**The gate, after all of it.** `cargo test` 681 passed / 1 ignored (the pty test, Unix only) across its
suites — unchanged, since this round retargeted three tests rather than adding any; `cargo clippy
--all-targets -- -D warnings` silent; `node scripts/term-layout-test.js`,
`node scripts/web-view-test.js`, `python examples/python/test_call.py` and `python examples/mcp/test_mcp.py`
all green; and `scripts/browser-controls-test.js` **109/109** in headless Chrome on Windows — a hand run,
because it needs a real browser.

## 26. Cutting a branch from an answer — **built**

Asked for directly, after using the page: *the conversation branching came from Pi, where you can split
off from a node — and there is no way to do that in the web view; I want to branch from one of the AI's
answers.* The command existed in both doors and only one of them could be used: `/fork [n]` in the
terminal lists the questions and cuts in front of the one you name, while the page offered a row of
**numbers** in the settings dialog, which is a question list with the questions taken out, and nothing at
all in the transcript — the place a person is actually looking when they decide they want a different
answer to *that* question.

**The one hard part is that the two sides count different lists.** `/fork n` counts questions in the
run's *history*, and the page has drawn `chat` lines out of a *file*. Those are the same list only while
nothing has been folded: `/compact` replaces a prefix of the messages with one summary, so after it the
run holds fewer questions than the file has, and the page cannot know how many went — that is a fact
about the run's history, and the page may not read it (§11: the frame is the page's only channel). Nor
may the process count the page's turns for it: a "drawn index" per question is the process handing back
the page's own scrollback, and it would need a second bookkeeping of every message the writer ever wrote
to compute. So neither side counts for the other; the two lists are **paired**.

**What one pairing is.** The frame's `/fork` row now carries the questions twice: `values` (the numbers
the command takes) and `labels` (their first lines, `util::preview`'s own clipping — the same string the
terminal's list prints). `branchPoints` in the page walks the run's labels and the page's own user turns
**from the bottom together**, and believes a pairing only when that turn's first line is the question the
label names and is the only turn there that reads that way. From the bottom because that is where the two
agree: a fold drops a prefix and never the newest question, so the run's list is a *suffix* of what the
page drew. Where they disagree the page draws nothing — no button is a missing convenience, and a button
on the wrong answer cuts a branch somewhere nobody pointed at, which is a new file the person would have
to read to discover.

Three more rules, each a way this can be wrong:

- **The first value is never a button.** `/fork 1` cuts in front of the question the run has held
  longest, and nothing the run still holds comes before it — so the answer the page drew above it is a
  fold's tail (or nothing), and a button there would promise a branch that does not contain it.
- **A question that reads the same twice is skipped, not guessed at.** Two turns matching one label is
  two answers a person could have meant, and the page has no way to tell the folded one from the held
  one; the numbers in the dialog's list are what disambiguate that, so the transcript says nothing and
  the labelled list still works.
- **A turn in flight takes the buttons away.** The frame is built between turns, so while an answer
  streams the run's list is one question behind the page's drawing and *every* pairing is out of step.
  Refusing them all is the honest drawing, and it is why this round did not need a rule about which turn
  is "the one being asked".

**The state frame is what carries the list, so it is also what draws the buttons** — and that was a
defect the Node harness found rather than a detail: `showState` repainted the settings and the panel's
header, and the transcript only repainted on a transcript event. A page that had just loaded reads
`/session`, draws the conversation, and then waits for exactly the frame that names the questions — so
the buttons would have appeared only after the next answer moved the transcript, which is the one moment
they are wanted. `showState` now paints the transcript as well (the given document, not the page's own —
a state that arrived for another one has to draw there).

**Three defects in the terminal half came out of building this**, and each is the same fault seen from
the page: a control offered for something that will be refused.

| What was wrong | What it did | What it is now |
|---|---|---|
| a fold's summary counted as a question | `/fork` with no argument listed `1. [the conversation before this point, summarized by flint at the person's request] …` and numbered every real question one higher than the transcript; `--fork` at startup and the page's own values were one out of step with it too | `session::is_summary` (one marker constant, `SUMMARY_MARK`, next to the message that writes it) and `questions()` skips it |
| `n == 1` refused by name | cutting in front of the first question a compacted run still holds keeps the **summary**, which is a branch with something in it — the digest, ready for the question being re-asked — and the refusal was false exactly there | the copy is built and refused only when it is **empty** |
| a `--no-session` run offered the questions | the frame carried `/fork` values whose every press answered with `this run keeps no conversation (--no-session)`; the terminal checks that *first* and lists nothing, so the two doors disagreed | `page_rows` builds the list empty when `no_session()` |

**What was measured, and where.**

| Claim | Where it was measured | What came back |
|---|---|---|
| A fold's digest is not a question, and cutting in front of the first real one keeps it | `tests/cli_output.rs::a_compacted_conversation_forks_from_the_questions_it_still_holds` | the list is `1. the kept question` and `(1 question`; `/fork 1` prints `1 message kept, cut at question 1 of 1: the kept question` and the branch's file holds the summary and not the question. Watched red first: with the summary counted, the captured screen reads `1. [the conversation before this point …` / `2. the kept question` / `nothing to keep: cutting at question 1 would leave an empty conversation` |
| A `--no-session` run is offered no cut, on a question it does hold | `tests/cli_output.rs::a_run_that_keeps_no_conversation_offers_the_page_no_cut` | the frame under test is fetched by moving a setting (`/verbose off`), so its arrival is certain; without the guard the same frame carries `"labels":["why does the socket close early"]`. Watched red first |
| The frame carries the questions beside their numbers | `tests/cli_output.rs::the_page_is_offered_the_arguments_a_command_takes` | `"labels":["why does the socket close early","what about the retry path"]` for `/fork`, alongside `"values":["1","2"]` |
| Where a button may be drawn, and what it sends | `scripts/web-view-test.js`, eight claims | the settings list labels each value; exactly one button, under the answer a cut keeps; the line is the frame's `send` plus the frame's value; a frame that arrives on its own draws the button (watched red first: with the `showState` paint removed, the claim fails and the button never appears); over a fold the *folded* answer is left alone and the held one is marked; a turn in flight empties the transcript of buttons; two questions that read alike mark only the third; a repaint keeps the untouched answers' nodes |
| The page's half of the rules | `tests/web_view.rs::the_branch_button_is_drawn_from_the_frame_and_verified_against_the_turn` | the row is found by the frame's `send` and the line composed from it; labels and values must line up in length; the pairing is text-checked and must be the only one; the first value is skipped; the mark is part of `paint`'s comparison; and a state frame paints the transcript |
| The whole interaction, in a browser | `scripts/browser-controls-test.js`, four new claims | with one question there is no button anywhere; after a second question exactly one appears, under the answer the cut keeps, naming that question; pressing it makes the run print `cut at question 2 of 2: and the tests`; and the page is then drawing the branch — the second question is *gone*, which is the `reset` frame plus `GET /session` doing what §15 says |

**The gate, after this round.** `cargo test` **684 passed / 1 ignored** (the pty test, Unix only) across
its 14 suites — 681 before the round, plus two `cli_output` tests and one `web_view` policy test;
`cargo clippy --all-targets -- -D warnings` silent; `node scripts/term-layout-test.js` and
`node scripts/web-view-test.js` (nine claims added to the transcript section, all green) — both run by
CI; both Python doors green; and `scripts/browser-controls-test.js` **113/113** in headless Chrome on
Windows (was 109, with four claims for this round: the absent button, the button under the right answer,
the press and what the run printed, and the branch the page is left reading), by hand, because it needs
a real browser.

**What is deliberately not here.** A button for *branching from a question* rather than from an answer:
the cut keeps what is in front of the question, so the honest place for it is the answer that survives —
the same reason `--fork` takes a question *number* and not a message. Nor a branch button on the newest
answer, which would mean "ask this again in the same conversation" and is a different act. Nor a way to
un-fold, which the file allows by hand and the page has no reason to offer.

## 27. A directory, read: the panel's second reading — **built**

Reported directly, 2026-09-23, while reading a run that had listed a directory:

> 现在目录还是跳转不了，然后带空格的目录依然无法正常识别
> — *pressing a directory still does not go anywhere, and a directory with a space in its name is still
> not recognised at all.*

Two symptoms, and they turned out to be one missing idea. A directory had exactly one treatment in this
page: `GET /file` refuses it by its own rule ("is a directory, not a file"), so a path that named one
filled the panel with a true sentence and nowhere to go — and a *name with a space* could not even reach
that far, because the scanner reads tokens and `My Projects` is two of them. §18 stated that residue as
unfixable, and for a reader of *text* it is: nothing in `Application Data  (0 bytes)` says where the name
ends. What §18 did not consider is that the page does not have to read that line as text at all — the
run knows the directory, and can be asked.

**The answer is a route, not a smarter scanner.** `GET /dir?path=…` answers with what is in a directory
as **data**: `{"path", "parent", "entries":[{"name","line","path","dir","size"}], "total", "shown"}`.
Three decisions in that shape are the whole of it:

- **Each entry carries the full path, joined by the process.** The page never joins a name onto a
  directory: separators are the business of the machine the run is on, and `My Projects` is one name
  there and two tokens here. This is the same rule the page follows about commands — it composes
  nothing it can be told.
- **Each entry carries its own `line`** — `name/` for a directory, `name  (N bytes)` for a file — built
  by `tools::DirItem::line`, which is the function the `list` tool prints from. One answer, two doors: a
  person reading the panel and a model reading a tool result are told the same thing in the same order,
  and neither can drift from the other without the shared function changing.
- **The path is a *listing* route rather than a mode of `/file`.** `/file` serves bytes; a directory has
  none, and the two answers do not even have the same content type. A third route is cheaper than a
  route with two shapes.

The `list` tool was rewritten onto the same function (`tools::directory_items`), which is a small
refactor with one visible consequence: the entries are sorted by the **printed line** in both doors, so
`My Projects/` sorts beside `My Notes.txt` rather than in a block of its own. It was `lines.sort()` on
the printed lines before, so nothing moved — the sort is now stated where both callers read it.

**The refusal keeps its sentence and gains a header.** A path whose *name* ends in a separator is asked
of `GET /file` first, because that is the one thing a name can say about being a directory. Everything
else — `C:\Users\zhangzhuo`, which is how a `list` call names the directory it wants, and which is the
case that was reported — goes to `/file`, whose refusal carries `X-Flint-Dir: 1`. The page reads that
header and knows what it is holding; the sentence in the body is unchanged, because it is for the
person. Matching the words "is a directory" was rejected: that is this page parsing prose it wrote
itself, which is worse than parsing a stranger's, because it looks safe.

*(What the page did with that knowledge changed one round later — the header now leads to a **press**,
not to a reading of the panel. §28 is where that decision lives; the route's half above is unchanged.)*

**The panel's second reading.** A listing draws one `button.path` per entry into the same body a file's
lines go in — a directory is a *reading of a path*, not a different panel — with `..` first when the
route says there is a parent, the directories in full ink and the files dim, and the raw/rendered switch
put away (there is no Markdown reading of a directory, exactly as there is none of a refusal). Pressing a
row asks for that row's path: a file opens beside the turn, and a directory is opened where it lives
(§28 — when this section was written it went *into* the directory, and the round after changed that).
The panel's head keeps its own rule — it shows the path that was pressed, not the one the route resolved
— and now strips a trailing separator before splitting it, so `/a/b/` reads as `b` inside `/a/` rather
than as a whole path with an empty name after it. A listing with more entries than one answer carries is
**counted** rather than quietly shortened: the note says `the first 2000 of 4321 entries`, the same rule
`/file` follows when it cuts a long file.

**The run's own listing is readable as names, which is where the space is fixed.** The `list` tool
prints one entry per line as `NAME/` or `NAME  (N bytes)`, and that line is evidence no token stream
has: the mark says where the name ends, so everything before it — spaces and all — is one name. The
page's splitter gained a second alternative that matches exactly such a line (`addressParts`, tried
before the bare-token rule and anchored to the line), and the row becomes one button whose text is the
name: `Application Data  (0 bytes)` is now a button reading `Application Data`, and `My Projects/` a
button reading `My Projects` with the run's `/` left as the text that follows it. This is the same kind
of rule as the one that reads `src/web.rs:412` as a path and a line: a *known format of this program's
own output*, read where relative names are read at all.

**What was measured, and where.**

| Claim | Where it was measured | What came back |
|---|---|---|
| `/dir` lists what is in a directory, in the `list` tool's own order | `src/web.rs::a_directory_is_read_as_a_listing_of_what_is_in_it` | `["My Projects", "notes.txt"]`, the directory's `path` joined by the process, `parent` for the way up, and the tool's own text asserted beside it (`My Projects/`, `notes.txt  (4 bytes)`) — the same directory, two doors. Watched red first: `404 no such route` |
| `/file`'s directory refusal names the kind in a header | `src/web.rs::a_missing_file_a_directory_and_a_binary_each_say_what_they_are` | `X-Flint-Dir: 1` beside `sub is a directory, not a file`. Watched red first: the header was absent, and the test's message says why the page needs it |
| A file is not a directory, and nothing is not either | the same test | `notes.txt` → 400 `not a directory`; `nowhere` → 404; `?path=` empty → 400 |
| Which route a path is asked of, and what a listing becomes | `scripts/web-view-test.js`, two claims | the route's encoding; a trailing separator decides the first request; the header decides the correction; `..` first, then the entries, each carrying the run's own path and label; a capped listing counting what is there; the drawn rows' classes, labels and tooltips; the head after a press |
| The run's own listing rows are names, spaces and all | `scripts/web-view-test.js` | `My Projects/`, `notes.txt  (12 bytes)` and `Application Data  (0 bytes)` are each one path; the pieces rejoined are the line that came in, so nothing is eaten; prose is not a listing. Watched red first: with the row rule disabled, the same claim reports `["notes.txt"]` — the spaced rows vanish and the note survives only because it has an extension |
| The whole interaction, in a browser, on a real directory | `scripts/browser-controls-test.js`, seven claims | a directory named in the transcript is **read** rather than refused; `..` is the first row; the way up lists the parent and a row of that listing is a way back down; pressing `sub dir` — a real directory with a space in its name — lists what is inside it; the head shows the whole path; a file reached through the listing reads as its own bytes; and the same name is pressable in the run's own listing in the transcript, with the block unfolded. **Those seven claims were replaced a round later**, when a press on a directory stopped going into the listing at all: they are now four claims in §28, and the listing they used to drive is held by the two `scripts/web-view-test.js` rows above and by `/dir`'s own route test |

**The residues, stated rather than hidden.**

- **A row from the run's own listing resolves against the run's working directory**, because that is the
  name a model listing `.` means. A listing of somewhere *else* therefore names entries relative to the
  wrong directory, and the panel says `nothing at AppData` rather than opening the wrong file: pressing
  the *directory* first is how to read that one, and the panel's own listing carries full paths for
  exactly this reason.
- **A bare path with a space outside a listing stays split** — the residue §18 named, and the one §28
  then closed for a *bare absolute* path by asking the run. What is left of it is in §28: a relative
  name still is not asked about (in prose `src/My Dir` is two words far more often than one directory),
  and a page with no run behind it asks nobody and keeps the cut.
- **A path in a tool call's arguments is shown as the JSON that carried it**, so on Windows its
  separators read doubled (`C:\\Users\\…`) in the button and in the panel's head. Measured: the run
  resolves it anyway — repeated backslashes are one separator to Windows — and the panel showing what it
  was asked for, rather than a normalised spelling of it, is the rule the head already followed. The
  terminal is not in this position: it prints its own summary of a call, not the wire form.

**The gate, after this round.** `cargo test` **686 passed / 1 ignored** (the pty test, Unix only) across
its 14 suites — 684 before the round, plus one `web.rs` route test and the header assertion in an
existing one; `cargo clippy --all-targets -- -D warnings` silent; `node scripts/term-layout-test.js` and
`node scripts/web-view-test.js` (two claims added, all green) — both run by CI; both Python doors green;
and `scripts/browser-controls-test.js` **121/121** in headless Chrome on Windows (was 113, with seven
claims for this round), by hand, because it needs a real browser.

**What is deliberately not here.** A panel-side path box or "go to…" field: the transcript and the
listing are the two doors, and a third place to type a path is a third place to be wrong. A way to
*create*, rename or delete anything from the listing: this panel reads, `POST /open` hands a path to the
machine, and writing a directory has no route because it is not a reading. Nor a page-side resolution of
a row against the directory that was listed — see the first residue above; it needs a `base` on the
route and a base threaded through the renderer, and the honest failure is cheap.

## 28. A directory is a place, not a reading — and the run says where a path ends — **built**

Reported directly, 2026-09-23, in one sentence with two halves:

> `C:\Users\zhangzhuo\My Documents` 这个依然识别不了，另外行为不对，点击以后应该是直接用电脑本身的
> 方式打开这个目录，而不是预览他的文件目录
> — *that path still cannot be recognised; and the behaviour is wrong -- pressing it should open the
> directory with the machine's own way of opening one, rather than previewing the files in it.*

§27 fixed a directory by making it **readable**. This round says the reading was the wrong answer to a
press, and that the *name* was the half still missing. Both are about the same confusion: a directory
is not a document.

**1. Pressing a directory opens it where it lives.** The panel's listing is what a *reader* asked for,
and a person pressing a directory in the transcript is not asking to read it — they are asking to go
there, which is what every other surface on a desktop does with a directory. So the press leaves the
page: `POST /open`, the same route the panel's own named control uses, with the same body and the same
refusals. `openPreview` is where this is decided, because it is the one door every press on a path goes
through — the transcript's buttons and the listing's rows alike — and a path whose *name* ends in a
separator is decided there without asking anything: `/file` is not even tried. A path that does not say
it is a directory is still asked of `/file` first, and the `X-Flint-Dir` header §27 added is what turns
that press into this one; the header's job did not change, only what is done with it.

Three consequences, all of them deliberate:

- **A successful open draws no panel at all.** The hint line says what happened and with what
  (`opened <path> with explorer`) and the desktop has the window. A panel that opened to say "opened
  it" would be a panel in the way of the thing that was just opened.
- **A refused open says why, in the route's words**, on the hint line — the same line the named control
  has always used. That is the whole of the failure for a directory that is gone or unreadable.
- **The one refusal that is not a failure is `readonly`.** A run whose guard refuses to start programs
  cannot open a directory, and the listing is then the only reading left: the panel shows the sentence
  and *then* the listing, so a person is told why and still gets to look. This is the case `/dir` is
  kept for, and it is why the route and its test did not become dead code when the press changed.
  (`409` is the route's own status for it, which is how the page tells "you may not" from "it is not
  there" without parsing prose.)

**2. The run says where a path ends.** `C:\Users\zhangzhuo\My Documents` is one name to a person, two
tokens to any scanner, and nothing in the *text* tells it apart from a path followed by a word. §18 wrote
that residue down as unfixable and §27 fixed it only for the run's own `list` output, where the line's
mark is evidence. What neither considered is that the page does not have to work it out **alone**: the
run has a filesystem, so the page asks.

`GET /resolve?text=…` takes the line *from the first character of the path* and answers with the longest
prefix of it that exists, and how many UTF-16 code units that was: `{"path": "C:\\…\\My Documents",
"used": 31}`. The page then draws the button over exactly those characters, so the words after the name
stay the words they are, and a `:N` inside the span becomes a line exactly as it does everywhere else
(the span goes back through `asPath`). Four decisions in that shape:

- **`used` is a length, not a path**, because the page's use for it is a `slice`. It counts **UTF-16 code
  units** — a JavaScript string index — which is asserted with a directory named in Chinese rather than
  assumed.
- **Longest prefix first, word by word, trailing punctuation trimmed at each step.** So
  `read <path>, and then` names the file and not the comma, and a sentence that follows a path is not
  swallowed by it. The ambiguity that is left is real and visible: a directory literally named `My Notes
  are here` would be found in preference to `My Notes`, and the *button's own text* is what shows which
  one won.
- **Only a token that looks absolute is asked about, and only when a word follows it.** A drive letter,
  a UNC share, a home-directory path, a rooted one — `pathStarts`. A relative name is not asked about at
  all: in prose `src/My Dir` is two words far more often than it is a directory, and that is the same
  reason the narrow scanner rule exists. A token at the end of a line is not asked about either: there is
  nothing after it to absorb, and a request per path in a conversation would be a request per path.
- **One request per text, one redraw per batch.** `pathEnds` caches every answer *including a refusal*
  (`null`), and `pathEndsAsked` is what stops a second request while the first is in flight; without
  both, a line would be asked about again on every paint, and a paint happens on every token of a
  streaming answer. When answers land, only the blocks whose own text was asked about are drawn again —
  `pathEndsCallers` is why — because `paint`'s per-block revision is what normally preserves a selection
  and an unfolded `details`, and a node replaced for a reason that is not its own would close both.

**Nothing is asked without a run behind the page.** A dropped session file or an exported conversation
has no process to ask, so the candidate keeps its own reading: the first fragment is a button to
something that may not exist, which is exactly what §18 said the honest failure was. The *page* is not
the only reader of a transcript — the terminal quotes what it prints (`display::quote_if_spaced`) — and
a quoted path is still one candidate and is never asked about, because the quotes are the writer's own
evidence.

**What was measured, and where.**

| Claim | Where it was measured | What came back |
|---|---|---|
| The run says where a path in a line ends | `src/web.rs::the_run_says_where_a_path_in_a_line_ends` | `C:\…\My Projects is where it goes` → the `My Projects` directory and `used` = that name's own UTF-16 length; a file with a comma after it resolves to the file and not the comma; a directory named `资料 夹` counts 4 units; a text with no path in it → 404 in a sentence; no `?text=` → 400. Watched red first: `404 no such route` |
| A directory press opens it, and asks nothing else | `scripts/web-view-test.js` | one press, one request, and it is `/open` with the path as written, space and separator included; **no** `/dir` request; a press on a file still goes to the panel. Watched red first: the harness recorded zero requests, which is how the sandbox's missing `location` was found (a request needs the token header, and every fetch path had been throwing inside its own `try`) |
| A spaced path becomes one button, with what the run found | `scripts/web-view-test.js`, calling `addressParts` with the answer seeded | one button over `C:\Users\me\My Documents`, the press asking for that whole name, every other word of the sentence still on the page, and `:12` at the end of a resolved name arriving as `line: 12`; a refusal, and an answer that used nothing, leave the token as it was |
| The same two rules in the page's own bytes | `tests/web_view.rs::a_path_cut_by_a_space_is_resolved_by_the_run_rather_than_guessed_at`, and the directory half of `a_path_opens_outside_the_page_only_through_the_route` | the ask is bounded (`pathStarts`, a word after it), the request is one per text and encoded, the answer replaces the token only when it is *longer*, the redraw touches only the blocks that asked, and only a `409` falls back to a listing |
| The whole thing in a browser, on a real directory | `scripts/browser-controls-test.js`, four claims | a **bare** absolute path with a space in its name — the model's own prose, no quotes — becomes one button reading the whole name, with the words after it still the sentence they were; the run's own listing draws that name as one row the way the process printed it; a press on a directory that is *gone* asks `POST /open` and the route's sentence arrives on the hint line; and the panel lists **nothing**, which a page that read the directory into the panel could not have produced. The press is made against a missing directory on purpose, exactly as the panel's own open control already is: a real one would put a file manager on the screen of whoever ran the harness |

**The residues, stated rather than hidden.**

- **A relative name with a space is still split**, in prose and in a tool result. The page asks only
  about the shapes prose does not make, and that is a deliberate limit rather than an oversight: the run
  would happily answer for `src/My Dir and then`, and the cost of asking is a request per line that
  merely *looks* like it holds a path.
- **A path in a tool call's arguments keeps its JSON spelling**, doubled separators and all, because
  that is what the transcript carries. Measured: the run resolves it to the same place anyway, and the
  resolution above is asked about the text *as written*, so `C:\\Users\\me\\My` is asked about as the
  text it is and answered with the real path.
- **A resolved name is what the run found, and a run can find a longer name than was meant.** The button
  shows the characters that won, so the choice is visible rather than silent; there is no route that
  returns two candidates, because a page with two answers would have to guess anyway.
- **The `/resolve` question is asked about a line, not about the reader's intent.** The text handed over
  is the line from the candidate's first character to its end (bounded at 400 units, cut on a whole
  character so a lone surrogate cannot make `encodeURIComponent` throw). A page that handed over a whole
  sentence would be asking this route to find where the path *starts*, which is the scanner's half.

**The gate, after this round.** `cargo test` **687 passed / 1 ignored** (the pty test, Unix only) across
its 14 suites — one new route test (`/resolve`), one new `web_view` policy test (a spaced path resolved
by the run rather than guessed at), and assertions added to two existing ones (the directory press in
`a_path_opens_outside_the_page_only_through_the_route`, the menu's tie-break in
`the_slash_menu_is_a_launcher_drawn_from_the_frame`); `cargo clippy --all-targets -- -D warnings`
silent; `node scripts/term-layout-test.js` and `node scripts/web-view-test.js` (two claims added and one
corrected, all green) — both run by CI; both Python doors green; and `scripts/browser-controls-test.js`
**118/118** in headless Chrome on Windows (four claims for the resolution and the press, and the two menu
claims corrected), by hand, because it needs a real browser.

**The mojibake guard caught this round's own test, which is worth recording.** The resolver's trailing
punctuation includes four full-width marks, and the CJK name the UTF-16 count is measured against was
written as a literal — both are non-ASCII in `src/web.rs`, and `the_source_tree_contains_no_mojibake`
refuses CJK in a file nobody declared to hold it, listing the very characters. `src/web.rs` is a file
where damage would be invisible, so the fix is the guard's own prescription rather than an entry in
`CJK_FILES`: the marks and the name are `\u{…}` escapes, the character is still the character, and the
test that proves the count in UTF-16 units still runs against a real directory with a real space in its
name. The guard then caught the *comment* explaining that, which had one of the marks quoted in it —
which is the guard working exactly as its comment says it should.

**What is deliberately not here.** A route that answers with several candidates, a page-side heuristic
for where a name ends, and a "go to this path" box: the first would move the guess back into the page,
the second is what this round deleted, and the third is §27's own refusal. Nor does the page *remember*
a resolution across a reload — `pathEnds` is a cache of answers for the transcript on screen, and a
reload asks again, which is one request per line and not derived state on disk.
