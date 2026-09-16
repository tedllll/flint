# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

**The next round is §10 of `ROADMAP.md`: "flint as a function a program can call" — the agents work
landed in front of it, and §10 is still the queue.** Steps 1 and 2 are done; the pieces below them are
listed in order. The section is an audit in three buckets (what cannot be done at all, what cannot be
told apart, what is a hole), the reference points it was measured against (Claude Code's
`-p --output-format json`, `llm`, and the `sysexits.h` convention for exit codes), and the order to
build it in. Step 1 landed the turn's `outcome` and the exit codes, and fixed the C1 bug (a `/stop`ped
run exited 0 while carrying a truncated answer — now 130).

**Step 2 is `error.code` produced where the cause is known. Its first job was money, and it is done**:
§10 **B6** was added after the fact, from a report of hitting an empty balance repeatedly, and it was a
bug fix as much as a classification. `provider::ProviderFailure` now carries `code` and `retryable`
out of the code that read the response, `classify` reads the body as well as the status, the `error`
frame names the cause, and the exit code follows the cause (`69` for money or credentials, `75` for a
failure the retries could not outlast). **Still open in B6**: only the `map_calls` circuit breaker, which belongs to step 7.
`flint balance` is built (a preflight that asks `/user/balance`, falls back to `/models`, and says
"cannot tell" rather than "usable" when neither answers). **B7 is built too**, so a `--json` caller now
hears about *every* failure on the stream, including a refusal resolved before the stream would have
been opened: the command line is read in `main`, a failure is written there as one `error` frame (from
the same `error_frame` the end of a turn uses) and not to stderr, and the case where flint was refused
*before* it had read `--json` is the honest residue — it is reported on stderr like any other bad
command line. **Step 3 is built**: `flint --list-sessions --json` prints the list as one object
(`type`, `count`, `sessions[]`) with each row's id, path, directory, name, preview and label, built
from the same `list_detailed` the printed listing uses so the numbering cannot drift, and the label
rule now lives once (`SessionSummary::label`); and `--result-file <path>` writes this run's answer to
a file as well as to the stream — the answer text, or the validated object pretty-printed when a
schema was given — with the one property that makes it safe to reuse a path across a batch: **the
file is emptied when the run starts and filled only if this run answers**, so an empty file means
"nothing answered" rather than an earlier run's answer looking like this one's. **Step 4 is built
too**: `@path` in a one-shot prompt is replaced by that file's contents before the request
(`src/attach.rs`), which is how A1 is solved — a 247 KB document now travels through four characters
of command line, checked in `examples/python/test_call.py` on the same Windows where a 33k argument
raises `WinError 206` in `subprocess` before flint even starts. The rule is deliberately small (a
name is inlined only when it is a readable text file; prose is left alone; `turn.started`'s
`attachments` says what went in, so a mistyped name is a visible empty list), the cap is 256 KB
refused before anything is sent, and the stream keeps the caller's own words while the request and
the session hold the expanded prompt. **Step 5 is built too**: `--max-seconds N` bounds the whole run
(one absolute instant, so a repair attempt does not get a fresh budget), and when it runs out the turn
is dropped where it stands with the drawn half kept — the same machinery as `/stop` — reporting
`outcome:"incomplete"` with `"reason":"seconds"` and exit 65 rather than `stopped`, because nobody
changed their mind: a limit the caller set was reached, which is what the step limit already meant.
That added `reason` to `turn.completed` (`steps` or `seconds`), since the two budgets are raised in
two different places. The plain path got the same bound and its exit code now carries the outcome
(65 unfinished, 130 stopped, where it used to say 0 for both). **Step 6 is built too**: the stream's
byte-level promise — one JSON object per line and nothing else on stdout, no `\r`, no escape code,
UTF-8 strictly, a documented `type` — is asserted on the raw bytes of four shapes of run, with the
vocabulary listed in the test as well as the README (which it turned out had drifted: `status` and
`command` were missing). Next in the queue: step 7, the Python side.
Read it before touching `src/main.rs`'s argument handling. In short: DeepSeek says it with
**402**, OpenAI-shaped endpoints say it with **429 `insufficient_quota`** — the same status as a rate
limit — and Anthropic with a 400 and a sentence. flint decides "transient" from the status **and** the
body (`provider::classify`), which is what stopped the four retries: before that, a quota 429 was
retried four times with a 1+2+4+8-second backoff and the caller learned nothing from the exit code.

**flint is now callable from an MCP-speaking agent, and that is built rather than planned.**
`examples/mcp/flint_server.py` is a stdio MCP server (standard library only, one tool, `flint_ask`)
for Codex, Claude Code and Cursor; the result text carries flint's exit code, outcome, cause and
session path, and a schema run also returns `structuredContent`. `examples/mcp/test_mcp.py` speaks
the protocol at it — handshake, listing, a real answer, structured output, and a schema that never
matched arriving as `isError` + exit 65 instead of as a success. §10's "being used by another agent"
section also records what the wrapper deliberately does about MCP's token cost (one tool, the tool
loop stays in the child, answers instead of transcripts, a byte-stable description so a parent's
prompt cache survives a release). The Python caller's `balance()` is the other half of B6: ask before
the batch instead of discovering an empty account on call one.

**Open and newly written down**: `docs/agents.md` is the plan for runs that spawn, find and talk to
each other — prompted by three questions asked directly, and by the incident it opens with (two agents
in this checkout at once, one of them mid-write in `docs/sandbox.md`, and the other committing a
half-written revision of it with `git add -A`). Its claim is that a subagent, a Python call, an MCP call
and "a background process" are one thing — a run — seen through four doors, and that the MCP and Python
doors are already built. **Stages 1, 2 and 4 are built, and so is the mailbox half of stage 3**
(`flint who`, the `task` tool, `tasks`, profiles, `flint say`), the "Subagents" entry in `ROADMAP.md`
was edited in the same commit as the code that adopts it, and **the five decisions at the end of that
file were answered on 2026-09-16 at the recommended values** — the depth bound (`FLINT_DEPTH`, maximum
2), the mailbox default (a peer's words are shown to the human and never fed to the model, which is why
the opt-in is not built), what "another agent is here" may claim (no author, ever), and presence in
`FLINT_HOME` only. What stage 3 still owes: the `.flint/` marker in the project and that opt-in. Stage
4 was written into `ROADMAP.md` and `docs/agents.md` in the same commit as the code, with the line
drawn where it was built: profiles and an explicit, capped fan-out, and nothing above it — no shared
context, no merge, and no flint choosing to parallelise on its own.

Everything is committed, the working tree is clean, and `main` is pushed to `origin/main`.
As of the commit that carries this file, `cargo test` is 502 passing, 1 ignored (302 lib, 3 in
the binary's own tests, 33 `agent_loop`, 61 `cli_output`, 34 `json_output` (7 structured
output, 1 the heartbeat, 2 the stop channel, 8 the exit codes and the turn's outcome, 2 the
balance, 1 what a caller's pipe must not come back out of, 3 the refusal a `--json` caller has to
be able to read, 4 the answer written where the caller asked, 3 the file inlined into the prompt,
1 the stream checked on its bytes),
7 `balance`, 4 `search_tool`, 20 `term_capture` plus the ignored cost measurement, 22 `web_view`, 6 `who`, 8 `task`, 2 `say`), `cargo clippy
--all-targets` is silent, both `node scripts/term-layout-test.js` and `node scripts/web-view-test.js`
pass, and `python examples/python/test_call.py` is 61 checks, all passing (one of them waits
out the fifteen-second retry ladder on a dead endpoint, deliberately: that is where `75` comes from),
and `python examples/mcp/test_mcp.py` passes its own 23.

