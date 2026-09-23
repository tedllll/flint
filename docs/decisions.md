# Decisions

Why flint is the way it is: the choices that had a real alternative, and the reason the
alternative lost. Code says what it does; commits say what changed; this file is for the
reasoning that neither can carry — the ones worth not re-litigating in six months.

Anything here that later turns out to be wrong should be corrected here first, then in the
code. A decision that is no longer true is worse than no record at all.

## State, and the files it lives in

**Nothing is derived.** No index, no cache, no database. The session list, the resume
target, the skill catalog and the transcript are all computed from real files every time
they are needed. If flint is killed, or the power goes out, the files on disk are still the
whole truth. The cost is speed, paid only where a directory listing is already fast enough.

**Sessions are append-only JSONL.** Nothing is ever rewritten — not to rename a
conversation, not to add a token count. A rename is one appended `title` event, and the last
one in the file wins. This is what makes a session repairable with a text editor, and it is
why a crash mid-write loses at most one line. Reference:
[`session-format.md`](session-format.md).

**The format version is a defaulted field.** A file that does not mention `v` is v1, which
is every file written before conversations could be named. Requiring the field would have
turned every existing file into an unreadable one, and hand-written files are a supported
input.

**An unknown event type is skipped in silence; a known one that will not parse is
damage.** The difference is the whole point: "another version, or you, wrote this" is not a
reason to refuse a conversation, while a `chat` line that will not parse means the
conversation has a hole in it. Reporting the first as corruption would make every future
version a corrupt file for this one.

**Listing reads only the two ends of each file** — 64 KiB of head, 16 KiB of tail. A list
needs a name, a date and a size; reading a hundred megabytes to get them would make the
listing of a long conversation cost seconds, which is exactly when a listing is wanted.

**Timestamps are `epoch:<seconds>`.** Not a date format, and the function that produces it
was called `now_iso8601` until writing the format document found the lie. A date library is
a dependency; ordering sessions and showing a person roughly when one was written is not
worth one.

**`/archive` is a `mv`.** The session moves into `sessions/archive/`, which is why it is
instant, why it needs no model and no key, and why undoing it by hand is the same command
backwards. The moment you want to tidy the list is often the moment the network is broken.

**The config is hand-editable TOML, and missing keys take defaults.** A new key must not
break an old file. A *wrong* value, though, fails at load with the values that would work —
`instructions = "sometime"` is reported when the file is read, rather than being discovered
later as "the model ignored my instruction file".

**There are two `proxy` keys.** The one inside a provider table is how flint reaches the
model; the top-level one is exported to the commands the `bash` tool runs. They are
different routes, and on a machine where one works and the other does not, conflating them
takes away the ability to fix either.

**`--no-session` is a property of the run, so the doors are what it has to close.** The file was
never the hard part — a session is created by its first event, so a run that says nothing writes
nothing — and the decision that made the flag real is that a promise about what is written is only as
good as the ways to write it. So `--continue`, `--resume`, `--fork` and `--name` refuse it on the
command line, `/new`, `/resume`, `/import` and `/fork` refuse it inside the run (through one function,
so the page's sidebar rows and a typed line answer the same), and the funnel every mid-run switch goes
through —
which *creates* the session when the old conversation has not said anything yet — is told once and
carries the flag. The obvious shortcut is wrong for a reason worth keeping: the flag cannot be derived
from "there is no writer", because a writer that has not appended yet and a run that will never write
look identical from the outside. What such a run still writes is what it needs to work — spilled tool
output, a background command's log — under `spill/unattached-<pid>/`, one directory per process rather
than one shared `unattached/`, because two of these runs both number their spill files from `1.txt`.
And the flag reaches a `task` child, which is the part that is not obvious until you look at what
excludes a child's conversation from the person's list: that is `FLINT_PARENT`, the parent's session id,
which a run with no conversation does not have.

**A fork is a copy, and its lineage is an event — never `meta.parent`.** `/fork <n>` cuts this
conversation at the n-th question and starts a conversation of its own from what came before it;
`--fork <file>` copies the whole thing at startup. The original keeps every byte, which is the whole
point: "that went wrong four messages ago, start again from there" is a thing a person needs, and doing
it by copying the file and deleting lines from it by hand is what people actually did. Both doors write
one `fork` event — `from`, `from_id`, and `kept` when the copy is a prefix — and the reason it is not
`meta.parent` is that `parent` means "another run started this one" and is what files a conversation
under `children/`: a branch would then be excluded from `/sessions`, the sidebar and `--continue` as
somebody's child, which is the opposite of what it is. It is an event rather than a field on `meta` for
the reason `title`, `switch` and `import` are: the file is append-only, and a fact that arrives at a
moment is a line.

**A fork cuts at a question, and only at a question.** The unit is a `chat` line from the person — a
turn boundary, the same one `trim_old_turns` cuts on, and for the same mechanical reason: a tool result
whose call was left behind is a request the provider rejects. A question is also the only boundary a
person can *name* at the prompt, which is why `/fork` takes the ordinal of a question rather than a
message index: a count of chat lines is not a thing anybody knows about their own conversation, and the
list `/fork` prints is what makes the number choosable. The alternatives were refused for their own
reasons — a bare `/fork` that cut at "the last exchange" would throw work away without being asked, which
is the surprise `/sessions` exists to avoid — and the one that *looked* refused for a reason and was not
is cutting in front of the first question: with nothing folded that leaves an empty conversation (an
empty file in the list that looks real, which is why `/new` is that act instead), so the copy is built
and refused only when it is **empty**, which is exactly the case a `/compact` changes: after a fold the
first question the run still holds has the summary in front of it, and cutting there is the branch a
person wants — the digest, ready for that question to be asked again.

**A fold's summary is not one of the questions.** It is written as a `chat` line with the person's role,
because that is the only shape every endpoint accepts, so anything counting questions by role counted it
too — and the count is what `/fork <n>` takes, which is what the page's buttons and `--fork`'s list
hand a person. So the marker that says "this line is a summary" is one constant
(`session::SUMMARY_MARK`), written by the code that composes the message and read by the one function
that answers "what are this conversation's questions", and nowhere else: two copies of that sentence is
how the count and the list would drift apart again.

**The page is told the questions twice — numbered and named — and pairs them itself.** The page cannot
count the run's questions (a fold is a fact about the history, and the page reads a file) and the
process may not count the page's turns (that is the page's own scrollback, and it would need a second
bookkeeping of every message ever written), so `/fork`'s row carries the numbers the command takes
*and* their first lines, and the page puts a **fork from here** button on the answer above the question
it can point at. Pointing at it means: paired from the bottom of both lists together, where a fold's
dropped prefix cannot reach; believed only when the turn's own first line is the question named; and
believed only when that turn is the *only* one that reads that way. Two alike questions therefore mark
nothing rather than guessing, the first value is never a button (nothing the run still holds is in front
of it), and a turn in flight takes the buttons away because the run's list is one question behind the
page's drawing until the turn ends — the dialog's labelled list is the fallback that always works,
which is what makes refusing the rest honest.

