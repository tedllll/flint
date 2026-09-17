# The session file format

A conversation is one file, `~/.flint/sessions/<dir>/<id>.jsonl`, and it is **append-only**: flint
adds lines and never rewrites one. That single rule is what makes everything else here
possible — a rename costs one line, a killed process leaves a readable file, and a person
with a text editor can repair a conversation without a tool that understands it.

`<dir>` is the directory the conversation was held in: its last path component, lower-cased
and cleaned up, then a hash of the whole canonical path — `flint-1f0a7c93`. The readable part
is so that a person looking at `sessions/` can see whose conversations are whose; the hash is
so that two projects whose last component matches (`D:\work\api` and `E:\work\api`) do not
share one. The directory itself does not move when a project does: a session records the
working directory it was held in on its `meta` line, and that recorded path — not the name of
the directory it sits in — is what `--continue` believes. Sessions written before this
subdirectory existed sit directly in `sessions/` and are still found, which is why the reader
looks in both places. A conversation another run started is one level further down, in
`sessions/<dir>/children/`, and the reader looks there for nothing: that is what keeps a
child's conversation out of everybody's list of their own. See `parent` under `meta` below.

The id is the file name: `<seconds>-<milliseconds>-<pid>`, e.g. `1789290356-957-41232.jsonl`.
Seconds first so ids sort by time, and the process id last so two runs that start in the same
millisecond cannot propose the same name — which they did: six concurrent calls from
`examples/python/flint_call.py` wrote five conversations into one file before the pid was in there.
Nothing else identifies a session, so `cp` is how you fork one and `mv` is how you rename the file.

The file is created by the **first event in it**, not when flint starts. A run that says
nothing — the page opened and closed again, a `--continue` that found no conversation — leaves
no file behind, and a run refused before it says anything (no key, an endpoint that cannot be
reached) leaves nothing either. That is why `meta` is still the first line of every file that
exists: whoever creates the file writes `meta` into it first, and a file whose first line is
not `meta` is a conversation with no model and no working directory attached to it. Creating the
file is also what claims the name (`create_new`): a name that turns out to be taken moves that
writer to the next millisecond rather than appending to somebody else's conversation, and the
`id` it writes moves with it, because the id in the file has to be the id *of* the file.

**A run can be told to write none of this.** `--no-session` means exactly that: no file is created, so
there is no `meta`, no id, and nothing for a listing to find — and no way to continue from it later,
which is the point. What such a run still writes is the state it needs to work: spilled tool output and
a background command's log, under `~/.flint/spill/unattached-<pid>/` rather than under a session's name,
because there is no session name to use and two of these runs would otherwise share one directory and
overwrite each other's `1.txt`. The flag travels to a `task` child as well, which is the one
conversation such a run could otherwise create by itself.

## One event per line

Every line is a JSON object with a `type`. This build understands nine:

| `type` | Written when | Fields |
|---|---|---|
| `meta` | once, as the first line | `v`, `id`, `created`, `cwd`, `provider`, `model`, `parent` (only when another run started this one) |
| `chat` | a message is added to the conversation | `message` |
| `import` | a conversation is copied in from a file (`/import`) | `from`, `from_id`, `messages` |
| `fork` | this conversation is a copy of another one (`--fork`, `/fork <n>`) | `from`, `from_id`, `kept` (only when the copy is a prefix) |
| `peer` | a peer left a message while this conversation was open | `from`, `text`, `at`, `heard` |
| `usage` | the provider reports token counts | `usage` (`prompt_tokens`, `completion_tokens`, and `cache_hit_tokens` when the endpoint reported a cache split) |
| `title` | the conversation is named | `name` |
| `switch` | the provider or model in force changes | `provider`, `model` |
| `schema` | the answer shape in force changes | `schema` (absent or `null` when cleared) |

