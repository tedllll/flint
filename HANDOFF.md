# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

Everything is committed, the working tree is clean, and `main` is pushed to `origin/main`.
As of the commit that carries this file, `cargo test` is 326 passing (237 lib, 28
`agent_loop`, 28 `cli_output`, 4 `json_output`, 4 `search_tool`, 18 `term_capture`, 7
`web_view`), `cargo clippy --all-targets` is silent, and both `node
scripts/term-layout-test.js` and `node scripts/web-view-test.js` pass.

**This round was on macOS** (Darwin arm64, rustc 1.98.1). That is worth knowing before
anything below: the rounds before it were on Windows, and every Windows-specific item still
outstanding is one that cannot be measured from here. `docs/windows-tooling.md` labels each
by what it needs.

Build with:

```bash
cargo build --release
```

The binary is held open by any running `flint`, so close those before replacing it.
`docs/windows.md` is the field notes for the Windows terminal.

## What was just done

**L3: the browser page is a composer, with a sidebar of conversations.** This is the last of
`docs/web-mode.md` §9, and it was built against a real browser rather than reasoned about —
which is what turned up three defects that no amount of reading the source would have shown.
All three are recorded in that document's §11; the two that mattered:

- **A sidebar click during a turn did nothing.** `/resume 3` went up the message route, joined
  the channel the keyboard feeds, and `run_turn` — which picks a mid-turn line up to make it the
  next *prompt* and has no way to run anything — handed it to the model. The conversation did
  not switch. The moment you want another conversation is while one is churning, so this was the
  ordinary path, not a corner. A line that is a command is now handed back to the REPL
  (`for_the_repl`, and `run_turn` returns `Option<String>`), because the REPL is the only place
  that knows what a typed line means.
- **The status bar sat on top of the composer.** `position: fixed` put it over the input and the
  hint, so the bottom two lines of the page were unreadable exactly while a turn was running —
  which is when the status line is showing. It is the second grid row now.

**The shape of it.** `POST /message` carries *text* and does not interpret it: the page has no
idea what `/resume` is, so a slash command from the sidebar works without the browser being a
second place that decides what a typed line means. `GET /sessions` goes through `session::list`,
the same function `resolve_session` uses, so the numbers in the sidebar *are* the numbers
`/resume` accepts. One `Live::line` push of `turn.started` at the top of `run_turn` is what makes
a question visible to a watching browser wherever it was typed — before that, a message typed in
the *terminal* never appeared in the page until a reload.

**And the end of input had to become a fact rather than an inference.** `run_turn` deliberately
*consumes and drops* a `Quit` that arrives mid-turn, because a pipe closing is not a person
asking to stop. That was invisible while the sender lived and died with the reader thread — the
channel closed by itself and the REPL left on `Disconnected`. The browser is a second producer on
that channel, which keeps it open for the life of the process, and a dropped `Quit` then meant
`printf 'a\n/exit\n' | flint` against a dead endpoint printed its error and **waited forever**.
`InputReader::ended` records the fact; `next_line` consults it, and only when the channel is
empty. Regression test: `the_end_of_input_ends_the_repl_even_when_a_turn_consumed_the_quit`,
which fails on a 30-second deadline when the check is removed.


**`--web` now opens the page — that was the actual complaint.** The report was "I sent `--web`
and no page opened", and the first attempt at it answered a different question: it made the flag
*refused* with a note saying to type `/web`. True as far as it went, and not what was asked for.
Someone who has already said what they want should not be told to say it again in another
spelling, and what they wanted was a browser window.

So two changes, and they are both about the same thing:

- **A bare flint flag typed at the prompt is *translated*, not refused.** `--web` becomes
  `/web`, `--provider x` becomes `/provider x`, `--help` becomes `/help`. Only an exact flag:
  `why does --web need a token?` is a question and reaches the model untouched, and so does a
  pasted bullet list, which is why a rule over anything starting with `-` would have been worse
  than the bug. A flag with no equivalent inside a running process (`--json`, `--cwd`,
  `--no-color`, `-p`) says which one it is.
- **The view opens the browser.** `open` on macOS, `cmd /C start` on Windows, `xdg-open`
  elsewhere; detached, best-effort, and the URL is printed either way so a failed launch costs
  a paste and nothing else. **Only when stdout is a terminal** — a pipe is not a person, and
  without that check a test run would open windows on whoever ran it. The assertion that keeps
  that honest is that a redirected run must not print `(opening it)`.

