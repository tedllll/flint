# Handoff

State of `flint` as of the last working session, and what to do next. Written so the
next session — on another machine, or after a week — can start without re-deriving any
of it.

## Where things stand

**The sixth of the twelve items taken from the reading of Pi is built: a fork from a chosen point.**
`--fork` copied a whole conversation and its tail came with it, so "that went wrong four messages ago,
start again from there" could only be approximated by forking everything and deleting lines from the
copy by hand — which is what people actually did. `/fork <n>` now cuts *this* conversation at the n-th
question the person asked here and starts a conversation of its own from what came before it, and a bare
`/fork` lists the questions and writes nothing, because which point to cut at is the one thing the
command may not guess. The cut is at a **question** — a `chat` line from the person, the same unit
`trim_old_turns` counts turns in — for a mechanical reason and a human one: a tool result whose call was
left behind is a request the provider rejects, and a question is the only boundary in a conversation a
person can name at the prompt. A count of chat lines is not something anybody knows about their own
conversation, which is why the ordinal is what the command takes and why the list is what makes it
choosable.

**The lineage is a session event, never `meta.parent`.** Both doors — `/fork <n>` and `--fork <file>` —
write `{"type":"fork","from":…,"from_id":…,"kept":…}` above the conversation they describe, in the place
`import` writes its own and for the same reason: the file answers "where did this come from" before it
answers "what was said in it". `kept` is present only when the copy is a prefix, which is how one event
covers both a cut and a whole copy, and it is stored rather than derived because the branch *grows* from
there. **Writing `meta.parent` would have been actively wrong rather than merely imprecise**: that field
means "another run started this one" and is what files a conversation under `children/`, so a branch
would have been excluded from `/sessions`, the sidebar and `--continue` as somebody's child — the
opposite of what a fork is. The event is read back into `LoadedSession::origin` (which replaced the
`imported` field: both kinds of copy are one thing to every reader) and printed by the two doors that
name a conversation.

**Every provenance line moved onto a line of its own, for a reason that was measured rather than
argued.** The first version appended a fragment — `, forked from <file> (the first 4 messages)` — to the
line that names the conversation, and a test at 100 columns caught what that costs: the line already
carries the file in force and the model, so the whole thing passed 100 columns and the terminal wrapped
it *inside the count*, which is the part a reader is looking for. Both doors now print `this conversation
was imported from <file>` / `this conversation was forked from <file>, and holds the first <n> messages`
underneath, from one function (`origin_note`) — one wording, two doors, and no layout to keep in step.
The import tests' assertions moved with it; the sentence is unchanged in meaning and better on screen,
and the failure is worth keeping as the reason the line is shaped this way.

**Red first by mutation, seven times, because the tests were written after the code and that claim has to
be earned rather than asserted.** The cut moved one message (`asked[n - 1].0 + 1`) → the branch held the
question it was cut at; the fork line written after the messages (`write_messages` before
`forked_from`) → the ordering assertion failed with `meta, chat, chat, fork`; `kept` dropped to `None` →
"the branch does not say what it was cut from"; the `Forked` arm of `origin_note` emptied → the resumed
line said nothing about where it came from; bare `/fork` made to fall through to the usage line → the
list test failed; the `no_session()` check disabled → the `--no-session` count came back 3 instead of 4;
and the page's `/fork` values truncated to none → the picker test failed with the row present and
`"values"` absent. All seven were reverted. Two of the six new tests drive the flag itself
(`repl_of_with`, which takes arguments and merges stderr, because `flint: forking …` is on stderr), and
one drives a real `--web` run to read the frame the page draws its rows from — the values that test
asserts are computed from the conversation each time the frame is built, which is what makes the buttons
the questions this conversation has actually been asked.

**Gate: `cargo test` 600 passing, 1 ignored, clippy silent, both Node checks passing, and the browser
harness 56/56 by hand.** Clippy's silence took a fix (below), and the browser harness was re-run because
this round touches the page's rows: it was already passing before the run and passed after, which is the
whole claim a hand-run harness can make — it does not press `/fork`, so what it shows is that the new row
broke nothing the page already did.

**Clippy caught the shape of `seed`, and the fix is better than the signature it replaced.** Handing the
lineage to `seed` as an eighth argument (`from: (&Path, Option<&str>)`) made it fail the gate with `this
function has too many arguments (8/7)` — and the honest read is that the function was being told four
things that are one thing: the messages, the name they travel with, the file they came from and that
file's id. They are now a [`session::Copy`] value, which is six arguments, and the pair that most needed
keeping together is kept together: `from` with `from_id` is the same fact read two ways — the path flint
was pointed at and the id that file's own `meta` was created under — and a signature taking them side by
side invites a call that passes one file's path with another's id. This is the second time a lint has
paid for itself this round; the first is not a lint but the byte guard described above. One thing the
refactor settled that the first commit left open: the `/fork` arm had created its writer, written the
lineage and then the messages *by hand*, which is the three-call sequence `seed`'s own comment says must
not be repeated at a second call site. `Copy` carries `kept` now — how much of the conversation came
along is part of what the copy *is*, not of the call — so both doors write their copy through one
function and the ordering is one function's business. A follow-up commit, because the first was already
pushed, and the tests that hold the order are about behaviour rather than the call graph: the branch
still carries `"kept":2` directly under `meta`.

**Also in this round, and owed by the last one: the no-session door list was stale in three files.**
`--no-session` refuses `/new`, `/resume`, `/import` and `/fork` inside a run, and `README.md`,
`ROADMAP.md` and `docs/decisions.md` still said `/new` and `/resume` — the import round added the third
door without editing the list. The four are named together now, in the same commit as the fourth, and
the invariant is unchanged: one property, one sentence, every door that would open, copy into or name a
conversation.

**And one thing the previous round left behind: `docs/session-format.md` said "eight" event types.**
`fork` is the ninth, and the count, the "not conversation and never become history" list (now three:
`peer`, `import`, `fork`) and the damage rule all move together, because a count in prose is a claim the
next reader can check against the table above it.

**The fourth of the twelve items taken from the reading of Pi is built and pushed: the cache hit rate,
beside the token counts it is a share of.** The number is what tells a person whether the prompt flint
builds is stable from turn to turn -- a cached prefix is money and latency that a prompt reassembled in
a different order every turn quietly spends -- and the item was taken for that reason rather than
because Pi prints one too.

**Two endpoints, two field names, one number.** DeepSeek reports `prompt_cache_hit_tokens`; OpenAI
reports `prompt_tokens_details.cached_tokens`; both mean "a subset of the prompt the provider had
already seen", so `provider::usage_from` collapses them and nothing downstream knows which endpoint
answered. `Usage` gained `cache_hit_tokens: Option<u64>`, and the `Option` is the whole design in one
type: **not reported** and **reported zero** are different facts, only one of them is about the prompt
flint builds, and an endpoint that says nothing about caching printing `0% cached` would accuse the
prompt of being unstable. So the rule the tests hold is not only "the rate is shown" but "an endpoint
with no split produces no cache text at all" -- nothing on `/usage`, nothing in the footer, no
`cache_hit_tokens` key on either `--json` frame.

**Where it is printed, and why there rather than on a screen of its own.** `/usage` puts it on the same
line as the prompt it is a share of (`cache 87% (871 of 1000 prompt tokens)`, because 871 is good news
on a 1000-token prompt and bad news on a 200,000-token one), and the footer under every answer appends
`, 87% cached` -- the footer because the point of the number is to notice it *move*, and a number nobody
sees while working is a number nobody notices. The rate itself is computed at the point of printing and
never stored: it is arithmetic on two numbers that are both in the file, and a stored percentage is a
number that can disagree with its own inputs after a hand-edit. It refuses a prompt of no tokens (the
division would panic, and a request that sent nothing has no rate) and bounds itself at 100 (a provider
reporting more hits than tokens is wrong, and `130%` would spread the mistake).

**Building it found a small piece of damage, and fixing that is part of the round.** `session::load`
has always parsed the last `usage` line of a file into `Loaded::last_usage` -- and nothing anywhere
consumed it, so `/usage` on a conversation somebody came back to answered *"no usage reported yet by
this provider"* while the line it wanted sat in the file it had just opened. Every mid-run rebuild had
the same hole for the same reason: `/model`, `/provider`, `/reload` and `/readonly` all build a
replacement agent around a conversation that stays, and all of them hand over the messages and nothing
else. `Agent::set_last_usage` is now called in all four places, so the counts survive a resume and a
switch; the field went from written-and-never-read to the number `/usage` prints. It was found by
writing the test, which is the reason that test drives a *resumed* session rather than a live turn: a
resumed session is race-free (nothing is typed while a turn is running -- an earlier version of the
test typed `hello` and `/usage` in one write and the second line interrupted the first turn, which is
what an interrupt looks like from inside the harness), and it holds the format as well, since the number
comes back out of the file.

**Red first, in five places, and two mutations.** `reads_both_shapes_of_the_cache_split`
(`src/provider.rs`) failed on `left: None, right: Some(871)` -- the DeepSeek shape was being ignored --
and holds the OpenAI shape and the silent-case control beside it. `a_cache_rate_needs_both_numbers_and_stays_a_percentage`
and `a_usage_without_a_cache_split_says_nothing_about_caching` (`src/event.rs`) hold the arithmetic and
the absent field. `the_turn_footer_carries_the_cache_rate` was watched failing with the old footer
`[ctx 1000 prompt + 7 completion = 1007 tokens]` and no rate. `usage_prints_the_cache_split_it_read_back_from_the_session`
was red as "no usage reported yet" (the dead field above) and then, deliberately, red again with the
cache fragment removed from the `/usage` line. `the_cache_split_reaches_a_program_and_never_as_a_zero`
(`tests/json_output.rs`) was red on `Null` vs `871`, and the absence half was made red on purpose by
mutating the frame to always insert the field: it failed with
`{"cache_hit_tokens":0,…}` where the key had to be missing. Both mutations were reverted.