**Where a conversation came from is a sentence on its own line.** A copied conversation — imported or
forked — says so when it is named to a person, on the `resumed` line and under `/resume`: `this
conversation was forked from <file>, and holds the first <n> messages`. Not a fragment appended to the
line that names the conversation, which is where it started and where it had to move: the line already
carries the file in force and the model, and the two together with a provenance fragment passed 100
columns — where what wrapped was the count a reader is looking for. It is one function for both kinds of
copy and both doors, so no two of them can drift into saying different things about the same file, and
the startup line on stderr says the same sentence for the same reason even though nothing wraps there.

## An export is the page, not a second reading of a conversation

**The renderer is the page's, and the export is that page with the conversation inside it.**
`flint export` writes `web/view.html` with the session file's own lines in a JSON island. The
alternative — rendering the conversation to HTML in Rust — was refused for the rule this repository
keeps everywhere: it would be a second place where "what a tool call looks like" is decided, and the
second place is the one that drifts, because the page is what somebody is actually looking at. The
lines are the file's own bytes rather than a derived shape for the same reason: `applyText` already
parses `meta`, `chat`, `usage` and `title`, so an export is the same vocabulary the page has always
read, and a shape invented for the export would be derived state inside a file meant to be opened by
somebody else's browser for years. It also means the drawing needs no test of its own: it is the
page's own model half, which `scripts/web-view-test.js` runs line by line.

**An export does not carry the directory the conversation was held in.** `cwd` is removed from the
`meta` line. It is the one field in a session file that names the machine rather than the
conversation, and the export is the one file here whose entire purpose is to leave that machine —
which is what makes this worth doing deliberately rather than inheriting. The provider and the model
stay: they are facts about the conversation. A related accident is why the removal happens after a
byte-order mark is stripped: `notepad` writes utf-8 with a mark, JSON stops at it, and a `meta` line
that reads as damage is a line whose `cwd` was never taken out — so a file that is already damaged on
this machine would have exported the very thing the export exists to leave out.

**A conversation cannot become script in the file it is exported to.** The island lives inside a
`<script>` element, which ends at the first `</script` in its text, and its text is a conversation: a
tool result quoting a file, or a model asked about this page. `<`, `>` and `&` are therefore escaped
as `\u003c` and friends, which `JSON.parse` reads back as the same characters and the HTML parser does
not see at all. This is not a precaution against a hostile model; it is the ordinary case of a
conversation containing HTML, and the failure mode is that the rest of the conversation is parsed as
markup in a file somebody was sent.

**The page's documents are the API, and a caller redirects an artifact.** With `--out` the page is
written there and stdout carries one line naming it; without `--out` stdout is the page and nothing
else. That is `--json`'s rule — a mode either owns stdout or owns none of it — because a caller who has
to strip a banner out of an artifact is a caller who will strip the wrong line one day. It joins
`--archive`, `--delete` and `/import` in needing no key and no reachable endpoint: it reads one file and
writes another, and the machine where the provider is in doubt is exactly where a conversation is worth
handing to somebody. No messages means no page, refused the way `/import` refuses an empty file: one
that looks like a conversation and holds nothing cannot be told from one that failed to load.

## A directory is opened where it lives, and read only where opening is refused

**A press on a directory is a request to go there, not to read it.** This section first said a directory
is a reading, and the listing route below is what came of it. A person using it said otherwise, in one
sentence: *pressing it should open the directory with the machine's own way of opening one, rather than
previewing the files in it*. That is the right reading of the gesture — every desktop surface opens a
directory on a press, and a panel of file names is what a person asks for when they want to read, not
what they mean when they press — so the press now goes to `POST /open`, and a run that may not start a
program (the guard refuses it with `409`) answers with the listing instead. The two are not two features:
the listing is what is left of a reading when the machine will not open anything, which is why the route,
its test and the panel's drawing of it all survived the change.

**A directory is still read as data, so `GET /dir` answers with a listing rather than with prose.** `GET
/file` refused a directory by its own rule — "is a directory, not a file" — which is true and useless to a
person who pressed the path to go there, and to a page that then had to decide what to draw. The three
ways out were weighed. Drawing the refusal with an `open` button is what the press does now. Parsing the
run's own `list` output in the page was refused for this repository's oldest rule about the page: it
would be a second place where "what a listing looks like" is decided, and it cannot be done at all for the
case that was reported — `My Projects/` is one name to a person and two tokens to any reader of text. So
the process answers, in a shape the page draws controls from, and the entries' full paths are **joined
here**: the page never puts a separator next to a name, which is the same reason it composes no shell
command.