Verified in a pty with `open` on `PATH` replaced by a script that records its argument — which
measures the whole path, decision, program and argument, without a browser appearing on this
machine. It received the URL from `--web` typed at the prompt *and* from `flint --web` at
startup. The terminal showed
`--web is a start-up flag — doing /web instead (that flag opened it at start-up)` followed by
`web: http://127.0.0.1:59331/?token=… (opening it)`.


**`/web` — the browser view is now reachable from inside a conversation.** This came from a bug
report, and the report was exact: *"I typed `--web` inside flint and the model answered it as a
sentence."* Of course it did. `--web` is a command-line flag, and it is the only name for the
feature anyone has met — it is in `--help`, in the README and in flint's own error messages —
so there was nothing to mark it as belonging to the command line rather than to the
conversation. The run looked like it had worked.

Two changes, and the second one is the feature:

- **`/web [port]` opens the listener now.** `Viewer` replaces the bare `Window` handle the CLI
  used to hold: it is *asked* for (a feed, no socket), then *opened* (binds, returns the URL).
  The split matters because a turn that starts while the socket is still being bound must not
  lose the frames it produces. `/web` twice reports where the view already is rather than
  binding a second listener — two would be a quiet failure, since the second bind succeeds, a
  second URL prints, and the page already open is on neither.
- **A bare flint flag typed at the prompt is refused, not sent.** Only the *exact* flag:
  `why does --web need a token?` is a question and reaches the model untouched, and so does a
  pasted bullet list. A rule over anything starting with `-` was the obvious version and is
  wrong, because a paste starts that way. There is no escape hatch, because a sentence is one.

**And an open window follows the run.** `/new`, `/resume` and `/reload` move the session the
page is showing, which needed `State.session` to become a shared handle rather than a path
copied when the socket was bound, and a new named SSE frame — `event: reset`, which the page
already knew how to handle — because a *connected* client is current and so no cursor
difference can reach it. The ring is emptied at the same time: those frames belong to the
conversation being left, and replaying them on top of the new file would splice two
conversations together.

**Verified in a pty against the release binary**, because a pipe cannot carry `/web`
(`from_stdin` reads lines and never reaches the event reader): `/web` printed a live URL,
`GET /session` served the conversation, a `/events` listener attached across a `/new` received
`event: reset` and then the *new* file at the same URL, and `/web` twice printed one URL twice.
The pty also found a trap worth knowing — **in raw mode `\n` is Ctrl-J, not Enter**, so a
script driving flint must send `\r`; the first attempt read `> /webj` and looked like a flint
bug.


**A pasted block is one message again, and keeps its line breaks.** Reported from a real
session as "it treats it as one sentence", and the report was accurate. Two faults were
stacked, and each one on its own would have produced the complaint:

**Bracketed paste was never enabled.** A terminal only wraps a paste in `\x1b[200~ … \x1b[201~`
when the program asks, and nothing ever asked — so the paste arrived as one keystroke per
character and every newline in it was an Enter. A three-line prompt became three messages, and
the second and third arrived while the first turn was still running, which is the *steering*
path: they interrupted it. The model was given the first line and interrupted twice.

**And the handler for it deleted the line breaks.** `Event::Paste` had been in `term.rs` all
along, with a test asserting that newlines are removed "or the line breaks the input row" — a
test of a branch that could not be reached. Had the enable been there, a pasted list or stack
trace would have reached the model as one run-on line: the same complaint by another route.

So the enable is there now, and a paste containing a line break is submitted as **one message
with its breaks intact**. A paste with no line break still joins the line being typed, which is
what a pasted path or snippet is for. The input row is one row, so a block is submitted rather
than parked in it: showing a paragraph there would be showing something other than what Enter
sends. The REPL echoes one transcript line per line of the message, because a single write
carrying a newline would move the cursor down through the rows the layout reserved.

**Verified in a real pty, because nothing else can.** `from_stdin` reads lines and never
touches the event reader, so bracketed paste is a terminal-only path: no pipe reaches it and
`cargo test` cannot cover it. What was checked by hand, under a pty with a scratch
`FLINT_HOME`: the enable sequence goes out, and a paste of three lines comes back as three
echoed lines under one prompt rather than as three messages. The unit tests cover the two
shapes the handler must produce — a block with breaks, a fragment that joins the line — but the
*enable* itself is only checked there.

**Text now arrives while it is being written.** Deltas used to be collected per attempt and
handed over only when that attempt completed, so that a retry could discard them — which meant
a terminal, a browser and a `--json` reader all saw *nothing* until a whole response had
arrived. Measured before the change: 128 `message.delta` frames arriving as a single burst;
after it, the same 128 spread over 5.5 seconds, the first at 1.9s.