Three of the nine are not conversation and never become history: `peer` (a peer's words are shown to
the person and kept out of `messages` on purpose — see its section), `import` and `fork` (both
provenance, read into `LoadedSession::origin` and read by the lines that name a conversation). An
**unknown** `type` is skipped in silence, which is what lets a hand-edited file, or one written by a
newer flint, still load; a line that names one of these nine and cannot be parsed is reported as
damage instead.

A whole conversation, then — a real one is longer, this is the shape:

```json
{"type":"meta","v":2,"id":"1789290356-957","created":"epoch:1789290356","cwd":"C:\\work\\dsh","provider":"deepseek","model":"deepseek-chat"}
{"type":"chat","message":{"role":"system","content":"You are flint, ..."}}
{"type":"chat","message":{"role":"user","content":"why does dsh fail to start"}}
{"type":"chat","message":{"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"dsh --version\"}"}}]}}
{"type":"chat","message":{"role":"tool","tool_call_id":"call_1","content":"1.2.3\n"}}
{"type":"chat","message":{"role":"assistant","content":"It starts fine; 1.2.3 is the current release."}}
{"type":"switch","provider":"deepseek","model":"deepseek-reasoner"}
{"type":"chat","message":{"role":"user","content":"and what changed in it?"}}
{"type":"usage","usage":{"prompt_tokens":1204,"completion_tokens":88}}
{"type":"title","name":"dsh start failure"}
```

### `meta`

```json
{"type":"meta","v":2,"id":"1789290356-957","created":"epoch:1789290356","cwd":"C:\\work","provider":"deepseek","model":"deepseek-chat"}
```

`v` is the format revision, and it is the only field with a default: a file that does not
mention it is **v1** — a line with no `v` key at all, which is every file written before a
conversation could be named. That default is the reason a hand-written or hand-trimmed file
still loads: requiring the field would turn every file from before the change into an
unreadable one.

`created` is `epoch:` followed by Unix seconds, not a date — enough to order sessions and to
tell a person when one was written, with no date library behind it.

`id` is the same string as the file name. `cwd`, `provider` and `model` record the
environment the conversation started in, which is what makes an old session answerable
later: "which model said that, and from where?" — **where it started**, not where it is
now: `/model`, `/provider` and `/reload` append a `switch` when they move a conversation
to another model, and the last `switch` in the file is the one in force (`--resume` reads
it). Editing the first line to change a session's model would be a lie about where it
began; appending a `switch` is how you say it moved.

`parent` appears on a conversation **another run started** — a `task`/`tasks` child, or the
same thing reached from Python or MCP — and holds the parent's session id:

```json
{"type":"meta","v":2,"id":"1789540741-220","created":"epoch:1789540741","cwd":"C:\\work","provider":"deepseek","model":"deepseek-chat","parent":"1789456770-557"}
```

It is absent (not `null`) on a conversation a person started, so the first line of an
ordinary session is unchanged, and a reader that does not know the key sees a normal file.
It is written from `FLINT_PARENT`, which the `task` tool sets on the child it starts and
which nothing else sets; a person who exports it by hand gets a session filed as somebody's
child.

**Where such a file lives is the other half, and it is the part a person notices.** A
child's conversation goes under `sessions/<dir>/children/` rather than beside its parent's.
It is a session file like any other — same format, readable, resumable by path — and no
listing reads that directory, so `/sessions`, `flint --list-sessions`, the page's sidebar
and `--continue` all show the person their own conversations and nothing else. That is the
same argument the archive makes: the layout does the filtering, so there is no flag to
honour and no filter to keep in step. It matters because a child is *newer* than the parent
that started it, which is enough for "the newest conversation in this directory" to mean the
child's — measured, before this existed: `--continue` resumed a child's session while the
parent was still open. To read a child's conversation, use the path its result named, or the
path in `parent` the other way round:

```
flint --resume "$FLINT_HOME/sessions/<dir>/children/1789540741-220.jsonl"
```

### `import`

