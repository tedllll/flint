# Agents as processes: spawning, finding, and talking to peers

**Status: stages 1, 2, 3 and 4 are built; the MCP and Python doors are built.** — a running flint writes
a presence record and `flint who` reads it (`src/live.rs`,
`tests/who.rs`), a running flint can start another flint with its `task` tool and start several at once
with `tasks` (`src/tools.rs`, `tests/task.rs`), a profile in `.flint/agents/` says how a child should
start (`src/context.rs`), a peer can leave it a message with `flint say` that is shown to the person
and sent to a model only when that run asks to hear peers (`tests/say.rs`), so *being called*, *seeing
each other*, *spawning*, *talking*, *naming a way to work* and *handling a run nobody waited for* all
work today. The `.flint/` marker that lets two `FLINT_HOME`s see each other is built as well (see
"Presence lives under `FLINT_HOME`" below and decision 5), so what stage 3 promised it now all does. The
five decisions at the end of this file **were answered on 2026-09-16: every recommended value was
adopted**, and each is marked below with what that means for the code.

Written because of three questions asked directly, and because the same afternoon produced the incident
that makes the middle of this document concrete: two agents were working in this checkout at once, one
of them was writing `docs/sandbox.md`, and the other swept a half-written revision of it into a commit
with `git add -A`. Nothing was lost. Nobody could have known.

## The one idea: there is only a run

A subagent, a call from Python, a call over MCP, and "a background process" are not four features. They
are four doors onto one thing:

> **A run** is one flint process: a session file that records it, a presence record that says it is
> alive, a mailbox it can be spoken to through, and an exit code when it ends.

That is the whole vocabulary. Everything below is a consequence of taking it seriously, and the reason
to take it seriously is that it is already true: flint has never needed to be told it is a subagent to
be started by something else, and `examples/python/flint_call.py` has been spawning runs since it was
written. The only thing missing is that the runs cannot *see* each other and the parent cannot *handle*
them.

**So the word "subagent" is not load-bearing here.** What would make flint's use of another flint
different from Python's is nothing at all — same binary, same interface, same stream, same exit codes.
That equality is the design, not a coincidence: a feature that works from outside and not from inside
(because the insides use a private channel) is a feature with two behaviours to keep in step.

## The three doors

| Door | Who opens it | State |
|---|---|---|
| `flint -p … --json` over a pipe | Python, a shell, an editor, another agent's shell tool | **Built.** `examples/python/flint_call.py`; every fact needed to branch is on the stream |
| an MCP tool call | Codex, Claude Code, Cursor | **Built.** `examples/mcp/flint_server.py`, one tool, stdio |
| a `task` tool inside flint | flint itself | **Built.** `src/tools.rs` (`TaskTool`), `tests/task.rs`; one child per call, `tasks` for several at once, and a handle by default -- pid and the conversation the answer is being written to -- with `job_op` to look at, collect or stop a job nobody waited for (`background: false` is the door that waits). A background *command* (`bash`/`pwsh`/`exec` with `background: true`) is the same handle, the same three verbs and the same one notice, with a log file where a child has a conversation |
| a profile in `<project>/.flint/agents/<name>.md` | a person, once; a model or a person afterwards | **Built.** `src/context.rs`; the same child, started with instructions, a model and `readonly` already decided |

The third door is the first two in Rust. It runs `std::env::current_exe()` with the same arguments,
reads the same stream, and returns the same facts — which is the argument for building it *that* way
rather than as an internal function call: a child that is a process is a child whose death, timeout,
output size and exit code are already solved by the operating system and by `bash`'s existing
machinery, instead of by new code that would have to invent all four.

What makes it a tool rather than "the model can already do this with `bash`":

- the answer comes back **structured** (the `result` object beside the prose) with the exit code, the
  outcome, the cause and the child's session path attached — a shell pipeline gives a blob;
- `readonly`, the model and the provider for the child are chosen by **flint**, in the tool's schema,
  not typed into a command line by a model that may get it wrong;
- **`readonly` is monotonic**: a `readonly` run may not spawn a writing child, whether or not the model
  tries. That is the property that makes "an explorer subagent" mean something;
- **depth is bounded**: `FLINT_DEPTH`, maximum 2, set by the tool for its child and by nothing else. It
  was unbounded until this was built, and nothing stopped a flint from starting a flint that started a
  flint: the bill is real, and so is the wall clock;