**What was read and not taken: the cost.** `ROADMAP.md` B5 stands -- flint does not know what a token
costs on the endpoint it was pointed at, and Pi knows because it generates a model registry from
`models.dev` and OpenRouter, which is a dependency and a derived table this project will not take. The
flint-shaped version (a price in the provider's own config table) is a config key plus arithmetic that
would need its own argument, and the rate is the half that answers the question the item was taken for.

**Not measured, and said so in the docs: a real endpoint's number.** No key is in this machine's
environment, and spending one that is not would not be flint's call, so every claim here is about the
plumbing -- both field shapes read into one number, the arithmetic right for the values in the tests,
and silence from an endpoint that reports no split. The honest way to get a real figure is two short
turns against a provider with a key; `/usage` then prints what that endpoint said. Reading flint's own
construction first does say something about what to expect: the system prompt is built once per agent
out of facts that are properties of the *run* -- shell, working directory, where flint keeps its config
and sessions, the `AGENTS.md` note, the skill names -- with **no clock, no date and no per-turn state**,
so within one conversation the only thing that moves is the conversation appended after it. The prefix
is stable by construction rather than by tuning, and the two ways to break it are deliberate: a mid-run
`/reload`, `/model` or `/provider` rebuilds the prompt and rewrites the first message (and a model
switch invalidates the cache wherever it lives anyway).

**Test counts this round: 583 → 589 passing, 1 ignored** (341 lib, 5 in the binary's own tests, 33
`agent_loop`, 83 `cli_output`, 36 `json_output`, 7 `balance`, 4 `search_tool`, 20 `term_capture` plus
the ignored cost measurement, 27 `web_view`, 10 `who`, 17 `task`, 6 `say`), clippy silent, both Node
checks passing.

**The fifth of the twelve items taken from the reading of Pi is built: `/import <file>`, a
conversation copied in from a file this run did not start from.** Pi exports a session, imports one
back and shares one as a static page; flint already opens any session file by path (`--resume
<path>`), which is why the interesting part of this item was never the loading. It was deciding what
the command must *not* do. `--resume` continues **inside** the file it is handed — that file grows,
gains this machine's `usage` lines, and keeps somebody else's `cwd` in its `meta` — and that is the
wrong thing to do to a file somebody gave you or you hand-edited. So `/import` copies, and the source
is not written to at all. It is `--fork`'s act for a file this run never started from, and it is what
makes a hand-written file a first-class way to work rather than something only `--continue`'s
directory scan can find.

**The copy says where it came from, on its own line above the conversation.** A new session event,
`{"type":"import","from":…,"from_id":…,"messages":…}`, and an event rather than a field on `meta` for
the reason `title` and `switch` are events: the file is append-only and a fact that arrives at a
moment is a line — which also makes a copy of a copy read as a chain of files rather than as one
original. It is written before the messages it names, so a reader going down the file is told where it
came from before being told what was said, and `session::load` reads it into `LoadedSession::imported`,
which **the two doors that name a conversation** then print — the startup
`resumed` line and `/resume`, through one function so the wording cannot drift. That read-back is not
decoration: this repository treats a field that is written and never read as damage, and the previous
round fixed exactly that fault in `last_usage`. `from` is the path as it was given, a fact of the
moment rather than a pointer to follow — the file may have moved since, or never have been on this
machine at all, and a copy that could not be read without it would not be a copy. `from_id` is the
source's id as flint read it (its `meta` id, or its file name when the file had no `meta` at all).

**Three refusals, and each is a decision rather than an error path.** A file with **no conversation in
it** is refused instead of imported as nothing: an empty import would answer "imported" with a
transcript of nothing and leave a session in the list that nobody could tell from a real one. The file
**this run is currently writing** is refused — `/import` would otherwise copy a growing conversation
into itself, one line behind where it had got to — and the sentence names `--fork` at startup, which
is that act. And `--no-session` refuses `/import` like every other door that would create a
conversation, which is the flag's whole promise; its test deliberately hands it a file that *would*
have imported, so the refusal is about the flag and not about the file.

**Red first, four times, and four mutations.** The command did not exist, so the first run of the new
test failed with `unknown command '/import'`. The copy test then failed on the provenance with
`writer.imported_from` commented out (the file held `meta` and two `chat` lines and nothing else), the
empty-file test failed on `if false && count == 0` (it asserted the missing "nothing to import"
sentence), and the source-untouched assertion failed when the writer was mutated to
`SessionWriter::resume(&path)` — the exact mistake the command exists to prevent. The ordering claim
was watched red by writing the messages before the import line, which the *unit* test could not catch
(it builds the writer itself) and the command's own file could: the assertion compares the line
positions, and the failure printed `meta, chat, chat, import`. All four mutations were reverted.

**A test-helper note worth keeping.** The three new tests drive the REPL in a **working directory of
their own** (`repl_of`), which is not fussiness: `repl` runs the child in the test process's
directory, so a run started there reads *this* repository's `AGENTS.md` and belongs to this
checkout's sessions. None of these lines reach a model — every one of them is a command, and the
provider in the config is a port that refuses — so the whole test costs a process spawn.

**The one wording fault the built feature had, found by running the shipped binary by hand.**
`/import` was finished, committed and pushed, and the hand check — a scratch home, a one-message file
given to `/import` — printed `imported: given.jsonl (1 messages) into …`. The count is right and the
plural is not, and one message is the case an import hits most often: an import is usually one
question somebody handed over. A count as a person would write it is now one rule (`counted`) used by
the four lines that put a conversation's size on screen — the startup `resumed` line, `/resume`,
`/import`, and the header of a drawn transcript (`— 1 earlier message, `). The claim lives in the two
import tests: a one-message import says `(1 message)`, and the resumed line reads `(2 messages, model
stub-model), imported from given-to-me.jsonl`. Watched red by putting `{count} messages` back into the
`/import` line, and the first version of the assertion was itself wrong in a way worth remembering:
`contains("1 message")` passes against `"1 messages"`, so the assertion has to be on the whole
parenthesised fragment.

**Also in this round, and not part of the twelve: the agent's own working file was damaged and
rebuilt.** While mutating the ordering claim, a `Get-Content -Raw` / `Set-Content -Encoding utf8`
round trip on `src/main.rs` read UTF-8 as the system code page: the file came back with a BOM, and
every em dash in a comment had turned into the three-character CP936 reading of its own bytes. It was
caught immediately (the byte check plus the `no_mojibake` test this repository already has), the
pristine file was restored from the last commit, and the round's edits were re-applied from the record
and then compared line by line against the damaged copy with non-ASCII stripped — 6690 lines each,
two intended differences, nothing lost. The lesson is the one `AGENTS.md` already carries and this
session earned twice: **never round-trip source through PowerShell's text cmdlets**; use the file
tools, or `[System.IO.File]::WriteAllText` with an explicit UTF-8-without-BOM encoding. The guard then
caught this paragraph, because writing *about* the damage is one more place the damaged characters can
appear — which is the guard working as designed rather than a false positive (the first version of
this paragraph quoted them, and the tree was red for a few minutes in CI until the words were used
instead of the bytes).

**A conversation you switch to inside a run is drawn, and not only loaded — reported against the
terminal, and the page had already been doing it.** Starting with `--continue`, `--resume` or
`--fork` has printed the tail of the conversation since sessions became reachable, but `/resume
<n|id>` — the door you use when you are already in a run and want the older conversation — printed one
line naming the file and left the screen otherwise as it was, so a switch that loaded a conversation
and a switch that loaded nothing looked the same. It now prints the same block, from the same function,
at the same point in the arm (before the loaded messages are *moved* into the new agent, which is what
the ordering costs). The page was never the problem: `Viewer::follow` makes a browser re-read the file
the run moved to, so this is the terminal catching up with its own startup path.

**The second of the twelve items taken from the reading of Pi is built and pushed: `--no-session`, a
run that writes no conversation.** A one-off question asked against a big model, a look at a machine
somebody else owns, a script that wants no trace in the list. The file is the small half; the *doors*
are the whole of it, and that is the sentence to remember: a flag whose promise is "nothing is written"
is only true if the ways to write something are closed, so `--continue`, `--resume`, `--fork` and
`--name` are refused on the command line, `/new` and `/resume` are refused inside the run, and the one
funnel every mid-run switch goes through — which creates the session file when the old conversation has
not said anything yet, so a `/model` would have quietly created the file this flag promised not to
write — cannot create one either. What the run still writes is what it needs to work, under a name of
its own: spilled tool output and a background command's log go to `spill/unattached-<pid>/`, because
two such runs would otherwise share `spill/unattached/` and overwrite each other's `1.txt`. And the
flag travels to a `task` child, which is the half worth reading twice: a child is a conversation *this*
run asked for, it is started by `task_argv` like any other child, and because a session-less parent has
no conversation id to set `FLINT_PARENT` to, that child's file would not have been filed under
`children/` at all — it would have appeared in the person's own list. The three places that name a
child's conversation now tell "none, and none was asked for" from "not named yet": the handle, the
status line, and the answer a `wait` hands over.

**Also in this round, and not part of the twelve: the `aarch64-unknown-linux-musl` job went red on
`bacae13` and it was not the code.** It never reached cargo — the "Install zig" step of a workflow
matrix built for static musl failed after *one second*, while the x86_64 musl job on the same commit
downloaded the same tarball for fourteen minutes and then built and passed. `goto-bus-stop/setup-zig@v2`
is unmaintained, now force-run on Node 24, hits ziglang.org from every job and caches nothing; the step
is `mlugg/setup-zig@v2` in its own commit (`adcde74`), which rotates the community mirror list, checks
the tarball's signature and keeps the download and the Zig cache between runs. Nothing about the Rust
build changed. **Measured on the run that carried the fix: the step went from 981 and 991 seconds on the
two commits before it to 10 (arm64) and 45 (x86_64), and all four targets are green** — so the sixteen
minutes per musl job were never the download's size, they were one mirror being hammered twice a push.

**The first of the twelve items taken from the Pi reading is built and pushed: a command knows what run
it is in.** `FLINT_SESSION` (the conversation's file, absolute), `FLINT_PROVIDER` and `FLINT_MODEL` are
set on every command a run starts — the model's `bash`, `pwsh` and `exec`, and a person's own `!cmd`,
which goes through the same runner and must not be told a different run. The names follow Pi's
(`PI_SESSION_FILE`, `PI_PROVIDER`, `PI_MODEL`); there is no `FLINT_SESSION_ID` because flint has no per-
entry id to give, and the path is the name. `ROADMAP.md`'s bullet for it was edited in the same commit,
as the repository's rule for adopting an item says.

**The one decision worth remembering is that "no session" must mean *removed*, not merely unset.** A
command inherits the environment of the process that spawned it, and that process may itself have been
started by another run's command (a model running `flint -p ...` through `bash`, or `flint exec`). The
first cut skipped an empty name instead of taking it away; the e2e test that plants a stale
`FLINT_PROVIDER` in flint's own environment caught the stale value arriving at the command, which is a
class of defect that reads identically from inside flint. So every name flint owns is either written or
removed, and `flint exec` — not a conversation, so nothing true to say — removes all three. Both spawn
sites now go through one function (`apply_child_env`), which is where the six-line proxy block had
already been duplicated; a fact that reaches a foreground command but not a background one is a fact a
script cannot rely on. A `task` child still gets `FLINT_DEPTH` and `FLINT_PARENT` and a command gets
neither: a command is not a run and does not claim to be one.

**The tests are the point of the round rather than the code.** Two in `src/tools.rs` (the env a command
is handed, asserted on `Command::get_envs` so that "removed" and "never mentioned" are different
answers; and the same through a real `bash` call) and three in `tests/cli_output.rs` (a `-p` run whose
model calls `bash` to write the three variables into a file, compared against the session file the run
actually wrote; the person's `!cmd` in a real REPL against the same file; and `flint exec` with the
names planted in its own environment, which must not pass them on). The wiring was watched failing
first in both halves — with `run_env.apply(cmd)` removed, and separately with the `Agent::new` call
removed, each failing with `"%FLINT_SESSION%\r\n%FLINT_PROVIDER%\r\n%FLINT_MODEL%\r\n"` where the values
should have been.

**The two rounds after §10 were §9's page work, both asked for in the same breath on 2026-09-17, and
both are built and pushed.** A file path in the transcript is a button that opens the file beside the
conversation (`GET /file`, `docs/web-mode.md` §12); and the run's own background work — a `task` child,
or a `bash`/`pwsh`/`exec` command started with `background: true` — is a **jobs** list in the header,
drawn from `GET /jobs` and told to re-read itself by an `event: jobs` frame that carries a revision
rather than the list (`docs/web-mode.md` §13). Both were measured in a real browser from a harness that
starts its own scripted model, which is what lets a claim be about a real turn rather than an empty
transcript. The records are in the sections below, and §9 of `ROADMAP.md` now points at them.

**The jobs round has two halves and both are in, the second one last.** Seeing a job you no longer want
is only useful if you can end it, and until the second half the only door onto that was `job_op`'s
`stop`, which is a tool: a person who started a ten-minute build by accident had to ask the model to
stop it, or kill flint and take the child with it. `/jobs` now prints the same listing the panel shows
and `job_op status` returns (`tools::jobs_report`), `/jobs stop <pid>` ends one through the same
`tools::stop_job` the tool calls, and the page reaches it as a `danger` row whose candidates are the
live rows of the jobs panel — the pids already on screen, so nothing new had to be sent for it. The
defect that came out of building it is the one worth remembering: **a kill's exit status is the shell's,
not the command's**, so a job a person stopped deliberately was listed as `failed` on Windows
(`taskkill /T /F` leaves the `cmd.exe` reporting 1) while Unix reported -1 and read correctly. `Job`
now records that *this* run ended it (`ended_by_us`, set before the signal) instead of inferring it from
a number afterwards (the budget ending a command counts as this run ending it too, with the note saying
why). `docs/web-mode.md` §13 has the measured record; the browser harness is **56 of 56** now, up from
53.

**And a reading of Pi (pi.dev) landed as a document, not as code** — `docs/pi-agent-harness.md`, with
`AGENTS.md` and `ROADMAP.md` pointing at it. It is the minimal TypeScript agent harness, built on the
opposite bet, and the reading found more agreement than either side would probably expect (no
permission layer, plans and to-dos as files, append-only JSONL sessions, writing to scrollback rather
than taking over the terminal, progressive disclosure, a small prompt, no MCP). Everything in it is
sourced from a page that was loaded, and the pages that were not are named. Twelve of its candidates
were then **taken** and one declined — the run-level tool allowlist — so they are in `ROADMAP.md` under
"Taken from a reading of Pi" rather than in the candidate list, and `docs/decisions.md` gained a
section ("Why this program, next to Pi") answering the question the reading raises: the overlap is the
commodity layer, and the three things flint has that Pi refuses on purpose are the reason this program
exists. One consequence is written down there rather than left to be discovered: **flint will never
match Pi on provider breadth.**

**A note for the next session, because it cost time here: pushing fails on the network this machine is
on.** `origin`'s push URL is `git@github.com` over port 22 and the connection is reset mid-transfer
("Connection reset by peer", several attempts in a row, after a successful handshake). The push goes
through over GitHub's port-443 endpoint, which is the same key and the same repository:

```
GIT_SSH_COMMAND="ssh -i C:/Users/zhangzhuo/.ssh/flint_github -o IdentitiesOnly=yes -p 443" \
  git push ssh://git@ssh.github.com:443/tedllll/flint.git main:main
```

The deploy key authenticates fine on port 22 (`ssh -T` answers "Hi tedllll!"), so this is transport
rather than credentials, and it is the same flakiness that makes some `web_fetch` calls fail. The
remote was deliberately left as it is; use the line above when a push hangs or resets.

**Before those, the round that was open was §10 of `ROADMAP.md`: "flint as a function a program can call" — the
agents work landed in front of it, and §10 itself is now done, steps 1–7.** The section is an audit in
three buckets (what cannot be done at all, what cannot be told apart, what is a hole), the reference
points it was measured against (Claude Code's `-p --output-format json`, `llm`, and the `sysexits.h`
convention for exit codes), and the order it was built in. Step 1 landed the turn's `outcome` and the
exit codes, and fixed the C1 bug (a `/stop`ped run exited 0 while carrying a truncated answer — now
130).

**Step 2 is `error.code` produced where the cause is known. Its first job was money, and it is done**:
§10 **B6** was added after the fact, from a report of hitting an empty balance repeatedly, and it was a
bug fix as much as a classification. `provider::ProviderFailure` now carries `code` and `retryable`
out of the code that read the response, `classify` reads the body as well as the status, the `error`
frame names the cause, and the exit code follows the cause (`69` for money or credentials, `75` for a
failure the retries could not outlast). **Still open in B6**: nothing — the `map_calls` circuit breaker belongs to step 7 and is built there.
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
`command` were missing). **Step 7, the Python side, is built too** — `Chat`, the streaming callbacks,
`history()`, the three promises about a file, `require_read=`, `map_calls(workers=)` and the balance
breaker — so §10 is done and the queue moves on to what §11 and the other sections still hold.
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
doors are already built. **Stages 1–4 are all built** (`flint who`, the `task` tool, `tasks`, profiles,
`flint say`, `background: true` with `job_op` for a job nobody waited for — a `task` child, or a
`bash`/`pwsh`/`exec` command — and the `--hear-peers` /
`/hear-peers` opt-in that lets a peer's words reach a model when a person asks), the "Subagents" entry
in `ROADMAP.md`
was edited in the same commit as the code that adopts it, and **the five decisions at the end of that
file were answered on 2026-09-16 at the recommended values** — the depth bound (`FLINT_DEPTH`, maximum
2), the mailbox default (a peer's words are shown to the human and reach a model only when that run was
asked to hear peers, which is what decision 3 said the opt-in had to be), what "another agent is here"
may claim (no author, ever), and presence in
`FLINT_HOME` only. What stage 3 still owed — the `.flint/` marker in the project, so two installations
with different homes can see each other — **is now built**, and the answer decision 5 left open is
settled the other way with a bound on the cost: the home record stays, and a project that already keeps
a `.flint/` gets a second copy of the presence record and the mailbox. **flint never creates the
marker**, so nothing is written into a checkout that did not ask. Stage
4 was written into `ROADMAP.md` and `docs/agents.md` in the same commit as the code, with the line
drawn where it was built: profiles and an explicit, capped fan-out, and nothing above it — no shared
context, no merge, and no flint choosing to parallelise on its own.

Everything is committed, the working tree is clean, and `main` is pushed to `origin/main`.
As of the commit that carries this file, `cargo test` is 600 passing, 1 ignored on this machine
(343 lib, 5 in the binary's own tests, 33 `agent_loop`, 92 `cli_output`, 36 `json_output` (7 structured
output, 1 the heartbeat, 2 the stop channel, 8 the exit codes and the turn's outcome, 2 the
balance, 1 what a caller's pipe must not come back out of, 3 the refusal a `--json` caller has to
be able to read, 4 the answer written where the caller asked, 3 the file inlined into the prompt,
1 the stream checked on its bytes, 1 how long a turn took, 1 the cache split and its absence),
7 `balance`, 4 `search_tool`, 20 `term_capture` plus the ignored cost measurement, 27 `web_view`,
10 `who`, 17 `task`, 6 `say`),
and one more on Unix, `tty_hangup`, which is `#![cfg(unix)]` and needs a real pty — as is the Unix
half of the process-group kill, `a_killed_command_takes_its_children_with_it_on_unix`. `cargo clippy
--all-targets` is silent, both `node scripts/term-layout-test.js` and `node scripts/web-view-test.js`
pass, and `node scripts/browser-controls-test.js` is **56 of 56** — the browser harness, run by hand
because CI has no browser, and the only place the two defects in the page's later controls were ever
visible (the first two were found by the file-preview claims: a fixed panel that covered the block the
path was pressed in, and a covered path that could not be pressed at all until the block was opened;
the jobs claims then found two *harness* defects rather than page ones, both recorded in
`docs/web-mode.md` §13, and each looked like a page bug until it was read properly; the three stop
claims are the ones that hold a kill to reading as `killed`).
`python examples/python/test_call.py` is 118 checks, all passing (one of them waits out the
fifteen-second retry ladder on a dead endpoint, deliberately: that is where `75` comes from), and
`python examples/mcp/test_mcp.py` passes its own 23.

**`/say` is built, so leaving a peer a message no longer needs a second terminal, and the run that
writes one is not shown it back as a peer's.** It is the same write as `flint say` through the same
function (`live::say_line`), and the first thing building it corrected was a sentence that had become
false: the reply said "never sent to a model", which was true until `--hear-peers` existed and untrue
after it. It now says what happens — a run working here shows it to its person, and a run started with
`--hear-peers` also passes it to its model — and the test that held the old wording was rewritten with
the reason instead of deleted. Two things the round added on top of the shared write. The reply **names
who is here**: the presence records filtered to the runs that share this mailbox (`live::peers_here`),
so it says which pids will read it, or plainly that nobody else is working here and the message waits
in the file for the next run — "nobody" is not a failure, which is why the CLI still exits 0 either
way. And **the writer is not shown its own line**: a run follows the mailbox it writes to, so without a
fix the next turn boundary printed `peer pid 12345 says: …` about itself. The fix is exact bytes and
not the `from` field — `Mailbox::note_own` keeps the line the append returned, the reader consumes one
match per note — because `from` is a claim that anything able to write the file can forge, and matching
it would let a forger hide its own message instead. `flint say` and `/say` share `live::say_reply`, so
the two doors cannot drift apart in what they promise. The mutation check is the honest one for this
kind of filter: commenting out `note_own` makes the new test fail with "the run showed its own message
as a peer's". `tests/say.rs` is 6 tests now (2 of them new), the lib is 323 (2 new unit tests over pure
helpers — the audience sentence in its three shapes, and the own-line filter, which was extracted as
`live::peer_messages` so it can be held with no home directory in the way), and the binary's own tests
are 5 (`/say`'s flag split, including that `--to4242` is prose).

**The round in front of that one is closed, item by item, and every item ended with a test that was
watched fail first** — the five the person asked for, in the order they were asked for:

1. **The mojibake guard is a whitelist.** The scan that looks for damage in the documents used to
   *skip* a line that carried a marker it knew (`中文`, a listed repo path) and so was blind to real
   damage sitting beside one. Now a line must *match* a known shape to be exempt, and anything else is
   reported. `3ec8aff`.
2. **A project that keeps a `.flint/` can be seen across two `FLINT_HOME`s.** Presence and the mailbox
   get a second, project-local copy where a person has already put the marker — and flint still never
   creates it. `b4b5e9e`. That one then caught a bug of its own, on the Windows runner rather than on
   this machine: `%TEMP%` there is the 8.3 short name while `home_dir()` is `C:\Users\runneradmin`, so
   the walk that stops at the home directory compared two spellings of one place, went past the stop,
   and accepted the home's own `~/.flint` as a project marker — which moved the mailbox path *mid-run*
   and failed a test about what arrives. The fix is `same_place` (canonicalise both sides, and fold
   case and separators on Windows), with the red-first proof run locally in both spellings. `f2ab33e`.
3. **A Unix command's children die with it.** The Windows kill was `taskkill /T`; on Unix the shell was
   killed and its background work was not. `KillTree::detach` puts the child in its own process group
   and the kill signals the group. Committed as a test that CI watched fail on ubuntu first
   (`the command's child outlived the kill and wrote …child-survived.txt`), then green. `43297ae`,
   `eb22cb8`.
4. **`/config set <key> <value>`**, and the page form it unlocks. Building it found a lie in the wizard
   it copies: `/config edit` saved the file and left the running tools on the old settings, because
   every tool holds a *copy* of the config and `max_steps` went into the agent. Both paths now end in
   the same rebuild `/reload` uses, and the message says `in force now` because it is. `bb9e07c`.
5. **The page's later controls, measured in a real browser** — `0b15390` for the harness, `97229fc`
   for what it found. See "A browser" under "Still owed on the page" below; the short form is that a
   fresh `--web` run's page drew no controls at all, and that the `commands` panel could cover the
   `send` button.
6. **Everything §11 still listed as reasoned rather than seen, read in a real browser** — the
   sidebar's own `⋯` menu on both kinds of row, both drag grips (a real pointer drag, the arrow keys
   and the double-click reset), and a picker driven from the keyboard. All of it passed the first
   time, which is a result about the page and not about the harness: the mutation that neuters the
   page's `pointermove` handler fails the two drag claims, so they are load-bearing. The harness went
   from 20 claims to 34. What a browser has still not touched is now one widget and one section: a
   native `<select>`'s open popup, which is the operating system's, and §11's own long answers and
   reconnection, which were measured on macOS over a different harness.

