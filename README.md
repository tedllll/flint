# flint

A minimal cross-platform command-line agent.

One static binary that talks to a model and works directly on your machine:
reading and writing code, running commands, searching a tree, setting a machine
up, debugging what is broken, or just answering a question. It needs no runtime,
no package manager and no toolchain.

It is also what is still there when your usual tooling breaks — DSH, Codex, your
editor, whatever — which is where the constraints below come from. That is a
property of how it is built, not a description of what it is for: you should not
have to be in trouble to use it.

Those constraints drive every design decision:

- **No GUI.** Terminals only. It works over SSH, in a container, in a broken
  terminal.
- **Very few dependencies.** No SQLite, no OpenSSL. The one terminal library is
  there for a single reason, below.
- **Hand-editable state.** Config is TOML, history is JSONL. Both repairable
  with a text editor, because when things are broken you may not have a working
  model to fix them for you.
- **`flint exec` needs no model at all.** If every provider is unreachable, you
  can still run commands. It also needs no config: it creates none, prints no
  advice, and does not care whether an existing config can be parsed. When it is
  the last thing working, it must not depend on anything else working.

**Pi** ([pi.dev](https://pi.dev)) is another harness built on the opposite bet, and it agrees with
most of that list — no permission layer, plans and to-dos as files, append-only JSONL sessions, plain
files, no MCP. That agreement is a good sign for the list rather than a reason to use theirs. Where
flint differs is deliberate and narrow: the run's own background work is a record with a listing and a
stop rather than a multiplexer outside the program, a child run is a conversation you can read and
resume rather than a black box, and one run is drivable from a page, a `--json` caller, an MCP client
and Python alike. Those are the parts that cannot be borrowed from anyone, and they are argued on
purpose in [`docs/decisions.md`](docs/decisions.md#why-this-program-next-to-pi),
against the reading in [`docs/pi-agent-harness.md`](docs/pi-agent-harness.md).

The one affordance the interactive session does have is a **fixed input line**:
the bottom row is reserved, so the model's output scrolls above it and your
half-typed message never travels up the screen. Everything else is plain text.

That is also the one thing that switches itself off. When stdout is not a
terminal — piped, redirected, run from a script — flint emits no escape codes at
all, so `flint -p "..." | grep`, `flint exec` in a Makefile, and
`flint --help | less` all behave like ordinary Unix programs.

`flint --version` prints one line — the build's number, the same one the interactive banner shows —
and exits without reading a config, needing a key or starting a run, so a script or an installer can
ask which flint it is talking to. (Careful with the name: the `version` field on `session.started` and
in a session's `meta` line is the **session file format's** version, not the build's.)

## Install

Download the binary for your platform from Releases and put it on your PATH.
No toolchain required — that is the whole point.

```bash
# Linux (x86_64, fully static — runs on any distro)
curl -fsSL -o flint https://github.com/tedllll/flint/releases/latest/download/flint-x86_64-unknown-linux-musl
chmod +x flint && sudo mv flint /usr/local/bin/

# macOS (Apple Silicon)
curl -fsSL -o flint https://github.com/tedllll/flint/releases/latest/download/flint-aarch64-apple-darwin
chmod +x flint && sudo mv flint /usr/local/bin/
```

Windows: download `flint-x86_64-pc-windows-msvc.exe` and put it somewhere on
your `PATH`.

## Configure

First run creates `~/.flint/config.toml`:

```toml
default_provider = "deepseek"

# Shell used by the bash tool.
#   shell      = program name only
#   shell_args = the arguments that make it run a command string and exit
# If the program is missing, flint falls back to another shell on the platform
# (Windows: cmd -> powershell -> pwsh -> bash -> sh; Unix: sh -> bash -> zsh).
shell = "cmd"              # Unix: "sh"
shell_args = ["/C"]        # Unix: ["-c"]

max_tool_output = 30000   # cap on tool output fed back to the model
max_request_chars = 400000 # cap on the conversation in one request; 0 = off
max_steps = 100           # runaway-loop guard, not a work ration
readonly = false          # true = refuse every write

instructions = "hint"     # AGENTS.md: "hint" (name them) | "paste" | "off"
skill_dirs = []           # extra skill directories, after the standard two

thinking = "off"          # reasoning to ask for: off | low | medium | high. "off" sends no
                          # reasoning parameter at all -- the endpoint's own default, which for
                          # some models is reasoning on. /thinking changes it while a run is open

[[providers]]
name = "deepseek"
base_url = "https://api.deepseek.com/v1"
api_key = ""                       # or leave empty and export the env var
api_key_env = "DEEPSEEK_API_KEY"
model = "deepseek-chat"
thinking_field = "reasoning_effort"   # the JSON key the level goes in. Vendors disagree about
                                      # this one and agree about nothing else here, so it is a
                                      # per-provider field name; empty = never ask this endpoint

[[providers]]
name = "ollama"                    # local fallback: still works when the
base_url = "http://localhost:11434/v1"   # internet does not
api_key = "ollama"
model = "qwen2.5-coder:7b"
```

Any OpenAI-compatible endpoint works (DeepSeek, Kimi, GLM, OpenRouter, Ollama,
vLLM, llama.cpp). The client appends `/chat/completions` to `base_url`.

## Use

```bash
flint                            # interactive session
flint -p "why is my dsh broken"  # one-shot
flint why is my dsh broken       # same thing
flint -p "apply @rules.csv"      # @file is replaced by that file's contents (for anything too
                                 # big to fit on a command line)
flint -p "why?" --max-seconds 30 # bound the whole run; over budget it ends `incomplete` (exit 65)
flint -p "what is in /etc/hosts" --no-session   # answer without writing a conversation anywhere
flint -p "prove it" --thinking high   # ask for more reasoning (the field it goes in is the
                                      # provider's `thinking_field`; with none set, nothing is sent)
flint --continue                 # resume the last session here
flint --resume 3                 # resume a particular one (see the list)
flint --resume 1789116592        # ...by id prefix, or by path to the .jsonl
flint --fork 3                   # copy that session and carry on in the copy
flint --fork                     # ...the most recent one, when a branch is the point
flint exec "npm i -g @deepseek-ai/dsh"   # no model involved
flint balance                    # is this provider usable, and what is left in the account?
flint balance --json             # the same answer for a program
flint who                        # who else is working in this directory, and what changed
flint say "the tree is yours"    # leave whoever is working here a message (they see it; --hear-peers relays it)
flint --list-sessions            # numbered, so --resume N works
flint --list-sessions --json     # the same list as data, each row with its session path
flint --name "codex config"      # name the conversation you are in
flint --archive 3                # file it away, out of the list
flint --delete 4                 # remove the session file
flint export 3 > page.html       # one HTML file with the conversation in it, and nothing behind it
flint export 3 --out page.html   # the same, written where you say (stdout says where it went)
```

A session can be named, archived and deleted, and none of the three needs a model or a
key: they are file operations, and the moment you want to tidy the list is often the
moment the network is what is broken. Archiving moves the file into
`~/.flint/sessions/archive/`, so `mv` is the whole operation and undoing it by hand is
the same operation backwards. The file itself — every event, the rules a reader keeps, and
what can safely be edited by hand — is documented in
[`docs/session-format.md`](docs/session-format.md).

**`flint export` writes a conversation out as one file you can send somebody.** It is the same page
`--web` serves, with the conversation welded into it: open it in any browser, from disk, with no flint
running and no network — the question, the answers, every tool call and its result, the reasoning, the
same colours and the same fold-out rows. It is not a screenshot and not a second renderer: the page has
always been able to draw a session file dropped on it, and an export is that page with the file already
in it, so what you send is what you were looking at. Two things it deliberately does not carry: the
directory the conversation was held in (the one field in a session file that names your machine rather
than the conversation) and any request to anywhere — an export is one file, and it works on a machine
that has never heard of flint. A conversation with nothing in it is refused rather than written, since
a page that looks like a conversation and holds nothing is indistinguishable from one that failed to
load.

**The list is your conversations, not every conversation on the machine.** A run that
another run started — a `task`/`tasks` child, or the same thing through Python or MCP — writes
its session under `~/.flint/sessions/<dir>/children/`, and nothing lists that directory. So
`/sessions`, `flint --list-sessions`, the page's sidebar and `--continue` all mean "mine",
which they did not before: a child is newer than the parent that started it, and
`--continue` would happily resume the child's conversation instead of yours. The child's file
is a session like any other — same format, readable, `--resume <path>` opens it — and its
`meta` line names the conversation that asked for it. `mv` it up a level to adopt it.

**A conversation carried in colours the answer, and nothing is ever carried silently.**
`--continue` and `--resume <path>` put the earlier conversation into the request — which is the point
of them, and also the one way a caller can cross purposes without meaning to: a conversation that held
untrusted input colours the answer to a question about something else, and the run succeeds, so nothing
announces it. flint cannot tell a follow-up from a new question, so it does not guess; what it does is
never continue anything you did not ask for (a run with no continuation flag starts a new conversation,
and the file is created by the first thing said in it), and name the conversation it is in on
`session.started`, so a caller that must not mix purposes can check before it reads the answer. `--fork`
is the door for carrying a prefix into a *new* conversation. `docs/python.md` has the reasoning.

**A run can keep nothing at all.** `--no-session` writes no conversation: nothing to continue
from later, nothing in any list, no file on disk. It is a property of the whole *run* rather than
of one command, so it refuses `--continue`, `--resume`, `--fork` and `--name` on the command line,
and `/new`, `/resume`, `/import` and `/fork` typed inside the run, because each of those opens, copies
into or names the file the flag promised not to write; a provider switch, which normally creates the
file if the conversation has not said anything yet, cannot sneak one in either. What the run still writes is what it needs
to work — spilled tool output, and a background command's log — under
`~/.flint/spill/unattached-<pid>/`, a directory of its own so two such runs cannot overwrite each
other's `1.txt`. A `task` child is started with the same flag: a child is a conversation *this* run
asked for, and a parent with no conversation has no id to file it under `children/`, so the child's
file would have landed in your own list.

`flint balance` is the preflight, and it never sends a completion. It asks the provider
`GET /user/balance` — DeepSeek publishes one, with `is_available` (its docs: "whether the user's
balance is sufficient for API calls") and the granted, topped-up and total amounts — and falls back to
`GET /models`, which proves the key and the route and says nothing about money. When the endpoint
answers neither it says **"cannot tell"** rather than "usable": a local engine that serves only
`/chat/completions` is normal, and a preflight that reports a verdict nothing established is worse
than one that reports none. The exit code is the vocabulary a run already uses — `0` usable, `69` a
person must act, `75` the check could not get out and is worth repeating, `1` reached but
undecidable — so a batch that begins with `flint balance` learns about an empty account once instead
of on its hundredth call. `--json` gives the same answer as one object, with the figures absent when
the provider did not publish them.

`flint who` answers the other question a second agent in the same directory has to ask. A running
flint writes one small JSON record under `FLINT_HOME/live/`, refreshed every five seconds and removed
when the run exits, and `flint who` lists what is alive in this directory — pid, directory, provider,
model, whether it is `readonly`, how long it has been running and how long ago it last said so.
Anything left behind by a killed process is reported as **stale** rather than as alive, and a record
somebody edited into nonsense is reported as unreadable rather than skipped, because silence would
look exactly like "no other agent". Alongside that it prints the files that changed recently, from
`git status` and their modification times, and it says plainly that this line **names no author**: a
Codex, a Claude Code, an editor's autosave and a person all look the same through it, so "no other
flint" is not "nobody else". `--all` names runs in other directories; `--json` gives the whole answer
one object at a time, with the same warning in it.

Resuming prints the tail of the transcript, so "did it load?" is answerable at a
glance. Loading also happens when there is no network: an unreachable provider is
reported and the session still opens, because the history is how you find out what
you were doing when you broke it.

`--fork` is resuming's other half: it takes the same three ways of naming a session
(the list number, an id prefix, a path — or nothing at all for the most recent), copies
the conversation into a new session, and continues there. The original is not written
to, which is the point: `cp` can already do this, but wanting to try something without
losing the conversation you have should not require knowing where flint keeps its
sessions. The copy carries the conversation and the name, the run says which file
it is writing, and the copy records what it was copied from — see
[cutting a conversation at an earlier question](#cutting-a-conversation-at-an-earlier-question),
which is the same act taken at a point inside a conversation rather than at startup.

Inside the REPL:

| Command | Effect |
|---|---|
| `/help` | command list |
| `/provider [name]` | list, switch, add, edit or remove providers |
| `/provider add <name> <base_url> [model]` | add one; bare, it asks for each part |
| `/provider key <key>` | set the API key for the active provider |
| `/model` | show the model in force |
| `/model <name>` | switch to one of that provider's models |
| `/usage` | context size and token accounting, including the provider's cache hit rate when it reports one |
| `/compact` | fold the earlier part of this conversation into a summary the model writes (one request; the session file keeps every message) |
| `/verbose [on\|off\|full]` | how much of the agent's activity to narrate |
| `/detail [on\|off]` | print tool output (default off: one line per result) |
| `/readonly [on\|off]` | toggle the write guard |
| `/thinking [off\|low\|medium\|high]` | how much reasoning to ask the provider for, and which field it goes in (default `off`: ask for none) |
| `/hear-peers [on\|off]` | relay messages from `flint say` to the model (default off) |
| `/say [--to <pid>] <text>` | leave a message for whoever else is working in this directory; on the page the address is a picker over the live runs, and leaving it empty reaches everyone here |
| `/queue <text>` | say this after the turn that is running, without interrupting it |
| `/tools` | list tools |
| `/jobs` | the background work this run started, with each job's pid and what it is doing |
| `/jobs stop <pid>` | end one of them (a child is asked, a command is killed) |
| `/skills [name]` | list skills, or print one the way the model would get it |
| `/skill <name> [args]` | send a skill's instructions as your next message, aimed at `args` |
| `/prompts [name]` | list your saved prompts, or print one the way it would be sent |
| `/prompt <name> [args]` | send a saved prompt (typing `/<name>` is the same thing) |
| `/sessions` | list past sessions, numbered |
| `/resume <n\|id>` | switch to one of them, without restarting — it prints the conversation it moved to, as `--resume` does |
| `/import <file>` | copy a conversation in from a session file you were given or hand-edited; the file you name is not written to |
| `/export <file>` | write **this** conversation, as it stands, out as one self-contained HTML page — the same artifact `flint export` writes, and no model is asked for anything |
| `/fork [n]` | start a new conversation cut at question `n` of this one, keeping what came before it (bare, it lists the questions) |
| `/name [text]` | show or set a name for this conversation |
| `/archive <n\|id>` | move a session into `sessions/archive/` |
| `/delete <n\|id>` | delete a session file |
| `/new` | start a fresh conversation |
| `/config [edit]` | show or change shell, steps, proxy |
| `/reload` | re-read the config file after editing it yourself |
| `!cmd` | run a shell command, bypassing the model |

Type while the model is working to interrupt it; your line becomes the next
input. Ctrl-C clears a half-typed line, and quits when the line is already
empty. Ctrl-D quits.

`/queue <text>` is the other way to send a line mid-turn, for the sentence that is
not a correction: it is held and sent when the turn that is running finishes,
answer and all, and the transcript shows it as held (`queued for after this turn:
…`) and then as the question it became. A stop ends the answer in flight and not
what you queued behind it — the same rule Pi's queue follows — and a line queued
when nothing is running is simply this turn's message, because there is no turn to
hold it for. The page's command panel offers it as a form, which is the only shape
a follow-up can take from a composer.

Flags: `--provider`, `--model`, `--readonly`, `--no-session`, `--thinking`, `--hear-peers`, `--cwd`, `--no-color`
(or `NO_COLOR`), `--continue`, `--resume`, `--fork`, `--name`, `--archive`, `--delete`,
`--json`, `--schema`, `--result-file`, `--list-sessions`, `--max-seconds`.

### Reading a run from a program

`flint -p "..." --json` writes the run as one JSON object per line on stdout, and nothing
else goes there — no banner, no status row, no summary line — so a script can read the
stream without filtering prose out of it:

```bash
flint -p "why is my dsh broken" --json | while read -r line; do
  echo "$line" | jq -r 'select(.type == "tool.completed") | "\(.name): \(.ok)"'
done
```

```json
{"cwd":"C:\\work","model":"deepseek-chat","session":"C:\\Users\\me\\.flint\\sessions\\1789290-1.jsonl","type":"session.started"}
{"prompt":"why is my dsh broken","type":"turn.started"}
{"text":"Let me look.","type":"message.delta"}
{"id":"call_1","name":"bash","type":"tool.started"}
{"arguments":"{\"command\":\"dsh --version\"}","id":"call_1","name":"bash","type":"tool.args"}
{"id":"call_1","name":"bash","ok":true,"output":"1.2.3","type":"tool.completed"}
{"text":"Let me look. It is version 1.2.3.","type":"message.completed"}
{"prompt_tokens":1204,"completion_tokens":88,"outcome":"complete","duration_ms":8123,"provider_retries":0,"type":"turn.completed"}
```

The vocabulary is closed and small: `session.started`, `turn.started`, `message.delta`,
`reasoning.delta`, `message.completed`, `tool.started`, `tool.args`, `tool.completed`,
`usage`, `status`, `command`, `warning`, `error`, `result`, `turn.completed`. Four things about it
are worth knowing:

- **The stream is bytes, and what is promised is about the bytes.** One JSON object per line,
  nothing else on stdout: no prose, no blank line, no carriage return, no escape code, and every
  line a `type` from the list above. That is asserted on the raw bytes of a real run
  (`tests/json_output.rs`), because a parser a caller writes is built on all of it at once — a
  single stray line breaks every caller there is, and a `\r` or a lost newline breaks the ones
  that read a pipe on Windows.

- **A line is always a line.** Tool output containing newlines, quotes and escape codes is
  JSON-escaped, never printed raw, so splitting the stream on `\n` cannot cut an object in
  half. `message.completed` carries the whole answer, for a reader that would rather not
  reassemble the fragments.
- **`cache_hit_tokens` is on `usage` and `turn.completed` when the endpoint reported one**, and
  absent when it did not: a `0` there would be flint inventing a fact about the prompt. It is the
  raw count, so a caller can compute a rate over its own window rather than over one request.
- **The stream is a view, not the record.** The session file is written exactly as in any
  other run, and `session.started` names it, so a `--json` run can be resumed, listed and
  read afterwards like anything else.
- **A failure is on the stream too**, as an `error` line plus a non-zero exit code, so a
  caller reading stdout does not also have to read stderr to find out what happened. That includes a
  refusal decided *before* the stream would have been opened — a `--schema` this build cannot check, a
  missing prompt — which is written as one `error` line and nothing else, never as half a run. The one
  case that still reaches stderr is a command line flint could not read before it got as far as
  `--json` (`flint --nope -p x --json`), because at that point it does not know a stream was asked for.
- **The answer can be written where you asked for it.** `--result-file <path>` puts this run's answer
  in a file as well as on the stream: the answer text, or the validated object (pretty-printed) when a
  schema was given. The file is **emptied when the run starts** and filled only if this run answers —
  so an empty file means "nothing was answered", and a stale answer from an earlier run can never be
  read as this one's. It needs `--json` and a prompt; with no stream there is nothing to save a caller
  from, and redirecting stdout is the same thing.
- **A document too big for a command line travels as its name.** Windows caps a command line at about
  32k characters, and the cap is enforced by `CreateProcess`: a longer prompt never reaches flint at
  all — Python's `subprocess` raises `FileNotFoundError [WinError 206]` in the caller's own code. So
  `@file` in a prompt is replaced by that file's contents before the request, and the argument stays
  as short as the name:

  ```bash
  flint -p "apply @rules.csv to today's orders" --json
  ```

  ```json
  {"prompt":"apply @rules.csv to today's orders","attachments":[{"bytes":252894,"lines":12000,"path":"C:\\work\\rules.csv","token":"@rules.csv"}],"type":"turn.started"}
  ```

  The contents go *in the prompt*, which is the point: a path is a request the model may decline,
  `read` returns 2000 lines by default, and a long tool result spills to a file. `@name with spaces`
  is written `@"name with spaces"`, and `@a.txt.` at the end of a sentence works — the full stop is
  the sentence's. A name that is not a readable text file is left exactly as typed, because a prompt
  is prose and `someone@example.com` is an address: so `attachments` is how a caller sees what went
  in, and an empty list is how a mistyped name becomes visible instead of a model quietly answering
  about a path. Whole-prompt inlining is capped at 256 KB, refused by name before anything is sent —
  a silent truncation at the endpoint would be a wrong answer that looks complete. The session file
  records the expanded prompt (what the model was given); the stream keeps your words.
- **A call can be given a budget.** `--max-seconds 30` is a wall-clock bound on the whole run —
  including the repair attempts after a schema miss — and when it runs out the turn is dropped where
  it stands, whatever had been drawn is kept, and the run ends `incomplete` with
  `"reason":"seconds"` and exit **65**. That is the flag's whole point: `max_steps` cannot cut a
  request that never comes back, and a caller asking over a flaky link needs "something in a minute,
  or tell me you could not" rather than a process that is still going. It needs a prompt (a budget
  bounds a call, and an interactive session is bounded by whoever is typing at it). A run that
  finishes inside its budget is unaffected and carries no `reason`.
- **The end of a turn says what the answer is worth.** `turn.completed` carries an `outcome`:
  `complete` (the model finished), `incomplete` (flint stopped asking at a limit, so
  the text above is half of what it had — `"reason":"steps"` for the `max_steps` guard and
  `"reason":"seconds"` for a `--max-seconds` budget, which are raised in two different places) or
  `stopped` (the caller cut it short, below). A caller that
  acts on the answer reads this before it acts, because the type it validated says nothing about
  whether the model was finished. The exit code says the same thing to a shell:

  | Code | Meaning |
  |---|---|
  | `0` | the turn finished |
  | `1` | a failure flint has not classified — it means "I do not know", not a named cause |
  | `2` | the command line is wrong — nothing was asked of the model |
  | `65` | the answer is not usable: a schema that never matched, or a turn that ran out of steps |
  | `69` | a person has to act: no key, credentials the provider rejected, or an account with nothing in it |
  | `75` | the retries ran out and asking again later is the right move (a rate limit, a broken server, a network fault) |
  | `130` | the run was interrupted — `/stop` on the pipe, or Ctrl-C on a terminal |

  The numbers are `sysexits.h`'s, because a program branches on the code and one code for
  everything says nothing: *"a CLI that always exits 0 (or always 1) hides this signal, forcing
  agents to parse error text with regex"*.

- **And how long it took.** The same line carries `duration_ms`, measured from `turn.started` to the
  end of the turn, so a caller does not have to time the subprocess — which would also measure flint's
  start-up and the caller's own reading, and which a caller streaming the answer cannot do at all. It
  is present on every ending, `stopped` and `incomplete` included: "was that slow or was it stuck" is
  exactly the question a caller has after waiting. What it is **not** is a price: flint does not know
  what a token costs on the endpoint it was pointed at, so the money half of that row is not here.

- **And whether the slowness was a retry.** The same line carries `provider_retries`: how many of this
  turn's requests the provider had to send twice. `duration_ms` can raise "that took eleven seconds"
  and cannot answer it, because a retry happens *before* anything has been drawn — it leaves no mark on
  the stream at all, only a notice on stderr, which a `--json` caller does not read and a log cannot
  count. Unlike `cache_hit_tokens` the field is always present: flint always knows this number, and `0`
  is the answer "the first attempt worked" rather than a silence.

- **A failure names its cause when flint knows it.** The `error` line carries `code` and
  `retryable` — `{"type":"error","message":"…","code":"insufficient_balance","retryable":false}` —
  and omits both when nothing established a cause, rather than guessing at the edge. That last field
  is the question a caller actually has: *may I try again, or must a person do something first.* The
  case that made it necessary is money, because the status code cannot tell it apart from a rate
  limit: an OpenAI-shaped endpoint reports an exhausted quota as a **429**, the same status as "slow
  down", and flint used to retry it four times with a 1+2+4+8-second backoff — fifteen seconds to be
  told the same thing, twenty-five minutes across a hundred calls. DeepSeek says it with **402**,
  Anthropic with a **400** and a sentence, and all three are now the one code
  `insufficient_balance`. Reading the body is what makes that possible; the status alone is not the
  classification.

- **Two signals, two questions — the one combination that looks like a bug.** When a `--schema` run's
  answers never match, the stream ends with `turn.completed` carrying `outcome:"complete"` **and then**
  an `error`, and the process exits **65**, with no `result` line anywhere. It is deliberate, and it is
  written down here because the next reader's instinct is to make the two agree. They answer different
  questions: `outcome` is about the **turn** — the model did answer, three times, and flint stopped
  asking because the shape kept being refused rather than because a budget ran out, so `incomplete`
  would name a limit that never came due — while the `error` and the exit code are about the
  **answer**, which is not one a caller can use. A caller that branches should read it as: `outcome`
  says whether anything was cut short, `error`/`result` says whether what came back is trustworthy.
  `tests/json_output.rs` pins the order and the outcome together so neither "fix" can land by accident.
- **Retrying is the caller's decision, and flint hands over the evidence.** Nothing identifies a
  request — no id, no idempotency key — so a retry after a timeout is a new turn and its tools run
  again: a file written, a commit made, a message sent may all happen twice, and only the caller knows
  which of its prompts are side effects. What flint will not do is hide the half that happened: on a
  turn that was cut short, the `tool.started`/`tool.completed` frames are on the stream **before** the
  ending, so a caller that keeps its own stream can see exactly what a retry would repeat.
  `docs/python.md` has the rest, and `tests/json_output.rs` holds the ordering.
- **A silent run is not a dead one.** Between `tool.started` and `tool.completed` nothing happens
  for as long as the tool runs, and from a pipe that is the same thing as a crashed process. So a
  run that is working and not talking says so every five seconds:
  `{"elapsed_secs":42,"restarted":false,"text":"running bash","type":"status"}`. `restarted` is true
  on the first line about a wait — a renderer starts its clock there — and `elapsed_secs` is for a
  reader that cannot run one, such as one reading a log later.
- **A run can be stopped without killing it.** Write `/stop` to its stdin — the same word the
  interactive session takes, which exists precisely because a key is not always available. flint
  drops the turn, commits the answer it had already drawn to the session file, says so in a `warning`
  and exits **130**, so the half-answer you read is the one the next call is answered with in view.
  It does not exit 0: a caller branching on the code would take half an answer for a finished one,
  which is the fault the code is there to prevent. Killing
  the process instead loses exactly that. A line that is not `/stop` is counted and reported as a
  `warning` — "ignored 2 lines on stdin" — and deliberately **not repeated**. A pipe into a run is not
  a private channel: echoing what arrived would put a caller's diff, record or token on stdout, which
  is what gets logged, and it would grow with whatever was piped in. The count stays because the other
  failure is silence — a caller that wrote a line deserves to know it did nothing, without flint
  repeating what it wrote.

`--json` needs a prompt: an interactive session has no stream to write, and `flint exec`
is plain by contract because its output is the child's own bytes. Ctrl-C during a `--json`
run ends the process; the session file keeps every event that was complete.

A caller that does not start in the project it is asking about names it with `--cwd`: the
path is resolved absolutely at startup and refused if it is not a directory, and the
resolved path is what goes into the session's `meta` line — the same value `--continue`
matches on, so a program driving flint one process per question finds its own conversation
again from any directory it happens to run in.

### Asking for an answer with a shape

Prose is the wrong interface for a caller that has to *act* on the answer. `--schema` gives the
run a JSON Schema and makes the last line of the stream the answer as data:

```console
$ flint -p "when is the last trading day of 2026?" --json --schema trading-day.json
{"cwd":"C:\\work","model":"deepseek-chat","session":"...","type":"session.started"}
{"prompt":"when is the last trading day of 2026?","type":"turn.started"}
{"text":"{\"trading_day\": \"2026-10-21\"}","type":"message.delta"}
{"text":"{\"trading_day\": \"2026-10-21\"}","type":"message.completed"}
{"json":{"trading_day":"2026-10-21"},"attempts":1,"type":"result"}
{"prompt_tokens":1204,"completion_tokens":31,"outcome":"complete","duration_ms":2407,"provider_retries":0,"type":"turn.completed"}
```

`--schema` takes a path, or the schema itself when the value starts with `{`. It needs
`-p --json`: the schema is a promise to a program reading the stream.

**flint checks the answer itself, because nobody else will.** The only JSON mode the
OpenAI-compatible surface agrees on is `response_format: {"type":"json_object"}`, which promises
the reply parses — not that it has the fields you asked for. (DeepSeek rejects `json_schema`
outright; measured, `docs/decisions.md`.) So the schema goes into the system prompt, the request
asks for an object, the answer is validated locally against a **subset of JSON Schema** —
`type`, `properties`, `required`, `additionalProperties`, `items`, `enum`, and length/bound
keywords — and a keyword outside that subset is refused before the run rather than ignored. If
the answer does not match, the model is told which JSON path failed and asked again, up to three
answers in all; `attempts` says how many it took. When none of them match there is no `result`
line at all — a caller reading that type can trust it describes what the schema asked for — and
the run ends with an `error` line and exit code **65** — `EX_DATAERR`, "there is an answer and it is
not one you can use", which is a different problem from an argument to fix (`2`) or a provider to
repair (`69`).

**The schema is recorded in the session file**, as a `schema` line holding the whole schema. A
resumed conversation is therefore held to the same contract without the caller passing anything
again, and `--no-schema` is how a caller says "prose this time" without editing the file. That is
the same rule as everything else here: what a run agreed to is in the file, and the file is the
truth.

### Calling flint from Python

`examples/python/flint_call.py` is a single dependency-free file that runs a turn and hands back the
stream as a `Turn`: `ask()` for prose, `ask_json()` for a checked object, and every event untouched
for anything it does not name. Copy it into your project, or read it first — there is nothing under
it but `subprocess` and `json`.

```python
from flint_call import ask_json

day = ask_json("MA2610 的最后交易日是哪天？", cwd="/path/to/project", schema={
    "type": "object",
    "properties": {"last_trading_day": {"type": "string"}},
    "required": ["last_trading_day"],
})
print(day["last_trading_day"])
```

Two things about it are worth knowing before you build on it, and the second is the reason
`ask_json` exists: **a call blocks** until the run is over, and **a failed run does not raise** —
`ask()` returns a `Turn` whose `ok` is `False` and whose `answer` is `''`, so a caller that does not
check carries on with nothing. `cwd=` is required and is what separates conversations. `Chat` is the
same calls for a conversation rather than a question: it pins the session path on the first call,
passes it to `--resume` afterwards (so two callers in one directory cannot land in each other's
history), takes `on_delta=` for rendering an answer as it is written, and reads the record back with
`history()`.

Putting a file in the prompt is three arguments, because they make three different promises:
`attach=[path]` is a promise (flint reads the file and its text is in the prompt, and `ask` raises
`NotAttached` if it is not), `paths=[path]` is a hope (the names go in and the model decides whether to
read them), and `inline=[text]` is a promise by construction (the text *is* the prompt).
`require_read=[path]` checks the other direction — what the run *did*, from the `read` tool's frames —
and raises `NotRead` otherwise. Both refusals carry the `Turn`, and `Chat` keeps the conversation, so a
refused promise is not a lost answer. `map_calls(prompts, workers=…)` is the batch: one run per item,
each in its own conversation, results in the order asked for, and the first `insufficient_balance`
cancels what has not started and raises `OutOfBalance` rather than paying for nineteen more discoveries
that the account is empty. `docs/python.md` is the whole story, and
`examples/python/timing_demo.py` shows both blocking behaviours as measured output.

### Being used by another agent (MCP)

Codex, Claude Code and Cursor speak MCP, so `examples/mcp/flint_server.py` is a stdio MCP server —
standard library only, one tool — that lets any of them ask flint about a directory without inventing
a shell pipeline. Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.flint]
command = "python"
args = ["C:\\path\\to\\flint\\examples\\mcp\\flint_server.py"]
```

Claude Code takes the same shape: `claude mcp add flint -- python <path>`. The tool is `flint_ask`
with `prompt`, `cwd`, `readonly`, `model`, `provider`, `schema` and `timeout_secs`; the answer comes
back as text with flint's **exit code, outcome, cause and session path attached**, and a schema run
also returns `structuredContent`. That trailing block is the point: a parent agent can tell a finished
answer from half of one, can see that the account is empty instead of guessing, and can point at the
conversation that produced the value.

Two deliberate choices keep it cheap, because MCP's real cost is what the *client* carries:

- **one fat tool, not twenty thin ones.** A tool description and its schema are re-sent on every
  request of every conversation, so the cheapest thing a server can be is a single tool with a short
  description. Everything flint can do lives behind that one name.
- **the tool loop stays in the child.** The parent never receives flint's ten tool schemas — it asks a
  question and gets an answer. That is the advantage of calling an agent rather than a tool
  collection, and it is where the tokens are.

A long run is stopped gracefully: the server writes `/stop` to flint's stdin (the same word the
terminal takes) so the half-answer drawn so far stays in the session file, and kills only if flint
ignores it. `examples/mcp/test_mcp.py` speaks the protocol at it and checks all of the above.

### Subagents, without a new word for them

flint can start another flint. Not a mode, not a second kind of process: the `task` tool runs the same
binary with `-p … --json` — the same door a Python caller or an MCP client opens — and gives the answer
back with `exit code: N (meaning)`, the outcome, the cause, and the child's session path. The model gets
one block of text; a person can read the child's conversation afterwards, because it is a session file
like any other — it lives under `children/` so it does not join your own list of conversations, and its
`meta` line says which conversation asked for it.

```jsonc
// what the tool takes
{"prompt": "…", "cwd": "…", "readonly": true, "model": "…", "provider": "…", "schema": {…}, "timeout_secs": 600,
 "agent": "explorer",    // optional: a profile, which brings its own instructions, model and readonly
 "background": false}    // optional: wait here for the answer instead of taking the handle
```

A **profile** is a file — `<project>/.flint/agents/<name>.md`, or `<FLINT_HOME>/agents/<name>.md` for one
that applies everywhere — so that "the explorer" means the same thing to a person typing it and to a
model naming it, instead of a model composing a command line and getting a flag wrong:

```markdown
---
description: Reads the tree and reports. Never writes.
model: deepseek-flash
readonly: true
---

You are exploring this repository and reporting what is there. Read, do not change.
```

A profile's `model`, `provider` and `readonly` are **defaults**, which a `task` call may override —
except `readonly`, which a profile can only add. `/agents` lists what is on disk and prints one the way a
child would receive it.

```jsonc
// several jobs in one call, run at the same time, answers labelled in the order asked
{"tasks": [{"prompt": "…", "agent": "explorer"}, {"prompt": "…"}], "max_parallel": 4}
```

`tasks` prepares every child before starting any of them, runs them a few at a time (1–8 jobs, 4 at
once by default), and returns one block per job with the same provenance `task` gives. It is still not a
shared context: the children cannot see this conversation or each other, so a set of jobs that depend on
each other is the wrong set of jobs for it.

**A `task` comes back at once with a handle, and says when the job ends.** Waiting for a child was the
old default and it was wrong in practice: the parent sat there for minutes — measured in a real session,
where the person typed at the frozen parent, which drops the turn, so the tool result became
`interrupted by the user: tool 'task' was requested but never ran` while the child kept working and
billing. So the call returns the child's pid and the conversation its answer is being written to, and
the work happens off to the side. A model that needs the answer *now* says `"background": false` and
waits; otherwise it carries on and is told, in its next request, that a job finished — one line per job,
with the pid and the verb that collects it. You are told too, in the transcript, as soon as it ends.
The collecting verb is one tool — `job_op`, which answers for a child and for a background command
alike — with an action:

```jsonc
{"action": "status"}                  // this run's jobs: running, or ended with their exit code
{"action": "output", "pid": 12345}    // what a background command has printed since you last asked
{"action": "wait",   "pid": 12345}    // block until it ends, and take the answer
{"action": "stop",   "pid": 12345}    // ask it to stop (the graceful 'stop' the MCP door uses)
```

"Exactly once" is the property: a job that ended is reported once, and a `wait` or a `stop` counts as
having collected it, so nothing is repeated at every request afterwards. A job this run did not start
can still be seen — `status` reads the presence record and says what it is and where its conversation
is — but it says plainly that it cannot be waited for or stopped from here, because the pipe a `stop`
needs belongs to whoever spawned it.

**A command is a job too, and that is what the same verb is for.** A `bash`, `exec` or `pwsh` call
still waits by default — a command's output is usually the input to the next step — but it takes
`"background": true` when it is the wrong thing to wait for:

```jsonc
// bash, exec and pwsh all take it; the budget defaults to 900s for a backgrounded command
{"program": "cargo", "args": ["build", "--release"], "background": true}
```

The call comes back at once with a pid and a log file — both streams in one file, under this session's
directory, named in the reply — and the same three verbs apply: `status`, `wait` (the whole log plus
`exit code: N`), `stop` (a kill, because there is no conversation to interrupt), and `output`, which
reads only what has been printed since the last time you asked, so watching a ten-minute build costs
the new lines rather than the whole log. The budget is enforced by flint whether or not anything is
waiting, and the end of the job is announced the same way a child's is. There is nothing to
collect into a session file, so this is not a run: no presence record, no conversation, just a pid, a
log and a notice.

While a `task` call waits, the child's own work shows on the parent's status row — `task: running
search`, `explorer: waiting for the model` — because the child is already saying what it is doing on its
own `--json` stream and a row that says one unchanging word for two minutes is a row that tells nobody
whether anything is happening. Typing at the parent still interrupts the turn (that is what typing
does), but the child is a process of its own and does **not** stop: the turn that was dropped records
what it left running, with the child's pid and — once the child has named its conversation, which it
does at its first write — the session its answer will land in, so the work is collected rather than
repeated. A child still starting up says `It has not named its conversation yet` and points at `job_op`
instead of a path, because the sentence's promise is that the work is not lost and not worth paying for
twice, not that a path is always available. A `task` child outliving the turn that started it is real —
measured, by way of a bug report — so the honest thing is to say so rather than to claim the tool never
ran.

Three things it is not, said here because each one is a reasonable expectation to have:

- **It is not more context.** The child starts with no history from this conversation, so its value is
  isolation and least privilege — a wide search, a lot of reading — never a bigger window. A `task`
  call does not buy context, it spends it.
- **It is not a sandbox.** The child runs as the same user with the same tools. `readonly` is the only
  switch there is, and it is **monotonic**: a `readonly` run cannot be talked into a writing child, so
  "explore in readonly" means what it says.
- **It is not unlimited.** `FLINT_DEPTH` in the environment bounds the chain at two (a child and a
  grandchild), set by the tool for its child and by nothing else — a bound a model can edit out of its
  own command line is not a bound.

A command any of them runs is told which run it is in: the model's `bash`, `pwsh` and `exec` — and your
own `!cmd` — see `FLINT_SESSION` (the conversation's file), `FLINT_PROVIDER` and `FLINT_MODEL`. So a
script can name the conversation it belongs to, read the log of a job that run started, or ask the same
endpoint a second question. It is Pi's idea under flint's names (`PI_SESSION_FILE`, `PI_PROVIDER`,
`PI_MODEL`), and `flint exec` — which is not a conversation — has all three taken away rather than left
to inheritance, because a `flint` started by another run's command inherits a stale set.

Two runs working in one directory can see each other: `flint who` prints the live runs flint knows
about, and, separately, what changed recently — a line that names no author, because a changed file is
not evidence of who changed it. `flint who --all` lists runs in other directories too.

And they can say something to each other:

```console
$ flint say "please do not commit docs/sandbox.md, I am still writing it"
said: please do not commit docs/sandbox.md, I am still writing it
  in: C:\Users\you\.flint\mailbox\flint-9c1f0a3d.jsonl
  here: pid 41288 is working here; it shows what arrives between turns
  (a run working here shows it to its person; a run started with --hear-peers also passes it to its model)
```

From inside a run it is `/say <text>`, which writes the same line through the same function, so a
message no longer needs a second terminal: the reply names the runs that share this mailbox — or says
plainly that nobody is here and the message is waiting in the file — and the run that wrote it never
reads its own words back as a peer's. `/say --to <pid>` addresses one run, and the **page offers the
pids**: the command's row there draws a picker over the runs sharing this directory's mailbox — the same
`GET /peers` listing the terminal's own `/say` is answered from — because a pid typed into a text field
would be prose, and a message that quietly went to whoever the sentence named is worse than one that
reached everybody here. Empty means everyone, which is what the row did before it could address one.

The sentence appears in a running flint's transcript, prefixed with who said it. By default that is all
it does: it is written to the session file as its own `peer` event, never as a chat message, so it
cannot end up in a request body — anything that can write a mailbox could otherwise steer the tool loop
of a process that has no permission layer. `--to <pid>` addresses one run instead of everyone here.

Feeding a peer's words to the model is a decision a *person* makes, and there is a switch for it:

```console
$ flint --hear-peers              # this run relays what `flint say` or a peer's `/say` leaves here
> /hear-peers off                 # or back off, mid-session
peer messages OFF — shown to you, never sent to the model
```

With it on, a message that arrived between turns is sent with the next request as a user message
labelled as coming from another process — the model is told where it came from and that you let it
through — and the transcript says `passed on to the model` instead of `not sent to the model`. It is a
session switch and not a one-shot flag: relaying needs a *next* turn to relay into, so `-p` with
`--hear-peers` is refused rather than quietly ignored. Three more things bound it, and they are the
reason it is safe to have at all: the default is off and there is no config key, so a standing session
never relays by accident; the setting is per run, so a *resumed* conversation inherits nothing (the peer
event is still not history, so a decision made once cannot become permanent); and the session file
records `"heard":true` on the message that was relayed, which is the only place that says so. The page
has the same switch, drawn from the run like its others.

### Watching a run in a browser

A terminal is a poor renderer for a long answer: the scroll region fights you, a tool call is
one line, and selecting text across a redraw is a losing game. So flint can also serve a view
of the **running process** on loopback — not a second mode, and not a file viewer. The
terminal keeps working, and closing the tab loses nothing.

```
> /web
web: http://127.0.0.1:58962/?token=7fc045f7ab4cd01377715fe0401a8c47
```

That is the whole of it: `/web` from inside a conversation and the browser opens on the URL.
`flint --web` does the same before the first turn if you know in advance, `--port <n>` picks the
port instead of taking whatever is free, and **`--web` typed at the prompt works too** — a bare
flint flag at the prompt is read as the command it names, so `--provider x` becomes
`/provider x`. That last one is not politeness: `--web` is the only name for this feature
anyone has met, since it is in `--help` and in the README, and without the translation it went
to the model as a sentence.

The browser is opened with `open` on macOS, `cmd /C start` on Windows and `xdg-open` elsewhere,
and **only when stdout is a terminal** — a pipe is not a person, and without that check a test
suite would launch windows. If the launch fails nothing is lost: the URL is printed either way,
so the fallback is pasting it.

Three things about the view are deliberate, and each one was a decision rather than a default:

- **Loopback only.** The address is the literal `127.0.0.1`, never `0.0.0.0` and never a
  resolved `localhost`. Remote access is a different program with a much harder problem.
  Requests that did not come to this address, and requests from another origin, are refused.
- **A token, in the URL.** 128 bits, minted per run. It is on `/` in the query string because
  that is the URL you paste; every other route wants it in an `X-Flint-Token` header, so it
  cannot leak through a `Referer`, a log or a screenshot. The page carries
  `Referrer-Policy: no-referrer` for the same reason.
- **The page is a window and a composer.** There is a sidebar of conversations and an input box
  at the bottom. What you type goes into the *same channel the keyboard feeds*, which is the
  whole of the design: a message sent while the model is working steers the turn exactly as
  typing does, and a slash command typed into the page is a slash command — so `/resume` from
  the sidebar works without the page knowing that slash commands exist, let alone what one does.
  Enter sends; Shift+Enter is a newline, because a prompt is a paragraph more often than a line.

  The sidebar lists them the way `/resume` numbers them — it reads the same listing — and
  clicking one is exactly `flint`'s own `/sessions` followed by `/resume <n>`. `+ new` at the
  top starts a conversation, and it is `/new`: the page has no separate idea of what starting
  one means.

  The command panel offers the arguments a command takes, and `/fork` is the one whose arguments are
  about *this conversation*: the questions asked in it, as buttons, rebuilt every time the frame is —
  press the second one and the page sends `/fork 2`. The value is the number, not the question: the page
  composes `/<name> <value>`, so a button carrying the question's own text would send it as part of the
  line.

What it shows is the conversation the process is in, read from the session file on disk, plus
the live event stream — the same events `--json` writes, produced by the same code. So a tool
call, a streamed answer, a reasoning delta and the status line all appear, and `/new`,
`/resume` and `/reload` move the page to the conversation the terminal moved to.

**The file paths in the transcript are buttons.** Every path a run leaves behind — a `read`'s
argument, the `notes.txt:2:` lines a `grep` prints, the name at the end of a `write` — can be pressed,
and the file opens in a panel beside the conversation: the file's own bytes, its size, and a `reload`
for one that changed while you were reading it. A `:line` in the path opens at that line. A relative
path is the path of the process that wrote it, so `src/web.rs` means the run's `src/web.rs`, and
`--cwd` is what decides that. It is `GET /file`, which reads a file the transcript already names and
nothing else; §12 of `docs/web-mode.md` has the rule for what in a line counts as a path and the
measured record from a real browser.

**And the panel can hand a path to your machine.** Some of what a transcript names is not text this
page can show: a directory, a PDF, a log past the preview cap, an image. The panel's head has one more
control for that — `open` — and it hands the path to whatever *your* computer uses for it: `explorer`,
`open`, or `xdg-open`, so a directory opens in the file manager and a file in the program its type is
registered to. It is a second, deliberate press rather than the path itself, because the text in a
transcript is model-written and a plain click on it must not be what starts a process. It is refused
outright in a `readonly` run — that guard means the model may not start a program, and a button that
started one on a person's click would be the same program running anyway — and the page does not offer
it there, reading that from the run's own state rather than deciding for itself. `POST /open`, with the
command lines asserted per platform and never actually launched by a test: §16 of `docs/web-mode.md`.

**A web address in the transcript is a link.** An `http` or `https` address in the run's own words —
the answer that cites a page, a fetch result, a URL in a tool's output — opens in a new tab, with the
opener severed, so the conversation you are reading stays where it is. It is the same splitter as the
paths, and the same rule about what a line may contain: only those two schemes ever become a link, so
`javascript:` and `data:` stay the words they are. That is deliberate rather than incidental — the page
holds the run's token, and an `href` taken from a model's words is script in *that* document if the
scheme is not checked. A path stays a button rather than a link, because a file has no address a
browser may open from a page served over http — and the door it does have is `POST /open`, a
deliberate press in the panel (§16). §11 of `docs/web-mode.md` is the measured record.

**The run's background work is in the header.** A `task` child, or a `bash`/`pwsh`/`exec` command
started with `background: true`, appears in a **jobs** list — one row per job, badged with what it is,
saying what was asked, and ticking how long it has been going. A job that ends keeps its row and gains
its exit code in words (`exit code 0 (finished)`) and how long it took, so a build you started and
forgot about is still there when you come back. Pressing a row opens the job's own output in the panel
above: a command's log, or a child's conversation. The list is `GET /jobs`, read off the same records
`job_op` answers from; the page is told to re-read it on an `event: jobs` frame, checked when a job
starts and when one ends — including a command that ends while the run is idle, which is what the
four-second timer is for. `docs/web-mode.md` §13.

**And stopping one is a command, not a button.** `/jobs` prints the same listing in the terminal, with
each job's pid, and `/jobs stop <pid>` ends one: a child is *asked* to stop — `/stop` on its stdin, so it
keeps the half of an answer it had drawn — while a command is killed, and the sentence you get back says
which happened. The page's command panel offers the same line, and its candidates are the live rows of
the jobs panel you are already looking at, which is why nothing had to be sent for it. A job this run
ended reads as `killed`, never `failed`: a kill's exit status is the shell's, and on Windows a killed
`cmd.exe` reports 1. And between the ask and the end it reads as **`stopping`** — neither `running`,
which says it is doing something it is not, nor ended, which the run cannot promise yet — in the
terminal's listing, in `job_op status` and on the page's row alike, because all three read the same
record. `docs/web-mode.md` §13.

**What that costs, stated plainly:** a caller who has the port and the token can run the agent,
because that is what an input box is. What keeps it acceptable is §4 of `docs/web-mode.md` —
loopback only, the `Host` and `Origin` checks, and a token another origin's page cannot set. Any
line the page sends is treated as typed, including `!command`, which means the page is exactly as
powerful as the terminal it is watching.

`flint debug prompt-input` is what to use when the question is what the *model* was given;
this is for reading what happened.

### Seeing what the model is actually sent

```bash
flint debug prompt-input                        # the request as the conversation stands
flint debug prompt-input "why is it failing"    # ...with that message appended
flint debug prompt-input | jq .messages[-1]     # just the last thing it would read
```

It prints the request body — the system prompt, the history, the tool schemas — and exits
without sending anything or writing a session.

The reason this is worth a command is that the request is **not** the transcript. Tool
results are pruned from the request once they are stale, so a turn that ran twenty commands
sends the model recent output and drops the listings it has already summarised, while the
session file keeps every byte. A conversation past `max_request_chars` loses its oldest
*turns* the same way, replaced by a note saying how many and where they are. "Why did it
forget what it read ten steps ago" and "why is the prompt this big" both have their answer
in this output and nowhere else.

It is built by the same function the client posts, and there is a test that runs a turn
against a stub provider and asserts the preview equals the bytes the server received — so
the command cannot quietly start describing something that is not sent.

### Reading the web

`fetch` reads a URL and returns its text: markup stripped, length bounded, and a line saying
where the page came from and what was left out. It replaces `bash` and `curl` for reading a
page, which hands you raw bytes that are mostly markup — measured at 94,879 bytes for one
search result page against a 30,000-character budget, so the part worth reading is exactly
what gets cut.

**It reaches the public internet and nothing else.** The host is resolved once and every
answer is checked: loopback, the private ranges, link-local (where cloud metadata lives) and
the rest are refused, an IPv6 address that is really an IPv4 one is judged as the IPv4
address it is, and the connection is pinned to an address that was checked so a second
resolution cannot move it. Redirects are followed by hand, five at most, re-checked each hop.

This is a boundary around *this tool*, not around flint: `bash` reaches whatever the machine
can, and nothing here pretends otherwise. What it buys is that the safe path is also the easy
one.

### Search

`search` asks DeepSeek to look something up and returns a summary with its sources — so a
local model can search too, because the search is a tool rather than a capability of whichever
model is driving. DeepSeek performs it server-side on its Anthropic-compatible endpoint, so
the search is **not** sent to the provider in `config.toml`: that surface ignores `web_search`
and answers without searching.

**It needs no configuration if you already have a DeepSeek provider.** The credential is
inherited, resolved exactly the way that provider resolves it (`api_key_env` first, then the
literal `api_key`). With no DeepSeek provider at all, name one:

```toml
[search]
provider = "deepseek"        # borrow this provider's key and proxy
# or, with no DeepSeek provider configured at all:
api_key_env = "DEEPSEEK_API_KEY"
model = "deepseek-flash"     # which model DeepSeek searches on, not yours
max_uses = 1                 # how many searches one call may trigger
enabled = true               # false turns it off
```

**It is not cheap, and the price is not per search.** There is no per-search fee anywhere in
DeepSeek's pricing: the retrieved pages go into the context of the model turn doing the
searching, and you pay for those input tokens. Measured: 16,561 input tokens for one search
and 119,581 for a call that made two — roughly ¥0.02 to ¥0.13, halved outside DeepSeek's peak
hours (09:00–12:00 and 14:00–18:00 Beijing time on weekdays). A model that treats `search` as
cheap will spend real money.

`docs/deepseek-search.md` is the full record of what was measured, including why the summary
is labelled as untrusted: in the first live run the model used it, found it disagreed with the
local toolchain, checked another source, and reported that the summary was stale.

### Local engines

A local model server is a program that has to be running before its endpoint answers, that
holds gigabytes while it is, and that nothing else on the machine will start. So a provider
may name the commands that do it, and flint runs them as you switch:

```toml
[[providers]]
name = "llamacpp"
base_url = "http://127.0.0.1:8080/v1"
model = "gemma-4-12b-it-Q5_K_M"
start = "bash ~/gemma4-12b/server.sh"      # when flint needs it and nothing answers
stop  = "pkill -f 'llama-b10839/llama-server'"   # when a switch leaves it behind
start_timeout_secs = 180                   # a 12B model takes a while to load
```

`/provider llamacpp` then starts what it needs, waits for the endpoint — saying so, because a
minute of nothing on screen is indistinguishable from a hang — and switching to another
provider stops the one being left. On a machine that cannot hold two models at once, that is
the difference between switching and running out of memory.

**`start` has three meanings**, and the difference is the whole interface:

| | |
|---|---|
| absent | use what flint knows about this engine |
| a command | use it, and stop guessing |
| empty | not flint's to manage — leave it alone |

Left absent, flint derives the command from the provider's **name**, for `ollama`, `mlx` and
`llamacpp`. It only does so when that can actually work: the endpoint is local, the program is
on `PATH`, and the model is named. When it cannot, it says which of those is missing rather
than running something that would fail — a name-derived command that fails looks like flint
being broken, which is worse than having no default at all.

`stop` is run in the open, because for a recognised engine it is a `pkill -f` and the pattern
decides what dies. And with no `stop`, an engine is left running on purpose: an ollama that is
also serving a GUI must not be shut down because a conversation moved on.

Engine output goes to `<FLINT_HOME>/engines/<provider>.log`, and when a start times out that
path is in the error — it is where the reason is.

## Permissions

**Full by default.** There is no approval prompt; flint runs what it decides to
run. This is intentional — an approval dialog in an emergency is friction you do
not want — but it means flint can damage your system.

The one guard is `/readonly`, which refuses `write`, `edit`, and any mutating
shell command — and, for `exec`, any program that is not inspection only, judged by the
program and its verb rather than by re-reading a command line it never had. Use it when you want flint to look but not touch.

It holds on **every door**, including the two that are yours rather than the model's: `!cmd`
typed at the prompt, and `flint exec <command>` (from `--readonly` or from a read-only config).
A read-only run that could be walked around by the person at the keyboard would make the banner
it prints about itself untrue.

`--readonly` at startup turns it on for the whole run.

## Tools

Every tool call in one assistant message runs **at once**: a model that asks for six files, or three
commands, in a single message does not wait for them one after another. Two things about that are
deliberate. The **results are still reported in the order they were asked for**, so a transcript reads
top to bottom and a call sits next to its own result; what is concurrent is the waiting, and no line
ever carried that. And two calls that write the same file cannot lose each other's work by overlapping:
the read-before-mutate gate refuses the second one, because the file changed since *that* call read it.

| Tool | Purpose |
|---|---|
| `bash` | run a shell command (120s default timeout, `timeout_secs` to raise, `background: true` for one nobody should wait for) |
| `exec` | run a program with its arguments as an array — no shell, so nothing re-parses them |
| `pwsh` | run a PowerShell script, written to a `.ps1` file (Windows only) |
| `read` | read a file with line numbers, pageable via `offset`/`limit` |
| `write` | create or overwrite a file |
| `edit` | exact string replacement, unique-match enforced |
| `apply_patch` | several files in one all-or-nothing patch |
| `list` | list a directory |
| `glob` | find files by name pattern (`*.rs`, `**/test_*.py`), recursively |
| `grep` | search file contents for a literal string, recursively, with line numbers |
| `skill` | load the full instructions of a skill named in the catalog (only offered when skills exist) |
| `search` | look something up on the web and get a summary with its sources (only offered when a search credential is configured) |
| `fetch` | read a URL and get its text, with the markup stripped and the length bounded |

`glob` and `grep` are built in rather than shelled out on purpose. Every other
platform difference flint can paper over, but this one it cannot: `grep` does not
exist in `cmd`, `findstr` is not recursive, and `find` does not match file names --
so on the platform where a rescue tool is most likely to be needed, "where is this
file" and "where is this symbol" would have no working answer. They are also what
makes getting your bearings in an unfamiliar tree a single step instead of a dozen
`list` calls.

`exec` exists because a command *line* is read again by every layer between flint and
the program, and each layer's escaping is correct for itself and wrong for the next one.
An array has no second reader. `exec` takes the program and one string per argument, so
quotes, spaces, trailing backslashes, `$`, `%` and non-ASCII text inside an argument
arrive exactly as written; the tool result is the program's own output, not a transcript
of what a shell made of it. Shell syntax does not work there — no pipes, redirects, `&&`,
variables or globbing — and that is what `bash` is still for. A payload that is not really
an argument (a regex, a JSON body, a document) belongs in `stdin` or in a file whose path
is passed, rather than on a command line at all.

```json
{
  "program": "git",
  "args": ["commit", "-m", "fix: a \"quoted\" message, and a path ending in C:\\dir\\"]
}
```

`exec` is offered on every platform rather than only on Windows, because it is the right
tool everywhere: the model stops writing a command line and starts writing a list, and
`/readonly` gets to judge a program and its verb instead of guessing where the words are.

`pwsh` is Windows-only and exists for the case the other two handle badly: Windows
management. A service, a registry key, a CIM query or an event log is a multi-line script
with quoting of its own, and as a `bash` command line it is unreadable and fragile. The
script is written to `<FLINT_HOME>/spill/<session>/<n>.ps1` — UTF-8 with a BOM, because
Windows PowerShell 5.1 reads a `.ps1` without one as ANSI and turns every non-ASCII
character into mojibake — and run with `-File` and `-ExecutionPolicy Bypass`. The result
names the file, so the script can be read, edited and run again by hand.

Worth being precise about why it is a file: **not** because `-Command` mangles quoting. It
was measured, and `-Command` handles quotes, newlines and non-ASCII exactly as written —
PowerShell parses a command line the way the C runtime does, unlike `cmd`. The file is the
better shape because it is an artifact a person can keep, because the result can name it,
and because a script is not capped by the ~32k command-line limit. Reaching for `pwsh` for
something one program with arguments can do is what `exec` is for.

Tool output is capped at `max_tool_output` characters before going back to the model. When
it goes over, the whole of it is written to `~/.flint/spill/<session>/<n>.txt` and the
reply keeps both ends -- the first 4096 characters and the last 1024 -- with a line saying
how long the output was and which file holds the rest. Cutting the tail instead, which is
what a plain `[truncated]` marker does, throws away the end of a build log: the part that
says what failed and the part that gets looked for.

A whole conversation is capped too, at `max_request_chars` characters per request (400,000 by
default; `0` turns it off). Past it, the oldest *turns* are left out of the request and a note
in their place says how many went: `[12 earlier messages were left out of this request: the
conversation is longer than max_request_chars (400000). The session file keeps every one of
them...]`. What is dropped is the request, never the transcript — the session file keeps every
message, so a turn that has scrolled out of the request can still be read, or asked about, or
recovered by resuming the session. The newest turn and the system prompt are never dropped,
however small the budget, and a tool result is never separated from the call it answers.
`/config` shows the number in force.

That bound is a guard, not a policy: it fires when a conversation has grown too large, in the
middle of a turn, and what it drops is chosen by size rather than by meaning. `/compact` is the
deliberate version. It asks the model to summarize everything before your newest question — one
request, with no tools, so the summary cannot act — appends that summary to the session file along
with the byte offset of the first message that is still sent, and folds the same way in memory, so
the next request carries the summary and the tail instead of the whole conversation. The messages
it folded are **still in the file**: what changed is what gets sent. A run resumed tomorrow reads
the same fold and sends the same thing, and deleting that one `compact` line puts the conversation
back exactly as it was. Two things it will not do: decide on its own that a conversation is too
long (that is your call, and a model turn nobody asked for is a bill nobody agreed to), and compact
a run started with `--no-session`, which has nowhere to write the fold down.

`write` and `edit` refuse to touch a file this run has not read, and refuse again if the
file was read and then changed on disk by something else — a build, a formatter, a
generator. The first refusal exists because the alternative is a model overwriting a file
with its *idea* of what the file said; the second because the case a read-tracking gate
usually misses is the file that was read several turns ago and is no longer that file.
Creating a file is not gated, since nothing is being destroyed, and a file the run wrote
itself counts as known — the tool produced those exact bytes. `bash` is deliberately not
gated: a shell command can write anything it likes, and a guarantee that held only for
`write` and `edit` would be worse than no guarantee at all.

`apply_patch` takes Codex's format — `*** Begin Patch`, then `*** Add File:`,
`*** Update File:` or `*** Delete File:` sections, then `*** End Patch`:

```text
*** Begin Patch
*** Add File: src/new.rs
+pub fn hello() {}
*** Update File: src/main.rs
@@ fn main() @@
 mod new;
-use std::io;
+use std::io::{self, Write};
*** Delete File: src/old.rs
*** End Patch
```

Three things about it are decisions rather than syntax. A hunk is located by its own
lines, so there is no line-number arithmetic to get wrong and no way to land a change in
the wrong place because the file grew since it was read — if the context and the removed
lines are not there exactly once, the patch does not apply, and the error names the file
and the hunk. **Nothing is written until everything matches**: several files in one patch
are one edit, so a hunk that fails in the third file leaves the first two as they were
instead of leaving the tree half-changed. And a patch cannot touch the same file twice in
one call, since both sections would be planned from the same contents on disk and the
second would undo the first. Updates and deletions go through the same read-first gate as
`edit`. Not supported, on purpose: `*** Move to:` renames and any kind of fuzzy matching.

A tool argument of the wrong type is refused and named — `argument 'path' must be a
string, but it is a number` — rather than reported as missing or quietly read as absent.
For an optional argument the second half matters most: a mistyped `"limit": "5"` read as
"no limit" asks for five lines and gets two thousand, with nothing saying the instruction
was dropped.

The same call with the same arguments three times in one turn gets a one-line note saying
so -- and again at five and eight. Nothing is refused, because a repeat is sometimes right
(a file another process is writing, a command whose answer really has changed); what is
worth avoiding is repeating it silently while the budget goes into the same output twice.

### Downloads

A command that fetches something over the network is treated differently from one
that does not, because the two have nothing in common where time is concerned. An
ordinary command is bounded by a total budget: nothing about `ls` is expected to
take long, so two minutes means something is wrong. A download may legitimately take
an hour, and the question that matters is not how long it has run but whether it is
still moving. So a download is bounded by *silence*: it is left alone however long it
takes, and killed after five minutes without a byte. A total budget cannot express
that -- it kills the slow and tolerates the dead.

Which commands those are is decided by the command line, in its fetching forms only:
`curl`, `wget`, `git clone|fetch|pull|submodule`, `pip|npm|cargo|brew|apt* install`,
and so on. The tool name alone never decides it -- `npm install` fetches and
`npm run build` does not, and flagging the tool would make every local build look like
a transfer. When the command line cannot say, the model can with the `download`
argument; that is an addition to the check rather than a replacement for it, because
behaviour that depends on the model remembering a flag is behaviour that silently
stops working.

Progress goes to the status row, not the transcript. A download's own progress
frequently redraws one line with a carriage return rather than emitting newlines, so
each redraw is read as a line and shown where the running clock already is. Printing
one transcript line per percent would bury the conversation under its own transport.

## Context

There is no automatic context management. `/usage` shows the size of your last
prompt (that *is* your context) and the token accounting. If it grows too large,
`/new` starts fresh.

The footer under each answer carries the same counts for that turn, and when the endpoint
reports a cache split it adds the hit rate — `[ctx 12004 prompt + 88 completion = 12092
tokens, 87% cached]` — which is the one number that says whether the prompt flint builds is
stable from turn to turn. Endpoints differ in what they report: DeepSeek's
`prompt_cache_hit_tokens` and OpenAI's `prompt_tokens_details.cached_tokens` are both read,
and an endpoint that reports neither gets no cache text at all rather than a misleading
`0%`. The line in the session file is `{"type":"usage","usage":{…,"cache_hit_tokens":1050}}`
when there is one, and a resumed conversation starts with the last of those lines already in
force, so `/usage` answers with the size the conversation really had.

What flint does add to the prompt is what the project has written down, and nothing
else.

### Instruction files

`AGENTS.md` files are found from the working directory upwards, outermost first, so the
closest one to the work is the most specific. The search stops at the project root (the
first directory with a `.git`) and never goes above your home directory. `~/.flint/AGENTS.md`
applies everywhere and comes first.

`instructions` in the config decides what happens to them:

| Value | Effect |
|---|---|
| `hint` (default) | names the files and says to read them before changing anything |
| `paste` | puts their contents in the prompt, up to 32 KiB, truncated with a marker |
| `off` | says nothing |

Naming is the default on purpose: the model reads the file with `read`, so the prompt
does not carry a document that changes, and a long one costs nothing until it is
relevant. `/config` shows the mode in force and which files were found, and `/reload`
re-reads them (a file edited mid-session is picked up then, not at the next restart).

### Skills

A skill is a directory with a `SKILL.md` in it -- a procedure worth following step by
step, kept where it can be edited by hand:

```text
<project>/.flint/skills/tidy-commits/SKILL.md
~/.flint/skills/release-notes/SKILL.md
```

```markdown
---
name: tidy-commits
description: Squash, reword and split commits in this repository.
---

Step one: ...
```

Both keys are optional (`name` falls back to the directory name, `description` to the
first line of the body). The prompt gets one line per skill -- the name and the summary
-- and the body arrives only when the model calls `skill` with that name. So a skill
costs a line until it is used, and a repository with thirty of them does not spend a
prompt on all thirty bodies. The catalog says explicitly that the summaries are not the
instructions, because a model that follows a one-line summary and calls it done is
worse than one that asks.

`skill_dirs` in the config adds directories to search, after the standard two.
`/skills` lists what was found and where; `/skills <name>` prints the body exactly as
the model would receive it, which is the answer to "did it load what I wrote".

Search is one level deep, `<dir>/<name>/SKILL.md` and no deeper: a recursive search
would offer a project's test fixtures as procedures. The project's skills win a name
conflict, then the working directory's, then yours, then `skill_dirs`.

A skill is not only the model's to load. `/skill <name> [args]` sends the same body as
your own next message, with everything after the name appended -- or put where the body
says `{args}` -- so the person who wrote a procedure can aim it instead of watching for
the model to decide to. `/skills <name>` still only prints it, which is the difference
between reading one and using one.

### Saved prompts

A prompt you type often is a file, in the same shape and the same three places as a skill,
and it is sent by typing its own name:

```text
<project>/.flint/prompts/tidy-commits.md
~/.flint/prompts/release-notes.md
```

```markdown
---
description: Tidy the commits on this branch.
---

Tidy the commits touching {args}, then say what changed.
```

`/tidy-commits src/parser.rs` sends `Tidy the commits touching src/parser.rs, then say what
changed.` -- one line, no ceremony. `{args}` is where the rest of the line goes; a file with
no `{args}` gets it appended as a last paragraph, so a saved prompt can be aimed at
something else even if you did not plan for it. With nothing after the name the file is sent
as written. `/prompts` lists what was found and where, `/prompts <name>` prints one as it
would be sent, and `/prompt <name> [args]` is the same send under a name a menu can press.

Nothing about a template reaches the model until you send it: no line in the prompt, no tool
schema, unlike the skill catalog. The transcript shows the line you typed and a dim line
under it naming the file it came from, so which of two files by the same name won is
answerable at a glance; the session file keeps the expanded text, because that is what was
actually sent.

### Bringing a conversation in from a file

A session file is plain JSONL and hand-editable, so one can arrive from anywhere: written in an editor,
produced by a script, or handed over by somebody else. `/import <file>` starts from such a file:

```text
/import ~/Downloads/the-socket-question.jsonl
```

It **copies** the conversation into a conversation of this run's own — a fresh file in your
`FLINT_HOME`, with `meta` naming *this* machine and *this* run's provider and model, and the source is
not written to at all. That is the difference from `--resume <path>`, which continues *inside* the file
it is given: a file somebody handed you would grow, gain your `usage` lines, and keep their `cwd`. It is
the same act as `--fork`, for a file this run never started from.

The copy says where it came from, on a line above the conversation:

```json
{"type":"import","from":"/home/you/Downloads/the-socket-question.jsonl","from_id":"1789512345-88-4412","messages":12}
```

`from` is the path as you gave it — a fact of the moment, not a pointer to follow, since the file may
have moved since or never have been on this machine. `from_id` is the source's own id (or its file name
when it had no `meta` line at all), which is what you would search another sessions directory for. A
sentence under the line that names the conversation later — `/resume`, and the line a startup resume
prints — says `this conversation was imported from <file>`, so a copy can never be mistaken for a
conversation that began here.

Three things are refused rather than half-done: a file with **no conversation in it** (importing
nothing would leave a session in the list that looks real), the file **this run is currently writing**
(that would copy a growing conversation into itself; `--fork` at startup is that act), and a run
started with `--no-session`, which refuses `/import` like every other door that would create a
conversation.

### Cutting a conversation at an earlier question

The other direction is a conversation you are already in and want to take back. `/fork 2` starts a new
conversation from the first two questions and their answers, so the third question can be asked again
differently in a conversation that never saw the answer that went wrong:

```text
/fork
   1. why does the socket close early
   2. what about the retry path
   3. and the timeout
     /fork <n> starts a new conversation cut at question n, keeping what came before it (3 questions)
/fork 2
forked: 20260101000000-1-777.jsonl → 1789639658-585-17152.jsonl
  2 messages kept, cut at question 2 of 3: what about the retry path
```

Bare `/fork` lists the questions and writes nothing, because which point to cut at is the one thing the
command may not guess. The cut is always at a **question** — a turn boundary, and the only point in a
conversation a person can name from the prompt — since cutting between a tool call and its result would
leave a request the provider rejects. The conversation you came from keeps every byte; the branch is a
file of its own, with a `fork` line above the conversation saying what it was cut from and how much of
it was kept:

```json
{"type":"fork","from":"/home/you/.flint/sessions/C--work/1789512345-88-4412.jsonl","from_id":"1789512345-88-4412","kept":2}
```

`--fork <file>` at startup writes the same line without `kept`, because it copies the whole
conversation rather than a prefix. `/fork 1` is refused (cutting at the first question would leave an
empty conversation, which is what `/new` is for), as is a number past the end, and `--no-session`
refuses the door like the others. A startup resume and `/resume` say where the branch came from
underneath the line naming it.

## Build from source

```bash
cargo build --release
# fully static Linux binary:
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

Releases are built by GitHub Actions for Linux (x86_64/aarch64, musl), macOS
(aarch64/x86_64) and Windows (x86_64).

Where it is going, and what it deliberately will not do, is in
[`ROADMAP.md`](ROADMAP.md); why it is built the way it is — decision by decision, with the
reasoning — is in [`docs/decisions.md`](docs/decisions.md). Every door this build offers,
with what to do at it and what comes back — the checklist for checking a build — is
[`docs/features.md`](docs/features.md).

## Design notes

Sessions are append-only JSONL at `~/.flint/sessions/<dir>/<id>.jsonl`, one event
per line. A damaged line is skipped and reported rather than taking the session
down. A resumed session is appended to, not rewritten, so nothing said after
`--continue` is lost.

**A conversation you go back to is drawn, not merely loaded.** `--continue`, `--resume`, `--fork`,
`/resume <n|id>` and `/fork <n>` all print the tail of the conversation they open — the last twelve
messages,
under a line that says how many earlier ones were left out — before the prompt comes back. The
reason is that a line naming a file, on a screen that still holds the conversation you just left,
cannot be told apart from a switch that opened nothing: seeing where the conversation got to is the
whole reason for going back to it. A `--json` run prints none of it — that stream is for a program,
and the same fact is already in the request the model is sent.

A session file is created by the first thing *said*, not when flint starts. Open the REPL or
`--web` and type nothing, and there is no file, no row in `/sessions` and nothing in the
sidebar — a run that is refused before it says anything (no key, an endpoint that cannot be
reached) leaves nothing behind either. A command that changes the conversation's environment,
such as `/provider`, does count as something happening.

`<dir>` is the working directory the conversation was held in — its last path component and
a hash of the whole path, `flint-1f0a7c93` — so two projects sharing one home are separated
on disk and not by a filter that has to read every file to decide whose it is. The session
file's `meta` line records that directory too, and that is what `--continue` believes, so
moving a file (or the project) does not change which conversation is "the one I was just
in". Sessions written before this layout sit directly in `sessions/` and are still found.
A conversation another run started goes one level deeper, in `<dir>/children/`, which is the
same idea used a second time: the level that is read is the level that is listed, so a
child's conversation is out of every person-facing list without a filter anywhere — and the
`meta` line of one names its parent, so the provenance runs both ways.
`--resume` names any session outright, anywhere.

Point `FLINT_HOME` at a project (`FLINT_HOME=/path/to/project/.flint`) to give it its own
config, sessions and skills as well; that is the belt to this layout's braces, and it is what
a program driving flint per project, one process per question, usually wants.

Nothing is ever rewritten, which is what makes the format repairable by hand:

- `Meta` records the format revision (`"v": 2`). A file that names no revision is
  v1, and a file that names a *newer* one is read as far as it can be.
- An event whose `type` this build does not know is skipped **without comment**.
  That is what lets a later version add one: a newer flint's session must not
  look like corruption to an older one. A line that names a type this build knows
  and still cannot be read *is* reported, because that means the transcript has a
  hole in it.
- Naming appends a `"type":"title"` line, so the name is the last one in the
  file; archiving *moves* the file to `sessions/archive/` rather than marking it.

Listing reads the two ends of each file and nothing else -- the head for `Meta`
and the first thing you said, the tail for a name appended later. A conversation
that grew to hundreds of kilobytes costs the same to list as a short one.

### Proxies

By default flint connects **directly**, and that is a deliberate choice rather
than an absence of one. `reqwest` would otherwise apply the platform's proxy
setting -- on Windows, the registry one under Internet Settings, which is not an
environment variable and is invisible from inside flint. A proxy client that is
installed but has no server selected leaves that setting enabled and pointing at
a closed port, so every request dies inside a tunnel that nothing owns. The
symptom is a working network, a working `curl`, and an agent that cannot connect,
with no configured proxy to blame because there is not one.

The only proxy used is the one written down: `proxy` on a provider in
`config.toml`, or the top-level `proxy` that the `bash` tool exports to its
children. When a request fails, the error names the proxy that was in force and
whether anything is listening on it, because "the proxy is up and the remote is
down" and "the proxy is not running" need opposite responses.

The provider layer implements the OpenAI streaming protocol only, including the
two parts that are easy to get wrong: SSE frames split across network chunks
(handled with a carry-over buffer) and tool-call arguments arriving as string
fragments that must be concatenated by index before they are valid JSON.

### The bottom strip

Notices -- anything that must reach the user while a tool is still running, such as
"this command has been running for 20s" -- go through the transcript machinery, not
stderr. With the strip active there is no safe place for a stray write: stderr lands
wherever the cursor happens to be, which is inside the answer strip, and it tears the
layout apart. The half-written answer is committed first, so the notice reads as a
line above an answer that then continues.

A tool result line names *what* the tool was pointed at (`✓ read src/lib.rs 14
lines`), because `✓ read 55 lines` twice in a row is unreadable: nothing
distinguishes two calls to two files from one call printed twice, and the latter is a
bug this transcript has had.

The input row is pinned to the last line of the screen, and the three rows above
it are the answer strip, where a streamed answer is drawn. Above that, output is
ordinary transcript that scrolls.

Inserting a transcript line means narrowing the terminal's scroll region to the
rows above the strip and writing at the bottom of it, so the newline scrolls the
transcript up and the strip is never part of the scroll. That is Codex's inline
viewport, and it is why the strip cannot disturb the transcript and the
transcript cannot disturb the strip ([codex-rs/tui](https://github.com/openai/codex/tree/main/codex-rs/tui)
uses the same idea through `insert_history_lines`).

The strip is a fixed slice, so an answer taller than it is drawn
bottom-anchored and its earlier lines are handed to the transcript in order as
they leave the top. Nothing is lost, and the strip never moves.

### Testing the non-terminal paths

The guarantee that pipes stay escape-free is easy to break and impossible to
notice, so it is checked by script rather than by eye:

```bash
cargo test                                  # unit tests, incl. key translation
node scripts/run-capture.js out.bin ./flint --help   # assert esc=0
node scripts/pipe-check.js ./flint cmds.txt out.bin  # a piped REPL session
```

These capture raw bytes and count escape sequences; `scripts/interactive-check.js`
drives a real Windows pseudo-console for the path that needs one.

### Testing the bottom strip

The strip's layout is a deterministic question -- escape sequences either put
text where the user can see it or they do not -- so it is checked by replaying
bytes through a screen model rather than by watching a terminal:

```bash
cargo test --test term_capture        # capture the real interactive byte stream
node scripts/term-layout-test.js      # replay it, plus hand-written scenarios
node scripts/layout-trace.js          # frame-by-frame trace, for diagnosing
node scripts/vtscreen.js raw.bin 24 70  # one raw dump, as a screen
```

`tests/term_capture.rs` drives `Term` the way the REPL does and writes the byte stream
to a file of its own, and the layout test replays that file. A debug build honours
`FLINT_TERM_CAPTURE` for this — the only way to reach the interactive branches from a
test — and `FLINT_TERM_CAPTURE_FILE=<path>` says where the bytes go; without it they go
to stdout, which is what `examples/live_turn.rs` below wants. Release builds do not
compile either variable.

The file is named rather than reached by pointing the process's stdout at it, which is
what this used to do. Redirecting file descriptor 1 also captures whatever else writes
there, and in a test binary that is the harness's own progress lines: one of them lands
on the bottom row mid-capture, its newline scrolls the screen, and the transcript a test
is about to assert on has left the recorded screen. That produced a blank screen and a
failure with nothing wrong in the layout code — roughly one full-suite run in four,
against zero in ten after the change.

The replay model counts CJK characters as two columns and expands tabs, because it
is used to judge output that contains both. A tool that disagrees with a real
terminal about width produces phantom wrapping, which is worse than no check at
all — so `scripts/term-layout-test.js` asserts the width behaviour itself.

`examples/live_turn.rs` runs one turn against a real provider through the same
layout, for checking that genuine model output — reasoning, tool calls, and all —
lands where it should:

```bash
FLINT_TERM_CAPTURE=1 cargo run --example live_turn -- "your question" > live.bin
node scripts/vtscreen.js live.bin 24 100     # the visible screen
node scripts/tall-replay.js live.bin 100     # the whole transcript
```

`examples/read_probe.rs` prints what a tool returned with tabs and carriage
returns made visible, which is how the transcript is separated from the tool's own
formatting when something looks wrong.

`examples/channels.rs` prints which channel each fragment arrived on — `content` or
`reasoning_content` — which is how a stray line in the transcript is attributed to
the model or to the display.

Tests can pin the window size with `FLINT_TERM_SIZE=100x24` (debug builds only). A
capture made at whatever width the test harness happens to report cannot be replayed
at a different one without manufacturing failures: lines wrap in different places,
and every assertion about them becomes a guess.

### What the transcript shows

One line per tool call and one per tool result — never the output itself, which is
routinely hundreds of lines. A failure keeps its first line, because that is the
part a reader may have to act on. Reasoning shows a single `… thinking` marker
rather than the stream, which arrives one token at a time and would otherwise be a
word per line.

Tool output lives behind its own switch, `/detail`, and not behind `/verbose`. They
are different wants: "tell me more about what the model is doing" should not also
print every file it reads, and tying them together meant anyone who wanted the first
got the second. `/detail on` prints up to 25 lines per result and notes how many it
withheld.

Rows in the transcript are counted as *screen* rows, not lines, and a line wider
than the window is wrapped before it is written. Both of those are load-bearing.
History is inserted inside a scrolling region, so a line the terminal wraps itself
continues past the region's bottom margin and the same paragraph is written down the
whole screen; and counting a 283-column line as one row makes the transcript commit
the wrong rows as the answer streams.

Within one turn the model streams in *segments*: it reasons, calls a tool, reasons
again, and only then answers. Each segment is a fresh streamed answer, so each one
starts from an empty strip and the earlier segment is committed first. A row is
committed once and only once, tracked by count rather than by position — a long
answer has most of its rows committed while it streams, so re-committing "the rows on
screen" sends the overlap again and the paragraph reappears under the next round.

## License

MIT