```json
{"type":"import","from":"/home/you/Downloads/given-to-me.jsonl","from_id":"1789512345-88-4412","messages":12}
```

Written by `/import` when a conversation is **copied in** from a file rather than started here, and
written **above the conversation it describes** — so a file read top to bottom answers "where did this
come from" before it answers "what was said in it". It is its own event rather than a field on `meta`
for the reason `title` and `switch` are events: the file is append-only, and a fact that arrives at a
moment is a line. Importing a conversation that was itself imported therefore records the file it was
copied *from*, and a chain of copies reads as a chain of files rather than as one original.

- `from` is the path **as it was given**. A fact of the moment, not a pointer: the file it names may
  have moved since, may live on a machine this one cannot reach, or may never have been on this
  machine at all, and a copy that could not be read without it would not be a copy.
- `from_id` is the source's id as flint read it — its `meta` id, or its file name when the file had no
  `meta` at all (the hand-written case). Absent when the source's id could not be established at all.
- `messages` is how many messages were copied. Stored rather than counted later because the copy
  *grows*: this is the size of the import, not the size of the conversation.

Everything else in the copy is ordinary: the same `chat` lines, the same `title` if the source had one,
and a `meta` line that belongs to this run (this machine's working directory, the provider and model
in force here) rather than to the file it came from. The source is not written to at all, which is the
difference from `--resume <path>`: that continues *inside* the file it was handed.

The record is read back: a conversation that came in this way says so wherever a conversation is named
to a person — the startup `resumed` line and `/resume` both print `this conversation was imported from
<file>` on the line under it — because a record nothing reads cannot be told apart from one that was
never written.

### `fork`

```json
{"type":"fork","from":"/home/you/.flint/sessions/C--work/1789512345-88-4412.jsonl","from_id":"1789512345-88-4412","kept":4}
```

Written when this conversation is **a copy of another one**: by `/fork <n>`, which cuts this
conversation at the n-th question the person asked here, and by `--fork <file>` at startup, which copies
the whole thing. Same place as `import` and for the same reason — above the conversation it describes —
so the file answers "where did this come from" before it answers "what was said in it".

The two kinds of copy are one event because they are one act with one difference, and the difference is
`kept`:

- `from` is the source's path **as this run read it**, a fact of the moment rather than a pointer, for
  `import`'s reason: the file it names may have moved, and a copy that could not be read without it
  would not be a copy.
- `from_id` is the source's id as flint read it — the file's *stem*, which is the id its `meta` line
  was created under, so the two agree by construction.
- `kept` is how many messages this copy holds, written **only when the copy is a prefix** of the source.
  An absent `kept` is a copy of the whole conversation (`--fork`); a present one is a cut (`/fork <n>`),
  and it is stored rather than derived for `import`'s reason: this copy *grows* from there, so counting
  its messages later would answer a different question than "where did the branch start".

**This is deliberately not `meta.parent`.** That field means "another run started this one" and is what
files a conversation under `children/`; a fork is a copy a person made of their own conversation, and
writing `parent` would make `/sessions`, the sidebar and `--continue` treat a branch as somebody's
child. It is a `fork` event rather than a field on `meta` because the file is append-only and a fact that
arrives at a moment is a line — and because a copy copies *messages*, not events, so a chain of copies
reads as a chain of files rather than as an original with a paragraph of history above it.

The cut is always at a **question**: a `chat` line from the person, which is where a turn starts. That is
the same boundary `trim_old_turns` cuts on and for the same mechanical reason — a tool result whose call
was dropped is a request the provider rejects — with the addition that it is a boundary a person can
name in the terminal, since it is a thing they typed.

A forked conversation is otherwise ordinary: the same `chat` lines it kept, the same `title` if the
source had one, and a `meta` line that belongs to this run (this machine's working directory, the
provider and model in force here). The conversation it was cut from is not written to at all, and the
record is read back the same way an import's is: the startup `resumed` line and `/resume` print
`this conversation was forked from <file>, and holds the first <n> messages` under the line that names
the conversation.