- the child's session file is named in the result, so a value can be traced to the conversation that
  produced it and a human can read what actually happened.

## What can be seen, and what cannot

This is the part to get right before writing any of it, because the failure that matters is a **false
negative**: two agents editing one file is the accident, and "no other agent here" being wrong is how
you get it.

| Signal | Precision | What it may honestly claim |
|---|---|---|
| A presence record written by a flint | exact, for flint | "a flint run started 04:12:33 is alive in this directory, last heartbeat 8 s ago" |
| `git status` plus file mtimes | vague, universal | "6 files in this directory changed in the last 4 minutes" — **not** "an agent did it" |
| Session file mtimes | vague, flint-only, works after the fact | "a flint conversation in this directory was written to 20 s ago" |

Two rules fall out of the table:

1. **Both signals are reported, and they are labelled differently.** `flint who` prints the live runs
   it knows and then a separate line about recent changes that names no author. Collapsing them into
   one "agents here: 2" would be a lie in both directions.
2. **Presence only sees flint.** The other writer in the incident above was probably not flint at all.
   That is why the vague signal is not a nicety: for every non-flint agent on the machine it is the
   only signal there is, and it is worth saying out loud that it cannot distinguish an agent from
   your own editor's autosave.

Presence lives under `FLINT_HOME`, next to the sessions it describes, because a `readonly` run must
still be able to announce itself and a project directory is not always writable. That home record is
still the one that must exist; what was missing was that **two flint installations with different
`FLINT_HOME`s cannot see each other**, and the `.flint/` marker in the project is now built to close it.

**The marker is a directory that flint never creates.** A project that keeps a `.flint/` — for a skill,
a profile, or a bare `mkdir .flint` — is a project that has opted into flint's project-local state, and
a run in it writes a *second* copy of its presence record to `<project>/.flint/live/`, with the same
file name (pid plus nonce), and removes both copies on the way out. `flint who` reads both directories
and counts a name it has seen once, keeping the fresher of the two copies: two files, one run. The
opt-in rule is what makes this safe to look for on every run — a checkout that never asked gets nothing,
not even an empty `.flint/`, which is held by a test that starts a real run in an unmarked directory and
asserts the directory was not created. The cost is the one this section always named — **flint writes
into somebody's checkout, but only one that asked** — and the honest residue is that a project which
never creates `.flint/` still cannot be seen across installations, which is now a choice a person can
make in one command rather than a missing feature.

Two directories are refused even when they are named `.flint`, and the reason is worth keeping: the walk
stops at the home directory, because `~/.flint` is `FLINT_HOME`'s default and belongs to every project
and none. Without that rule every run under a home directory would find the marker, and `flint say`
would write one mailbox for the whole machine.

A record that is not cleaned up (a killed process cannot clean up) is not an error: every record
carries `last_seen`, and anything older than a fixed window is reported as *stale* rather than as
alive. Nothing about this may depend on pid liveness, which is not portable and would make the answer
depend on the machine rather than on the file.

## Talking: the mailbox

`flint say` appends one line to a mailbox, addressed to a session, to a directory,
or to whoever is listening. The file is `<FLINT_HOME>/mailbox/<dir-key>.jsonl` for a directory that is
not part of a project, and **`<project>/.flint/mailbox.jsonl`** for one that is — one mailbox for the
whole project, so a run in `src/` and a run at the root can hear each other, and so can two
installations with different homes. The project's file *replaces* the home's rather than being written
beside it, and that is the one place the mailbox differs from the presence record, which is written to
both: a record is state, deduplicated by file name, while a mailbox is a log of events, and one message
in two logs would be shown twice by a run that can see both with no way to tell a duplicate from somebody
saying the same sentence again. A running flint already polls a channel every turn — the stdin reader
that takes `/stop` — so a mailbox is one more source on a mechanism that exists, not a new lifetime, not
a socket, and not a server. The record stays append-only plain JSONL, so a human can read it, and so the
conversation between two agents is on disk like everything else in this project.

**The safety rule, which is not negotiable and is the reason this is stage 3:**

> A peer's words are **shown to the human, not fed to the model**, unless the run was started with an
> explicit opt-in.