So §8 is not only built but *driven*, and the list at the end of `docs/web-mode.md` §11 is down to the
operating system's own popup and the page's performance section — no control of the page's is left
asserted-but-undriven.


**A `task` child is started in the background by default now, and a job that ends says so — the default
was the bug.** Reported directly ("my subagent cannot run in the background; the main session calls it
and can only wait"), and confirmed in the person's own session files rather than taken on trust: one
conversation, two `task` calls, **neither** carrying `background`, one of them interrupted by typing at
the frozen parent -- which drops the turn -- with the literal tool result
`interrupted by the user: tool 'task' was requested but never ran`. The mechanism (supervisor, handle,
`job_op`) was built and tested; what was wrong is that the tool's description ended with "this tool
waits for it", so a model never asked for the handle. So `task` returns the child's pid and conversation
at once unless the call says `background: false`, and the second half is what makes that safe rather
than a way to lose work: a job that ends is **reported exactly once** -- to the person in the transcript
(`tools::notice`, which used to be silent for a child nobody asked about) and to the model as one
user-role message labelled `[a note from flint, not from the person: …]` in its next request, listing
each ended-unread job with the pid and the verb that collects it. `wait`, `stop`, and a `stop` on a job
that had already ended all count as collecting it, which is what makes "once" true instead of "every
request from here on"; the bookkeeping is one `AtomicBool` on the job. The report is a *view*
(`Agent::with_jobs`, beside the peer relay, never `history`), so a conversation resumed from the file is
not told about a job that ended days ago. The shape came from reading DSH's job runtime: the
kind-independent verbs and the once-only, read-suppressed notice are adopted, and **waking an idle owner
with a turn of its own is refused** -- flint's REPL has a person at the keyboard and a model turn nobody
asked for is a bill nobody agreed to. Tests, all watched red first: the default (a parent that exits in
under six seconds against an eight-second child), the explicit `background: false` door (where a handle
would be a lie), the report landing in exactly one request -- the parent was given a turn *after* the one
that carried it, and the mutation that drops the `reported` check fails with "the model was told 2
times, out of 4 requests: [2, 3]" -- and the person's notice on stderr. Five existing tests that had
quietly assumed `task` waits now say `background: false` with a comment saying what each one is about.

**The other half of that landed next, and it is the same record rather than a second feature: a
background command.** Asked for directly ("some other tasks must be backgroundable too, surely —
certain Python scripts, bash"), and the ground was already prepared, which is why the change is small:
the sentence that had said "`bash`/`pwsh`/`exec` still wait" was true only of the *default*. `Job`
gained a `kind` (`Child`/`Command`), a `log` path and a read cursor; `bash`, `exec` and `pwsh` gained
`background: true`, which spawns through `start_background_command` — **both streams into one file**
under this session's spill directory (`background-<tool>-<n>.log`, named in the handle, readable with
`tail` while it is still being written) — and a supervisor that owns the child, its budget and the
`KillTree`; and `job_op` gained `output`. The variant renamed `job_op` to `job_op` in the same commit,
because it had stopped being only about children. Three differences are all that the kinds do not
share: where the output goes (a conversation versus a log file), how it is stopped (`/stop` on stdin
versus `taskkill /PID … /T /F` — killing the shell alone leaves `cargo` holding `target/`, measured in
`KillTree`'s comment), and what `output` means (a child has none; its answer is `wait`, while a command
can be polled incrementally from the job's own cursor, so watching a build costs the new lines and a
finished job read to its end counts as collected). Four decisions worth keeping: **commands stay
foreground by default** while `task` does not (a command's output is usually the input to the next
step); a background command gets the **long** default budget (900 s, enforced by the supervisor, so it
comes due whether or not anything is waiting); **`output` is a window and `wait` is the answer**; and
**nothing new was invented for the notice** — a command's exit rides the same `reported` flag to the
same two lanes. Tests, watched red first: the handle arriving before the command (the call used to
block 3.07 s), reading a running command and then collecting the whole of it (the mutation that ignores
the cursor fails with "the same output was handed over twice"), a stop that keeps what was already
written, the budget coming due with nobody watching (the mutation that removes the supervisor's
timeout fails with "the budget was not enforced while nobody waited"), and the readonly gate refusing a
mutating *background* command before anything is spawned. Two limits are written down rather than
implied: a background command dies with the run on Windows and not on Unix (the same
`docs/windows-tooling.md` §6.1 gap), and it is not a run — no session file, no presence record, so
`flint who` will not name it.

**A run can now be asked to hear its peers, which is the opt-in decision 3 was holding open.**
`--hear-peers` for a run, `/hear-peers [on|off]` while one is open, and the same switch on the page
(drawn from the run's own `toggles` field, so the fourth switch cost the page nothing). A message that
arrived between turns is then sent with the *next* request as one user-role message labelled as another
process's words — not as the person's — and the transcript says `passed on to the model` instead of
`not sent to the model`. Four things hold the safety argument rather than promising it: the relay goes
into the request *view* (`Agent::with_peers`, beside `prune_tool_output`) and never into `history`, so
the file keeps the `peer` event and a *resumed* run inherits nothing; there is no config key, because a
standing property is one somebody forgets they set; the session event gained `heard`, which is the only
place that answers "was the model told this" since a request body is kept nowhere; and the mailbox is
still read between turns only, so nothing arrives mid-loop. `--hear-peers` with `-p` is **refused**
rather than ignored — a one-shot run has no next turn to relay into, and a flag that silently did
nothing would look like it worked, which is the dangerous direction for a switch whose point is that
somebody chose it. Mutation-checked: forcing `heard` to false in `note_peer` fails the new `say` test
on "relayed nothing", with the two prompts visible as two turns in the request body.

**Two documents were lying, and both are corrected in the same round.** `ROADMAP.md`'s "Subagents"
bullet still said a peer's words "never reach a request"; it now says what is true — never *by default*,
and exactly once, on the person's word, when `--hear-peers` is on — with the reason this is a narrowing
of the bullet rather than a hole in it. And `HANDOFF.md`'s own "the Windows half of the tooling plan is
still unwritten" was stale by five steps: `docs/windows-tooling.md` §7 marks all five done, and the two
tests that paragraph called unwritten exist and pass (`a_quoted_command_reaches_the_shell_verbatim`,
`a_powershell_script_runs_from_the_file_it_was_written_to`, `a_killed_command_takes_its_children_with_it`).
That shorter list §7 ends with — a Unix process-group kill — **is now closed too**, so nothing on it is
open: `KillTree::detach` puts a command in a process group of its own (`CommandExt::process_group(0)`,
a `std` method rather than `setsid` or an unsafe `pre_exec`) and both kill paths signal the group with
`libc::killpg` — the guard's `Drop` and `Job::kill`, whose `kill <pid>` subprocess is retired with it.
The red was watched where it could be: the test (`a_killed_command_takes_its_children_with_it_on_unix`
in `tests/agent_loop.rs`) was pushed **alone**, the ubuntu job failed with `the command's child outlived
the kill and wrote /tmp/flint-tree-kill-unix-3199/child-survived.txt`, and the commit with the code made
it pass. The other item on it, the line-ending sentence for `apply_patch`, is built — and building it
measured that a patch cannot express a CRLF line at all (`str::lines` drops the `\r` before each `\n`,
and the file's lines keep theirs), so the sentence names the reason and the tool that works, `edit`.

**And the last "not measured" note in `docs/web-mode.md` §11 is now measured**: a report asked for
*while a turn runs*. The gap was never the code — the wait is stashed (`Handover`) and answered once the
model is done — it was that the suite had no turn slow enough to ask during. It has one (a stub provider
that draws a delta and holds the socket open), and `a_report_asked_for_mid_turn_waits_for_the_turn`
drives a real `--web` process through it: the report is accepted at `/report` mid-turn, the turn is then
stopped for real (`outcome: stopped`), and both frames are read off one feed so the **order** is the
assertion — `turn.completed`, then the `panel: true` answer. Mutation-checked by inverting the ordering
assertion, which fails and prints the two frames.

**A batch of concurrent runs found a real fault in the session file naming, and it is fixed.** The
id was `<seconds>-<milliseconds>`, and six `map_calls` runs started at once proposed **the same id
five times over** — they were started within a millisecond of each other and did the same work before
reaching `new_id` — and the older `append` answered "the file is already there" by opening it for
append, so five conversations went into one file. One writer per file is the property every listing,
`--continue` and the archive rule leans on, so this was not a cosmetic fault. Two changes, because
there were two faults: `new_id` now ends in the process id, so two runs cannot propose the same name
(`session.started` names the file *before* the first write, so a name that had to move at write time
would be a frame that lied); and taking the name is now a claim — `create_new` is what decides who
writes `meta`, and a taken name moves that writer to the next millisecond instead of merging with a
stranger's conversation, with `meta.id` moving with it. `append`, `title`, `schema` and `switched` are
`&mut self` for that second half. The Rust test was watched red first
(`a_writer_that_finds_its_name_taken_takes_another`, which forces the collision rather than waiting
for one); the integration proof is the Python batch check, which failed with
*"and each one is its own conversation"* — six turns, two files — and passes now. Also learned and
written into `AGENTS.md`: `cargo test --lib` does not rebuild `target/debug/flint.exe`, so a Python
check run without `cargo build` first tests the previous binary and its bugs.

**The Python caller is a batch runner as well as a caller — `ROADMAP.md` §10 step 7 is done.**
`ask()`/`Chat.ask()`/`ask_json()` take `attach=`, `paths=`, `inline=` and `require_read=`, and the
three ways of putting a file in a prompt are three arguments because they make three different
promises: `attach=[path]` is a promise (it travels as a real `@` name, and `ask` raises `NotAttached`
if `turn.started`'s `attachments` does not name it), `paths=[path]` is a hope (the names go into the
prompt as prose and the model decides), `inline=[text]` is a promise by construction (the text *is*
the prompt). `require_read=[path]` checks the other direction — what the run *did* — from the `read`
tool's `tool.args` frames, resolved against the working directory, and raises `NotRead`; a file read
through `bash` deliberately does not count, because a command line containing a path is not evidence of
a read. Both refusals happen *after* the run and carry the `Turn`, and `Chat` records that turn and
pins the session before re-raising, so a refused promise is not a lost answer. A call whose command
line would exceed 30000 characters is refused by the module itself (measured first: a 33k prompt dies
in `CreateProcess` as `FileNotFoundError [WinError 206]`, naming no argument), and the message points
at `attach=`, which is the way through — flint reads the file and the argument stays four characters.
The checks needed the stub to see more: it now logs every request body (`FLINT_STUB_LOG`), which is the
only place `attach=`/`paths=`/`inline=` are distinguishable, and it answers a prompt containing
`[[read: <path>]]` with a real `read` tool call, so `require_read`'s positive case is a run that read
something rather than a hand-built event. Five sections of `test_call.py` (20 checks, 75 → 95), and
each new promise was mutation-checked: making `verify_read` or `verify_attached` a no-op, dropping the
names from `paths=`, and attaching the file's path instead of its `@` name each fail the check that
owns them.

**`map_calls` is the other half of step 7, and step 7 is done.** One run per item, results in the order
asked for, jobs taken by whichever thread is free (`workers=`, default 4, 1–8, refused before anything
is spent). The roadmap's sketch said "one session per worker" and the build rejected it: which calls
shared a conversation would then depend on the thread schedule, so it is **one session per call** — and
that decision is what found the real fault above, because six concurrent runs turned out to propose the
same session id. Inside a batch a refused promise is `turn.refused` rather than an exception (one
refused job out of twenty must not throw away the other nineteen answers), `on_turn=(index, turn)` is
called from the worker thread as each settles, and a failed run is not an error. The breaker is on by
default: the first turn whose `error_code` is in `stop_on=("insufficient_balance",)` cancels what has
not started, **waits** for what is in flight rather than killing it, and raises `OutOfBalance` carrying
the turns that did finish, `cancelled` (what was never spent) and the turn that found the empty account;
`stop_on=()` turns it off. Two measurements back the parallelism rather than asserting it: six jobs
against a one-second stub delay on six workers finish in about one call's time, with the stub's own
`max_in_flight` above three, and twenty jobs on two workers with one empty account cancel more than ten.
The stub grew the two markers that make those measurable (`[[balance]]` → a real `402`, `delay=<secs>`)
and a `/stats` route. `python examples/python/test_call.py` is now **117 checks, all passing**.

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
in `src/tools.rs`, and the stream test's budget shape in `tests/json_output.rs`.

**The handle itself is built, and building it moved three things out of the tool call.** `task` now
takes `background: true` and returns at once with the child's pid and the conversation it is holding;
`job_op` takes `action: "status" | "wait" | "stop"` and a `pid`, and answers in the same shape a
foreground `task` does. It is **one** tool rather than the three the sketch named, because every schema
is re-sent with every request and three verbs about one child do not deserve three schemas — and it is
not folded into `task`, because a call that sometimes returns an answer and sometimes a receipt is one
whose result a model has to guess at. The registry that the dropped-turn sentence already needed
(`CHILDREN`) became the handle: it holds the child's stdin, the frames its reader absorbs, the session
it named, and the exit status once it ends. Three consequences, each of which was a decision:

- **The timeout moved off the waiter.** A background child has nobody waiting, so a `timeout_secs` that
  only the waiter enforced was a budget that never came due. Every child now gets a detached
  supervisor: it waits for the exit, writes `/stop` when the budget runs out, kills twenty seconds later
  if that was ignored, and records the status for whoever asks — later, or never. A foreground `task`
  became only a wait on a job that is already being watched.
- **The child's stdin outlives the tool call.** It is kept in the job, because `/stop` is a line written
  to a stdin the *parent* holds and by the time a model asks a child to stop, the future that spawned it
  is a returned tool result. A background child therefore no longer sees its stdin close as the call
  returns, which is what makes the stop possible at all.
- **The handle is not the presence record**, which is what the plan sketched. A record can say a run is
  alive; it cannot carry a pipe. So presence is for *seeing*: asked about a pid this run did not start,
  `job_op status` reads the records and reports directory, endpoint, `readonly` and session, and says
  plainly that it can be seen and not waited for or stopped. What `flint who` gained from the previous
  commit — `Presence.session` — is exactly the field that answer needs.

Three tests in `tests/task.rs`, all watched red first (with `background` temporarily ignored, all three
fail on the assertion that owns them): a background child whose answer lands in its own session file
after the parent has exited, a `job_op` status-then-wait pair where the scripted model reads the pid
out of the tool result it was given, and a stop that ends a child stuck in a model call with exit 130 —
a stop, not a kill, so the half-answer it had drawn survives. That last one needed a new stub shape: a
scripted model that answers **by conversation** rather than by request number, because a background
child's requests race the parent's and "the parent's turn is request 3" is no longer true.

**Measured while writing those tests, and now written down in `docs/agents.md`:** the parent process
was done in 0.4 s and the caller's read of its stdout hit EOF at 8.4 s, when the child finished. That is
not a bug and not a Windows one — a spawned process inherits the standard output its parent was given,
so anything that reads a run's stdout to EOF is waiting for the whole process tree rather than for the
run. flint's own `bash` tool has always worked this way. The tests therefore measure the parent's own
exit rather than the harness's patience (`run_flint_until_exit` in `tests/task.rs`), and a caller that
wants to come back with the run it started should stop at the terminal frame rather than at EOF.

**`turn.completed` now carries `duration_ms`, which is the last thing §10's own table was missing
except the money.** The clock starts where `turn.started` is written and stops at the frame that ends
the turn, so the number is the caller's wait rather than the process's lifetime: timing the subprocess
would include flint's start-up and the caller's own reading, and a caller streaming the answer has no
end to time at all. It is on every ending — `stopped` and `incomplete` included, which is exactly when
a caller asks — and it is per turn, so a schema repair attempt and a turn a steering line started each
report their own. The test asserts it in both directions rather than merely present: a stub held for
1200 ms must be reported as at least 1100, and the frame may not claim more milliseconds than the run
the harness just measured (`a_turn_says_how_long_it_took` in `tests/json_output.rs`, watched red first
— *"turn.completed carries no milliseconds"*). `Turn.duration_ms` in `examples/python/flint_call.py` is
the same field for the Python caller, checked in the batch section of `test_call.py` where the stub
already holds each call a second (`[1007, 1007, 1006, 1007, 1008, 1006]`), which is 118 checks now.
The money half of that row is **refused rather than pending**, and `ROADMAP.md` records why: flint talks
to whatever OpenAI-compatible endpoint it is pointed at, providers disagree about cached-prompt pricing,
and a price table kept in this repository would go stale into a wrong number — worse than no number. A
caller with its own tariff has the token counts on the same line.

**And a child's conversation no longer joins the person's list of conversations — the second report of
that same evening.** "子代理的会话窗口居然可以被会话历史栏看到": a `task` child's session file was in
`/sessions`, in `flint --list-sessions` and in the page's sidebar exactly like a conversation the person
had — same directory, same shape, nothing to tell them apart — and the sharp edge was `--continue`,
because a child is *newer* than the parent that started it, so "the newest conversation in this
directory" answered with the child's. One fact fixes both: the tool sets `FLINT_PARENT` on the child (an
environment variable, for `FLINT_DEPTH`'s reason — the model driving the call does not choose or omit
where its child came from), and `main` uses it twice. The child's `meta` line gains `parent` (absent,
not `null`, on an ordinary session, so the first line of one is byte for byte what it was), and the file
is written under `sessions/<dir>/children/`, which **no listing reads** — the same argument the archive
makes, so `/sessions`, `--list-sessions`, the sidebar and `--continue` agree without a filter to drift.
The child's conversation is unchanged in every other way: same format, resumable by path, named in the
result, and `mv` out of `children/` is how a person adopts one. Three tests, each watched red first:
`a_childs_conversation_is_not_in_the_persons_list_of_conversations` in `tests/task.rs` (a real parent and
child, then the child's path, its `meta`, and `--list-sessions --json` asserting **one** row),
`a_childs_session_is_kept_out_of_the_listing_by_where_it_lives` in `src/session.rs` (the layout rule and
`latest_for`, which is `--continue`), and one more fixture in `src/web.rs`'s sidebar test (the reported
surface).

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
else, so a model cannot edit it out of its own command line). **The presence record now names the
conversation a run is holding**, which was the other open half: `Presence.session` carries the session
*path* (a path rather than an id, for the same reason `--list-sessions --json` does — the id does not say
which directory keyed the file or whether it sits in `children/`), it is written the moment the writer
exists and follows `/new` and `/resume` because the page is told at the same place and for the same
reason, and `flint who` prints the id on the human line and the path in `--json`. A record written by an
older build or by hand still reads (`#[serde(default)]`), and reports "session": null rather than an
empty name; the reader keeps the older "newest session file written here" line as well, because it covers
a record that has said nothing yet. **Built since: the background handle** — `background: true` on
`task`, `bash`, `exec` and `pwsh`, with `job_op`'s `status`/`output`/`wait`/`stop` (the three verbs
this paragraph sketched as `task_status`/`task_wait`/`task_stop` became one tool with an `action`) —
so the record is collectable from a model's side as well as readable from a person's.

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
provider received is free of it**. **Built since**: the opt-in that feeds a peer's words to a model
(`--hear-peers`, or `/hear-peers on` while a run is open) and the `.flint/` presence marker in the
project, both bound the way `docs/agents.md` decisions 3 and 5 say — the relay lands in the request
view and never in `history`, and the marker is only ever looked for, never created. **The `/say` this
paragraph called not built is built** — `/say <text>` from the prompt (or the page's form, which sends
the terminal's own line) and `/say --to <pid>` for one run; see the top of this file for the two things
it added beyond the shared write (it names who is here, and the writer is not shown its own line).
One bug worth
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

**The installed binary matches the tree as of 2026-09-17 15:24, and updating it while flint is running
needs one extra step.** `flint` on this machine's PATH resolves to `C:\Users\zhangzhuo\bin\flint.exe`,
which is a copy of `target\release\flint.exe`; the page is `include_str!`-embedded, so a page change does
not reach an existing binary and the copy is what makes a rebuild visible. `Copy-Item -Force` over a
*binary that is running* is refused by Windows ("being used by another process"), which the last sessions
recorded as "close every flint window first". It does not have to be: Windows refuses to **overwrite** a
mapped image but allows it to be **renamed**, so the update is `Rename-Item flint.exe
flint.exe.bak-<stamp>` and then copy the new file into the freed name — done that way this round, with
two flint processes running (`2668` and `37632`, both started from this path). Those two keep executing
the old bytes out of `flint.exe.bak-20260917-152423` until they exit, so that file cannot be deleted
before then; the build before it (`flint.exe.bak-20260917-143312`) is still there as the older copy.

**One CI run was red, and it is worth reading because the guard was right.** `a4071ca` — the jobs commit
itself — failed `ci` on both platforms with nothing but the mojibake whitelist:
`docs/web-mode.md:1067` quoted the request that §13 was built from, and a file that was never declared to
hold CJK is reported rather than assumed. The fix is one line in `CJK_FILES` (`682314f`, pushed
immediately after), which is why the pair is in the log in that order; `release` on the first commit was
still building when the second landed.

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
cargo build                                       # the Python and browser checks run this binary
cargo test                                        # 600 passing, 1 ignored
cargo clippy --all-targets                        # silent, and worth keeping that way
node scripts/term-layout-test.js                  # 全部通过
node scripts/web-view-test.js                     # all passed
node scripts/browser-controls-test.js             # 56/56 -- needs a browser, so it is not in CI
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

**`/resume` drew nothing, and the fix is one call in the right place.** Reported from a real session:
going back to an older conversation inside a running flint left the screen clean apart from the one
`resumed: <file> (N messages)` line. `--resume` at startup had printed the conversation for a long time,
so the run had two readings of the same act: opening a conversation, and *showing* it. The page had both
too (`Viewer::follow` re-reads the file), and only the terminal's mid-run door had one. A test watched
red first — `/resume` in a real REPL, with a fixture whose words appear nowhere else — then the same
`print_transcript` the startup path calls. The subtlety the fix is arranged around is that
`splice_loaded_history` *moves* the loaded messages, so the count and the drawing both have to happen
before it. The section is below.

**`--no-session`: a run that keeps no conversation, and the door it would have left open.** The second
of the twelve items taken from the Pi reading, its own commit, and the shape of it is the doors rather
than the file — `--continue`/`--resume`/`--fork`/`--name` refused at the command line before the config
is even loaded, `/new` and `/resume` refused inside the run through one function so the page's sidebar
rows get the same sentence a typed line does, and the switch path (which creates the session when the
old one has no file) told once and carried on the agent. The children inherit it through `task_argv`,
which matters more than it sounds: with no conversation id to hand down, a child's session would have
been written *outside* `children/` and shown to the person as one of their own. Four tests in
`tests/cli_output.rs` and `src/tools.rs`, two more in `tests/task.rs`; three mutation checks, each
watched failing with the branch disabled and restored byte-for-byte. The section is below.

**And the failure that was not a compile error.** The release matrix's `aarch64-unknown-linux-musl` job
went red on the commit before that and never ran cargo: its zig install step died after one second while
the sibling x86_64 job spent fourteen minutes downloading the same version successfully. Read off the
step timings through the actions API rather than guessed at, fixed by moving the step to the maintained
action (`adcde74`) — and the run that carried the fix brought that step down to ten seconds for arm64
and forty-five for x86_64, four green targets. `## Pushing from this machine` records how to read a red
job without a log.

**The round before this one, recorded here so it is not re-done: an interrupt no longer throws away the
answer it was drawing, the commands that rebuild the agent
no longer throw the conversation away, a stopped turn now ends on the page as well as in the
terminal, `/readonly` sets the guard it says it sets, `/verbose off` outlives the run, the page
can change the model or the provider from its own header, a command typed into the page's composer
answers there — because a command that failed no longer ends the run — the page is told what
commands the run has, from the same table `/help` prints, and the one class of those commands that
takes no argument is a button in the header.** That was that round and the four before it: ten items
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

**Measured since, in a browser** — this paragraph used to say the opposite, and the correction is worth
keeping as a record of what "pinned" does not cover. They are pinned as behaviour
(`applyState`, `fillSelect`, `showToggles`, `showCommands` and `showActions` over the stub DOM in
`scripts/web-view-test.js`) and as bytes (`the_pickers_offer_the_runs_own_commands`,
`the_toggles_are_switches_that_show_their_value`, `the_command_panel_is_drawn_from_the_frame`,
`the_action_buttons_send_the_frames_own_line`), and
the frame end to end (`the_page_is_told_the_state_its_controls_would_show`, which reads `/events` on
connect, sends `/model stub-other` and `/verbose full` to `POST /message`, and then opens a *second*
stream, which can only have been handed the snapshot). And since 2026-09-17 the header has been looked
at with a real font and driven with real events: a switch moves the control and the run together, a
picker moves from the keyboard and the run is told which model, a command's answer lands in the panel
and not in the terminal, the panel is opened and read with a real click, and the send button is
reachable under it — `docs/web-mode.md` §11 is the table.

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

**Measured in a browser since 2026-09-17**: a real click on `/reload`, one of the run's two action
buttons — the run printed `reloaded` in the terminal, which is the witness that the press reached it —
with the buttons themselves read out of the header first. `/new` was not pressed: it would start a fresh
conversation mid-pass and the rest of the checks are about the one that is open. The click path is
pinned as bytes and the answer path end to end as well, and that is the three ways it is held.

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

**Not measured**: a report asked for *while a turn runs*, which needs a stub turn slow enough to click
during — a report asked for between turns **is** measured in a browser now: the `/config` row was
pressed with a real click, the answer was read in the panel, and the terminal gained not one byte.

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
another. Both are the next round's question — and the masked box half is **measured in a browser since
2026-09-17**: the `/provider key` field is `type=password`, a key typed into it reaches the run
(`key saved`), the field is emptied, the secret is nowhere in the page's markup, and `config.toml`
really did receive it, which is what stops the other three claims passing on a command that never ran.

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

**Measured in a browser since 2026-09-17**: the menu is opened by a real click on the row's `⋯`, and
what it sends is checked against the run and against the disk — the open conversation's field renames
it (`named: named from the sidebar`), and a fixture conversation's `/delete <n>` row removes that
conversation's file while nothing at all was sent by the press that opened the menu. The *placement* is
therefore seen; what is still reasoned rather than seen is the dismissal (the button toggles it, a
`reset` clears it, and a click elsewhere does **not** close it). That dismissal is the thing worth
watching first when someone finally looks at this page with a mouse.

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

**Measured in a browser since 2026-09-17**: a real click opens a `/delete <n|id>` row's choices, the
run's stdout gains nothing, and backing out leaves the sessions directory byte-identical. The second
press in *that* list was not made, and deliberately: it would delete a real conversation. It is made
from the sidebar's own menu, on a conversation the harness wrote itself, where the deletion is a fair
thing to ask a browser to do — and it is checked against the directory listing, which is the same
witness one level out. What the page adds is the two presses, and what stops a single
press is that there is nothing to press that sends.

### A path in the transcript opens the file, and the panel is a column

Asked for in two sentences: *"现在看不到子代理和后台任务的情况，在web页面上面"* and *"他对话里显示的文件真实地址，没做超链接，不能直接点开文件，有点麻烦"*. This one is the second half, and it is one commit: `feat: a path in the transcript opens in the page`.

**The route.** `GET /file?path=…` (`serve_file` in `src/web.rs`), added beside `/sessions` in the same literal table. It takes the string the transcript wrote — `?path=` is a *parameter*, so the route table keeps its shape and §6's "nothing is served from disk" stays true in the form that matters: nothing is served that a tool result already in the transcript did not name. Three decisions inside it: a relative path resolves against **the run's working directory** (`Viewer::asked` gained `cwd`, fed from `agent.cwd()`, because `--cwd` moves what every relative path in a conversation means); a trailing `:line` or `:line:column` is stripped and answered in an `X-Flint-Line` header rather than being treated as part of the name (`split_line` — and the *first* thing tried is the literal path, so a file genuinely called `a:1` still opens); and a file longer than 512 KB is **cut, not refused** — its own bytes, with `X-Flint-Cut` and `X-Flint-Size`, because half a 40 MB log honestly labelled is what a reader wants. What *is* refused, each with its own sentence: not valid UTF-8 (no lossy conversion — a page of replacement characters is worse than "not text this page can show"), a directory, and over 64 MB.

**The page.** The path splitter (`pathParts`/`asPath`) is a pure function in the model half, and the rule is written down rather than hidden in a regex: a candidate has no whitespace, quote, bracket or comma in it; it is a path if it is absolute, or its last segment has an extension of two to eight characters, or it has three or more segments. `e.g.`/`i.e.` are why a one-character extension is not one (`foo.c` is the price); `and/or`, `read/write` and `24/7` are why one separator and no extension is not one (`src/bin` is the price); `//` means a URL; `4/2` and `2024/09/17` are ratios and dates. **Only tool blocks are split** — prose is where those abbreviations live. The panel shows the file's bytes in a `<pre>`, the directories dim and the filename in full ink, the size, a `line N` when the transcript named one, `reload` and `close`, and Escape from anywhere.

**Two things the browser settled, and both were changes to the design rather than to the code.** The panel was `position: fixed` over the right edge; with it open, `elementFromPoint` at the next path's centre answered `pre#preview-text`, so the second path in a turn could not be pressed at all. It is now a fourth grid column (`#preview` inside `#app`, `grid-column: 4`, `.app.plain #preview { grid-column: 2 }`), which narrows the transcript instead of covering it — the same shape as DSH's sidebar preview, which is where the feature came from. And the Node checks caught `segments.slice(1).every(…)` being `true` for a name with no separator at all, which swallowed every bare filename (`Cargo.toml`) until it was in the test.

**Measured.** `cargo test` (the new `web::tests` cases: the file the transcript names, a `:line`, a missing file, a directory, a binary, a cut file counted on a character boundary, and the token still required), the page's policy tests in `tests/web_view.rs` (`a_path_opens_in_this_page_or_not_at_all` forbids `window.open` and `target="_blank"`, and pins the percent-encoding of the route), three checks in `scripts/web-view-test.js`, and ten claims in the browser harness — 44/44 at the time, against a run whose turns are scripted by a stub model the harness now starts itself; the jobs panel has since taken it to 53. Mutation-checked on the route: with `cwd.join(path)` replaced by `path.to_path_buf()` and `split_line` forced to `None`, exactly four tests fail.

### The run's background work is in the header, and a frame says it changed

The other half of the same request: *"现在看不到子代理和后台任务的情况，在 web 页面上面，你可以参考 dsh加上功能"*. One commit: `feat: the page shows the run's jobs`.

**What was invisible, and why it mattered.** A `task` child and a `bash`/`pwsh`/`exec` command started with `background: true` are one record in `src/tools.rs` (`Job`, in `CHILDREN`), with a handle and three verbs — and the page had no way to see any of it. The terminal says one line when a job ends and `job_op` answers whoever asks; in a browser there was nothing at all, so a build started and forgotten, or a child left running, could only be found by asking the model to interrupt itself.

**The route, and the one decision inside it.** `GET /jobs` answers `tools::jobs_snapshot()` — read off the same records `job_op` reads, so the page and the tool cannot describe one job two ways — in the run's own listing order (running first, then newest first). A row carries `pid`, `kind`, the label, the status word, the exit code in a sentence, and `started_secs`/`ended_secs`. Three of those fields exist because the reader is a page and not a model. The times are **absolute epoch seconds**, derived at read time from the job's `Instant` rather than stored, so a page open for an hour is still right about a job it heard about when it opened, and the ticking is the page's own arithmetic rather than a request; a status word is **the exit code read as a person reads it** (`running`/`completed`/`killed`/`failed`, `-1` being its own word, because a kill and a failure look identical from outside and "failed" sends somebody looking for a bug that is not there); and `path` is what makes a row a door — a command's log or a child's conversation, opened through the same `GET /file` §12 built.

**The change signal is a revision, not the list.** `tools::jobs_revision()` is one process-global `AtomicU64`, bumped when a job is registered and when one settles, and `web.rs` sends an empty `event: jobs` when it differs. Three reasons, each the reason for the next: the *list* is the route (a frame carrying it would be a second answer that can disagree with `GET /jobs`), a *counter* rather than a flag (a reader that missed one change still sees the next, and a boolean cleared by two readers can lose one), and **the tool code grows no way to reach a connection** — nothing in `tools.rs` knows a listener exists, which is why this works for `--web` and `/web` alike with nothing threaded through `main.rs`. It is compared on connect, after every frame the connection forwards (so a job started by a tool call appears during the turn that started it) and on a four-second timer, which exists for the one change with no traffic to ride on: a background command ending while the run is idle.

**The page.** DSH's job popover is the shape, and the two things refused from it are stated in `docs/web-mode.md` §13: the live tail inside the list (the preview column already reads a job's output) and a `stopping` status (nothing on the `Job` records a stop until it has ended, so the word would be a claim the run cannot back). The panel is a `<details>` in the header, present only when there is a job; the summary reads `jobs (2) · 1 running`; each row is a dot coloured by status, the kind, the label, the detail and the ticking `running for 3s` / `took 4s`; a row with a `path` is a button that closes the list and opens the preview; Escape closes the list, and the clock gets cleared with it.