**The `task` bug report of 2026-09-16 is fixed, and it was the missing background handle seen from the
other side.** A person started a child, the parent's status row went on saying `task` and nothing else
for minutes, so they typed at it — which is what typing does, it steers, so the turn was dropped — and
the parent then recorded *"interrupted by the user: tool 'task' was requested but never ran."* while the
child went on working with its own session file and its own bill. Three things changed. The child's own
`tool.started` and `status` frames are forwarded to the parent's status row (`task: running search`),
because the parent was already reading those frames and discarding them; the sentence a dropped turn
writes is now the true one (*"the result of 'task' never came back"*) and continues with each child that
is still going, naming its pid and the session its answer will be written to (`tools::children_running`,
fed by a registry the reader task fills and clears, so it survives the dropped future); and a `--json`
run that is cut short says the same thing as a `warning` before its outcome, because a one-shot has no
next turn in which `close_dangling_tool_calls` could say it. Four tests, each watched red first:
`a_childs_own_progress_reaches_the_parents_status_row` and `an_interrupted_task_says_what_it_left_running`
in `tests/task.rs` (the second drives the real REPL through a pipe, steers mid-turn, then reads the
parent's own session and stops the child by the pid the parent named), `a_childs_frames_are_reduced_to_what_it_is_doing_right_now`
in `src/tools.rs`, and the stream test's budget shape in `tests/json_output.rs`. What is *still* missing
is the handle itself — `task_status`, `task_wait`, `task_stop` — so collecting a child means reading the
session the note names.

**`flint who` is built — stage 1 of `docs/agents.md`.** A running flint writes one record per run
under `<FLINT_HOME>/live/` (`src/live.rs`), refreshed every five seconds by a thread that waits on a
channel rather than sleeping, and removed by a `Drop` so that every exit path is covered by
construction. `flint who` lists the live runs in this directory, calls a record left by a killed
process *stale* rather than alive, reports a record somebody edited into nonsense as unreadable rather
than skipping it, and prints the recent changes that it deliberately refuses to attribute to anybody.
`who` needs no key and does not list itself; `--all` names runs elsewhere; `--json` is one object.
One measurement is worth carrying forward: the first version of the refresh thread slept and was
joined on the way out, which made **every run take up to five seconds longer to exit** — the existing
quota test, which has a timing bound, caught it at 5.019 s. That is why the thread waits on
`recv_timeout`.

**A flint can start another flint — stage 2 of `docs/agents.md`, and the "Subagents" entry in
`ROADMAP.md` was adopted in the same commit.** The `task` tool (`TaskTool` in `src/tools.rs`) runs the
same binary (`std::env::current_exe()`, `FLINT_BIN` to override) with `-p … --json`, reads the child's
stream, and returns one block of text: the child's answer, then `exit code: N (meaning)`, `outcome`,
`cause`, `error`, the validated `result` object when a schema matched, and `session: <path>`. Three
properties are the point, and each has a test in `tests/task.rs` that was watched red first: the child
is a **real process**, so its session file exists and holds the prompt and the answer (provenance); a
`readonly` run **cannot be talked into a writing child** (the attempt is made in the test, and the same
script with a writable parent does write, so the refusal is the flag and not the child's inability);
and the chain is **bounded** by `FLINT_DEPTH` (maximum 2, set by the tool for its child and by nothing
else, so a model cannot edit it out of its own command line). Not built: the background handle
(`background: true`, `task_status`, `task_wait`, `task_stop`) and a session id in the presence record,
so a child appears in `flint who` as a run in that directory rather than as *this* run's child.

**Two runs in one directory can talk — the mailbox half of stage 3 of `docs/agents.md`.** `flint say
"…" [--to <pid>] [--cwd <dir>] [--json]` appends one JSON line to
`<FLINT_HOME>/mailbox/<dir-key>.jsonl`, keyed by the same directory key the sessions use, and needs no
key: it writes a file rather than asking a model anything, like `who`. A running flint follows its
directory's mailbox from wherever it is when the run starts (so it shows what arrives *while* it works
rather than replaying yesterday), renders it in the transcript as `peer <who> says: …` followed by
`(shown to you; not sent to the model)`, and writes it to the session file as its own `peer` event.
That last part is the safety property this half was allowed to ship on, and it is enforced rather than
promised: `session::load` reads a `peer` event **without** putting it in `messages`, so the history a
request is built from cannot contain one; `tests/say.rs` makes a peer speak *during* a turn through the
real command and then asserts the person saw it, the session file kept it, and **every request body the
provider received is free of it**. Deliberately not built: the opt-in that would feed a peer's words to
a model, the `.flint/` presence marker in the project, and a `/say` inside the prompt. One bug worth
remembering: `flint say` takes everything after it as prose, so `--cwd` and `--json` have to be pulled
out before the rest is joined — the first version put `--cwd` *inside* the message and sent it to the
wrong directory's mailbox, which the end-to-end test caught.

**A profile is a file, and a fan-out is one call — stage 4 of `docs/agents.md`.** `<project>/.flint/agents/
<name>.md` (and `<FLINT_HOME>/agents/`, with the project's copy winning on a name) has front matter for
`description`, `model`, `provider` and `readonly`, and a body that is the child's instructions. It is
read by the same widened hand-written front-matter parser the skill catalog uses — no YAML crate — and
only the front matter is read during discovery, so a directory of long profiles costs a few lines of
request rather than all of their text, and editing one takes effect in the next child without a restart.
Three properties are deliberate and each is asserted in `tests/task.rs`: a profile's facts are
**defaults** an argument on the call still wins, *except* `readonly`, which a profile can only add,
because a profile that could talk a readonly run into a writing child would route around the one
property this path exists to keep; the instructions go **in front of** the job (they answer different
questions, and the child has no flag for a system prompt); and the catalog reaches the model **inside
the `task` tool's schema**, only when this directory has profiles at all, since an `agent` property with
nothing behind it is a schema's worth of tokens teaching a model that a tool lies. `/agents` lists them
in the REPL (`[readonly, model cheap]`) and `/agents <name>` prints one the way a child receives it. The
fan-out
is `tasks`: 1–8 jobs, `max_parallel` 4 by default, **every** child prepared before any starts (a bad
argument in job three must not leave one and two already spending money), one labelled block per job in
ask order, and the same provenance a `task` gives. Two tests were watched red first, and both are worth
keeping: with `DEFAULT_PARALLEL = 1` the fan-out test reported "their requests arrived 2.8068615s
apart", and with the profile body replaced by the empty string the profile test reported "the profile's
instructions never reached the child". Not built, and not an oversight: no shared context (children
cannot see this conversation or each other), no merge, no background handle, and no flint deciding to
parallelise — the model asks for N jobs or it does not. The price is stated where it lands: `tasks`
adds one tool schema to every request, always offered, and N jobs is N bills at once, which is why every
block comes back with its session path attached.

**A `--json` run now beats while it works.** `src/main.rs` spawns `beat_while_working` beside the
turn: every five seconds it emits the `status` frame with the phrase the stream last described, plus
`elapsed_secs` and `restarted`. The flag that stops it is cleared under the same lock the beat writes
under, so the last line of a stream is never a heartbeat — the test asserts the beat comes *before*
`message.completed`, because a beat that arrives with the answer fills no silence. Verified live
against a dead endpoint (`{"elapsed_secs":5,"restarted":true,"text":"thinking","type":"status"}`) and
in `tests/json_output.rs` with a six-second stub.

**An exit code means something now, and a stopped run is not a success.** Every failure used to exit 1:
a typo in an argument, a missing key, an unreachable endpoint and an answer that never matched its
schema were the same signal, which forced a caller to match on error text. They are now 2 (the command
line), 65 (`EX_DATAERR`: an answer that cannot be used — a schema that never matched, or a turn that
ran out of steps), 69 (`EX_UNAVAILABLE`: the provider cannot be used at all), 130 (interrupted), 1 for
what is not classified yet, and the turn's end carries an `outcome` (`complete` / `incomplete` /
`stopped`) so a program reading the stream is told the same thing. The classification is made where the
cause is known — `parse_args` is wrapped in a `Usage` error type rather than the message being read
back — which is the shape step 2 extends to the provider. 75 (`EX_TEMPFAIL`) is declared and unused
until then. The Python caller's `Turn.ok` is now false for a stopped run, deliberately: it was true
before, and that was the bug.

**`/stop` works on the `-p` channel.** A `--json` run reads its stdin for the one line it acts on:
`/stop` drops the turn, calls `agent.commit_drawn_answer()` (the same fix the REPL has had since the
half-answer fault was reported), emits a `warning`, ends the turn and exits **130**. Any other line becomes
a `warning` saying it did nothing, rather than being dropped in silence. The Python caller sends
`/stop` when `timeout=` runs out instead of killing the process, so the half-answer survives in the
session: `test_call.py`'s last section checks a stalled stub's fragment lands in the file, and
`tests/json_output.rs` has the Rust half (`a_run_can_be_stopped_from_stdin`,
`a_stopped_run_keeps_what_it_had_drawn` — the latter watched failing with only the user's message in
the record when the commit is removed).

**The Python caller lives in the repository**: `examples/python/flint_call.py` (`ask`, `ask_json`,
`Turn`, no dependencies), `test_call.py` (21 checks against a local stub, `cargo build` first),
`stub_provider.py`, `timing_demo.py` (what blocking and a non-raising failure actually look like) and
`ask_schema.py` (live, needs a key). `docs/python.md` is the prose, README points at it, and
`flint_call._binary` runs the build in this checkout when there is one so a check cannot pass against
an older installed flint.

**Structured output is in**: `flint -p "..." --json --schema <file|{...}>` puts the schema in the
system prompt, asks the provider for `response_format: {"type":"json_object"}` (the only JSON mode
DeepSeek accepts — it rejects `json_schema`), validates the answer against a hand-written JSON Schema
subset in `src/schema.rs`, asks again with the specific JSON paths that failed (three attempts), and
emits `{"type":"result","json":...,"attempts":n}` once an answer passes. No `result` line is ever
emitted for an answer that did not pass; that run ends with an `error` and exit 1. The schema is
recorded in the session as a `schema` line holding the whole schema, so `--resume`/`--continue` keep
the contract without repeating the flag, and `--no-schema` records that it was dropped. Verified
against the real DeepSeek endpoint (one `result`, `attempts: 1`, the `schema` line in the file) and
against the stub in `tests/json_output.rs`, which also asserts the request body carried both halves.

**Sessions are now separated by working directory.** A conversation lives in
`sessions/<dir>/<id>.jsonl`, where `<dir>` is the last path component of the working directory plus
a hash of its canonical path; older sessions sit directly in `sessions/` and are still found.
`--continue` looks in this directory's own subdirectory first and falls back to the root, filtered by
the `cwd` recorded in `meta`. The listing reads both levels, `--resume`/`/delete`/`/archive` resolve
through the path the listing carries rather than rebuilding one, and `session::list_archived` searches
every archive (the root's and each project's). `--cwd` is resolved to an absolute path at startup and
refused if it is not a directory, and it is what gets recorded — so a program driving flint one
process per question can name its project and still find its own conversation from any directory.

**A session file is created by the first thing said in it**, not when flint starts: opening the REPL or
`--web` and typing nothing leaves no file, no sidebar row and nothing in `/sessions`, and a run refused
before it says anything (no key, an endpoint that cannot be reached) leaves nothing at all.
`SessionWriter` decides this with `create_new` — whoever creates the file is the one holding the `meta`
line — so `meta` is still the first line of every file that exists. `SessionWriter::resume` now refuses
a path that is not there, which is the backstop for the case that produced a `meta`-less file during
the first attempt at this.

**The last sessions were on Windows** (10.0.26200, AMD64, rustc 1.98.1, PowerShell 5.1.26100.6584
as the only PowerShell on `PATH`, locale ANSI code page 936), and the work is now being moved to
another machine — hence the checklist below. Nothing in the tree is Windows-specific except where
the tooling documents say so: the Windows command-line work is finished, `docs/windows.md` is the
field notes for the terminal, and `docs/windows-tooling.md` records what the machine said rather
than what the documentation implied (five of its predictions were wrong, and the corrections are
more useful than the fixes).

Build with:

```bash
cargo build --release
```

The binary is held open by any running `flint`, so close those before replacing it.

## Picking this up on another machine

Written for a cold start: a different machine, and possibly a session with no memory of the last
one. The repository is the whole state — there is no index, no cache and no database anywhere in
flint, on purpose — so cloning it and running the gate below is all that "catching up" means.

**Where it was left.** `main` at the commit that gives the example and the REPL one rendering of a turn
(`refactor: one rendering of a turn, shared with the example`), plus the documentation commit that
carries this file, working tree clean, `origin/main` level with it. **§8 is finished** — all five
controls are on the page, the destructive rows have a second home on the sidebar, the two switches are
offered the names the run already knows, and the one hole the round left open (adding a provider) is
closed — §9 has five entries, all closed, and the roadmap's small list is now **empty**: this was its
last item. What remains is written down where it belongs: `/config edit` as a page form would need a
`/config set <key> <value>` the terminal does not have, the panel's groups are still §8's classes
rather than a task, and the mid-turn report wait is unasserted (`docs/web-mode.md` §11).

**The installed binary is older than the tree, and that matters for looking at the page.** `flint` on
this machine's PATH resolves to `C:\Users\zhangzhuo\bin\flint.exe`, which is a copy of
`target\release\flint.exe` as it was on 2026-09-14 16:08 — before the panel, the selectors, the forms,
the destructive controls and the sidebar menu. The page is `include_str!`-embedded, so a page change
does not reach an existing binary: `cargo build --release` and copy it over, and the `commands` panel
appears in the header (a `<details>`, opened by clicking the word). A running `flint` holds that file
open, so the copy needs every flint window closed first — measured twice.

**What the other machine needs.**

1. `rustup` with the stable toolchain (`rustc 1.98.1` here; nothing needs nightly), plus `node` for
   the two replay scripts — `cargo test` does not run them, so a machine without Node passes
   `cargo test` while the page's own renderer is untested.
