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

### 6. The transcript as cells — **all three steps landed**

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

A delta was charged for **rows, not for the text it carries**: the marginal cost was about 78
characters painted and about 200 bytes per delta, whether that delta carried two characters or
nine. Answer text arriving whole costs 1.0× its own length, which says the painter is not
wasteful about a single frame — the waste was entirely in *repeating* one, and it scaled with how
finely the answer was chopped rather than with the answer. That is what a text offset does: it
has no way to know that 77 of the 78 characters it just drew were already on the screen.

It also sets step 2's acceptance rule, which is what the measurement was for: **the same final
screen — byte-identical under `scripts/vtscreen.js` — at a cost that grows with the text and not
with the delta count.** The 256-delta row is the one to watch, and the byte-exact tests in
`tests/term_capture.rs` already pin the screens it must not change.

**Step 2 landed**, 2026-09-14, and the same measurement after it:

| deltas | bytes | row writes | row erases | characters painted | × the answer |
|---|---|---|---|---|---|
| 1 | 5,557 | 80 | 75 | 2,224 | 1.0× |
| 4 | 6,166 | 101 | 90 | 2,403 | 1.1× |
| 16 | 9,098 | 171 | 129 | 3,338 | 1.5× |
| 64 | 14,017 | 338 | 176 | 4,554 | 2.1× |
| 256 | 21,772 | 882 | 357 | 4,928 | **2.2×** |

Bytes per delta at 256 fell from 222 to 85, and painted characters from 10.0× the answer to
2.2×. Three paints changed:

- **A row that is already right is not rewritten.** The frame compares each strip row against
  what it held last time, per *screen row* rather than by index into the slice — the slice
  slides as the answer grows, so an index is not a row.
- **A row that grew is written from the first difference on**, positioned at the column that
  difference starts in, instead of the whole row from column 1.
- **The slide itself is delegated to the terminal.** When the slice's window moves, every row
  on screen changes content; `CSI S` inside the strip's own scroll region moves those cells
  instead, and only the rows that are genuinely new are painted. The rows that scroll out at
  the top are the ones already handed to the transcript a few lines earlier, so nothing is
  lost by moving them.

**2.2× is the floor, not a shortfall.** The measurement splits the painted characters by
region: the transcript costs a constant **2,158** characters at 1, 64 and 256 deltas, and the
strip costs the answer's own **2,191**. Every row is drawn twice because it *is* drawn twice —
once while it is the visible tail of the streamed answer, and again when it scrolls out into
the transcript above, which is the same text at two screen positions at two times. The
single-delta run never streams through the strip and so pays 54 characters there. What matters
is that the second number does not grow: 2,184 characters at 64 deltas and 2,196 at 256, and
`streaming_in_many_deltas_paints_only_what_changed` asserts exactly that (four times the
deltas, under 1.25× the paint; it was 2.9× before this change).

Two things this leaves behind for step 3, both now visible because the painter has a model of
what is on screen:

- The strip's rows are still an **absolute-address rewrite** driven from `stream_rows`, not a
  cell grid. `stream_rows`, `stream_first` and `previous_drawn` are the beginnings of that
  grid; the interim text-offset state (`begin_answer`, `last_segment_text`, `committed`,
  `fresh_segment`) is untouched so far.
- The **paused resize** below.

**Step 3, measured before it was written — half of it is already true.** A resize wraps the
answer differently, and the strip remembers rows wrapped at the width in force when they were
drawn, so it looked like the obvious next job was to tag the cache with a width and throw it
away when `screen_cols` changed. Captured and replayed, both directions already come out right:
70 → 40 columns re-breaks the text into more rows and writes every one of them from column 1,
and 40 → 100 re-breaks it into fewer and clears the rows the answer no longer reaches, both
ending with the strip showing the new wrapping and nothing of the old. The reason is worth more
than the fix would have been: the painter never *trusts* a row it remembers. It writes the new
row's text from the first character that differs and erases what the old row had past the end,
so a row remembered from another width is overwritten rather than believed, and a width tag
would be state kept for a case the diff already handles.

That is also why the test for it was not kept. It was written, and then three mutations were run
against it — never erasing a departed row, trusting every remembered row, and mapping
remembered rows by index instead of by screen row — and **all three passed**. The reason is the
same one that makes the behaviour correct: the scenario's old and new rows share so little text
that even the wrong mapping writes from column 1. A gate that cannot be shown to fail is not a
gate, so it is not in the tree; what is here is the measurement and the reason.

What step 3 still owes, and the shape of it:

- **A resize with no fragment after it left the old wrapping on the strip — fixed**, 2026-09-14.
  The strip is re-drawn when the next fragment arrives, which is milliseconds later in a live
  stream and *never* if the answer is paused between rounds, so a paused answer sat on a strip
  wrapped for a window that no longer existed. The fix is not to re-wrap it: the transcript
  above already holds part of that answer in the old wrapping, and committing a re-wrapped
  remainder against a `committed` count that counts rows of the old one writes text the
  transcript already has, which is how a sentence ends up in the scrollback twice. Instead the
  resize **closes the in-flight answer first**, while the width it was drawn at is still the
  width in force: `close_stream` hands the transcript what only the strip had, empties the
  strip, and the next fragment starts a fresh segment wrapped for the window the reader now
  has. `a_resize_closes_the_answer_that_was_still_arriving` is the gate, red before the change
  because the handler only recomputed the layout and repainted the input row.
  What it deliberately does not do is re-wrap the rows already in the transcript: those are
  scrollback, drawn at the width they were drawn at, and the answer keeps the seam.
- The interim state is still there. Deleting it is what makes the strip a cell grid rather than
  a text offset, and it is the larger half of step 3. The character/row mismatch behind the
  paragraph above is one of the things that goes with it: `committed` counts *rows*, which is
  why a width change has to end the segment rather than continue it.

**What that rewrite has to keep**, worked out 2026-09-14 by trying to derive each piece instead
of storing it. Two of the four cannot go, and it is worth knowing why before starting:

- `segment_text` — the text as it actually *arrived* — is not `last_segment_text + stream_text`.
  That concatenation looks right and is wrong twice over: the strip drops the leading blank space
  when it strips a restated head, so the bought-back string is missing it, and when the strip
  does not fire at all (a fragment that does not repeat the committed head) the concatenation
  glues the head onto text that never followed it. It is the only record of what arrived, which
  is what the segment-boundary test compares against.
- `committed` — rows of the drawn text already handed to the transcript — cannot go either while
  answers stream *into* the transcript rather than appearing in it at the end. Incremental commit
  is the feature: it is why a long answer is readable while it arrives. So the cell model does not
  delete the boundary, it owns it.

Which makes the rewrite a consolidation rather than a deletion: one model holding the rows on the
strip, the answer row the first of them holds, the arrival, the drawn text, the handed prefix and
the committed count — instead of six fields behind three different locks whose invariants are
maintained by hand across `stream`, `close_stream`, `clear_viewport` and `begin_answer`. The
deletion the roadmap promised is the *locks and the agreeing-by-hand*, not the information.

- `stream_active` — set when a frame draws, swapped off by `close_stream` — cannot become a check
  on `stream_text` either, though it reads like one. The drawn text is empty whenever a restatement
  strips to nothing, and the answer is still in flight then: `close_stream` has to commit and blank
  and reset `committed` for that frame, and the text is not around to say so. Every piece of this
  state has turned out to be load-bearing; the rewrite moves it, it does not shrink it.

