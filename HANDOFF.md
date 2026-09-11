# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

Everything is committed, the working tree is clean, and `main` is pushed to
`origin/main` (tip `8b0e788`). `cargo test` is 106 passing, `cargo clippy
--all-targets` is silent, and `node scripts/term-layout-test.js` passes every layout
assertion.

Build with:

```bash
cargo build --release
```

The binary is held open by any running `flint`, so close those before replacing it.
On macOS the release binary was also installed behind a launcher at
`~/.local/bin/flint`, which execs `~/flint/target/release/flint`; the API key is read
from `$DEEPSEEK_API_KEY`, loaded from a 600-mode `~/.flint/api_key`.

## What was just fixed

Six commits, all pushed. The full reasoning is in each commit message; this is the
index.

- **Terminal layout.** Transcript is top-aligned and grows downward; the running clock
  has its own row and names the phase (waiting / thinking / writing / tool / no
  response); resizing no longer wipes the conversation; startup clears every row and
  requests a full repaint.
- **Provider and model state.** `Flow::NewAgent` carries the new configuration, not
  only the new agent, so `/model` and `/provider` stop reporting the previous provider.
  `/model` switches and lists; a provider can list several models.
- **Provider retry.** A dropped stream is retried with bounded backoff; events are
  buffered per attempt so a retry cannot duplicate text.
- **Search tools.** `glob` and `grep` are built in, so "where is this file" works in
  `cmd.exe`, where `grep` does not exist and `findstr` is not recursive.
- **Prompt.** It no longer names a job ("diagnose and repair"), and it now says where
  flint keeps its own configuration, so "change your model" edits the config instead of
  the source.
- **Mojibake.** Fifteen user-visible strings had shipped as `readonly 鈥?writes ...`;
  a test now walks the tree for it. Also `exec` no longer creates a config file or asks
  for an API key it never uses.

## Known unfinished

**Windows is unverified.** Every fix above was developed and tested on macOS. The
layout depends on DECSTBM scroll regions, which `conhost` and Windows Terminal do not
handle identically, and the test suite cannot tell them apart — `scripts/vtscreen.js`
is one model of one terminal. The cross-compile cannot even be type-checked here:
`aws-lc-sys` (rustls's crypto backend) needs a Windows C toolchain. **CI does not run on
push** — `.github/workflows/release.yml` triggers only on `v*` tags and
`workflow_dispatch`, so a push to `main` checks nothing. Running that workflow by hand,
or tagging a release, is the only way to exercise Windows.

**Three `eprintln!` sites can still fire mid-turn**, which puts them inside the strip:
`agent.rs` (cannot persist session event), `config.rs`, `session.rs`. They are rare
enough that they have not been seen, but they are the same fault that was fixed for tool
notices — they should move to the notice sink.

**The transcript is not trimmed by construction.** The step guard is 100, and what
keeps a long turn from walking into the context ceiling is request-side pruning of
stale tool output (see `prune_tool_output` in `agent.rs`): the four most recent results,
anything short, and every failure are kept; older long output is replaced with a note.
The session file keeps every byte.

**Streamed output arrives in one go**, because events are buffered per attempt to make
retries safe. The status line is what tells the user work is happening. If real
token-by-token output is wanted back, the buffer has to be flushed as it fills and the
retry path has to reconcile what was already drawn — that was considered and not done.

## Verification without a terminal

```bash
cargo test                            # 106 tests, including the real byte stream
node scripts/term-layout-test.js      # replay the layout through a screen model
node scripts/vtscreen.js raw.bin 24 100          # one raw dump, as a screen
node scripts/vtscreen.js raw.bin 24 100 --prefill OLD   # ...onto a dirty screen
```

`FLINT_TERM_CAPTURE=1` and `FLINT_TERM_SIZE=100x24` (debug builds only) force the
interactive branch with a pinned size. Since the last session the startup path runs
under capture too, so `setup` — scrolling the old screen away and clearing it — is
reachable from a test; that is where "the previous screen is still visible" lives.

For a real terminal, the useful trick is to read the text buffer rather than look at
pixels, which settles what is on screen character by character:

```bash
osascript -e 'tell application "Terminal" to get contents of selected tab of item 1 of windows'
```

`examples/live_turn.rs` drives a real turn against the configured provider. It has its
own copy of the event handling and does **not** go through `run_turn`, so it must be
kept in step by hand — twice now it has reported behaviour the REPL no longer had, and
cost time chasing a fault that was only in the example.

## Two histories on the remote — resolved

An earlier session found the local branch and `origin/main` sharing no common commit.
That is no longer true: `main` fast-forwards from `270add6`, which is what the remote
already had. Nothing to reconcile.

Pushing worked over SSH without a proxy. `HANDOFF` used to say the company machine
needed `socks5h://127.0.0.1:10808` for remote operations; that was a *local* git
setting for HTTPS and is not in the repository, and the remote here is `git@github.com:`
in any case.