2. No API key, and no network, for the gate below: every test that talks to a model uses a
   `wiremock` stub, and the ones that need a real process point a scratch `FLINT_HOME` at
   `http://127.0.0.1:9/v1` (a port nothing listens on) precisely so that no test can reach out.
3. A terminal to *use* it, which is not needed to verify it: `FLINT_TERM_CAPTURE=1` with
   `FLINT_TERM_CAPTURE_FILE` and `FLINT_TERM_SIZE=100x24` forces the interactive path into a file
   for `scripts/vtscreen.js` (debug builds only), which is how the layout tests see real bytes.

```bash
git clone git@github.com:tedllll/flint.git && cd flint
cargo test                                        # 499 passing, 1 ignored
cargo clippy --all-targets                        # silent, and worth keeping that way
node scripts/term-layout-test.js                  # 全部通过
node scripts/web-view-test.js                     # all passed
cargo test --test term_capture -- --ignored --nocapture measured_cost_of_streaming   # the cost number
```

The rest of this file is the reference for the parts that are easy to get wrong:
`## Verification without a terminal` has the capture variables (`FLINT_TERM_CAPTURE*`) and the two
traps that cost time when writing a new REPL test, `## Pushing from this machine` has the deploy key
(a fresh checkout needs `core.sshCommand` only if the default key is not authorised on the
repository), and `AGENTS.md` has the rules a change is expected to follow — the one worth repeating
here is that a regression test has to be *seen* red, or mutation-checked when the code came first.

**Trying it without touching a real setup.** Point `FLINT_HOME` at a scratch directory; the tests
do exactly that. Two commands are worth knowing before changing anything:

```bash
FLINT_HOME=/tmp/flint-try flint                       # a session in a throwaway home
FLINT_HOME=/tmp/flint-try flint --web                 # prints the token URL to open
FLINT_HOME=/tmp/flint-try flint debug prompt-input    # the request body that would be sent, no key needed
```

That is the bash spelling; in PowerShell it is `$env:FLINT_HOME='C:\temp\flint-try'; flint` for the
length of the session, or one command at a time with `$env:FLINT_HOME='C:\temp\flint-try'; flint --web`.

The page is reachable only at the printed URL: loopback plus a per-run token in the header
(`docs/web-mode.md` §4). A page opened from a *dropped file* has no process behind it and therefore
no header controls — that is the level-1 view, not a fault.

**Two traps, both cost time before.** A test run after restoring a file by copy may test the *old*
binary, because Windows `CopyFile` preserves the source's modification time and cargo then decides
nothing changed — watch for the `Compiling` line. And CI runs nothing on push:
`.github/workflows/release.yml` triggers on `v*` tags and `workflow_dispatch` only, so a green push
means the local gate was green and nothing more.

**What is not verified anywhere yet.** The page's controls have never been looked at in a browser:
the pickers, the switches, the command-answer block, the new command panel and the action buttons are
pinned as behaviour (Node over the stub DOM) and as bytes (`tests/web_view.rs`), and measured end to
end against a real process (`tests/cli_output.rs`), but nobody has used them with a real font, a real
keyboard or a real scroll position. `docs/web-mode.md` §11 says so in the same words, and the first
thing worth doing on the new machine is `cargo run -- --web`, press the header's buttons and change a
picker, type `/config` into the composer, and see whether the answer lands in the transcript the way
the block is drawn — then open the `commands` panel and read it against `/help` in the terminal.

## What was just done

**An interrupt no longer throws away the answer it was drawing, the commands that rebuild the agent
no longer throw the conversation away, a stopped turn now ends on the page as well as in the
terminal, `/readonly` sets the guard it says it sets, `/verbose off` outlives the run, the page
can change the model or the provider from its own header, a command typed into the page's composer
answers there — because a command that failed no longer ends the run — the page is told what
commands the run has, from the same table `/help` prints, and the one class of those commands that
takes no argument is a button in the header.** That is this round and the four before it: ten items
in one family — what the user has already read must not go missing, a surface must not go on saying
a turn is running after it has stopped, a switch must not report a state it did not set, a setting
must not be forgotten by the file that is supposed to hold it, a page must have a *read channel* and
an *output channel* before it can offer a control at all, the list of commands has to have exactly
one copy before a control is drawn from it, and the first control has to send the line the process
composed rather than one the page put together.
Each with a test watched red first, or mutation-checked where the code came first. The
measurement that corrected last round's diagnosis is below, then the second bug, which the first
one's measurement is what found, then the third, then this round's five.

**And the round before it, recorded here so it is not re-done — six commits on the page, all
pushed:**

- **The scrollbars are themed, and both boundaries drag.** `scrollbar-width`/`scrollbar-color`
  plus the `-webkit-` longhands with the thumb inset by `background-clip`, so the default grey
  bars are gone; the left hand resizes the sidebar (`--side`) and the right one the reading
  column (`--read`), both with pointer capture, arrow keys and a double-click reset. The reading
  hand carries a hairline at rest: at the right edge of the text there is no seam to notice,
  unlike the seam between the two panes, so it had nothing to be discovered by.
- **The transcript scrolls in its own box.** Content (`#doc`) and scroll container
  (`#transcript`) were one element, so the scrollbar sat inboard of the window edge and the
  reading hand's line ran through the composer. Both fixed, and the follow-the-tail test that
  caught the first regression is in `scripts/web-view-test.js` (shown red by reverting the line).
- **The page writes its own log, by default, to `$FLINT_HOME/web.log`** (`POST /log`, one
  whitespace-collapsed line per entry, capped). It exists because a diagnostic behind `?debug=1`
  is a diagnostic nobody has: four reports of "the page does not change" arrived with no evidence
  in any of them. `?debug=1` now only decides whether the same numbers appear in the hint line.
- **And that log found the bug in one line**: `boot failed: TypeError: Cannot read properties of
  undefined (reading 'length') at paint (…:862:19)`. The reading hand declared `const doc =
  document.getElementById("doc")`, shadowing the conversation document for the rest of its block
  — including the boot's `paint(doc)`. The throw took the sidebar, the session list and the feed
  with it and left the served markup on screen: exactly the report, four times. Renamed to
  `reading`. The boot now has an error surface, `readSessions` has a catch, and the events fetch
  has a deadline on *connecting* only — armed for the whole response it aborted a healthy silent
  stream every ten seconds, which the log showed as twenty-two identical `AbortError` lines.
- **`/stop` is the interrupt as a short word** — in the mid-turn input match and not in the
  command table: a key is not always available (ssh, a pipe, the browser's composer), and it is
  deliberately *not* handed back to the REPL, so a stopped turn does not then print "nothing is
  running", which reads as the stop having failed.

### The interrupt does not lose the conversation any more — and the diagnosis here was wrong

Reported once `/stop` worked: *the stop succeeds, and the next thing said does not know what was
said before.* That part was exact. What was written down here about *why* was not, in two places:
the history does **not** lose the user's line, and re-deriving from the session file would have
changed nothing, because the file did not hold the missing thing either.

**Measured**, with a stub that draws an answer and then holds the connection open (a body delivered
in one piece ends the turn, so there is no window to interrupt in it — `HangingProvider` in
`tests/cli_output.rs`):

- After `/stop`, and after a line typed mid-turn, the next request still carries the earlier
  conversation *and* the interrupted question. The history was intact all along.
- **What was missing is the answer that had already been drawn.** Text becomes a message only when
  the step that produced it *completes*, so an interrupt — which is a dropped future — takes the
  answer out of the history and out of the file at once. The session it was reported from has the
  shape (`~/.flint/sessions/1789373441-50.jsonl`: two user messages in a row with no answer between
  them; the model's own reply says it had no draft, "我上一轮只做了检查就被打断了", and then goes
  looking on disk for what it had written). The next session ends the same way
  (`1789373985-837.jsonl`: an article, then "写长一点", and the file stops there).

**The fix** keeps the drawn text where dropping the turn cannot reach it: `Agent::drawn` is a field
rather than a local of the future, every delta lands in it as it is drawn, `commit_drawn_answer`
turns what is left into an ordinary assistant message and appends it to the file, and `run_turn`
calls it straight after the drop — the agent cannot do it for itself once its future is gone.
Verbatim, with no marker inside it: the words are the model's, and an answer that stopped
mid-sentence says what happened better than a note would. Both doors are gated end to end by a test
that was watched red without the call (`a_stopped_turn_keeps_the_answer_it_drew`,
`a_steered_turn_keeps_the_answer_it_drew`).

### `/model`, `/provider` and `/reload` threw the conversation away — fixed in the same round

Found while measuring the above, and it is the same complaint through a different door. Same stub,
same session: `/resume <id>` on a file holding a question and an answer, then `/model <other>`, then
one message — and the request that went out had **two** messages, the system prompt and the new
line. All three commands build a fresh `agent::Agent`, and a fresh agent has an empty history *and a
session file of its own*: the conversation was gone from the request and from the session directory
at once, while the transcript on screen went on showing it. Nothing failed, and nothing said so.

**The shape it took** (the decision `ROADMAP.md` §9 had written down before it was built): a *new*
file, seeded with the conversation. `SessionWriter::seed` writes a `meta` naming the provider and
model the conversation is being continued with, then every message in hand except the system prompt
— rebuilt for every run, and a copy in the file would come back through `/resume` as a stale message
— then the conversation's name if it had one. `continue_conversation` in `main.rs` puts the history
back through `splice_loaded_history`, the route `/resume` takes. Nothing was added to the session
format. The other shape, appending to the old file, was rejected because `meta` names the model a
session is held with and `load` and `/resume` believe it.

**The page follows the conversation now too.** `/new` and `/resume` pointed the viewer at their new
file; `/model` and `/provider` — the two the page's own pickers reach — did not, so the view stayed
on a file nobody was writing and stopped moving. The call moved to the one place that swaps the
agent (`Flow::NewAgent` in the REPL), where the next such command gets it for free.

One consequence is accepted rather than fixed, and it is written down in §9: the
read-before-mutate gate lives in the tool box, so after a switch the model can be asked to read a
file it read just before it. It re-reads it.

Four tests, each watched red first — the three commands fail with a request body of
`[system, "and now?"]`, and the view test serves a run's own empty file:
`a_model_switch_keeps_the_conversation`, `a_provider_switch_keeps_the_conversation`,
`a_reload_keeps_the_conversation`, `the_view_follows_the_conversation_through_a_switch`. Each of the
three drives the switch between two requests the stub actually received, so what it asserts is the
*next request* — the context — and then that a file holding all of it exists, which is what survives
the process.

**Also measured, smaller, and left alone on purpose**: a line already waiting in the channel when a
turn starts is taken as an interrupt before the turn's future is ever polled, so that turn never
existed — not in the history and not in the file. It is what made the first question in the probe
above produce no request at all. The fix is a poll before the channel is read; a test for it would
be a race with the reader thread rather than an assertion, which is why §9 asks for the two lines
that make it structural first.

### The stop button, and the page that would not settle — fixed in the same round

**The button was the easy half.** It is offered from `turn.started` to `turn.completed`, sends
`sendText("/stop")` — the composer's own route, no new verb — disables itself on the first click so
one click cannot send two stops, and sits to the *left* of `send`, because a control that appears
under the pointer that was reaching for another one turns a steer into a stop. It reads
`doc.running`, the turn's own boundaries, and not `doc.status`: the status line also carries feed
trouble, and a stop button that appears because the connection dropped stops nothing.

**What the round found is that a stopped turn was over in the terminal and not on the page** — and
that the frame the page needed did not exist anywhere in the interactive path. `turn.completed` is
written by the one-shot `-p --json` path, so the page's handler for it (`doc.status = ""`, close
every open answer) had never run under `--web`. What normally closes an answer is
`message.completed`, and an interrupted turn never reaches it — the future is dropped — so the half
answer stayed open, the stop button stayed on screen, and the status line went on saying what the
turn had been doing. Measured with a stub that draws half an answer and then holds the connection
open, `--web` with its stdout in a file, and `/events` read as it arrives:

- **Every turn ends with `turn.completed`** — `turn_over` in `run_turn`, the same line `--json` ends
  a turn with from the same function, sent at both of the turn's exits together with clearing the
  status row.
- **`Term::activity_done` clears the activity without a terminal to draw on.** It returned early when
  `!interactive`, so the last frame after `/stop` was `{"text":"writing the answer","type":"status"}`:
  a `--web` run with piped output announced every wait it began and never the end of one.
  `activity_started` already answers "even when there is no terminal, because a browser watching a
  piped run still needs to be told" — this is the other half of that sentence.
- **`Live::answer_committed` drops the answer in flight once the drawn text is a message**, because
  `Event::Done` is what normally drains it and an interrupted turn never gets one. Otherwise the next
  page to open is handed the stopped half as an answer still being written.

The note that was here said the server should follow a stop with a **status** frame. That was half
right: the status is the strip and the answer is a block, so clearing the status closes nothing —
`turn.completed` is what the page already had a handler for. Four gates, all watched red first:
`a_stopped_turn_tells_the_page_it_is_over` (a real process and a live `/events` read),
`the_composer_can_stop_the_turn_it_is_watching` (the page's bytes), the in-crate
`a_page_opened_after_a_stop_is_not_told_the_stopped_answer_is_still_arriving`, and the page's Node
check, which runs `paint` and reads the button back.

### `/readonly` did not set anything — fixed, and it was found on the way into §8

**The only guard this tool has was a print statement.** `/readonly on` said "no writes, no mutating
commands", the `/config` line under it said `readonly = false`, and `config.toml` had no `readonly`
key at all: the arm worked out the value it meant to set, printed it, and set nothing. So a session
that believed it was guarded had full permissions, and the mistake was of the worst shape — not a
command that fails, but one that reports a permission the session does not have. It was found by
reading that arm while planning the `state` frame, and it is why the frame came second: a frame that
*reports* `readonly` out of a command that does not set it is the same lie in a new place.

The value is now written to the file first, and then the tool set is rebuilt — the flag is baked
into the tools as they are constructed (`ToolBox::new` hands `readonly` to `write`, `edit`,
`apply_patch`, `bash`, `exec` and `pwsh`), so the guard is in force from the next tool call rather
than from the next process. The rebuild is deliberately *not* `continue_conversation`: that writes a
new session file, which is right for a switch of model and wrong for a change of permission, so the
agent is rebuilt around the same file (`SessionWriter::resume`), history and model, and goes back to
the REPL as `Flow::NewAgent` so the page is re-pointed at the file it was already following. What it
costs is what every rebuild here costs and what §9 already accepts for `/model`: a fresh tool box has
read nothing, so the model may be asked to re-read a file. `readonly_guards_the_run_it_is_typed_into`
asserts the three promises — the file, the value in force in the run that typed it, and the value a
second process starts with — and failed before the fix on the first, with the file printed in full
and no `readonly` key in it.

### `/verbose off` did not outlive the run — the second toggle a switch would have gone on

**A setting that can be chosen and not kept is not a setting.** `/verbose off` set the printer to
QUIET and then saved `cfg.verbose = next >= CHATTY` — a `bool`, where `false` was both "off" and
the default — so the next run came back *on*. `/config` printed `verbose = false` while the run was
printing a line per tool call: the report agreed with the file and both disagreed with the run,
which is the same shape as the `/readonly` lie one layer down. Found the same way, by looking at
what a switch would have to be drawn from.

The key now holds the word — `verbose = "off" | "on" | "full"` — read and written through
`display::Verbosity`, which sits beside the three levels because that is the one place all of them
meet: `/verbose` takes the word, `config.toml` stores it, the printer compares the level, and the
page's switch will be drawn from the same name. A bool in an existing file is still read as what it
has always meant (`false` is `on`, `true` is `full` — reading `false` as `off` would turn every
existing config silent), and a word that names nothing is refused with the value in the message
rather than defaulted, because that file is meant to be hand-edited. `verbose_off_is_what_the_file_records`
(the file, the run that typed it, and a second process) and `the_old_bool_for_verbose_still_reads_as_it_did`
(both spellings, and the typo) were each watched red by reverting the line they pin — and the first
version of the second test passed for the wrong reason, having appended `verbose = false` *after*
`[[providers]]`, where a bare key belongs to the provider table and is not a setting at all.

### The read channel, and the first two controls — this round's §8 work

`ROADMAP.md` §8 says the page sends the command *line* and needs a read channel first. It has one:
a `state` frame carrying the provider and model in force, the configured providers and the models
each offers (through the same `ProviderConfig::choices` that `/model` lists from), and the
enumerable toggles. Named rather than a line of the run's vocabulary, because it is not an event but
where things *are*, like `status` — and re-derived from the live configuration at the top of the
REPL's loop, the one place every command returns to, which is what keeps it from drifting without
being derived state. `Live::state` drops it when it has not changed, and sends it once on connect
because a client with no cursor is never replayed the ring.

Drawn from it, in the header: a provider picker and a model picker, each sending `/provider <name>`
or `/model <name>` through `sendText` — the composer's route, so no command has a second
implementation — hidden until a frame arrives, and put back to the value in force when a send is
refused. The header stops printing the model and provider itself once a state is present: the
pickers say it and offer the alternatives, and the same fact twice, one copy unchangeable, reads as
two facts. A dropped file still gets it from the session's `meta`.

### The toggles are switches now — the rest of §8's selector class

**Drawn from the frame, not from the page.** Each toggle arrives as a name, the values that name
takes and the value in force — `{name, values, value}` — which is everything a `<select>` needs and
everything the command needs (`/<name> <value>`), so the page carries no list of its own: not the
toggles, not the words they take, and not which one is on. That is the shape that keeps a switch
from offering a word the command refuses, and it is why `/verbose`'s three-valued file key had to
come first: a switch drawn from a `bool` would have shown a value the file could not keep.

Three switches: `verbose` (off|on|full), `detail` (on|off) and `readonly` (on|off). A change sends
`/verbose full` or `/detail on` through `sendText` — the terminal's own line — and a refused send
puts the switch back to the value in force. A switch with only one value is disabled rather than
hidden, because it still has something to say. The `readonly` switch is here rather than in the
destructive class on purpose: the composer can already type `/readonly off` today, so the switch is
no new power over the run — and after this round's first discovery it is the one setting most worth
being able to *see*.

`showToggles` rebuilds the row when a state frame arrives, which is only when something actually
changed, and removes it when a frame carries no toggles at all — a switch showing a value nothing
reports any more is worse than no switch. Gates: the end-to-end test posts `/verbose full` over the
route the switch uses and reads the new value back off the feed; `the_toggles_are_switches_that_show_their_value`
is the page's policy (drawn from `toggle.name`/`values`/`value`, exactly one line per change, and it
was watched red by hard-coding `/verbose` in the handler); and the page's Node check runs the real
`showToggles` over the stub DOM, including the frame-with-no-toggles case.

### A command's answer reaches the page — and the bug that was under it

**The other half of the read channel.** A command answers through `printer.term()`, which draws in a
terminal and exists nowhere else, so every command typed into the composer answered into a place the
page's reader could not see. The capture is at the funnel: `Term::answer_start` / `answer_take`
record what is printed, and the REPL calls them immediately around `handle_command`, so what is
recorded is exactly one command's output. That scoping is the whole trick — a *turn* prints through
the same funnel and is already frames of its own, so recording it here would send every tool result
to the page twice. Recording at the funnel rather than converting a hundred `printer.term().line(…)`
call sites is why this was a small change. The printer's colour codes are stripped as the line is
recorded (`Term::plain`, both escape forms), because a browser draws escapes rather than obeying
them; the end-to-end run cannot see that half — its stdout is a file, so it has no colour — which is
why the stripper has its own unit test.

`Live::command` pushes the result as one `{"type":"command","input":…,"text":…}` line. It is not an
`event::Event`: that vocabulary is documented as a turn's, and a command runs *between* turns, with
its output deliberately outside the session file. It is on the same stream because the page renders
the transcript from that stream. The page draws it as a transcript block with the input as its label
— an answer with no question above it is a mystery, and the answer is often a listing (`/config`,
`/tools`, `/help`).

**The bug under it: a command that failed ended the session.** The command arms return `Result`, and
the REPL loop handed that error straight out of `interactive` with `?`, where it became the process's
exit status. `/verbose loud` — one word wrong — printed `flint: error: expected on|off|full, got
'loud'` and exited, taking the conversation with it. From the page it was worse: the composer sends
lines to the same place, so a mistyped command in the browser ended the run the page was watching,
and the page could not even say why, because the message went to stderr. A failure is an answer now:
printed on the terminal in the shape the input reader's own refusal already used (one `line` call per
line of the message — a single call carrying a newline moves the cursor down through the rows the
layout reserved), and carried to the page like any other. Found by running it, not by reading it:
`/verbose loud` followed by `/config` in a scratch `FLINT_HOME` showed the process dying after the
banner.

Gates: `a_command_that_fails_does_not_end_the_session` (red on its second assertion before the fix),
`the_page_is_told_what_a_command_answered` (a real process, `/events` read live), the two `term.rs`
unit tests, `a_command_answer_is_a_block_with_the_line_that_asked_for_it` for the page's bytes, and
three Node checks over the real `paint` and the real `applyEvent`. Two of them were mutation-checked
rather than watched red, because the code came first: neutering `Live::command`'s push fails the e2e
on "the page was never told what the command answered", and making `plain` keep the escape fails the
unit test.

### The page is handed the commands there are, and draws them as a panel

**The read half the last round left out.** `src/main.rs` now has `COMMANDS`, one row per command
(`label`, `send`, `help`, `section`, `on_page`), and `/help` prints it instead of a hand-written
block; the `state` frame carries every row whose class is not the terminal's own. The point is that
the list exists **once**: the page's panel, `/help` and the drift test are three readers of it, and a
second copy is how a page comes to offer a command that was renamed while the help goes on
describing the old one.