**The listing and the `list` tool are one function, so they cannot disagree.** `tools::directory_items`
is called by both, the order is the order the tool prints (sorted by the printed line), and the page
shows each entry's `line` as the tool would have printed it. A model reading a tool result and a person
reading the panel are therefore told the same thing; the alternative — a route that re-implemented the
listing for the page — is exactly the drift that the tool and the panel would be blamed for.

**The refusal keeps its sentence and gains a header.** A path whose name ends in a separator is decided
by the *page* without asking anything, because that is the one thing a name can say about being a
directory. Everything else goes to `/file`, whose directory refusal carries `X-Flint-Dir: 1`: the page
acts on the header, and the sentence stays for the person. Matching the words "is a directory" was
refused — that is this page reading prose it wrote itself, which is worse than reading a stranger's
because it looks safe. A page that guesses wrong costs one request, and nothing else.

**A line of the run's own listing is a name, which is where a space stops being ambiguous.** `list`
prints one entry per line as `NAME/` or `NAME  (N bytes)`, and such a line says where the name ends —
so the page's splitter reads it as one name, in the position where relative names are read at all. This
is the same kind of rule as the one that reads `src/web.rs:412` as a path and a line: a known format of
this program's own output, read where it appears. The residue it does not touch — a bare path with a
space in prose, written by a model that did not quote it — is the decision below.

## The page asks the run where a path ends

**A path with a space in it is a question about the filesystem, and the page does not have one.** §18 of
`docs/web-mode.md` stated the residue honestly and called it unfixable: no reader of `C:\work\My Notes`
can tell a name from a path followed by a word, and the scanner cut at the space and drew a button to
`C:\work\My`, a name that exists nowhere. Every page-side answer to it is a heuristic — "a capital letter
starts a name", "look for a known extension" — and each one is a guess dressed as a rule. The run has the
one thing that settles it, so the page asks: `GET /resolve?text=…` with the line from the candidate's
first character, answered with the longest prefix that exists and the **length** of it.

Three parts of that are decisions rather than plumbing. The answer is a length in **UTF-16 code units**,
because the caller's only use for it is a JavaScript `slice` — an offset in any other unit would be a
number the page cannot use without counting again. The question is asked about the shapes prose does not
make — an absolute path, a UNC share, a `~`, a rooted name — and only when a word follows it, because a
relative name in a sentence is two words far more often than it is a directory, and a request per
plausible-looking line is a request per line. And the answer is **cached per text and asked once**: the
page repaints on every token of a streaming answer, so without that cache a single sentence would be a
dozen requests to a run that is busy writing it. A refusal is cached too (`null`: asked, and nothing
there), and the token keeps its own reading until an answer arrives — the honest failure, stated rather
than hidden, is the same one §18 described, and it is what a page with no run behind it (an export, a
dropped session file) always shows.

## A cursor is a position in a file

The browser's reconnect cursor was a **frame count minted by the process** (`Live::next`), and that was
wrong in a way that only shows up when something restarts. A count starts at 1 in every process, so a
cursor means nothing in the next one; the ring answers only what it still holds, so a gap larger than it
is answered with `reset`; and the page rebuilt its whole transcript from `/session` on *every* reconnect
rather than only when it had to, which is what a two-second network blip cost — the reader's place, and
any answer still streaming.

It is now the **length of the session file** at the moment the frame was pushed, carried on every frame's
SSE `id:` line and reported by `GET /session` as `X-Flint-At`. The decision is not "use bytes": it is
that a cursor should name a position in the only thing here that outlives the process that made it. The
session file is append-only and is already the truth about the conversation, so a position in it is a
fact two processes can agree on, and `flush`-free: nothing has to be written anywhere to keep it true.

Three things follow, and the third is the one that took the work.

**The ring stays, as the fast path, because it holds what the file does not.** The deltas of an answer
being streamed are in no file yet. So the ring answers whenever it still holds every frame written at or
after the cursor, which is the ordinary case of a page that dropped for a second, and it replays frames.
The file answers when the ring cannot: the entries after that position go out as `event: file` frames,
and the page applies them as the file's lines. `reset` is left for a cursor neither source can place.

**A cursor names a conversation as well as a position, so the client sends both.** A position in a file
and a position in another file are the same number, and `/resume` moves the run between files while a
page may be disconnected and never see the `reset` frame that would have told it. So `?session=<id>`
travels beside `?last=`, and a mismatch is a reload rather than somebody else's lines in the middle of the
transcript.

**What was refused, and why.** Writing a line number or an entry id *into* the format — that is a change
to a hand-editable file to serve a browser, and a position needs no such change: it is derived from the
file's own newlines, so a hand-edited file degrades to "start at the next line boundary" rather than to a
dangling pointer. Keeping a durable log of the frame *stream* — that is the derived state this repository
does not keep, and the file is already a better log. And a page that persists its cursor in
`localStorage` — nothing would read it: a restarted flint listens on a new port with a new token, so the
page has nowhere to send it. That last one is the honest limit of the feature rather than a decision
against it; the server half now exists, and the client half needs a run that will accept a cursor it did
not mint.

## The level is flint's, the field is the endpoint's

Asking a model to reason more is four words and a JSON key, and the two halves belong to different
people. The words are flint's — `off`, `low`, `medium`, `high` — because a ladder the person has to
learn once should not change shape when the provider does. The key is the endpoint's, written in that
endpoint's own provider table as `thinking_field`, because that is the part vendors actually disagree
about: `reasoning_effort` for OpenAI-compatible servers, something else for the vendors that spell it
their own way.