**Measured, and what each layer caught.** `tools::tests` gained two cases (the status word from an exit code, and a real background command snapshotted while it runs and again after `job_op wait`) — and the second one caught a **design mistake**: the first version also sent a `tool` field read from `job.label`, which for a command *is* the command line, so every command row would have carried the same string twice. The field was removed rather than filled in. `tests/cli_output::a_background_command_is_a_job_the_page_can_watch_end` drives the real binary with a stub model that backgrounds a command, watches `/events` for two `event: jobs` frames and polls `/jobs` until it settles, then reads the log's own bytes; with the settle-time `jobs_changed()` removed it fails with "the end of the job was never announced". `tests/web_view.rs` gained `the_jobs_panel_reads_the_route_and_a_row_stays_in_this_page` (the route with the token in a header, the frame as a re-read, the row's press going to `openPreview`, no `fetch` in the tick). `scripts/web-view-test.js` holds the route's shape and the two words with every duration boundary — mutation-checked on the minute boundary. And `scripts/browser-controls-test.js` went from 44 claims to **53**, driving a scripted turn that starts a fifteen-second command *and* a `task` child: the count, the running row's kind and label, the duration **ticking** with nothing fetched, the settled row's `exit code 0` and `took Ns`, the child's row opening the child's own conversation, the command's row opening its log (first the line printed while it ran, then both lines after it settled), and Escape.

