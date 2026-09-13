# Roadmap

Where flint is going, in the order the work should happen, and why that order. This file is
the queue. The reasoning for each design lives next to the design itself:

| Document | Answers |
|---|---|
| [`HANDOFF.md`](HANDOFF.md) | what state the tree is in, and what the last session learned |
| [`docs/decisions.md`](docs/decisions.md) | why flint is built the way it is, decision by decision |
| [`docs/windows-tooling.md`](docs/windows-tooling.md) | the settled, unimplemented plan for the Windows command line |
| [`docs/web-mode.md`](docs/web-mode.md) | the plan for the browser view (`--web`); levels 1 and 2 are built |
| [`docs/deepseek-search.md`](docs/deepseek-search.md) | what a DeepSeek-keyed web search measured, and what it costs |
| [`docs/session-format.md`](docs/session-format.md) | the session file, for readers and for hand-editing |
| [`docs/windows.md`](docs/windows.md) | field notes on the Windows terminal, labelled by what was measured |
| [`AGENTS.md`](AGENTS.md) | the ground rules, and where flint keeps its own state |

## How work is planned here

- **One concern per commit**, with the reasoning in the message. The commit log is the
  review, because the project has one author and no reviewers.
- **Design before code** when the design is the hard part. Writing a plan down is a real
  state to stop in: `docs/windows-tooling.md` is settled and not implemented, and that is
  better than half-implemented with the reasoning lost.
- **A test that has never been red has not been shown to test anything.** Write it, watch it
  fail for the right reason, then fix the code.
- **Nothing is finished until all three pass**: `cargo test`, `cargo clippy --all-targets`
  (silent), and `node scripts/term-layout-test.js` after a fresh capture.
- **Features that cannot be verified here are labelled.** A plan that says "UNVERIFIED" is
  honest; a claim that is wrong costs a session of debugging in the wrong file.

## Done

### 1. Sessions that survive being read — **done**

Append-only JSONL, one file per conversation, named/archived/deleted without a model or a
key, format v2 with a defaulted version field, listing that reads only the two ends of each
file. Nothing is derived: the list, the resume target and the transcript are computed from
the files every time. Reference: [`docs/session-format.md`](docs/session-format.md).

### 2. Project context: `AGENTS.md` and skills — **done**

Instruction files are named in the prompt by default (`instructions = "hint"`; `paste` and
`off` are configurable), and skills in `<dir>/<name>/SKILL.md` are offered as a one-line
catalog whose bodies load on demand through the `skill` tool — which does not exist at all
when there are no skills. Nothing is cached, so `/reload` picks up a file written
mid-session. No new dependency: the front matter is parsed by hand.

### 3. Tool-loop quality — **done**

`apply_patch` in Codex's format, atomic across files, with hunks located by their own lines.
A read-before-mutate gate on `write`, `edit` and patch updates, with mtime-and-length
staleness. Honest truncation that keeps both ends and spills the whole answer to
`~/.flint/spill/<session>/<n>.txt`. Argument validation that names a wrong type instead of
reporting it as missing or quietly ignoring it. A non-blocking note at the 3rd, 5th and 8th
identical call in one turn.

### 3b. Web search — **done**

`search`, backed by DeepSeek's server-side search on its Anthropic-compatible endpoint. The
point of it is that search is a *tool* and not a capability of the driving model, so a
configuration running only a local model can search, with nothing deployed on any machine that
has a DeepSeek key. The credential is inherited from a DeepSeek provider when there is one and
named explicitly in `[search]` when there is not; the tool is offered only when it can work.
Measured record, including cost: [`docs/deepseek-search.md`](docs/deepseek-search.md).

**What it deliberately leaves undone:** `fetch`. Search returns sources, and reading one is
the next step a model wants — today the only way is `bash` and `curl`, which loses the page to
the truncation budget and shows the model raw HTML. `fetch` is the bigger and riskier half of
this pair: it is the first tool that brings outside content into a context belonging to a
program that can run commands, and it needs the SSRF defence `docs/deepseek-search.md` §4
describes before it is worth having.