The alternative was read first and refused. Pi keeps a per-provider compatibility table naming the
format (`reasoning_effort`, `openrouter`, `deepseek`, `together`, `qwen`, `chat-template`, …) with one
comment saying outright that "Grok models don't like `reasoning_effort`". That is a census of other
people's servers, kept in step by hand, and every entry in it is a guess about an endpoint flint cannot
see. A field *name* in the config is one string the person who knows their endpoint can set, and the
config is hand-editable — which is the whole reason this repository keeps its state in text.

**Nothing is sent unless both halves are set**, and that is the load-bearing rule. `thinking = "high"`
with no `thinking_field` sends exactly the request body flint sent before this existed, and says so out
loud (the flag warns, `/thinking` and `/config` name the field, and the run's own file records the
level). A field flint guessed wrong is not a preference that gets ignored: it is a request an endpoint
may refuse outright, in the middle of a turn, for a reason nobody watching could connect to a config
key. Guessing is therefore the one thing this does not do.

`off` is the default and it means *ask for nothing*, which is not the same as telling an endpoint to
reason less. flint does not know the word for that on any given server, and a wrong word is worse than
silence — so `off` is documented for what it is: the endpoint's own default applies, and for some models
that default is reasoning *on*. Naming the limit is the honest version of the feature; inventing a
`"none"` that half the servers reject is not.

Three smaller decisions came with it. The level **travels with the conversation** (an appended
`thinking` line, last one wins) rather than living only in the config, because a choice made in a
conversation is part of it — that is the same argument as `schema`, and the same mechanism. The *field*
deliberately does **not** travel: it is a fact about the endpoint, so a conversation resumed against
another provider asks in that provider's field with the level the person chose. And levels a model does
not have are **not** hidden: flint cannot know which rungs an endpoint supports without asking it, and a
menu that quietly drops one is a menu that lies. Four words, offered as they are.

## A rebuild of the agent is not a new run

Three commands replace the agent around a conversation that stays where it is: `/model` and `/provider`
(a different endpoint), and `/reload` (a config file somebody just edited). One function builds the
replacement, and what it is allowed to forget is the whole question — because the replacement is not a
new run, it is the same run asking somebody else, and every fact the person decided about *this run*
has to be handed over by name. `Agent::new` builds each of them fresh from the config, and none of them
is a config key: the read-only guard, the working directory, whether this run keeps a conversation, the
size of the last prompt, the reasoning level, and the answer shape.

**The rule is that the list is closed by the funnel, not by the command.** Every one of those is read
off the old agent in the one place every rebuild passes through, so a command cannot forget one — it
never had to remember. What the rule cost when it was not followed is worth keeping, because the failure
is silent in the worst way. Measured on 2026-09-23, from a report that a settings screen opened on `off`
for a run whose preset was `medium`: `/model` came through the funnel, and the reasoning level was not
on the list, so picking another model dropped the person's preset in the *run* — not on the screen. The
settings screen was telling the truth about a run that had stopped being the one they configured, and
the requests that followed stopped asking for reasoning. The answer shape was lost the same way, and its
ending is worse: a caller promised JSON gets prose, after a model switch, with nothing saying so.

Two placements fell out of fixing it, and both are decisions rather than tidiness. The **reasoning level
and the answer shape** are carried in the funnel function, because only the three commands that keep the
conversation come through it — `/new` and `/resume` really do move, and the shape belongs to the
conversation, so the new one's own file decides (`/new` correctly drops it). The **peer-relay decision**
is carried one level up, in the REPL's rebuild arm, because `--hear-peers` is about the process and
`/new` does not end it: two homes for one carry is how one of them comes to be missing a door.

The level carries and the *field* does not — the same split as the section above, one layer down.
`thinking = "medium"` surviving a `/provider` switch means the run still asks for medium; the key it
asks in is the new endpoint's, because a fact about somebody's server cannot travel with a person's
choice.

## The calls of one message run together, and the report stays in order

The model asks for several tool calls in one assistant message. flint used to run them one after
another, in the order asked, and the question item 12 of the reading of Pi raised is why not at once.

They are at once now, and the shape of the decision is that **the unit is the message**, not the step
and not the tool: what the model asked for together is what runs together. It asked for all of them in
one breath, so it is already waiting for all of them, and running them one at a time spends waits
nobody asked for. The step stays sequential above that, because a step is what the model has not seen
yet — the next request cannot be composed until this one's results are in the history.

Two things had to be true for this to be a small change rather than a redesign, and both were already
true. `Tools::invoke` takes `&self` and keeps what it must remember behind a `Mutex`, so the calls
share the tool set and not their answers. And the read-before-mutate gate is what makes concurrent
writers safe: two calls that write one file cannot lose each other's work, because the second one is
**refused** — the file changed since that call read it. That is the decision worth writing down, since
it is the one Pi needs a file-mutation queue and a per-tool `executionMode: "sequential"` for: flint
gets it from a gate it already had, and gets the better half of it, because a refusal is something the
model can read and act on while a hidden serialization is not. No tool-by-tool rule about what may run
at once was needed, and adding one later would be a second thing to keep true.

**The report stays in the order the model asked**, and the display does too. Only the waiting is
concurrent, and no line of a transcript ever carried the waiting, so nothing is lost by finishing one
sentence before starting the next. The alternative — printing each result the moment it finalizes, as
Pi does — would put the transcript in completion order, which is a race made visible in the one
artifact a person reads and greps; the pairing of a call with its result is the only structure the
transcript has.