**The merge landed**, 2026-09-14, as three commits, one per unit: the screen pair
(`stream_rows` + `stream_first`) into `Mutex<Strip>`; the three text records (`stream_text`,
`segment_text`, `last_segment_text`) into `Mutex<Answer>`; then the two counters (`committed`,
`stream_active`) into `Answer` beside the text they measure. Eight fields behind five locks,
kept in agreement by hand across `stream`, `close_stream`, `clear_viewport` and `begin_answer`,
are now two models under two locks — one per question, *what is on screen* and *what was said*.

Nothing was deleted, because measuring each field showed it carries something that cannot be
reconstructed, and two of the fields the plan named as interim are not interim at all:
`begin_answer` is the turn boundary, which the `Term` cannot infer (a new turn's answer can
open with the previous turn's words), and `committed` is what lets an answer be readable in the
transcript *while* it streams. What the merge removes is the chance to read the state
half-updated: `close_stream` used to take the drawn text under one lock and its row count under
another, so a frame landing between the two left it committing rows measured against text that
had already moved on — the rows in between committed twice or dropped. It takes both under one
lock now.

The step-3 resize behaviour is the deliberate deviation recorded above: a resize closes the
in-flight answer rather than re-wrapping it, because re-wrapping commits rows against a count
measured in the old wrapping. So §6 is complete in the form the measurements allow, and the
three things the plan got wrong about how it would go are written down here rather than
quietly dropped.

**One piece of it did come out**, 2026-09-14: `stream_rows` — the painter's memory of what is
on the strip — used to be cleared by hand in `begin_answer` and again at a segment boundary,
two places that had to remember, and neither of them the place that erased the rows. Clearing
it is now part of `clear_viewport`, which is what actually blanks the strip: the cache describes
what is on those rows, so rows that have just been cleared are rows it cannot speak for.

This was found while hunting a bug that turned out not to exist. A tool line printed mid-answer
commits the answer and blanks the strip, and the answer then continues with a restatement that
is *stripped* of the committed head — so the continuation is all that is left to draw and the
strip legitimately shows one row. That is the design working, not a fault; the test written for
it asserted the opposite and was dropped. The mutation that removes every clear (the two deleted
here *and* the new one) also leaves all twenty terminal tests passing, which says the diff's
erase pass already covers these paths — so this is a simplification and an invariant made local,
not a fix, and it is recorded that way.

One trap worth remembering, because it cost an afternoon: the `\r\n` that carries the cursor
from one strip row to the next belongs to the row that was *written*. Emitted after a row that
was skipped, it moves the cursor from wherever it was parked — the input row — and a line feed
on the screen's last row scrolls the whole transcript up by one. The visible effect was the
answer being eaten a row at a time with holes appearing in history, and it was only caught by
replaying the capture frame by frame.

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

### 8. Web mode, from using it — **the read channel is in, and the page reads as well as sends**

Four things turned up in the first real sessions with the browser page, after level 3 was
finished. Two are settled (below): the sidebar bug is fixed, and the commands gap now has a
design rather than a patch. The other two are parked, because neither should be built before
that design is.

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

**A command typed into the composer prints nothing — decided**, 2026-09-14. `/provider`,
`/config`, `/usage`, `/tools`, `/sessions`, `/skills`, `/model`, `/name`, `/readonly`,
`/verbose`, `/detail`, `/reload`, `/help` — all of them answer on the terminal, and the page
shows none of it, so the composer is a prompt for messages and not for commands. The cause is
structural: command output goes to `printer.term()`, which draws in the terminal and exists
nowhere else. It is neither a session event (commands do not write to the session file) nor a
live frame (the feed carries the *turn's* events), and the page renders exactly those two.

The design, in the order the pieces depend on each other:

- **The page sends the command *line*, not a command API.** `/provider llamacpp` goes over the
  input channel the composer already uses. There is no second implementation of any command, so
  the terminal and the page cannot drift, and the tests that cover a command cover both. Every
  control below is a different way of composing that line.
- **What is missing is a read channel, not a write channel.** A picker cannot be built without
  knowing the options, and the page must not read `config.toml` itself — that is a second reader
  of state, which is what "no derived state" and "one writer" both forbid. So the event stream
  grows a `state` frame: the current provider and model, the configured providers and the models
  each offers, the enumerable toggles and their values, and the command list. Nothing secret
  goes in it.
- **The controls are four classes, and they are not one feature:**

  1. **Buttons** — no argument, one action: `/new`, `/reload`, `/usage`, `/tools`, `/skills`,
     `/sessions`, `/help`, and the bare `/provider`, `/model` and `/config`, which are *reports*
     and belong as panels rather than as buttons. `/exit` stays off the page: the page is a
     window onto a process, and a misclick should not end a session.
  2. **Selectors** — the argument is one of the settings that already exist: `/provider <name>`,
     `/model <name>`, `/resume <n|id>`, `/skills <name>`, and the enumerable toggles
     `/verbose on|off|full`, `/detail on|off`, `/readonly on|off`. A toggle is a switch that
     shows its current value, not a button that blind-toggles. `/archive <n>` and `/delete <n>`
     are selector-shaped with the argument supplied by *which row was clicked*, which the sidebar
     already has. (`/skills` came out of the building as a panel row that carries values rather than
     as a selector: it reads rather than changes, and the values are then also the permission — see
     the built paragraph for §8.)
  3. **Forms** — free-form or typed input: `/name <text>`; `/provider key <key>`, a password
     field that is never echoed back, never in a `state` frame and never in the transcript;
     `/provider add` and `/provider edit <name>`, which are the terminal's interactive wizard as
     a form; and `/config edit`, whose keys are enumerable and whose values are not (a shell
     path, `max_steps`, a proxy URL), with the file it writes named on the form. (Built as a field
     per single-value row: `/name` is text, `/provider key` is a password. The wizards and
     `/config edit` are still the terminal's — see the built paragraph for §8.)
  4. **Destructive** — `/delete`, `/archive`, `/provider rm`: a confirmation step, because there
     is no undo anywhere in flint.

- **Where the output lives is answered per class, not globally.** What an *action* answers goes
  to the transcript, which means `printer.term()` has to become something the feed carries —
  the same gap `turn.started` closed for user messages. What a *panel* shows (the provider
  list, usage, the skill catalog) is that same capture with the printing turned off: a report is a
  command's answer, sent to the page as a `command` frame marked `panel`, so the terminal is not
  flooded with a listing its reader did not ask for there. Built; the paragraph below has what it
  cost and the three decisions inside it.
- **The page may write `config.toml`**, which is what `/provider add` is for. That is acceptable
  because of what already exists rather than anything new: `--web` binds loopback and hands the
  page a per-run token (docs/web-mode.md §4), and that token is checked on every request,
  including the event stream.

**The toggle this would put a switch on did not work — fixed, 2026-09-14, and it was measured
before the `state` frame was written because a frame that *reports* `readonly` out of a command
that does not set it is the same lie in a new place.** `/readonly on` printed "no writes, no
mutating commands", the `/config` line under it printed `readonly = false`, and `config.toml` had
no `readonly` key at all: the arm worked out the value it meant to set, printed it, and set
nothing. The guard flint's own manual calls its only permission control had never been on, and a
session that believed it was guarded had full permissions — worse than a command that fails,
because it reports a permission the session does not have.

`/readonly` now writes the file and then **rebuilds the tool set**, because the flag is baked into
the tools at construction (`ToolBox::new` hands `readonly` to `write`, `edit`, `patch`, `bash`,
`exec` and `pwsh`): the guard is in force from the next tool call rather than from the next
process, which is what someone typing it wants. The rebuild is deliberately *not*
`continue_conversation`: that writes a new session file, and this is a change of permission, not
of conversation, so the agent is rebuilt around the same file, history and model
(`SessionWriter::resume`, the route `/resume` uses for the same reason). What it costs is what
every rebuild here costs — the tool box's read history starts over — and it is the consequence
already accepted for `/model` and `/provider` below. The file is written first, so what the next
run reads agrees with what this one enforces; a run guarded only by `--readonly` is reported
honestly by `/config` (this run's value, and the file's) and can be unguarded from inside by an
explicit `/readonly off`, which writes the file too. `readonly_guards_the_run_it_is_typed_into` is
red without the fix on the first of its two halves (the file) and green on both after it.

**The second toggle a switch would have gone on could not be saved either — fixed in the same
round.** `/verbose off` set the printer to QUIET and then saved `cfg.verbose = next >= CHATTY`,
which is a `bool` that cannot say "off": `false` was both the quietest setting and the default, so
the setting was chosen and then lost — the next run started *on*. `/config` was worse than
unhelpful about it: it printed `verbose = false` while the run was printing a line per tool call,
so the report agreed with the file and both disagreed with the run, which is the same lie
`/readonly` was telling, one layer down. The key now holds the word
(`verbose = "off" | "on" | "full"`), read and written through `display::Verbosity`, which sits
beside the levels it names because three things speak it — the command, the file and the page's
switch — and they have to agree. A `bool` in an existing file is still read as what it has always
meant (`false` is `on`, `true` is `full`), and a word that names nothing is refused rather than
defaulted, because that file is meant to be hand-edited. `verbose_off_is_what_the_file_records`
(the file, the run that typed it, and a second process) and `the_old_bool_for_verbose_still_reads_as_it_did`
(both spellings, and a typo) are each red without their half.

**The toggles are switches, and the frame says what they can be — built, 2026-09-14.** §8's second
class is selectors, and its smallest members are the enumerable toggles: `/verbose on|off|full`,
`/detail on|off`, `/readonly on|off`. The frame carries each one in the shape a `<select>` wants and
the command needs — a name, the values that name takes, and the value in force — so the page holds
no list of its own: not the toggles, not the words they take, and not which one is on. That is the
difference between a switch that tells the truth and one that lies, and it is why the words had to
stop being a `bool` in the file first. A switch sends `/<name> <value>` — the terminal's own line —
through the same `sendText` as everything else, and a refused send puts it back to the value in
force. A two-valued toggle is a two-valued switch, and a switch with only one value is disabled
rather than hidden, so it still says what is in force. Measured end to end in
`the_page_is_told_the_state_its_controls_would_show`, which now posts `/verbose full` over the
route the switch uses and reads the new value back off the feed; `the_toggles_are_switches_that_show_their_value`
is the page's policy (drawn from `toggle.name`/`values`/`value`, one line per change), and the page's
Node check runs the real `showToggles` over the stub DOM. What is left of the selector class is what
is not a setting: `/resume <n|id>`, which the sidebar already composes, `/skills <n|name>`, and the
sidebar rows' `/archive <n>` and `/delete <n>`, which belong with the confirmation step.

**First: the read channel — built, 2026-09-14, which is the piece every control below waits
for.** The event stream grew the `state` frame: the provider and model in force, the configured
providers and the models each offers (through `ProviderConfig::choices`, the same function
`/model` lists from, so the terminal and the page cannot come to disagree about which names are
valid), and the enumerable toggles. It is a *named* frame and not a line of the run's vocabulary,
because it is not an event — it is where things are, like `status` — and it is re-derived from the
live configuration at the top of the REPL's loop, which is the one place every command returns to.
That is what keeps it honest without being derived state: the process is the only writer of its own
configuration, and the frame is a view of it, sent only when it differs from the last one. Sent
once on connect as well, because a client with no cursor is not replayed the ring — a page opened
after the state was announced would otherwise be told nothing for the rest of the session.

The first two controls are drawn from it: a provider picker and a model picker in the header, each
sending the line it names (`/provider <name>`, `/model <name>`) through the composer's own
`sendText`, so no command has a second implementation. Both are hidden until a state frame
arrives — a page opened from a dropped file has no process behind it — and a refused send puts the
control back to what is in force, because nothing else would: the next frame only arrives if
something actually changed. The header stops printing the model and provider itself when a state
is present, since the pickers say it and offer the alternatives; a dropped file still gets them
from the session's `meta`.

**The command list waited for a reader.** A field nothing renders is a field that drifts, so the
frame grew its consumers one at a time: the toggles arrived because a picker needs their values, and
the command list arrived with the panel that draws it — which is also where the question "what
should the page offer" got its answer, since a list with no classes in it is a list nobody can build
a control from. What came first instead was the *output* half of the same design, because every class
below needs it:

**What a command answers is on the feed now — built, 2026-09-14.** The design above splits output by
class, and the missing half was the action's: a command answers through `printer.term()`, which draws
in a terminal and exists nowhere else, so a button that does something and then says nothing would
have been a button nobody could trust — and a command typed into the composer already answered into
a terminal its reader could not see. The capture is at the funnel, and only for the length of a
command: the REPL starts recording just before `handle_command` and takes the recording just after,
pushing one `{"type":"command","input":…,"text":…}` line. Capturing there, rather than converting
every `printer.term().line(…)` call site, is what made this small; scoping it by the caller is what
keeps a *turn*'s lines out, because they come through the same funnel and are already frames of their
own — recording them here would send every tool result to the page twice. The recorded text is
stripped of the printer's colour codes at the same point, since a browser draws escapes rather than
obeying them. The line is not one of the turn's events — a command runs *between* turns, and its
output is deliberately not in the session file — so `Live::command` pushes it directly rather than
widening `event::Event`, which is documented as the vocabulary of a turn. The page renders it as a
transcript block with the input as its label: an answer with no question above it is a mystery, and
the answer is often a listing.

One bug had to be fixed before the channel was worth anything, and it is the reason the page could
never have been trusted with a button: **a command that failed ended the session.** The arms return
`Result`, and the REPL loop handed that error out of `interactive` with `?`, where it became the
process's exit status — `/verbose loud`, one word wrong, printed `flint: error: …` and took the
conversation with it. From the page it was worse rather than better: the composer sends lines to the
same place, so a mistyped command in the browser ended the run the page was watching, and the browser
could not even say why, because the message went to stderr. A failure is an answer now — printed on
the terminal in the shape the input reader's own refusal already used, and carried to the page like
any other — and it is one `line` call per line of the message, since a single call carrying a newline
moves the cursor down through the rows the layout reserved.

Gates. `a_command_that_fails_does_not_end_the_session` was red on the second of its two assertions
(the failure was reported *and* the session ended); `the_page_is_told_what_a_command_answered` needs
a real process — `/config` over `POST /message`, its answer read off the live `/events` stream, then
`/verbose loud` answered rather than fatal, and a state frame afterwards proving the run is still
there. In-crate, `what_a_command_says_is_recorded_for_the_page_as_plain_text` (recorded plain, and
not recorded after the take) and `both_escape_forms_a_terminal_uses_are_stripped` pin the two halves
of the funnel; `a_command_answer_is_a_block_with_the_line_that_asked_for_it` is the page's policy,
and the Node check paints a real command frame and reads the label and the body back. Two of them
were mutation-checked rather than watched red, because the code came first: neutering `Live::command`'s
push fails the e2e on "the page was never told what the command answered", and neutering the
colour-stripping fails the unit test on the escape it should have removed.

**The page knows what commands there are — built, 2026-09-15.** The trap named below was the third
copy, and the answer was to have no second one: `COMMANDS` in `src/main.rs` is now the list, and
`/help` prints it. The check that this is real rather than asserted is behavioural, in
`tests/cli_output.rs`: the frame's own `send` strings are posted back through the route the composer
uses and the answers are read, so a command that has been renamed fails the test the moment the
table is edited — `every_command_the_page_may_offer_is_one_the_terminal_takes`, and the same test
carries a floor on how many rows it checked, because a frame with nothing in it would otherwise pass
the loop in silence. The other half of the pair reads *both* renderings — the frame off the live
stream, `/help` off the run's own stdout — and asserts every row the page was handed is a row the
terminal prints, with the same description: the two are one table, and this is the test that would
notice if somebody made them two again.

An entry is `{label, send, help, class}`, and the two string fields are the interesting decision.
`label` is what `/help` prints (`/provider key <key>`); `send` is what goes on the wire
(`/provider key`). Splitting a label into a name and an argument list would have been the obvious
shape and is wrong, because the split is not uniform: `add` is part of `/provider add` and `<key>` is
not, and a page that had to tell those apart would be re-deriving the terminal's own grammar — which
is exactly what a control drawn from this frame must not do. `class` is §8's own four
(`panel`/`button`/`selector`/`form`/`danger`, `OnPage` in the source) and it is what a control will be
chosen by, so it is carried now: the drift test above uses it to run only the commands that are safe
to run with no argument, since running `/delete` to see whether it exists is not a check anybody
wants. Two kinds of row are left out rather than marked: the three toggles, which the `toggles`
field already carries in the shape a switch needs, and the terminal's own — `/exit` above all, since
a misclick in a browser must not end a session, `/web` because the page *is* the web view, `!`
because it is a shell escape the composer can type anyway, and `/stop` because the composer already
has the button.

`/help` was rewritten to print the table, and the gate for it was the cheapest possible: the output
was captured before the change and diffed after. It comes out byte-identical, including the hand-made
wrap of `/stop`'s row — the wrap is now a greedy function at the width the column leaves, which
reproduces the breaks that used to be typed in by hand — with exactly one deliberate difference:
`/config [edit]` became `/config` and `/config edit`, because one row cannot carry two classes and
the page needs to know that one of them is a report and the other takes an argument.

The page's half is a `<details>` in the header, collapsed, listing the rows in groups the page names
itself — reports, actions, selectors, forms, destructive — which is a reading order rather than the
table's order. It is deliberately not a control yet: a report is *shown* rather than sent (§8 below),
and a button that sends a command this page has not been taught to confirm would be a button that
destroys a conversation on one click. The policy test asserts the panel is built from the frame *and*
that the page contains no command name of its own (`/provider key`, `/delete <n|id>`, `/reload`),
which is the same drift guard one level down: a page that knew a command would go on offering it
after a rename, in a panel whose whole job is to say what there is.

**The actions are buttons now — built, 2026-09-15.** §8's first class, and it is the whole control:
an action takes no argument, so there is nothing to ask for and nothing to confirm — the button
sends the row's own `send` string through `sendText`, exactly as a picker or a switch composes its
line, and its answer comes back on the `command` line. `showActions` draws one button per `class:
"button"` row into the controls row, labelled with the row's `label` (the command's own words, which
is what `/help` prints) and titled with its `help`; a press that is refused calls `showState(doc)`,
the way a switch does, because a control that quietly did nothing is worse than one that says it
could not. Two rows today, and the test that matters is the one that keeps that true rather than the
two names: the frame's button rows are posted back to the terminal and their answers read, so a
`class: "button"` row whose command does not exist fails before anybody presses it — and an action
whose answer is *empty* fails too, because a button's only feedback on this page is the line it
prints, and nothing at all would be indistinguishable from a press that never arrived.

Only the button class is drawn. A report is deliberately not sent (the next bullet), and a
destructive command still needs a confirmation this page does not have.

**A report is a command answered to the page, not printed here — built, 2026-09-15.** §8's second
class, and the first one that needs something the composer route could not give it. A report *is* a
command's answer, so the cheap route was to send it through `POST /message` like a button — but then
the listing prints on the terminal as well, and the terminal is where somebody typed `/help`: the
person watching a run would read `/config`'s twelve lines because a browser asked for them. So the
same channel carries a second kind of line. `POST /report` queues a `Report` where `/message` queues
a `Line`, `InputMsg` keeps them apart, and the REPL runs a report with `Term::quiet_start` — the
recording a command's answer already got, with the printing off — so the answer is captured, sent as
a `command` frame carrying `"panel": true`, and drawn nowhere else. One funnel, one dispatch, one
flag; `Term::quiet_take` puts the terminal back whatever the command did.
Three decisions are worth keeping:

- **The refusal lives beside the table, not in the route.** `web.rs` does not know what a slash
  command means — that is the property that keeps the browser from being a second place that decides
  — so the route takes any line, and the REPL runs it only if `COMMANDS` has a row with that exact
  `send` *and* `class: "panel"`. Matched on `send` rather than on the label because `/delete <n|id>`
  is a label with a placeholder in it: a page that composed an argument of its own would otherwise
  have the process run it. The page has no confirmation step yet, so this is the safety half as well
  as the drift guard.
- **A report waits for the turn.** A report arriving mid-turn is kept and answered when the model is
  done, where a typed line interrupts: a listing printed into the middle of an answer would be read
  as part of the answer. `Handover` is what a turn leaves the REPL — the line it could not use, and
  the reports the page asked for — and the line goes first because it is what a person typed.
- **The page reads one listing at a time, and caches nothing.** A press replaces the panel's list
  with the reading and a way back; the answer fills it when the frame arrives, and only if it is the
  answer to what is on screen. No cache, because a listing held from a moment ago would be a second
  copy of a fact the process owns: asking again is cheaper and truer, and `/config` after an edit
  shows what is in force now.

**The selectors are done, and the last one cost a new kind of frame field — built, 2026-09-15.** Four
of §8's five selector controls existed already: the provider picker, the model picker, the switches,
and `/resume`'s numbers supplied by whichever sidebar row was clicked. The fifth, `/skills <name>`,
had no source for its options — the plan said where `/model`'s and `/resume`'s come from and left this
one open — and it is the one that reads rather than changes, so it belongs in the panel class with an
argument. So a row may now carry `values`: the argument values the page may ask for, on the row that
takes one. Two things follow from that being *on the row* rather than in a `providers`-style field.
The page draws one pressable line per value (`/skills alpha`), composed from the frame's own two
strings exactly as a toggle composes `/<name> <value>`; and the report route accepts a line when the
menu offers it — a panel row's own `send`, or its `send` plus one of its values — which makes the
values the *permission* as well as the options. `/provider <name>` takes a value too, and running that
one quietly would start a local engine without printing a word; `/delete <n|id>` is refused for the
same reason, and stays refused until the confirmation below exists.
The names come from the run's own walk of the skill directories — the same walk as its system prompt,
which is why the menu cannot offer a skill the model was never told about, and why `/reload` is what
makes a new one appear. That is asserted with a skill written *after* the run started: the command
would read it, and the menu must not. `Agent::skills` is where the names live, because the frame is
built after every line and a directory walk per line is not the comparison its own comment claims.

**The forms are in, and the one that takes a credential is the reason the table grew a column — built,
2026-09-15.** A row may now carry `field`: the kind of input the page may draw for its argument
(`text` or `password`, which is also the input's `type`). Two rows have one — `/name [text]` and
`/provider key <key>` — and the page composes the line as `send` plus what was typed, exactly as a
toggle and a value row do, and posts it to `/message`, because a form changes something and its answer
belongs in the transcript where the change is. A form row without an answer the page may collect is a row
of reference, which is how `/provider edit <name>` and `/config edit` stay where they are. (This grew a
list rather than a single kind later in the same day: see the built paragraph on `/provider add` below,
where a row carries `fields`.)
Two things were decided rather than discovered:

- **`/config edit` on the page needs a command the terminal does not have.** Its keys are enumerable
  and only its values are free-form, so a page form would send several settings at once; the terminal's
  command prompts for them one at a time. A page form would therefore need something like `/config set
  <key> <value>` — a command invented for the page's benefit, which is what §8's whole design exists to
  avoid (the page is offered what the terminal takes, not a private vocabulary of its own). So the page
  writes nothing into the config file in this round, and that is the remaining half of the class.
- **A credential must not be echoed, and the echo is not the page's to control.** The `command` frame
  carries the line that asked for the answer, which is right for every other command and wrong for
  this one: it goes to *every* page connected to the run and stays in the event ring, so a key typed
  into a masked field would be handed straight back. The table says which rows take a credential
  (`Field::Password`), and the echo becomes the row's own `send` — `/provider key` — in both places a
  line is repeated: the answer, and the refusal a read route produces for a line it will not run. The
  page is told the input's `type` by the frame rather than deciding from the command's name that a key
  is a secret, and it empties the field the moment it sends it.

**The destructive class closes §8, and it is the one where the frame had to say where a value comes
from — built, 2026-09-15.** `/delete <n|id>`, `/archive <n|id>` and `/provider rm <name>` are controls
on the page now, and the shape of the control is the promise: the row does not send, it *opens its
candidates*, and the candidate row prints the whole line it will send (`/delete 7`) — so the second
press is one that can be read before it is made. There is no undo anywhere in flint for the page to
offer, and a one-press `/delete` is the exact misclick §8 keeps `/exit` off the page for.
Two decisions were forced by that:

- **A destructive row has to say which list its argument comes from.** §8 says a selector's values come
  from a frame that already describes them *and that the command list should not say where* — the
  control decides. That holds while the control is one control. It does not hold here, because the
  page must draw the choices before anything can be confirmed, and the two lists are different ones:
  conversations by the sidebar's numbers, providers by name. Without a mark on the row the page would
  have to tell `/delete <n|id>` from `/provider rm <name>` by reading the commands, which is the one
  thing every other control is built to avoid. So three rows carry `from`, and the assertion counts
  them: exactly three, because a `from` anywhere else would have the page offering candidates for a
  command that reads.
- **`doc.confirm` is the row, not a countdown.** Nothing has been sent while the choices are open, so
  an armed state is harmless and a timer would only be a second thing to get wrong. It is cleared by
  the way back, by anything else the panel is asked to draw, and by a `reset` — a choice list is aimed
  at a document, and the numbers in it are positions, so an open list whose document was replaced
  would point the next press at the conversation below the one that went. The same reason makes the
  sidebar's own re-read redraw an open list: `/delete` in the terminal shifts every number under it.

**The destructive rows have a second home, on the conversation they act on — the sidebar's own menu,
2026-09-15.** Asked for after using the page: deleting a conversation meant opening the command panel,
finding the `/delete <n|id>` row and reading the number off the sidebar by eye. The row *is* the
conversation, so the row now carries a `⋯` button that opens a short menu -- the panel one level down,
in the shape DSH uses -- and a row in that menu prints the whole line it will send (`/delete 3`) before
it sends it. Two decisions worth keeping:

- **The menu is built from the frame**, like every other control, and through the same helper the
  panel's choice lists use: the rows the state frame marks `class: "danger"` with `from: "sessions"`.
  The page still does not know the word `/delete`, and a command the terminal gains or loses moves both
  controls with no edit to the page.
- **The conversation you are in is offered the actions too.** The terminal refuses that one and says
  why ("/new starts a fresh one; then this one can be filed away by its number"); a page that hid the
  row would be deciding a rule the terminal owns, and a menu with a hole in it explains nothing.
  The open menu is named by the conversation's **id** rather than its number, and closed by a `reset`
  or by a list re-read that no longer holds it -- the numbers in it are positions, and a list that
  shifted under an open menu would point the next press at the conversation below the one that was
  meant. That is the same argument `doc.confirm` is built on, one level down.

**The switches are offered their values, and a plain row says why it is plain — 2026-09-15, both asked
for after using the page.** `/provider <name>` and `/model <name>` take an argument out of a list the
run already knows and the page was already shown (the header's pickers), and the panel drew them as
plain rows, so switching meant reading a name off one control and typing it into another. Both carry
`values` now, and the page draws one pressable line per value. Two things make that safe rather than
merely convenient:

- **A value is a reading only on a `panel` row.** The report route runs its command with the terminal
  quiet, and for `/provider llamacpp` that is starting a local engine without printing a word. So the
  class routes the line: a `panel` row's value goes to `/report`, anything else is *typed* through
  `/message` and its answer lands in the transcript next to the change. `/model [name]` had to split
  into a report (`/model`) and a selector (`/model <name>`) for the same reason — one row cannot be
  both. `tests/cli_output.rs` asserts the refusal, because the page honoring the rule is not the same
  as the process enforcing it.
- **A row the page cannot press now says so.** The panel had been drawing reference rows to look
  exactly like pressable ones, on the theory that no command should look different from the others;
  the first person to use it asked why some rows press and others do not. Reference rows are marked
  and dimmed, and their hover says where the control is (`COMMAND_HOMES`: the header, the list on the
  left, or the terminal for the commands that ask questions one at a time).

`/provider key`'s sentence names the provider in force — "the active provider" is a phrase a browser's
reader cannot resolve, and `page_help` substitutes the name. The frame may do that and nothing else to
a help line, and `tests/cli_output.rs` holds the line at exactly that substitution.

**What this left open, now built: adding a provider from the page.** It was a terminal-only command when
this was written, because `/provider add` asked four questions one at a time and a page form could send
exactly one value. Both halves the note asked for are in: a non-interactive
`/provider add <name> <base_url> [model]` — the same answers as the words of one line, ending in the same
`save_provider`, and bare it still asks for each part — and a frame row that carries *several* answers,
as `fields`. The page asks for each of them, and **sends nothing at all while an answer the frame did
not mark optional is empty**, because the bare command is the wizard: a served run has nobody at the
terminal to answer it, so an incomplete form is a hang rather than a wrong answer. The key is not one of
the three answers — a credential does not go in a transcript — which is why `/provider add` switches to
the provider it just wrote: `/provider key` sets the key of the provider *in force*, and the masked row
names it.

The second shape that note refused is still refused: `/config edit` is not on the page. Its keys are
enumerable and its values free-form, so it would need a `/config set <key> <value>` the terminal does not
have. The panel's grouping is also still §8's classes rather than a task: "set a key" means switching
provider in one group and filling a masked box in another.

**§8 is built.** All five controls are on the page — the buttons, the panels that read, the selectors,
the forms and now the destructive ones — and what is left is not a class but three residues, each
written down where it belongs: `/config edit` as a page form (it would need a command the terminal
does not have, above), the mid-turn wait for a report (not asserted; needs a stub turn slow enough to
click during, in `docs/web-mode.md` §11), and a browser, which nobody has opened with a real font and
a real click. The queue below is what comes next.

- **A selector's options come from somewhere the frame already describes** — built: `/model`'s and
  `/provider`'s from `providers` (both controls exist), `/resume`'s and `/archive`'s from the
  sidebar's rows, which is the picked row's number rather than anything the page has to know, and
  `/skills`'s from the row's own `values`. The command list says which commands take a value *it may
  be read with*; it does not say where a picker's values come from, and it should not, because that
  is what the control is for. The two kinds are deliberately different fields: `values` on a row is
  the permission to read that argument, which is a smaller thing than the list of models on offer.
- **A form is the composer's problem** — built, for both shapes: the page draws one input per answer the
  frame names, sends them as the words of one line, and refuses to send while one the frame did not mark
  optional is empty. `/provider add` is addable from the page now (see the built paragraph above);
  `/provider edit` stays in the terminal, and `/config edit` stays there too, because it would need a
  command the terminal lacks. `/provider key`'s redaction is in the table rather than in the page.
- **The destructive class gets the page's own confirmation** — built: two presses, the second naming
  the line, because there is no undo anywhere in flint. The candidates come from the list the row's
  `from` names; the process-side command is unchanged, since a confirmation the terminal does not have
  would be a second way to run `/delete`.

Four gates. `the_page_is_told_the_state_its_controls_would_show` is the one that needs a real
process: `--web`, `/events` read as it arrives, the state frame read on connect, then `/model
stub-other` sent to `POST /message` — the route the page's own picker uses — and the changed state
read back, and finally a *second* subscriber, which can only be handed the snapshot. In-crate,
`the_state_frame_is_sent_when_it_changes_and_kept_for_a_later_page` pins the two behaviours
directly (the same state twice is one frame in the ring; a client that connects late is sent
exactly one, and it is the current one). `the_pickers_offer_the_runs_own_commands` is the page's
policy: the markup starts hidden, each picker sends exactly one line, and the `state` frame is
applied where it arrives rather than falling through to `applyLine`, where a named frame's data
would be read as a line of the run's vocabulary and skipped in silence. The page's Node check runs
the real `applyState` over the stub DOM and reads the options back — including the case that made
`fillSelect` more than three lines, a current value the frame does not list among the
alternatives, where a `<select>` silently keeps its first option and would then *send* it.

**Renaming a conversation from the sidebar.** Not tried by the person who asked, and the mechanism
is already there: `/name <text>` appends a `title` line, the page already *renders* titles (its
header shows one), and `/name` now has a field in the panel — so a rename typed on the page works and
its answer lands in the transcript, which was the feedback that was missing. What is still missing is
the *sidebar* affordance (`/resume`'s neighbour), and that is a small drawing job rather than a
mechanism. It is not worth doing first.


### 9. The page, and the conversations that get thrown away

The browser view was worked on hard in one round (six commits, all pushed). What belongs here is
what is *not* done, because the page is further along than §7 and §8 say.

**First: an interrupted turn loses the conversation — fixed, 2026-09-14, and the diagnosis that
was written down here first was wrong in two places.** Reported after `/stop` was added: the stop
succeeds, and the next thing said has no memory of what came before. What was written here said the
in-flight future leaves the history holding the user's line and not the answer, and that
re-deriving from the session file was the fix.

**Measured, against a stub provider that draws an answer and then holds the connection open** (a
body delivered in one piece ends the turn, so there is no window to interrupt in it):
the history is *not* what goes missing. After `/stop`, and after a line typed mid-turn, the next
request still carries the earlier conversation *and* the interrupted question; the earlier plan's
remedy would have changed nothing, because the file did not hold the missing thing either.

**What goes missing is the answer that was already drawn.** Text arrives delta by delta and becomes
a message only when the step that produced it *completes* -- so an interrupt, which is a dropped
future, takes the answer with it, out of the history and out of the file at once. The session this
was reported from has exactly that shape: two user messages in a row with no answer between them
(`~/.flint/sessions/1789373441-50.jsonl`), the user's second line asking the model to *finish* an
article, and the model's reply saying in its own words that it had no draft -- "我上一轮只做了检查
就被打断了" -- before going off to search the disk for words it had written and could not find. The
session a few minutes later ends the same way (`1789373985-837.jsonl`: an article, then "写长一点",
and the file stops there). A half answer on screen that the model denies writing is a conversation
the user and the model disagree about, and the user is the one who is right.

**The fix keeps the drawn text where dropping the turn cannot reach it.** `Agent::drawn` holds what
the step in flight has drawn -- a field, because every local of that future goes with it;
`Agent::commit_drawn_answer` turns what is left into an ordinary assistant message and appends it to
the file; and `run_turn` calls it straight after the drop, because the agent cannot do it for itself
once its future is gone. Verbatim, with no marker inside it: the words are the model's, and an
answer that stopped mid-sentence says what happened better than a note would. Both doors are
covered, and both are gated end to end by a test that is red without the call
(`a_stopped_turn_keeps_the_answer_it_drew`, `a_steered_turn_keeps_the_answer_it_drew`).

**Second: `/model`, `/provider` and `/reload` throw the conversation away — fixed, same day.**
Found while measuring the item above, and it is the same complaint through a different door:
`/resume` a file holding a question and an answer, `/model <other>`, then one line -- and the
request that goes out has **two** messages, the system prompt and the new line. All three commands
build a fresh `agent::Agent`, and a fresh agent has an empty history and a session file of its own,
so everything said before the switch was gone from the request *and* from the session directory
while the transcript on screen went on showing it. Nothing failed and nothing said so. It is
reachable from the browser the moment §8 puts a model picker on the page -- worse there, because
the page's transcript outlives the context that produced it.

**The shape it took, of the two written down here before it was built: a new file seeded with the
conversation.** `SessionWriter::seed` writes a `meta` naming the provider and model the
conversation is being *continued* with, then every message in hand except the system prompt (which
is rebuilt for every run, and a copy in the file would come back through `/resume` as a stale
message), then the conversation's name if it had one. `continue_conversation` in `main.rs` puts the
history back through `splice_loaded_history` -- the same route `/resume` takes and for the same
reason. Nothing was added to the session format: the file is an ordinary session that happens to
begin with a conversation already in it.

- The road not taken was to keep appending to the *same* file, which is cheaper by one file write.
  It is rejected because `meta` names the provider and model a session is held with, and `load`,
  `/resume` and the session header all believe it: carrying on in the old file leaves it claiming a
  model that nothing has been sent to since. Recording the switch instead means a new session
  event, a line in `docs/session-format.md` and a rule in `load` -- a format addition for something
  the user expects to be invisible.
- **One consequence, accepted rather than fixed**: the read-before-mutate gate lives in the tool
  box, so after a switch the model can be told to read a file it read a moment before the switch.
  That is one extra read with the reason in the message; the alternative was throwing the
  conversation away, and the gate exists to catch flint being wrong about a file, not to remember
  what was read.
- **The page follows the conversation now too.** The viewer was pointed at the new file by `/new`
  and `/resume` only, so `/model` and `/provider` -- the two the page's own pickers reach -- left it
  tailing a file nobody was writing, and the page simply stopped moving. The call moved into the one
  place that swaps the agent (`Flow::NewAgent` in the REPL), which is where the next command that
  replaces an agent gets it for free. Gated end to end by
  `the_view_follows_the_conversation_through_a_switch`, which reads `/session` -- the route that
  serves the followed file byte for byte.

The tests are the other half of the record: `a_model_switch_keeps_the_conversation`,
`a_provider_switch_keeps_the_conversation` and `a_reload_keeps_the_conversation` drive one command
each and then assert that the *next request* carries the whole conversation and that a file holding
all of it exists. All three were watched red first, failing on precisely that: the body was
`[system, "and now?"]`.

**Third: a stop button in the composer — fixed, 2026-09-14.** Shown only while a turn is in
flight, sending `sendText("/stop")` — no new route and no new verb, because the composer's route
already carries lines and `/stop` is a line. **The button was the easy half.** What the round
actually found is that *a stopped turn was over in the terminal and not on the page*, and that the
frame the page needed did not exist anywhere in the interactive path: `turn.completed` is written by
the one-shot `-p --json` path, and the page's own handler for it (`doc.status = ""`, close every open
answer) had never once run under `--web`. `message.completed` is what normally closes an answer, and
an interrupted turn never reaches it — the future is dropped — so the page kept the half answer open,
kept offering the stop, and kept saying what the turn had been doing. Measured against a stub that
draws half an answer and then holds the connection open, with `--web`, its stdout in a file and
`/events` read as it arrives:

- **Every turn ends with `turn.completed`**, from `ndjson::turn_completed` — the same line `--json`
  ends a turn with, from the same function, because a second spelling of "the turn is over" is a
  second thing to keep in step. `run_turn` says it in `turn_over`, which is also where the status row
  is cleared: both of the turn's exits (a command handed back to the REPL, and the end of the turn)
  need both, and they are one fact.
- **`Term::activity_done` clears the activity without a terminal to draw it on.** It returned early
  when `!interactive`, so a `--web` run whose stdout was a pipe or a file announced every wait it
  began and never the end of one: the last frame after `/stop` was
  `{"text":"writing the answer","type":"status"}`. `activity_started` already answers "even when
  there is no terminal, because a browser watching a piped run still needs to be told"; this is the
  other half of that sentence, and only the drawing needs a terminal (`paint_activity` checks).
- **`Live::answer_committed` drops the answer in flight when the drawn text becomes a message.**
  `Event::Done` is what normally drains that accumulator and an interrupted turn never reaches it, so
  the next page to open was handed the stopped half as an answer still being written — and the
  accumulator would have carried the stopped text into the following turn's `message.completed`.

The button reads `doc.running` (set by `turn.started`, cleared by `turn.completed`) and not
`doc.status`: the status line also carries feed trouble, and a stop button that appears because the
connection dropped is a button that stops nothing. It sits to the *left* of `send` on purpose — a
control that appears under the pointer that was reaching for another one turns a steer into a stop.
**The note here said a status frame, and that was half right**: the status is the strip and the answer
is a block, so clearing the status closes nothing; `turn.completed` is what the page already had a
handler for. Codex's tracker has the lesson twice (`openai/codex#28104`, `#28813`), and the shape of
it is the same: the turn ends in the process and the surface keeps saying it has not. Gated by
`a_stopped_turn_tells_the_page_it_is_over` (a real process, `/events` read as it arrives: red before
the fix, with no `turn.completed` in the frames and the status frame still naming the stopped turn),
`the_composer_can_stop_the_turn_it_is_watching` (the page's bytes: the word, the route, and the state
it is offered in), `a_page_opened_after_a_stop_is_not_told_the_stopped_answer_is_still_arriving`, and
the page's own Node check, which runs `paint` and reads the button back.

**Fourth: a line that is already waiting erases the question it interrupts — fixed, 2026-09-15, and
the test the note said could not be written was written.** The smaller hole in the same family, found
while measuring the first one. A line already in the channel when a turn starts was read *before the
turn future was ever polled*, and the turn's `user` message is pushed by that first poll -- so a
`/model` or a second line that arrived first was executed with the turn never having existed, in the
history and in the file nowhere, and the question produced no request at all. The fix is the one the
note asked for: the turn is polled once *before* the input channel is read, which makes the ordering a
fact rather than a two-millisecond window. The note was wrong about one thing, and it is worth
recording because it nearly stopped the fix: it said a test for this would be a race with the reader
thread. There is no reader thread to race when the line is put in the channel *before* `run_turn` is
called -- which is exactly the state the old order got wrong, and which waiting cannot tell apart from
a fast typist. `a_line_that_was_already_waiting_does_not_erase_the_question` binds a listener that
accepts and then says nothing (the shape the first item's measurement used), queues `/model
stub-other`, runs the turn, and asks the history whether the question is in it. It was watched red
first, with the history holding the system prompt and nothing else. The one consequence worth knowing:
a queued `/stop` now lets the request go out before it stops the turn. A question that was asked is
worth one wasted request, and that is the whole point of the fix.

**Fifth: switching provider or model split one conversation into two files — fixed, 2026-09-15, found
by using the page.** The second half of the second item above, and the quieter half. The history came
back after a switch, and the *file* did not: the replacement agent seeded a brand new session with the
whole conversation copied into it, because `Meta` names the provider and model and `--resume` believes
it. So picking another provider in the header -- which is one press now -- grew a row in the sidebar,
numbered the same conversation twice in `/sessions`, left a twin behind when `/delete` removed one of
them, and made resuming either half resume half a conversation. The fix is not a new file but an event:
`{"type":"switch","provider":…,"model":…}` is appended to the file the conversation is already in, and
`load` reports the last one, so `--resume` still believes the file. That is the argument `usage` has
made for its own numbers since the format was written -- the file is append-only, so what changed is a
line in it -- and it means `SessionWriter::seed` is now only for `--fork`, which really is a copy. Two
tests, both watched red: a lib test that a switch is a line and the last one is believed, and an e2e
test that a `/provider` on a served run leaves exactly one session file, appended to rather than
rewritten.

**Sixth: `--continue` meant "the newest session anywhere", not "the one held here" — fixed, 2026-09-15,
found while working out how a program should drive flint.** The session file has recorded `cwd` in its
`meta` line since the format was written, and nothing chose a session by it: `session::latest` took the
newest `.jsonl` in the home by modification time, so a second project's first `--continue` resumed the
first project's conversation, appended to it, and said nothing. For a person with one project this is
invisible; for a program driving flint one process per question -- one `FLINT_HOME`, one directory per
project -- it is the default case, and the symptom is answers that refer to another project's files.
`session::latest_for(dir, cwd)` compares the recorded directory with the one in force as **canonical**
paths (Windows does not distinguish case or separator, `current_dir` may spell a path differently than it
was recorded, and the file is hand-editable), and a session that records no directory belongs to nobody
rather than to everybody. With nothing held here the run starts a conversation and *says so* on stderr:
silently continuing from nothing is worse than an error, because nothing looks wrong. Three tests, all
watched red by making the choice ignore the directory again: a lib test that the newest session in the
home loses to the one held in this directory, and two e2e tests -- the other directory's file is
byte-identical afterwards and this one's grew, and the empty directory gets a new session plus the note.

**And the directory a run is *given* now means one thing — same commit.** `--cwd` was
`PathBuf::from(dir)`: a relative path stayed relative. Everything downstream then disagreed about
which directory the run was in — the tools used it as written, the session's `meta` line recorded it
as written, and `--continue` compares the two — so a caller that passed a relative path recorded a
relative one, and the next process (started somewhere else, which for a program driving flint is every
time) resolved it to a different directory and matched nothing. It is now made absolute with
`std::path::absolute`, deliberately *not* `canonicalize`: canonicalising on Windows prepends the
verbatim `\\?\` prefix, and this path goes into a file meant to be read and edited by hand — caught by
an existing `json_output` test, which is the only reason it is not in the tree. A `--cwd` that is not a
directory is refused rather than created, and the message says which of the two it is. Two e2e tests: a
run driven from one directory with `--cwd` naming another records that directory and then continues its
own conversation from a *third* process directory, and a `--cwd` that does not exist is refused with
nothing created and no session left behind.

**Seventh: conversations of two projects shared one directory — fixed, 2026-09-15, asked for after
the `--continue` fix above.** That fix chose the right session *by reading every file's `meta` line
and comparing directories*, which is a filter: it works, and it is the wrong shape. The directory a
conversation belongs to is not something to be inferred from a line inside it when it can be the
place the file lives. One home serving several projects — the normal shape for a program driving
flint one process per question, and for anyone who keeps one `FLINT_HOME` across repositories — put
every conversation in one directory, where the only thing keeping them apart was code. Sessions now
live in `sessions/<dir>/<id>.jsonl`, where `<dir>` is the working directory's last path component
lower-cased and cleaned up plus a hash of its whole **canonical** path (`flint-1f0a7c93`): readable
so a person can see whose conversations are whose, hashed so that `D:\work\api` and `E:\work\api` do
not share one, and canonical so that the same directory reached through `..`, a symlink or a
different case does not grow a second home. The layout *is* the separation; nothing infers anything.

Three things this turned up, all of them real. **`/delete 1` broke** -- and so would `/resume 2`,
`--archive 3` and anything else that names a session by its number or id -- because the resolver put
a path together as `sessions_dir().join("<id>.jsonl")`, which is a file that no longer exists; the
listing now carries each session's path and the resolver uses it, because a reader cannot rebuild a
path it does not know the shape of. **Archive had to follow**, since a file is now filed away beside
the conversations it came from (`sessions/<dir>/archive/`), and `--resume <id>` searching only the
root archive would have made a project's archived conversation unreachable: `session::list_archived`
searches the root's and every project's, which is the promise that `mv` by hand cannot lose one. And
the **listing had to learn two levels** (`session::list_detailed` walks the root and one level below
it, skipping `archive` at both), because it is a listing of the home and not of the directory it was
asked from. Sessions written before this keep working: they sit in the root, and `latest_for` looks
in the directory this working directory owns *first*, then in the root filtered by the recorded
`cwd` -- which is why `same_dir` is still there rather than deleted as obsolete.

Five tests, all watched red by making the choice ignore the directory again or by undoing the
nesting: `a_working_directory_gets_a_name_of_its_own` (one directory spelled three ways is one key;
two projects with the same last component are two), `a_new_session_lives_with_the_directory_it_was_held_in`,
`two_projects_in_one_home_keep_their_conversations_apart` (two runs, two directories, each holding
its own question and not the other's, and `--list-sessions` seeing both from a directory that has no
conversation of its own), plus the two `--cwd` end-to-end tests, which now assert the layout rather
than assuming it.

**Eighth: opening flint created an empty conversation — fixed, 2026-09-15, reported from the page.**
"我打开 web 的瞬间就有一个空的会话" — and measured before it was believed: starting `--web` against a
scratch home left exactly one session file, `{"type":"meta",…}` and nothing else, before a word had
been typed. The REPL did the same. So a home collected one conversation per look at the page: rows in
the sidebar that never had anything in them, numbers in `/sessions` that counted openings, and
`--continue` able to resume a conversation nobody had. The writer's file is now created by its first
event, not by the run starting, and the directory with it — a run that is refused before it says
anything (no key, an endpoint that cannot be reached) leaves nothing behind at all.

**The rule is "the first thing said", and deciding it needed a mechanism rather than a flag.**
`create_new` decides: whoever creates the file is the one holding the `meta` line, and nothing has to
be remembered about whether it was written. A flag was wrong the moment a writer is handed on, and the
switch is exactly that — `/provider` builds a new agent for the same conversation — which is how the
first attempt produced a file whose first line was the switch instead of `meta`. Two more real defects
came with it, both caught by existing tests: `/readonly` rebuilt its tool set through
`SessionWriter::resume` on a session whose file did not exist yet and failed outright, and
`continue_conversation` recorded the switch in one arm only, so a `/provider` on a session that had
said nothing wrote nothing while the page was told the provider had changed. `resume` now also refuses
a file that is not there, which is the backstop that turns "a session with no `meta`" into an error
next time.

**Two page tests changed their minds, deliberately.** Both asserted that a fresh `--web` run writes
exactly one session — which was the behaviour, and was wrong. They now assert that opening the page
writes *nothing*, and that the provider switch is what creates the file, `meta` first. Two mutations,
both seen red: making the file eager ("a session file was written before anything was said") and
skipping `meta` (four tests, including the seeded fork, which is where a file with no `meta` would do
the most damage).

**Done in the same round, recorded so it is not re-done**: themed scrollbars (the default grey ones
were the complaint); a draggable sidebar and a draggable reading width, with a hairline hint at
rest on the right hand because there is no seam at the text's edge to be discovered by; the
transcript moved into its own scroll box so its scrollbar sits at the window edge and the hand's
line no longer crosses the composer; **the page's own log, written by default** to
`$FLINT_HOME/web.log` (`POST /log`) — which is what found the bug in one line: a shadowed `doc` in
the hand's code made the boot's first paint throw, taking the sidebar, the sessions and the feed
with it; and `/stop`, the interrupt as a short word, reachable from the composer today.

## Small, agreed, unscheduled

- ~~`read`/`write`/`edit` taking `file_path`, with `path` kept as an alias so nothing breaks.~~
  **Done, 2026-09-15**: the three schemas ask for `file_path` and say in the description that `path`
  is accepted; two names that disagree are refused rather than resolved. `list`, `glob` and `grep`
  keep `path`, which is not an oversight: theirs is a directory or a place to search, not a file to
  read or write, and the plan named these three.
- ~~`--fork`: copy a session file and continue the copy.~~ **Done, 2026-09-15**: it takes the same
  three ways of naming a session as `--resume` (and bare, the most recent), seeds a new file with
  the conversation and the name, and leaves the original byte-for-byte untouched. Combining it with
  `--resume`/`--continue` is refused, because both answer "which file does this run write".
- ~~`examples/live_turn.rs` keeps a hand-maintained copy of `run_turn`'s event handling and has
  drifted twice, costing time chasing faults that were only in the example.~~ **Done, 2026-09-15**: the
  event handling moved into `src/sink.rs` — `EventSink`, one implementation of "what a turn's events
  become": the transcript, the status line, and the page's stream — and both the REPL and the example
  feed it. The example is now a second *caller* rather than a second version, and a test refuses a copy
  coming back (`the_example_renders_with_the_repls_sink`: no `match event` in the example). That the
  extraction changed no output is what the byte-exact `term_capture` suite is for, and it passed
  unchanged. This was the last item on this list.

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