Anything that can write the mailbox could otherwise direct the model's actions, using the tool loop of
a process that has no permission layer, to do whatever it likes. That is not "prompt injection" as a
metaphor; it is the widest hole in the system, and it would be created by a feature whose only purpose
is convenience. So: `peer.message` appears in the transcript and in the session file; turning a peer's
message into something the model reads is a decision the *person* makes (a flag, a slash command, a
configuration line) and it is recorded when it happens. `ROADMAP.md` C4 already records the general
form of this ("injection is action injection") — this is the same fault with a friendlier face.

The useful messages are less chatty than "interjection" makes them sound, and the vocabulary should
say so:

- **a claim**: "I am editing `src/provider.rs`" — advisory, never a lock. flint has no lock and should
  not grow one: a lock that is not airtight is worse than a documented absence, which is the same
  argument `ROADMAP.md` already makes about permissions.
- **a boundary**: "please do not commit `docs/sandbox.md`; I am still writing it" — the exact sentence
  missing from the incident above.
- **a handoff**: "I have stopped; the tree is yours."
- **a question** and its answer, which is what a mailbox is normally for.

## Background is the same record

"Start this and let me carry on" needs no new machinery once presence exists: a run started with
`background: true` returns a **handle**, and the handle is the presence record — the session id that is
already the run's name. `task_status` reads it, `task_wait` waits on it (with the caller's own
timeout), `task_stop` writes `/stop` to it, which is the graceful stop the MCP server already uses, and
`flint who` is the same listing a human sees. Nothing here is derived state and nothing is a cache:
kill every process on the machine and the records still say what was running, which is exactly the
property the repository insists on.

Two consequences worth stating: a background run's cost lands on whoever pays for the provider, so its
`usage` is part of the handle rather than a secret; and a background run whose parent has exited keeps
its session file, which is the same provenance rule as everywhere else.

**Measured, and the reason to build the handle next:** a `task` child already *is* a background run in
everything but the handle. When the parent's turn was interrupted (a bug report, 2026-09-16), the child
kept working for minutes with its own session file and its own bill, while the parent's record said the
tool never ran and offered to do the work again. Both halves of that are now fixed from the parent's
side — it forwards the child's progress, and it records the child's pid and session instead of denying
that it started — but neither is a substitute for the three verbs: `task_status` to ask, `task_wait` to
collect, `task_stop` to stop paying. Until then, "collect" means reading the session file the note
names, and "stop" means stopping that process by hand.

**Built, and the sketch above was wrong in one place, which is the part worth reading.** The handle is
*not* the presence record, and the reason is mechanical rather than a preference: `/stop` is a line
written to the child's **stdin**, and only the process that spawned a child holds one. A presence record
can say a run is alive; it cannot carry a pipe. So the handle is the **job** — the parent's own record
of a child it started, held in memory (`CHILDREN` in `src/tools.rs`) next to the child's stdin, the
frames its reader has absorbed, the session it named and the exit status once it ends. It is live
process state, not derived state and not a cache: it is exactly the set of facts that stop existing when
the process that owns them exits, and nothing on disk pretends otherwise.

What presence is still for is the *looking*, and it is what makes the scope honest. `job_op` with
`action: "status"` lists the jobs this run started; asked about a pid it did not start, it reads the
presence records and answers with what those say — directory, endpoint, `readonly`, the conversation it
is holding — and says plainly that it can be seen and not waited for or stopped. A person gets the whole
listing from `flint who`.

The three verbs became **one** tool with an `action`, not three tools. Every tool schema is re-sent with
every request of every conversation, and three verbs about one child do not deserve three schemas; it is
deliberately *not* folded into `task` either, because a call that sometimes answers and sometimes hands
back a receipt is one whose result a model has to guess at. The names in the sketch (`task_status`,
`task_wait`, `task_stop`) are therefore `job_op` with
`action: "status" | "output" | "wait" | "stop"` -- the fourth added when a background *command* joined
the record, because a log can be read while the thing is still writing it and a child's answer cannot.

Three things fell out of building it that the sketch did not anticipate:

- **The timeout had to move off the waiter.** A background child has nobody waiting, so a
  `timeout_secs` that only the waiter enforced would have been a budget that never came due. Each child
  now gets a detached supervisor: it waits for the exit, writes `/stop` when the budget runs out, kills
  twenty seconds later if that was ignored, and records the exit status for whoever asks — later, or
  never. A foreground `task` is now only a wait on a job that is already being watched, which is why a
  `task` child whose turn was dropped still ends when its budget says.