Two decisions worth keeping:

- **`send` is carried beside `label`, not derived from it.** `label` is what `/help` prints
  (`/provider key <key>`), `send` is what the page puts on the wire (`/provider key`). Splitting a
  label into a name and an argument list is the obvious shape and is wrong: `add` is part of
  `/provider add` and `<key>` is not, so the page would be re-deriving the terminal's grammar.
- **The class is carried now, before any control reads it**, because the drift test needs it to know
  which rows are safe to run with no argument — `/delete` is not — and because "what should the page
  offer for this" should not be answered by guessing from the name. §8's classes live in `OnPage`,
  and `OnPage::class()` returns `None` for the two that are not offered: `Toggles` (the three
  switches, already on the page from the `toggles` field, so listing them here would be one fact in
  two places) and `Terminal` (`/exit` above all — the page is a window onto a process and a misclick
  must not end a session — plus `/web`, `!` and `/stop`, which the composer's own button covers).

`/help` came out **byte-identical** to the block it replaced — the gate was to capture the output
before the change and diff it after — except for one deliberate split: `/config [edit]` is now
`/config` and `/config edit`, because one row cannot carry two classes and the page has to know that
one is a report and the other takes an argument. The hand-wrapped continuation of `/stop`'s row is
now a greedy wrap at the width the column leaves, and it reproduces the same break.

The page's half is a collapsed `<details>` in the header, grouped reports / actions / selectors /
forms / destructive — this page's reading order, not the table's — and it is deliberately **not a
control yet**: a report is shown rather than sent, and a button that sends a command this page has
not been taught to confirm would be a button that destroys a conversation on one click.

Gates. `the_page_is_told_which_commands_it_may_offer` reads the frame off the live `/events` stream
*and* `/help` off the run's stdout, and asserts every row the page was handed is a row the terminal
prints with the same description — the check that fails if the two ever become two lists;
`every_command_the_page_may_offer_is_one_the_terminal_takes` posts the frame's own `send` strings
back through the composer's route and reads the answers, so a rename fails it (mutation-checked:
`/tools` → `/tool` in the table, and the test failed naming that row) and it floors the number of
rows it checked, because an empty frame would otherwise pass the loop in silence.
`the_command_panel_is_drawn_from_the_frame` is the page's policy, including that the page contains
**no command name of its own** (`/provider key`, `/delete <n|id>`, `/reload`); the two Node checks
build a panel from a real frame and take it away when a frame has none; and the `/help` diff above
is the gate for the rewrite. The cross-check was mutation-checked too: dropping the destructive rows
from `/help` fails it on "the page is offered `/provider rm <name>` and `/help` does not print it".

**Not in the frame, deliberately: any control drawn from this list.** The list, its classes and the
panel are the read half; buttons, selectors, forms and the destructive confirmation are next, and the
decisions already taken for them are below.

**Not measured yet: the controls, or the panel, in a browser.** They are pinned as behaviour
(`applyState`, `fillSelect`, `showToggles`, `showCommands` and `showActions` over the stub DOM in
`scripts/web-view-test.js`) and as bytes (`the_pickers_offer_the_runs_own_commands`,
`the_toggles_are_switches_that_show_their_value`, `the_command_panel_is_drawn_from_the_frame`,
`the_action_buttons_send_the_frames_own_line`), and
the frame end to end (`the_page_is_told_the_state_its_controls_would_show`, which reads `/events` on
connect, sends `/model stub-other` and `/verbose full` to `POST /message`, and then opens a *second*
stream, which can only have been handed the snapshot). Nobody has looked at the header with a real
font, or used a picker or a switch from the keyboard, or watched a command's answer land in the
transcript, or opened the command panel and read it with `/help` beside it; `docs/web-mode.md` §11
says so in the same words.

### The actions are buttons — §8's first class, and the smallest one to get right

**A control that sends a line the process composed.** An action takes no argument, so there is
nothing to ask for and nothing to confirm: `showActions` puts one button in the header's controls row
for every `class: "button"` row of the command list, labels it with the row's `label` (the command's
own words, which is what `/help` prints), titles it with the row's `help`, and sends the row's own
`send` through `sendText`. Its answer arrives on the `command` line built two rounds ago, in the
transcript, which is where an action's one-line report belongs. A press that is refused calls
`showState(doc)`, the way a switch does: a control that quietly did nothing is worse than one that
says it could not.

**Only the button class, and both omissions are decisions.** A report is not sent — §8 keeps the
reporting commands off this page's wire so the terminal is not flooded with a listing its reader did
not ask for, and the panel above is where a report is read. A destructive command needs a
confirmation this page does not have yet, and building the button before the confirmation would be
building the accident.

**The frame needed nothing new**, which is the point of having carried `class` before anything read
it. Two things are pinned, and the second is the one worth keeping:

- **The line is the frame's, not the page's.** `the_action_buttons_send_the_frames_own_line` asserts
  over the page's own bytes that the press sends `command.send` rather than a name reassembled in the
  page, that the class filter is what makes a command an action, that the label and the tooltip come
  from the row, and that a refused send puts the header back. The same shape as the switches'
  `/<name> <value>` check, and for the same reason: the stub DOM has no event delivery, so the
  composition is pinned as bytes and the two Node checks pin what is drawn.
- **An action that answers nothing fails.** `every_command_the_page_may_offer_is_one_the_terminal_
  takes` now requires a non-empty answer for the button rows: a button's whole feedback on this page
  is the line it prints, so nothing at all would be indistinguishable from a press that never
  arrived. Mutation-checked — neutering `Live::command`'s push fails it naming `/new`, and it took
  161 seconds, because every read in that test then waits out its deadline (1 second on a green run).
- The two Node checks draw the buttons from a real frame and take them away when a frame has only
  reports in it — which is also the assertion that a panel is not a button.

**Not measured in a browser either**: nobody has pressed one. The click path is pinned as bytes and
the answer path end to end, and that is all it is.

### A report is answered to the page, and not printed here

§8's second class, and the first one the composer's route could not carry. A report *is* a command's
answer, so the cheap route was to send it through `POST /message` like a button — but then the
listing prints on the terminal too, and the terminal is where somebody typed `/help`: whoever is
watching the run would read `/config`'s twelve lines because a browser asked for them. So the same
input channel carries a second kind of line, and the difference is one flag on the terminal.

**The route and the kind.** `web::FromPage` is `Line` or `Report`; `POST /message` queues the first
and `POST /report` the second, into the *same* channel, because they are the same queue of work for
the same loop. `main.rs` adapts them to `InputMsg::Line` / `InputMsg::Report`, which is the only
place the browser's vocabulary meets the keyboard's. `Term::quiet_start` turns on the recording a
command's answer already got *and* turns off the printing; `quiet_take` puts the terminal back
whatever the command did. The answer goes out as a `command` frame with `"panel": true` — one
vocabulary, not two, because it *is* a command's answer and only its destination differs.

**Three decisions worth keeping:**

- **The refusal lives beside the table, not in the route.** The route takes any line, and the REPL
  runs it only if `COMMANDS` has a row whose exact `send` is that line *and* whose class is `panel`.
  Matched on `send` rather than the label because `/delete <n|id>` is a label with a placeholder in
  it: a page that composed an argument would otherwise have the process run it. With no confirmation
  step on the page yet, this is the safety half as well as the drift guard. A refused report is said
  on the terminal *and* sent to the panel, so the page is not left waiting.
- **A report waits for the turn.** Mid-turn, a report is stashed and answered when the model is
  done, where a typed line interrupts: a listing printed into the middle of an answer would be read
  as part of the answer. `Handover` is what a turn leaves the REPL — the line it could not use, and
  the reports the page asked for — and the line goes first, because it is what a person typed.
- **The panel reads one listing at a time and caches nothing.** A press replaces the list with the
  reading and a way back; the frame fills it only when it is the answer to what is on screen, so a
  reader who moved on does not get the old listing under the new one's name. No cache: a listing
  held from a moment ago would be a second copy of a fact the process owns, and asking again is
  cheaper *and* truer.

**Measured, and the measurement is the whole point of the class.** `a_report_the_page_asks_for_is_
not_printed_here` runs a real `--web` process, asks for `/config` on `/report`, posts the same
command on `/message`, and counts: the transcript must carry `config.toml` exactly *once*. Then it
asks for `/new` as a report and asserts the refusal, because a report route that ran whatever it was
handed would delete a conversation with one click that never happened. Mutation-checked both ways —
`quiet_start` not setting the flag gives 2 occurrences and the right message; the e2e run stays under
a second.

**Page side.** Report rows in the panel are pressable, and the press posts the row's own `send` to
`/report` (`askReport`). The panel has two modes — the list, or one reading with a `‹ commands` way
back — and a `command` frame marked `panel` fills the reading rather than the transcript. The
composition is pinned as bytes (`the_panel_reads_a_report_rather_than_sending_it`) and the drawing in
Node (four checks), with the mutation checks recorded in `docs/web-mode.md` §11.

**Not measured**: a report asked for *while a turn runs*, which needs a stub turn slow enough to
click during; and a browser, as ever.

### A selector's values ride on the row, and they are the permission too

`/skills <name>` was the last selector without a control, because the frame had no source for its
options. It has one now: a command row may carry `values`, and `page_rows` fills that one row from
`Agent::skills` — the names this run's system prompt was built with, kept as a field so the state
frame stays a comparison rather than a directory walk per line (`/skills` typed by hand still
discovers fresh, because that is a person asking what is on disk now).

**Two things follow from `values` being on the row rather than in a `providers`-style field.**

- **The page draws one pressable line per value**, composed as `<send> <value>` from the frame's own
  two strings — the same thing a toggle does with `/<name> <value>`, and for the same reason: the
  page never assembles a command out of parts it invented. The row's own line is drawn beside them
  (`/skills` is the catalog; `/skills <name>` is one entry in it).
- **The report route admits a line when the menu offers it**: a panel row's bare `send`, or its
  `send` plus one of its own values. The values are therefore the permission as well as the options.
  `/provider <name>` takes a value too, and running that one quietly would start a local engine
  without printing a word; `/delete <n|id>` stays refused until the confirmation exists.

`/skills [name]` also moved from the selector class to `panel`: it reads rather than changes, and
the page puts it in the group it belongs in. The class is not printed by `/help`, so nothing
user-visible moved with it — the label, the `send` and the description in `/help` are unchanged.

**Measured.** `a_skill_the_run_has_is_readable_from_the_page_and_nothing_else_is` writes a skill into
a scratch `FLINT_HOME`, reads the frame's `values`, asks for `/skills demo` on `/report` (the body
arrives marked `panel`, the transcript never sees it) and then writes a *second* skill after the run
started and asks for that one. The second half is the discriminating case: the command discovers the
directory on every call, so a loose rule would read it, and the assertion is that the frame says
`not a report` instead. Mutation-checked both ways — dropping the values from the frame fails the
first half; admitting any argument that starts with a panel row's `send` reads the late skill and
fails the second.