### 4. Machine-readable runs — **done**

`flint -p "..." --json` writes the run as one JSON object per line on stdout, and nothing
else goes there. The session file is still written, so a streamed run resumes like any
other. Reference: the README section, and `src/ndjson.rs`.

`debug prompt-input` finishes the stream, and answers the question it cannot: the request is
**not** the transcript, because request-side pruning drops stale tool output the session file
keeps forever. It prints the body the client would post and sends nothing, built by
`provider::request_body` — the one place the request is serialised, which is what stops the
preview from describing something that is not sent. A test runs a turn against the stub
provider and requires the preview to equal the bytes the server received.

## Next

### 5. The Windows command line — **step 1, 2 and one fix done; the rest needs the machine**

[`docs/windows-tooling.md`](docs/windows-tooling.md) is the full design: why a command
string loses quotes and backslashes, what a tool can and cannot delete from the problem, and
five commits in order.

**Done, and verifiable off Windows:**

- the shared runner: `run_program_streaming` is the implementation, `BashTool` its special
  case, so the timeout, the progress reporting, the spill and the kill have one owner;
- **`exec`** — a program and its arguments as an array, on every platform rather than
  Windows only, with `stdin` for payloads that are not arguments. This is the largest single
  item in the queue and the one that deletes the most of the problem rather than working
  around it;
