# flint

A minimal cross-platform rescue agent.

When your usual tooling breaks — DSH, Codex, your editor, whatever — flint is
still there: a single static binary that can talk to a model and run commands on
your machine so you can repair the thing that broke.

That purpose drives every design decision:

- **No GUI.** Terminals only. It works over SSH, in a container, in a broken
  terminal.
- **Very few dependencies.** No SQLite, no OpenSSL. The one terminal library is
  there for a single reason, below.
- **Hand-editable state.** Config is TOML, history is JSONL. Both repairable
  with a text editor, because when things are broken you may not have a working
  model to fix them for you.
- **`flint exec` needs no model at all.** If every provider is unreachable, you
  can still run commands.

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
max_steps = 25            # runaway-loop guard
readonly = false          # true = refuse every write

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
flint exec "npm i -g @deepseek-ai/dsh"   # no model involved
flint --list-sessions
```

Inside the REPL:

| Command | Effect |
|---|---|
| `/help` | command list |
| `/provider [name]` | list, switch, add, edit or remove providers |
| `/provider key <key>` | set the API key for the active provider |
| `/model [name]` | show or change the model |
| `/usage` | context size and token accounting |
| `/verbose [on\|off\|full]` | how much tool detail to print |
| `/readonly [on\|off]` | toggle the write guard |
| `/tools` | list tools |
| `/sessions` | list past sessions |
| `/new` | start a fresh conversation |
| `/config [edit]` | show or change shell, steps, proxy |
| `/reload` | re-read the config file after editing it yourself |
| `!cmd` | run a shell command, bypassing the model |

Type while the model is working to interrupt it; your line becomes the next
input. Ctrl-C clears a half-typed line, and quits when the line is already
empty. Ctrl-D quits.

Flags: `--provider`, `--model`, `--readonly`, `--cwd`, `--no-color` (or
`NO_COLOR`), `--continue`.

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

Tool output is capped at `max_tool_output` characters before going back to the
model, with a `[truncated]` marker.

## Context

There is no automatic context management. `/usage` shows the size of your last
prompt (that *is* your context) and the token accounting. If it grows too large,
`/new` starts fresh.

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
down.

The provider layer implements the OpenAI streaming protocol only, including the
two parts that are easy to get wrong: SSE frames split across network chunks
(handled with a carry-over buffer) and tool-call arguments arriving as string
fragments that must be concatenated by index before they are valid JSON.

### The bottom strip

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
honours `FLINT_TERM_CAPTURE` for this, which is the only way to reach the
interactive branches from a test; release builds do not compile it.

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

### What the transcript shows

One line per tool call and one per tool result — never the output itself, which is
routinely hundreds of lines. A failure keeps its first line, because that is the
part a reader may have to act on; `/verbose` shows more. Reasoning shows a single
`… thinking` marker rather than the stream, which arrives one token at a time and
would otherwise be a word per line.

## License

MIT
