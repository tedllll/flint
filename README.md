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

The one affordance the interactive session does have is a **fixed input line**:
the bottom row is reserved, so the model's output scrolls above it and your
half-typed message never travels up the screen. Everything else is plain text.

That is also the one thing that switches itself off. When stdout is not a
terminal — piped, redirected, run from a script — flint emits no escape codes at
all, so `flint -p "..." | grep`, `flint exec` in a Makefile, and
`flint --help | less` all behave like ordinary Unix programs.

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
max_steps = 100           # runaway-loop guard, not a work ration
readonly = false          # true = refuse every write

instructions = "hint"     # AGENTS.md: "hint" (name them) | "paste" | "off"
skill_dirs = []           # extra skill directories, after the standard two

[[providers]]
name = "deepseek"
base_url = "https://api.deepseek.com/v1"
api_key = ""                       # or leave empty and export the env var
api_key_env = "DEEPSEEK_API_KEY"
model = "deepseek-chat"

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
flint --continue                 # resume the last session
flint --resume 3                 # resume a particular one (see the list)
flint --resume 1789116592        # ...by id prefix, or by path to the .jsonl
flint --fork 3                   # copy that session and carry on in the copy
flint --fork                     # ...the most recent one, when a branch is the point
flint exec "npm i -g @deepseek-ai/dsh"   # no model involved
flint --list-sessions            # numbered, so --resume N works
flint --name "codex config"      # name the conversation you are in
flint --archive 3                # file it away, out of the list
flint --delete 4                 # remove the session file
```

A session can be named, archived and deleted, and none of the three needs a model or a
key: they are file operations, and the moment you want to tidy the list is often the
moment the network is what is broken. Archiving moves the file into
`~/.flint/sessions/archive/`, so `mv` is the whole operation and undoing it by hand is
the same operation backwards. The file itself — every event, the rules a reader keeps, and
what can safely be edited by hand — is documented in
[`docs/session-format.md`](docs/session-format.md).

Resuming prints the tail of the transcript, so "did it load?" is answerable at a
glance. Loading also happens when there is no network: an unreachable provider is
reported and the session still opens, because the history is how you find out what
you were doing when you broke it.

`--fork` is resuming's other half: it takes the same three ways of naming a session
(the list number, an id prefix, a path — or nothing at all for the most recent), copies
the conversation into a new session, and continues there. The original is not written
to, which is the point: `cp` can already do this, but wanting to try something without
losing the conversation you have should not require knowing where flint keeps its
sessions. The copy carries the conversation and the name, and the run says which file
it is writing.

Inside the REPL:

| Command | Effect |
|---|---|
| `/help` | command list |
| `/provider [name]` | list, switch, add, edit or remove providers |
| `/provider key <key>` | set the API key for the active provider |
| `/model [name]` | show or change the model |
| `/usage` | context size and token accounting |
| `/verbose [on\|off\|full]` | how much of the agent's activity to narrate |
| `/detail [on\|off]` | print tool output (default off: one line per result) |
| `/readonly [on\|off]` | toggle the write guard |
| `/tools` | list tools |
| `/skills [name]` | list skills, or print one the way the model would get it |
| `/sessions` | list past sessions, numbered |
| `/resume <n\|id>` | switch to one of them, without restarting |
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

Flags: `--provider`, `--model`, `--readonly`, `--cwd`, `--no-color` (or
`NO_COLOR`), `--continue`, `--resume`, `--fork`, `--name`, `--archive`, `--delete`,
`--json`.

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
{"prompt_tokens":1204,"completion_tokens":88,"type":"turn.completed"}
```

The vocabulary is closed and small: `session.started`, `turn.started`, `message.delta`,
`reasoning.delta`, `message.completed`, `tool.started`, `tool.args`, `tool.completed`,
`usage`, `warning`, `turn.completed`, `error`. Three things about it are worth knowing:

- **A line is always a line.** Tool output containing newlines, quotes and escape codes is
  JSON-escaped, never printed raw, so splitting the stream on `\n` cannot cut an object in
  half. `message.completed` carries the whole answer, for a reader that would rather not
  reassemble the fragments.
- **The stream is a view, not the record.** The session file is written exactly as in any
  other run, and `session.started` names it, so a `--json` run can be resumed, listed and
  read afterwards like anything else.
- **A failure is on the stream too**, as an `error` line plus a non-zero exit code, so a
  caller reading stdout does not also have to read stderr to find out what happened.

`--json` needs a prompt: an interactive session has no stream to write, and `flint exec`
is plain by contract because its output is the child's own bytes. Ctrl-C during a `--json`
run ends the process; the session file keeps every event that was complete.

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

What it shows is the conversation the process is in, read from the session file on disk, plus
the live event stream — the same events `--json` writes, produced by the same code. So a tool
call, a streamed answer, a reasoning delta and the status line all appear, and `/new`,
`/resume` and `/reload` move the page to the conversation the terminal moved to.

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
session file keeps every byte. "Why did it forget what it read ten steps ago" and "why is
the prompt this big" both have their answer in this output and nowhere else.

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

`--readonly` at startup turns it on for the whole run.

## Tools

| Tool | Purpose |
|---|---|
| `bash` | run a shell command (120s default timeout, `timeout_secs` to raise) |
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
reasoning — is in [`docs/decisions.md`](docs/decisions.md).

## Design notes

Sessions are append-only JSONL at `~/.flint/sessions/<id>.jsonl`, one event per
line. A damaged line is skipped and reported rather than taking the session
down. A resumed session is appended to, not rewritten, so nothing said after
`--continue` is lost.

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
