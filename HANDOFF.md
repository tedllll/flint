# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

Everything is committed and the working tree is clean. `cargo test` is 79 passing
(53 lib, 3 + 14 + 5 + 4 integration), `cargo clippy --all-targets` is silent, and
`node scripts/term-layout-test.js` passes every layout assertion.

The binary is installed at `%USERPROFILE%\bin\flint.exe`. Build one with:

```bash
cargo build --release
```

It is held open by any running `flint`, so close those before replacing it.

## Known unfinished

**The strip merges two segments into one row.** When the model writes an opening line,
calls tools, and then continues *in the same streamed segment* (it re-sends the
cumulative text rather than new text), the opening line and the continuation render on
the same screen row:

```
I'll read both files.`src/lib.rs`: library root declaring nine modules...
```

The text is correct and appears exactly once — the row is just missing a break. Fixing
it means inserting a newline after the already-committed prefix when a segment's text
begins with text that has already been committed. That is invasive: a newline changes
where every following row wraps, so the commit accounting has to move with it. Not
attempted, deliberately.

**Three `eprintln!` sites can still fire mid-turn**, which puts them inside the strip:
`agent.rs` (cannot persist session event), `config.rs`, `session.rs`. They are rare
enough that they have not been seen, but they are the same fault that was just fixed
for tool notices — they should move to the notice sink.

**The transcript is not trimmed by construction.** The step guard is now 100, and what
keeps a long turn from walking into the context ceiling is request-side pruning of
stale tool output (see `prune_tool_output` in `agent.rs`): the four most recent results,
anything short, and every failure are kept; older long output is replaced with a note.
The session file keeps every byte. If a real turn still hits a context wall, that
policy is the thing to change — not the step count.

## Verification without a terminal

There is no TTY here, so interactive behaviour is checked by replaying bytes through a
screen model rather than by watching a terminal:

```bash
cargo test --test term_capture        # capture the real interactive byte stream
node scripts/term-layout-test.js      # replay it, plus hand-written scenarios
node scripts/vtscreen.js raw.bin 24 100   # one raw dump, as a screen
cargo test --test cli_output          # the real binary, in a pipe
```

`FLINT_TERM_CAPTURE=1` and `FLINT_TERM_SIZE=100x24` (debug builds only) force the
interactive branch with a pinned size. `examples/live_turn.rs` drives a real turn
against the configured provider and prints what the terminal received — that is how
the strip faults were found, and how to find the next one.

## Moving between machines

`git` here cannot reach GitHub: the company network needs a proxy and the local v2rayN
is not currently listening, so pushes and fetches fail. The repository moves by bundle
instead.

```bash
git bundle create flint.bundle --all     # on the machine that has the work
git bundle verify flint.bundle           # on the machine receiving it
git fetch /path/to/flint.bundle 'refs/heads/*:refs/remotes/bundle/*'
git merge bundle/main
```

`git config --local http.proxy` is `socks5h://127.0.0.1:10808` here. That setting is
per-repository and must not be copied anywhere else; on a machine without the proxy it
makes every remote operation fail with a connection error that looks like a network
outage rather than a wrong setting.

## Design constraints worth not re-litigating

- **The point is rescue, not comfort.** The tool exists so that a broken `dsh` or
  `codex` can still be repaired. History reading, the file tools, and `exec` must keep
  working when the provider is unreachable — that ordering is why the provider error is
  deferred rather than fatal at startup.
- **Direct by default.** No proxy is used unless `proxy` is set in the config.
  Inheriting the Windows system proxy is invisible and was the cause of a real outage.
- **A single Ctrl-C stops the turn; it never exits.** Two inside 1.5s, `Ctrl-D`, or
  `/exit` quit. Losing the session to a reflex is the expensive mistake.
- **Full permissions by default.** `/readonly` is the only guard, and it is explicit.
- **Piped output emits zero escape bytes.** `tests/cli_output.rs` counts ESC bytes in
  the real binary's output to keep that true.
