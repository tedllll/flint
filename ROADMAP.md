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

**The other half of the pair, `fetch`, is built as well — and this file said otherwise for several
sessions.** The paragraph that stood here described `fetch` as deliberately left undone while
`src/fetch.rs` had been in the tree since 2026-09-13 (`d5f0462`). Reading a source is the next thing
a model wants after search names it, and without the tool the only way was `bash` and `curl`, which
puts a page's raw markup against `max_tool_output` — measured at 94,879 bytes for one search result
page against a 30,000-character budget, so the model saw the `<head>` and the footer and the part
worth reading was exactly what was discarded. The design record is the module doc in `src/fetch.rs`,
and its one rule is why it is not a `curl` wrapper: **a fetch may only reach the public internet.**
The host is resolved once, *every* answer is checked, and the connection is pinned to an address that
was checked, because a second resolution is what a rebinding attack is. That is a boundary around the
tool and not around flint — `bash` still reaches whatever the machine can — and what it buys is that
the safe path is the easy one.

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

**Closed since**: the Unix process-group kill for work a command backgrounds (§6.1) — the one
item this paragraph carried as deliberately open — **is built**, with the code and the test that
was watched failing on the ubuntu job before the code existed. The other item it used to carry —
the line-ending sentence for
`apply_patch` (§6.4) — **is built too**, and `docs/windows-tooling.md` is where both are written
down. This file claimed otherwise for a session, and the correction is spelled out rather than
dropped in silence, because "what is left" is the sentence the next reader trusts.

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
- ~~The interim state is still there. Deleting it is what makes the strip a cell grid rather than
  a text offset, and it is the larger half of step 3.~~ **The merge landed, 2026-09-14, and this
  bullet was left standing over it** — "The merge landed" below is the record: the eight fields
  behind five locks are two models under two locks, and nothing was deleted, because measuring each
  one showed it carries something that cannot be reconstructed. `committed` counts *rows*, which is
  why a width change has to end the segment rather than continue it — a property of the count, not
  a piece of interim state waiting to be removed.

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

### 7. Web mode: a window onto the running process — **all three levels built**

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

**A conversation that has not happened yet is in the sidebar — fixed, and the fix is the option this
paragraph first rejected.** It opened on the file `SessionWriter::create` was thought to open at
startup, which listed as `(empty)` before anything was said. The file is not created then any more:
`SessionWriter::append` claims it on the **first event**, and creation *is* the claim — `create_new`
is the only portable way to ask the operating system "is this name mine", and the answer decides
both which file the conversation goes to and whether this writer writes `meta` into it (see
`claim`, which is also the fix for six concurrent runs proposing one name). Measured on 2026-09-16
with the real binary in a scratch `FLINT_HOME`, over the real route: `GET /sessions` before a word
was said answers `{"sessions":[]}`, and after one line it answers one row — the same session
`session.started` had named on the stream before the first write, so a caller is told its path
without the file existing yet. What the paragraph above called the cost of this option — a crash
before the first message leaves nothing — is the *point* of it, and the same decision already
refuses to leave a directory behind for a run that never got a key. `(empty)` survives as the label
for a file that holds a `meta` line and no messages, which is a real file and is described as one.

The other two ways out are therefore not taken, and the thing the paragraph wanted decided — whether
`/resume` should offer such a conversation — answers itself: there is no such file to offer.