This mattered most for a local model, and the numbers say why: on a real question, Ornith-1.5
spent 341 completion tokens of which 292 characters were the answer, and gemma4-12b spent 436
tokens of which 49 characters were — **most of a turn is reasoning, and reasoning was
invisible**, shown only as one word on the status line. So a twenty-second turn looked like a
twenty-second freeze followed by a short answer, and the model was blamed for it. Measured the
other way round: MLX was *faster* than the 12B gemma, 19.2s against 33.7s.

**The retry rule is now explicit: the ladder stops at the first drawn character.** Before it —
a connection that never opened, a 503, a stream that died during the reasoning — is still
retried and still leaves no trace. After it, the failure is reported, because a second attempt
would be written after the first: a terminal has scrolled the line, a pipe has emitted it, the
browser has rendered it, and nothing in the chain can take text back.

**And a defect fell out of the change.** A stream that ended without the provider's completion
signal was accepted as an answer: `parser.done` was only ever used to leave the read loop
early, never asked afterwards. A dropped connection usually shows up as a body simply ending,
so a half answer became *the* answer and the turn read as a success. It is a transport failure
now.

**Web search: `search`, backed by DeepSeek.** The point of it is that search is a *tool* and
not a capability of whichever model is driving, so a configuration running only a local model
can search, with nothing deployed on any machine that has a DeepSeek key. The credential is
**inherited** from a provider pointed at DeepSeek when there is one — resolved exactly the way
that provider resolves it — and named explicitly in a `[search]` block when there is not. The
tool is offered only when it can actually work, and the reason is said once at startup when it
cannot.

Four things came out of measuring rather than reading, and
[`docs/deepseek-search.md`](docs/deepseek-search.md) is the record:

- **The endpoint is not the provider's.** Search is on DeepSeek's *Anthropic-compatible*
  surface; the chat-completions one ignores `web_search` and answers without searching,
  saying nothing about it. A tool that reused `base_url` would look like it worked.
- **There is no per-search fee.** The retrieved pages are billed as *input tokens* on the
  model turn doing the searching — 16,561 for one search, 119,581 for a call that made two.
  Roughly ¥0.02–0.13 a call, halved outside peak hours. The number is in the README because
  a model that treats `search` as cheap will spend real money.
- **Snippets do not exist.** DSH builds them from `citations[]`, and DeepSeek returns none:
  its compatibility table lists `citations` as *Ignored*, and the citations are markdown links
  in the prose. So the answer is the url list plus DeepSeek's own summary.
- **The summary can be wrong.** In the first live run through the finished tool it claimed
  Rust 1.97.1; the model cross-checked against `rustc --version` and `endoflife.date`, found
  1.98.1, and said which source was stale. That is the label doing its job, and the argument
  for returning the summary *with* its sources.

