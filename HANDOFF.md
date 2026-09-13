# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

Everything is committed, the working tree is clean, and `main` is pushed to
`origin/main` (tip `a25a461`). `cargo test` is 144 passing (94 lib, 20 `agent_loop`,
12 `cli_output`, 18 `term_capture`), `cargo clippy --all-targets` is silent, and
`node scripts/term-layout-test.js` passes every layout assertion.

Build with:

```bash
cargo build --release
```

The binary is held open by any running `flint`, so close those before replacing it.
Development happens on Windows now; `docs/windows.md` is the field notes for it.

## What was just done

Four commits, all pushed. The reasoning is in each commit message; this is the index.

- **Narration and the clock.** `last_segment_text` outlived the turn it described, so
  the next turn's first round recommitted the previous answer; the strip is now reset
  once per turn (`Term::begin_answer`). Committing a transcript line no longer repaints
  the status row, so a fast tool cannot flash `0s`, and the clock is owned by the round
  rather than by each announcement — which is what leaves it running, nameless, for the
  model call that follows a tool round.
- **Sessions.** A conversation can be named (`/name`, `--name`), filed away
  (`/archive`, `--archive`) or deleted (`/delete`, `--delete`), none of which needs a
  model or a key. Naming appends a `title` event, so the file stays append-only and the
  last name wins; archiving moves the file into `sessions/archive/`. The format gained a
  version (`"v": 2`), an unknown event type is now skipped in silence, and listing reads
  only the two ends of each file — 64 KiB of head, 16 KiB of tail — instead of all of it.
- **Project context.** `AGENTS.md` files are named in the prompt by default (`hint`;
  `paste` and `off` are configurable) and skills in `<dir>/<name>/SKILL.md` are offered
  as a one-line catalog with the body loaded on demand by the `skill` tool. Nothing is
  cached, so `/reload` picks up a file written mid-session.
- **The handoff itself.** This file had drifted: it claimed 106 tests, a clean tree at
  `8b0e788`, and a verification procedure whose counts no longer matched anything.

## Known unfinished

**Windows terminal behaviour is still unverified.** Read
[`docs/windows.md`](docs/windows.md) first — it has the mechanisms, labelled by what was
measured versus reasoned. The short version: the layout is built on terminal
*behaviour* (scroll regions, absolute row addressing, what a newline does at the bottom
margin), `crossterm` sets only `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and never
`DISABLE_NEWLINE_AUTO_RETURN` — the bit that decides whether `\n` also returns the
carriage, while `insert_history` counts on `\r\n` advancing exactly one row — and the
console output code page (`SetConsoleOutputCP`) is never set, so non-ASCII text very
likely prints as mojibake. The machine flint is developed on now is the machine this can
finally be measured on. One measurement, while everything else was being tested:

**Do not trust a test run after restoring a file by copy.** Windows `CopyFile` preserves
the source's modification time, so `cargo` can decide nothing changed and run the
*previous* binary. It cost an afternoon chasing three failures that were a stale
`flint.exe` (`unknown flag '--archive'`). Touch the restored files, or check the
`Compiling`/`Finished` line before believing a result.

**CI does not run on push** — `.github/workflows/release.yml` triggers only on `v*` tags
and `workflow_dispatch`, so a push to `main` checks nothing on any platform. The
cross-compile cannot even be type-checked here: `aws-lc-sys` (rustls's crypto backend)
needs a Windows C toolchain, not just `rustup target add`.

**Three `eprintln!` sites can still fire mid-turn**, which puts them inside the strip:
`agent.rs` (cannot persist session event), `config.rs`, `session.rs`. They are rare
enough that they have not been seen, but they are the same fault that was fixed for tool
notices — they should move to the notice sink.

**The transcript is not trimmed by construction.** The step guard is 100, and what keeps
a long turn from walking into the context ceiling is request-side pruning of stale tool
output (`prune_tool_output` in `agent.rs`): the four most recent results, anything short,
and every failure are kept; older long output is replaced with a note. The session file
keeps every byte.

**Streamed output arrives in one go**, because events are buffered per attempt to make
retries safe. The status line is what tells the user work is happening. If real
token-by-token output is wanted back, the buffer has to be flushed as it fills and the
retry path has to reconcile what was already drawn — that was considered and not done.

## Planned and not started

In the order agreed, with the design settled in discussion:

1. **Tool-loop quality.** `apply_patch` in Codex's format (atomic across several files);
   a read-before-mutate gate (in-process `HashMap<PathBuf, (mtime, len)>`, DSH's wording
   for the error); honest truncation that spills the full output to
   `~/.flint/spill/<session>/<n>.txt` and says where it went; argument validation that
   reports a wrong type instead of ignoring it; and a non-blocking reminder when the same
   call is repeated with identical arguments (thresholds 3/5/8).
2. **The transcript as cells.** Three steps — measure, then paint only what changed, then
   a real re-render on resize. This is also what deletes the interim state the last fix
   left behind: `begin_answer`, `last_segment_text`, the `committed` count and
   `fresh_segment` all exist because the strip is a text offset rather than a model.
3. **Machine-readable runs.** `flint -p --json` emitting NDJSON (`session.started`,
   `turn.started`, `tool.started`, `tool.completed`, `message.completed`, `usage`,
   `turn.completed`, `turn.failed`, `error`), `docs/session-format.md`, and a
   `debug prompt-input` view of exactly what was sent.
4. **Small, agreed, unscheduled.** `read`/`write`/`edit` taking `file_path` with `path`
   kept as an alias; `--fork`; the `HANDOFF`'s own warning about
   `examples/live_turn.rs` — it keeps a hand-maintained copy of `run_turn`'s event
   handling and has drifted twice, costing time chasing faults that were only in the
   example.

Deliberately not doing: subagents, `flint doctor`, approval prompts, MCP, sandboxes,
SQLite, indexes, and any compressed or opaque state. State stays plain files a person can
repair with `notepad`, and nothing derived is ever persisted.

## Verification without a terminal

```bash
cargo test                            # 144 tests, including the real byte stream
node scripts/term-layout-test.js      # replay the layout through a screen model
node scripts/vtscreen.js raw.bin 24 100          # one raw dump, as a screen
node scripts/vtscreen.js raw.bin 24 100 --prefill OLD   # ...onto a dirty screen
```

`FLINT_TERM_CAPTURE=1` and `FLINT_TERM_SIZE=100x24` (debug builds only) force the
interactive branch with a pinned size, and a test can now drive the REPL's own commands
by piping stdin. That is how `/name`, `/delete` and `/skills` are covered end to end.
`FLINT_TERM_CAPTURE_FILE=<path>` sends the bytes to that file instead of stdout, and a
test must use it rather than redirecting stdout: a redirected fd 1 also collects the test
harness's own progress lines, and one of those landing on the bottom row scrolls the
transcript out of the recorded screen — a blank-screen failure with nothing wrong behind
it, at about one full-suite run in four before the file variable existed.

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
in a Windows path and then reports an identity file that does not exist.
