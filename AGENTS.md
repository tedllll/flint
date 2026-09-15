# Working on flint

flint is a coding agent in one static Rust binary: a terminal UI, a tool loop, and plain
files on disk. This file is what flint reads about its own repository, so it is written for
whoever is changing the code — a person or a model driving it.

## What this repository believes

- **Dependencies are the enemy.** Ten crates, and each new one has to be argued for. There
  is no YAML crate (the front matter of a `SKILL.md` is parsed by hand), no async runtime
  beyond tokio, no TUI framework beyond crossterm. A new dependency needs a reason that
  survives being written down.
- **No derived state.** Anything that can be rebuilt from real files is not persisted: no
  index, no cache, no database. Sessions are append-only JSONL, config is TOML, spilled
  tool output is plain text. If flint is killed at any moment, the files on disk are still
  the truth.
- **State has to be hand-editable.** Never compressed, never opaque, never a binary blob.
  Someone must be able to repair their session with `notepad`. This is a hard rule, not a
  preference: it rules out formats and shortcuts that would otherwise be reasonable.
- **Comments say why.** The code says what it does. A comment is for the decision that is
  not visible: why this ordering, what broke last time, what the obvious alternative was
  and why it is wrong. Delete a comment rather than leave one that restates the line.
- **Nothing user-visible may depend on a test-only path.** If `readonly` mode or the
  capture hook is how a guarantee is honoured, that guarantee does not exist.
- **Never put a real API key in a test.** The stub provider is `wiremock`; that is enough.

## Layout