**Not measured**: whether a page should *also* offer the values of a command whose values the frame
does not carry; nothing does today, and the field is where that would go.

### A form row gets a field, and a credential never comes back

`/name [text]` and `/provider key <key>` are filled in on the page now. A command row may carry
`field` (`text` or `password`), which is both "the page may draw a field for this" and the input's
`type`; the page composes the line as `send` plus what was typed and posts it to `/message`, because a
form changes something and its answer belongs in the transcript. A form row *without* the mark is a
row of reference, which is how `/provider add`, `/provider edit <name>` and `/config edit` stay in the
terminal: they ask for several values, one at a time.

**The redaction is the part with a promise in it, and it is not the page's to keep.** The `command`
frame carries the line that asked for the answer — right for every other command, wrong for this one,
because the frame goes to *every* page connected to the run and stays in the event ring. So
`echoed_input` replaces that line with the row's own `send` (`/provider key`) whenever the row takes a
credential, and it is used in both places a line is repeated: the answer, and `report_refused`, since a
key posted to the read route is refused and the refusal quotes what it refused. The page's half is
smaller: it masks the input because the frame said `password`, and empties it the moment it sends.

**Measured.** `a_key_typed_into_a_field_is_not_echoed_anywhere` runs a real `--web` process, checks the
frame marks the row `password`, posts a fixture key on `/message`, and asserts four things separately:
the answer says `key saved`, the frame does not contain the key, the transcript does not either, and
`config.toml` does. The fourth is what keeps the first three from being satisfied by a command that
never ran. Mutation-checked: echoing the raw line fails the frame assertion with the key visible in
the frame text, and quoting the refused line fails the refusal assertion.

**Not built, deliberately:** `/config edit` as a page form. Its keys are enumerable and its values are
free-form, so a page form would send several settings at once, and the terminal's command prompts for
them one at a time — it would need a `/config set <key> <value>` that the terminal does not have.
Inventing a command for the page's benefit is the thing §8's design exists to prevent. The page
therefore writes nothing into the config file in this round.

### The example and the REPL share one rendering of a turn

The last item on the roadmap's small list, and it was there because the drift had already cost time
twice: `examples/live_turn.rs` kept a hand-written copy of `run_turn`'s event match, so the example
showed a transcript the REPL no longer produced — and a layout judged from the example was judged
against the wrong rendering. Both times the fault was chased in the wrong file.

The event handling moved into `src/sink.rs`: `EventSink` owns what a *rendering* of a turn needs — the
tool calls announced but not answered (so a result can name what it was done to), the calls of the
current round (so the clock moves to whatever is being waited for now), the answer so far (a fragment is
not a line), and whether anything was streamed — plus the status line and the phase labels. The REPL's
loop keeps the interrupt and steering logic, which needs the input channel and the future; everything
inside the closure is now `sink.event(event)`. `waiting`/`named` are free functions as well as methods,
because the loop that escalates a silent wait into "no response yet" cannot touch the sink while the
turn's future holds it.

`examples/live_turn.rs` is 60 lines shorter and calls the same sink. A test refuses the copy coming
back: `the_example_renders_with_the_repls_sink` fails if the example matches on events itself
(mutation-checked — re-inlining an `Event::Text` arm fails it).

The extraction's whole risk was that it changed the output, which is what the byte-exact
`term_capture` suite exists to catch: 20 tests, unchanged and passing. Counts above are 58 `cli_output`
and the same 22 `web_view`; the test count did not move because the example had no test of its own.

### A provider can be added from the page, which is the question §8 left open

Asked straight after the two switches were made pressable: "现在添加provider要怎么在网页上操作呢" — and the
answer was still "in the terminal". `/provider add` was the interactive wizard and nothing else, so the
page could only mark it a row of reference and say so on hover.

Two things had to exist, both named in `ROADMAP.md` when the gap was written down:

- **`/provider add <name> <base_url> [model]`** — the wizard's five answers as the words of one line.
  Bare, it still asks for each part, so the terminal loses nothing. Both ends go through the same
  `save_provider`, because the half that can go wrong is not how the answers arrived: a name that
  replaces an existing provider, and an endpoint that needs a key it does not have. Adding twice is
  refused by name (`/provider edit <name>` is the way to change one).
- **A frame row that carries several answers**, as `fields`: one entry per word the line wants, each
  with the input's kind (`text`/`password`, which is the input's `type`), the argument's name for the
  placeholder, and whether the command works without it. `PageRow` and `CommandHelp` carry `args` now
  instead of a single `field`, so a row and its grammar are one list.

**The refusal is the point**, and it is why this needed a test harness change rather than just markup.
The bare form of a command is a *different* command — `/provider add` with no arguments is the wizard —
so a page that sent a partly-filled form would hand a served run to a wizard whose only reader is a
terminal nobody is sitting at. That is a hang, not a wrong answer. So `formLine(send, fields, values)` is
a pure function that returns `null` when a required answer is missing and the joined line otherwise, and
the submit handler sends only what it returns.

That function lives *inside* the page's handler chain, which the old Node harness could not reach: it
threw listeners away. The harness now keeps them (`addEventListener` records, and a check delivers one
with `fire`) and has a `fetch` spy, so a drawn form's decision can be asserted instead of only its
markup. Nothing in the page depends on any of it — the harness is the only thing that changed.

**Measured**: an e2e test that reads the frame's `fields`, posts the exact line the page composes
(`/provider add claw http://127.0.0.1:9/v1`), and checks that the config gained the provider with the
wizard's default model, that the run switched to it, that a second add of the same name is refused with
`already exists` in the transcript, and that the conversation file gained a `switch` line rather than a
second file — the fix above, reached from the page this time. A Node check for the drawing and for
`formLine` (empty optional left off, required empty means no line). A policy test pinning `command.fields`,
the per-answer input type, the password clearing, the `formLine` call and `if (!line) return;`.

**Both new tests were watched red by mutation, not by writing them first** — the code and the tests
landed in the same round, so the honest substitute was deleting the behaviour: removing
`if (!line) return;` from the page fails the policy test, and making the add not switch fails the e2e
test on "adding a provider did not switch to it". Two traps worth remembering from that: `Copy-Item`
preserves the source's modification time, so a restored file can leave cargo testing the *mutated*
build — touch the file (`AGENTS.md` names this one) — and one policy test was not enough to cover a
deletion, because the assertion about the wiring was in the drawing test and the deletion left the
drawing intact.

### A switch is a line in the session file, not a second file

Reported from the page: picking another provider in the header "automatically creates a new session",
and it did. `continue_conversation` — the path `/model`, `/provider` and `/reload` all take — seeded a
**new** session with the whole conversation copied into it, because `Meta` names the provider and model
and `--resume` believes it. One conversation became two files with the same messages: the sidebar grew
a row nobody asked for, `/sessions` numbered the same conversation twice, `/delete` on one of them left
a twin behind, and resuming either half resumed half a conversation.

**The fix is the argument `usage` already makes for its own numbers**: the file is append-only, so what
changed is a line in it. `{"type":"switch","provider":…,"model":…}` is appended to the file the
conversation is already in, `load` reports the last one, and `--resume` still believes the file.
`SessionWriter::seed` is now only for `--fork`, which really is a copy. `/new` and `/resume` still move
to another file, because that is what they are for. The `viewer.follow` call stays and is now a no-op
for the three commands that do not move — which is exactly what the page wants, since it is what fixed
the view tailing a file nobody was writing.

**Measured**: a lib test (`a_switch_is_a_line_and_the_last_one_is_believed`) that two switches leave the
file with one extra line each and that `load` reports the last; and an e2e test
(`switching_provider_keeps_the_conversation_in_its_file`) that a real `--web` run over a two-provider
home has exactly one `.jsonl` before and after a `/provider`, that it is the *same* file and still
starts with the bytes it had, and that the `switch` line names the new provider and model. Both were
watched red first — the e2e one failed with "switching provider started a second conversation: 2 files
with the same messages in them". The three older `*_keeps_the_conversation` tests were updated: they
asserted the conversation had been carried into a *new* file, which is the behaviour that was wrong.

`docs/session-format.md` documents the new event and says what `meta` means now (where the session
*started*), including how to change a session's model by hand — append a `switch`, do not edit the first
line.

### The switches are pressable, and a plain row says why it is plain

The second and third things asked for after using the page. `/provider <name>` and `/model <name>` take
an argument out of a list the run already knows — the same list that fills the header's pickers — and
the panel drew them as rows of reference, so switching meant reading a name off one control and typing
it into another. Both carry `values` now (`page_rows` takes `cfg` and the provider config for exactly
these), and the page draws one pressable line per value.

**The near-miss is the part to keep.** `values` had meant "a read the page may ask for", and the report
route runs its command with the terminal **quiet** — for `/provider llamacpp` that is starting a local
engine and printing nothing anywhere. So the *class* routes the line now: a `panel` row's value is a
read, and a value on any other row is typed through `/message`, where the answer lands in the
transcript next to the change. `/model [name]` had to split into `/model` (a report) and
`/model <name>` (a selector) for the same reason. `tests/cli_output.rs` asserts that `/report` refuses
`/provider other`: the page honoring the rule is not the same as the process enforcing it.

**A reference row is now marked and dimmed, and says where its control is.** The panel had drawn them to
look exactly like pressable rows, deliberately; the first person to use the panel asked why some rows
press and others do not, which is a puzzle rather than a list. `COMMAND_HOMES` names the three homes
(the header, the list on the left, the terminal) and each entry is the page's own fact about where *it*
put its controls. A `selector` that carries values never reaches that branch.

**`/provider key`'s sentence names the provider.** `/help` says "the active provider", which a browser's
reader cannot resolve; `page_help` substitutes the name, and `tests/cli_output.rs` allows the frame's
help to be the table's sentence or that sentence with the name filled in — never a second description
that can drift from `/help`.

**Left open, deliberately**: adding a provider from the page. `/provider add` asks four questions one at
a time, and a page form can send exactly one value, so doing it from the browser needs a
non-interactive `/provider add <name> <base_url>` *and* a frame row carrying several fields — the
second is the same shape §11 refused for `/config edit`. The panel's grouping is also §8's classes
rather than a task, so "set a key" means switching provider in one group and filling a masked box in
another. Both are the next round's question. **Not measured**: a browser, as always.

### A conversation's row carries its own actions

The one part of the page asked for **after using it**, and the first thing this session added that no
plan asked for: archiving or removing a conversation meant opening the command panel, finding the
destructive row there and reading the number off the sidebar by eye. The row *is* the conversation, so
each row now carries a `⋯` button that opens a short menu — the panel one level down, the shape DSH
uses — and the rows in it print the whole line they will send (`/delete 3`) before they send it.