- **The child's stdin outlives the tool call.** It is kept in the job rather than inside the future that
  spawned the child, because that future is a returned tool result by the time anyone asks a child to
  stop. This is also why a background child no longer sees its stdin close when the call returns.
- **A caller's pipe is not the parent's exit.** Measured while testing this: the parent process was done
  in 0.4 s, and the caller's read of its stdout hit EOF at 8.4 s — when the child it had started
  finished. That is not a flint bug and not a Windows one: a spawned process inherits the standard
  output its parent was given (fd 1, or `hStdOutput`), so **anything that reads a run's stdout to EOF is
  waiting for the whole process tree, not for the run**. flint's own `bash` tool has always worked this
  way — a backgrounded command keeps the tool waiting on the pipe — which is why the tests measure the
  parent's own exit rather than the harness's patience. A caller that wants to come back when the run it
  started comes back should stop at the terminal frame (`turn.completed`, or the `result` object) rather
  than at EOF; Python's `subprocess.communicate()` reads to EOF by design and will therefore wait.

**Built since: the handle is the default, and a job that ends reports itself.** This is the correction a
person made after using it (2026-09-16): *"my subagent cannot run in the background -- the main session
calls it and can only wait."* The mechanism was there and tested; the default was the problem, and the
evidence is in that person's own session files: two `task` calls in one conversation, **neither** of
them carrying `background`, one of which the person interrupted by typing -- which drops the turn -- and
whose tool result is the literal sentence `interrupted by the user: tool 'task' was requested but never
ran`. A model reads the tool description, and that description ended with "this tool waits for it".

So `task` now starts a child and returns a handle unless the call says `background: false`, and that
needed a second thing to be safe, because a default that starts work nobody is waiting for is a default
that can *lose the work*: a model that has moved on has no reason to poll, and a child costs what it
costs whether or not its answer is ever read. So a job that ends is reported, once, in two lanes:

- **the person is told immediately** (`notice`), in the transcript they are already reading: what ended,
  its exit code, and where its answer is. A child that ends while they are watching used to be
  invisible until they happened to ask.
- **the model is told in its next request**, as one user-role message labelled `[a note from flint, not
  from the person: …]` listing every job that ended unread, with the pid and the verb that collects it.

The lanes are the same fact for two readers, so neither marks the other's work done, and the *third*
reader -- `job_op wait`, `job_op stop`, or a `stop` on a job that had already ended -- counts as a
read and suppresses the notice, which is what makes "once" true rather than "every request from now
on". The bookkeeping is one `AtomicBool` on the job (`reported`), and the report goes into the request
**view** beside the peer relay and never into `history`: the session file stays the record of what
happened, and a conversation resumed from it is not told about a job that ended days ago. The tests hold
all of it: the default (the parent exits in under six seconds against an eight-second child), the door
that waits (`background: false`, where a handle would be a lie), the report (exactly once -- the test
gives the parent a turn *after* the one that carried it, and the mutation that removes the `reported`
check makes it fail with "told 2 times"), and the person's notice on stderr.

**The shape was read off DSH's job runtime rather than invented**, which is worth recording because two
of its decisions were adopted and one was refused. Adopted: **the same verbs for every kind of job**
(DSH's `job_output`/`job_list`/`job_kill` are kind-independent, and flint's single `job_op` with
`status`/`wait`/`stop` is the same idea in one schema), and **a settled job is announced exactly once,
suppressed by any read or kill**. Refused: **waking an idle owner with a turn of its own**. DSH does
that because its agents run unattended goal loops; flint's REPL has a person at the keyboard, and
starting a model turn nobody asked for is a bill nobody agreed to. The person is told instead, and the
model learns on its next request -- which is the same information, one prompt cheaper.

**Built since: a command is a job too, and that is what made the record earn its name.** The question
that drove it was the same complaint one step further out: *"some other tasks must be backgroundable
too, surely -- certain Python scripts, bash."* They were, mechanically: `bash` had a timeout, a spill
file and a kill-tree, and `exec`/`pwsh` went through the same runner. What they did not have was a
handle, so a ten-minute build meant either a held turn or a `nohup` the model had to invent, with no
exit code and no notice at the end.