**Two claims were written wrong, and both are worth keeping because each looked like a page bug.** One looked for *any* settled row, and the child settles seconds before the command does — so the assertion "the job ended" was true while the log it then read was still half-written. The other marked a row with an `id` and never cleared it, so the second press found the *first* row by `querySelector` and re-opened the command's log when the claim was about the child. A harness that asserts on a list has to say which row it means, in both directions.

### Stopping one: `/jobs`, `/jobs stop <pid>`, and a kill that read as a failure

The second half of the same complaint, and its own commit: `feat: a person can stop a job`. Seeing the run's work was half of it; the other half is that until this landed the only door onto stopping a job was `job_op`'s `stop`, which is a tool. A person who started a ten-minute build by accident had to ask the model to stop it, or kill flint and take the child with it — which is exactly the shape `docs/agents.md` records as the reason the handle exists at all.

**Two commands, and one answer for three readers.** `/jobs` prints `tools::jobs_report(None)` line by line; `job_op status` returns it; the page's panel rows are the same record. `/jobs stop <pid>` goes through `tools::stop_job(pid, None)`, which is the function `job_op`'s `stop` arm now calls — the `status`/`stop` bodies were factored out of the tool so that a person and a model cannot come to disagree about what is running or about what happened when somebody stopped it. The two doors differ in exactly one place, and it is not an oversight: `job_op stop` with no pid acts on the only job in play (a model that has just started one is unambiguous), while `/jobs stop` with no pid is **refused** with a sentence pointing at the listing, because a person typing a kill is naming what to kill and a wrong guess is irreversible. A non-numeric pid and a third word are refused the same way. `ArgFrom::Jobs` is the third candidate list, `word()` reads `"jobs"`, and the page resolves it to `listedJobs.filter(jobIsLive)` — the pids already on screen, so nothing new had to be sent for the feature.

**The defect this found is the part to remember: a kill's exit status is the shell's, not the command's.** The new e2e ran red with `{"detail":"exit code 1 (failed, cause not classified)","status":"failed"}` where the claim was `killed` — Windows `taskkill /PID … /T /F` leaves the `cmd.exe` it signalled reporting **1**, so a job a person stopped deliberately was listed as `failed`, the one word that sends somebody looking for a bug that is not there. Unix reports `-1` (a signal) and had read correctly all along, which is exactly the platform difference a status word must not depend on. `Job` gained `ended_by_us`, set in `kill()` **before** the signal (the supervisor records the exit status as soon as it has one, so a flag set afterwards would sometimes lose the race it exists to win) and in the budget-expiry branch, and the supervisor consults it when it records the status — so `killed` now means *this run ended it*, on both platforms for the same reason. The budget-expiry branch was rewritten with it: it used to read `child.wait()`'s code, which was the number belonging to the shell it had just killed.

**Measured.** `tools::tests` gained `a_job_a_person_stops_reads_as_stopped_and_says_what_the_model_would_be_told`, which starts a real background command, stops it, and asserts the sentence, the row's `status == "killed"` and `exit code -1` in its detail, **and** that the tool's answer is the terminal's answer to the byte (`via_tool.trim() == jobs_report(Some(pid)).trim()`). `tests/cli_output::a_person_can_read_the_run_s_jobs_and_stop_one` reads the two commands out of the opening `state` frame (so a command that stopped being page-reachable fails here rather than in a browser), reports `/jobs` and asserts the panel names the pid, then stops it and polls until the status is no longer `running`. `tests/web_view.rs` gained `the_page_stops_a_job_from_the_rows_it_is_already_showing`. The browser harness went from 53 claims to **56**: the stop row's candidates are the panel's own rows, pressing one leaves a row the page classifies as `killed` with `exit code -1`, and the transcript then says "killed it, and it is gone" — against a second background command the harness now starts (an endless `node -e`) so that the thing being stopped is not work any other claim is waiting on. Mutation-checked twice: the page's `command.from === "jobs"` branch changed to `"jobs-nope"` makes the harness time out waiting for the stop row, and `was_ended_here()` forced to `false` makes the lib test fail with `left: String("failed") right: "killed"`.

**And one census assertion had to move, deliberately.** `a_destructive_row_says_where_its_argument_comes_from` counts the frame's `from` marks and asserted exactly three; `/jobs stop <pid>` is the fourth. It was changed to 4 with the reason written beside it rather than loosened to `>=`, so a fifth mark added without thinking still fails there.

### A command knows what run it is in

The first of the twelve items taken from the Pi reading, and its own commit: `feat: a command knows what
run it is in`. The sentence that opened it is in `ROADMAP.md` and in `docs/pi-agent-harness.md` §3.9: a
`task` child is told `FLINT_DEPTH` and `FLINT_PARENT`, and a `bash` command was told the proxy variables
and nothing else — so a script the model writes could not name the conversation it belonged to, read the
log of a job that run had started, or ask the same endpoint a second question. Pi's `bash` tool hands
over `PI_SESSION_ID`, `PI_SESSION_FILE`, `PI_PROVIDER` and `PI_MODEL` for exactly this reason.

