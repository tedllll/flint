# Pi, read for what flint can take from it

Pi (pi.dev, `github.com/Earendil-Works/pi`, MIT) is a minimal coding-agent harness written in
TypeScript by Mario Zechner: a unified provider library, an agent loop, a terminal UI library, and
the `pi` command that puts them together. It is the most useful comparison flint has, because it is
the same kind of program built on the opposite bet. Pi answers "how little can a harness be?" by
removing things; flint answers it by keeping one static binary and no permission layer.

This document is a reading, not a proposal. It follows the rule
`docs/research-permissions-prior-art.md` set: every claim about Pi is sourced from a page that was
loaded, the pages that could not be loaded are named as such, and nothing here was verified by
running Pi. The claims about flint were read out of this tree, and the ones that say "absent" were
searched for rather than assumed.

## 1. What Pi is, in one page

| Package | What it is |
|---|---|
| `pi-ai` | unified multi-provider LLM API (OpenAI, Anthropic, Google, and compatible endpoints) |
| `pi-agent-core` | the agent runtime: tool calling, state, message queues |
| `pi-coding-agent` | the interactive CLI, sessions, extensions, modes |
| `pi-tui` | the terminal UI library with differential rendering |
| `chord`, `pi-telemetry` | a service/plugin runtime and telemetry contracts |

**Four tools.** By default the model is given `read`, `write`, `edit` and `bash`. `grep`, `find`,
`ls` and `powershell` exist but are off, and the system prompt tells the model to use `bash` for
`ls`, `grep` and `find` rather than giving each its own tool. The system prompt and all four tool
definitions together are under 1000 tokens.

**Four modes, one session format.** Interactive, `-p`/`--mode json` for scripts and CI,
`--mode rpc` (a JSONL protocol over stdin/stdout for integrations that are not Node), and an SDK for
embedding. The README is explicit that they share one session format and one event stream, so the
terminal, a program and CI are looking at the same agent.

**Sessions are a tree in one file.** JSONL, one entry per line, every entry carrying an `id` and a
`parentId`. The current position is a *leaf pointer* rather than a rewrite, so branching is an append
and `/tree` moves that pointer to an earlier entry and carries on from there; `/fork` copies the active
path up to a chosen user message into a new session, records where it came from in the new file's
header, and puts that prompt back in the editor; `/clone` duplicates the whole active branch. Two
consequences are worth noticing from flint's side: a fork's file names its own lineage, and modal state
is *entries* -- a model change and a thinking-level change are lines in the tree, so what was in force
at any point is recoverable from the path rather than from a header that was rewritten.

**Compaction.** Automatic by default, triggered on context overflow (recovers and retries) or
proactively near the limit, and `/compact` does it on demand with optional instructions. The summary,
and the id of the first entry it keeps, are appended to the JSONL file as an entry of their own, so
the lossy part is visible in the same file as the history it replaced. §3.7 has the mechanism.

**Customization is three layers.** Skills (Markdown, the Agent Skills standard, loaded on demand and
invoked as `/skill:name`), prompt templates (Markdown with arguments, expanded by typing `/name`),
and extensions (TypeScript modules that register tools and commands, hook tool calls, replace
compaction, draw UI, and hot-reload). All three, plus themes, bundle into a "Pi package" installed
from npm or git.

**The TUI writes to the scrollback.** No alternate screen: it appends like any CLI, keeps the
terminal's own scrolling and search, and repaints by retaining a component tree -- each component
renders cached lines, the new frame is diffed against the old, only the changed tail is rewritten,
and the whole frame is wrapped in the terminal's synchronized-output sequence (`CSI ?2026h` /
`CSI ?2026l`) so it lands at once.

**The refusals**, from the README's own Philosophy section, verbatim:

> **No MCP.** Build CLI tools with READMEs (see [Skills](#skills)), or build an extension that adds
> MCP support.
>
> **No sub-agents.** There's many ways to do this. Spawn pi instances via tmux, or build your own
> with [extensions](#extensions), or install a package that does it your way.
>
> **No permission popups.** Run in a container, or build your own confirmation flow with
> [extensions](#extensions) inline with your environment and security requirements.
>
> **No plan mode.** Write plans to files, or build it with [extensions](#extensions), or install a
> package.
>
> **No built-in to-dos.** They confuse models. Use a TODO.md file, or build your own with
> [extensions](#extensions).
>
> **No background bash.** Use tmux. Full observability, direct interaction.

Each refusal has a reason in the author's own post, and the reasons are worth more than the
refusals. §4 is where they matter, because several of them are aimed at exactly the things flint
built.

## 2. Where flint already agrees, having got there separately

Two independent implementations landing on the same answer is the closest thing either has to
evidence, so the convergences are worth writing down.

| Pi | flint |
|---|---|
| No permission layer; containerize if you need a boundary | `AGENTS.md`: "There is no permission layer", with `readonly` and the read-before-mutate gate as mistake-guards rather than boundaries |
| Plans and to-dos are files (`PLAN.md`, `TODO.md`), not features | `ROADMAP.md` and `HANDOFF.md` are exactly that, and are what this project works from |
| One event vocabulary for provider deltas and agent activity, with UIs built on top | `src/event.rs` is one enum for the same two things; `src/sink.rs` is what a turn's events *become* |
| Every tool result is split into what the model sees and what a UI draws, as a contract rather than a habit | `src/display.rs` and `src/sink.rs` do the same job: a tool call renders as a block whose paths are buttons and whose output folds, while the model gets words |
| Sessions are append-only JSONL, documented, readable, forkable by copying | `docs/session-format.md`, `/resume`, `--continue`, `/archive`, and `--fork` (`SessionWriter::seed` writes a new file from the copied messages) |
| Append to the scrollback; do not take over the terminal | `src/term.rs`'s inline viewport -- a scroll region and a status line, not an alternate screen |
| A cancelled request keeps its partial output, and cancellation is designed in | the stopped turn keeps the half of an answer it had drawn, and says so separately to the page |
| Skills are disclosed progressively: a catalog line now, the body on demand | `instructions = "hint"`, the catalog capped at 100 characters a line and 1200 in total, and the `skill` tool that loads one |
| `AGENTS.md` is the project instruction file, layered global to local | `src/context.rs`, and `AGENTS.md` is what you are reading |
| A run's children can identify themselves to the processes they start | `FLINT_DEPTH` and `FLINT_PARENT` on a `task` child, and since 3.9 landed `FLINT_SESSION`/`FLINT_PROVIDER`/`FLINT_MODEL` on every command a run starts (`src/tools.rs`), all set by flint and by nothing else |
| Compaction is lossy and the file is the record | request-side `prune_tool_output` and `trim_old_turns` only; the session file keeps every byte, and the note in the request's place is a count |
| A tool must truncate its output, and must say so and where the rest is | `max_tool_output` with a spill file, and a tool result that names it |
| The loop has no step ration; it runs until the model stops asking for tools | `max_steps`, documented as "a runaway-loop guard, not a work ration" |
| No auto-retry and no auto-compaction, so "the turn ended" is unambiguous | the same: a turn ends when the loop ends, and the page is told once |
| No MCP; a capability is a CLI tool plus a README the model reads when it needs it | `ROADMAP.md` defers MCP for the same reason, and `search` is the built-in exception |

Two differences in the same area are deliberate rather than oversights, and both are measurable
(§5): flint's tool set is larger than four, and flint's provider layer speaks one protocol
carefully instead of four approximately.

## 3. What is worth taking

Ordered by value over cost, in the flint-shaped version rather than Pi's. None of this is agreed to;
it is a list to choose from, and each entry says what flint has today so the size of the change is
visible.

### 3.1 A fork from a chosen point, in the session you are in

**Built, 2026-09-17.** `/fork <n>` cuts at the n-th question asked in this conversation and starts a
conversation of its own from what came before it; `--fork <file>` copies the whole thing at startup. A
bare `/fork` lists the questions, because which point to cut at is the one thing the command may not
guess.

Flint already had `--fork`, which copies a session into a new file and continues the copy; what it
could not do was fork from a *point*. Pi's `/fork` opens a selector, copies the active path up to the
chosen user message, and puts that prompt back in the editor for editing before it is sent. That is
the shape of "that went wrong four messages ago, start again from there" -- the thing `--fork` plus
hand-editing a JSONL file could only approximate, because the whole tail came along.

The change is small and needs no format change: a session is append-only, so a fork from message *n*
is the first *n* lines of the file written into a new one, through the same `SessionWriter` that
`--fork` already uses. It is also the natural pair to the page's sidebar, where a person can already
see the messages a fork would cut at. Pi's tree-in-one-file with `id`/`parentId` is the general
solution; the prefix copy gets most of the value and keeps the property `/delete`, `/archive` and
"`cp` is how you fork one" (`docs/session-format.md`) are all written against.

Two details from Pi's version are worth taking with the prefix copy even though the tree is not.
`createBranchedSession(leafId)` is the same operation flint would have -- extract a branch into a
session file of its own -- which is evidence that the cheap version is not a toy. And the forked file
records its **lineage** in its own header (`parentSession`), so a conversation found later says what
it was forked from. Flint's `meta` line already has a `parent` field, but it means something else
today -- the run that started this one -- so a fork's lineage needs either a second field or a
deliberate decision to share one, and that decision is worth making out loud rather than by picking
whichever name is closer to hand.

**Which way that decision went, and the one place this reading was wrong about the mechanism.** The
lineage is neither a second `meta` field nor the shared `parent`: it is a `fork` **event**, for the
reason `title`, `switch` and `import` are events -- the file is append-only, and a fact that arrives at
a moment is a line. Writing `meta.parent` would have been actively wrong, not merely imprecise: that
field is what files a conversation under `children/` and keeps it out of `/sessions`, the sidebar and
`--continue`, so a branch would have been hidden from its own author as somebody's child. The event
carries `from`, `from_id` and `kept` (present only when the copy is a prefix, which is what tells a cut
from a whole copy).

The mechanism in the sketch above is also slightly off, and the difference is the format's own rule:
the copy is *messages*, not the first *n* lines of the file. `SessionWriter::write_messages` writes the
`chat` lines of the prefix and nothing else -- no `meta`, no `usage`, no `title`, no earlier `import`
or `fork` line -- so a chain of copies reads as a chain of files rather than as an original with a
paragraph of history above it, and the branch's `meta` belongs to the run that made it. The cut itself
is at a **question** rather than at a message index, which is where this went one step past Pi: a
question is a turn boundary (the same unit `trim_old_turns` cuts on, because a tool result whose call
was dropped is a request the provider rejects) *and* it is the only boundary a person can name at the
prompt.

Pi's selector has a counterpart here that the sketch did not ask for, and it fell out of the frame the
page already draws: `/fork`'s values are computed from the conversation each time the page's command
frame is built, so the browser offers the questions of *this* conversation as buttons. It is the same
mechanism `/skills` and `/prompt` use for their names — the page composes `/<name> <value>`, which is
why the value is the ordinal and not the question's text.

### 3.2 A follow-up message, as distinct from an interruption

Flint already does something Pi has no name for: a plain line typed while the model is working is
**steering**, and it drops the in-flight request and becomes the next prompt (`src/main.rs`, where
`/stop` sets `interrupted` and anything else becomes `steering`). The page's composer sends the same
keystroke event, so both doors behave the same way.

Pi's queue distinguishes two things flint does not: **steering** is delivered after the current
assistant turn finishes executing its tool calls, and **follow-up** only after the agent has finished
all its work. Enter steers, Alt+Enter follows up, Escape aborts and puts queued messages back in the
editor, and the settings choose whether they are delivered one at a time or all at once. Its RPC
documentation says where the queue lives, which is the part worth copying: aborting *continues* the
messages still queued in the session, and a client that wants Escape's behaviour sends `clear_queue`
first and then `abort`, putting the returned text back in its own editor. So the queue is the
session's, and discarding it is an explicit act rather than a side effect of stopping.

The interesting difference is not extra features but that Pi's steering *waits for a boundary* while
flint's *cancels the request*. Both have a place: "no, do it this way" wants flint's, and "also, when
you are done, ..." wants Pi's, and today flint's only answer to the second is to type it after the
turn ends and hope you remember. The smallest useful version is a second way to send a line that does
not interrupt -- a follow-up queue of one, drained at the start of the next turn -- with the page's
composer offering the same choice, and with the queue belonging to the session so that stopping a
turn does not silently throw away what somebody typed.

**Built, 2026-09-17, as `/queue <text>`** — and what the sketch got wrong is worth as much as what it
got right, because two of its three words mean something else in flint than they do in Pi.

*The queue is not the session's.* Pi's queue can live in the session because its session is the
process's own state and its client can be handed messages back. flint's session file is the record of
what was **asked**, and a line that has been queued has not been asked yet: writing it down would put a
message in the transcript of a conversation that never sent it, and reading it back on `--resume`
would deliver a sentence somebody typed an hour ago to a model that has never heard of it. So the queue
is the *run's* memory, it dies with the run, and what the file gets is the message, when it is sent --
which is also why there is no new event type and nothing for `docs/session-format.md` to describe.

*Nothing is discarded silently, but there is no `clear_queue`.* The rule the sketch wanted from Pi --
"discarding is an explicit act rather than a side effect of stopping" -- is kept where it matters: a
stop does **not** clear the queue (a stop is about the answer in flight, not about what somebody typed
next), and the two ways a chain of turns can end without sending it (a command typed mid-turn, an error
in the turn) print a sentence saying the queued lines were dropped. What flint does not have is the act
itself, and the reason is mechanical rather than a preference: Pi's client discards by taking the text
back into its own editor, where nothing is lost, and flint's prompt is a line from a terminal with
nowhere to put it back to. The spellings that suggest themselves (`/queue clear`) collide with a message
whose text is that word, and a queued line is short, visible in the transcript, and sent at the end of
the turn it was queued for.

*The boundary is a turn, not a tool call.* Pi's steering waits for the assistant turn to finish its tool
calls; flint's steering cancels the request and flint's follow-up waits for the whole turn, which is the
whole answer rather than one step of it. Nothing was taken from the difference: the two words flint has
are "interrupt this" and "when you are done", and the middle case -- deliver between steps -- is not
something a person can name from a prompt, because the steps are not on screen as boundaries.

*The page's half is a form row, not a key.* Alt+Enter is not something a text box can carry, so the page
is handed `/queue <text>` in its command panel (the same `Form` class `/say` and `/name` use) and the
line it composes is the line the terminal would have received. `tests/cli_output.rs` holds both halves:
the frame carries the row, and a real run's queue survives a stop and becomes the next question.

### 3.3 Choosing the tool set for one run

Pi: `--tools read,grep,find,ls`, `--exclude-tools`, `--no-tools`, `--no-builtin-tools`. Flint has
`readonly`, which is all-or-nothing and also refuses mutating shell commands. A named allowlist is
the narrower switch for the same purpose -- "read this and do not touch it", or "this run does not
need `search` or `fetch`" -- and flint's tool set is assembled in exactly one place, `ToolBox::new`
in `src/tools.rs`, called once from `Agent::new`, with `readonly` as the only per-run variation
today. So this is a flag and a filter, not a mechanism.

It is also the experiment that makes §5.3 possible: the same task with and without the tools a run
rarely uses is the only honest way to answer whether a large tool set costs anything.

### 3.4 `--no-session`, and a session directory to point at

Pi has `--no-session` (ephemeral: never save) and `--session-dir`. Flint has neither; its only
no-file case is incidental -- a session file is created by the first event in it, so a run that says
nothing leaves nothing. A flag that promises "this run leaves nothing behind" is small, and it is the
honest way to run flint over something that should not end up in your history.

**`--no-session` is built, 2026-09-17.** The halfway version above turned out to be the whole argument
for building it: a promise that depends on saying nothing is not a promise, and the flag's real content
turned out to be the *doors* rather than the file. It is a property of the run, so the command-line
doors that would open or name a conversation (`--continue`, `--resume`, `--fork`, `--name`) are
refused, `/new` and `/resume` are refused inside the run, and the funnel every mid-run switch goes
through (which normally creates the session when the old one has no file) cannot create one either.
What it still writes is what it needs to work -- spilled tool output and a background command's log --
under `spill/unattached-<pid>/`, and a `task` child inherits the flag, because a session-less parent
has no id to file a child's conversation under `children/` and it would otherwise land in the person's
own list. `--session-dir` is *not* built, and is not obviously wanted: `FLINT_HOME` is that knob
already, and a second way to say where sessions go is a second thing to keep in step.

### 3.5 Reading a finished conversation outside a live run

Pi exports a session to HTML or JSONL, imports one back, and can share one as a static page. Flint's
page is a window onto a *running* process: `GET /session` serves the file exactly as it is on disk,
but nothing renders a finished session, and `--json` is a stream rather than an artifact. Two
different things are missing and they are worth separating, and **one of them is now built**:

- **`/import <file>` — built, 2026-09-17**: start from a session file you hand-edited or were given.
  It was the most flint-shaped item in this document and building it confirmed why: the work was
  almost entirely deciding what it must *not* do. `--resume <path>` already opens any file, so the
  command would have been a synonym unless it differed in ownership -- and it does: resuming carries
  on inside the file it was handed, which is the wrong thing to do to somebody else's file (it grows,
  it gains this machine's `usage` lines, its `meta` still names their directory), so `/import` copies
  and does not write to the source at all. It is `--fork`'s act for a file this run never started
  from, and it is what makes hand-editing a first-class way to work rather than something only
  `--continue`'s directory scan can find. Three refusals carry the rest of the design: a file with no
  conversation in it (importing nothing would leave a session in the list that nobody could tell from
  a real one), the file this run is currently writing (a growing conversation copied into itself;
  `--fork` is that act), and `--no-session`, which refuses it like every other door that would create
  a conversation. The copy carries its provenance on a line of its own above the conversation
  (`{"type":"import","from":…,"from_id":…,"messages":…}`), and that line is read back by the two doors
  that name a conversation -- the same rule as everywhere else in this repository: a record nothing
  reads is not a record.
- **An HTML export**: the same reading the page already draws, written to one file with no process
  behind it. This is the bigger half, and it is only worth doing if the page's renderer can be driven
  from a static frame -- which, since the page's model half is pure and runs under Node in
  `scripts/web-view-test.js`, is a question with a cheap answer. Still owed.

There is a third thing in the same area, and it is the smallest of the three: Pi records every entry
with a stable id, which makes an id a **durable cursor** -- a client asks for everything after the
last id it saw and gets it, "even across client restarts", including history the compaction has since
taken out of the live context. flint's page has a reconnection cursor (`last=`), but it is a
per-process ring rather than a property of the file, so a page that reconnects after flint restarted
re-reads from nothing. Naming the line a cursor points at is a session-format question, and the format
is documented and hand-editable -- which is exactly why it is worth deciding deliberately rather than
slipping in.

### 3.6 Prompt templates, and a skill the person can invoke

**Built, 2026-09-17.** A Markdown file in a known directory, typed as `/name`, with arguments
substituted. Flint has skills (which the *model* loads through the `skill` tool) and profiles (which
start a child), but nothing that lets a person save a prompt they type often. Same discovery shape as
the skill catalog, no new format, plain files.

What was built is that shape rather than Pi's `User: <args>` verbatim, and the differences are the
decisions worth keeping. A prompt file is `<dir>/<name>.md` in the skill catalog's three places and its
priority order — `<project>/.flint/prompts/`, then the working directory's, then `<FLINT_HOME>/prompts/`
— with the file name as the word typed and front matter's `description` (or the body's first line) as
the one line `/prompts` lists. Arguments are a hole rather than an appendix: `{args}` is replaced where
the author put it, and a file with no `{args}` gets the words appended as a last paragraph, because
"save this and aim it at something else" is what a template is for and most authors do not think about
arguments while writing. An empty argument is not an error either way: the file is sent as written.

The companion gap was one line away and is closed by the same mechanism, with a second spelling rather
than a second implementation. `/skill <name> [args]` sends a skill's instructions *as the person's next
message* — the same body the `skill` tool would return and `/skills <name>` prints, through the same
`load_skill` — so the person's door onto progressive disclosure is `/skill`, and `/skills` keeps
meaning "print it". Pi spells it `/skill:name`; flint spells it as a command with an argument, because
a name inside the command token is a spelling the page's menu cannot send: a row there is a fixed
`send` plus one value, which is exactly `/skill <name>`. The same reason gives `/prompt <name> [args]`
as the door a *page* can press (`/prompts` is the reading), while `/<name>` typed at the keyboard is
the fast path for the person who already knows what they saved.

Three properties are the design rather than the plumbing. **A template never reaches the model**:
nothing about `/prompts` enters the system prompt or a tool schema, so a directory of long templates
costs a run nothing until one is typed — the opposite of the skill catalog, which is a summary per
skill in every request by design. **The transcript keeps the person's line**: the echo is
`> /tidy-commits src/parser.rs`, and the file it stood for is named once underneath, with how much of
it was sent, because a screenful of repeated template makes every invocation unreadable and the question
"which of the two files by that name won" still deserves an answer. **The session file keeps the
truth**: the request and the record carry the expanded text, so a conversation resumed from the file is
rebuilt from what the model was actually sent rather than from a line that no longer means anything
without the file that produced it.

### 3.7 Compaction that lives in the session file

Pi summarizes older messages when the context fills, optionally on demand with instructions, and says
openly that it is lossy while the full history stays on disk. Flint's request-side answer is
`prune_tool_output` and `trim_old_turns`: stale tool output is replaced by a line saying how many
lines were dropped, and the oldest whole turns past `max_request_chars` are left out, replaced by a
count. The session file is untouched, which is right, but the beginning of a long conversation is
simply absent from the request, and the note that stands in for it is a number rather than a
reminder of what was in it.

The flint-shaped version is a `/compact` that asks the model for a summary and writes it **into the
session file** as a line of its own -- visible, hand-editable, resumable, and never a silent rewrite
of history. The format already tolerates such a line (a test uses
`{"type":"compaction","summary":"earlier stuff"}` as an example of an unknown type that is skipped in
silence), and the request-side note is already read from the transcript rather than from anywhere
else, so the hook exists and nothing writes to it.

Pi does exactly this, and its shape is worth copying rather than inventing. The summary is an appended
JSONL entry carrying the summary text **and `firstKeptEntryId`**, the first entry that survives the
cut, so the file says both what the earlier conversation was about and where the live one resumes --
one line, readable by a person, and enough to rebuild the next request without a second source of
truth. Three of its rules are the kind of thing that only shows up after a summarizer has been used:

- **Never cut at a tool result.** A valid cut point is a user message, an assistant message, a
  bash-execution message or a custom message, because a tool result has to stay with the tool call it
  belongs to.
- **A turn that does not fit is split rather than lost**, with a history summary and a "turn prefix"
  summary merged -- which is the case flint's `trim_old_turns` handles by dropping the whole turn.
- **The text handed to the summarizer is not the conversation format**: it is `[User]:`,
  `[Assistant thinking]:`, `[Tool result]:` lines, deliberately so that the model does not treat the
  thing it is summarizing as a conversation it is in the middle of.

It is also honest about what it loses, and the honest part is instructive: everything before
`firstKeptEntryId` leaves the model's view, tool results are truncated to 2000 characters *while the
summary text is being built*, and the system messages inside the kept range are folded into the
summary's own header. Two more details are the kind a careful implementation has and a naive one does
not: the summary request uses a fresh routing session and, where the provider allows it, deliberately
*disables prompt-cache writes* because a one-off prompt is not going to be reused; and on a second
compaction the summarized span starts at the *previous* `firstKeptEntryId` rather than at the previous
compaction entry, so messages that survived the first pass are summarized again rather than skipped.

The design questions that remain are flint's own -- whether the request then carries the summary
instead of the turns, whether `/compact` takes instructions, and how a person can see at a glance what
is being sent -- and they are the reason this belongs in `ROADMAP.md` as an argument before it belongs
in the code.

### 3.8 Cache and cost, on screen

**The cache half is built, 2026-09-17; the cost half is still declined.** Pi's footer shows input and
output tokens, cache reads, cache writes, the latest cache hit rate, cost, and context usage. Flint's
`/usage` prints prompt, completion and total for the last request, and the turn footer prints the same
three -- and now appends the hit rate.

**What was taken: the rate, beside the counts it is a share of.** `Usage` gained
`cache_hit_tokens: Option<u64>`, read from either shape an endpoint uses for it (DeepSeek's
`prompt_cache_hit_tokens`, OpenAI's `prompt_tokens_details.cached_tokens`) because they mean the same
thing and a reader should not have to know which endpoint answered. The rate itself is computed where
it is printed -- `cache 87% (871 of 1000 prompt tokens)` on `/usage`'s own line, `, 87% cached` in the
footer under every answer -- and never stored, for the reason the repository gives for everything else:
a stored percentage is a number that can disagree with the two numbers it came from, and the file is
hand-editable. The `Option` is the whole design in one type: an endpoint that reports tokens and says
nothing about caching must produce **no cache text at all**, because `0% cached` would accuse the prompt
of being unstable when the truth is that this endpoint does not say. Two smaller decisions came out of
building it: the rate is bounded at 100 and refuses a prompt of no tokens (a provider reporting more
hits than tokens is wrong, and repeating `130%` would spread the mistake), and a resumed conversation
now starts with the last `usage` line in its file already in force -- that field had been parsed out of
the file and never read, so `/usage` used to answer "no usage reported yet" about a conversation whose
size was written down in the file it had just opened.

**What was read and not taken: the money.** `ROADMAP.md` B5 still stands -- flint does not know what a
token costs on the endpoint it was pointed at. Pi knows because it generates a model registry from
`models.dev` and OpenRouter, which is a dependency and a derived table flint would not take. The
flint-shaped version remains a price in the provider's own config table -- the person's own number, not
flint's guess -- and it is deliberately not built now: the rate is the half that answers "is the prompt
I build stable", and a price with no registry behind it is a new config key plus arithmetic that would
need its own argument. The author of Pi is candid about the same difficulty from the other side: some
providers report usage only when the stream ends, which makes exact cost impossible for a request that
was aborted, so it would have to be labelled as what the provider said.

**Why the rate is the interesting number, and what flint already does about it.** It is the one number
that says whether the prompt flint builds is stable, and `debug prompt-input` is the other half of the
same question. Pi's design is shaped around that number in a way worth noticing: mid-conversation system
messages are delivered in place "so the cached prefix stays intact", a tool that is added or removed
mid-session is announced as a patch rather than by rebuilding the prompt, and a tool that is removed
stays declared -- all so that the prefix the provider has already cached does not change. Read against
flint's own construction, flint is on the right side of this by accident of its design rather than by
tuning: the system prompt is built once per agent out of facts that are properties of the *run* (the
shell, the working directory, where flint keeps its config and sessions, the AGENTS.md note, the skill
names) and holds **no clock, no date and no per-turn state**, so within one conversation the only thing
that moves is the conversation appended after it. The ways to break that prefix are the ones worth
knowing about and they are all deliberate: a mid-run `/reload`, `/model` or `/provider` rebuilds the
prompt and rewrites the first message, and a model switch invalidates the cache wherever it lives
anyway. **Not measured here**: a real endpoint's number. Every figure in this document is from a page
that was loaded or a measurement on this machine, and the cache rate for a real DeepSeek or OpenAI
conversation is owed -- the honest way to get it is two short turns against a provider with a key, and
`/usage` prints what it says. What is measured is the plumbing: both field shapes parse into one number,
the rate is right for the values in the tests, and an endpoint that reports no split produces no cache
text anywhere.

One more accounting fact from Pi's tool contract is worth checking against flint rather than copying:
a tool result carries a `usage` field "for nested LLM work performed by the tool", and it counts toward
the session's totals. A `task` child spends real tokens on somebody's account; flint's handle carries
the child's usage, and whether the parent's own total includes it is a question this document leaves
open because it was not the one being read for.

One more accounting fact from Pi's tool contract is worth checking against flint rather than copying:
a tool result carries a `usage` field "for nested LLM work performed by the tool", and it counts toward
the session's totals. A `task` child spends real tokens on somebody's account; flint's handle carries
the child's usage, and whether the parent's own total includes it is a question this document leaves
open because it was not the one being read for.

### 3.9 A child process should know what run it is in

Pi sets `AI_AGENT=pi` and `PI_CODING_AGENT=true` so that tooling can attribute a process to the agent
that started it, and gives every command its `bash` tool runs `PI_SESSION_ID`, `PI_SESSION_FILE`,
`PI_PROVIDER`, `PI_MODEL` and `PI_REASONING_LEVEL`. Flint is halfway there with the same idea: a `task`
child gets `FLINT_DEPTH` and `FLINT_PARENT`, deliberately as environment variables so a model cannot
choose or omit them. What a `bash`/`pwsh`/`exec` command gets today is the proxy variables and nothing
else -- so a script the model writes cannot tell that it is running inside a run, which session it
belongs to, or which model is paying for it. Adding the session path and the provider to the command's
environment is a few lines in the same builder that already sets `HTTP_PROXY`, and it costs nothing at
all on the turns that do not look.

**Built, 2026-09-17, and the argument above is the one that was acted on.** `FLINT_SESSION` (the
conversation's file), `FLINT_PROVIDER` and `FLINT_MODEL` are set on every command a run starts: the
model's `bash`, `pwsh` and `exec`, and a person's own `!cmd`, which goes through the same runner and
must not be told a different run. Pi's `PI_SESSION_ID` has no counterpart because flint has no id to
give: a session is its path, and there is no per-entry id yet (§3.5 is the candidate that would make
one, and §3.7 is where Pi's own entry ids pay off).

One decision was made that the paragraph above did not foresee, and it is the whole of what the first
version got wrong: **"no session" has to mean *removed*, not merely unset.** A command inherits the
environment of the process that spawned it, and this process may itself have been started by another
run's command -- a model running `flint -p ...` through `bash` is exactly that, and `flint exec` is the
door where it happens most. The first cut left an unknown name unset, which reads identically from
outside the process, and the test that plants a stale `FLINT_PROVIDER` in flint's own environment caught
the stale value arriving at the command. So every name flint owns is either written or taken away, and
`flint exec` (not a run, so no conversation to name) takes all three away. Both spawn sites now go
through one function, `apply_child_env`, which is where the proxy block had already been duplicated --
a fact that reaches a foreground command but not a background one is a fact a script cannot rely on.

### 3.10 A thinking or reasoning level

Pi has `--thinking off|minimal|low|medium|high|xhigh|max`, `/thinking`, and Shift+Tab to cycle it, and
it stores the level with the session. Flint has no control over reasoning at all: the request body is
model, messages, `stream`, `stream_options`, tools and `response_format`, and nothing else. Flint does
*read* reasoning when a provider sends it (`reasoning_content`, or `reasoning`) and stores it in the
session, where it stays part of the history -- but there is no way to ask for more or less of it, which
for a project whose whole point is control over what reaches the model is a real gap.

Pi's provider layer is also the best available census of how *un*-standard this area is, and it is
worth reading before designing the flag rather than after: the request field differs by vendor, so it
keeps a per-provider or per-model `compat` record naming the thinking format
(`reasoning_effort`, `openrouter`, `deepseek`, `together`, `qwen`, `chat-template`, and more), the
token-budget field, and whether the endpoint supports a reasoning effort at all -- with one comment
saying outright that "Grok models don't like `reasoning_effort`". The ladder is unified for the person
and translated per provider underneath, and levels a model does not have are hidden from the picker
rather than offered and refused. For flint that is the argument for a config key with a per-provider
override instead of one global flag: the value is standard, the field is not.

The second thing in this area changes a question this document was going to leave open, and it is the
reason to write it down. When the model changes mid-conversation, Pi converts the old provider's
thinking blocks to text wrapped in `<thinking>` tags, and its documentation says why the conversion can
only ever be best-effort: some providers insert **signed blobs** into the stream that have to be
replayed with the messages they belong to, and a stale signed prefix can produce persistent 400s --
which is why it carries `allowEmptySignature` and a `prefix_mismatch_behavior: "drop_block"` for the
providers that need it. Flint speaks one protocol, so it has no signature to replay and no such flag,
but it does re-send a stored `reasoning` string verbatim to whatever endpoint the person switched to.
The honest position is the one Pi's author takes: reasoning is provider-shaped, so what flint sends
after a switch should be a decision (keep it, convert it to text, or drop it) rather than a default
nobody chose. That is a measurement for §5, not a defect to fix blind.

### 3.11 RPC, and the framing lesson that comes with it

Flint's `--json` is a one-way stream: `src/ndjson.rs` is a writer, with a closed vocabulary of event
types. Pi also has a bidirectional RPC mode so that a non-Node program can drive a session, and its
documentation is emphatic about the framing, in terms worth quoting because the reason is the whole
lesson:

> Split records on `\n` only; Accept optional `\r\n` input by stripping a trailing `\r`; Do not use
> generic line readers that treat Unicode separators as newlines. In particular, Node `readline` is
> not protocol-compliant for RPC mode because it also splits on `U+2028` and `U+2029`, which are valid
> inside JSON strings.

It does not stop at the warning: the reference reader is shipped next to the specification, hand-rolled
with an explicit `\r` strip, so the thing a generic reader would have hidden is visible. If flint ever
grows a reader for its own stream, that rule -- and that pairing of the rule with the reader -- belongs
in the comment above it.

Whether flint wants an embedding protocol at all beyond `--web` and `--json` is a separate question,
and Pi's answer to it is the interesting part: one session format and one event stream serving the
terminal, a page and a caller, so that a feature which works from outside cannot behave differently
from the inside.

### 3.12 Several tool calls in one assistant message, at once

Pi runs a batch of tool calls concurrently by default: it preflights them in order, executes the
allowed ones in parallel, emits each result as it finalizes, and still delivers the tool-result
messages in the assistant's own order. A tool that cannot be run concurrently declares
`executionMode: "sequential"`, and one such tool in a batch makes the whole batch sequential. Flint
runs tool calls one at a time, in the order the model asked for them (`src/agent.rs`, a plain loop over
`outcome.tool_calls`).

Two things make this more than a speed question, and Pi names both. Concurrency is only safe if the
tools that mutate one file cannot interleave, so its extension guide ships a `withFileMutationQueue`
helper keyed by the canonical path and explains the failure it prevents: two calls read the same old
contents, compute different updates, and the later write silently wins. And the *display* has to keep
the model's order even when the completion order differs, which is a rule flint would have to keep in
`src/sink.rs` rather than discover. The gain is real for a turn that reads six files; the cost is that
"tools run in the order asked" stops being true, which is a property some of flint's own tests assume.

## 4. Where flint goes the other way on purpose

### 4.1 Background work: jobs and a panel, not tmux

The sharpest disagreement, and flint is on the other side of it deliberately. The author's reason for
having no background bash at all is that background process management "adds complexity: you need
process tracking, output buffering, cleanup on exit, and a way to send input to a running process",
and that the implementations he had seen got **observability** wrong:

> Claude Code handles some of this with its background bash feature, but its observability is poor
> (a common theme with Claude Code) and it forces the agent to track running instances without
> giving it tools to query them. In earlier Claude Code versions, the agent would forget all
> background processes after context compaction and have no way to query them, so you had to
> manually kill them.

That paragraph is a specification, and flint has implemented all of it: one `Job` record for both
kinds of job, output into a log file with a path on disk, a budget enforced by a detached supervisor
whether or not anybody waits, a status that survives compaction because it is asked for rather than
remembered, `job_op` with `status`/`output`/`wait`/`stop`, and a jobs panel on the page with the
state, the output and a way for a person to end one. Pi's answer -- tmux -- is a good answer for
somebody who lives in tmux, and it is not available on Windows, which flint supports on purpose.

What to keep from his argument is the failure mode rather than the conclusion: the problem is not
having background jobs, it is having background jobs nobody can see, and any change to flint's job
handling should be judged against that sentence.

### 4.2 Sub-agents: flint's child is a readable file

Pi has no sub-agent tool, and the author's objections are specific: the child's work is invisible
("a black box within a black box"), the parent decides the child's context with no visibility, and
debugging a child's mistake is painful because the full conversation is unavailable. He also calls
fanning out several agents to implement several features in parallel an anti-pattern.

Flint's `task`/`tasks` answers the first three by construction: a child is the same binary, its
conversation is a session file under `children/`, it can be listed by path, read and resumed, the
jobs panel opens it, and the parent gets a handle -- a pid and the file the answer is being written
to -- rather than a transcript it has to interpret. `docs/agents.md` should say so in those words:
**the child is a file** is the answer to "black box", and it is a property to protect rather than a
detail.

The fourth point is worth recording as a caution rather than dismissed. Flint's `tasks` exists for
the same question asked of N things, which is narrower than parallel feature work; the author's claim
is that the wider use produces a garbage codebase, and the honest response is to keep the narrower
description in `docs/agents.md` so the wider one does not arrive by accident.

### 4.3 MCP: his numbers support the deferral

`ROADMAP.md` defers MCP. Pi refuses it outright, and the numbers in the author's post are the
strongest argument for the deferral: Playwright MCP is 21 tools and 13.7k tokens (6.8% of Claude's
context) and Chrome DevTools MCP is 26 tools and 18.0k tokens (9.0%), all of it in context whether
the session uses those tools or not, against 225 tokens for a README the model reads only when it
needs it. The alternative shape he argues for -- a CLI tool with a README, invoked through `bash`,
read on demand -- is what flint should prefer for any new capability, and it is worth saying so where
the ROADMAP records the deferral. Flint's built-in `search` tool is the counter-example that keeps
the rule from being absolute: a capability whose credential and request shape flint owns is better
built in.

### 4.4 Extensions: no plugin ABI

Pi's answer to almost every "why is this not built in?" is an extension: TypeScript modules that
register tools, hook tool calls, replace compaction and draw UI, hot-reloaded, installable from npm
or git as a package. It is a good design for a Node program and it is the one thing here flint cannot
copy: a plugin ABI in a single static binary means a scripting dependency, a stable Rust plugin
interface, or an out-of-process protocol -- each a larger project than the features it would enable,
and each weakening "one binary, no derived state".

If flint ever wants hooks, the flint-shaped version is not a plugin but a **declared command**: config
names a shell command, flint runs it on a named event with the event as JSON on stdin. Plain files,
no ABI, no build step, inspectable with `cat`. The shape already has precedent in the tree -- a
provider's `start` and `stop` are exactly that, a command named in `config.toml` that flint runs at a
moment it decides (`src/engine.rs`) -- which is the argument for it rather than against it.

### 4.5 Containers instead of grants, as the pragmatic answer

Pi has no permission system and says so, and then documents three ways to get a boundary anyway: a
local micro-VM that keeps the agent's credentials on the host, plain Docker, and a policy sandbox. It
is also the more honest of the two positions in one place: the author says outright that the middle
ground is theatre, because an agent that can read files, run code and reach the network cannot be
contained by asking it politely -- and names the residual risk himself ("it can use `curl` or read
files from disk, both of which provide ample surface area for prompt injection attacks"). Flint has
the same stance and no page about the alternative. Whatever `docs/sandbox.md` decides about grants, a
short note on running flint inside a container or a VM is the honest complement to a README that says
there is no boundary -- a page of prose rather than a feature.

There is one more thing worth copying in the same area, because flint is in a better position than Pi
and has not said so. Pi needs a trust step before a project's own extensions may run, because a
project-local extension is code. Flint's project-local files are `AGENTS.md`, `.flint/skills/` and
`.flint/agents/` -- text that goes into a prompt or describes how to start a child. Nothing in a
checkout executes because of flint, which is why flint has no trust prompt to build and why
"**flint never creates that marker**" is a stronger statement than it looks. That belongs in the
README next to the missing permission layer, where a person will look for it.

### 4.6 Four tools is a measurement, not a religion

Pi's four tools are backed by an argument about decision cost (fewer choices, fewer wrong ones) and
about prompt size, and by a Terminal-Bench 2.0 run the author submitted to the leaderboard. His own
words for why the small prompt works are worth keeping, because they are a claim about models rather
than about taste: "all the frontier models have been RL-trained up the wazoo, so they inherently
understand what a coding agent is. There does not appear to be a need for 10,000 tokens of system
prompt" -- followed by the measurement, under 1000 tokens for the prompt and the tools together.

Flint's set is larger -- bash, exec, read, write, edit, patch, list, glob, grep, task, tasks, job_op,
fetch, pwsh, and conditionally search and skill -- for reasons it can state: a patch format it
validates itself, a read-before-mutate gate, children, jobs, and capabilities whose credentials it
owns. The argument to watch is the one about size, because every schema is in every request and a tool
the model must consider but rarely uses is a tax on every turn. §5.1 and §5.3 are how to check rather
than argue.

## 5. Experiments worth running, with numbers

1. **What flint's prompt and tool schemas cost.** `flint debug prompt-input` prints the exact request
   body. Compare the system prompt plus tool definitions against Pi's self-reported "under 1000
   tokens", and look for sentences that are hand-holding rather than project facts.
2. **The cache hit rate over a long session.** Flint records usage per turn; the missing half is the
   cache read/write split. A stable prompt shows up as a high hit rate, and a prompt assembled in a
   different order every turn shows up as a low one, which is a bug worth finding rather than a
   curiosity. Pi's own design is shaped around keeping that prefix byte-identical, which is a standard
   flint can check itself against.
3. **Tool count against tool choice.** §3.3 is worth building partly because it makes this
   measurable: the same task, same model, with and without the tools a run rarely uses.
4. **What an aborted request did to the accounting.** Pi's aborted messages keep partial content *and*
   partial usage, and can be added to the context and continued -- which shows the number is
   obtainable, not that flint obtains it. Flint keeps the half of an answer it drew; whether the usage
   and finish reason of the request it dropped are recorded is an open question, and an aborted
   request is exactly where cost accounting is most likely to be wrong.
5. **A provider switch with stored reasoning in the history.** §3.10 asks whether the reasoning field
   flint re-sends verbatim is accepted, ignored or rejected by a provider that did not produce it. A
   test with the stub provider can only show what flint sends; the answer for a real endpoint needs a
   real switch, which is the kind of thing worth measuring once and writing down.
6. **Whether the tool list is stable across a session.** Pi patches a changed tool set instead of
   rebuilding the prompt, because the rebuilt prefix is a cache miss. Flint's tool set varies by
   configuration and by whether `search` has credentials, so the question is whether it can change
   *within* one conversation -- a `/config set` or a `/provider` switch is enough to find out.

## 6. What was read, and what could not be

Pi's own pages, all loaded in full:

- `github.com/Earendil-Works/pi` -- the repository README, including the Philosophy section quoted in
  §1.
- `packages/coding-agent/README.md` -- the CLI, sessions, modes, customization, the message queue, the
  environment variables a tool's commands are given, and the CLI reference.
- `packages/coding-agent/docs/`: `session-format.md` (the entry shape, the tree, the cursor),
  `compaction.md` (the trigger, the five steps, the losses), `rpc.md` (the framing rule and the command
  vocabulary), `extensions.md` (what can be hooked), `skills.md` (progressive disclosure, invocation),
  `models.md` and `providers.md` (the `compat` census).
- `packages/ai/README.md` (providers, the cross-provider handoff, abort and partial results) and
  `packages/agent/README.md` (the loop, the two queues, the hooks) and `packages/tui/README.md`
  (retained rendering, synchronized output, the opt-in alternate screen).
- `mariozechner.at/posts/2025-11-30-pi-coding-agent/` -- the author's own account, which is where the
  reasoning behind every refusal in §1 and §4 is quoted from.
- `mariozechner.at/posts/2025-11-02-what-if-you-dont-need-mcp/` -- the MCP argument, including the
  per-server token counts and the size of the README alternative.

One fetch note, because the rule in this project is that sources are stated rather than implied: the
author's post answered intermittently from this machine while this document was being written -- one
attempt failed before another returned the page -- and a Chinese translation of it
(`learnblockchain.cn/article/27095/`) was read alongside, as a cross-check on the parts quoted here.
Every quotation in this document is from a page that was loaded; the two figures that come only from
the MCP post are the MCP post's own.

Not read, and therefore not claimed: the source code of `pi-ai`, `pi-agent-core`, `pi-tui` or
`pi-coding-agent`, its RFCs, and Pi itself in any running form. Where this document says "Pi does X",
it means "Pi's own documentation says Pi does X".

Not read, and therefore not claimed: the source of `pi-ai`, `pi-agent-core`, `pi-tui` or
`pi-coding-agent`, its `docs/` directory, its RFCs, and Pi itself in any running form. Where this
document says "Pi does X", it means "Pi's own documentation says Pi does X".