**Built from the frame, through the helper the panel also uses.** `destroyingRows(doc, from)` is now
the single place that reads "a row that destroys something and says where its argument comes from", so
the panel's choice lists and the sidebar's menu cannot disagree about what may be sent at another
conversation. The page still does not contain the word `/delete`.

**`doc.menu` is named by the conversation's id, not its number**, and is cleared by a `reset` and by a
list re-read that no longer holds it. The argument is `doc.confirm`'s, one level down: the numbers in a
menu are positions, and a sidebar that shifted under an open menu would aim the next press at the
conversation below the one that was meant.

**The conversation you are in is offered the actions too**, and the terminal's refusal is the answer
("/new starts a fresh one; then this one can be filed away by its number"). Hiding the row would be the
page deciding a rule the terminal owns.

**Measured**: `sessionRow(doc, session)` is exported and driven under Node with a hand-written state
frame — the row's three children, the `⋯` button's text and title, the menu's two `row danger` items
reading `/archive 3` and `/delete 3` (the frame's `from: "providers"` and `selector` rows are *not*
there), a menu whose id is another row's drawing nothing, and an empty frame drawing `nothing to do
from here`. `tests/web_view.rs` pins the composition over the page's own bytes, including that both
the class and the `from` are required. Mutation-checked: asking `destroyingRows` for `"providers"`
fails both the Node check and the policy test.

**Not measured**: a browser, as ever — so the menu's placement, and its dismissal (the button toggles
it, a `reset` clears it, and a click elsewhere does **not** close it) are reasoned rather than seen.
That dismissal is the thing worth watching first when someone finally looks at this page with a mouse.

### `--fork` copies a conversation instead of continuing it

Second item off the roadmap's small list, and the one that was described as "`cp` already does this;
the flag is about making it discoverable" — which is exactly the shape it took. `--fork` names a
session the way `--resume` does (list number, id prefix, path, or nothing for the most recent), loads
it, and then does the one thing `--resume` must not: it makes a *new* file and continues there. The
original is never opened for writing, and the e2e asserts that as bytes rather than as intent.

**The file is seeded, not left empty.** `session::SessionWriter::seed` already existed for the
`/model` bug — a switch that moved the conversation into a new file — and a fork is the same problem
with a friendlier name: a run whose context held the conversation and whose session did not would be a
transcript on screen that no file contains, and a page tailing a session that begins mid-sentence. So
the fork goes through `seed`, which also carries the conversation's `title` line.

**Combining `--fork` with `--resume`/`--continue` is refused**, naming both. All three answer "which
file does this run write"; picking one silently is how an afternoon's work lands somewhere unexpected.

**Measured**: `a_forked_session_is_a_copy_and_the_original_is_untouched` drives the real binary with
`--fork 111-1 -p "now branch it"` against the stub provider, then asserts the original's bytes are
identical, that exactly one new `.jsonl` exists, that it holds the copied question, answer and title
*and* the new turn, and that stderr names both files. `forking_and_resuming_at_once_is_refused` pins
the conflict. Both were watched red by removing the flag from the parser — the run answered `unknown
flag '--fork'` — which is the honest red for a flag that did not exist a moment before.

`README.md` gained the two command lines and a paragraph; the roadmap item is struck.

### `file_path` is the name the file tools ask for, and `path` still works

The first item off the roadmap's small list. `read`, `write` and `edit` now declare `file_path` — the
name a model reaches for when a tool is shaped like the file tools it has used elsewhere — and
`require_path` accepts `path` as an alias, which is what every earlier version of flint asked for. The
schemas say so in the description, so a model that reads them learns both names from one place, and
`flint debug prompt-input` shows the same sentence.

**Two names that disagree are refused, not resolved.** `{"path": "a", "file_path": "b"}` is an error
naming both; picking one would be flint choosing which of two contradictory instructions was meant,
and the wrong guess writes a file where nobody asked. A wrong *type* still names the argument the
caller used (`{"path": 5}` says `path`), because that message is the one that tells a model what to
fix.

**`list`, `glob` and `grep` keep `path`** — theirs is a directory or a place to search, not a file to
read or write, and the roadmap item named these three tools.

Measured by `a_file_can_be_named_by_either_spelling` in `src/tools.rs`, watched red first (the old
schema answered `missing required string argument 'path'` for a call that used `file_path`). It asserts
both spellings find the same file, that `write` and `edit` take the new name, that a disagreement is
refused naming both arguments, and that the `read` schema's `required` list is `["file_path"]` with the
alias named in its description. The existing tests that call these tools with `path` are the other half
of the measurement: they pass unchanged, which is what "alias" has to mean.

### A line already waiting no longer erases the question it interrupts

§9's last open hole, and the one whose note in the roadmap said a test could not be written for it.
`run_turn` read the input channel *before* polling the turn future, and the turn's `user` message is
pushed by that first poll — so a line that was already in the channel when the turn began was taken as
an interrupt, the future was dropped unpolled, and the question was in the history and in the file
nowhere. Nothing failed; the command that had been waiting simply ran as if the question had not been
asked. The window is milliseconds wide, which is why it was found by accident while measuring the
`/model` fix and left alone then.

**The fix is the ordering, made a fact rather than a window**: the turn is polled once with
`std::future::poll_fn` before the loop starts reading input, so the question is in the history before
anything can pre-empt it. `std` only — no new dependency, and no timer, so a slow machine cannot
reopen the hole.

**The note was wrong, and that is the useful part.** It said a test would be a race with the reader
thread. There is no reader thread in the test: put the line in the channel *before* calling `run_turn`
and you have the exact state the old order got wrong, without waiting for anything. That is
`a_line_that_was_already_waiting_does_not_erase_the_question` in `src/main.rs`'s own tests — the second
test in that module, and the first async one. It binds a listener that accepts a request and then says
nothing (the shape §9's own measurement used), queues `/model stub-other`, runs the turn, and asks the
agent's history whether the question is in it. Watched red first: the history held the system prompt
and nothing else. It also asserts the queued line was handed back to the REPL rather than executed as a
steer.

**One consequence, on purpose**: a queued `/stop` now lets the request go out before the stop stops the
turn. A question that was asked is worth one wasted request, and that is what the fix is for.

### A destructive row opens its choices, and the second press is the one that sends

`/delete <n|id>`, `/archive <n|id>` and `/provider rm <name>` are page controls now, and the shape of
the control *is* the promise: the row does not send — it opens its candidates, and the candidate row
prints the whole line it will send (`/delete 7`), so the second press is one that can be read before
it is made. There is no undo anywhere in flint for a page to offer, and a one-press `/delete` is the
misclick §8 keeps `/exit` off the page for.

**The frame had to say one thing it otherwise never says: where a value comes from.** §8's rule for
selectors is that the control decides and the command list does not say. That holds while the control
is one control. It does not hold here, because the page must draw the choices *before* anything can be
confirmed, and the two lists are different ones — conversations by the sidebar's numbers, providers by
name. Without a mark the page would have to tell `/delete <n|id>` from `/provider rm <name>` by
reading the commands, which is what every other control is built to avoid. So three rows carry `from`
(`"sessions"` or `"providers"`), and the e2e that pins it counts them: exactly three, because a `from`
anywhere else would have the page offering candidates for a command that reads.

**`doc.confirm` is the row, not a countdown.** Nothing has been sent while the choices are open, so an
armed state is harmless and a timer would be a second thing to get wrong. It is cleared by the way
back, by a `reset` (a choice list is aimed at a document, and its numbers are positions), and the
sidebar's own re-read redraws an open list — `/delete` in the terminal shifts every number below the
one that went, and a stale number is a wrong deletion.

**Measured.** `a_destructive_row_says_where_its_argument_comes_from` reads the frame off a real
`--web` process; `a_destructive_row_opens_its_choices_and_sends_on_the_second_press` pins the page's
half over its own bytes, including that the press that opens the choices contains no `sendText`; and
the Node check drives `showCommands` through both states and reads back the rows — the candidates, the
way back, the destructive mark, and the empty list saying so. Mutation-checked: marking every row
`from: "providers"` fails the e2e, and drawing no choices fails the Node check.

**Not measured**: a browser, as ever — and the deletion itself, which is the terminal's own command
reached through the composer's route. What the page adds is the two presses, and what stops a single
press is that there is nothing to press that sends.

### Still owed on the page

**§8 is built, so this list is now the residues rather than a class.** The command list, its panel, the
buttons, the reports, the selectors, the forms and the destructive controls are all in. What is left,
each with the reason it is left:

1. **`/config edit` as a page form** — deliberately not built: its keys are enumerable and its values
   are free-form, so a page form would send several settings at once while the terminal prompts for
   them one at a time, and it would need a `/config set <key> <value>` the terminal does not have.
   Inventing a command for the page's benefit is what §8's design exists to prevent.
2. **The mid-turn wait for a report** — the behaviour is built (`Handover` stashes it rather than
   treating it as an interrupt) and asserted nowhere, because the assertion needs a stub turn slow
   enough to click during.
3. **A browser** — nobody has opened this page with a real font and a real click. That is what §11's
   "not measured" lines keep saying, and it is the one gap that a test cannot close.
4. **Renaming from the sidebar** — a `/name` field exists in the panel and works; the sidebar has no
   affordance for it. Small drawing job, not a mechanism.

- The small queued-line hole above still wants its two structural lines before a test can hold it.
  The report path now leans on the same machinery and does *not* have the hole: a report arriving
  mid-turn is stashed rather than consumed as an interrupt.

**The Windows plan is finished, and the measurements are the useful part.**
`docs/windows-tooling.md` had been "settled and unimplemented" for several sessions: a
labelled design waiting for a Windows machine. This round was on one, so the five ordered
steps from its §7 are all in the tree, in eight commits — plus the two defects that only
turned up once something was actually run:

- **A quoted command never reached `cmd`.** Measured: `echo "hello"` arrived as `\"hello\"`,
  and `dir /b "C:\Windows\System32\drivers\etc"` failed outright with "The filename, directory
  name, or volume label syntax is incorrect." std quoted the argument the way the C runtime
  does, and `cmd` does not read a command line that way. Fixed with `/S /C` and the command
  handed over verbatim through `raw_arg`, which is now one function
  (`tools::apply_invocation`) because the same dance was about to exist in three places.
- **GBK output became U+FFFD** — and the plan's own remedy did not survive contact: `chcp`
  changes *shared console state* (the same terminal reported 936 and 65001 within one session)
  and Python ignores it. The fix decodes with the locale ANSI code page (`GetACP`) instead,
  which is what CPython uses and what `chcp` cannot move.
- **`/web`'s browser line was broken** for the same reason as the first item: a URL has no
  spaces, so std did not quote it, so `cmd` read `?a=1&b=2` as two commands. `start ""` was
  right all along; the URL was the bug.
