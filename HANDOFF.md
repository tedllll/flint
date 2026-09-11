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

Pushing from here did not work at the end of this session, but the network was the
reason, not the repository: `git fetch origin` at 17:xx failed with
`Failed to connect to github.com port 443 via 127.0.0.1`, while the same remote had
been fetched successfully at 15:36 the same afternoon. The company machine reaches
GitHub only through a local proxy (v2rayN), and that proxy was not listening at the
end of the session. `git config --local http.proxy` is
`socks5h://127.0.0.1:10808` — a *local* setting, deliberately, so it is not carried
anywhere else. On a machine without that proxy it turns every remote operation into a
connection error that reads like a network outage.

To push once the proxy is up:

```bash
git fetch origin
git log --oneline HEAD..origin/main    # empty means a plain fast-forward
git push origin main
```

### Two unrelated histories on the remote

Worth knowing before pushing. The local branch and `origin/main` share no common
commit, so there is nothing to fast-forward:

| | root | tip | author |
|---|---|---|---|
| local `main` | `2b51f31` 13:57 | `d9e2e6c` | zhangzhuo |
| `origin/main` | `59a3243` | `e03b34f` 14:23 | tedllll |

Every file that exists on `origin/main` also exists locally, and the local versions are
strictly more developed: `README.md` is 245 lines against 116, `Cargo.toml` is 50
against 46, and `.github/workflows/release.yml` (97 lines) and `.gitignore` are already
present locally. Merging the two would therefore conflict on all 18 files and resolve
to the local side anyway.

So the decision is a deliberate one, not a mechanical merge — either

```bash
git push --force-with-lease origin main     # keep the local history, discard the remote's
```

or start a branch from `origin/main` and cherry-pick. Neither should be done by
reflex; the remote history is someone's commits, whatever it looks like.

### Offline transfer

The bundle is the fallback that always works, and it carries both histories:

```bash
git bundle create flint.bundle --all refs/remotes/origin/main
git bundle verify flint.bundle
git fetch /path/to/flint.bundle 'refs/heads/*:refs/heads/*' refs/remotes/origin/main:refs/remotes/origin/main
```

The refspec in the fetch matters: without it a bundle only moves `HEAD`.

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