| Path | What lives there |
|---|---|
| `src/main.rs` | argument parsing, the REPL, slash commands, one-shot `exec` |
| `src/agent.rs` | the tool loop: build the prompt, call the model, run tools, persist events |
| `src/provider.rs` | the OpenAI-compatible client, streaming, retries, usage |
| `src/tools.rs` | the tool set, and the read-before-mutate gate |
| `src/patch.rs` | the `apply_patch` format, parsed and applied — pure functions |
| `src/term.rs` | the inline viewport: scroll region, answer strip, status clock |
| `src/display.rs` | how a tool call and its result read in the transcript |
| `src/session.rs` | reading and writing `sessions/*.jsonl`, listing, archiving |
| `src/schema.rs` | the JSON Schema subset flint validates a `--schema` answer against, by hand |
| `src/context.rs` | `AGENTS.md` discovery and the skill catalog |
| `src/config.rs` | config load/save and the paths under `FLINT_HOME` |
| `src/search.rs` | web search: where the credential comes from, and DeepSeek's search endpoint |
| `src/web.rs` | `--web`: the embedded viewer and the loopback listener that serves it |
| `tests/` | `agent_loop` (stub provider), `cli_output` (real binary, raw bytes), `term_capture` (byte-exact terminal), `search_tool` (stub search endpoint), `web_view` (the page's policy) |
| `scripts/` | Node replay tools: `vtscreen.js`, `term-layout-test.js`, `layout-trace.js` |
| `docs/windows.md` | field notes on Windows terminal behaviour; read before touching layout |
| `docs/windows-tooling.md` | the plan for command-line escaping on Windows, half built (`exec` and the shared runner are in; the PowerShell half needs a Windows session); read before touching `probe_shell` or adding a tool |
| `docs/session-format.md` | the session file format, for readers and for hand-editing |
| `docs/web-mode.md` | the browser view over a running flint: all three levels are built (read-only view, live feed, composer with a sidebar) and §11 records what was measured in a browser |
| `docs/deepseek-search.md` | web search: what was measured about DeepSeek's search, and what it costs |
| `docs/decisions.md` | why flint is built this way, decision by decision |
| `ROADMAP.md` | the plan of record: the ordered queue, and what is deliberately not done |
| `HANDOFF.md` | state of the project at the end of the last working session |

## Where flint keeps its own state

Everything is under `FLINT_HOME`, which defaults to `~/.flint` (on Windows,
`C:\Users\<you>\.flint`). Point `FLINT_HOME` at a temporary directory to try something
without touching a real setup — the tests do exactly that, and every command below works
in a scratch directory for the same reason.

| Path | What it is |
|---|---|
| `<FLINT_HOME>/config.toml` | the configuration; created on first run |
| `<FLINT_HOME>/sessions/<dir>/<stamp>-<n>.jsonl` | one conversation per file, append-only, in the directory belonging to the working directory it was held in (older sessions sit directly in `sessions/`, and are still found). A file is created by the first thing said in it, so opening flint and typing nothing leaves nothing |
| `<FLINT_HOME>/sessions/archive/` | conversations filed away with `/archive` (a project's archive is `sessions/<dir>/archive/`) |
| `<FLINT_HOME>/spill/<session>/<n>.txt` | tool output too long for one request, in full |
| `<FLINT_HOME>/engines/<provider>.log` | a local engine's output, and the only place a failed start says why |
| `<FLINT_HOME>/AGENTS.md` | instructions that apply to every project |
| `<FLINT_HOME>/skills/<name>/SKILL.md` | skills available everywhere |
| `<project>/AGENTS.md` | instructions for that project |
| `<project>/.flint/skills/<name>/SKILL.md` | skills for that project |

Nothing else. A web search keeps no state on this machine at all: the request goes out, the
answer comes back as a tool result, and the sources land in the session file like any other
tool output.

### `config.toml`, key by key

One table per endpoint, then the settings that apply to the run as a whole. A key that is
missing takes the default shown here, so an old file keeps working when a new key appears.

```toml
[[providers]]                   # repeat this table for each endpoint
name = "deepseek"               # the name used by --provider and /provider
base_url = "https://api.deepseek.com/v1"   # /chat/completions is appended if absent
api_key = ""                    # a key written here is a key in this file
api_key_env = "DEEPSEEK_API_KEY"           # ...or read it from the environment
model = "deepseek-chat"
models = []                     # extra models for /model to offer
proxy = ""                      # route to this provider, e.g. socks5h://127.0.0.1:10808
```

```toml
default_provider = "deepseek"   # which provider is used without --provider
shell = "cmd"                   # bash tool's shell program (Unix: "sh")
shell_args = ["/C"]             # arguments that run a command string (Unix: ["-c"])
max_tool_output = 30000         # characters of tool output per reply; the rest spills
max_steps = 100                 # runaway-loop guard, not a work ration
readonly = false                # refuse write/edit/apply_patch and mutating bash
proxy = ""                      # exported to bash children as HTTP(S)_PROXY
verbose = "on"                  # off|on|full: how much of the agent's activity to narrate
tool_detail = false             # print the output behind a tool result
instructions = "hint"           # AGENTS.md: "hint" (name them), "paste", "off"
skill_dirs = []                 # extra skill directories, after the standard two
```

```toml
[[providers]]                   # ...and the two fields that run a local engine
start = ""                      # run when flint needs this provider and nothing answers
stop = ""                       # run when a switch leaves this provider behind
start_timeout_secs = 0          # how long to wait for the endpoint; 0 means 180
```

A **local** model server is started and stopped as providers are switched, so that
`/provider llamacpp` brings up the server it needs and switching away frees the memory it
held. `start` has three meanings and the difference is the interface: **absent** asks flint to
use what it knows about the engine, **a command** is used as written, and **empty** means this
one is not flint's to manage — which is how a provider that is somebody else's process (an
ollama also serving a GUI, say) says so.

Left absent, flint derives the command from the provider's **name**, for three engines it
knows: `ollama`, `mlx` (or `mlx-lm`, `mlxlm`) and `llamacpp` (or `llama-server`). It only does
so when that can work — the endpoint is on this machine, the program is on `PATH`, and for
`mlx` and `llamacpp` the `model` field names what to load — and when it cannot, it says which
of those is missing instead of running a command that would fail. A name-derived command that
fails looks like flint being broken, which is why the check exists rather than the guess.

```toml
[search]                        # optional, and normally absent
enabled = true                  # false turns the `search` tool off
provider = ""                   # borrow this provider's key and proxy
base_url = ""                   # default: DeepSeek's Anthropic endpoint
model = ""                      # which model *DeepSeek* searches on; default deepseek-flash
max_uses = 1                    # how many searches one call may trigger
api_key = ""                    # used when `provider` names nothing
api_key_env = ""
```

With no `[search]` block at all, the credential is inherited from any provider whose
`base_url` is DeepSeek's, resolved the way that provider resolves it. The endpoint is **not**
inherited: a provider's address is a chat-completions one and that surface ignores
`web_search` without saying so. `docs/deepseek-search.md` is the measured record, including
what a search costs — it is billed as input tokens, not per search.

The distinction between the two `proxy` keys is worth keeping straight: the one inside a
provider table is how flint reaches the *model*, and the top-level one is handed to the
*commands* the `bash` tool runs. They are different routes and are often not the same one.

`/config` prints the path in force and the settings that matter; `/reload` re-reads the
file, including `AGENTS.md` and the skill catalog, after something has changed it.

### Sessions, and how to look at them

`/usage` shows the size of the last prompt. `/sessions` lists what exists (it reads only
the two ends of each file), `/name`, `/archive` and `/delete` manage the open one, and
`flint --list-sessions` does the listing without a model. A session file is one JSON object
per line; an unknown event type is skipped in silence, and a line that names a *known* type
but cannot be parsed is reported as damage rather than ignored. `docs/session-format.md` is
the reference for the format itself, including what can be edited by hand.

## There is no permission layer

This is the most important thing to understand before running anything here.

flint has **no approval prompts, no sandbox, no allow-list, and no undo.** A command the
model asks for runs as your user, with your environment, on your machine, immediately. The
`bash` tool is not gated at all: `rm -rf`, `git reset --hard`, `git clean -xdf`,
`curl | sh` and a push to a shared branch all work exactly as well as `ls`.

The only switch is `readonly` (`/readonly`, or in the config), and it is all-or-nothing: it
refuses `write`, `edit`, `apply_patch` and mutating shell commands, while still allowing
inspection. For `exec` it refuses any program that is not inspection only, judged from the
program and its verb rather than from a command line — see `exec_is_readonly` and
`is_readonly_words` in `src/tools.rs`. Useful for a first look around an unfamiliar machine;
not a safety net for ordinary work.

The read-before-mutate gate — `write`, `edit` and patch updates refuse a file this run has
not read, or one that changed since it was read — is a guard against *mistakes*, not a
boundary. It exists because a model that overwrites a file with its idea of the file
destroys work no error message can bring back. It does not slow down a `bash` command for a
moment, and it is not a substitute for thinking.

So, when working in this repository:

- **Read before you destroy.** Every irreversible action deserves a look at what is
  actually there first: `git status`, `git log`, the file itself, not the idea of it.
- **Prefer the reversible step.** A new commit over an amend; a branch over a force-push;
  writing a file over deleting a directory. If two ways exist, take the one that can be
  undone.
- **Never `git push --force`, `git reset --hard`, `git clean -xdf`, or delete outside the
  repository** unless that exact thing was asked for, in those words.
- **Stay inside the working directory.** Nothing in this project needs to touch `C:\`,
  `~`, or another checkout. If a task seems to, stop and ask.
- **Say what will change before changing it** when the change cannot be undone — a
  deletion, a rewrite of history, a config file that other tools read.
- **Do not commit secrets.** No API keys, no tokens, no `config.toml` with a real key. If
  one lands in a diff, take it out before committing rather than after.

## Verifying a change

```bash
cargo test                      # the lib, the loop, raw CLI bytes, the terminal
cargo clippy --all-targets      # expected to be silent, and worth keeping that way
node scripts/term-layout-test.js
```

`flint debug prompt-input` prints the request body that would be sent — system prompt,
history, tool schemas — without sending it or needing a key. It is built by the same
`provider::request_body` the client posts, so it is the honest way to check what the model is
actually given after changing the prompt, a tool schema or the request-side pruning. The
request is *not* the transcript: the session file keeps every byte, and pruning drops stale
tool output from the request only.

`HANDOFF.md` has the current counts and the state of the tree as the last session left it;
the count in this file would only be a date. Keep the tests honest instead: write the test
that fails first, watch it fail for the right reason, then fix the code. A regression test
that has never been red has not been shown to test anything. Assertions on the terminal must
be on real bytes: `FLINT_TERM_CAPTURE=1` plus `FLINT_TERM_CAPTURE_FILE=<path>` and
`FLINT_TERM_SIZE=100x24` (debug builds only) force the interactive path into a file, which
`scripts/vtscreen.js` then replays as a screen. `FLINT_TERM_CAPTURE=1` alone writes to
stdout, which is what `examples/live_turn.rs` wants.

Two traps worth knowing on Windows:

- **A test run after restoring a file with a copy may test the old binary.** `CopyFile`
  preserves the source's modification time, so cargo can decide nothing changed. Watch for
  the `Compiling` line, or touch the restored files.
- **A running `flint` holds the release binary open**, so `cargo build --release` fails to
  replace it. Close sessions before rebuilding.

## Commits and pushes

One concern per commit. The subject is imperative and lower-case after the type
(`fix: ...`, `feat: ...`, `docs: ...`, `refactor: ...`); the body explains the problem and
the decision, not the diff. Commit messages in this repository are long on purpose — they
are where the reasoning lives for the next person.

`origin` fetches over HTTPS and pushes over SSH (`git@github.com:tedllll/flint.git`, a
deploy key named in this repository's `core.sshCommand`). Pushing to `main` is what the
project does; it is a small project with one author, and the commit log is the review.
