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
the same operation backwards.

Resuming prints the tail of the transcript, so "did it load?" is answerable at a
glance. Loading also happens when there is no network: an unreachable provider is
reported and the session still opens, because the history is how you find out what
you were doing when you broke it.

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
`NO_COLOR`), `--continue`, `--name`, `--archive`, `--delete`.

## Permissions

**Full by default.** There is no approval prompt; flint runs what it decides to
run. This is intentional — an approval dialog in an emergency is friction you do
not want — but it means flint can damage your system.

The one guard is `/readonly`, which refuses `write`, `edit`, and any mutating
shell command. Use it when you want flint to look but not touch.

`--readonly` at startup turns it on for the whole run.

## Tools

| Tool | Purpose |
|---|---|
| `bash` | run a shell command (120s default timeout, `timeout_secs` to raise) |
| `read` | read a file with line numbers, pageable via `offset`/`limit` |
| `write` | create or overwrite a file |
| `edit` | exact string replacement, unique-match enforced |
| `list` | list a directory |
| `glob` | find files by name pattern (`*.rs`, `**/test_*.py`), recursively |
| `grep` | search file contents for a literal string, recursively, with line numbers |
| `skill` | load the full instructions of a skill named in the catalog (only offered when skills exist) |

`glob` and `grep` are built in rather than shelled out on purpose. Every other
platform difference flint can paper over, but this one it cannot: `grep` does not
exist in `cmd`, `findstr` is not recursive, and `find` does not match file names --
so on the platform where a rescue tool is most likely to be needed, "where is this
file" and "where is this symbol" would have no working answer. They are also what
makes getting your bearings in an unfamiliar tree a single step instead of a dozen
`list` calls.

Tool output is capped at `max_tool_output` characters before going back to the
model, with a `[truncated]` marker.

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

`tests/term_capture.rs` points stdout at `target/term-capture.bin`, drives `Term`
the way the REPL does, and the layout test replays that file. A debug build
honours `FLINT_TERM_CAPTURE` for this, which is the only way to reach theinteractive branches from a test; release builds do not compile it.

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
