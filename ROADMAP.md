# Roadmap

Where flint is going, in the order the work should happen, and why that order. This file is
the queue. The reasoning for each design lives next to the design itself:

| Document | Answers |
|---|---|
| [`HANDOFF.md`](HANDOFF.md) | what state the tree is in, and what the last session learned |
| [`docs/decisions.md`](docs/decisions.md) | why flint is built the way it is, decision by decision |
| [`docs/windows-tooling.md`](docs/windows-tooling.md) | the measured record of the Windows command line, and what each fix cost |
| [`docs/web-mode.md`](docs/web-mode.md) | the plan for the browser view (`--web`); levels 1 and 2 are built |
| [`docs/deepseek-search.md`](docs/deepseek-search.md) | what a DeepSeek-keyed web search measured, and what it costs |
| [`docs/session-format.md`](docs/session-format.md) | the session file, for readers and for hand-editing |
| [`docs/windows.md`](docs/windows.md) | field notes on the Windows terminal, labelled by what was measured |
| [`AGENTS.md`](AGENTS.md) | the ground rules, and where flint keeps its own state |

## How work is planned here

- **One concern per commit**, with the reasoning in the message. The commit log is the
  review, because the project has one author and no reviewers.
- **Design before code** when the design is the hard part. Writing a plan down is a real
  state to stop in, and a labelled one is worth more than a guess: the Windows work sat
  written-down and unimplemented for several sessions, and when a Windows machine turned up
  the measurements settled in an afternoon what reasoning had got wrong in five places.
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

### 5. The Windows command line — **done**

[`docs/windows-tooling.md`](docs/windows-tooling.md) is the full design and now the measured
record: why a command string loses quotes and backslashes, what a tool can and cannot delete
from the problem, and five commits in order. All five are in the tree, and the parts that
could only be settled on a Windows machine were settled on one (Windows 10.0.26200, rustc
1.98.1, PowerShell 5.1 as the only PowerShell on `PATH`).

**The five steps:**

- the shared runner: `run_program_streaming` is the implementation, `BashTool` its special
  case, so the timeout, the progress reporting, the spill and the kill have one owner;
- **`exec`** — a program and its arguments as an array, on every platform rather than
  Windows only, with `stdin` for payloads that are not arguments. This is the largest single
  item in the queue and the one that deletes the most of the problem rather than working
  around it;
- the `\` separator fix in `glob`/`grep` (§6.3), which was a wrong answer rather than an
  error — the expensive kind — and which the document itself described incorrectly in two of
  the three places it named;
- **`pwsh`** — the script written to a BOM'd `.ps1` and run through `-File` with
  `-ExecutionPolicy Bypass`, both of which turned out to be required rather than prudent;
- the process-tree kill (§6.1), the child output encoding (§6.2, built as code-page decoding
  because `chcp` is shared console state and Python ignores it), the PowerShell facts in the
  system prompt (§6.9), the reserved-name refusal (§6.6) and the `/web` browser line (§6.10).

**What the measurements changed**, which is the part worth reading before touching any of it:

- the documentation was wrong about which file names bite through `std::fs`: `NUL` wrote
  nothing and reported success, `trailing.` became `trailing`, and `a:b.txt` became an
  alternate data stream, while `CON`, `NUL.txt` and a 1619-character path all worked;
- `-Command` does **not** mangle PowerShell quoting, so the file handover is for
  artifact-ness and the command-line limit, not for correctness;
- the browser line was broken for the same reason as everything else in the document: std
  quotes an argument only when it has a space in it, and a URL does not.

**Left open, deliberately, and recorded in the document**: a Unix process-group kill for
work a command backgrounds (§6.1) and the line-ending sentence for `apply_patch` (§6.4).
Neither is a Windows item, and both need a test before they need code.

### 6. The transcript as cells — **step 1 measured**

Three steps: measure the transcript as cells, paint only what changed, then re-render for
real on resize. This is also what deletes the interim state the clock fix left behind —
`begin_answer`, `last_segment_text`, the `committed` count and `fresh_segment` all exist
because the strip is a text offset rather than a model. Deliberately after the Windows
stream: it is a rewrite of the rendering core, it touches the byte-exact terminal tests, and
it wants a session with room to hold the whole layout in mind.

**Step 1 is measured**, 2026-09-14, by
`tests/term_capture.rs::measured_cost_of_streaming_an_answer` — an `#[ignore]`d measurement
(`cargo test --test term_capture -- --ignored --nocapture measured_cost_of_streaming`), so it
runs the real painter through the capture path instead of a model of it. One 40-line answer of
2,191 characters, streamed in a growing number of deltas, the *same final screen* every time
(`scripts/vtscreen.js` output is byte-identical across all five), so the only thing that varies
is what it cost to arrive:

| deltas | bytes | row writes | row erases | characters painted | × the answer |
|---|---|---|---|---|---|
| 1 | 5,557 | 80 | 75 | 2,224 | 1.0× |
| 4 | 6,186 | 101 | 90 | 2,413 | 1.1× |
| 16 | 9,212 | 185 | 143 | 3,373 | 1.5× |
| 64 | 20,210 | 404 | 253 | 7,507 | 3.4× |
| 256 | 56,813 | 1,124 | 434 | 21,973 | **10.0×** |

A delta is charged for **rows, not for the text it carries**: the marginal cost is about 78
characters painted and about 200 bytes per delta, whether that delta carried two characters or
nine. Answer text arriving whole costs 1.0× its own length, which says the painter is not
wasteful about a single frame — the waste is entirely in *repeating* one, and it scales with how
finely the answer was chopped rather than with the answer. That is what a text offset does: it
has no way to know that 77 of the 78 characters it just drew were already on the screen.

It also sets step 2's acceptance rule, which is what the measurement was for: **the same final
screen — byte-identical under `scripts/vtscreen.js` — at a cost that grows with the text and not
with the delta count.** The 256-delta row is the one to watch, and the byte-exact tests in
`tests/term_capture.rs` already pin the screens it must not change.

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

**Done: all three levels.** The static viewer, level 2 — `--web [--port]` and `/web`, `GET /`,
`GET /session`, `GET /events` as SSE with a cursor and replay, the `status` event that keeps a
browser from looking frozen during a slow turn — and level 3, `POST /message` and `GET
/sessions`: the page is a composer with a sidebar of conversations. Step 7's measurement pass is
done too, in a pty and in a real browser over the DevTools protocol, and it is what found the
**seven** defects §11 of that document records. Three came from one decision — `paint` rebuilt
the whole transcript on every frame, which cost the scroll position, the open `details` and any
selected text, and *selecting the answer* is the promise the page exists to keep. One was worse:
a `reset` mid-turn destroyed what the page was showing (1,424,691 characters on screen, 64,999
after), because `/session` serves a file that cannot have an in-flight answer in it yet.

**One limit is recorded rather than fixed**: painting is still quadratic in the size of the
answer — 85 KB/s rendered when small, 17 KB/s past a megabyte — because the browser lays out one
very large text node every frame. A real model emits ~300 bytes a second, so it is two orders of
magnitude from mattering.


**And `/web` is now how level 2 is actually reached — and it opens the page.** `--web` has to be
decided before the run starts, and the moment you want a real renderer is mid-answer. Measured
from a real session, the first thing anyone did was type `--web` *at the prompt*, where it is a
flag and not a command, so it went to the model and came back as a sentence — and the complaint
was precisely that no page opened. So: a bare flint flag at the prompt is translated into the
command it names (`--web` → `/web`, `--provider x` → `/provider x`), `/web [port]` opens the
same listener from inside a conversation, `/web` twice reports where the view already is rather
than binding a second one, the browser is actually launched, and `/new`, `/resume` and
`/reload` move an open page to the conversation the terminal moved to. Only the *exact* flag is
caught, so a pasted bullet list and a sentence that mentions a flag both still reach the model.