What is deliberately **not** done: cancelling the siblings when one call fails. A failure is
information about one call, the model asked for all of them, and killing work somebody is paying for
because a different command exited non-zero is a decision flint should not make silently. The evidence
that this is real is a test and not a clock: the first command waits two seconds for a file only the
second one writes and exits non-zero on its own if it never appears, so a sequential loop cannot pass
— and it was watched failing exactly that way, with that exit code, before the calls were joined.

## A fold is a position in the file, never a count

Pi records a compaction as an appended entry carrying the summary and `firstKeptEntryId` — the id of
the first entry the fold keeps. flint has no entry ids, and minting some would be a change to the
format in service of one feature: an id is derived state that has to be kept unique by something and
repaired by hand when somebody inserts a line. What a session file already has, always, for free, is
**byte offsets**. So a fold is `{"type":"compact","summary":…,"from":<byte offset>}` and `load` drops
every `chat` line that starts before it. That is the same idea as the page's reconnection cursor (§15
of `docs/web-mode.md`): a position in a file is the only identifier this project needs, and the second
feature that needs one is the argument for the first.

A **count** was the other candidate, and it is worse in a way that only shows up later: "the last N
messages" is not the same set twice once anything is appended, so a resumed conversation would fold a
different window than the one that was summarized. A position does not move. It also degrades honestly
— a person who edits `from` by hand gets exactly the fold they asked for, including a pointer past the
end (fold everything) and a deleted line (fold nothing) — which is the standard every other part of
this format is held to.

Two smaller decisions came with it. The cut is always the newest **question**, never a message index
and never a tool result: a request whose tool result was summarized away, or whose call was, is a
request the provider rejects, and a boundary inside a tool exchange is exactly the boundary this
format cannot see. And the summary is inserted as one framed `user` message rather than as a second
system message, because a system message in the middle of a conversation is accepted by some endpoints
and refused by others; the framing (`[the conversation before this point, summarized by flint at the
person's request]`) exists so a model does not answer the summary as though the person had just said
it.

**`/compact` spends a request, and nothing spends one for you.** The summary is written by the model,
because a fold written by dropping the oldest third of a conversation loses the decisions and keeps
the pleasantries; that costs one request, so the command says so before it makes it. The request
carries **no tools**: a summarizer with tools is a turn, and the one thing a request the person did not
compose must not be able to do is act. Automatic compaction — Pi triggers it on a token budget — is
refused for the reason the job report is: starting a model turn nobody asked for is a bill nobody
agreed to, and the number a budget would be checked against is a guess about an endpoint flint cannot
see. `max_request_chars` remains what it always was, a last-resort guard that drops whole turns from
the *request* when a conversation has grown past it; the fold is the deliberate version of the same
thing, and the difference is who decided.

What a fold never does is shrink the **record**. Every folded message stays in the file, in order,
above the line that folds it, and deleting that line undoes the whole thing. The asymmetry is the
feature: the request is the thing with a price and a limit, and the session file is the thing a person
has to be able to read, quote and repair. A fold that deleted messages would make `/compact` the only
flint command that destroys work, and it would make the summary the only surviving account of a
conversation — which is the kind of derived state this repository refuses to keep.

## Turns, and the lines typed into them

**A queued follow-up is the run's memory, not the file's.** `/queue <text>` holds a line until the turn
that is running finishes; the alternative was an event (`queued`) in the session file, which is what the
reading of Pi describes, and it is wrong here for the reason `--json` is a view and not the record: the
file is what was **asked**. A line that has been typed and not sent has not been asked, so writing it
down would put a message in a conversation that never sent it — and reading it back on `--resume` would
hand a sentence somebody typed an hour ago to a model that has never heard of it, in a conversation that
has moved on. What the file gets is the message, at the moment it is sent, which is also when it gains
its place in the request. The cost is stated rather than hidden: a queue dies with the run (`/exit`,
Ctrl-D, a kill), and the transcript's `queued for after this turn: …` line is the record that it was
ever typed.

**A stop ends the answer in flight, not what was typed behind it.** Pi's rule, and the one worth
copying: aborting *continues* the messages still queued, so `/stop` in the middle of a turn leaves the
follow-up queued for it to be sent next. The alternative — clearing the queue on a stop — makes the
stop mean two things, and the second one is invisible: a person who types a correction and then stops
has two intentions about two different turns, and only one of them was about the turn in flight.

**There is no `clear_queue`, and dropping a queue is said out loud.** Pi's client discards queued
messages by taking the text back into its own editor, so nothing is lost by the act. flint's prompt is a
line from a terminal with nowhere to put the text back to, and the spellings that suggest themselves
(`/queue clear`) collide with a message whose text *is* that word. So the discarding that does happen is
not something a person asks for: it is what a chain of turns ending without sending the queue means, and
there are exactly two such endings — a command typed mid-turn (which hands the line back to the REPL and
ends the chain, because only one thing can be the next prompt) and a turn that ended with an error. Both
print a sentence naming how many queued lines were dropped and why. A queue that vanished without one
would be the failure this feature exists to remove, one step further on.

**With nothing running, `/queue <text>` is this turn's message.** There is no turn to hold it for, and
the alternative — refuse and make the person retype it — is a command with a rule about *when* it may be
said, whose enforcement is a line of output that could have carried the sentence instead. The note under
the echo says which of the two happened, because "queued" and "sent" look identical in a transcript
otherwise.

## Context: instructions and skills

**`instructions = "hint"` is the default.** flint names the instruction files it found and
lets the model read them, rather than pasting their contents into every request. Pasting
spends the context budget of every turn on files that most turns do not need; `paste` and
`off` remain one config edit away.

**Discovery runs outermost-first and stops at a boundary.** The home-level `AGENTS.md` first,
then upwards from the working directory, stopping at `.git` or the home directory. A file
higher up is more general, so it belongs earlier in the prompt, and a repository is a
boundary rather than a suggestion.