**The names, and the one flint does not have.** `FLINT_SESSION` (the conversation's file, absolute — it
is enough to find everything else a run keeps, since the spill files and a job's log are named from it),
`FLINT_PROVIDER` and `FLINT_MODEL`. There is no `FLINT_SESSION_ID`: flint's sessions have no per-entry
ids, so the path *is* the name, and inventing an id here would have been a second name for the same
thing. A `task` child still gets `FLINT_DEPTH` and `FLINT_PARENT`; a command gets neither, because a
command is not a run and `docs/agents.md` says so.

**Where it lives.** `RunEnv` (`src/tools.rs`) is what a command is told, filled in by
`ToolBox::with_run_env` from `Agent::new` — the same order and the same reason as `with_task_endpoint`:
the tool set is built before the run's provider is resolved, and the conversation's path is known to
`Agent::new` and not to `ToolBox::new`. `ToolBox` keeps its own copy as well as pushing it into the
tools, because the REPL's `!cmd` runs through `run_command_raw` rather than through a tool, and a person
typing a command into a conversation must not be told a different run than the model's `bash` is. Both
spawn sites (`run_program_streaming` and `start_background_command`) now call one `apply_child_env`,
which is where the six-line proxy block had already been copied — a fact that reaches a foreground
command but not a background one is a fact a script cannot rely on. `start_background_command` is at
eight arguments and carries an `#[allow(clippy::too_many_arguments)]` with the reason beside it, which is
what `main.rs` does for the REPL for the same reason.

**The decision the first version got wrong, and it is the one to remember: "no session" must mean
*removed*, not merely unset.** A command inherits the environment of the process that spawned it, and
that process may itself have been started by another run's command — a model running `flint -p ...`
through `bash`, or `flint exec` — so an unset name and an inherited stale one are not the same answer
from inside the command. The first cut skipped an empty name; the new e2e that plants a stale
`FLINT_PROVIDER` in flint's own environment caught the stale value arriving (`[%FLINT_SESSION%]
[another-run]`, with the session correctly removed and the provider not), and every name flint owns is
now either written or taken away. `flint exec` is not a conversation, so it takes all three away.

**Measured, five tests, and each is a different layer.** In `src/tools.rs`:
`the_run_is_written_onto_the_command_and_a_missing_session_is_removed` asserts on
`Command::get_envs`, with each name set to a stale value **first**, because "removed" and "never
mentioned" read identically from outside a running child and only one of them is right; and
`a_command_the_bash_tool_runs_can_read_the_run_it_is_in` runs a real shell through the `bash` tool and
reads the three values back, which is the half the first test cannot see (that `with_run_env` reaches
the tool and the tool reaches the spawn). In `tests/cli_output.rs`:
`a_command_the_model_runs_is_told_which_run_it_is_in` drives the real binary with a stub model whose
first response is a `bash` call that writes the three variables into a file, and compares them against
the session file the run actually created; `a_command_the_person_types_inside_a_run_is_told_the_same_run`
does the same for a `!cmd` typed into a real REPL (no model needed — the escape is the person's) and
needs `/name` first only because that is what creates the session file; and
`exec_does_not_pass_on_a_session_name_it_inherited` runs `flint exec` with the names planted in its
environment, which is the removal rule. **Watched red in both halves**: with `run_env.apply(cmd)`
removed, three of them fail with `"%FLINT_SESSION%\r\n%FLINT_PROVIDER%\r\n%FLINT_MODEL%\r\n"` where the
values should be, and separately with the `Agent::new` call removed the e2e fails the same way — so the
wiring is covered and not just the object.

### `--no-session`: a run that keeps no conversation

The second of the twelve items taken from the Pi reading, and its own commit. The sentence that opened
it is in `ROADMAP.md` and in `docs/pi-agent-harness.md` §3.4: Pi has `--no-session` (ephemeral: never
save), flint's only no-file case was incidental — a session file is created by the first event in it, so
a run that says nothing leaves nothing — and a flag whose promise depends on saying nothing is not a
promise.

**The file is the small half. The doors are the whole of it.** Every way a conversation can be opened or
named had to be closed, and each one is a different mechanism, which is why the round is 150 lines of
`src/main.rs` for four lines of "write nothing":

- **The command line refuses the contradiction before anything else happens.** `--continue`, `--resume`,
  `--fork` and `--name` each say "the conversation this run is in", and `--no-session` says there is
  none: one sentence, one `usage()`, exit 2. It is checked with the other command-line refusals and
  *before* the config is loaded, so a caller that typo'd its way into a contradiction does not get a
  config file created on the way to being told so.
- **`/new` and `/resume` refuse from inside the run**, both through one `keeps_no_conversation(cmd,
  printer)` so the page's sidebar rows — which go through the same dispatcher as a typed line — get the
  same sentence, and so a third door added later inherits it. `/reload` is deliberately *allowed*: it
  re-reads config and rebuilds the agent, which is not opening a conversation, and the test holds that
  distinction as a count (two refusals, not three) plus the file count staying at one.
- **The switch path is the one nobody would have thought of.** `continue_conversation` is the single
  funnel `/model`, `/provider`, `/reload` and the page's switch rows all go through, and it *creates* a
  session when the old one has no file. So a `--no-session` run that switched provider would have
  written the file the flag exists to prevent. The flag is read off the old agent (`Agent::no_session`)
  and set on the new one rather than re-derived — deriving it from `writer.is_none()` is what looks
  right and is wrong, because `SessionWriter::create` only *proposes* a path and the file appears on the
  first append, so "no writer" and "no file yet" are different states.

**What the run still writes, and why it has a name of its own.** Spilled tool output and a background
command's log are not the conversation; they are how a run works at all. They go under
`spill/unattached-<pid>/` — `tools::unattached_spill_dir()` — and the bug that named it came from two
`--no-session` runs sharing `spill/unattached/`: both number their spill files from `1.txt`, so the
second run's first spill overwrote the first run's.

**The part worth reading twice: a `task` child inherits the flag.** A child is a conversation *this* run
asked for, so a parent that promised to write nothing would leave a file behind through the one door it
opened itself. It is worse than untidy, because the exclusion that keeps a child's conversation out of
the person's list is `FLINT_PARENT` — the parent's id, which a session-less parent does not have — so
the child's file would have been written at the top level of `sessions/<dir>/` and listed as one of the
person's own. `TaskConfig.no_session` → `Child.no_session` → `Job.no_session`, and `task_argv` pushes
`--no-session` onto the child's command line beside the endpoint. That last field is also what the three
renderings needed: "its session is not named yet" is an answer that stays wrong forever about a child
that will never name one, so the handle, the `job_op status` line and the result a `wait` hands over all
say "none" and say why.

**Measured, six tests, and the interesting ones are end to end.** `src/tools.rs`:
`a_run_with_no_conversation_spills_into_a_directory_of_its_own` (the directory is named from the pid and
lives under `spill/`), and `the_child_command_line_says_exactly_what_the_child_should_be` gained the
`--no-session` argument and the assertion that a bare child does *not* get it. `tests/cli_output.rs`: a
`-p --json --no-session` run asserting exit 0, `"nothing kept"`, `frame["session"].is_null()` and **no
`.jsonl` anywhere under the home**; the same run refusing all five flags with exit 2 and a message that
names the flag; and — `#[cfg(debug_assertions)]`, through a real REPL at `80x24` — `/reload` allowed,
`/new` and `/resume` refused in the same words, counted as exactly two. `tests/task.rs`:
`a_run_that_keeps_no_conversation_starts_children_that_keep_none` (a waiting `task`; the child's answer
really arrives, so an empty home means something, and the child's result says
`session: none (--no-session)`) and `a_background_child_of_a_run_that_keeps_nothing_says_so_everywhere`
(the handle, the status line and the collected answer, plus an empty home).

**Three mutation checks, each restored byte-for-byte afterwards.** The writer branch made
`if false && no_session` → the file count test sees two files instead of one (a
`sessions/<dir>/<stamp>.jsonl` appears beside the switch line's). `next.set_no_session(no_session)`
removed from `continue_conversation` → the REPL test counts zero refusals instead of two. The `/new`
refusal removed → zero instead of two. And on the child side, `with_task_no_session(false)` in
`Agent::new` → the task e2e fails on the handle text, and the status line's branch reverted to "not named
yet" → the background test fails on the status line. A seventh check is the reason the REPL test counts
*two*: its first version expected `/reload` to refuse and was wrong, and the fix was the test, not the
code — re-reading config is not opening a conversation.

**Known and deliberate: `flint who` shows such a run with no `session=`.** The presence record gains no
new field for this. Omitting a field is not the claim "not yet" — which is exactly the claim that had to
be fixed in the three child renderings — and a peer's line says the truth by saying nothing. If a second
door onto that distinction appears, the record is where it would go.

### `/resume` moved to a conversation and drew none of it

Reported: *going back to an older conversation leaves the screen clean.* That was exact, and it was
only true of one door. `--continue`, `--resume` and `--fork` have printed the conversation's tail since
sessions became reachable — `print_transcript`, twelve messages, under a line saying how many earlier
ones were left out — and that is why the report read as strange rather than as a missing feature: the
run *had* a reading of "open a conversation" that included showing it. `/resume <n|id>`, the door for
someone already inside a run, had the other half: it printed `resumed: <file> (N messages)` and nothing
else, so a switch that loaded a conversation and a switch that loaded nothing looked identical on
screen. The page has never had this problem, which is what makes it a defect rather than a design:
`Viewer::follow` makes every open browser re-read the file the run moved to, so the browser draws the
conversation the terminal was hiding.

**The fix is the same call, in the one place where the messages still exist.** The `/resume` arm builds
the new agent and hands the loaded history over with `splice_loaded_history` — which *moves* it — so the
count (`count`), the `resumed:` line and the new `print_transcript(&loaded.messages, &printer)` all have
to happen before that call. The arm is now ordered: resolve and load, apply the file's model, build the
provider and the writer and the agent, print the line, draw the conversation, move the history in,
return. Everything fallible is before the first line of output, which is why the drawing sits where it
does rather than immediately after the load.

**Measured red first, in a real REPL.** `a_resumed_conversation_is_drawn_and_not_only_loaded` (in
`tests/cli_output.rs`) builds a fixture session whose two messages say
"the socket question from yesterday" and "the socket answer from yesterday" — words that appear nowhere
else in the run — drives the binary at `100x24` with `FLINT_TERM_CAPTURE=1`, types `/resume 111-1`, asks
one question so the switch is followed by a turn, and asserts on the captured bytes. Before the fix it
failed on the first assertion with the capture in the message; after it, both lines are there. The
startup path's half was confirmed by hand through the byte stream rather than by reading the code:
`FLINT_TERM_CAPTURE_FILE` plus `scripts/vtscreen.js` shows a sixteen-message conversation drawn as the
last twelve under `── — 4 earlier messages, resumed transcript ──`, which is also what told us the block
was working and only the mid-run door was not.

**One thing checked and left alone: `--json` prints none of it, and that is correct.** The transcript
goes to a person, and a `--json` caller must be able to read every line of stdout as one object; the
same conversation is already in the request the model is sent, so nothing is lost by not printing it.
Verified by running `--resume … -p hello --json` and reading the stream: three objects, no transcript.
A plain one-shot *does* print it, which is the right side of that line — a person reading a terminal
wants it, a program reading a stream does not.

### Prompt files, and a skill a person can invoke

**The third of the twelve items taken from the reading of Pi is built and pushed: a prompt you type
often is a file, and the person can aim a skill.** Both halves are one act rather than two features,
which is what makes the round small: a saved prompt (`/<name>`) and a skill invoked by hand
(`/skill <name> [args]`) become the person's next message through the same `Flow::Send`, composed by
the same `context::fill_args`. Until this existed the `skill` tool's description was the only door out
of the catalog — *call this before following a skill* — so a procedure flint hoped the model would
consult could be read by a person (`/skills <name>`) and never aimed at anything.

**The files.** `<project>/.flint/prompts/<name>.md`, then `<cwd>/.flint/prompts/`, then
`<FLINT_HOME>/prompts/`, first found wins on a name — the skill catalog's three places and its
priority order, one level deep, `.md` only, front matter's `description` (or the body's first line) as
the one line `/prompts` lists, and front matter's `name` able to override the file name. Arguments are
a hole: `{args}` is replaced where the author put it. With no `{args}` the words typed after the name
are appended as a last paragraph, because "save this and aim it at something else" is the whole point
and most templates are written in thirty seconds without thinking about arguments. An empty argument is
not an error in either case, which is the branch that is easy to get wrong: `/name` with nothing after
it sends the file as written.

**The one decision worth arguing is that a template never reaches the model's prompt.** A skill is
offered (one summary line per skill, by design, in every request); a template is not — no catalog line,
no tool schema, nothing, so a directory of long prompts costs a run exactly zero until one is typed.
The price of that is discoverability, and the answer is a command rather than a prompt cost:
`/prompts` lists what was found and where, `/prompts <name>` prints one as it would be sent, and the
unknown-command line now ends `/help for the commands, /prompts for your saved prompts`.

**Typing `/<name>` resolves after every built-in command, and that order is the safety property.**
The lookup is in the dispatcher's last arm, which is only reached by a word no command matched, so a
template called `help` loses to `/help` instead of shadowing it — a person's own files get a namespace
that cannot take the commands they depend on. `/prompt <name> [args]` is the second spelling, and it
exists for one mechanical reason: a page's menu row sends a fixed command plus one value and cannot
type `/<name>`, so `/prompt` is the press a page can make (`/prompts` stays the reading). The pair is
what `docs/web-mode.md`'s new section measures; the four commands are rows in `COMMANDS`, so `/help`
and the page's panel are built from the same table as always.

**The transcript keeps the person's line and the session file keeps the truth.** Both sends return
`Flow::Send { text, note }` — the text is what the turn carries, the note is drawn *under* the echo
(`> /skill tidy-commits the parser`, then `  sent 75 characters from …SKILL.md`) — and the note is in
the variant rather than printed by the command because a command runs before the echo and the two have
to be drawn in that order. Echoing three paragraphs of template back at somebody who just saved it
makes every invocation unreadable; echoing the typed line and naming the file once answers both "was
that understood" and "which of the two files by that name won". The session file and the request carry
the expanded text, which is what a resumed conversation is rebuilt from.

**Red first, in four places.** `a_prompt_file_is_sent_when_its_name_is_typed` (the request body holds
the filled template, the echo holds the typed line, and the expansion is *not* printed) and
`a_skill_can_be_invoked_by_the_person` were watched failing with "no request was made, so nothing typed
at the prompt reached the model" — which is exactly what an unknown command does; `prompt_files_are_listed_with_their_descriptions`
failed on `/prompts` drawing nothing. The page test was made red on purpose by emptying the frame's
values (it failed on exactly that fragment) and then restored, and the two unit tests in `src/context.rs`
— precedence, and the `{args}`/append rule — were made red by mutating `prompt_dirs_for`'s order and
`fill_args`'s substitution, each failing on its own assertion. `tests/cli_output.rs` gained a
`typed_at_a_repl` helper for the two REPL cases: a scratch `FLINT_HOME`, one line on stdin, the pipe
closed rather than `/exit` typed (an `/exit` arriving mid-turn now interrupts the turn whose request
the test reads).

**Checked by hand on a real screen, and one thing it caught that no assertion would have.** The
capture of a run typing `/prompts`, `/tidy-commits src/parser.rs`, `/skill tidy-commits only the last
three` and `/nope` shows the four answers in the order the design wants, and the session file holds
`{"type":"chat","message":{"role":"user","content":"Tidy the commits touching src/parser.rs, then say
what changed."}}` — the expansion, not the line. See the note under *Traps* about `$home` in
PowerShell: running that check cost two minutes of cleanup because `$home` is read-only and the scratch
`FLINT_HOME` silently became the real one.

### Bringing a conversation in: `/import`, a copy and not a resume

**The command is one act with three refusals, and the refusals are the design.** `/import <file>` reads
a session file — by path, or by the id `resolve_session` accepts, which is the same resolver `/resume`
uses — and copies its conversation into a conversation of this run's own, in this run's sessions
directory, with this run's `meta`. The source is never opened for writing, which is what the test
asserts on bytes: the given file is read before the run and compared after. A file that holds no
messages is refused (`nothing to import: <file> holds no conversation`), the file this run is currently
writing is refused (canonicalised on both sides, because the same file reached by another spelling is
the same file — a run whose conversation has not been written yet has no path to compare against and
cannot be in that position), and `--no-session` refuses it with the same sentence every other
conversation-opening door gets.

**The provenance line, and why it is an event.** `SessionWriter::imported_from` writes
`SessionEvent::Import { from, from_id, messages }` between creating the writer and writing the messages,
so it lands directly under the `meta` line. `SessionWriter::seed` was split into `create` +
`write_messages` to make that ordering possible without duplicating the loop — the loop's one rule being
that a system prompt is not part of a conversation, since it is rebuilt per run from the machine flint
is on. `write_messages` is also what `--fork` uses now, so a copy made by a fork and a copy made by an
import cannot drift apart on that rule. `from` is the path as given (a fact of the moment: the file may
have moved, or never have been on this machine, and a copy that could not be read without it would not
be a copy), `from_id` is the source's id as flint read it (its `meta` id, or its file name when it had
no `meta` — the hand-written case), and `messages` is the size of the *import*, stored rather than
counted later because the copy grows. It is read back by `session::load` into
`LoadedSession::imported`, and printed by `imported_note` in the two lines that name a conversation to a
person: the startup `flint: resumed … (12 messages, imported from given.jsonl)` and `/resume`'s own
line. `KNOWN_TYPES` went from seven to eight, which is what keeps a *damaged* import line reported as
damage rather than skipped as somebody else's event.

