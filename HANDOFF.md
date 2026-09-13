# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

Everything is committed, the working tree is clean, and `main` is pushed to `origin/main`.
As of the commit that carries this file, `cargo test` is 298 passing (220 lib, 28
`agent_loop`, 19 `cli_output`, 4 `json_output`, 18 `term_capture`, 5 `web_view`, 4
`search_tool`), `cargo clippy --all-targets` is silent, and both `node
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