**Skills are one level deep, and the project wins a name conflict.** `<project>/.flint/skills`,
then the working directory's, then the config directory's, then `skill_dirs`. A deeper tree
would need a discovery story and a conflict story; one level needs neither.

**The `skill` tool does not exist when there are no skills.** A tool that can only fail is
worse than no tool: it costs tokens in every request and teaches the model that its tools
lie.

**Nothing about context is cached.** Not the instruction files, not the catalog. `/reload`
re-reads them, and a file written mid-session takes effect on the next turn.

**A saved prompt is the person's, and stays out of the model's prompt.** `.flint/prompts/*.md`
and `<FLINT_HOME>/prompts/*.md` are found in the same three places, in the same priority order,
and one level deep like the skills — but unlike the skill catalog, no line about them enters the
system prompt and no tool schema mentions them. A skill is offered to the model because the model
is the one who has to decide to load it; a template is only ever sent because a *person* typed its
name, so putting it in the prompt would spend tokens on every request of every conversation to
answer a question nobody asked. The cost of that choice is discoverability, and the answer is one
command: `/prompts` lists what was found, where, and what each is for.

**Arguments fill a hole or are appended, and never silently vanish.** `{args}` in a template is
replaced where the author put it; with no `{args}`, what was typed after the name becomes a last
paragraph. Both are one function (`context::fill_args`) so a prompt file and an invoked skill
cannot drift, and an empty argument is not an error: the file is sent as written. The alternative —
appending always, Pi's `User: <args>` — reads well until the author wants the words in the middle
of a sentence, and the alternative of substituting only when a hole exists is what "aim this saved
prompt at something else" means for the file somebody wrote in thirty seconds.

**Typing `/<name>` is the same act as `/prompt <name>`, and a command always wins the name.** The
dispatcher looks at a saved prompt only after no built-in command matched, so a template called
`help` loses to `/help` rather than shadowing it. A person's own files getting a namespace that can
override the commands they depend on would be a surprising thing for a feature whose whole promise
is "save this and type it again"; the second spelling (`/prompt <name>`) exists because a page's
menu can send a fixed command plus one value and cannot type `/<name>`.

**A skill has two doors, and reading is not one of them.** `/skills <name>` prints the body and
`/skill <name> [args]` *sends* it as the person's own next message — the same bytes the `skill`
tool would return, through the same loader, so "did it send what I wrote" has one answer. The
transcript keeps the line the person typed (`> /skill tidy-commits the parser`) with one dim line
naming the file underneath, and the session file and the request carry the expanded text: the echo
is for the person, who already knows what they saved, and the file is the record of what was
actually said.

## Tools

**Read before you mutate.** `write`, `edit` and patch updates refuse a file this run has not
read, and refuse one that changed on disk since it was read (mtime and length). The record
lives in memory only — what this run has looked at is a fact about this run, and persisting
it would be exactly the stale derived state the project forbids. Three deliberate holes: a
file that does not exist yet can be written, a file this run wrote counts as read, and
`bash` is not gated at all. That last one is the honest part: flint cannot both allow `bash`
and pretend writes go through a gate. **This is a guard against mistakes, not a security
boundary**, and `AGENTS.md` says so in those words.

**Long output keeps both ends and spills the rest to a file.** The head is where the command
and the first errors are; the tail is where "build failed" and the summary are; the middle is
what a person would skim. The whole output goes to `~/.flint/spill/<session>/<n>.txt`, plain
text, named in the reply — `[truncated]` alone used to throw away the line that mattered.

**What a request carries is bounded, and the transcript is not the request.** Three separate
caps, because they protect against three different things: `max_tool_output` bounds one tool
result (the rest spills to a file), `prune_tool_output` shrinks the stale results *inside* a
turn before it is sent, and `max_request_chars` (400,000 characters, `0` turns it off) drops
the oldest *turns* of a conversation past it. All three are request-side only: the session
file keeps every message, which is what makes a dropped turn something that can still be
read, be named in a note, or be recovered by resuming — and it is why `flint debug
prompt-input` is a command rather than a debugging session. The unit of the third one is a
turn because a tool result separated from the call it answers is a request the provider
refuses, and the newest turn and the system prompt are never dropped because a request with
nothing to answer is not a smaller request. Pruning runs before trimming, so the cheap bound
is spent before the expensive one.

**A repeated call is noted, never blocked.** The 3rd, 5th and 8th identical call in one turn
gets one line saying so, and the note is attached to the result so the *model* sees it. A
repeat is sometimes right — another process may have changed the file, a command may be
genuinely slow — so flint informs instead of refusing. The count is per turn: a repeat in a
new question is a different thing.

**Argument validation distinguishes absent from mistyped.** "Missing required string
argument 'path'" for a `path` that was sent as a number sends the model to fix a problem it
does not have. Optional arguments matter more: read as absent, `"limit": "5"` silently
becomes "no limit", and the instruction disappears with nothing said.

**`apply_patch` locates hunks by their own lines, and is all-or-nothing.** No line-number
arithmetic to get wrong, and no way to land a change in the wrong place because the file
grew since it was read. Several files in one patch are one edit, so a hunk that fails in the
third file leaves the first two untouched. One section per file, because two sections would
both be planned from the same contents on disk. No `*** Move to:` and no fuzzy matching: a
rename is a delete and an add, and a patch that does not fit is a patch that needs rewriting
against the file as it is.

**`readonly` is all-or-nothing.** There is no middle setting, because a middle setting that
looks safe and is not is worse than a switch whose meaning is obvious.

## Output

**A redirected run emits no escape codes.** `flint --help | less`, `flint exec ... | grep`
and every pipeline depend on it, and it is easy to break in one place while everything else
stays clean — which is why `tests/cli_output.rs` counts ESC bytes in the real binary's
output rather than trusting the code paths.