**A command typed into the composer prints nothing — decided 2026-09-14, and built.** `/provider`,
`/config`, `/usage`, `/tools`, `/sessions`, `/skills`, `/model`, `/name`, `/readonly`,
`/verbose`, `/detail`, `/reload`, `/help` — all of them answer on the terminal, and the page
showed none of it, so the composer was a prompt for messages and not for commands. The cause was
structural: command output goes to `printer.term()`, which draws in the terminal and exists
nowhere else. It is neither a session event (commands do not write to the session file) nor a
live frame (the feed carries the *turn's* events), and the page renders exactly those two. The
design below was carried out, and the two halves are worth keeping apart: a command *typed* into
the composer ends up in the transcript, because the capture is at the funnel (`Term::answer_start`
/ `answer_take` around `handle_command`) and `Live::command` pushes one `command` line; a command
*read* by the page's own panel goes over `POST /report` and comes back marked as a panel's. HANDOFF
has the round it landed in and `docs/web-mode.md` §11 the measurement — including the bug under it,
where one mistyped command used to end the run. What was left of this residue — the sidebar's rename
and the empty conversation — is settled: the empty conversation by the paragraph above it, and the
rename after the class list below.

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

**The sidebar's rename — built 2026-09-17, and it is a drawing job after all.** The name is the one
thing the sidebar *shows* that nothing on the page could change: `/name <text>` was in the forms
class above from the start, and the panel's row for it can only ever rename the conversation that is
open. So the row's own menu now carries a field, which is where the name it starts on already is —
and only on the **open** conversation. That is the one decision in it, and it is the terminal's rule
rather than the page's taste: `/name` names the conversation the run is *writing*, so a field offered
on another row would either rename the wrong conversation or would have to switch to it first, which
is a second line whose refusal (a session archived in the meantime) would leave the rename aimed at
whatever was open. Click the row, then rename it. The field starts on the label in force, because a
rename is usually a correction, and not on the page's own `(empty)`, which is a placeholder for a
conversation with no name rather than a name — sending it would make `(empty)` the name. And it is the
*frame's* row, not the page's: the field is drawn only when the frame describes a `/name` that takes
answers, and its line is composed by the same `formLine` the panel's forms use, so a name with two
spaces in it arrives as one name and an emptied field sends nothing — `/name` with no text *reports*
the name, which is a different command, and nobody who cleared a field asked to be told what it is
called. The command is named in one place (`frameForm(doc, "/name")`) for the reason `resumeLine` names
`/resume`: the sidebar's own lines are its vocabulary, and everything *about* the command comes from the
frame.

The half that is not the drawing is that `/name` now pushes the `sessions` frame `/archive` and
`/delete` already push. The rename field is *in* the sidebar and the sidebar draws the name, so a page
that renamed a conversation and went on showing the old label is a rename that looks like it failed —
the same complaint the archive bug arrived as ("the page did not refresh"), one command over. The page
does not have to guess: `list_changed` says the route is stale and the sidebar re-reads it, and the
transcript is left alone because the rename did not change it. Three tests hold it, and each holds a
different half: the drawing and the empty-field refusal under Node (`scripts/web-view-test.js`, which
can deliver a submit to the page's own handler), the composition and the refusal over the page's bytes
(`the_sidebar_renames_a_conversation_through_the_same_form_composition`, mutation-checked by deleting
the `if (!line) return;`), and the frame over a real `--web` process
(`renaming_a_conversation_tells_the_page_to_read_the_list_again`, red without `list_changed`). What a
browser has not been asked yet is whether the field keeps the focus it was given while the sidebar
re-reads itself, and whether Enter in it submits — §11's `Not yet measured in a browser` note is where
that belongs, and this control joins it.

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
does not have, above), the mid-turn wait for a report — **asserted since 2026-09-16** by
`a_report_asked_for_mid_turn_waits_for_the_turn`, which drives a real `--web` process against a stub
slow enough to ask during, so this residue is closed — and a browser, **measured**: §11's `L3 in a
real browser` table was driven over the DevTools protocol against the real page (sidebar listing, row
click, `+ new`, Enter, the layout at two sizes, three defects found), and the *later* controls have
since had the same treatment from a committed harness rather than a one-off —
`scripts/browser-controls-test.js` drives the switches, the command panel, an action button, the masked
credential field and a destructive row's menu, checks every press against the run's own stdout, and
found two more defects, both fixed (a fresh run's page drew no controls at all; the open panel covered
the composer's send button) — and the last residues were driven in the same harness: the sidebar's own
`⋯` menu on both kinds of row, both drag grips by a real pointer drag with the arrow keys and the
double-click reset beside them, and `#pick-model` from the keyboard. All of it passed, and the mutation
that neuters the page's `pointermove` handler fails the two drag claims, so they are checks rather than
decoration. §11's `The later controls, in a real browser` is that table and the method,
and what no browser has touched is one widget and one section — a native `<select>`'s open popup, which
is the operating system's, and the page's own long answers and reconnection. The
fourth residue, the sidebar's rename, is built as well and is recorded below. The queue is what comes
next.

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

**Renaming a conversation from the sidebar — built 2026-09-17.** The mechanism had been there since
`/name` got a panel field (`/name <text>` appends a `title` line, the page already *renders* titles,
and the answer lands in the transcript), so what was missing was only the *sidebar* affordance —
`/resume`'s neighbour — and "not worth doing first" was right about the order: it waited until the
forms class and the row's own menu existed, and then it was the drawing job this line predicted. The
field lives in that menu, on the open conversation only, and one `list_changed()` call makes the
sidebar show the name it was just given. The record after the class list above says what the five
decisions in it were and which test holds each one.


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
line in it -- and it means `SessionWriter::seed` is for the two doors that really are a copy: `--fork`,
which copies a whole conversation at startup, and `/fork <n>`, which copies a prefix of the one the run
is in. Two
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

**Ninth: structured output — `--schema`, and flint checks the answer itself.** A caller that has to
*act* on the answer cannot use prose, and the obvious answer is not available: measured against
DeepSeek, `response_format: {"type": "json_schema"}` is rejected outright and only `json_object` is
accepted, which promises the reply parses and says nothing about its shape. `{"day": 3}` is valid
JSON and the wrong answer to a schema that asked for `trading_day`. So the shape goes into the system
prompt and flint validates the answer locally, against a hand-written subset of JSON Schema in
`src/schema.rs` (`type`, `properties`, `required`, `additionalProperties`, `items`, `enum`, and the
length and bound keywords) — no crate, because the value here is agreement rather than coverage: a
keyword flint implements badly is worse than one it refuses, and a keyword it cannot check is refused
at startup rather than ignored, which would mean certifying answers against a rule nobody applied.

A mismatch is not a failed run: the model is told which JSON path failed, in the words its own answer
used, and asked again (`attempts` says how many answers it took; three is the limit). When none match
there is no `result` line at all — a caller reading that type can trust it is what the schema
describes — and the run ends with an `error` and exit 1. Deliberately *not* done: a `--tools`
allow-list (asked for and then dropped — the tool set is not the caller's problem, the answer shape
is), and `json_schema` negotiation, which DeepSeek does not have.

**The schema is recorded in the session file, as asked for in the words "这些应该跟随会话记录文件做记录".**
A `schema` line holds the whole schema, not a path to it, so a session stays readable when the file it
came from changes, and the last line wins: `--resume` holds the conversation to the same contract
without the caller passing anything again, `--schema` overrides it, and `--no-schema` writes a cleared
line rather than leaving the next reader to infer it from silence. A run that merely inherited a shape
writes nothing.

**Tenth: the Python caller is in the repository, not in a scratch directory.** It was written and
measured in `%TEMP%`, which is where a wrapper that only its author has is kept: the measured record
of how a Python caller has to behave — a call blocks, a failed run does not raise, `cwd` is what
separates conversations — was in files no clone would ever see. It now lives in `examples/python/`
with the stub it tests against, and `docs/python.md` says what the four things a caller must get
right are and which script shows each one. The checks run the binary **built from the checkout** when
there is one, because a check that runs yesterday's installed flint passes for the wrong reason: the
session-layout expectations in `test_call.py` were first written while `PATH` still held a build from
before the layout changed, and they failed for a reason that had nothing to do with the code.

**Eleventh: a silent turn now says it is still working.** The gap was the one the streaming interface
left: `--json` flushes every line as it happens, so a caller sees the answer being written — but
between `tool.started` and `tool.completed` nothing happens for as long as the tool runs, and a slow
tool, a slow model and a crashed process are the same thing from a pipe. Measured before it was
fixed: a six-second turn produced no line at all between `turn.started` and the answer. A run that is
working and not talking now emits a `status` line every five seconds, carrying the phrase the stream
last described (`running bash`, `writing the answer`, `thinking`) plus `elapsed_secs` and
`restarted`. It reuses the `status` frame rather than adding a type, because the page's status row
already reads it — and `restarted` keeps its existing meaning, so a renderer that runs its own clock
is unaffected while a reader that joined late, or that is reading somebody else's log, gets a number
instead. Deliberately not a config key: the rate is not a preference anybody has.

**Twelfth: a one-shot run can be stopped without being killed.** `/stop` was the interactive
session's word for the interrupt that works when there is no key to press — and a caller reading the
stream from a pipe is exactly the case it was written for, so a `-p --json` run now takes it on
stdin. The turn is dropped, the answer it had drawn is committed to the session file, a `warning` says
so and the run exits 0; a line that is not `/stop` gets a warning of its own rather than being
dropped in silence, because a one-shot run has no next prompt to steer and guessing whether a line
arrived is not a caller's job. `examples/python/flint_call.py` now sends that word when `timeout=`
runs out instead of using `subprocess.run`, which kills: a killed flint loses the half-answer the
caller has just read, so the next question about it is answered as if it had never been written. Both
halves are pinned by tests, including the one that watches a stalled stub's fragment appear in the
session file after the stop — with the commit removed the test fails with only the user's message in
the record, which is how it was checked that the assertion tests anything.

**Done in the same round, recorded so it is not re-done**: themed scrollbars (the default grey ones
were the complaint); a draggable sidebar and a draggable reading width, with a hairline hint at
rest on the right hand because there is no seam at the text's edge to be discovered by; the
transcript moved into its own scroll box so its scrollbar sits at the window edge and the hand's
line no longer crosses the composer; **the page's own log, written by default** to
`$FLINT_HOME/web.log` (`POST /log`) — which is what found the bug in one line: a shadowed `doc` in
the hand's code made the boot's first paint throw, taking the sidebar, the sessions and the feed
with it; and `/stop`, the interrupt as a short word, reachable from the composer today.

**And the complaint that opened the next round: a file path in the conversation could not be
clicked — fixed, 2026-09-17.** *"他对话里显示的文件真实地址，没做超链接，不能直接点开文件"*. Every path a
run leaves in the transcript is now a button: `GET /file?path=` reads the file the line names —
relative to the **run's** working directory, with a trailing `:line` opening at that line — and the
page shows it in a panel beside the conversation, with the size, the file's own bytes and a
`reload`. The panel is a column of the grid and not a fixed overlay, which a browser settled: the
overlay covered the very block the first path was pressed in, so the second path in a turn could not
be pressed at all. `docs/web-mode.md` §12 has the rule for what in a line counts as a path, the two
capability arguments (why serving any readable file adds nothing, and why a cwd jail was refused),
and the ten browser claims that hold it — including one `every()` on an empty array that swallowed
every bare filename until `Cargo.toml` was in the test.

**Then the other half of the same complaint: the run's background work was invisible in the page —
fixed, 2026-09-17.** *"现在看不到子代理和后台任务的情况，在 web 页面上面，你可以参考 dsh加上功能"*. The header
now carries a **jobs** panel, drawn from a new route (`GET /jobs`) that answers off the same `Job`
records `job_op` uses: the `task` children and the background commands this process started, running
first, each row badged with its kind, saying what was asked, ticking its duration once a second while
it runs, and carrying the exit code in words when it ends. A row is a door — pressing it opens that
job's log, or a child's conversation, in the preview column the paragraph above built. The frame that
says the list changed is a **revision counter**, not the list: `tools::jobs_revision()` is compared by
the SSE loop on every frame it forwards and on a four-second timer, which is the one change with no
traffic to ride on (a command ending while the run is idle). DSH's job popover is the shape, and the two
things refused from it are the live-tail inside the list (the preview column already reads a job's
output) and a `stopping` status (nothing records a stop until it has ended). `docs/web-mode.md` §13 has
the measured record, including the two claims that were written wrong — one looked for *any* settled
job when the child settles seconds before the command, and one reused a row's `id` and so pressed the
wrong row the second time.

**And the door beside it, in the same round: seeing a job you no longer want is only useful if you can
end it — built, 2026-09-17.** Until then the only way to stop a child or a background command was
`job_op`'s `stop`, which is a tool: a person who started a ten-minute build by accident had to ask the
model to stop it, or kill flint and take the child with it. `/jobs` now prints the same listing the
panel and `job_op status` read (`tools::jobs_report`, one function), and `/jobs stop <pid>` ends one
through the same `tools::stop_job` the tool calls — so a person and a model cannot come to disagree
about what is running or about what happened when somebody stopped it. The two doors differ in exactly
one place on purpose: `job_op stop` with no pid acts on the only job in play, while `/jobs stop` with no
pid is refused with a sentence pointing at the listing, because a person typing a kill is naming what to
kill. The page reaches the same command through its `danger` group, and its candidates are the **live
rows of the jobs panel**: the pids are already on screen, so nothing new had to be sent for it — the
third `ArgFrom` (`jobs`), and the one that shows what the field was for.

Two things came out of building it and are worth keeping. **A kill's exit status is the shell's, not the
command's**: Windows `taskkill /T /F` leaves the `cmd.exe` it signalled reporting 1, so a job a person
stopped deliberately was listed as `failed` — the one word that sends somebody looking for a bug that is
not there (Unix reports -1 and read correctly, which is exactly the difference a status word must not
depend on). `Job` gained `ended_by_us`, set in `kill()` *before* the signal and in the budget-expiry
branch, so the status word is read from the decision rather than inferred from a number afterwards. And
the budget ending a command is deliberately the same word, because the fact a reader wants is "did it
stop on its own or was it stopped", with the note saying why. `docs/web-mode.md` §13 has the measured
record — the lib test that holds "the tool's answer is the terminal's answer" to the byte, the e2e that
reads the two commands out of the opening state frame, the browser claims (**56/56**, 53 before), and
the two mutations.

**Still open in this section:** the item §9 was written about is now built, and the next round is
whatever using it turns up. Two limits are recorded rather than owed: a `stopping` state would need a
flag on the `Job` that nothing sets today, and a job started by an *earlier* run of flint is not in the
list at all — a handle is what this process started, and deriving a list from files is the thing this
repository does not do. A third is now stated rather than implied: **the panel has no kill control**, and
that is a decision, not a gap — a row is a door (it opens the log), and a second gesture on the same
target is the ambiguity that made a browser harness press the wrong row in the round above, so the
person's stop is a command reachable from the page's command panel.

### 10. flint as a function a program can call — **done: steps 1–7 landed (B7 included, and the duration half of B5)**

flint answers; it cannot yet be *trusted as a function*. A caller that acts on the result — writes a
config, queues a job, retries a batch, feeds it data it did not author — has to know four things
flint does not say: why the run ended, whether the answer is complete, whether the value is
trustworthy, and whether retrying it is safe. Three of the items below were found by using flint from
Python with a program on the other end rather than by reading the code, which is the only way to find
this class: every one of them is invisible from a terminal.

**What other "one command, one answer" tools converged on**, and which of the six flint has:

| The property | Who works this way | flint |
|---|---|---|
| the answer is separated from the noise | `simonw/llm`, `mods -r` | ✓ stdout is only the stream |
| the result says **why the run ended** | Claude Code `-p --output-format json`, whose `subtype` is `success`, `error_max_budget_usd`, … | ✓ `outcome` on `turn.completed` (step 1) |
| structured output is a field, not prose | the same: `structured_output` beside `result` | ✓ the `result` frame |
| cost and duration are part of the result | the same: `total_cost_usd`, `duration_ms`, `model_usage` | ✓ the duration half: `duration_ms` on `turn.completed` (B5); ✗ the money half, and deliberately — flint does not know what a token costs where it was pointed |
| the conversation can be named and resumed | the same: `session_id`, `--resume` | ✓ |
| the exit code classifies the failure | `sysexits.h`: 2 usage, 65 data error, 69 unavailable, 75 retryable. "A CLI that always exits 0 (or always 1) hides this signal, forcing agents to parse error text with regex" | ✓ 0, 1 unclassified, 2, 65, 69, 75, 130 (steps 1–2) |

At the time this section was written, flint did exactly what that last row warns against, and the row
was the section in one line. It no longer is: five of the six properties are in whole, and the sixth is
in with the half that is honest — a run says how long it took, and cannot say what it cost, because
flint has no idea what a token costs on the endpoint somebody pointed it at, and a number invented from
a price table would be worse than the absence.

#### A. Cannot be done at all

1. **A large input.** *Solved by `@path`, in step 4.* Measured from Python: a 30000-character prompt
   is fine, 33000 is `FileNotFoundError [WinError 206]`, because the prompt can only travel as an
   argument and Windows caps a command line at ~32k. The failure is *not* flint's: it is `subprocess`
   refusing to start, which is the worst way to learn this. A caller with a document, a rule table or
   a diff had no way in at all; now it names the file — `flint -p "apply @rules.csv"` — and flint
   puts the contents in the prompt. The same measurement is a check in `examples/python/test_call.py`:
   a 247 KB document goes through four characters of command line.
2. **A budget for one call.** *Solved by `--max-seconds`, in step 5.* `max_steps` was a runaway
   guard, and exhausting it emitted a `warning` string. A caller can now ask for "something in thirty
   seconds, or tell me you could not" and get `outcome:"incomplete"` with `"reason":"seconds"` and exit
   65 — and a `turn.completed` that says which limit ran out, which is the first-class outcome rather
   than an error string. Claude Code's `error_max_budget_usd` is the same shape of evidence; what is
   *not* done is the money half — nothing caps spend, only time.
3. **The answer in a file.** Only a schema run has a `result` line to read. Everything else is prose
   the caller must reassemble from deltas or `message.completed` — so every caller writes the same
   parser, and each one can get it subtly wrong.
4. **A preflight.** *Built, as `flint balance`.* There is no cheap "is this provider usable right now"
   — a missing key or an unreachable endpoint is discovered by a real call failing, after it may have
   spent money or run a tool. `debug prompt-input` answers a different question (what would be sent)
   without needing a key.
5. **Retry safety.** Nothing identifies a request. A caller that times out and retries may repeat the
   tools the first attempt already ran. The tool events are in the stream, so the information exists,
   but nothing states it as a contract and nothing tests it.

*One item was withdrawn after checking*: `--readonly` **does** exist as a one-shot flag
(`--readonly`, `--no-edit`), so a query-shaped call can already refuse to write. The audit assumed it
was config-and-slash-command only. What remains true is that it is all-or-nothing, which is a
decision, not a gap.

#### B. Cannot be told apart

1. **Why the run ended — the one that matters most.** *Done in step 1.* `turn.completed` carries two
   token counts and nothing else. `error` carries a message and nothing else. The step limit is a
   `warning` string.
   So to a program, a complete answer, a truncated answer, a refusal and a tool failure that still
   produced prose are the *same shape*. Measured, and it is exactly the trap: with
   `{"type": "string"}` as the schema, `{"last_trading_day": "error: 无法确定"}` validates — the type
   is right, it is a string, and it says the model could not tell. A caller acting on that value has
   a bug that nothing in the interface reported.
2. **One exit code for everything.** *Done in step 1.* Usage errors, a missing key, an unreachable
   endpoint, a rate limit, a schema that never matched and a run with no answer all exit 1. Callers
   branch on exit codes; the only alternative offered is matching text.
3. **Retryable or not.** *Done in step 2, with the balance bug as the case that forced it.* No signal
   distinguishes "try again in ten seconds" from "this will fail identically forever, fix the input".
4. **Whether the turn changed anything.** Tool calls are visible individually, but no summary says
   which files were written or which commands ran. For a caller deciding whether it is safe to
   proceed, that is the first question, and today it is answered by reading the whole event stream.
5. **What it cost.** Token counts for the last turn are in `turn.completed`, and *done since*: the
   wall-clock duration is there too (`duration_ms`, measured from `turn.started`, on every ending
   including a stopped one), and so is the **retry count** (`provider_retries`, added 2026-09-18) —
   how many of the turn's requests the provider had to send twice. Both halves were the same question
   ("was that slow, or was it stuck") and neither was answerable from the stream: a retry happens
   before anything has been drawn, so its only other trace was a notice on stderr, which a `--json`
   caller does not read and a log cannot count. The field is always carried, unlike `cache_hit_tokens`
   and `reason`, because flint always knows this number and `0` is the answer "the first attempt
   worked" rather than a silence. What is still absent is the money — see the note under B5 above for
   why that half is refused rather than pending.

#### C. Holes

1. **A stopped run exits 0 — introduced by the round above this one, found by this audit.** `/stop`
   drops the turn and commits the half-answer, then exits 0 with a `warning` on the stream. A caller
   that branches on the exit code — which is what callers do — treats a truncated answer as a
   finished one. The stream is honest; the exit code lies. This one is flint's own bug and is fixed
   as step 1.
2. **Stream integrity is a promise with no test.** *Done in step 6.* `--json` means "one object per
   line on stdout and nothing else", and a stray `println!` anywhere on that path, or a child process
   inheriting stdout, breaks every caller at once. The bytes are now checked in `tests/json_output.rs`
   for four shapes of run, against a vocabulary list that lives in the test as well as the README --
   which it turned out the README had already drifted from, by two frame types (`status`, `command`).
3. **Retrying is not safe** (A5), and a truncated turn makes it worse: the answer says nothing about
   what already happened.
4. **Injection is action injection.** flint has no permission layer by decision, and it reads data
   with tools — so data that a caller fed in can direct actions. `--readonly` is the only gate and it
   is global, which means "ask me a question" and "do something for me" cannot be separated by a
   caller that handles untrusted input.
5. **A session reused for a second purpose.** `--continue` carries the earlier conversation into the
   new question. Data crosses purposes, and the answer is coloured by context the caller did not
   intend — a silent fault, because the run succeeds.
6. **A schema that passes is not a value that is true.** The subset has no `pattern` and no `format`
   by decision (the reasons are in the code), which leaves dates, identifiers and enumerations to
   `enum` or to the caller. The documentation has to say this plainly, because a caller who reads
   "schema" as a guarantee will act on a string that says the model could not determine anything.
7. **A refusal before the stream starts reaches only stderr.** *Done.* Found while building step 1: a
   `--schema` this build cannot check is refused while the arguments are being resolved, which is
   before the stream is opened, so a `--json` caller gets an empty stdout and a code. The code carried
   it (2) and the reason was on stderr, but every other failure in this mode is also *on the stream*,
   and the comment in `run_json_turn` says why ("a run which cannot even start is still described on
   stdout"). Two shapes of failure for one caller is one shape too many.

   **Built**, and where it lives is the decision worth recording: the command line is now read in
   `main`, before the run, and a failure is reported in whichever channel the run promised — one
   `error` frame on stdout for a `--json` caller, the usual stderr line for everyone else. Three
   details are deliberate. The frame is written by the same `error_frame` the end of a turn uses, so a
   failure before the run and a failure at the end of one cannot drift into two shapes. A refusal
   still writes **no half stream**: no `session.started`, no `turn.started`, nothing that would let a
   caller mistake a run that never happened for one that answered nothing — which is what the older
   test was protecting, and it now asserts exactly that (`kinds == ["error"]`). And the flag has to
   have been *read*: the parser sets `stream_seen` the moment it sees `--json`, so `flint -p x --json
   --nope` is answered on the stream and `flint --nope -p x --json` is not, because flint has learned
   nothing at that point and reading the rest of a line it could not parse would mean guessing at a
   command line that is, by construction, not the one it was written to understand. That last case is
   the honest residue of this item.

#### The order

**Step 1 is done**, in the commit that carries this file. What was built, and the two places where
building it changed the plan: the turn end now carries `outcome` (`complete` / `incomplete` /
`stopped`), the exit codes classify (0, 1 unclassified, 2 usage, 65 unusable answer, 69 provider
unavailable, 130 interrupted), and the classification is made where the cause is known rather than
read off a message. **`refused` is not one of the outcomes**: flint cannot know a refusal — a model
that answers "cannot determine" in prose is answering, and calling that a refusal here would be the
same guess this section exists to remove, so it belongs in the caller's own check. **75 is not emitted
yet**: telling a rate limit from a dead endpoint needs the typed errors of step 2, and a code that is
never produced is worse in the table than a gap. The bug in C1 is fixed and pinned by a test that
asserts 130 where it used to assert 0; `README.md`, `docs/python.md` and `examples/python/flint_call.py`
carry the vocabulary, and the Python checks were what noticed the change (`Turn.ok` is now false for a
stopped run, because it was true before — that was the bug). B7 above is what the work turned up.

1. ~~**An outcome, and exit codes that classify.**~~ Done. The shape of it: the outcome is a field on
   the turn's end rather than a new event type, because the end of the stream already exists and a
   field costs consumers nothing; `EXIT_TEMPFAIL` (75) is declared and unused until step 2 gives it
   something to mean — **which step 2 did**: a failure the provider calls retryable exits `75` once the
   ladder has run out.
2. **`error.code`**, produced where the cause is known — in `provider` for no key, network and status
   codes; in `agent` for a schema that never matched; in `main` for arguments. Classified by matching
   on the error's text at the edge would be the same fragility this is meant to remove. **Done**,
   including **B7** (a refusal made before the stream opens is now described on the stream whenever
   the caller asked for one — unless the command line was refused before flint had read `--json`).
   `flint balance` -- the preflight A4 asked for -- is built
   too (below).
   **Its first job was the balance/quota cause below**, which was a bug fix as much as a
   classification: an OpenAI-shaped `429 insufficient_quota` used to be retried four times with
   backoff (see §10 B6), and the only way a caller could recognise "there is no money" was to match
   the message text.

#### B6. Balance and quota, which the first draft of this section missed

Reported from use — "I have hit this several times" — and it is the clearest case of a cause that
cannot be read off the status code, so it belongs with B rather than with the exit-code plumbing.

| Provider | What running out of money looks like |
|---|---|
| DeepSeek | **HTTP 402 `Insufficient Balance`** — the [error-code table](https://api-docs.deepseek.com/quick_start/error_codes) says "you have run out of balance… go to the Top up page to add funds" |
| OpenAI and OpenAI-compatible endpoints | **HTTP 429**, with `error.code = "insufficient_quota"` — the same status as a rate limit, which is why [openai-python added a dedicated `InsufficientQuotaError`](https://github.com/openai/openai-python/pull/3042) |
| Anthropic | 400 `invalid_request_error`, "credit balance is too low" (it recurs in the wild, e.g. [continue#10061](https://github.com/continuedev/continue/issues/10061)) |

What flint does today, read from the code rather than assumed: `status_is_transient` is "429 or 5xx"
and `MAX_ATTEMPTS` is 4 with a 1+2+4+8-second backoff. So

- a DeepSeek 402 is **not** retried (correct) and its body does reach `error.message` (correct), but it
  exits **1** — unclassified — so a caller has to match text to know the account is empty;
- an OpenAI-shaped `429 insufficient_quota` **is** treated as a rate limit and **retried four times**,
  waiting about fifteen seconds to be told the same thing. For a hundred calls in a batch that is
  twenty-five minutes spent hitting an empty account, which is the kind of waste that looks like
  flint being slow;
- a balance that runs out **mid-answer** cuts the stream, and that is **indistinguishable from a
  network drop**. Nothing can classify it, which is exactly why step 1's `outcome` matters: the one
  thing that can still be promised is "this answer is half of one, and no value was invented".

What to build for it, in step 2:

**Done**, in the commit that carries this file, except the last two bullets:

- `error.code` gains `insufficient_balance` (mapped per provider from the body, not the status), and
  the **transient decision is made after reading the body** — the bug above was that it was made
  before. `provider::ProviderFailure` is the type that carries it, `classify` is the table, and its
  unit test asserts the pair that proves the point: the same `429` body-classified two ways.
- the `error` frame gains **`retryable: true|false`**, which is B3's missing signal and now has its
  first hard case: quota is `false`, a rate limit is `true`. Done, together with the code — and 75
  (`EX_TEMPFAIL`) stopped being a number flint declared and never used: a failure the provider calls
  retryable exits 75 once the ladder has run out.
- its **exit code is 69**, not 75: 75 means "retry, this is temporary" and quota means the opposite
  ("stop, a person must pay"). The precise cause is in `error.code`, so the code stays a class. If a
  shell-level branch between "top up" and "fix the key" is ever wanted, that is a new number and a
  deliberate decision, not something to slip in here. Done as decided.
- **`flint balance`** (or a check before a `-p` run) as the concrete first use of the preflight in A4:
  DeepSeek's [`GET /user/balance`](https://api-docs.deepseek.com/api/get-user-balance) returns
  `is_available` — defined as "whether the user's balance is sufficient for API calls" — plus
  `total_balance`, `granted_balance` and `topped_up_balance`. A batch can then be told before it
  starts, and a strategy that only has a few calls left can be told how many. **Built**, and the
  build changed the design in one place worth recording: the money question is asked by *behaviour*
  rather than by provider name. The first cut asked DeepSeek's host, which misses a
  DeepSeek-compatible gateway, a proxy in front of it, and every stub -- and it makes "this endpoint
  has no balance API" and "this is not DeepSeek" the same fact, when only the first one is
  established. So `flint balance` asks `/user/balance`, falls back to `/models` on a 404, and says
  **"cannot tell"** (exit `1`) when the endpoint answers neither rather than claiming "usable":
  a local engine that serves only `/chat/completions` is normal, and a verdict nothing established
  is the one thing a preflight must never report. The exit codes are the run's own vocabulary
  (`0`, `69`, `75`, `1`) so a caller branches once, and the check never sends a completion.
- **the batch-level circuit breaker is the caller's**, and that is a feature of step 7 rather than of
  flint: the first `insufficient_balance` should cancel the rest of the batch and raise, because flint
  cannot know that twenty other calls are queued behind this one. flint's whole job is to make the
  cause sayable; the Python side is what turns it into "stop everything". **Built**, as
  `map_calls(..., stop_on=("insufficient_balance",))` in `examples/python/flint_call.py`: the first turn
  whose `error_code` is in `stop_on` sets an event, the jobs that have not started are skipped, the ones
  in flight are waited for rather than killed, and the batch raises `OutOfBalance` carrying the turns
  that did finish, how many were never spent, and the turn that found the empty account. `stop_on=()`
  turns it off for a provider that uses `402` for something else, and the sentence above is why the
  decision stayed in the caller rather than in flint.
- **flint never switches provider by itself.** DeepSeek's own advice for a 429 is to use another
  provider for a while; for a system whose answers become recorded values, a silent change of model is
  a provenance fault, not a resilience feature.
- **tests**, which wiremock already supports: one stub per shape (402, 429 with `insufficient_quota`,
  429 with `rate_limit_exceeded`), asserting that the quota cases are attempted **exactly once** and
  the rate-limit case is retried. The pair is the proof that the status code alone is not the
  classification. Done: two end-to-end tests (`expect(1)` is the attempt assertion) and a unit test
  over `classify` that asserts the same-status pair directly.

3. **`--result-file`** (the answer written where the caller asked, so no caller parses a stream to
   get it) and **`--list-sessions --json`** (so a caller does not reimplement the session-directory
   naming, which is an internal detail). — **The listing half is built**: `flint --list-sessions
   --json` prints one object (`type`, `count`, `sessions[]`), each row carrying the number `--resume`
   takes, the id, the **path**, the directory it was held in, the name, the preview and the label.
   Two details are the design: the rows come from the same `list_detailed` the printed listing uses,
   so the two can never disagree about the order or the numbering, and the label rule now lives in
   one place (`SessionSummary::label`) for the same reason — a rule written twice drifts once. The
   path is the field a caller cannot reconstruct, which is the point: a session lives in the
   subdirectory belonging to the directory it was held in, so joining an id onto `sessions/` names a
   file that is not there. — **`--result-file` is built too**, and its design is one sentence: *the
   file is emptied when the run starts and filled only if this run answers the prompt.* That is what
   makes it useful rather than dangerous — an empty file cannot be mistaken for a value, and a
   *stale* file is a wrong value that looks right, which is exactly the accident a caller reusing one
   path across a batch would have. It holds the answer the caller asked for: the prose answer text,
   or the validated object (pretty-printed) when a schema was given, written only where the schema
   passed, because a file holding a rejected answer would be the one thing the missing `result` line
   exists to prevent. It requires `--json` and a prompt and is refused otherwise: with no stream the
   answer is already everything on stdout, so redirecting it is the same thing, and a second way to
   write those bytes in a mode whose whole contract is "stdout is the answer" is a second thing to
   keep in step. A write that fails is a frame with the code `result_file` and a non-zero exit, not a
   warning: the caller named a file and is going to read *that*. Step 3 is done.
4. **`@path`** — a file named inside the prompt and inlined by flint before the request. This is how
   A1 is solved, and it is deliberately not "tell the model a path": content that must be seen has to
   *be* in the prompt, where it is not subject to the model's discretion, to `read`'s 2000-line
   default, or to the 30000-character tool-output spill. It also composes with prose ("compare
   `@a.txt` with `@b.txt`") and removes shell quoting from the caller's life. — **Built**
   (`src/attach.rs`). The rule is small because a prompt is prose and `@` is an ordinary character in
   it: a name is a candidate when it is `@` followed by a word (or by a quoted name that may contain
   spaces), an `@` *inside* a word is not one (so `someone@example.com` is an address, not a file
   called `example.com`), and a candidate is replaced **only if it names a file that can be read as
   text**. Everything else is left exactly as typed — a mistyped path is a mistyped path rather than a
   run that refuses to start over a character in prose — which is why the turn says what went in
   (`turn.started`'s `attachments`: token, path, bytes, lines) instead of leaving a caller to hope;
   the empty list is how a typo becomes visible. A name that is not a file is tried once more without
   trailing punctuation (`@a.txt.` at the end of a sentence), trimmed-first so that one prompt means
   one file on Windows, where the filesystem API strips a trailing dot and `@a.txt.` would otherwise
   "work" while naming a file that resolves nowhere else.
   Three decisions are worth keeping. **The cap is 256 KB of inlined text per prompt**, refused by
   name before anything is sent: both alternatives are worse, since sending it lets the endpoint
   refuse *after* the run with a provider's error instead of flint's, and an endpoint that silently
   truncates turns an oversized document into a wrong answer that looks complete. **The stream keeps
   the typed words** — the expanded prompt goes in the request and in the session file, because that
   is what the model was given, but a frame carrying a whole attached document would be the caller
   paying twice for bytes it already has. **A one-shot prompt only**: the wall is the command line
   (A1), a person at a terminal has no such wall, and a conversation that silently grew by a megabyte
   is a surprise in the one place where nobody asked for one. `flint debug prompt-input` expands too,
   since a preview of a different request would be a lie about what would be sent.
   *Not done, and deliberately*: an attachment does **not** count as the read that the
   read-before-mutate gate wants. It would be defensible — the model really has seen the contents —
   but the gate's bookkeeping lives in the tool set, so it means plumbing a path from `main` into it
   for the rare case of editing a file that was also attached, and the cost of leaving it is one
   `read` in that case, which also hands the model line numbers. Worth revisiting if it bites.
5. **`--max-seconds`**, so a caller can ask for a bounded run and get `incomplete` rather than a
   process that is still going. — **Built.** The bound is wall-clock over the *whole run*, including
   the repair attempts after a schema miss (one absolute instant, so a retry does not get a fresh
   budget), because the thing it protects a caller from is a process that is still going: `max_steps`
   cannot cut a request that never comes back, and a provider's own timeout is the provider's. What it
   does is drop the turn where it stands — the same machinery as `/stop`, which is also how the drawn
   half-answer gets committed to the session. What it reports is `outcome:"incomplete"` with
   `"reason":"seconds"` and exit **65**, *not* `stopped`: nobody changed their mind, a limit the caller
   itself set was reached, and a caller reading `stopped` would go looking for the person who pressed
   the key — while `incomplete` is the same word the step limit uses, which is what it is. That needed
   `reason` on `turn.completed` (`"steps"` for the `max_steps` guard), because the two budgets are
   raised in two different places and "unfinished" alone does not say which one to change; the key is
   absent on a finished run, so its presence is the fact. It requires a prompt: a budget bounds a
   *call*, and an interactive session is bounded by whoever is typing at it. The plain path (no
   `--json`) got the same bound, checked by the tick its loop already runs, and its exit code now
   carries the outcome — 65 for unfinished, 130 for stopped — where it used to return 0 for both,
   which was invisible only because a one-shot run with no stream has no way to be stopped but a
   signal.
6. **A stream-integrity test** — the thing C2 says does not exist: a run's stdout contains nothing
   but parseable frames, asserted on raw bytes. — **Built.** Four shapes of run (a tool round, a
   refusal while the command line is being read, a plain answer, and one cut short by its budget) are
   checked on the bytes that came out of the pipe: stdout decodes as UTF-8 *strictly*, ends in exactly
   one newline, carries no `\r` and no escape code, and every line is a JSON object whose `type` is in
   the documented vocabulary — which is now a list in the test as well as in the README, so a new frame
   type has to be added by hand in both places. The other tests could not see any of this: they read a
   `from_utf8_lossy` string and call `.lines()`, so a `\r\n` terminator passes every one of them
   (measured: with the writer emitting `\r`, the parse-based tests stayed green and this one failed),
   and a line that happens to be valid JSON passed them too.
7. **The Python side, once flint can be told apart.** `Chat`, which pins the session path
   (`continue_last` re-derives "the latest for this working directory" on every call, which is a race
   the moment there are two workers — and one file has one writer by design); streaming callbacks, so
   the deltas and the heartbeat reach the caller while they happen; `history()` to read the record
   back (this is how "the half-answer was really committed" was verified); `paths=` / `attach=` /
   `inline=` as three separate arguments, because their guarantees differ and folding them into one
   would leave the caller unable to say which is a promise and which is a hope; `map_calls(workers=)`
   with one session per worker (measured: 190 ms per call with an instant stub, so 100 calls are
   ~19 s in series and ~3 s on eight threads); and `require_read` for the case where a path named in
   prose has to be *seen* to have been read. — **`Chat`, the streaming callbacks and `history()` are
   built** (`examples/python/flint_call.py`, `docs/python.md`): the first call learns the session path
   from `session.started` and every later one passes it to `--resume`, so the target is decided once
   and a mismatch raises `SessionMoved` instead of answering from another conversation; `on_event` and
   `on_delta` are called from the reading thread as the frames arrive, and that is *measured* — the
   first fragment of a stalled run reaches the callback a second before the run is stopped, which a
   reader that parsed at the end could not do; `history()`/`messages()` read the file, and a line that
   is not an event raises `DamagedSession` rather than being skipped. **`attach=`/`paths=`/`inline=`
   and `require_read=` are built too**: `attach=` travels as a real `@` name and is checked afterwards
   against `turn.started`'s `attachments` (raising `NotAttached`), `paths=` writes the names into the
   prompt as prose and promises nothing, `inline=` is the prompt, and `require_read=` is checked
   against the `read` tool's `tool.args` frames (raising `NotRead`) — all three differences are visible
   in the checks because the stub logs the request bodies, which is the only place they are
   distinguishable; both refusals carry the `Turn` and `Chat` keeps the conversation. **`map_calls(workers=)`
   and the balance breaker are built too, and the step is done.** The sketch above said "one session per
   worker" and the build rejected that: with jobs taken as threads free up, which calls shared a
   conversation would depend on the thread schedule, so it is **one session per call** — every job is its
   own run, its own session and its own bill, and `turn.session` names it. That is not a preference: the
   parallel checks found that six concurrent runs proposed *the same* session id (they started in the same
   millisecond) and the older writer appended to an existing file, so five conversations went into one
   file — `new_id` now ends in the process id and taking the name is a `create_new` claim, which is a bug
   fix in `src/session.rs` rather than a change here. The breaker is `stop_on=("insufficient_balance",)`,
   on by default: the first such turn cancels what has not started, waits for what is in flight (it never
   kills a run mid-answer) and raises `OutOfBalance` with the turns that did finish, how many were never
   spent, and the turn that found the empty account; `stop_on=()` turns it off. A refused promise inside a
   batch is a `Turn.refused`, not an exception, because one refused job out of twenty should not throw
   away the other nineteen answers. Measured rather than asserted: six jobs against a one-second stub
   delay on six workers finish in about one call's time with the stub's own `max_in_flight` above three,
   and twenty jobs with one empty account cancel more than ten of them.

#### What other people's wrappers already learned

The shape is not new, which is worth knowing before inventing anything. Three tiers exist: the
official [Claude Agent SDK for Python](https://github.com/anthropics/claude-agent-sdk-python) (bundles
the CLI, `query()` as an async iterator, `ClaudeSDKClient` when you want a resident one, a permission
layer, hooks and in-process MCP tools); small community adapters that shell out and parse, e.g.
[`oh-no-my-claudecode`'s adapters](https://github.com/adaline-ankit/oh-no-my-claudecode/blob/main/src/oh_no_my_claudecode/loop/adapters.py),
where the entry point is literally `runner(prompt, escalation_level=0) -> AgentRunResult` for
`claude -p`, `codex exec` and `opencode run`; and the older "LLM is a function" line — `llm`, `fabric`
(pipes), `shell_gpt`, `marvin`, `instructor`, `DSPy` — which never spawns anything. flint's differences
are the ones already decided: one file and no dependencies, blocking rather than async, one process per
call, and the CLI itself owning the schema check rather than the caller.

Five lessons from them, and what each becomes here:

1. **An error can arrive inside a structurally successful envelope.** A comment in that adapter file
   records the case: the CLI reports an auth failure as `{"subtype": "success", "is_error": true,
   "api_error_status": 401}`, and *"without this check the error text would be parsed as ordinary agent
   output and a lenient verifier could let the loop converge on a run where the agent never actually
   authenticated"*. flint is half-way safe here already: a failed run emits `error` and no
   `turn.completed` at all. But there is one combination that is deliberate, undocumented and
   **untested**: when a schema never matches, the stream carries `turn.completed` with
   `outcome:"complete"` *and then* `error`, and exits 65 — because the turn really did finish while
   the answer is unusable. Two signals, two questions (`outcome` is about the turn, the code and the
   `result` line are about the answer). It has to be written down and pinned by a test, or the next
   reader will "fix" it in one direction or the other.
2. **Defensive parsing across versions** (they try several known key layouts and fall back to raw
   stdout) is the cost of a stream that is not a contract. That is step 6's stream-integrity test, and
   the reason the vocabulary is closed.
3. **A timeout that kills loses the half answer** — they use `subprocess.run(timeout=)`, get exit 1 and
   a `[agent timed out after Ns]` note on stderr, and the words the agent had written are gone. That is
   the fault step 1's `/stop` work fixed; here the prior art is worse, which is worth remembering when
   someone proposes the simple version.
4. **"What did this turn change?" is answered outside the agent** — that adapter derives `files_touched`
   by diffing `git status --porcelain` before and after, "so the list is always derived from the real
   working tree rather than fabricated". This settles B4: the caller owns the write and the diff, and
   flint's part is to be auditable (the session path, the tool events). A flint-side summary would have
   to work outside git as well, for a question the caller can already answer.
5. **No structured output leaves a hack in its place** — with nothing schema-shaped to read, that
   project keeps `prediction = first non-empty line, truncated to 120 characters`. `--schema` is the
   answer to that. Its other gap is B5: Codex headless reports no token usage at all and OpenCode no
   cost. **`duration_ms` on `turn.completed` is built** — one `Instant` per turn, started at
   `turn.started` and reported on every ending including `stopped`, so "was that slow or was it stuck"
   has an answer in the stream rather than in a stopwatch around the subprocess. What the number is
   *not* is a price, and that half stays absent for a reason worth writing down rather than a gap to
   close: flint talks to whatever OpenAI-compatible endpoint it is pointed at, providers disagree about
   whether a cached prompt costs less, and a price table maintained in this repository would go stale
   into a wrong number — which is worse than no number. A caller that knows its own tariff has
   `prompt_tokens` and `completion_tokens` on the same line.

Also worth doing while the release workflow is fresh: it **builds on every push and runs no tests**.
The three commands in `AGENTS.md` ("Verifying a change") are exactly what a job should run, and a
green build says nothing about whether a `[exit code: N]` path works.

#### Being used by another agent

Asked directly ("can Codex use flint as a subagent?"), and the answer changes two small things. Codex
has no first-class subagent the way Claude Code does (`.claude/agents/*.md` plus a `Task` tool); its
native extension point is **MCP**, configured in `~/.codex/config.toml` under `[mcp_servers.<name>]`
with `command`/`args` for a local stdio server (and a tool allow/deny list alongside). So there are
three ways in, and two of them need nothing from flint:

1. Codex's shell tool runs `flint -p "…" --json --readonly --cwd <dir> --fork`, and the model reads the
   `result` line and the exit code. Works today.
2. A thin **MCP wrapper** — `examples/mcp/flint_server.py`, stdio JSON-RPC, one tool — which makes
   flint a tool rather than a command. flint itself stays out of MCP, which was a decision about
   *consuming* MCP rather than about being callable; this is the other direction, and it is a Python
   example with no dependencies, like the caller in `examples/python/`. **Built**, with
   `examples/mcp/test_mcp.py` speaking the protocol at it: handshake, listing, a real answer, structured
   output, and a schema that never matched arriving as `isError` + exit 65 rather than as a success.
   MCP's tool input is a JSON Schema and flint's `--schema` already takes one, so the mapping is direct
   and the answer comes back structured. The result text carries flint's exit code, outcome, cause and
   session path, which is what lets a parent agent tell a finished answer from half of one.
3. A custom prompt or skill that wraps option 1 — the lightest, and the least honest about it.

Two findings from checking rather than assuming:

- **Codex's sandbox covers flint**, because Codex runs its shell commands inside an OS-level sandbox
  (Landlock on Linux, Seatbelt on macOS) and children inherit it: with `--sandbox read-only`, a flint
  that Codex starts cannot write either. Worth knowing, because it means the guard is not flint's to
  invent — and `--readonly` on the flint side is then a second, cheap one rather than the only one.
- **flint's stdin steering is unconditional**, and a parent agent or a pipeline that writes to flint's
  stdin gets `warning` lines for it. Harmless in practice (the lines are ignored and reported, never
  acted on), but it is a surprise a caller should not have to discover. **Decided and built**: the
  reader stays unconditional, and what arrives on it is *counted and never repeated* — the warning says
  how many lines were ignored and says plainly that flint does not repeat what a caller piped in,
  because a caller's pipe is not a private channel. A `--steer` flag was rejected: the Python caller
  relies on `/stop` for its timeout, and an opt-in reader would silently downgrade that to a kill with
  the half-answer lost.
- Nothing stops a flint from starting another flint, and nothing bounds how deep that goes: tools are
  not restricted by choice, so a nested call is a spend that recurses. Recording it rather than
  proposing a guard — the honest place for a limit here is the caller that started the first one.

**What the MCP wrapper deliberately does about token cost.** Asked directly ("MCP is a settled
protocol, but it spends too many tokens for what it returns — can it be improved?"). The cost is not
in the transport; it is in what a client carries *because* a server exists: every tool's name,
description and full JSON Schema is re-sent with every request of every conversation, and a client
with four servers and forty tools pays for all forty on every turn. A server cannot fix its client,
but it can refuse to be the expensive part:

- **One tool, not twenty.** `flint_ask` is the only name a parent ever pays for, and its description
  is short — the full documentation lives in `--help`, not in the schema.
- **The tool loop stays in the child.** The parent never receives flint's ten tool schemas. This is
  the structural advantage of calling an *agent* rather than a tool collection: MCP's usual bill is
  handing twenty schemas to a parent that needs none of them.
- **Results are answers, not transcripts,** with the session path attached so "tell me more" is a
  second cheap call rather than a bigger first one.
- **Cost is reported** (`usage` is in the stream) instead of hidden, which MCP normally does not do.
- **The description is byte-stable across versions** — a description that drifts with a release
  invalidates the parent's prompt cache, and that is a real bill paid by somebody else.

Still to do, and worth doing only if a caller asks: forwarding `heartbeat` as
`notifications/progress`, and a batch argument that runs N prompts in one round trip.

**Deliberately not in this section**: a resident `flint serve` (190 ms per call does not buy back the
complexity of a second process lifetime, and a resident mode was explicitly not wanted), an asyncio
API (every call is a process; `await` would add a second surface and no capability), Python-side tool
callbacks (a protocol to invent, against "no MCP and no subagents"), and `pattern`/`format` in the
schema subset (a dependency and a rabbit hole — `enum` and the caller's own check are the substitute,
and C6 is the note that the documentation has to say so).

### 11. What a survey of the tree found, 2026-09-18 — **the promised halves in §9/§10 are being finished first; item 6 below is built**

Written because the queue above ran out. §5–§10 have all landed, the small unscheduled list below is
empty, `## Known unfinished` opens with "No known defect is open", and the twelve items taken from the
reading of Pi are built. A queue that has run out is not the same as nothing being left, so this section
is what reading the tree — `src/`, `tests/`, `scripts/`, `docs/`, `HANDOFF.md`, this file — turned up
that is worth having, cheapest-and-highest-value first. **Every item names the evidence it came from**,
because the failure mode of a survey is a list of plausible-sounding features, and the failure mode of
*this* repository is a feature that contradicts something it already decided. What the survey looked at
and decided **not** to propose is at the end of the section, with the reason: a survey that only adds is
not a survey.

**Worked before the list below, because it was already agreed rather than merely proposed:** the halves
§9 and §10 named as still open, which a survey should finish rather than replace. The order is that
list's, not this one's, and each line is edited in the same commit as the code that closes it.

- §10 B5's retry half — `provider_retries` on `turn.completed` — **built 2026-09-18**. The remaining
  line in B5 is the money, which is refused with its reason rather than owed.
- §10's B-section: the schema-miss ending written down and pinned — **built 2026-09-18**, recorded in
  this section's own item 6 rather than twice.
- §9: `/export` from inside a running conversation ("the obvious next door"); a job that can say it is
  `stopping`; and the page's own half of the reconnect cursor, which `HANDOFF.md` calls the part of the
  item that stays open.
- §8: the picker of live runs that the page's `/say --to` is waiting on.
- §10 C3 and C5 — the two holes that need a decision rather than code: a request nothing identifies
  (so a caller's retry may repeat tools), and a session carried into a second purpose by `--continue`.

**Ordered, cheapest-and-highest-value first.** Two sweeps went into this: one over the documentation,
one over `src/`, `tests/`, `scripts/` and `examples/` — and every item below was checked against the
file it came from before it was written down, which is why three of the candidates a sweep proposed are
in the "checked and left out" list at the end instead (they were deliberate decisions wearing the
vocabulary of gaps). The sweeps also found no `TODO`, `FIXME`, `unimplemented!` or `todo!(` anywhere in
the tree, and exactly one `#[ignore]`d test, which measures rather than asserts by its own comment. **So
the work that is left is not marked in the code — it is prose that says a limit out loud, and the places
where prose and tree disagree are items 2, 3 and 9(iii).**

1. **Two of the three Node harnesses are headless and a push runs neither of them.** The highest-value
   item here is not a feature — it is a **guard**. CI's job is `cargo test` and `cargo clippy` and
   nothing else (`.github/workflows/ci.yml`: the `Test` step, the annotation step, then clippy), and
   `HANDOFF.md` spells out what that leaves open in its own words: "`cargo test` does not run them, so a
   machine without Node passes `cargo test` while the page's own renderer is untested".
   `scripts/web-view-test.js` needs no browser at all — it runs the *embedded* viewer script under Node
   against a stub DOM and asserts which line becomes which block — and `scripts/term-layout-test.js`
   replays the escape sequences and checks the screen that comes out. Both surfaces are the ones whose
   defects are invisible in the source, both harnesses exist and pass, and neither is run by anything
   automatic. `browser-controls-test.js` is the one that stays by hand, on purpose and for a stated
   reason (it needs a real browser, and CI does not have one — its own header says "deliberately **not**
   part of CI"). So this item is one Node step in the CI job running the two headless harnesses, which
   is exactly what `AGENTS.md`'s "Verifying a change" already tells a person to run.
2. **Stale sentences, found by checking claims instead of reading them — one fixed in this commit.**
   This is a class, not an incident, and it is the finding that says the most about the repository: four
   passages state something the tree stopped being true of, and **nothing in the gate can catch prose**.
   The four: (i) `HANDOFF.md`'s cold-start section says a `/config edit` page form "would need a
   `/config set <key> <value>` the terminal does not have" — the terminal has had it since 11:36 on the
   same day that section is dated from (`bb9e07c`; the help row is `src/main.rs:2876`), and the sentence
   is corrected in this commit; (ii) `docs/sandbox.md` records that "CI checks nothing on push", which
   was true when it was written and is not now (`.github/workflows/ci.yml` runs the tests and clippy on
   Linux and Windows); (iii) `ROADMAP.md`'s §2 says of recursive spawning that "nothing bounds how deep
   that goes", superseded by `FLINT_DEPTH` (maximum 2, set by the tool and by nothing else, §"Not doing"
   above); (iv) `HANDOFF.md`'s cold-start section lists "adding a provider from the page" as left open,
   which §8 records as closed on 2026-09-17. The fix for (i) is in this commit because a survey that
   points at a wrong sentence and leaves it there has made the problem worse. The other three want the
   same treatment, and the general one — a snapshot section that says when it was true instead of
   sounding like state — is now stated at the top of that section.
3. **What the page claims and what the harness holds are not the same set.** `HANDOFF.md` records it in
   its own words — "the mid-turn report wait is unasserted (`docs/web-mode.md` §11)" — and §11 does
   claim the behaviour: a report is accepted at `/report` **mid-turn**, and the turn is then stopped (a
   real interrupt, `outcome: stopped`). Both the Rust suite (`tests/web_view.rs`, a report read in the
   panel) and the browser harness (`scripts/browser-controls-test.js`, a report answered in the panel)
   cover a report *between* turns and neither covers one *during* a turn — the case where the composer
   is busy and the stop is being asked for through a different door than `/stop`. `docs/web-mode.md`
   §12 says the same thing about itself: pressing the button there is "**not measured here**", and "the
   press belongs to the harness in §11, which drives the page's controls and could be extended to these
   two rows". One extension of the harness that already exists, or two claims softened — the honest
   version of "the page's controls are driven in a real browser" is a list of which presses are, and the
   harness is where that list should be.
4. **Retry safety: nothing identifies a request.** `ROADMAP.md`'s §10 already states the hole — "A
   caller that times out and retries may repeat the tools the first attempt already ran" — and it is
   the one item in that section that is a design question rather than an interface one. A caller has no
   idempotency key to send and flint has nowhere to remember one, so a retry after a timeout is a
   second run of `rm`, `git push` or `apply_patch`. The options all cost something this repository
   cares about (a key on the request is a second protocol; remembering keys on disk is state that
   outlives the process; refusing retries puts the decision on the caller), which is exactly why it is
   worth a round of thinking rather than a paragraph of guessing. Highest-value *design* item here.
5. **`flint --version` — a released binary cannot be asked what it is.** Measured this round: `flint
   --version` prints `flint: error: unknown flag '--version'. Try --help.` and `--help` has no version
   line either. Checked where the number *does* appear, and it is two places, neither of them a caller's:
   the REPL's banner line (`src/main.rs:1659`, `env!("CARGO_PKG_VERSION")`) and the `User-Agent` flint
   sends when it fetches a URL (`src/fetch.rs:377`, `flint/0.1.0`) — so flint tells the *network* which
   build it is and not the program that started it. Worse for a reader in a hurry: the `--json` stream
   *does* carry a field called `version` (`src/main.rs:740`), and it is the **session file format's**
   version, `1`, not the build's — a caller who takes the field that looks like the answer gets a
   different number that is also true. The release workflow builds four targets by tag and the PATH copy
   is refreshed after every round, so "which flint is this" is the first question a bug report asks. One
   flag, one line in `--help`, one test that the number on the flag is the number on the banner.
6. **One ending of a `--json` stream is deliberate — and was documented nowhere and untested.** *Built
   2026-09-18.* §10 said it in its own words: when a schema never matches, the stream carries
   `turn.completed` with `outcome:"complete"` **and then** an `error`, and the process exits 65 — "the
   next reader will 'fix' it in one direction or the other". What was built is the two halves of that
   sentence and nothing else, because the behaviour was already right: `README.md` states the
   combination for a caller (two signals, two questions — the turn did finish, the answer is
   unusable), the comment where the `error` is emitted says the same thing where the next reader will
   actually be standing, and `tests/json_output.rs` pins **the order and the outcome together** in
   `a_schema_that_never_matches_ends_the_stream_with_an_error`. The assertion was shown to be live
   rather than decorative: reporting the schema miss as `incomplete` makes it fail with `outcome:
   "incomplete"` and `reason:"steps"` — naming a budget that never came due, which is exactly the
   "fix" the test exists to refuse.
7. **A job cannot say it is stopping.** §9 records the limit: a `stopping` state "would need a flag on
   the `Job` that nothing sets today". A stopped job reads as `running` until it is gone, so the
   person's status row and the page's jobs panel cannot distinguish "asked to stop, not gone yet" from
   "still working" — and after the process-group kill work in `docs/windows-tooling.md` §6.1, a stop
   that takes a moment is a real window rather than a theoretical one. Small: set the flag where the
   stop is written (`job_op`'s `stop`, the page's row, `/jobs stop`), report it in the same listing, and
   one test that a stopped job says so before it ends.
8. **The page cannot get back to a restarted flint by itself.** §9's cursor work made the *server* able
   to answer a reconnect from a file position, and `HANDOFF.md` names the half that is left: "the new
   process listens on a new port with a new token, so the page's stream has nowhere to go, and the page
   keeps no cursor of its own." The honest shape is the page holding the position it has read to (and
   the run it was reading from) somewhere it survives a reload, so that reopening the page after a
   restart is a reconnect rather than a fresh view — which needs an answer to what a *stale* position
   means before it needs any code.
9. **Three small gaps that the code names about itself.** (i) `tests/cli_output.rs` says of itself that
   the paste fix "is not covered here (see `HANDOFF.md`)", which by this repository's own rule — a
   regression test that has never been red has not been shown to test anything — means a shipped fix
   with nothing holding it; (ii) `/say` on the page has no `--to`, and `src/main.rs:2848` says why:
   "Addressing is a terminal move until the page can offer a picker of live runs" — the picker is the
   missing half, and the presence records it would read already exist; (iii) the comment that justifies
   where the report whitelist lives says "the page has no confirmation step yet"
   (`src/main.rs:3156`), while §8 records a second press as the confirmation for destructive rows — one
   of the two is stale, and the whitelist's own reason is worth stating in the terms that are true.
10. **`/export` from inside a running conversation.** §9 calls it "the obvious next door": the export
    path exists and is tested for a finished conversation and for the command line, and what is missing
    is the door from a live run — which needs its own answer to where the page goes while the terminal
    owns stdout, because that is the whole reason `flint export` owns stdout when `--out` is absent.
11. **One cosmetic thing, recorded because a survey should be honest about the tail.** §8's page groups
    in the commands panel are still the *classes* the round that built them was working through rather
    than a task a person would name. Low value, no behaviour; listed only so the next reader knows it
    was seen and judged not worth a round on its own.

12. **The largest item, and the only one that is a decision rather than a task: the one place the plan
    of record contradicts a plan document.** `## Not doing, and why` refuses a permission layer, and
    `docs/sandbox.md` — "Status: a plan. Nothing in this file is built." — argues for grants instead of
    modes, kept beside the decision it contradicts on purpose, with adopting any stage meaning the
    bullet is edited in the same commit. It is last because it is the size of a project, not because it
    is small: either take a first stage and edit the bullet, or decline it in writing. What puts it on
    this list at all is that it is the only place where reading the repository can give two opposite
    answers, which is the thing this project refuses to have — and the sandbox document is also the
    reason the `readonly` half is honest about being all-or-nothing rather than half a boundary.

**Read, and deliberately left out of the queue** — each of these came up in one of the two sweeps or in
this reading, and was not proposed, for a reason worth keeping rather than rediscovering:

- **A job list that reaches a run this process did not start.** §9 records the limit and the reason in
  one breath: a handle is what *this* process started, and a list derived from the files other runs
  left behind is the derived state this repository does not do. `flint who` already answers the
  question that limit is about, from records that exist on purpose, and it says honestly what it cannot
  see. Not a gap.
- **A kill control on the jobs panel.** Decided in §9, not deferred: a row is a door that opens the
  log, and a second gesture on the same target is the ambiguity that made the browser harness press the
  wrong row once. The person's stop is a command in the command panel, which is where commands live.
- **A cost estimate in dollars.** Pi shows one, and the question will come back, so the answer belongs
  somewhere. flint shows tokens, the cache hit rate and the split the endpoint reported
  (`src/event.rs`'s usage), and it has those because the endpoint said them. A dollar figure needs a
  per-model price table, which is data that goes stale silently and would be the only thing in the
  repository that is true only until a vendor changes a page. Refused for the same reason
  `flint doctor` and an index are refused: it is a second source of truth about somebody else's
  machine.
- **An MCP server inside flint.** Already recorded as deferred rather than refused in
  `## Not doing, and why`, and nothing has changed since: being *callable* over MCP is built
  (`examples/mcp/`), and nothing in the tree needs the other direction yet.
- **A `/config edit` page form.** The `set` half is built and the `edit` half would need a text field
  on the panel and a write path for a file a person may be looking at in another window. The command
  line is one keystroke away on the page's own command panel, so the form buys convenience and costs
  the one thing the page has kept: everything it can do is something the terminal can also say.
- **The numbers that need somebody else's machine or somebody else's key.** `docs/deepseek-search.md` §6
  is a section called "Still not measured" with six questions in it, including whether `max_uses` has
  any effect at all; `HANDOFF.md` records that no endpoint's real rate has been measured because there is
  no key on this machine and spending one would not be flint's call; and `docs/windows.md` has one
  `UNVERIFIED` label left standing, on whether Windows Terminal and conhost differ in how they render VT
  sequences. Each is one measurement away from being closed and none of them is closable from here, so
  they are named rather than queued — a queue item nobody can start is a queue item that makes the queue
  a lie.
- **The Windows tree that outlives its shell.** `src/tools.rs` records the measurement: once the shell
  has exited, `taskkill` reports "not found" (exit 128) with the tree still running. That is a fact about
  the operating system, not a gap in the guard, and the guard's comment already says which cases it is
  for (the interrupt and the timeout, not a command that deliberately detached).
- **A graded permission setting narrower than `readonly`.** Refused in `docs/decisions.md` —
  "`readonly` is all-or-nothing. There is no middle setting" — and the reason survives the survey: a
  middle setting that is not airtight is the thing `docs/sandbox.md` exists to argue about properly, so
  inventing one quietly here would answer the fork above by accident.

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
  unchanged.
- ~~**The mojibake scan knows one generation of damage.**~~ **Closed 2026-09-17, by making the second
  check a whitelist instead of a longer blacklist.** The marker list stays and still earns its place —
  inside a file that holds Chinese text it is the only thing that can tell prose from damage — but the
  tree is now checked the other way round as well: every non-ASCII character in a scanned file must be
  one this repository *means*, either a punctuation or symbol class, or CJK in a file that is declared
  to hold CJK (`CJK_FILES`, each entry with a reason, so Chinese in a new file fails the build until
  somebody writes down that it belongs there). Both halves were watched failing for the right reason:
  U+597D in `docs/web-mode.md` is refused by the whitelist and held by no marker, and U+8DEF in
  `README.md` is refused by the marker list while the whitelist allows it. The residual is stated
  rather than implied — a *second* generation artifact inside a declared Chinese document is still
  invisible, because that is the one place a whitelist cannot tell damage from prose, and closing it
  would need a per-file character list whose maintenance cost is larger than what it buys. `.html` was
  the other hole in this test and is closed too: `web/view.html` is scanned, measured with a middle dot
  replaced by its artifact.

## Taken from a reading of Pi

[`docs/pi-agent-harness.md`](docs/pi-agent-harness.md) is a reading of Pi — the minimal TypeScript
agent harness at pi.dev, the same kind of program built on the opposite bet — with every claim sourced
from a page that was loaded. It was read on 2026-09-17, and **the list below was taken from it**: these
are agreed, not scheduled, and each is one commit that edits its own line here when it lands. The
argument, the cost and what flint has today are in the document, §3.

- **A fork from a chosen point — built, 2026-09-17.** `--fork` copied the whole conversation and its
  tail came with it; `/fork <n>` now cuts at the n-th question the person asked **here**, keeps what
  came before it, and starts a conversation of its own from that — the retry after a wrong turn, which
  used to be performed with `cp` and an editor. A bare `/fork` lists the questions and writes nothing,
  because which point to cut at is the one thing the command may not guess, and the cut is at a
  *question* (a turn boundary, the same unit `trim_old_turns` cuts on) since a tool result whose call
  was left behind is a request the provider rejects. Both doors now record the lineage as a session
  **event** — `{"type":"fork","from":…,"from_id":…,"kept":…}`, `kept` present only for a cut — and
  never as `meta.parent`, which means "another run started this one" and is what files a conversation
  under `children/`: a branch is not somebody's child, and writing `parent` would have hidden it from
  `/sessions`, the sidebar and `--continue`. The original keeps every byte, the branch's `meta` belongs
  to this run, and where it came from is read back on the startup `resumed` line and under `/resume`.
- **A follow-up message that does not interrupt the turn — built, 2026-09-17.** A plain line typed
  mid-turn is steering and drops the in-flight request, by design; this is the second way to send one.
  `/queue <text>`, read in `run_turn`'s own poll loop *before* the classification that hands a command
  back to the REPL — which is the whole mechanism, because every other line typed mid-turn ends the turn
  by being taken, and this one is taken and then keeps waiting. The text is held in a queue that lives
  as long as the chain of turns (`VecDeque`, drained in the one arm a turn can end in), echoed as
  `queued for after this turn: …` when it is taken and as the question it became when it is sent; a
  *stop* therefore ends the answer in flight and not what somebody queued behind it, which is Pi's rule
  and the one worth copying (`docs/pi-agent-harness.md` §3.2). With nothing running the same command is
  this turn's message, and says so, because there is no turn to hold it for. Three decisions are in
  `docs/decisions.md`: the queue is the run's memory and not the file's (no new session event — a queued
  line is written when it is *asked*, which is when it is sent), a chain of turns that ends without
  sending the queue drops it **out loud** (a command typed mid-turn, or an error), and there is
  deliberately no `clear_queue`: Pi's client can put the text back in its editor and flint's prompt has
  nowhere to put it back to. The page is handed `/queue` as a form row, which is what "the composer
  offers the same choice" means when all a composer can send is a line.
- **`--no-session` — built, 2026-09-17.** A run that writes no conversation: no file, nothing in any
  list, nothing to continue from. It is a property of the whole run rather than of one command, which
  is why it refuses `--continue`, `--resume`, `--fork` and `--name` at the command line and `/new`,
  `/resume`, `/import` and `/fork` inside the run — each of those opens or names the file the flag
  promised not to write — and
  why the one path every mid-run switch goes through cannot create one either. What the run still
  writes is what it needs to work: spilled tool output and a background command's log, under
  `spill/unattached-<pid>/`, a directory of its own so two such runs cannot overwrite each other. A
  `task` child is started with the same flag, and that is the part worth keeping: a child is a
  conversation *this* run asked for, and a parent with no conversation has no id to file it under
  `children/`, so its file would have landed in the person's own list.
- **`/import <file>` — built, 2026-09-17.** A conversation somebody handed you -- or one you
  hand-edited -- becomes a conversation of this run's own. The difference from `--resume <path>` is
  *ownership*, and it is the whole reason the door exists rather than a note in the docs telling people
  to copy the file first: resuming carries on **inside** the file it was handed, so it grows, it gains
  this machine's `usage` lines, and its `meta` still names somebody else's working directory. `/import`
  copies instead and does not write to the source at all -- the same act `--fork` performs for a
  conversation this run is already in, for a file it never started from, and the door that makes
  hand-editing a file a first-class way to work rather than something only `--continue`'s directory
  scan can find. The copy records where it came from, on its own line above the conversation
  (`{"type":"import","from":…,"from_id":…,"messages":…}`), which is a session *event* rather than a
  field on `meta` for the reason `title` and `switch` are events: a fact that arrives at a moment is a
  line, and a copy of a copy reads as a chain of files. It is read back -- the startup `resumed` line
  and `/resume` both say "imported from \<file\>" -- because a record nothing reads is the fault the
  counts in `last_usage` had. Three refusals are the design rather than the plumbing: an **empty** file
  is refused instead of imported as nothing (a conversation of no messages would sit in the list,
  indistinguishable from a real one), importing the file this run is currently writing is refused (it
  would copy a growing conversation into itself, and `--fork` is that act), and a run started with
  `--no-session` refuses it like every other door that would create a conversation.
- **A static export of a finished conversation** — one HTML file, no process behind it. **Built,
  2026-09-17.** `flint export <n|id|path> [--out <file>]`, with stdout as the default destination so
  that `flint export 3 > page.html` is the whole invocation, and nothing else on stdout in that case
  — `--json`'s rule that a mode either owns stdout or owns none of it. What it writes is
  `web/view.html` with the session's own lines welded into it as a JSON island, so there is **one
  renderer**: the page already draws a finished conversation (`applyText` parses `meta`, `chat`,
  `usage`, `title`), and a second reading of a conversation written in Rust would be a second place
  where "what a tool call looks like" is decided. Three things are what make it an artifact rather
  than a copy: the `cwd` is taken out of the `meta` line (the only field in a session file that names
  the machine rather than the conversation, and an export exists to leave that machine); `<`, `>` and
  `&` are escaped inside the island, because a `<script>` element ends at the first `</script` in its
  text and that text is a conversation (a tool result quoting a file, or a model asked about this page)
  — without the escapes the rest of the conversation is parsed as HTML, where `<script>` is a script;
  and a byte-order mark is stripped before parsing, since `notepad` writes one and a JSON parser stops
  at it, which would carry the `meta` line as damage *with its `cwd` still in it*. It needs no key and
  no endpoint, like `/import` and `--archive`: it reads one file and writes another, and this is the
  artifact somebody wants on the machine where the provider is what is in doubt. A conversation with
  no messages is refused, for `/import`'s reason — a page that looks like a conversation and holds
  nothing is one nobody can tell from a page that failed to load. Verified in a real browser, offline
  from `file://`: the question, the answer, a tool call and its result are drawn, and a conversation
  containing `</script><script>document.body.setAttribute("data-pwned","1")</script>` does not run.
  Not built with it, and worth naming because it is the obvious next door: `/export` from inside a
  running conversation, which needs its own answer to where the page goes while the terminal owns
  stdout.
- **An entry id the page can resume from after a restart — built, 2026-09-17.** The cursor is no longer
  this process's frame number, which starts at 1 in every process and meant nothing in the next one.
  It is **a position in the session file** -- the file's length when the frame was pushed -- carried on
  every frame's SSE `id:` line and reported by `GET /session` as `X-Flint-At`. The ring is still asked
  first, because it is the only source with the deltas of an answer still streaming; when it cannot
  place the cursor (a gap of more than 512 frames, or a process that has only just started) the file
  answers instead, sending the entries after that position as `file` frames. `reset` is left for what
  neither source can place. Two things came with it: the page no longer re-reads and rebuilds the whole
  transcript on **every** reconnect -- a delta is applied to the document it already has, which is what
  kept a two-second drop from throwing away the reader's place and the answer in flight -- and the
  client now sends the conversation's id beside the cursor (`?session=`), because a position in a file
  and a position in another file are the same number. What it does **not** do is reach a page across a
  restart: a restarted flint listens on a new port with a new token, so the page cannot get back to it,
  and the page persists no cursor of its own. The design is now ready for a client that can; the honest
  next step, if one is ever wanted, is a page that remembers `(id, position)` and a run that will accept
  one from a client it did not mint it for. §15 of `docs/web-mode.md` is the record.
- **Prompt templates, and a way for a *person* to invoke a skill — built, 2026-09-17.** A prompt file is
  `<dir>/<name>.md`, in the same three places and the same priority order as a skill
  (`<project>/.flint/prompts/`, `<cwd>/.flint/prompts/`, `<FLINT_HOME>/prompts/`, first found wins on a
  name), and it is sent by typing `/<name>`: one line, with everything after the name filling `{args}`
  where the file put it, or becoming a last paragraph where it did not. The skill half is the second
  door onto what the `skill` tool already loads — `/skill <name> [args]` sends a skill's instructions as
  the person's own next message, so an instruction file flint hoped a model would consult can finally be
  aimed at something by the person who wrote it. Both go through one act (`Flow::Send`) and one
  `fill_args`, and both are listed and printed by the plural command a person reads first (`/prompts
  [name]`, and `/skills [name]` unchanged for reading). Nothing about a template reaches the model's
  prompt or the tool schemas — a directory of long templates costs a run exactly nothing — and the
  transcript keeps the line the person typed while the request carries the file's words, named once
  under the echo with the file it came from.
- **Compaction written into the session file — built, 2026-09-17.** Pi's entry carries the summary and
  the id of the first entry it keeps (`firstKeptEntryId`); flint's carries the summary and the **byte
  offset** of the first `chat` line it keeps (`{"type":"compact","summary":…,"from":…}`), because this
  format has no entry ids and a position in the file is the same idea as the page's reconnection cursor
  (§15 of `docs/web-mode.md`) — and the one idea that survives a hand-edit, since a person can count
  bytes and cannot count an id that does not exist. `load` drops everything that starts before `from`
  and puts the summary in its place as one framed `user` message, so a resumed run is folded without
  being told; the folded lines stay in the file, which is what makes the line deletable and the fold
  reversible. `/compact` is the door: one request with **no tools** (nothing a summarizer says may act)
  asking the model to summarize everything before the newest question, then one appended line, then the
  same fold applied in memory so the next request is the smaller one. Cutting at a **question** and not
  at a message count is Pi's rule and the request-side trimming's: a cut inside a tool exchange leaves
  the model holding half a step. **Automatic** compaction is refused: starting a model turn nobody asked
  for is a bill nobody agreed to, which is the same refusal `job_op`'s report makes. Two honest limits:
  a `--no-session` run is refused (a fold nobody wrote down is one the next run does not have), and
  `/fork`/`/import` copy messages rather than events, so a fold does not travel into a copy.
- **Compaction, the automatic half** — *not built, on purpose.* `trim_old_turns` still bounds a request
  by dropping whole turns past `max_request_chars` when nothing else has, which is a guard rather than a
  policy: the person decides when the conversation is folded, and the request-side trim stays the
  last-resort bound it always was. Pi decides by token budget on its own; flint's answer is that the
  number a budget would be checked against is a guess about an endpoint it cannot see.
- **The cache hit rate next to the token counts — built, 2026-09-17.** The one number that says
  whether the prompt flint builds is stable, and it is built as the *rate* rather than as a second set
  of counts: `cache 87% (871 of 1000 prompt tokens)` on `/usage`'s own line, `, 87% cached` appended to
  the footer under every answer, and the raw count (`cache_hit_tokens`) on the session file's `usage`
  line and on the two `--json` frames so a program can compute its own window. DeepSeek's
  `prompt_cache_hit_tokens` and OpenAI's `prompt_tokens_details.cached_tokens` are read into that one
  field, and the field is an `Option`, which is the whole design in one type: an endpoint that reports
  no cache split produces **no cache text at all**, because a `0%` would accuse the prompt of being
  unstable when the truth is that the endpoint does not say. The rate refuses to divide by a prompt of
  no tokens and is bounded at 100. Building it turned up a small piece of damage worth recording: the
  counts a resumed conversation reported had been parsed out of the file into `Loaded::last_usage` and
  never read, so `/usage` answered "no usage reported yet" about a conversation whose size was written
  down in the file it had just opened — and every mid-run rebuild (`/model`, `/provider`, `/reload`,
  `/readonly`) lost them the same way. `Agent::set_last_usage` carries them across both kinds of
  replacement. The **cost** half stays declined: `ROADMAP.md` B5 stands, and a price needs a model
  registry flint will not take. **Not measured: a real endpoint's rate** — the plumbing is held by
  tests on both field shapes, and the honest way to get the number is two short turns against a
  provider with a key.
- **The session and provider in a command's environment — built, 2026-09-17.** Every command a run
  starts (the model's `bash`, `pwsh` and `exec`, and a person's own `!cmd`) is handed `FLINT_SESSION`
  (the conversation's file), `FLINT_PROVIDER` and `FLINT_MODEL`; a run with none of its own takes the
  names *away* rather than leaving what it inherited, because a `flint` started by another run's
  command has a stale set in its environment. Both spawn sites go through one `apply_child_env`. A
  `task` child still gets `FLINT_DEPTH` and `FLINT_PARENT`, and a command gets neither: it is not a run
  and does not claim to be one.
- **A thinking level — built, 2026-09-17.** flint sent no reasoning parameter at all, and stored what a
  provider sent back. It now asks, out of four words (`off`, `low`, `medium`, `high`), and the design is
  the answer to the census the reading of Pi produced: **the value is standard, the field is not**, so
  the level is flint's and the JSON field it goes in is the *provider's*
  (`thinking_field = "reasoning_effort"`, or whatever the endpoint wants; empty means never ask). That
  is instead of Pi's per-vendor compatibility table, which is a list of other people's servers to keep
  in step with, and it is why nothing is sent until both halves are set: a field flint guessed wrong is
  a request an endpoint may refuse outright. `--thinking <level>` for a run, `/thinking [level]` while
  one is open, `thinking = "off"` as the config default, and the same switch on the page — the frame's
  `toggles` row, so the browser offers exactly the words the command accepts and carries no copy of
  them. **The level travels with the conversation**: it is an appended `thinking` line, the last one
  wins, and a resumed conversation asks for what it was being held at without being told again — which
  is the half a flag alone would not have. Two things are deliberately not built: hiding levels a model
  does not have (flint cannot know, and a menu that lies is worse than four words), and any automatic
  choice of level. `off` says what it is honestly — flint sends nothing, so the endpoint's own default
  applies, which for some models is reasoning *on*. A `task` child is a new run and starts at the
  config's level rather than the parent's; the endpoint travels to a child because a child is the same
  endpoint, and a level is a choice about this conversation.
- **Several tool calls of one assistant message, at once — built, 2026-09-17.** The calls of one message
  are started together and awaited together (`futures_util::future::join_all`, a crate already in the
  tree), and the unit is the **message** rather than the step, because that is the unit in which they
  were asked for. `Tools::invoke` already took `&self` and kept what it must remember behind a `Mutex`,
  so nothing about the tool set had to change: the calls share the tools and not their answers. Two
  things are deliberately unchanged. The **report** stays in the order the model asked — the transcript
  is read top to bottom and the pairing of a call with its result is the only structure in it, and
  concurrency was never a thing a line of it carried — and there is **no tool-by-tool rule** about what
  may run at once, because the read-before-mutate gate already refuses the second write of a file that
  changed since that call read it. That is what makes it safe rather than lucky. What is left undone is
  **cancelling the siblings** when one call fails: the model asked for all of them, a failure is
  information about one of them, and killing work the person paid for because another command exited
  non-zero is a decision flint should not make silently.

**Read and not taken, recorded so the argument is not lost:** a **run-level tool allowlist** (Pi's
`--tools`/`--exclude-tools`) as something narrower than `readonly`. It is the one candidate of the
eleven that was declined, and the reason is a judgement about this program rather than about the
argument: flint's tool set is the shape of the work it is for, and a per-run allowlist is a second way
to say what `readonly` already says. §3.3 of the document keeps the case for it, including the
experiment it would make possible (§5.3).

Two of flint's own decisions are argued *against* Pi in the same document, and both are already in the
tree: jobs and a panel instead of tmux, and a `task` child whose conversation is a readable file
instead of no sub-agent tool at all. Pi's numbers in that document are also the strongest argument for
the MCP deferral recorded under "Not doing, and why" — 21 tools and 13.7k tokens for one popular MCP
server is the cost that entry predicted.

## Known unfinished

No known defect is open. The five that were on this list are closed, and they are kept here
rather than deleted, because "this was broken and is not now" is the part a next reader
cannot reconstruct from the code. [`HANDOFF.md`](HANDOFF.md#known-unfinished) has the long
form of each, labelled by what was measured and what was not. What is left in this file is
planned work, not damage: the queue above, the small agreed items in §8, and the entries in
"Not doing" below.

**A conversation had no bound**, and that is the fifth. The request grew with the transcript
until the provider refused it — in the middle of a turn, as an error nobody decided on —
because the step guard bounds one turn and pruning bounds the stale output inside it, and
neither of those is a bound on a *conversation*. `max_request_chars` (400,000 characters by
default, in the unit `max_tool_output` uses, `0` to turn it off) and `trim_old_turns` in
`src/agent.rs` are the bound, request-side only: the session file keeps every message and a
note where the dropped turns were says how many went. The unit is a **turn**, so a tool result
is never separated from the call it answers; the newest turn is never dropped, because a
request with nothing to answer is not a smaller request; and the note is charged to the
budget it describes. `tests/cli_output.rs` holds it end to end — a resumed conversation whose
request opens with the note while the file on disk still has the first question.

Four things left this list earlier rather than being carried on it. The `eprintln!` sites that could
land inside the answer strip are fixed — there were four, not the three this file used to
name, and the fourth (`provider.rs`, printing the retry ladder from inside the request loop)
was the likeliest to fire; all four go through the notice sink now. CI checks a push: the
claim here was wrong as written (`release.yml` has always triggered on every push and builds
four targets), what was missing was a test job, and `.github/workflows/ci.yml` is one now —
`cargo test` and `cargo clippy` on Linux and Windows. A terminal that goes away no longer
takes a core with it: the spin was `crossterm`'s (`event::read` polls a hung-up descriptor for
ever), so it is watched for from outside the call — `watch_for_hangup` in `src/main.rs`, Unix
only — and `tests/tty_hangup.rs` holds it with a real pty, which took three CI rounds to get
right, the middle one burning *two* cores because the first watcher waited for a hangup with
nothing readable in it and Linux never reports that. And Windows newline and code-page
behaviour came *off* the list because it was measured: the console output code page is never
set and does not need to be (Rust writes to a console as UTF-16), and
`DISABLE_NEWLINE_AUTO_RETURN` is clear with a linefeed at the last column still advancing one
row, so nothing there was broken.

Two more left it in the round that finally measured the page: driving §8's controls in a real browser
(`scripts/browser-controls-test.js`, written up in §11 of `docs/web-mode.md`) found that a fresh
`--web` run answered **500** on `/session` — the run names its session when it starts while the file is
created by the first thing said in it — so a first run's page drew no controls at all, and that with
the `commands` panel open the `send` button could not be clicked, because the panel is the one thing in
the header that grows without bound and the reading painted over the composer. Both are fixed, both
were invisible in the source, and both are held by the harness that found them.

## Not doing, and why

- **A permission layer** — no approval prompts, no allow-list, no sandbox. An approval dialog
  in an emergency is friction at the worst moment, and a permission system that is not
  airtight is worse than a documented absence. `readonly` stays all-or-nothing, and the
  README and `AGENTS.md` say plainly that flint can damage the machine. **[`docs/sandbox.md`](docs/sandbox.md)
  argues against this entry and is not built**: a plan for grants instead of modes, kept
  deliberately beside the decision it contradicts rather than folded into it. Nothing in it is
  adopted; adopting any stage means editing this bullet in the same commit, because a plan of
  record that contradicts a plan document is how a repository starts lying to itself.
- **Subagents** — the original decision: flint is one conversation and one context window.
  Splitting it invents coordination, budgets and merge problems that a rescue tool does not
  need. **Adopted in the narrow form on 2026-09-16, and the reason above survives it**: a
  `task` tool that starts another flint the same way a Python caller or an MCP client already
  does (`--json`, the same stream, the same exit codes), with a depth bound and a `readonly` a
  child cannot loosen. What the bullet was right about is kept where the model reads it — the
  tool's own description says the child starts with no history from here, so its value is
  isolation and least privilege and **never a bigger window**: a `task` call does not buy
  context, it spends it. What is *not* adopted is the version the bullet actually feared — no
  shared context, no automatic fan-out, no merge step. One run may ask another run one question,
  and the answer comes back with its exit code and its session path attached. The other half
  `docs/agents.md` argues for, two runs in one directory being able to see each other, is built
  too (`flint who`, `src/live.rs`) and does not contradict this bullet — and the one gap that used to
  be named here is closed: a project that keeps a `.flint/` gets a second copy of the presence record
  and the mailbox, so two installations with different `FLINT_HOME`s see and hear each other. The
  marker is never created by flint, which is what keeps this from writing into a checkout nobody
  asked about. **The mailbox is built as
  well, in the only form this bullet allows by default**: `flint say` writes a line, a running flint
  shows it to its person, and it reaches no request — a peer's words cannot become a second author of
  this conversation, so the "one conversation, one context window" claim is untouched. **The one
  exception is asked for, per run, by the person**: `--hear-peers` (or `/hear-peers on`) passes what a
  peer said to the model with the next request, as a user message labelled as another process's words.
  That is a deliberate narrowing of what follows rather than a hole in it — it is off unless somebody
  says otherwise, there is no config key to forget, the relay goes into the request and never into the
  history the file is rebuilt from (so a resumed run inherits nothing), and the session records
  `"heard":true` on the message that was passed on. What the bullet protects is *automatic* second
  authorship, and there is none: without that flag, not one byte of a peer's message can reach a
  request, and that is what `tests/say.rs` asserts against the bytes a provider received. **A message
  can also be left from inside a run** — `/say <text>`, and `/say --to <pid>` to address one — which is
  the same write through the same function rather than a second path, so what this bullet allows by
  default is unchanged: a message written from a prompt is still a `peer` event, still shown to the
  person, and still outside every request unless that run asked to hear peers. **Stage 4 is adopted
  too, and the line is drawn inside it rather than around it**: a profile
  (`<project>/.flint/agents/<name>.md` — instructions, model, `readonly`) is a way to write down what
  "the explorer" means, and `tasks` runs several children **at the same time** when the model asks for
  several jobs in one call. What is still refused is everything the bullet was actually about: there
  is no shared context, so nothing learned by one child reaches another; there is no merge step beyond
  labelling each answer with the job that produced it; and flint never decides to fan out on its own —
  a model that wants N children asks for N children, and a cap of 8 jobs, 4 at a time, keeps "ask for
  N" from being a way to spend without saying so. The parent's context is still one window, and a
  child still spends it rather than adding to it. **A child's conversation is a session file, and it is
  not one of the person's** — measured before this was fixed: `/sessions` and the page's sidebar showed
  a child beside its parent with nothing to tell them apart, and `--continue` resumed the *child's*,
  because a child is newer than the parent that started it. It is written under
  `sessions/<dir>/children/`, which no listing reads, so "mine" is decided by the layout rather than by
  a flag every reader would have to honour; the child's `meta` line names the parent (`parent`, set
  from `FLINT_PARENT`, which the tool writes for the child and nothing else does); and reading the
  child's conversation is by path, or by `mv`-ing it up a level to adopt it. **A child nobody waited
  for is the default rather than an option**: `task` hands back a handle (its pid and its conversation)
  unless the call says `background: false`, because the first version made the parent sit still for
  minutes and a real session shows what that costs — the person typed at the frozen parent, the turn was
  dropped, and the record said the tool *never ran* while the child kept spending. `job_op` takes
  `status`, `output`, `wait` or `stop` for such a job, and a job that ends is reported exactly once — to
  the person when it ends, and to the model in its next request — with any `wait`/`stop` counting as
  having collected it, so a default that starts work nobody is waiting for cannot lose the work. **A
  background command is the same job**, because the same complaint had a second half: a ten-minute build
  or a long Python script had a timeout and a kill-tree but no handle, so it meant a held turn or a
  `nohup` the model invented by hand, with no exit code and no notice. `bash`, `exec` and `pwsh` take
  `background: true` (a command's output is usually the input to the next step, so they still wait by
  default, unlike `task`), write both streams to one log file under this session's directory, and answer
  to the same `status`/`output`/`wait`/`stop`. See
  "Background is the same record" in [`docs/agents.md`](docs/agents.md) for why the handle is the
  parent's own job record rather than the presence record, for the measured fact that a caller reading
  stdout to EOF waits for the whole process tree rather than for the run, and for the two decisions taken
  from DSH's job runtime and the one refused (flint will not spend a model turn nobody asked for).
- **MCP** — deferred, not refused: it is a protocol with real weight, and nothing here yet
  needs what it offers. **Being callable over MCP is the other direction and is built**:
  `examples/mcp/flint_server.py`.
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