- **File names Win32 rewrites silently** are now refused: `NUL` wrote nothing and said
  "wrote 5 bytes", `trailing.` created `trailing`, `a:b.txt` created an alternate data
  stream. `CON`, `NUL.txt` and a 1619-character path all work here, so the refusal is exactly
  the silent cases and nothing else.
- **A `\n` edit against a CRLF file** now says so in the refusal instead of "old_string not
  found", which cost a turn every time and was the one place the fix is a sentence rather
  than a mechanism.

The new tools are `exec` (a program and its arguments as an array) and `pwsh` (a script
written to a BOM'd `.ps1`, run with `-File` and `-ExecutionPolicy Bypass`) — both measured
before being written, and the BOM and the policy switch both turned out to be required rather
than prudent. The system prompt now states which PowerShell is on the machine and what it does
with native arguments, which is the fact that stops a model writing `??` on 5.1.

**Recorded rather than fixed**, because both need a test before they need code: a Unix
process-group kill for work a command backgrounds (there is no process group and no `setsid`,
and closing it means `libc`), and the same line-ending sentence for `apply_patch`.

**The terminal side was measured too** (`docs/windows.md` §1–§3, all four items of the
checklist at its end). Two things came out of it that the next session should not re-derive:

- **§1's prediction is wrong.** The `DISABLE_NEWLINE_AUTO_RETURN` bit is clear, as the file
  said — but a linefeed at the last column then lands at column 0 of the next row, one row,
  exactly where `\r\n` lands. Measured in a private hidden console (`FreeConsole`,
  `AllocConsole`, `ShowWindow(SW_HIDE)`), through both the wide and the byte write path. No
  double advance, so `insert_history` is fine and the remedy §1 argued for is not needed.
- **The console is not mangling flint's output, measured end to end.** Run in a hidden console
  at output code page 936 with its screen buffer read back, `flint --readonly` prints
  `readonly <U+2014> writes and mutating commands are refused` — the em dash intact. The same
  three bytes through the byte API become U+9225 in the same console, so the difference is the
  writer: Rust's standard library writes text to a console as UTF-16, so the code page never
  applies, for any path flint uses (including a 10,500-byte single CJK write). Earlier in this
  stretch the opposite was written down here — that the mojibake was real and the fix measured
  to work — on the evidence of the byte-API experiment and PowerShell's own UTF-8-through-CP936
  file reads. Both are real, and both belong to other programs. **`SetConsoleOutputCP(65001)`
  is not needed**, and that is no longer an open decision.

The method is worth keeping: a private console is how Windows terminal behaviour gets measured
without a human watching a window and without writing test bytes into somebody's terminal.

**One side effect worth knowing about**: measuring the browser launch opened two real browser
windows on the desktop before the harness was rewritten to stand a `.cmd` file in for the
browser. Nothing was damaged, but the lesson is general — a measurement that launches the
thing under test can do it for real.

**The step 7 measurement pass, and the four defects it found.** `docs/web-mode.md` §11 has the
numbers; the short version is that three of the four had one cause — `paint` rebuilt the whole
transcript on every frame, so every node on screen was a new node, and the scroll position, which
`details` were open and any selected text went with the old ones. **Selecting the answer is the
promise this page exists to keep**, and a full repaint cannot keep it. The paint is incremental
now: a `rev` per block, nodes reused when the revision has not changed, and the expansion state
kept in the block so even a rebuild restores it.

**The fourth was worse and had a different cause.** A `reset` in the middle of a turn *destroyed*
what the page was showing — measured at 1,424,691 characters on screen, 64,999 after — because
`/session` serves the file and the file gets the assistant message when the turn *ends*. Two
mechanisms now cover it: the page carries the streaming block across its own re-read (only when
the file's copy is a prefix, so a finished turn cannot be duplicated), and the server sends the
answer so far as a named `answer` event — state, like `status` — on connect and after a lagging
reset, which is what a page *loading* mid-turn needs. Verified: 280,522 characters on screen
against the 279,900 the model produced.

**The limit, recorded rather than fixed:** painting is still quadratic in the size of the answer,
about 85 KB/s rendered when small and 17 KB/s past a megabyte, because the browser lays out one
very large text node on every frame. A real model emits ~300 bytes a second, so it is two orders
of magnitude from mattering; the fix would be to append to the text node instead of rewriting it.

**And the harness matters as much as the result.** Two bugs in it each looked like a flint bug
first: a driver that stops reading the pty **deadlocks the program under test** the moment it
writes a streamed answer, and a measurement script that kept typing interrupted the very turn it
was measuring. Both are written into `docs/web-mode.md` §11 so the next person does not pay for
them again.


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

**Three things the first real sessions with the browser page turned up** are written down in
[`ROADMAP.md`](ROADMAP.md) §8 rather than here, because the middle one is a design question and
not a defect: a conversation that has not happened yet shows in the sidebar as `(empty)`, a command
typed into the composer prints nothing (its output goes to the terminal and nowhere the page can
read), and renaming a conversation from the sidebar has no affordance. None is started. A fourth
from the same sessions -- history tidied in the terminal leaving the sidebar stale -- is fixed,
because the numbers in that list are positions and a stale row resumes the wrong conversation.

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

**Windows terminal behaviour has now been measured, in a private console.** Read
[`docs/windows.md`](docs/windows.md) first — it has the mechanisms, labelled by what was
measured versus reasoned, and §1–§3 are all measured as of the session that finished
`docs/windows-tooling.md`. The short version: the layout is built on terminal *behaviour*
(scroll regions, absolute row addressing, what a newline does at the bottom margin),
`crossterm` sets only `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and never
`DISABLE_NEWLINE_AUTO_RETURN` — the bit that decides whether `\n` also returns the carriage —
while `insert_history` counts on `\r\n` advancing exactly one row. Both of those were settled
rather than reasoned: the bit is clear, and a linefeed at the last column still advances one
row, so nothing there is broken. The console output code page is never set, and that turned out
not to matter either, because Rust writes text to a console as UTF-16.

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
2. **The transcript as cells** — steps 1 and 2 are done. The measurement found one 40-line
   answer streamed in 256 deltas painting **10×** the characters it contains (about 78
   characters of waste per delta) for a screen identical to the one a single delta produces;
   after step 2 the same answer costs **2.2×**, and the second number is now the floor rather
   than a shortfall: the transcript is painted once (a constant 2,158 characters) and the strip
   paints the answer once as it streams, because every row really is drawn twice — as the
   visible tail, then again when it scrolls out into the transcript. `ROADMAP.md` §6 has both
   tables, and `tests/term_capture.rs::streaming_in_many_deltas_paints_only_what_changed` gates
   the part that matters (four times the deltas, under 1.25× the paint). The tool is
   `cargo test --test term_capture -- --ignored --nocapture measured_cost_of_streaming`. Step 3,
   re-render on resize, was **measured before it was written and half of it is already true**: a
   resize re-wraps the strip correctly in both directions, because the painter writes the new
   row from its first differing character and erases what the old row had past the end — so it
   overwrites a row remembered from another width instead of trusting it, and no width tag is
   needed. The test written for it was dropped after three mutations all failed to make it fail
   (`ROADMAP.md` §6 has the detail). Step 3 has started: a resize now **closes the in-flight
   answer first**, while the width it was drawn at is still in force, so the transcript gets what
   only the strip had, the strip is emptied, and the next fragment starts a fresh segment at the
   new width (`a_resize_closes_the_answer_that_was_still_arriving`; re-wrapping instead would
   commit rows against a `committed` count measured in the old wrapping and duplicate text in the
   scrollback). What step 3 still owes is the **consolidation**, and `ROADMAP.md` §6 now records
   what it has to keep, because two of the four pieces cannot be deleted: `segment_text` (what
   actually arrived) is not the concatenation of the handed prefix and the drawn text — the strip
   drops the leading blank space and may not fire at all — and `committed` has to stay while
   answers commit *into* the transcript as they stream, which is the feature that makes a long
   answer readable while it arrives. So the rewrite is one model owning the strip's rows, the
   answer row the first holds, the arrival, the drawn text, the handed prefix and the committed
   count — six fields behind three locks whose agreement is kept by hand across four functions —
   rather than four deletions. The one trap to design around: with a single mutex, no guard may
   live across the `close_stream` / `clear_viewport` calls that `stream` makes (it takes
   `last_segment_text` at line ~1122 and `segment_text` at ~1146 today, both dropped before those
   calls), so the frame has to snapshot under the lock, emit with it released, and relock to
   store. Also worth knowing: `clear_viewport` now owns clearing the painter's memory of the rows
   it erases — that came out of `stream_rows` in the last session, and a mutation removing every
   such clear still passes all twenty terminal tests, so it is a simplification, not a fix.

   **Where every one of those fields is touched** (checked while mapping the merge, so nobody has
   to map it again; line numbers drift, the functions do not):
   **Both groups are now merged**, and what that leaves is the point of the exercise:
   `stream_rows` + `stream_first` are the `rows`/`first` pair inside `Mutex<Strip>` (taken as a
   pair in `stream`, cleared in `clear_viewport`), and `stream_text` + `segment_text` +
   `last_segment_text` are the `drawn`/`arrived`/`handed` trio inside `Mutex<Answer>` (written as
   a pair in `stream`, `drawn` taken by `close_stream`, `handed` cleared in `begin_answer` and
   appended to in `close_stream`). Still free-standing: nothing.
   `committed` and `stream_active` were the last two, and they are inside `Mutex<Answer>` now,
   with the text they measure. **So the strip's state is two locks and no free-standing fields**:
   `strip` for what is on screen, `answer` for what was said and how far it has got — which is the
   merge ROADMAP §6 asked for, done as a move rather than a deletion because every one of the
   eight turned out to be load-bearing. The one change beyond the move: `close_stream` takes the
   drawn text and its row count under a single lock, because the count measures that text, and
   sampling them separately is how one frame's text gets measured against the previous frame's
   rows — the rows in between are then either committed twice or dropped. The two sites that used
   to hold a guard across a region are both scoped.
3. **Web mode** — `--web` as a window onto the running process rather than a mode. The first
   three steps need no decision, and the one open question (§7 of that document: a hand-rolled
   HTTP server or `hyper`, which is already in the tree via `reqwest`) blocks only step 4.
   The next web work is §8's **commands and config from the page**: the page sends the command
   *line* over the channel the composer already uses; the `state` frame it needs for options
   **is built** (provider, model, what each provider offers, the toggles, and now the command list
   with §8's classes — see above); the provider and model pickers and the toggles are the first
   controls drawn from it, and the command list is drawn as a panel of reports; and a
   command's *output* now reaches the page too (the `command` line built two rounds ago), so
   nothing structural is left between here and the buttons. What is left is a control per class:
   buttons for the no-argument actions (**built** — see above), selectors, forms, and a
   confirmation step for the destructive ones — the decisions taken for that unit are listed
   under "Still owed on the page" above. `config.toml` writes are allowed because the per-run
   loopback token already covers them, and `/exit` stays off the page.

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