**Web mode, levels 1 and 2.** `flint --web` serves a browser view of the running process on
loopback. Four commits, and `docs/web-mode.md` §9 and §11 are the reference: the static
viewer (`web/view.html`, one file, no build, with a policy test over the embedded bytes and a
Node harness that runs the renderer's pure half); the hand-rolled listener with the four
constraints of §4 and `GET /`; `GET /session`; and `GET /events` as SSE with a cursor, replay
and the `status` event. The §7 decision — hand-rolled, not `hyper` — is in `decisions.md`.

Three things that came out of *measuring* rather than reasoning, all recorded in §11:

- The viewer painted the system prompt expanded and pushed the conversation off the first
  screen. One screenshot in headless Chrome found it.
- The `status` event first went out *after* the text it announced, and carried the empty
  activity name instead of the words on the row — which blanked the browser's status line at
  the exact moment the wait began. A live capture found both.
- A page opened in the middle of a turn has missed every status frame, and no cursor can
  bring them back. The current status is now sent once on connect, as state rather than as a
  change.

**The limit that measuring found.** Streamed output is buffered per attempt, so the browser
shows no text until a model response has finished — the same known limitation the terminal
has, made obvious by a renderer that is not where the person already is. Level 2 is therefore
*live about the phase and late about the text*, and making the text arrive as it is written is
now the first thing worth doing next. `docs/web-mode.md` §11 says so at length.

**`docs/deepseek-search.md`.** Two real requests, written down, because the documentation
misled this session twice: DeepSeek searches on its **Anthropic** surface and not the
chat-completions one, with the key already configured, so a local model can search with no
second service. Also what it costs (16,561 input tokens for one search; 119,581 for a call
that made two), that snippets do not exist, and that `max_uses: 1` did not stop a second
query.

### The round before

Six commits, all pushed. The reasoning is in each commit message; this is the index.

- **`exec`.** A program and its arguments as an array, with no shell anywhere between. This
  is Windows plan step 2 and the largest single item in the queue: a command *line* is
  re-read by every layer between the model and the program, and each layer's escaping is
  right for itself and wrong for the next, whereas an array has no second reader. Registered
  on every platform rather than Windows only, with `stdin` for payloads that are not
  arguments, and `readonly` judged from the program and its verb rather than from a string
  that had to be re-split — which is why `is_readonly_command` is now two functions.
- **The runner extraction before it.** `run_program_streaming` is the implementation and
  `BashTool` is its special case, so the timeout, the idle kill, the progress reporting, the
  output cap and the kill semantics have one owner instead of two. Pure refactor, 190 tests
  unchanged.
- **`debug prompt-input`.** Prints the request body that would be sent — system prompt,
  history, tool schemas — and sends nothing. This finishes the machine-readable-runs stream:
  it answers the question the session file cannot, because request-side pruning means the
  request is *not* the transcript. The body comes from `provider::request_body`, extracted
  from `stream_chat` and now the only place the request is serialised, and a test runs a turn
  against the stub provider and requires the preview to equal the bytes the server received.
- **`glob`/`grep` and a backslash pattern.** `walk_files` builds `/`-separated paths, so
  `src\*.rs` matched nothing and the answer read as "there is no such directory". Fixed at
  the tool's door, and the plan document's own instruction — "normalise `\` in the `pattern`
  and `path` arguments" — turned out to be wrong in two of its three places, which is now
  written down there.
- **`/resume` was dropping the system prompt entirely.** Found while extracting the
  loaded-history splice for `debug`: `/resume` replaced the whole history with a session file
  that has no system message in it, so the model was sent no instructions at all after a
  resume. The roles actually sent were `["user"]`. Two tests, both reading the body the stub
  provider received, because that is the only place it is visible.
- **A `[timeout:N]` marker that nothing parses.** Both timeout messages told the model to
  write it. Removed: the cost was not the missing feature but what the model learns from
  being told something untrue about the tool.

## Known unfinished

**A terminal that goes away takes a core with it.** When flint's pty is closed without a
`SIGHUP` -- a terminal emulator that crashes, a `close(master)` from the other end -- the process
spins at 100% of a core forever and never exits. The spin is inside `crossterm`, not here:
`crossterm::event::read()` is `try_read(None)`, whose loop condition
(`timeout.leftover().map_or(true, |t| !t.is_zero())`) is always true, and the fd reports `POLLHUP`
without `POLLIN`, which none of its three branches handles -- so `poll` returns immediately,
nothing is consumed, and it polls again. `src/event/source/unix/tty.rs` in crossterm 0.29 is the
place to read. Measured on both this build and the one before it (100.3% and 100.7%), so it
predates the browser work. A fix means driving the loop from `event::poll(timeout)` and checking
whether the terminal is still there, which is a change to the key thread rather than to a flag.

**The Windows half of the tooling plan is still unwritten** — `pwsh` (step 3), the
process-tree kill and the child output encoding (step 4), and the PowerShell facts in the
system prompt (step 5). None of them can be written from reasoning alone: §4.2's
execution-policy wrinkle, §6.6's reserved names and long paths, and `taskkill`'s behaviour
all want a Windows session, and a guessed implementation would be worse than an absent one.
[`docs/windows-tooling.md`](docs/windows-tooling.md) §7 marks each. The two Windows tests
that would matter most — that an argument survives the round trip, and that a killed command
takes its children with it — are also unwritten for the same reason.

**Windows terminal behaviour is still unverified.** Read
[`docs/windows.md`](docs/windows.md) first — it has the mechanisms, labelled by what was
measured versus reasoned. The short version: the layout is built on terminal *behaviour*
(scroll regions, absolute row addressing, what a newline does at the bottom margin),
`crossterm` sets only `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and never
`DISABLE_NEWLINE_AUTO_RETURN` — the bit that decides whether `\n` also returns the carriage,
while `insert_history` counts on `\r\n` advancing exactly one row — and the console output
code page (`SetConsoleOutputCP`) is never set, so non-ASCII text very likely prints as
mojibake.

**Do not trust a test run after restoring a file by copy.** Windows `CopyFile` preserves
the source's modification time, so `cargo` can decide nothing changed and run the
*previous* binary. It cost an afternoon chasing three failures that were a stale
`flint.exe` (`unknown flag '--archive'`). Touch the restored files, or check the
`Compiling`/`Finished` line before believing a result.

**CI does not run on push** — `.github/workflows/release.yml` triggers only on `v*` tags
and `workflow_dispatch`, so a push to `main` checks nothing on any platform. The
cross-compile cannot even be type-checked from a Mac: `aws-lc-sys` (rustls's crypto backend)
needs a Windows C toolchain, not just `rustup target add`.

**Three `eprintln!` sites can still fire mid-turn**, which puts them inside the strip:
`agent.rs` (cannot persist session event), `config.rs`, `session.rs`. They are rare
enough that they have not been seen, but they are the same fault that was fixed for tool
notices — they should move to the notice sink.

**The transcript is not trimmed by construction.** The step guard is 100, and what keeps
a long turn from walking into the context ceiling is request-side pruning of stale tool
output (`prune_tool_output` in `agent.rs`): the four most recent results, anything short,
and every failure are kept; older long output is replaced with a note. The session file
keeps every byte. `flint debug prompt-input` is how to see the difference.

**A stream that ends without a completion signal used to be taken for an answer.** Found
while making the text stream: `parser.done` was only ever used to leave the read loop early,
and nothing asked about it afterwards — so a connection that dropped mid-answer, which shows
up as a body simply ending, produced a half answer that read as a complete one. It is a
transport failure now, which is also what puts a mid-answer drop back on the retry ladder
when nothing has been drawn.


## Planned and not started

**[`ROADMAP.md`](ROADMAP.md) is the plan of record** — the ordered queue, what is done, what
is next, the small agreed items and the not-doing list. It was written by folding this
section into it, so the two do not drift. The shape of it now:

1. **The Windows command line**, settled in full in
   [`docs/windows-tooling.md`](docs/windows-tooling.md) and now **half built**: the runner
   extraction, `exec` and the `glob`/`grep` separator fix are in the tree. What is left is
   the part that needs the machine.
2. **The transcript as cells** — measure the transcript as cells, paint only what changed,
   then re-render for real on resize — which is also what deletes the interim state the clock
   fix left behind: `begin_answer`, `last_segment_text`, the `committed` count and
   `fresh_segment`.
3. **Web mode** — `--web` as a window onto the running process rather than a mode. The first
   three steps need no decision, and the one open question (§7 of that document: a hand-rolled
   HTTP server or `hyper`, which is already in the tree via `reqwest`) blocks only step 4.

Both of the last two are platform-independent and can be done on either machine.

## Verification without a terminal

```bash
cargo test                            # the whole suite, including the real byte stream
node scripts/term-layout-test.js      # replay the layout through a screen model
node scripts/vtscreen.js raw.bin 24 100          # one raw dump, as a screen
node scripts/vtscreen.js raw.bin 24 100 --prefill OLD   # ...onto a dirty screen
flint debug prompt-input              # what the model is actually given, sending nothing
```

`FLINT_TERM_CAPTURE=1` and `FLINT_TERM_SIZE=100x24` (debug builds only) force the
interactive branch with a pinned size, and a test can now drive the REPL's own commands
by piping stdin. That is how `/name`, `/delete`, `/skills` and `/resume` are covered end
to end.
`FLINT_TERM_CAPTURE_FILE=<path>` sends the bytes to that file instead of stdout, and a
test must use it rather than redirecting stdout: a redirected fd 1 also collects the test
harness's own progress lines, and one of those landing on the bottom row scrolls the
transcript out of the recorded screen — a blank-screen failure with nothing wrong behind
it, at about one full-suite run in four before the file variable existed.

Two traps for a new REPL test, both paid for this round:

- `test_home(tag, ...)` keys the scratch directory on the process id, so two tests in the
  same binary that pass the same tag share a directory and delete each other's fixture
  mid-run. The failure reads as "no request was made".
- The REPL opens its own session on startup, and that one is the newest. `/resume 1`
  therefore resumes the empty file the run just created — use an id.

`examples/live_turn.rs` drives a real turn against the configured provider. It has its
own copy of the event handling and does **not** go through `run_turn`, so it must be kept
in step by hand.

## Pushing from this machine

`origin` fetches over HTTPS and pushes over SSH
(`git@github.com:tedllll/flint.git`). The key is a deploy key for this repository only,
kept at `~/.ssh/flint_github` and named in this repository's `core.sshCommand`:

```bash
git config core.sshCommand   # ssh -i C:/Users/<you>/.ssh/flint_github -o IdentitiesOnly=yes ...
```

Note the forward slashes: `core.sshCommand` is parsed by git, which eats the backslashes
in a Windows path and then reports an identity file that does not exist. The Mac checkout
has no `core.sshCommand` set at all, so a push there uses the default key.