- the `\` separator fix in `glob`/`grep` (§6.3), which was a wrong answer rather than an
  error — the expensive kind — and which the document itself described incorrectly in two of
  the three places it named.

**Left, and it wants a real Windows session before any code is written:**

- `pwsh`, the script handed over as a BOM'd `.ps1` through `-File` (§4.2) — the
  execution-policy wrinkle is unresolved;
- the process-tree kill (§6.1) and the child output encoding (§6.2), both Windows-only
  behaviour that cannot be observed from a Unix machine;
- the PowerShell facts in the system prompt (§6.9);
- §6.6's reserved names, trailing dots and long paths.

The two Windows assertions that would matter most are also unwritten for the same reason:
that an argument survives the round trip, and that killing a command kills its children.

**If the session is not on Windows, do not start 5's remainder — start 6 or 7.** The order
below is the order for a Windows machine. 6 and 7 are platform-independent, and each is a
larger piece of work than anything left in 5, so waiting for the right machine to do the
small item first is the wrong trade. What 5's remainder needs is written down in full,
labelled, and waiting; what 6 and 7 need is a session with room to hold a design in mind.

### 6. The transcript as cells — **designed, not implemented**

Three steps: measure the transcript as cells, paint only what changed, then re-render for
real on resize. This is also what deletes the interim state the clock fix left behind —
`begin_answer`, `last_segment_text`, the `committed` count and `fresh_segment` all exist
because the strip is a text offset rather than a model. Deliberately after the Windows
stream: it is a rewrite of the rendering core, it touches the byte-exact terminal tests, and
it wants a session with room to hold the whole layout in mind.

### 7. Web mode: a window onto the running process — **levels 1 and 2 built**

[`docs/web-mode.md`](docs/web-mode.md) is the design, and the decision at its centre is that
`--web` is **a window, not a mode**: flint starts exactly as it does now and additionally
serves a view of *this* process on loopback, so the terminal keeps working, the process stays
the only writer of the session file, and closing the tab loses nothing. The browser is the
fourth renderer over the event funnel that already has three, the SSE payload is the NDJSON
that `--json` already produces, and a typed message goes into the `InputMsg` channel the
terminal already steers with.

It is **independent of 5 and 6**. The one open question — a hand-rolled HTTP server or
`hyper`, which is already in the tree via `reqwest` — was §7 of that document and is settled in
[`decisions.md`](docs/decisions.md#dependencies): hand-rolled. The port defaults to ephemeral
rather than a fixed one, because a fixed port collides with whatever else is on loopback and
the URL has to be printed anyway.

**Done:** the static viewer (level 1), and level 2 — `--web [--port]`, `GET /`, `GET /session`,
`GET /events` as SSE with a cursor and replay, and the `status` event that keeps a browser from
looking frozen during a slow turn. **Not done:** `POST /message` (level 3, typing into the
browser) and the measurement pass of step 7 — though `/web` itself is now measured end to end
in a pty (§11), which is most of what that pass was for.

**And `/web` is now how level 2 is actually reached.** `--web` has to be decided before the run
starts, and the moment you want a real renderer is mid-answer; measured from a real session,
the first thing anyone did was type `--web` *at the prompt*, where it is a flag and not a
command, so it went to the model and came back as a sentence. `/web [port]` opens the same
listener from inside a conversation, `/web` twice reports where the view already is rather than
binding a second one, and `/new`, `/resume` and `/reload` move an open page to the conversation
the terminal moved to. A bare flint flag typed at the prompt is now refused rather than sent,
with the slash command that means it as the answer — the *exact* flag only, so a pasted bullet
list and a sentence that mentions a flag both still reach the model.

**And the text now arrives while it is written.** Deltas used to be buffered per attempt so a
retry could discard them, which meant a terminal, a browser and a `--json` reader all saw
nothing until a whole response had arrived — measured at 128 deltas over 5.5 seconds arriving
as one burst at the end, and it made a local reasoning model feel far slower than it is, since
most of a turn is text nobody could see. The retry rule is now explicit: the ladder stops at
the first drawn character, because nothing in the chain can take text back. `docs/web-mode.md`
§11 has the measurement; `src/provider.rs` has the rule.

## Small, agreed, unscheduled

- `read`/`write`/`edit` taking `file_path`, with `path` kept as an alias so nothing breaks.
- `--fork`: copy a session file and continue the copy. `cp` already does this; the flag is
  about making it discoverable.
- `examples/live_turn.rs` keeps a hand-maintained copy of `run_turn`'s event handling and has
  drifted twice, costing time chasing faults that were only in the example. It should use
  the same sink the CLI does.

## Known unfinished

Five known defects are not repeated here, so that the list cannot drift apart from the
state of the tree: Windows newline and code-page behaviour, CI that checks nothing on push,
three `eprintln!` sites that can land inside the answer strip, a transcript that is not
and a transcript that is not trimmed by construction.
[`HANDOFF.md`](HANDOFF.md#known-unfinished) has each in detail, labelled by what was
measured and what was not.

## Not doing, and why

- **A permission layer** — no approval prompts, no allow-list, no sandbox. An approval dialog
  in an emergency is friction at the worst moment, and a permission system that is not
  airtight is worse than a documented absence. `readonly` stays all-or-nothing, and the
  README and `AGENTS.md` say plainly that flint can damage the machine.
- **Subagents** — flint is one conversation and one context window. Splitting it invents
  coordination, budgets and merge problems that a rescue tool does not need.
- **MCP** — deferred, not refused: it is a protocol with real weight, and nothing here yet
  needs what it offers.
- **`flint doctor`** — a second program inside the first, with its own failures, to check
  things flint can already check by running them.
- **An index, a cache, a database, compression** — anything that makes the state on disk
  unreadable by a person, or true only until it is stale.
- **A YAML parser, a date library, a TUI framework, `serde_json`'s `preserve_order`** — each
  is a dependency bought to avoid a small amount of code. The front matter of a `SKILL.md`
  is parsed by hand; timestamps are `epoch:<seconds>`; NDJSON keys come out in a stable order
  rather than a chosen one.
- **A PowerShell parser, and `-EncodedCommand`** — reasons in §7 of the tooling document.
- **A remote or multi-user web view, and a build step for the page** — the browser view binds
  to loopback with a per-run token and an `Origin`/`Host` check, none of it configurable,
  because a listener on a program with no approval prompts is the one place where the quiet
  boundary has to be a real one. The page itself stays one hand-editable file with no pnpm,
  no bundler and no framework; the reasons are §4 and §5 of [`docs/web-mode.md`](docs/web-mode.md).
