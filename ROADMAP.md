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

### 8. Web mode, from using it — **one fixed, one decided, two parked**

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
     already has.
  3. **Forms** — free-form or typed input: `/name <text>`; `/provider key <key>`, a password
     field that is never echoed back, never in a `state` frame and never in the transcript;
     `/provider add` and `/provider edit <name>`, which are the terminal's interactive wizard as
     a form; and `/config edit`, whose keys are enumerable and whose values are not (a shell
     path, `max_steps`, a proxy URL), with the file it writes named on the form.
  4. **Destructive** — `/delete`, `/archive`, `/provider rm`: a confirmation step, because there
     is no undo anywhere in flint.

- **Where the output lives is answered per class, not globally.** What an *action* answers goes
  to the transcript, which means `printer.term()` has to become something the feed carries —
  the same gap `turn.started` closed for user messages. What a *panel* shows (the provider
  list, usage, the skill catalog) comes from the `state` frame and is rendered in the page only,
  so the terminal is not flooded with a listing its reader did not ask for there.
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

**The command list is deliberately not in the frame yet.** It belongs with the buttons and panels
that read it — the design above wants an action's *output* carried too, which is the other half of
this — and a field nothing renders is a field that drifts. The frame grew its consumers one at a
time, which is also how it stays small: the toggles are in it because a picker needs their values,
and nothing else is.

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
header shows one), and typing `/name x` in the composer works today. What is missing is an
affordance and any feedback — and the feedback is the previous item. So this one is small once that
is built, and it is not worth doing first.


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

**And a smaller hole in the same family, measured by accident and left alone on purpose.** A line
that is already waiting in the channel when a turn starts is taken as an interrupt *before the turn
future is ever polled*: the turn's `user` message is pushed on its first poll, so a `/model` or a
second line that arrives first is executed with the turn never having existed -- not in the history
and not in the file. It showed up while measuring the item above, where the first question produced
no request at all. The fix is a turn polled once before the input channel is read, but a test for it
would be a race with the reader thread rather than an assertion, so it wants the two lines that make
it structural rather than a gate over a 2ms window.

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

**Done in the same round, recorded so it is not re-done**: themed scrollbars (the default grey ones
were the complaint); a draggable sidebar and a draggable reading width, with a hairline hint at
rest on the right hand because there is no seam at the text's edge to be discovered by; the
transcript moved into its own scroll box so its scrollbar sits at the window edge and the hand's
line no longer crosses the composer; **the page's own log, written by default** to
`$FLINT_HOME/web.log` (`POST /log`) — which is what found the bug in one line: a shadowed `doc` in
the hand's code made the boot's first paint throw, taking the sidebar, the sessions and the feed
with it; and `/stop`, the interrupt as a short word, reachable from the composer today.

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