**What was deliberately not built.** `--import` on the command line: `/resume <path>` already starts a
run from a file, and the only thing that needed adding was the door that does not write into it — which
inside a run is one line typed. And no `SessionSummary` field: the listing does not need provenance to
answer "which conversation is which", and a field carried by four readers is four places to keep in
step with the two that actually print it.

### The cache split, and the counts that were written and never read

**The number is an `Option` because three facts wear two faces.** An endpoint may report a cache split
in DeepSeek's shape (`prompt_cache_hit_tokens`), OpenAI's (`prompt_tokens_details.cached_tokens`), or
not at all — and "not at all" is not `0`: it says nothing about whether the prompt flint builds is
stable. So `provider::usage_from` collapses the two shapes into `Usage::cache_hit_tokens`, and every
printer (`/usage`, the footer, the two `--json` frames) skips the cache entirely when it is `None`.
`Usage::cache_rate` is the one place the arithmetic lives: rounded half-up, bounded at 100, and `None`
for a prompt of no tokens rather than a division that panics. It is computed at the point of printing
because the two numbers it needs are both in the file, and a stored percentage could disagree with them
after a hand-edit.

**The damage that building it exposed.** `Loaded::last_usage` was parsed out of every session file and
read by nobody, so a resumed conversation's `/usage` said "no usage reported yet by this provider" while
its own file held the line; four mid-run rebuilds (`/model`, `/provider`, `/reload`, `/readonly`) lost
the same numbers by handing over the messages and nothing else. `Agent::set_last_usage` is called at all
four sites plus startup now, which is why `usage_prints_the_cache_split_it_read_back_from_the_session`
drives a *resumed* session: it holds the format (a `usage` line written by one run, with the split on
it, still means the same thing to the next), and it is race-free, because nothing is typed while a turn
is running.

**A harness detail worth keeping, because it cost a red herring.** The first version of that test typed
`hello\n/usage\n` in one write, and the capture showed the first turn dropped with *"no usage reported
yet"* and no request made at all: a line that arrives while a turn is in flight is an **interrupt**, not
a queued command, so the second line killed the turn whose footer the test wanted. One line per REPL
test is the safe shape; where two lines are needed, the second must wait for the turn (or, better, be
avoided — as here, by resuming a session instead of asking twice).

**Also worth knowing when a test asserts a word is *absent*:** the scratch `FLINT_HOME`'s path is
printed on the run's first line, so a fixture tag containing that word (`flint-cache-footer-quiet-1234`)
puts it on screen and fails the control for the wrong reason. The tags in these tests deliberately name
no cache.

### Still owed on the page

**§8 is built, so this list is now the residues rather than a class.** The command list, its panel, the
buttons, the reports, the selectors, the forms and the destructive controls are all in. What is left,
each with the reason it is left:

1. **`/config edit` as a page form** — **closed 2026-09-17, the other way round from how it was
   written here.** The line above said the page would need a `/config set <key> <value>` the terminal
   did not have, and that inventing a command for the page's benefit is what §8's design exists to
   prevent — both true, and the answer was to give the *terminal* the command it should have had
   anyway: `/config set <key> <value>` is the wizard's four questions asked and answered in one line,
   the keys are the four the wizard edits, and the terminal keeps the wizard for whoever wants to be
   asked. With the command in the terminal, the page is handed the row the ordinary way (`fields`:
   the key, and the value marked optional, because a blank value is how a proxy is cleared) and writes
   the config file through `/message` like every other row action. `/config edit` itself stays a row of
   reference on the page, which is the rule rather than a residue: a wizard is not a form. Building it
   also fixed a lie in the wizard that predates the page — it saved the file and left the running tools
   on the old settings (`ToolBox::new` clones the config into each tool, and `max_steps` goes into the
   agent), so `/config` printed a value that was not the one in force. Both paths now end in the same
   rebuild `/reload` uses, and the command says `in force now` because that is true.
2. **The mid-turn wait for a report** — ~~asserted nowhere~~ **asserted since, 2026-09-16**, and this
   line was stale: `a_report_asked_for_mid_turn_waits_for_the_turn` in `tests/cli_output.rs` drives a
   real `--web` process against a stub that draws a delta and holds the socket open, accepts a report
   at `/report` mid-turn, stops the turn for real, and asserts the *order* of the two frames off one
   feed (`turn.completed`, then the answer). The mechanism (`Handover` stashing it rather than
   treating it as an interrupt) was already right; what was missing was a turn slow enough to ask
   during, and the stub now provides one.
3. **A browser** — **done for every control §8 built, and the two halves are still worth keeping
   apart.** §11's `L3 in a real browser` table was measured over the Chrome DevTools protocol against
   the real page and the real binary: the sidebar listing, switching by clicking a row, `+ new`, Enter
   to send, the layout at two window sizes — and it found three defects that reading the source could
   not. The *later* controls have since had the same treatment, from a committed harness rather than a
   one-off: `scripts/browser-controls-test.js` drives the switches, the command panel, an action
   button, the masked credential field and a destructive row's menu with real input events, and checks
   every press against the *run's* stdout rather than against the page — which is the only witness that
   can tell a click that sent something from a click that sent nothing. It found two defects, both
   fixed and both invisible in the source: a fresh `--web` run answered 500 on `/session` (the run
   names its session before the file exists, so the page never drew a single control), and with the
   `commands` panel open the `send` button could not be clicked at all, because the panel is the one
   thing in the header that grows without bound and it pushed the reading over the composer. §11's
   `The later controls, in a real browser` is the table and the method; what is still unmeasured is a
   short and named list — a native `<select>`'s open dropdown, the sidebar's own menu, and the drag
   grips. **The last two of those three are now driven too, and the pickers are driven from the
   keyboard**: the harness is 53 claims (was 44, then 34, then 20) and covers the sidebar's `⋯` menu
   on both kinds of
   row — the open conversation's, whose rename field reaches the run and whose first press sends
   nothing, and a fixture conversation's, whose second press carries *that row's* number and removes
   that file, which the directory listing witnesses — both hands by a real pointer drag with the
   button held, plus the arrow keys and the double-click reset, and `#pick-model` by focus and
   ArrowDown, checked against the run's own `ok model …` line. A mutation proves the drag claims are
   load-bearing rather than decorative: neutering the page's `pointermove` handler leaves the two
   "takes a real drag" claims failing (32/34) while the arrow-key and double-click claims still pass.
   What is left uncovered is one widget and one section: a native `<select>`'s open *popup*, which
   belongs to the operating system and no protocol can reach into, and §11's own long answers and
   reconnection, measured on macOS over a different harness.

   **The ten claims the file preview added changed the harness itself, and the nine the jobs panel
   added used what that built**: it starts a *scripted model* (`stubModel`, the SSE shape
   `tests/task.rs` serves) so a browser claim can be about a real turn — a `write`, a `read` of that
   file, a `read` of a file that is not there, a `grep` whose output carries a line number, and now a
   background command plus a `task` child — rather than about a transcript that is empty because
   nothing was ever asked. That is what makes "the panel shows the file the run just wrote" an
   end-to-end claim: the bytes in the panel travelled from the model's tool call, through `write`,
   through `GET /file`, into the page. §12 of `docs/web-mode.md` is the preview table and §13 the jobs
   one. Two of the preview claims found defects rather than confirming the design: the fixed panel
   covered the block the path was pressed in (the panel is a grid column now, and the harness's own
   coverage check is what caught it), and `every()` on an empty array — a JavaScript trap in the path
   splitter — swallowed every bare filename until `Cargo.toml` was in the Node checks. The jobs claims
   found two defects *in the claims themselves*, which is the same lesson one level in: one accepted
   any settled row when the child settles before the command, and one reused a row's `id` and so
   pressed the wrong row the second time.
4. **Renaming from the sidebar** — ~~a `/name` field exists in the panel and works, and the sidebar
   has no affordance for it~~ **built 2026-09-17**, exactly as this line predicted: a `/name <text>`
   line through `/message` like every other row action, with no route of its own. The row's menu
   carries a field on the **open** conversation only, and that is the one design decision in it:
   `/name` names the conversation the run is *writing*, so a field on another row would either rename
   the wrong conversation or have to switch to it first — a second line whose refusal (the session
   archived in the meantime) would leave the rename aimed at whatever was open. The field starts on
   the label in force, because a rename is usually a correction, except when that label is the page's
   own `(empty)`, which is a placeholder for a nameless conversation and not a name. The field is
   drawn from the frame's own `/name` row — only when the frame describes one, like the destructive
   rows above it — and its line is composed by the same `formLine` the panel's forms use. An emptied
   field sends nothing, and the reason is not politeness: `/name` with no text *reports* the name, so a
   cleared field would ask a question nobody asked. A press inside the field stops propagating, or
   reaching for it would open the conversation under the person typing in it. And `/name` now pushes
   the `sessions` frame `/archive` and `/delete` push (`viewer.list_changed()`): the rename field is
   *in* the sidebar, so a page that renamed a conversation and went on showing the old label is a
   rename that looks like it failed. Tests: `the_sidebar_renames_a_conversation_through_the_same_form_composition`
   (`tests/web_view.rs`, the composition and the refusal, mutation-checked), the rename check in
   `scripts/web-view-test.js` (the drawing, the prefill and the empty-field refusal), and
   `renaming_a_conversation_tells_the_page_to_read_the_list_again` (`tests/cli_output.rs`, a real
   `--web` process, red without the frame).

- ~~The small queued-line hole above still wants its two structural lines before a test can hold it.~~
  **Fixed, and this bullet was left standing over it** (it was written 2026-09-14 and survived the fix):
  the hole is the one "A line already waiting no longer erases the question it interrupts" below records,
  and the two structural lines it wanted are the `poll_fn` that polls the turn once before the input loop
  starts reading — held since by `a_line_that_was_already_waiting_does_not_erase_the_question` in
  `src/main.rs`'s own tests. The report path leans on the same machinery and never had the hole:
  a report arriving mid-turn is stashed rather than consumed as an interrupt.

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

**Recorded rather than fixed** was the shape of this paragraph. A Unix process-group kill for work a
command backgrounds is now **built** — `process_group(0)` at spawn and `killpg` at both kill paths, with
the test watched red on the ubuntu job before the code existed (the paragraph above has the mechanism
and the failure it printed). The second item this paragraph used to carry — the same line-ending
sentence for `apply_patch` — is built; see the paragraph above.

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

**The mojibake scan had a hole, and this session fell into it.** Rewriting `tests/cli_output.rs`
through PowerShell 5.1 (`Get-Content -Raw` → `[System.IO.File]::WriteAllText`) round-tripped the file
through CP936 and turned comments and fixtures into mojibake — and `the_source_tree_contains_no_mojibake`,
the test written for exactly this, **stayed silent**. Two reasons, and they are separate:

- **The page was not scanned at all.** The extension list was `rs`, `js`, `md`, `toml`, `yml`, `yaml`;
  `web/view.html` is `.html`, so the one file a person reads flint's words in through a browser was
  outside the guard. **Closed 2026-09-17**, red-first: with a middle dot in the page's composer hint
  replaced by the CP936 artifact it decodes to, the test passed without `"html"` in the list and fails
  with it. Nothing had to be exempted — the page's non-ASCII is an em dash, a middle dot, a section
  sign, an ellipsis and two ballot marks.
- **The marker list knows one generation of damage.** It matches what *one* bad round trip produces;
  a second round trip over already-damaged text produces characters it does not hold. That is what
  happened here (the file already held second-generation characters from an earlier accident), and it
  is why the damage was only caught by reading the diff. **Fixed the same day, and not by extending the
  list**: the test is a whitelist now, so every non-ASCII character in a scanned file has to be one the
  repository means — a punctuation or symbol class, or CJK in a file declared in `CJK_FILES` with a
  reason. Both layers were watched failing for the right reason: U+597D in `docs/web-mode.md` is
  refused by the whitelist and held by no marker, and U+8DEF in `README.md` is refused by the marker
  list while the whitelist allows it. The residue is stated rather than implied — inside a declared
  Chinese document, a second-generation artifact is still indistinguishable from prose.

The practical rule that came out of it, for the next session: **do not rewrite a source file with
PowerShell's text cmdlets or `WriteAllText`.** They round-trip through the console code page. The `edit`
and `write` tools write UTF-8 and are safe; `git checkout -- <file>` then re-applying is the repair.

**Reading is a trap too, and it cost a detour while this paragraph was being written**: `Get-Content`
without `-Encoding UTF8` decodes a file as the ANSI code page, so it *fabricated* U+9225 and U+6402 in
`HANDOFF.md` and `ROADMAP.md` — files this test passes — and made the plan of record look damaged. A
clean file read that way reports the same characters a damaged one does, so the check is worthless as
a detector of anything. Read non-ASCII with
`[System.IO.File]::ReadAllText($path, [System.Text.Encoding]::UTF8)`, or with the `read` tool; and
when a marker's code point has to be named *inside a markdown file*, name the number (`U+9225`) rather
than the character, because the guard scans `.md` too and would rightly flag the character itself.

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
[`ROADMAP.md`](ROADMAP.md) §8 rather than here, because the middle one was a design question and
not a defect: a conversation that has not happened yet shows in the sidebar as `(empty)`, a command
typed into the composer prints nothing (its output goes to the terminal and nowhere the page can
read) -- **and that one is built**, both halves of the read channel, so a command's answer reaches
the page as a `command` block and the panel's own reads go over `/report` -- and renaming a
conversation from the sidebar has no affordance. **Two of the three are closed since**: the empty
row is gone, because the session file is claimed by the first event rather than at startup —
measured on 2026-09-16 with the real binary (`GET /sessions` in a scratch home answers
`{"sessions":[]}` before a word is said, one row after one line) — and the composer one is built.
Renaming from the sidebar is the last of them, and it is a drawing job rather than a mechanism. A
fourth from the same sessions -- history tidied in the terminal leaving the sidebar stale -- is
fixed, because the numbers in that list are positions and a stale row resumes the wrong
conversation.