**`--json` owns stdout, or it owns nothing.** One JSON object per line, no banner, no status
row, no trailing prose. The vocabulary is closed and the shape is asserted on bytes: a tool
result containing newlines, quotes and escape codes is JSON-escaped, never printed raw,
because a reader splitting on `\n` must not be able to cut an object in half.

**NDJSON keys are in a stable order rather than a chosen one.** Putting `type` first would
mean `serde_json`'s `preserve_order` feature and an `indexmap` dependency. Stable output —
two runs of one conversation compare with `diff` — is worth more than any field being first.

**The stream is a view, not the record.** The session file is written exactly as in any other
run, and `session.started` names it. Nothing in the stream is the only copy of anything, and
a failure is described on the stream as well as by the exit code, so a caller reading stdout
does not have to read stderr to learn that nothing is coming.

**The status clock belongs to the round, not to each announcement.** Every tool call in a
round is announced before any of them runs; naming the clock on each announcement claimed the
last call was the one being waited for. The clock therefore starts on the first call, moves
to the next as results arrive, and runs nameless — "waiting for the model" — between rounds.
Committing a transcript line does not repaint it: a fast tool used to flash `0s`.

**A captured run writes to a file instead of taking over stdout.** The capture tests used to
redirect file descriptor 1, which also collected the test harness's own progress lines; one
landing on the bottom row scrolled the transcript off the recorded screen and made a passing
run look broken. `FLINT_TERM_CAPTURE_FILE=<path>` is the recording, and no test moves a
descriptor.

**The cache split is `Option`, and the rate is derived rather than stored.** Two endpoints, two
field names, one number: DeepSeek reports `prompt_cache_hit_tokens`, OpenAI reports
`prompt_tokens_details.cached_tokens`, and both mean "a subset of the prompt the provider had
already seen", so `provider::usage_from` collapses them and nothing downstream knows which
endpoint answered. The field is `Option<u64>` because *not reported* and *reported zero* are
different facts and only one of them is about the prompt flint builds — an endpoint that says
nothing about caching printing `0% cached` would accuse the prompt of being unstable, which is
exactly the reading the number exists to make trustworthy. The percentage is computed at the
point of printing (`Usage::cache_rate`) rather than stored in the file: it is arithmetic on two
numbers that are both there, and a stored rate is a number that can disagree with its own
inputs after one of them is hand-edited. It refuses to divide by a prompt of no tokens and
bounds itself at 100 — a provider reporting more hits than tokens is wrong, and repeating that
as `130%` would spread the mistake.

**The rate is printed where the counts are, not on a screen of its own.** `/usage` puts it on
the same line as the prompt it is a share of, and the footer under every answer appends
`, 87% cached` — because the reason to have this number is to notice it *move* between turns,
and a number nobody sees while working is a number nobody notices. The file's `usage` line and
the `--json` frames carry the raw `cache_hit_tokens` and never the rate, so a program can
compute a rate over its own window instead of over one request.

**A conversation you were given is copied, never continued in place.** `/import <file>` exists because
`--resume <path>` is the wrong thing to do to a file somebody handed you: resuming writes into it, so
it grows, it gains this machine's `usage` lines, and its `meta` still names their working directory —
the file you were given stops being the file you were given. So the act is a *copy* into this run's own
sessions (the same act `--fork` performs for a conversation this run is already in) and the source is
not written to at all. That is also what makes hand-editing a first-class way to work: a file written
in an editor, with no `meta` line at all, is a conversation this run can pick up, and the copy is the
thing that keeps going.

**An imported conversation says where it came from, on a line of its own.** The record is a session
*event* rather than a field on `meta`, for the reason `title` and `switch` are events: the file is
append-only, and a fact that arrives at a moment is a line — and a copy of a copy then reads as a chain
of files rather than as one original. It is written **above** the conversation it describes, so a file
read top to bottom answers "where did this come from" before "what was said". `from` is the path as it
was given and not a pointer to follow: the file may have moved, or never have been on this machine, and
a copy that could not be read without it would not be a copy. The record is *read back* — the startup
`resumed` line and `/resume` both append `, imported from <file>` through one function, so two doors
cannot drift into saying different things about the same file — because a line nothing consumes is the
fault this repository calls damage: `Loaded::last_usage` sat unread in exactly that way until this
round's other half fixed it.

**A file with no conversation in it is refused, not imported as nothing.** An empty file, or one whose
lines are all events and no messages, would otherwise answer "imported" with a transcript of nothing
and leave a session in the list that nobody could tell from a real one — the same failure the
`children/` directory exists to prevent, arriving by a different road. The two other refusals are the
same shape of decision: importing the file this run is currently writing would copy a growing
conversation into itself (`--fork` at startup is that act), and `--no-session` refuses `/import` like
every other door that would create a conversation, because creating one is exactly what the flag
promised not to do.

**A number that is in the file is read back out of the file.** The counts a resumed
conversation reported were parsed into `Loaded::last_usage` and then dropped on the floor: the
field had no reader, so `/usage` on a conversation somebody came back to answered "no usage
reported yet by this provider" while the line it wanted sat in the file it had just opened, and
`/model`, `/provider`, `/reload` and `/readonly` — every path that rebuilds the agent around a
conversation that stays — lost the numbers the same way. `Agent::set_last_usage` is the one
line that carries them across both kinds of replacement, and it exists because the alternative
(a field that is written and never read) is the kind of quiet wrongness this project counts as
damage even when nothing crashes.

## Tests

**No test ever uses a real API key.** The stub is `wiremock` replaying hand-written SSE
frames, including tool arguments split across frames. It is the only way to test the retry,
streaming and tool paths deterministically, and the only way a machine with no key can run
the suite at all.

