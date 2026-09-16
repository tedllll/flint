# Agents as processes: spawning, finding, and talking to peers

**Status: stages 1, 2 and 4 are built, and so is the mailbox half of stage 3; the MCP and Python doors
are built.** — a running flint writes a presence record and `flint who` reads it (`src/live.rs`,
`tests/who.rs`), a running flint can start another flint with its `task` tool and start several at once
with `tasks` (`src/tools.rs`, `tests/task.rs`), a profile in `.flint/agents/` says how a child should
start (`src/context.rs`), and a peer can leave it a message with `flint say` that is shown to the person
and never sent to a model (`tests/say.rs`), so *being called*, *seeing each other*, *spawning*,
*talking* and *naming a way to work* all work today. The five decisions at the end of this file **were
answered on 2026-09-16: every recommended value was adopted**, and each is marked below with what that
means for the code.

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
| a `task` tool inside flint | flint itself | **Built.** `src/tools.rs` (`TaskTool`), `tests/task.rs`; one child per call, plus `tasks` for several at once. No background handle yet |
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
still be able to announce itself and a project directory is not always writable. The cost is honest and
must be documented: **two flint installations with different `FLINT_HOME`s cannot see each other.** A
`.flint/` marker inside the project would fix that and is the natural stage-3 addition, at the price of
writing into somebody's checkout.

A record that is not cleaned up (a killed process cannot clean up) is not an error: every record
carries `last_seen`, and anything older than a fixed window is reported as *stale* rather than as
alive. Nothing about this may depend on pid liveness, which is not portable and would make the answer
depend on the machine rather than on the file.

## Talking: the mailbox

`flint say` appends one line to a mailbox under `FLINT_HOME`, addressed to a session, to a directory,
or to whoever is listening. A running flint already polls a channel every turn — the stdin reader that
takes `/stop` — so a mailbox is one more source on a mechanism that exists, not a new lifetime, not a
socket, and not a server. The record stays append-only plain JSONL, so a human can read it, and so the
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
a stage-2 addition rather than a reason to hold this one back.

**Stage 2 — the `task` tool.** One child per call, with `readonly`, `model`, `provider`, `cwd`,
`schema`, a timeout and a depth bound. Adopting it edited the "Subagents" entry in `ROADMAP.md` in the
same commit, keeping the original reason. — **Built**, and the differences from the sketch are the part
worth reading:

- **Nothing here is a background handle yet.** `background: true`, `task_status`, `task_wait` and
  `task_stop` are not built: a `task` call blocks until the child ends, and the only stop is the
  caller's timeout, which writes `/stop` to the child's stdin first and kills it only if that is
  ignored. `/stop` rather than `kill` because the half-answer the child had drawn is still written to
  its session file, and a person reading that later should find work, not a corpse. The handle is the
  presence record, and it is available to a *person* through `flint who` even though the tool does not
  return one.
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
- **A session id in the presence record is still open.** The stage-1 note above stands: the record has
  no session id, so a `task` child appears in `flint who` as a run in that directory rather than as
  *this* run's child, and a parent cannot point at a handle for it.
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
  were added after a bug report, and both are the price of having no handle yet. A realistic `task`
  runs for minutes; the parent's row said one unchanging word (`task`) for all of it, because the child
  was already describing what it was doing on its own `--json` stream and the parent was reading those
  frames and discarding them. Typing at a parent whose row is frozen is what a person does next, and
  because a typed line steers, the turn was dropped — and the child, a process of its own, kept going
  for minutes while the parent's record said the tool *"was requested but never ran"*. So: the child's
  `tool.started` and `status` frames are forwarded as `task: running search` (its answer is not — a
  status row is not a second transcript), the placeholder a dropped turn writes says the result never
  came back **and** describes the child that is still going, with its pid and the session its answer
  will be written to, and the same sentence goes to the person's transcript and onto a `--json` stream
  that is about to end. What is still missing is the handle itself: nothing here can *wait* for that
  child or *stop* it (see "Background is the same record"), so the answer is collected by reading the
  session file it names rather than by asking the parent.

**Stage 3 — the mailbox.** `flint say`, `peer.message`, and the opt-in that lets a peer's words reach
the model. Presumably a `.flint/` presence marker in the project as well, so two `FLINT_HOME`s can see
each other. — **The mailbox half is built, and the opt-in deliberately is not.** `flint say "…"
[--to <pid>] [--cwd <dir>] [--json]` appends one JSON line to `<FLINT_HOME>/mailbox/<dir-key>.jsonl`
(the same directory key the sessions use, so a message reaches the runs working where it was left), and
it needs no key, like `who`: it writes a file rather than asking a model anything. A run follows its
own directory's mailbox from wherever it is *when the run starts*, so it shows what arrives while it
works rather than replaying yesterday -- and a fresh run therefore begins quiet, which is the honest
default.

Two details are the design rather than the plumbing. **The message is written to the session file as
its own `peer` event and never as a chat message**, and `session::load` reads that event without putting
it in `messages`: the history a request is built from cannot contain it, by construction rather than by
a check somebody could later remove. And the transcript says so out loud -- "peer X says: …" followed by
"(shown to you; not sent to the model)" -- because a person who has just been ignored by an agent that
never saw their message needs to know that is what happened.

What is *not* built, and is not an oversight: the opt-in that would let a peer's words reach the model,
the `.flint/` marker in the project, and a `/say` inside the prompt. `flint say` from another terminal
is the primitive, which is the case two agents in one directory actually have. `tests/say.rs` holds the
rule to the bytes: a peer speaks *during* a turn, through the real command, and the test asserts the
person saw it, the session file kept it, and **every request body the provider received is free of it**.

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
   may not ship without it — so the half that shipped is the half that only shows, and the opt-in that
   would feed a model is not built at all. Built the way it is enforced rather than promised: a peer's
   message is a `peer` session event, `session::load` keeps it out of `messages`, and a test asserts
   that no request body a provider received ever contained one.
4. **What "another agent is here" may claim.** — **Name no author, ever.** Built: the human line says
   "this names no author", and the machine-readable answer carries the same warning in a `note` field,
   because a program is the reader most likely to treat a short list as a complete one.
5. **Presence in `FLINT_HOME` only, or also `.flint/` in the project?** — **`FLINT_HOME` only, for now**
   (the stage-3 `.flint/` marker is the addition that makes two installations see each other). Built as
   decided, with the cost written down where a reader will meet it: two `FLINT_HOME`s cannot see each
   other.

## What would make this the wrong idea

If the only real use is "run twenty prompts in parallel", a shell loop and `map_calls` are already
that, and stage 4 is unnecessary. It was built anyway, on the narrower argument that a fan-out the
*model* asks for, capped and labelled, is not the same thing as flint deciding to parallelise work on
its own — and if it turns out nobody asks for several jobs in one call, the honest move is to delete
`tasks` rather than to grow it into a scheduler. If peers are only ever used to *watch* rather than to
*work together*, the mailbox is a `flint who` that prints and nothing more. And if the first thing that
happens when two agents can talk is that one of them talks the other into something it should not do,
the default in decision 3 was the only part of this that mattered.