### `chat`

`message` is one of four shapes, distinguished by `role`, and they are the provider-neutral
form of the conversation:

```json
{"role":"system","content":"..."}
{"role":"user","content":"..."}
{"role":"assistant","content":"...","reasoning":"...","tool_calls":[...]}
{"role":"tool","tool_call_id":"call_1","content":"..."}
```

- `content`, `reasoning` and `tool_calls` are omitted when empty or absent, so a message
  that is only a tool call has no `content` key at all rather than an empty string.
- `reasoning` is the model thinking out loud, when it exposes that. It is kept because the
  transcript does not print it: the file is where you go to read what the model was
  actually doing.
- `tool_calls` on an assistant message is what the model asked for; the matching
  `{"role":"tool"}` message carries the result, tied back by `tool_call_id`. A conversation
  read straight down therefore alternates between what was asked and what came back.
- `arguments` inside a tool call is a **string**, not an object: that is exactly how
  providers stream it, in fragments, and flint parses it once the fragments are complete.

### `usage`, `title`, `switch` and `schema`

```json
{"type":"usage","usage":{"prompt_tokens":1204,"completion_tokens":88}}
{"type":"usage","usage":{"prompt_tokens":1204,"completion_tokens":88,"cache_hit_tokens":1050}}
{"type":"title","name":"dsh start failure"}
{"type":"switch","provider":"deepseek","model":"deepseek-reasoner"}
{"type":"schema","schema":{"type":"object","properties":{"day":{"type":"string"}},"required":["day"]}}
{"type":"schema"}
```

`usage` is whatever the provider last reported, written as it arrives; it is an event rather
than a field on `meta` because the file is append-only and the numbers change every turn.

`cache_hit_tokens` is on the line **only when the endpoint reported a cache split**, and it is
flint's own field name rather than either of the two the endpoints use: DeepSeek's
`prompt_cache_hit_tokens` and OpenAI's `prompt_tokens_details.cached_tokens` are read into this
one number, because they mean the same thing (a subset of `prompt_tokens` the provider had
already seen) and a reader of this file should not have to know which endpoint wrote it. An
absent field is a fact and not a gap: it means this endpoint said nothing about caching, which
is different from `0`, and the rate flint prints (`/usage`, and the footer under each answer)
is derived from the two numbers here rather than stored. Hand-editing one of these lines is
safe in either direction — the field may be added, changed or dropped, and the next run reads
what is on the line.

The counts are also **read back**: a resumed conversation starts with the last `usage` line in
the file already in force, so `/usage` answers with the size the conversation really had rather
than with "no usage reported yet" until the next turn runs.

`title` is a human name for the conversation, and it is an event for the same reason:
renaming appends one line. **The last `title` in the file is the one in force**, which is
why `/name` costs nothing and rewrites nothing. `/name` on a conversation that was already
named appends a second `title`; the first is still there, and still true of the moment it
was written.

`switch` is the same shape of fact about the model: `/model`, `/provider` and `/reload`
replace the agent around a conversation that stays in this file, so the file says when it
moved and what it moved to. **The last `switch` is the provider and model in force**, and
`--resume` acts on it. It exists because the alternative was worse: a switch used to seed a
whole new session with the conversation copied into it, which left one conversation in two
files — two rows in the page's sidebar, two numbers in `/sessions`, and half a conversation
behind either of them.

`schema` is the answer shape the conversation is being held to, written by a run that was
*told* one (`--schema` or `--no-schema`) and not by a run that merely inherited it, so the
file gains a line when somebody decides something and stays quiet otherwise. The whole
schema is in the line rather than a path to the file it came from: a session pointing at
somebody's disk would stop being readable the moment that file changed, and the promise of
this format is that the file is the truth. **The last `schema` is the shape in force**, so
`--resume` holds a conversation to the same contract without the caller passing anything
again; a bare `{"type":"schema"}` is a *cleared* shape, which is what `--no-schema` writes.
The schema in the line is checked when it is read: a keyword this build cannot validate is
refused with the conversation named, rather than an answer being certified against a rule
nobody checked.

