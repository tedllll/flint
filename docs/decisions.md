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

**`--no-session` is a property of the run, so the doors are what it has to close.** The file was
never the hard part — a session is created by its first event, so a run that says nothing writes
nothing — and the decision that made the flag real is that a promise about what is written is only as
good as the ways to write it. So `--continue`, `--resume`, `--fork` and `--name` refuse it on the
command line, `/new` and `/resume` refuse it inside the run (through one function, so the page's
sidebar rows and a typed line answer the same), and the funnel every mid-run switch goes through —
which *creates* the session when the old conversation has not said anything yet — is told once and
carries the flag. The obvious shortcut is wrong for a reason worth keeping: the flag cannot be derived
from "there is no writer", because a writer that has not appended yet and a run that will never write
look identical from the outside. What such a run still writes is what it needs to work — spilled tool
output, a background command's log — under `spill/unattached-<pid>/`, one directory per process rather
than one shared `unattached/`, because two of these runs both number their spill files from `1.txt`.
And the flag reaches a `task` child, which is the part that is not obvious until you look at what
excludes a child's conversation from the person's list: that is `FLINT_PARENT`, the parent's session id,
which a run with no conversation does not have.

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

**What a request carries is bounded, and the transcript is not the request.** Three separate
caps, because they protect against three different things: `max_tool_output` bounds one tool
result (the rest spills to a file), `prune_tool_output` shrinks the stale results *inside* a
turn before it is sent, and `max_request_chars` (400,000 characters, `0` turns it off) drops
the oldest *turns* of a conversation past it. All three are request-side only: the session
file keeps every message, which is what makes a dropped turn something that can still be
read, be named in a note, or be recovered by resuming — and it is why `flint debug
prompt-input` is a command rather than a debugging session. The unit of the third one is a
turn because a tool result separated from the call it answers is a request the provider
refuses, and the newest turn and the system prompt are never dropped because a request with
nothing to answer is not a smaller request. Pruning runs before trimming, so the cheap bound
is spent before the expensive one.

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

**The loopback listener is hand-rolled, not `hyper`.** This was `docs/web-mode.md` §7, the
one question that document deliberately left open, and it is recorded here because it is a
dependency decision and those live in one place. `hyper`, `hyper-util`, `http-body-util` and
`tokio-util` are **already in the lock file**, pulled in by `reqwest`, so the usual argument
against a dependency — build time, binary size, a longer supply chain — does not apply. The
argument that does is API surface: the routes are four, two of them are a string comparison
each, and `Connection: close` removes keep-alive, pipelining, chunked encoding and request
framing from the problem entirely. What is left is reading to a blank line and writing a
status line. Enabling `tokio`'s `net` feature for it compiles nothing new either: `mio` is
already there through `process` on Unix, which `Cargo.lock` confirms by not changing.

The counter-argument is real and is not waved away: HTTP has more corners than the `SKILL.md`
front matter this reasoning is modelled on, and this is the one place in flint that listens
on a socket. If a route ever needs framing this file does not have, the answer is `hyper` and
not a hand-rolled chunked decoder.

## Why this program, next to Pi

On 2026-09-17 another agent harness was read from end to end: **Pi** (pi.dev,
`github.com/Earendil-Works/pi`), the minimal TypeScript harness by Mario Zechner. It is the
same kind of program built on the opposite bet — one agent loop, a handful of tools, sessions
on disk, a terminal UI — and [`pi-agent-harness.md`](pi-agent-harness.md) is the record, with
every claim sourced from a page that was loaded. It is kept because "why is this not just
somebody else's program" is a question worth answering in writing, and because a second
reading of the same problem is the cheapest test this design will ever get.

**It agrees with flint on more than either program would probably guess.** No permission
layer; plans and to-dos as files; append-only JSONL sessions a person can read; writing to
the scrollback instead of taking over the terminal; progressive disclosure for skills; a
system prompt measured in hundreds of tokens; no MCP; tool output that must truncate and say
where the rest is. Ten years of convergent evolution in two programs written by two people who
have not met is evidence about the *problem*, not about either program: an agent harness wants
plain files and small prompts, and the field will keep proving it.

**The overlap is the commodity layer, and the difference is the part that cannot be
imported.** Pi refuses, in writing and on purpose: background work (its answer is tmux),
sub-agents ("a black box within a black box"), a permission layer, a plan mode, built-in
to-dos, and MCP. Three of those refusals are things flint has and uses: **jobs**, with one
record, a listing, an exit code, a panel and a stop; a **`task` child whose conversation is a
file** a person can read, resume and move up a level; and **doors**, so that a page, a
`--json` caller, an MCP client and Python all drive the same run. A loop and a session format
are a weekend's work in any language; those three are the reason this one exists, and they
are exactly what is argued against there.

**What the reading changed here, and what it did not.** It added `ROADMAP.md`'s "Taken from a
reading of Pi" — twelve agreed items, none of them a new bet, all of them cheap — and it
declined one, a run-level tool allowlist. It changed none of the rules below. Two of them are
worth restating with Pi as the test:

- **Nothing is derived, and the state is hand-editable.** Pi arrived at the same rule
  independently: a JSONL tree, entries appended rather than rewritten, even modal state
  (which model, which thinking level) written as entries so the setting in force at any point
  is recoverable from the path. That is a good sign for the rule, and the only place the two
  part company is how much is written back: flint's request-side pruning leaves the file
  untouched and says so with a count.
- **Dependencies are the enemy.** Here the programs genuinely diverge. Pi is a TypeScript
  monorepo on Node, with npm-installed extensions, a generated model registry and a
  per-vendor compatibility table for four wire protocols. Flint is one static binary and ten
  crates. That is the bet this file is about, and it has one consequence that is written down
  rather than discovered later: **flint will never match Pi on breadth of providers, and if
  breadth is what somebody needs, Pi is the better program.** Flint is for the machine the
  person is actually on, for seeing and stopping the work a run actually started, and for
  files that can be repaired with a text editor.

**What would make this the wrong bet.** If the machine stops mattering (every host with Node
and tmux on it), if the doors go unused, or if the work a run starts is genuinely better
observed by an external multiplexer than by the program that started it. None of those is true
here today, and the way to find out is the list in `ROADMAP.md` rather than an argument in
this file.

## Not doing

Subagents, a permission layer, `flint doctor`, MCP (deferred rather than refused), indexes,
caches, databases, compression, a PowerShell parser, and `-EncodedCommand`. The reasoning for
each is in [`ROADMAP.md`](../ROADMAP.md#not-doing-and-why); the short version is that each
one adds a second thing that can be wrong, and flint is meant to be the program that is still
working when the others are not.