**Terminal assertions are on real bytes.** `FLINT_TERM_CAPTURE=1` plus
`FLINT_TERM_CAPTURE_FILE` and `FLINT_TERM_SIZE` (debug builds) force the interactive path
into a file that `scripts/vtscreen.js` replays as a screen. A test that asserts on a
reimplementation of the renderer tests the reimplementation.

**Fixtures are built with `serde_json`, not by hand.** A tool call's arguments are a JSON
string inside a JSON string, and a Windows path is the case where writing that by hand goes
wrong silently — the fixture then measures a tool that never ran. One fixture written by
hand cost an hour of chasing a "bug" that was in the test.

## Dependencies

Ten runtime crates, and a new one needs a reason that survives being written down. What that
has bought so far: no YAML crate (the front matter of a `SKILL.md` is parsed by hand), no
date library (`epoch:<seconds>`), no TUI framework beyond crossterm's raw mode, no
`preserve_order`, and a `reqwest` configured with rustls rather than pulling in OpenSSL.

**The loopback listener is hand-rolled, not `hyper`.** This was `docs/web-mode.md` §7, the
one question that document deliberately left open, and it is recorded here because it is a
dependency decision and those live in one place. `hyper`, `hyper-util`, `http-body-util` and
`tokio-util` are **already in the lock file**, pulled in by `reqwest`, so the usual argument
against a dependency — build time, binary size, a longer supply chain — does not apply. The
argument that does is API surface: the routes are four, two of them are a string comparison
each, and `Connection: close` removes keep-alive, pipelining, chunked encoding and request
framing from the problem entirely. What is left is reading to a blank line and writing a
status line. Enabling `tokio`'s `net` feature for it compiles nothing new either: `mio` is
already there through `process` on Unix, which `Cargo.lock` confirms by not changing.

The counter-argument is real and is not waved away: HTTP has more corners than the `SKILL.md`
front matter this reasoning is modelled on, and this is the one place in flint that listens
on a socket. If a route ever needs framing this file does not have, the answer is `hyper` and
not a hand-rolled chunked decoder.

## Why this program, next to Pi

On 2026-09-17 another agent harness was read from end to end: **Pi** (pi.dev,
`github.com/Earendil-Works/pi`), the minimal TypeScript harness by Mario Zechner. It is the
same kind of program built on the opposite bet — one agent loop, a handful of tools, sessions
on disk, a terminal UI — and [`pi-agent-harness.md`](pi-agent-harness.md) is the record, with
every claim sourced from a page that was loaded. It is kept because "why is this not just
somebody else's program" is a question worth answering in writing, and because a second
reading of the same problem is the cheapest test this design will ever get.

**It agrees with flint on more than either program would probably guess.** No permission
layer; plans and to-dos as files; append-only JSONL sessions a person can read; writing to
the scrollback instead of taking over the terminal; progressive disclosure for skills; a
system prompt measured in hundreds of tokens; no MCP; tool output that must truncate and say
where the rest is. Ten years of convergent evolution in two programs written by two people who
have not met is evidence about the *problem*, not about either program: an agent harness wants
plain files and small prompts, and the field will keep proving it.

**The overlap is the commodity layer, and the difference is the part that cannot be
imported.** Pi refuses, in writing and on purpose: background work (its answer is tmux),
sub-agents ("a black box within a black box"), a permission layer, a plan mode, built-in
to-dos, and MCP. Three of those refusals are things flint has and uses: **jobs**, with one
record, a listing, an exit code, a panel and a stop; a **`task` child whose conversation is a
file** a person can read, resume and move up a level; and **doors**, so that a page, a
`--json` caller, an MCP client and Python all drive the same run. A loop and a session format
are a weekend's work in any language; those three are the reason this one exists, and they
are exactly what is argued against there.

**What the reading changed here, and what it did not.** It added `ROADMAP.md`'s "Taken from a
reading of Pi" — twelve agreed items, none of them a new bet, all of them cheap — and it
declined one, a run-level tool allowlist. It changed none of the rules below. Two of them are
worth restating with Pi as the test:

- **Nothing is derived, and the state is hand-editable.** Pi arrived at the same rule
  independently: a JSONL tree, entries appended rather than rewritten, even modal state
  (which model, which thinking level) written as entries so the setting in force at any point
  is recoverable from the path. That is a good sign for the rule, and the only place the two
  part company is how much is written back: flint's request-side pruning leaves the file
  untouched and says so with a count.
- **Dependencies are the enemy.** Here the programs genuinely diverge. Pi is a TypeScript
  monorepo on Node, with npm-installed extensions, a generated model registry and a
  per-vendor compatibility table for four wire protocols. Flint is one static binary and ten
  crates. That is the bet this file is about, and it has one consequence that is written down
  rather than discovered later: **flint will never match Pi on breadth of providers, and if
  breadth is what somebody needs, Pi is the better program.** Flint is for the machine the
  person is actually on, for seeing and stopping the work a run actually started, and for
  files that can be repaired with a text editor.

**What would make this the wrong bet.** If the machine stops mattering (every host with Node
and tmux on it), if the doors go unused, or if the work a run starts is genuinely better
observed by an external multiplexer than by the program that started it. None of those is true
here today, and the way to find out is the list in `ROADMAP.md` rather than an argument in
this file.

## Not doing

Subagents, a permission layer, `flint doctor`, MCP (deferred rather than refused), indexes,
caches, databases, compression, a PowerShell parser, and `-EncodedCommand`. The reasoning for
each is in [`ROADMAP.md`](../ROADMAP.md#not-doing-and-why); the short version is that each
one adds a second thing that can be wrong, and flint is meant to be the program that is still
working when the others are not.