**A terminal that goes away now takes the run with it, and the spin it used to take a core with was
`crossterm`'s.** When flint's pty is closed without a `SIGHUP` -- a terminal emulator that crashes, a
`close(master)` from the other end -- the process spun at 100% of a core forever and never exited
(100.3% and 100.7%, measured on the two builds before this one, so it predates the browser work). The
spin is not flint's: `crossterm::event::read()` is `try_read(None)`, whose loop condition
(`timeout.leftover().map_or(true, |t| !t.is_zero())`) is always true, and a hung-up descriptor keeps
reporting itself as ready -- `POLLHUP`, with Linux leaving `POLLIN` set as well -- while a read on it
produces end of file rather than an event, so `poll` returns at once and it polls again
(`event/source/unix/tty.rs` in crossterm 0.29 is the place to read). The paragraph that used to sit
here sketched the fix as driving the key thread from `event::poll(timeout)` and checking whether the
terminal was still there; what is built is the check, in a thread of its own. `watch_for_hangup` in
`src/main.rs` polls the descriptor crossterm reads -- stdin when stdin is a terminal, `/dev/tty`
otherwise, which is `tty_fd`'s own rule -- and on a hangup sends `Quit` on the input channel, which is
how an EOF on a pipe already ends a run. The key thread is untouched on purpose: on Windows the read
reports a vanished console itself, and nothing on this machine can drive the Windows key path (the
capture hook reads lines from a pipe, and there is no pty without a console), so the one path that
cannot be exercised here is left exactly as it was. `libc` moved from the dev-dependencies to a
Unix-only dependency for it, which adds no crate -- it was already in the tree -- and it also removed
half the cost of the Unix process-group kill that `docs/windows-tooling.md` §6.1 has since closed with
it.

The test is Unix-only because the bug is: `tests/tty_hangup.rs` gives a child a real pty as its
standard input, output and error (so `isatty` is true and the interactive path -- the one with the key
thread in it, which nothing else in the suite reaches -- is what runs), marks the master `FD_CLOEXEC`
so the child cannot keep its own terminal alive, waits for the banner, closes the master, and asserts
the process ends within ten seconds. Two guards stop it passing for the wrong reason: the banner must
have been drawn, and the process must still be alive at that moment, so a child that died at startup
cannot satisfy "it is not running any more". Its failure message carries the CPU the child burned,
because a hang and a spin are the same to a stopwatch and nothing alike in `/proc`. Three CI rounds
were needed, and each one is the reason for something in the code: the first failed the *guard* (the
banner was in the capture and the assertion did not match it, because the banner is coloured per piece
and only contiguous on screen -- stripping the escapes is the fix, and it is the same trap as
asserting on a terminal without replaying it); the second failed with "10s after the pty was closed it
was still running, having burned 10.2s of CPU", which is the defect reproduced on Linux at one core;
and the third burned **20.2s in the same window**, two full cores, because the first version of the
watcher waited for `POLLHUP` *without* `POLLIN` on the theory that a hangup arriving with readable
input was the last of the input and the key thread should have it. On a hung-up pty `POLLIN` stays set
for ever and a read clears nothing, so that rule never fired and the watcher spun beside crossterm. A
hangup is a hangup: bytes still in flight are lost with the terminal they came from. The run after
that one is green on both platforms, and that CPU number is the whole argument for watching the red
before believing the green -- the second bug lived inside the fix for the first.

**The Windows half of the tooling plan is written, and this paragraph used to say it was not.** All five
steps in [`docs/windows-tooling.md`](docs/windows-tooling.md) §7 are done on this machine — the shared
runner, `exec` with an argument array, `pwsh` with the script handed over as a BOM'd file, the three
small fixes (backslash normalisation, the `taskkill /T` process-tree kill, code-page decoding of child
output), and the PowerShell facts in the system prompt — and the two tests that paragraph called
unwritten exist and pass: `a_quoted_command_reaches_the_shell_verbatim` and
`a_powershell_script_runs_from_the_file_it_was_written_to` (argument round trip, non-ASCII script text)
in `tests/agent_loop.rs`, and `a_killed_command_takes_its_children_with_it` for the tree kill. What was
genuinely open on that list — a Unix process-group kill for backgrounded work — is **built now**: a
process group of its own at spawn and `killpg` at both kill paths, with its own test watched red on the
ubuntu job and green after the fix, `taskkill /T` being what Windows already had. The line-ending
sentence for `apply_patch` (§6.4) was the other item on that list and is now built, with the fact it
measured: a patch line cannot carry a `\r`, so the sentence names `edit` instead.

**Windows terminal behaviour has now been measured, in a private console, and is kept here as
history rather than as an open item — nothing in it turned out to be broken.** Read
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

**CI runs the tests now, on every push, on two platforms — and the first red build it produced was
the point of building it.** The paragraph that used to sit here was wrong in both halves:
`.github/workflows/release.yml` triggers on *every* push (`on: push:` with no filter), not only on
tags and `workflow_dispatch`, and the ten most recent runs are green — so a push to `main` has been
building four targets all along. What no workflow did was run a single test, and a binary that links
and a program that works are different claims. `.github/workflows/ci.yml` is `cargo test` plus
`cargo clippy --all-targets -- -D warnings`, on `ubuntu-latest` and `windows-latest`, on push,
`pull_request` and `workflow_dispatch`. Linux was green from its first run (143 s including a cold
build); Windows was red twice before it was green.

Reading a red build from *this* machine is the part worth writing down, because the obvious route
does not work. A step's own output is behind the log-download endpoint, which wants a token with
write access, and this repository is pushed over a deploy key and has none — so the first Windows
failure arrived as nothing but "Process completed with exit code 1". A workflow can say more than
the runner does: `check-runs/{id}/annotations` answers an *anonymous* request, so the job re-emits
the failing test's name and its panic through `::error::` and the reason is readable from the same
place as the conclusion. Two bugs in that step were caught by its own first run — recent Rust prints
`thread 'x' (1234) panicked at`, so an anchored pattern matched nothing, and a `grep` that matches
nothing exits 1, which turned the reporter red and added a failure that was the report's own.

**The Windows failure was a wall-clock bound, not the tool.** Named by that annotation:
`a_fan_out_runs_the_jobs_at_the_same_time_and_labels_every_answer` at `tests/task.rs:718`, "the
fan-out did not overlap: it took 5.0782874s" against `elapsed < 5000ms`. The assertion that actually
measures concurrency — the children's requests arriving within 600 ms of each other — passed. The
failing one was a stopwatch guess that this machine wins every time; measured while fixing it, six
busy loops saturating this machine still left the test passing at 4.59 s of its 5 s, so the margin
was under half a second on a machine that is not the slow one. The bounds are written in the test's
own unit now: the stub holds each child's answer for `JOBS_DELAY` (3 s), so three children one after
another *cannot* finish in less than 9 s, and the assertion is that the fan-out beats that floor;
the spread bound is a third of the hold. A tighter number than the serial floor would be measuring
the runner.

The cross-compile still cannot be type-checked from a Mac: `aws-lc-sys` (rustls's crypto backend)
needs a Windows C toolchain, not just `rustup target add` — which is an argument for this CI leg
rather than against it.

**The `eprintln!` sites that could fire inside the strip are fixed, and there were four rather
than three.** The list named `agent.rs` (a session event that could not be persisted), `config.rs`
and `session.rs`; walking the tree for the same shape found the fourth and the most likely one —
`provider.rs` printed the retry ladder's line from *inside the request loop*, so every endpoint
hiccup wrote to stderr during a turn. All four go through the notice sink now (`tools::notice`,
whose doc says it is the one route for anything below the REPL that must not write to stderr), which
means the transcript when there is a UI and the same stderr line as before when there is not: a
`--json` run and a test both fall back, so nothing a program reads changed. `config.rs`'s message
was one write carrying a newline and is two notices now, because one `line` call with a newline in
it walks down rows the layout reserved for something else — and the two lines it prints on a first
run are byte-for-byte what they were on stderr. The remaining `eprintln!` calls are all
*startup* ones (the missing-session and forked-into lines, the presence record that could not be
written, the top-level error printer, a one-shot failure), which run before the strip exists or on a
run that has no UI at all.

Two tests, both watched red first, both two-sided on purpose — a warning that vanished passes "not
on stderr", and one printed twice passes a one-sided check: `/resume` on a file with an unreadable
line (`session.rs`'s path, cheap and deterministic), and a turn against a dead endpoint so the
ladder walks its 1s, 2s and 4s waits (`provider.rs`'s path, ~10s). The second one also measured
something worth keeping: piped input is *steering*, so the first version of it sent `/exit`
immediately and the turn was dropped before any retry happened — the test passed nothing and failed
nothing until the second line was written late. It also shows why the assertion is on `"trying in"`
rather than the whole sentence: the notice is longer than the strip, so the captured bytes carry a
wrap and a repaint escape in the middle of it.

**The request is bounded by construction now, not only the turn.** The step guard is 100, and what
keeps a long turn from walking into the context ceiling is request-side pruning of stale tool output
(`prune_tool_output` in `agent.rs`): the four most recent results, anything short, and every failure
are kept; older long output is replaced with a note. Neither of those bounds a *conversation*,
though, and nothing ever dropped an old turn — so a long one grew the request until the provider
refused it, in the middle of a turn, as an error nobody decided on. `max_request_chars` is the bound
(default 400,000 characters, the unit `max_tool_output` uses, `0` turns it off) and `trim_old_turns`
in `agent.rs` enforces it on the request side only: the session file keeps every message, the same
promise pruning makes, and a note where the turns were says how many went and that the file has them.

Three decisions are worth keeping. The unit is a **turn** — a user message through to the next one —
because a tool result whose call has been dropped is a request the provider rejects, and a turn
boundary cannot fall inside such a pair (`every_result_answers_a_kept_call` in the tests is the check,
and it fails if the trim is changed to cut at every message). The **newest turn is never dropped** and
the system prompt is never counted against the budget, whatever the numbers say, because a request with
nothing to answer is not a smaller request — and the note itself is charged to the budget, so the
message that says the request was too long cannot be what makes it too long. Pruning runs first, in
both `Agent::step` and `Agent::request_preview`, so the cheap bound is spent before the expensive one;
that same order in both places is what keeps the preview's promise that it prints what would be sent.
Seen from a terminal: `/config` prints the number in force, and `flint --resume 1 debug prompt-input
"..."` in a home whose `max_request_chars` is small shows a request that opens with the note while the
file on disk still has the first question.

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

1. **The Windows command line** — settled in full in
   [`docs/windows-tooling.md`](docs/windows-tooling.md) and **built**: all five steps are in the
   tree, and the parts that needed a real machine were settled on one (Windows 10.0.26200,
   rustc 1.98.1, PowerShell 5.1 as the only PowerShell on `PATH`). The one item this entry used to
   carry as open — a Unix process-group kill for work a command backgrounds (§6.1) — is **built**
   (`KillTree::detach` puts the child in its own process group, the guard signals the group, and the
   ubuntu job watched the test fail before the code existed). Nothing in the plan is left open; what
   remains here is the honest limit `docs/agents.md` states — a run killed outright leaves its
   children behind, because the guard is flint's own code and does not run.
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
   scrollback). What step 3 owed was the **consolidation**, and it landed a few paragraphs below in
   this same entry — this sentence and the "What step 3 still owes" line in `ROADMAP.md` §6 were both
   written before it and were left standing over it until 2026-09-17, when they were rewritten to
   point at the record. `ROADMAP.md` §6 keeps what the merge had to
   preserve, because two of the four pieces cannot be deleted: `segment_text` (what
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
3. **Web mode** — `--web` as a window onto the running process rather than a mode, and it is
   **built**: the three levels, §7's server question (settled without `hyper` — `src/web.rs` is a
   hand-rolled loopback listener), and §8's five classes of control — the buttons, the panels that
   read, the selectors, the forms and the destructive ones. What was left was the two residues listed
   under "Still owed on the page" above, and both have moved: `/config edit` as a page form is
   **closed** — the terminal got the `/config set <key> <value>` it was missing, so the page writes the
   config file through a two-field form like any other row, and the wizard stays in the terminal where
   a person can answer it (see that entry for the lie in the wizard this also fixed). The later
   controls were the one thing still asserted as bytes rather than driven as clicks; they have since
   been driven, in `scripts/browser-controls-test.js`, which found two defects and fixed both (see
   entry 3 under "Still owed on the page"). §11 names what a browser still has not touched.
   `config.toml` writes are allowed because the per-run loopback token already covers them, and
   `/exit` stays off the page.

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

### Driving flint by hand from PowerShell

Checking a REPL change without the test harness means pointing `FLINT_HOME` at a scratch directory and
typing into the binary, and there is one trap in that worth writing down because it silently ruins the
check instead of failing it: **`$home` is a read-only automatic variable in PowerShell**, so
`$home = "$env:TEMP\flint-scratch"` is refused with *"Cannot overwrite variable HOME"* — the *session*
keeps the real home directory — and every later `$home\...` path then writes into your own profile: a
`config.toml`, a `prompts\`, a `work\`, and a `sessions\` full of scratch conversations, all of which
have to be found and removed by hand afterwards. Use another name (`$fh`) and set `$env:FLINT_HOME`
from it. The same shape of trap catches a real run: `FLINT_TERM_CAPTURE_FILE` is read only by a debug
build, so a release binary ignores it and the capture goes nowhere.

### Reading a red job without a log

GitHub will not hand a job's log to an unauthenticated reader — `GET
/repos/tedllll/flint/actions/jobs/<id>/logs` answers 403 "Must have admin rights to Repository" — and
the web UI's log pane is not fetchable either. What *is* readable is the structure, and a failing step's
name plus its duration is usually the whole diagnosis:

```bash
curl.exe -s "https://api.github.com/repos/tedllll/flint/actions/runs?per_page=4"   # runs, with conclusions
curl.exe -s "<the run's jobs_url>"                                                 # the four targets
curl.exe -s "https://api.github.com/repos/tedllll/flint/actions/jobs/<job id>"     # steps, conclusions, timestamps
```

Two facts worth keeping: `ci` is `cargo test` and `clippy` on Linux and Windows, and `release` is the
four binaries (two static musl, macOS, Windows). So **a red `release` job with a green `ci` is almost
never a compile error** — read the step names before reading the diff. The case that proved it
(2026-09-17) was `aarch64-unknown-linux-musl` failing on `bacae13` inside "Install zig (for static musl
builds)" after **one second**, while the x86_64 musl job on that same commit spent **824 seconds** on the
same step for the same version, downloaded it, and passed — and the aarch64 job had spent **981** and
**991 seconds** there on the two commits before it. The action was the fault, not the code: the fix was
`mlugg/setup-zig@v2` (community mirrors, signature check, cached tarball) in its own commit, and the
next run confirmed it — the same step took **10 seconds** for arm64 and **45** for x86_64, four green
targets, and the `release` workflow has been quiet since.
