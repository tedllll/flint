# Agents as processes: spawning, finding, and talking to peers

**Status: stage 1 is built; the MCP and Python doors are built; stages 2–4 are a plan.** — a running
flint writes a presence record and `flint who` reads it (`src/live.rs`, `tests/who.rs`), so *being
called* and *seeing each other* work today. Stage 2 deliberately contradicts the "Subagents" entry in
`ROADMAP.md`. Read that entry beside this file; adopting any stage below means editing it in the same
commit. The five decisions at the end of this file **were answered on 2026-09-16: every recommended
value was adopted**, and each is marked below with what that means for the code.

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
| a `task` tool inside flint | flint itself | **Planned**, stage 2 |

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
- **depth is bounded** (stage 2), which today it is not: nothing stops a flint from starting a flint
  that starts a flint, and the bill is real;
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

**Stage 2 — the `task` tool.** spawn one child (or the same tool with `background: true`), with
`readonly`, `model`, `provider`, `cwd`, `schema` and a depth bound. Adopting it edits the "Subagents"
entry in `ROADMAP.md` in the same commit, and the honest version of that edit keeps the original
reason: the parent's context is still one window, and a subagent's value is isolation and least
privilege, not a bigger window.

**Stage 3 — the mailbox.** `flint say`, `peer.message`, and the opt-in that lets a peer's words reach
the model. Presumably a `.flint/` presence marker in the project as well, so two `FLINT_HOME`s can see
each other.

**Stage 4 — profiles and fan-out.** `.flint/agents/*.md`: a name, a description, a model, `readonly`,
and a prompt, so "the explorer" is a thing a person and a model can both refer to; and one call that
runs N children at once. This is where "native subagents" as people mean the phrase actually lands — and
it is last on purpose, because everything above is useful without it.

## Decisions this needed from the person whose repository it is

**Answered on 2026-09-16: every recommended value was adopted.** Recorded with what each one binds,
because a decision that is only in a conversation is not a decision.

1. **Adopt stage 2, and edit the "Subagents" entry?** — **Yes, the narrow version.** A `task` tool that
   starts another flint the way a Python caller or an MCP client does. The `ROADMAP.md` bullet still
   reads "under review" and is edited **in the same commit as the code that adopts it**: nothing is
   adopted by this document, and the original reason survives the edit (the parent's context is still
   one window; a subagent's value is isolation and least privilege, not a bigger window).
2. **The depth bound.** — **`FLINT_DEPTH`, maximum 2, an environment variable** rather than a flag, so
   that a model cannot edit the bound out of its own command line. Not built yet; it arrives with the
   `task` tool, because a bound on a feature that does not exist is a promise rather than a guard.
3. **The mailbox default.** — **Shown to the human, never fed to the model**, until a person opts in
   per run. This is the one decision that is a safety property rather than a preference, and stage 3
   may not ship without it.
4. **What "another agent is here" may claim.** — **Name no author, ever.** Built: the human line says
   "this names no author", and the machine-readable answer carries the same warning in a `note` field,
   because a program is the reader most likely to treat a short list as a complete one.
5. **Presence in `FLINT_HOME` only, or also `.flint/` in the project?** — **`FLINT_HOME` only, for now**
   (the stage-3 `.flint/` marker is the addition that makes two installations see each other). Built as
   decided, with the cost written down where a reader will meet it: two `FLINT_HOME`s cannot see each
   other.

## What would make this the wrong idea

If the only real use is "run twenty prompts in parallel", a shell loop and `map_calls` are already
that, and stage 4 is unnecessary. If peers are only ever used to *watch* rather than to *work together*,
the mailbox is a `flint who` that prints and nothing more. And if the first thing that happens when two
agents can talk is that one of them talks the other into something it should not do, the default in
decision 3 was the only part of this that mattered.