**And the text now arrives while it is written.** Deltas used to be buffered per attempt so a
retry could discard them, which meant a terminal, a browser and a `--json` reader all saw
nothing until a whole response had arrived — measured at 128 deltas over 5.5 seconds arriving
as one burst at the end, and it made a local reasoning model feel far slower than it is, since
most of a turn is text nobody could see. The retry rule is now explicit: the ladder stops at
the first drawn character, because nothing in the chain can take text back. `docs/web-mode.md`
§11 has the measurement; `src/provider.rs` has the rule.

### 8. Web mode, from using it — **one fixed, three parked**

Four things turned up in the first real sessions with the browser page, after level 3 was
finished. The fourth is fixed (below); the other three are parked, because the middle one needs a
decision rather than a patch. They are listed here rather than in the small-ideas list because one of them needs a
decision about *where a command's output lives*, which is a design question and not a patch.

**A conversation archived or deleted in the terminal stayed in the sidebar — fixed.** Reported as
"the page did not refresh", and it is worse than that. The numbers in the sidebar are the ones
`/resume` takes, and they are *positions in a list*: delete one conversation and every number below
it shifts up, so a stale row sends `/resume 4` for what is now conversation five and the person
carries on talking in the wrong one. Not parked, for that reason — `/archive` and `/delete` now
push a frame named `sessions`, and only the sidebar is re-read (the transcript has not changed, so
`reset` would throw away the reader's place for nothing). Measured: deleting a middle conversation
moved the page's rows from `1=gamma 2=beta 3=alpha` to `1=gamma 2=alpha 3=(empty)`, which is
exactly the terminal's own numbering.

**A conversation that has not happened yet is in the sidebar.** Start flint and type `/web`: the
list already shows one conversation, labelled `(empty)`. That is the file `SessionWriter::create`
opens at startup, and it is honest — the file *is* the session from the first moment, which is what
makes `/new` and `--continue` and a crash all behave — but it reads as a conversation when nothing
has been said. Three ways out, and they are not equivalent: do not create the file until the first
message (then "the session is the record from the start" stops being true, and a crash before the
first message leaves nothing); keep the file and hide empty sessions from the *list* (the sidebar
and `/sessions` already share one listing, so it is one change in one place); or show it and mark
it — dim, or "new conversation", or leave it out until it has a first message. The second is
probably right, and the thing to decide first is whether `/resume` should also stop offering it.

**A command typed into the composer prints nothing.** `/provider`, `/config`, `/usage`, `/tools`,
`/sessions`, `/skills`, `/model`, `/name`, `/readonly`, `/verbose`, `/detail`, `/reload`, `/help` —
all of them answer on the terminal, and the page shows none of it, so the composer is a prompt for
messages and not for commands. The cause is structural: command output goes to `printer.term()`,
which draws in the terminal and exists nowhere else. It is neither a session event (commands do
not write to the session file) nor a live frame (the feed carries the *turn's* events), and the page
renders exactly those two. So this is the same class of gap as the one `turn.started` closed for
user messages, and it needs the same kind of answer: either command output becomes something the
feed carries, or the page gets a second source for it. Worth deciding before building, because
"every line the terminal prints" is a large surface — the notice sink, the status line and the
usage summary all take that path.

**Renaming a conversation from the sidebar.** Not tried by the person who asked, and the mechanism
is already there: `/name <text>` appends a `title` line, the page already *renders* titles (its
header shows one), and typing `/name x` in the composer works today. What is missing is an
affordance and any feedback — and the feedback is the previous item. So this one is small once that
is decided, and it is not worth doing first.


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
three `eprintln!` sites that can land inside the answer strip, and a transcript that is not
trimmed by construction.
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