### `peer`

```json
{"type":"peer","from":"peer 41288","text":"please do not commit docs/sandbox.md; I am still writing it","at":1789290999,"heard":false}
```

A run in this directory was left a message with `flint say`, and it showed it to its person. `from` is a
claim rather than an identity — whoever can write a mailbox file can write that field — and `at` is a
unix second, or 0 when the sender did not say.

**It is not a `chat`, and that is the whole reason it is an event of its own.** `load` reads this line
and deliberately keeps it out of `messages`, so a peer's words cannot become history: anything able to
write a mailbox could otherwise steer the tool loop of a process that has no permission layer, and a
conversation resumed from this file would relay them again to a run whose person never asked. The
transcript shows the message at the moment it arrived; this line is the record that it did.

`heard` is true when that run was started with `--hear-peers` (or had `/hear-peers on` in force) and
therefore sent the words to the model with its next request — where they arrived as a user message
labelled with where they came from. It is the only place that answers "was the model told this", since a
request body is not kept anywhere, and it is defaulted on read so a file written before the option
existed still reads as what it was: a message no model ever saw.

## The rules a reader must keep

- **Unknown `type` is skipped in silence.** Another build, a newer flint, or you with an
  editor may leave an event this build has never heard of. Ignoring it is what lets the
  format grow: treating it as damage would turn every future version into corruption for
  this one.
- **A *known* `type` that does not parse is damage**, and it is reported rather than
  skipped. The difference matters: a `chat` line that will not parse is a conversation with
  a hole in it, and continuing as if nothing happened is how you get answers that ignore
  something you said.
- **Listing reads only the two ends.** A list needs a name, a date and a size, so it reads
  the head and the tail of each file and nothing else. A conversation of a hundred
  megabytes appears in the list exactly as fast as a short one.
- **Nothing is derived.** There is no index, no cache and no database: the list, the resume
  target and the display are all computed from the files. Delete a file and it is gone; copy
  one in and it appears.

## Working on one by hand

The format is plain text so that it can be repaired by hand. Some things that actually work:

```bash
# What did the model ask for, in order?
jq -r 'select(.type == "chat") | .message.role' ~/.flint/sessions/1789290356-957.jsonl

# The whole conversation as prose, for a bug report
jq -r 'select(.type == "chat" and (.message.role == "user" or .message.role == "assistant"))
       | "\(.message.role): \(.message.content // "")"' ~/.flint/sessions/*.jsonl

# Name it, without a model and without rewriting anything
printf '%s\n' '{"type":"title","name":"the one about the proxy"}' >> ~/.flint/sessions/1789290356-957.jsonl

# Trim the end of a conversation that went off the rails: delete lines from the bottom.
# Every remaining line is still valid, and the next `flint --resume` continues from there.
```

Two warnings about editing, both learned the hard way:

- **Keep it one object per line.** A pretty-printed object breaks the file: the reader is
  line-oriented, and a line that is half an object is damage.
- **Do not delete the first line.** `meta` is what identifies the session and its
  environment; without it the file loads as a conversation with no model and no working
  directory attached to it.

Archiving is `mv` beside the file it came from, into that directory's `archive/`
(`sessions/archive/` for a root session, `sessions/<dir>/archive/` for a project's), which is
why `/archive` is instant and why undoing it by hand is the same command backwards. `--resume`
searches every archive as well as every live directory, so filing a conversation away — or
moving it by hand — does not make it unreachable. A child's conversation is the same
operation in reverse: `mv` it out of `children/` and it becomes one of your own
conversations, listed and reachable by number, which is the honest way to adopt one you want
to keep. `/delete` removes the file, and nothing else knows it existed.
