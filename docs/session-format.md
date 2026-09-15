# The session file format

A conversation is one file, `~/.flint/sessions/<id>.jsonl`, and it is **append-only**: flint
adds lines and never rewrites one. That single rule is what makes everything else here
possible — a rename costs one line, a killed process leaves a readable file, and a person
with a text editor can repair a conversation without a tool that understands it.

The id is the file name: a Unix timestamp and a counter, `1789290356-957.jsonl`. Nothing
else identifies a session, so `cp` is how you fork one and `mv` is how you rename the file.

## One event per line

Every line is a JSON object with a `type`. This build understands five:

| `type` | Written when | Fields |
|---|---|---|
| `meta` | once, as the first line | `v`, `id`, `created`, `cwd`, `provider`, `model` |
| `chat` | a message is added to the conversation | `message` |
| `usage` | the provider reports token counts | `usage` |
| `title` | the conversation is named | `name` |
| `switch` | the provider or model in force changes | `provider`, `model` |

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

### `usage`, `title` and `switch`

```json
{"type":"usage","usage":{"prompt_tokens":1204,"completion_tokens":88}}
{"type":"title","name":"dsh start failure"}
{"type":"switch","provider":"deepseek","model":"deepseek-reasoner"}
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

Archiving is `mv` into `sessions/archive/`, which is why `/archive` is instant and why
undoing it by hand is the same command backwards. `/delete` removes the file, and nothing
else knows it existed.
