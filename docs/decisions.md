# Decisions

Why flint is the way it is: the choices that had a real alternative, and the reason the
alternative lost. Code says what it does; commits say what changed; this file is for the
reasoning that neither can carry — the ones worth not re-litigating in six months.

Anything here that later turns out to be wrong should be corrected here first, then in the
code. A decision that is no longer true is worse than no record at all.

## State, and the files it lives in

**Nothing is derived.** No index, no cache, no database. The session list, the resume
target, the skill catalog and the transcript are all computed from real files every time
they are needed. If flint is killed, or the power goes out, the files on disk are still the
whole truth. The cost is speed, paid only where a directory listing is already fast enough.

**Sessions are append-only JSONL.** Nothing is ever rewritten — not to rename a
conversation, not to add a token count. A rename is one appended `title` event, and the last
one in the file wins. This is what makes a session repairable with a text editor, and it is
why a crash mid-write loses at most one line. Reference:
[`session-format.md`](session-format.md).

**The format version is a defaulted field.** A file that does not mention `v` is v1, which
is every file written before conversations could be named. Requiring the field would have
turned every existing file into an unreadable one, and hand-written files are a supported
input.

**An unknown event type is skipped in silence; a known one that will not parse is
damage.** The difference is the whole point: "another version, or you, wrote this" is not a
reason to refuse a conversation, while a `chat` line that will not parse means the
conversation has a hole in it. Reporting the first as corruption would make every future
version a corrupt file for this one.

**Listing reads only the two ends of each file** — 64 KiB of head, 16 KiB of tail. A list
needs a name, a date and a size; reading a hundred megabytes to get them would make the
listing of a long conversation cost seconds, which is exactly when a listing is wanted.

**Timestamps are `epoch:<seconds>`.** Not a date format, and the function that produces it
was called `now_iso8601` until writing the format document found the lie. A date library is
a dependency; ordering sessions and showing a person roughly when one was written is not
worth one.

**`/archive` is a `mv`.** The session moves into `sessions/archive/`, which is why it is
instant, why it needs no model and no key, and why undoing it by hand is the same command
backwards. The moment you want to tidy the list is often the moment the network is broken.

**The config is hand-editable TOML, and missing keys take defaults.** A new key must not
break an old file. A *wrong* value, though, fails at load with the values that would work —
`instructions = "sometime"` is reported when the file is read, rather than being discovered
later as "the model ignored my instruction file".

**There are two `proxy` keys.** The one inside a provider table is how flint reaches the
model; the top-level one is exported to the commands the `bash` tool runs. They are
different routes, and on a machine where one works and the other does not, conflating them
takes away the ability to fix either.

## Context: instructions and skills

**`instructions = "hint"` is the default.** flint names the instruction files it found and
lets the model read them, rather than pasting their contents into every request. Pasting
spends the context budget of every turn on files that most turns do not need; `paste` and
`off` remain one config edit away.

**Discovery runs outermost-first and stops at a boundary.** The home-level `AGENTS.md` first,
then upwards from the working directory, stopping at `.git` or the home directory. A file
higher up is more general, so it belongs earlier in the prompt, and a repository is a
boundary rather than a suggestion.

**Skills are one level deep, and the project wins a name conflict.** `<project>/.flint/skills`,
then the working directory's, then the config directory's, then `skill_dirs`. A deeper tree
would need a discovery story and a conflict story; one level needs neither.

**The `skill` tool does not exist when there are no skills.** A tool that can only fail is
worse than no tool: it costs tokens in every request and teaches the model that its tools
lie.

**Nothing about context is cached.** Not the instruction files, not the catalog. `/reload`
re-reads them, and a file written mid-session takes effect on the next turn.

## Tools

**Read before you mutate.** `write`, `edit` and patch updates refuse a file this run has not
read, and refuse one that changed on disk since it was read (mtime and length). The record
lives in memory only — what this run has looked at is a fact about this run, and persisting
it would be exactly the stale derived state the project forbids. Three deliberate holes: a
file that does not exist yet can be written, a file this run wrote counts as read, and
`bash` is not gated at all. That last one is the honest part: flint cannot both allow `bash`
and pretend writes go through a gate. **This is a guard against mistakes, not a security
boundary**, and `AGENTS.md` says so in those words.

**Long output keeps both ends and spills the rest to a file.** The head is where the command
and the first errors are; the tail is where "build failed" and the summary are; the middle is
what a person would skim. The whole output goes to `~/.flint/spill/<session>/<n>.txt`, plain
text, named in the reply — `[truncated]` alone used to throw away the line that mattered.