The change is deliberately small, because it was designed as one record rather than two features.
`Job` gained a `kind` (`Child`/`Command`), a `log` path and a read cursor; `bash`, `exec` and `pwsh`
gained `background: true`, which spawns through `start_background_command` with **both streams into one
log file** under this session's directory and a supervisor that owns the child, its budget and its
`KillTree`; and `job_op` gained `output`. The three differences between the kinds are the three that
could not be shared: where the output goes (a conversation versus a log file), how it is stopped
(`/stop` on stdin versus a kill, and on Windows `taskkill /PID … /T /F`, because killing the shell
alone leaves `cargo` holding `target/` -- measured, in `KillTree`'s comment), and what `output` means
(a child has none; its answer is `wait`).

Four decisions in that list are worth keeping:

- **Commands stay foreground by default, and `task` does not.** The defaults point opposite ways on
  purpose. A command's output is usually the input to the very next step, so waiting is useful; a
  child's answer is a piece of work whose whole value is that the parent keeps going.
- **A background command gets the *long* budget** (`LONG_BASH_TIMEOUT`, 900 s) rather than the ordinary
  two minutes: a command somebody chose to stop waiting for is by definition not a two-minute command.
  The budget is enforced by the supervisor, not by a waiter -- held by a test that starts a 1-second
  budget with nobody watching.
- **`output` is a window, `wait` is the answer.** Reading a running command is incremental from a
  cursor on the job, so polling a build costs the new lines rather than the whole log; `wait` hands over
  the entire file plus its exit code. Reading a job that has *already ended* to the end of its file is
  reading all of it, which counts as collecting it -- so it marks the job reported, exactly like `wait`.
- **Nothing new was invented for the notice.** A command's exit goes to the same person's lane and the
  same request-view report as a child's, through the same `reported` flag, which is the payoff for
  calling both of them one thing.

Two limits are honest and worth stating where a reader will meet them. A command dies with the run on
Windows (the guard fires as the runtime drops the task); on Unix it does not, which is the same
single-process gap `docs/windows-tooling.md` §6.1 already documents for foreground commands. And a
background command is **not** a run: it has no session file, no presence record, and no depth, so
`flint who` will not name it and `job_op` answers for it only from the process that started it.

## Stages

**Stage 1 — presence and `flint who`.** A record per live run, refreshed on the heartbeat flint
already emits, removed on exit, stale after a window. A command that prints it, plus the unattributed
"changed recently" line. No standing decision is touched, and it is what would have prevented the
incident. **This is the one to build first.** — **Built**, with one difference from the sketch above
worth recording: the refresh is *not* driven by the heartbeat. The heartbeat only beats while a model
call is in flight, so a run in the middle of a two-minute `bash` command would have looked dead. The
record is refreshed by a thread that waits on a channel with a five-second timeout, which is
independent of what the run is doing, and the record is removed by a `Drop` so that every exit path is
covered by construction. A record may go unrefreshed for sixty seconds before it is called stale,
because a *wrong* "no other agent" is the failure this exists to prevent. One thing the sketch promised
and the first version does not carry: the record names the run's provider, model and whether it is
`readonly`, but **not its session id**. `flint who` instead reports the newest session file written in
that directory and how long ago, which answers "which conversation is live" for a process it cannot
otherwise see. Putting the session id in the record needs a hook where the writer is created, and it is
a stage-2 addition rather than a reason to hold this one back. — **Built since**: `Presence.session`
carries the conversation's *path*, written where the writer is created and updated where the page is
told the run has moved (`/new`, `/resume`), so `flint who` names the file a run is holding rather than
guessing at the newest one in the directory. The oldest-signal line stays, because it still answers for
a record that has said nothing yet.

**Stage 2 — the `task` tool.** One child per call, with `readonly`, `model`, `provider`, `cwd`,
`schema`, a timeout and a depth bound. Adopting it edited the "Subagents" entry in `ROADMAP.md` in the
same commit, keeping the original reason. — **Built**, and the differences from the sketch are the part
worth reading:

- **The background handle is built, as one tool with an `action`.** `task` hands back a handle at once —
  the child's pid and the conversation it is holding — and `job_op` takes
  `action: "status" | "output" | "wait" | "stop"`, and `pid` names which job. The verbs did not become
  separate tools because every schema is re-sent with every request, and they did not go into `task`
  because a call that sometimes returns an answer and sometimes a receipt is one whose result a model
  has to guess at. What a caller gets is described under "Background is the same record", including the
  two things building it changed: the timeout moved off the waiter into a detached supervisor, and the
  child's stdin is kept in the job so that a later call can still write `/stop` to it. **The handle is
  the default and `background: false` is the door that waits** — see the note at the end of that
  section for why the default moved and what had to be built for the move to be safe.
- **The child is the same binary** (`std::env::current_exe()`), not a path looked up on `PATH`:
  a child built from a different flint than the caller is talking to is a different program. `FLINT_BIN`
  overrides it, for a wrapper that wants a specific build and for the tests, which live outside the
  binary.
- **The endpoint is passed explicitly**, from the parent's resolved provider and model, because
  `--provider`/`--model` on the parent's command line appear nowhere in the config — a child resolving
  its own default would quietly be a different model.
- **The refusal at the depth limit is a tool *result*, not an error.** "not started: this is already a
  flint run at depth 2…" reads as an answer the model can act on, which is what it is; an error would
  invite a retry that cannot succeed.
- **The result is answer-first**, then `exit code: N (meaning)`, `outcome`, `cause`, `error`, the
  validated `result` object on one line when a schema matched, and `session: <path>`. Everything a
  caller branches on is in the text, because a tool result is text — there is no second channel.
- **A session id in the presence record is built.** The stage-1 note above is now history: the record
  carries `session` as a **path** (written where the writer is created, updated where the page is told
  the run has moved), so a `task` child appears in `flint who` as a run *and* as the conversation it is
  holding, and a reader no longer has to guess which file is live. That path is also how `job_op`
  answers for a run this process did not start: the record says what it is and where its conversation
  is, and says honestly that it cannot be waited for or stopped from here.
- **A child's conversation is not one of the person's, and it says whose it is.** Reported from a real
  session: a `task` child's conversation appeared in `/sessions` and in the page's sidebar exactly like
  one the person had, and `--continue` — "the newest conversation in this directory" — resumed the
  child's, because a child is newer than the parent that started it. Both are the same fact, so the fix
  is one fact: the tool sets `FLINT_PARENT` on the child (an environment variable, for `FLINT_DEPTH`'s
  reason: the model driving the call does not get to choose or omit where its child came from), and
  `main` uses it twice — the child's `meta` line names the parent, and its session is written under
  `sessions/<dir>/children/`. The *layout* is what does the excluding, exactly as it does for the
  archive: no listing reads that directory, so `/sessions`, `--list-sessions`, the sidebar and
  `--continue` agree without a filter to drift, and `mv` out of it is how a person adopts a child's
  conversation they want to keep. The file itself is a session like any other — same format, resumable
  by path — which is what keeps "a run is a run" true rather than a promise about the happy path.
- **A child's own progress reaches the parent's status row, and a child left running is named.** Both
  were added after a bug report, before there was any handle. A realistic `task`
  runs for minutes; the parent's row said one unchanging word (`task`) for all of it, because the child
  was already describing what it was doing on its own `--json` stream and the parent was reading those
  frames and discarding them. Typing at a parent whose row is frozen is what a person does next, and
  because a typed line steers, the turn was dropped — and the child, a process of its own, kept going
  for minutes while the parent's record said the tool *"was requested but never ran"*. So: the child's
  `tool.started` and `status` frames are forwarded as `task: running search` (its answer is not — a
  status row is not a second transcript), and the placeholder a dropped turn writes says the result
  never came back **and** describes the child that is still going, with its pid and the session its
  answer will be written to. That sentence and the handle now read the same record, so a child that has
  ended stops being described as "still going", and `job_op` can hand over the answer the sentence
  promised.

**Stage 3 — the mailbox.** `flint say`, `peer.message`, and the opt-in that lets a peer's words reach
the model. Presumably a `.flint/` presence marker in the project as well, so two `FLINT_HOME`s can see
each other. — **Built, including the `.flint/` marker.** `flint say "…"
[--to <pid>] [--cwd <dir>] [--json]` appends one JSON line to `<FLINT_HOME>/mailbox/<dir-key>.jsonl`
(the same directory key the sessions use, so a message reaches the runs working where it was left), or
to `<project>/.flint/mailbox.jsonl` when the directory is inside a project that keeps a marker — one
file for the whole project, which is what lets a run in `src/` hear a run at the root and what lets two
installations hear each other. It needs no key, like `who`: it writes a file rather than asking a model
anything. A run follows its
own directory's mailbox from wherever it is *when the run starts*, so it shows what arrives while it
works rather than replaying yesterday -- and a fresh run therefore begins quiet, which is the honest
default.

Two details are the design rather than the plumbing. **The message is written to the session file as
its own `peer` event and never as a chat message**, and `session::load` reads that event without putting
it in `messages`: the history a request is built from cannot contain it, by construction rather than by
a check somebody could later remove -- which is also what stops the opt-in below from becoming
permanent. And the transcript says which of the two happened out loud -- "peer X says: …" followed by
"(shown to you; not sent to the model)" or "(passed on to the model, because you asked to hear peers)"
-- because a person who has just been ignored by an agent that never saw their message needs to know
that is what happened, and a person whose agent obeyed a stranger needs to know that too.

**The opt-in is built, and building it is where the shape of the safety rule became concrete.**
`--hear-peers` for a run, `/hear-peers on|off` while one is open (and the same switch on the page, drawn
from the run's own state frame like its other three). A message that arrives between turns is then sent
with the **next** request as a user message the model can tell is not the person's:

> `[a peer run working in this directory left a message, and the person who started this run asked for
> peers to be heard; it comes from another process, not from them]` … `peer 41288 says: …`

Four properties are what make that not the hole the section above warns about, and each is a decision
rather than a detail:

- **It is a per-run decision with no config key.** A flag and a slash command, never a setting in
  `config.toml`: a standing property is one somebody forgets they turned on, and this one lets anything
  that can write a file steer a tool loop.
- **It reaches the request, never the history.** The relay is a *view* -- built by `Agent::with_peers`
  beside `prune_tool_output`, drained once into the request that follows -- so the session file keeps
  the `peer` event and the conversation a resumed run is rebuilt from still cannot contain it. An opt-in
  that quietly persisted into every later run would be a decision made once and never again; the test
  asserts both halves, including `session::load` on the file the run just wrote.
- **The record says it happened.** The event is `{"type":"peer",…,"heard":true}`, which
  `docs/session-format.md` documents: the request body is nowhere, so the file is the only place that
  answers "was the model told this". A message heard once is not re-heard, and turning the switch off
  drops anything still queued rather than delivering it with the next question.
- **Nothing arrives mid-loop.** The mailbox is read between turns, so a peer's words cannot change a
  request that is already being written; `--hear-peers` affects the future and never the present.

What is *not* built: a `/say` inside the prompt, so that a person or a model can leave a message without
a second terminal. The `.flint/` marker this stage used to owe is built (above and decision 5).
`flint say`
from another terminal is the primitive, which is the case two agents in one directory actually have.
`tests/say.rs` holds both halves to the bytes: with the default, a peer speaks *during* a turn through
the real command and the test asserts the person saw it, the session file kept it, and **every request
body the provider received is free of it**; with `--hear-peers`, the second turn's request carries it,
the transcript says so, the file says `"heard":true`, and the history loaded back from that file does
not.

**Stage 4 — profiles and fan-out.** `.flint/agents/*.md`: a name, a description, a model, `readonly`,
and a prompt, so "the explorer" is a thing a person and a model can both refer to; and one call that
runs N children at once. This is where "native subagents" as people mean the phrase actually lands — and
it is last on purpose, because everything above is useful without it. — **Built**, and the two halves
are worth reading separately, because one is a file format and the other is a spending decision.

**A profile is a file, not a feature.** `<project>/.flint/agents/<name>.md` (and `<FLINT_HOME>/agents/`
for one that applies everywhere, with the project winning on a name), front matter for `name`,
`description`, `model`, `provider` and `readonly`, body for the instructions. The same hand-written
front-matter parser the skill catalog uses, widened from two keys to any key, so there is still no YAML
crate between a person and their own file. Discovery reads only the front matter, exactly like skills:
a directory of long profiles costs a few lines of request rather than all of their text, and the body
is read when a profile is used, so editing one takes effect in the next child without restarting the
run. Three properties are the design rather than the plumbing. A profile's facts are **defaults**: an
argument on the call still wins — except `readonly`, which a profile can only add, because a profile
that could talk a readonly run into a writing child would be a way around the one property this whole
path exists to keep. The instructions go in front of the job, because they say different things ("how
to work" and "what to do") and the child has no flag for a system prompt. And the catalog reaches the
model **in the tool's schema**, only when this directory actually has profiles: an `agent` property
with nothing behind it would be a schema's worth of tokens teaching the model that a tool lies, and a
model chooses a profile by reading the one line that says what it is for.

**The fan-out is one call, several children, and it is deliberately not more than that.** `tasks` takes
a list of jobs (1–8) and a `max_parallel` (default 4), prepares **every** child before starting any of
them — a bad argument in job three must not leave jobs one and two already spending money — then runs
them a batch at a time and returns one block per job, labelled in the order asked for, each with the
same provenance a single `task` gives (exit code, outcome, cause, session path). What it is not: a
shared context. The children cannot see this conversation or each other, so nothing learned by one
helps another, and a set of jobs that depend on each other is not a set of jobs for this tool. Nor is
it flint deciding to parallelise: the model asks for N jobs or it does not, and the cap exists because
"ask for N" is otherwise a way to spend without saying so.

Two things are worth keeping honest about the price. `tasks` costs one more tool schema in every
request, always offered rather than appearing when it might be useful — a tool that comes and goes is
one a model cannot plan around. And N jobs is N bills at once, which is why the answers come back with
their session paths attached: whoever pays can read what was actually asked.

## Decisions this needed from the person whose repository it is

**Answered on 2026-09-16: every recommended value was adopted.** Recorded with what each one binds,
because a decision that is only in a conversation is not a decision.

1. **Adopt stage 2, and edit the "Subagents" entry?** — **Yes, the narrow version.** A `task` tool that
   starts another flint the way a Python caller or an MCP client does. Built, and the `ROADMAP.md`
   bullet was edited in the same commit as the code that adopts it, with the original reason kept in
   the text — the parent's context is still one window, and a subagent's value is isolation and least
   privilege, not a bigger window. A plan document and a plan of record that disagree is how a
   repository starts lying to itself, which is why they moved together.
2. **The depth bound.** — **`FLINT_DEPTH`, maximum 2, an environment variable** rather than a flag, so
   that a model cannot edit the bound out of its own command line. **Built**: the tool sets
   `FLINT_DEPTH=<n+1>` on its child and is the only thing that writes it, and a run already at the
   limit refuses with a sentence rather than a truncated attempt.
3. **The mailbox default.** — **Shown to the human, never fed to the model**, until a person opts in
   per run. This is the one decision that is a safety property rather than a preference, and stage 3
   may not ship without it — which is why the half that only shows was built first and was already in
   use before anything could feed a model. **The opt-in is now built too, bound the way this decision
   says rather than the way that is convenient**: `--hear-peers` per run and `/hear-peers on|off` while
   one is open, no config key, the relay landing in the request view and never in `history`, the session
   recording `"heard":true` on the message that was relayed, and the transcript naming which of the two
   happened. Built the way it is enforced rather than promised: a peer's message is still a `peer`
   session event and `session::load` still keeps it out of `messages`, and the tests hold both
   directions to the bytes -- every request body free of it by default, and the words present in exactly
   the one request that followed a message the person had asked to hear.
4. **What "another agent is here" may claim.** — **Name no author, ever.** Built: the human line says
   "this names no author", and the machine-readable answer carries the same warning in a `note` field,
   because a program is the reader most likely to treat a short list as a complete one.
5. **Presence in `FLINT_HOME` only, or also `.flint/` in the project?** — **`FLINT_HOME` only, for
   now** was the answer on 2026-09-16, and **the marker is now built as well**, which supersedes it:
   the home record stays (a `readonly` run must be able to announce itself, and a checkout is not always
   writable) and a project that keeps a `.flint/` gets the second copy that crosses installations. The
   cost this decision named — writing into somebody's checkout — is bounded rather than accepted in
   general: **flint never creates the marker**, so the write happens only where a person already put
   one. A project with no `.flint/` is still invisible to a flint with a different home, which is now a
   one-command choice rather than a gap.

## What would make this the wrong idea

If the only real use is "run twenty prompts in parallel", a shell loop and `map_calls` are already
that, and stage 4 is unnecessary. It was built anyway, on the narrower argument that a fan-out the
*model* asks for, capped and labelled, is not the same thing as flint deciding to parallelise work on
its own — and if it turns out nobody asks for several jobs in one call, the honest move is to delete
`tasks` rather than to grow it into a scheduler. If peers are only ever used to *watch* rather than to
*work together*, the mailbox is a `flint who` that prints and nothing more. And if the first thing that
happens when two agents can talk is that one of them talks the other into something it should not do,
the default in decision 3 was the only part of this that mattered.
