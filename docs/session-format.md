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

## One event per line

Every line is a JSON object with a `type`. This build understands six:

| `type` | Written when | Fields |
|---|---|---|
| `meta` | once, as the first line | `v`, `id`, `created`, `cwd`, `provider`, `model`, `parent` (only when another run started this one) |
| `chat` | a message is added to the conversation | `message` |
| `usage` | the provider reports token counts | `usage` |
| `title` | the conversation is named | `name` |
| `switch` | the provider or model in force changes | `provider`, `model` |
| `schema` | the answer shape in force changes | `schema` (absent or `null` when cleared) |

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
{"type":"title","name":"dsh start failure"}
{"type":"switch","provider":"deepseek","model":"deepseek-reasoner"}
{"type":"schema","schema":{"type":"object","properties":{"day":{"type":"string"}},"required":["day"]}}
{"type":"schema"}
```

`usage` is whatever the provider last reported, written as it arrives; it is an event rather
than a field on `meta` because the file is append-only and the numbers change every turn.

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