**A repeated call is noted, never blocked.** The 3rd, 5th and 8th identical call in one turn
gets one line saying so, and the note is attached to the result so the *model* sees it. A
repeat is sometimes right — another process may have changed the file, a command may be
genuinely slow — so flint informs instead of refusing. The count is per turn: a repeat in a
new question is a different thing.

**Argument validation distinguishes absent from mistyped.** "Missing required string
argument 'path'" for a `path` that was sent as a number sends the model to fix a problem it
does not have. Optional arguments matter more: read as absent, `"limit": "5"` silently
becomes "no limit", and the instruction disappears with nothing said.

**`apply_patch` locates hunks by their own lines, and is all-or-nothing.** No line-number
arithmetic to get wrong, and no way to land a change in the wrong place because the file
grew since it was read. Several files in one patch are one edit, so a hunk that fails in the
third file leaves the first two untouched. One section per file, because two sections would
both be planned from the same contents on disk. No `*** Move to:` and no fuzzy matching: a
rename is a delete and an add, and a patch that does not fit is a patch that needs rewriting
against the file as it is.

**`readonly` is all-or-nothing.** There is no middle setting, because a middle setting that
looks safe and is not is worse than a switch whose meaning is obvious.

## Output

**A redirected run emits no escape codes.** `flint --help | less`, `flint exec ... | grep`
and every pipeline depend on it, and it is easy to break in one place while everything else
stays clean — which is why `tests/cli_output.rs` counts ESC bytes in the real binary's
output rather than trusting the code paths.

**`--json` owns stdout, or it owns nothing.** One JSON object per line, no banner, no status
row, no trailing prose. The vocabulary is closed and the shape is asserted on bytes: a tool
result containing newlines, quotes and escape codes is JSON-escaped, never printed raw,
because a reader splitting on `\n` must not be able to cut an object in half.

**NDJSON keys are in a stable order rather than a chosen one.** Putting `type` first would
mean `serde_json`'s `preserve_order` feature and an `indexmap` dependency. Stable output —
two runs of one conversation compare with `diff` — is worth more than any field being first.

**The stream is a view, not the record.** The session file is written exactly as in any other
run, and `session.started` names it. Nothing in the stream is the only copy of anything, and
a failure is described on the stream as well as by the exit code, so a caller reading stdout
does not have to read stderr to learn that nothing is coming.

**The status clock belongs to the round, not to each announcement.** Every tool call in a
round is announced before any of them runs; naming the clock on each announcement claimed the
last call was the one being waited for. The clock therefore starts on the first call, moves
to the next as results arrive, and runs nameless — "waiting for the model" — between rounds.
Committing a transcript line does not repaint it: a fast tool used to flash `0s`.

**A captured run writes to a file instead of taking over stdout.** The capture tests used to
redirect file descriptor 1, which also collected the test harness's own progress lines; one
landing on the bottom row scrolled the transcript off the recorded screen and made a passing
run look broken. `FLINT_TERM_CAPTURE_FILE=<path>` is the recording, and no test moves a
descriptor.

## Tests

**No test ever uses a real API key.** The stub is `wiremock` replaying hand-written SSE
frames, including tool arguments split across frames. It is the only way to test the retry,
streaming and tool paths deterministically, and the only way a machine with no key can run
the suite at all.

**Terminal assertions are on real bytes.** `FLINT_TERM_CAPTURE=1` plus
`FLINT_TERM_CAPTURE_FILE` and `FLINT_TERM_SIZE` (debug builds) force the interactive path
into a file that `scripts/vtscreen.js` replays as a screen. A test that asserts on a
reimplementation of the renderer tests the reimplementation.

**Fixtures are built with `serde_json`, not by hand.** A tool call's arguments are a JSON
string inside a JSON string, and a Windows path is the case where writing that by hand goes
wrong silently — the fixture then measures a tool that never ran. One fixture written by
hand cost an hour of chasing a "bug" that was in the test.

## Dependencies

Ten runtime crates, and a new one needs a reason that survives being written down. What that
has bought so far: no YAML crate (the front matter of a `SKILL.md` is parsed by hand), no
date library (`epoch:<seconds>`), no TUI framework beyond crossterm's raw mode, no
`preserve_order`, and a `reqwest` configured with rustls rather than pulling in OpenSSL.

## Not doing

Subagents, a permission layer, `flint doctor`, MCP (deferred rather than refused), indexes,
caches, databases, compression, a PowerShell parser, and `-EncodedCommand`. The reasoning for
each is in [`ROADMAP.md`](../ROADMAP.md#not-doing-and-why); the short version is that each
one adds a second thing that can be wrong, and flint is meant to be the program that is still
working when the others are not.
