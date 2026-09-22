# Working on flint

flint is a coding agent in one static Rust binary: a terminal UI, a tool loop, and plain
files on disk. This file is what flint reads about its own repository, so it is written for
whoever is changing the code — a person or a model driving it.

## What this repository believes

- **Dependencies are the enemy.** Ten crates, and each new one has to be argued for. There
  is no YAML crate (the front matter of a `SKILL.md` is parsed by hand), no async runtime
  beyond tokio, no TUI framework beyond crossterm. A new dependency needs a reason that
  survives being written down. `libc` is the one platform-specific addition (Unix only, and
  already in the tree): it is there to see a terminal that has gone away, which `std` cannot
  report.
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
| `src/main.rs` | argument parsing (`-V`/`--version` prints the build's number and exits before the config is read, because its caller is a script), the REPL, slash commands (including `/jobs` and `/jobs stop <pid>`, a person's door onto the same record the page and `job_op` read, `/import <file>`, which copies a conversation in from a file this run did not start from and leaves that file alone, `/fork [n]`, which cuts *this* conversation at the n-th question and starts a conversation of its own from what came before it — listing the questions when given no argument, because which point to cut at is the one thing it may not guess — `/queue <text>`, the follow-up that waits for the turn it was typed into instead of interrupting it (the one line `run_turn`'s poll loop takes without ending the turn, and a plain message when nothing is running), and the two sends a command can be — `/skill <name>` and `/prompt <name>`, or a saved prompt typed as its own name — which is the one place a line typed at the prompt becomes something other than itself), `origin_note`/`questions` (where a copied conversation says what it was copied from, and the unit `/fork` cuts on — every `user` line but a fold's summary, which is written with the person's role and recognised by `session::is_summary`), one-shot `exec`, `flint export <n|id|path> [--out <file>]` — which writes a finished conversation out as one self-contained HTML page and stops, needing no key because it reads one file and writes another, and owning stdout entirely when `--out` is absent so that `flint export 3 > page.html` is the whole invocation — and `/export <file>`, the same artifact for the conversation this run is holding, where the path is an argument because stdout is the terminal it is talking to (both doors build the page through one `page_for_session`, so a page exported mid-conversation and one exported afterwards cannot drift) — and the input reader the key thread and the hangup watcher belong to. `--no-session` is decided here and nowhere else: the flag's command-line refusals, and the one sentence every door that would open a conversation gets — `/new`, `/resume`, `/import`, `/fork`, and the page's sidebar rows, which go through the same dispatcher |
| `src/agent.rs` | the tool loop: build the prompt, call the model, run tools — **every call of one assistant message at once** (`join_all`, the message is the unit, and the read-before-mutate gate is what makes concurrent writers safe), with the results reported in the order they were asked for — and persist events |
| `src/event.rs` | the one enum a turn's events go through — provider deltas and agent activity alike — so the UI knows one vocabulary |
| `src/sink.rs` | what a turn's events *become*: the transcript, the status line and the page's stream, shared by the REPL and `examples/live_turn.rs` rather than copied |
| `src/attach.rs` | `@path` in a one-shot prompt: which names are files, the inline block the model reads, and the 256 KB cap |
| `src/provider.rs` | the OpenAI-compatible client, streaming, retries, usage — including `usage_from`, which collapses the two cache-split shapes endpoints use into one number, and `Thinking`, the split that keeps the *level* flint's (`off`/`low`/`medium`/`high`, state on the provider so `/thinking` can move it) and the JSON *field* the endpoint's (`thinking_field`, from the provider table) — nothing is sent unless both are set, because a field guessed wrong is a request an endpoint may refuse |
| `src/engine.rs` | bringing a *local* model engine up and letting it go: a provider's `start`/`stop` commands, and the derived command for the three engines flint knows |
| `src/tools.rs` | the tool set (`task`/`tasks` for children — a handle at once, `background: false` when the next step needs the answer — `bash`/`pwsh`/`exec` with `background: true` for a command nobody waits for, `job_op` for either kind of job, and the notice a job that ends leaves behind), `JobMoment` (when a job started and ended, recorded once in both clocks rather than derived from the clock at each look, because a start that can move by a second between two glances is not a start), `jobs_report`/`stop_job` (the same listing and the same stop for a person, a page and a model — one answer, three doors, with `ended_by_us` recording that *this* run ended a job so a kill never reads as a failure, and `stopping` — a flag set where the stop is asked for and read against `finished` — so a row can say "on its way out" instead of "still working"), `RunEnv`/`apply_child_env` (what a command a run starts is told about the run — `FLINT_SESSION`, `FLINT_PROVIDER`, `FLINT_MODEL`, and the proxy variables, on both spawn sites; a run that is not a conversation takes them away instead of leaving what it inherited), `task_argv` (a `task` child's whole command line, which is where a run hands its own properties down — the endpoint, and `--no-session`, so a run that keeps no conversation starts children that keep none), and the read-before-mutate gate |
| `src/patch.rs` | the `apply_patch` format, parsed and applied — pure functions |
| `src/term.rs` | the inline viewport: scroll region, answer strip, status clock |
| `src/display.rs` | how a tool call and its result read in the transcript |
| `src/session.rs` | reading and writing `sessions/*.jsonl`, listing, archiving, and the `children/` directory a child run's conversation goes in. The provenance events (`import`, `fork`) are read back into `LoadedSession::origin`, and `SessionWriter::forked_from`/`write_messages` are what a copy is written with — the lineage as an event rather than `meta.parent`, which means "the run that started this one" and is what files a conversation under `children/`. It also owns the fold: `SessionEvent::Compact` carries a summary and the **byte offset** of the first `chat` line still sent (`/compact`'s one line in the file, hand-editable, last one wins), `last_question_offset` is where that pointer is read off the file — the newest question, never a tool result — and `load` applies the fold to `messages`, so a resumed run sends the summary in place of what it stands for while the folded lines stay in the file |
| `src/schema.rs` | the JSON Schema subset flint validates a `--schema` answer against, by hand |
| `src/context.rs` | `AGENTS.md` discovery, the skill catalog, the agent profiles in `.flint/agents/*.md`, and the saved prompts in `.flint/prompts/*.md` (`Prompt`, and the one `fill_args` that puts the words typed after a name into `{args}` or after the body — the same rule for a prompt file and an invoked skill) |
| `src/config.rs` | config load/save, the paths under `FLINT_HOME`, and the project's `.flint/` marker that a run looks for but never creates |
| `src/ndjson.rs` | the `--json` stream: one object per line, and the rules that keep it that way |
| `src/search.rs` | web search: where the credential comes from, and DeepSeek's search endpoint |
| `src/fetch.rs` | reading a URL: markup stripped, length bounded, and the one rule — a fetch may only reach the public internet |
| `src/live.rs` | who else is working here: the presence record a run keeps while it lives (at home always, and in the project's `.flint/` when the project has one), the mailbox a peer speaks through — `flint say` from a terminal, `/say` from a run's own prompt, the same write either way — and the recent-changes signal that names no author |
| `src/web.rs` | `--web`: the embedded viewer and the loopback listener that serves it, including `GET /file` — the route that reads a file a tool result named, for the page's preview panel (§12) — `POST /open` — the one route that starts a **process**, handing a path to the program this machine uses for it, refused outright in a `readonly` run and decided by the pure `open_plan` so the command line is testable without opening a window (§16) — and `GET /jobs` with the `event: jobs` frame, the run's own background work for the header's jobs panel (§13). It also owns `export_html`, the other way that page leaves the process: the same `view.html` with a finished conversation welded into it as a JSON island (the session's own lines, `cwd` taken out, `<`/`>`/`&` escaped) — §14 of `docs/web-mode.md` is why there is one renderer rather than two — and the SSE cursor, which is a **position in the session file** (`X-Flint-At`) rather than a count of frames, so a reconnect is answered by the ring when it still holds the gap and by the file when it does not, and a page that dropped for a second is caught up instead of rebuilt (§15) |
| `src/util.rs` | small shared helpers (truncation on a char boundary, and the two-ended cut a long answer gets) |
| `tests/` | `agent_loop` (stub provider), `cli_output` (real binary, raw bytes — including a resumed conversation whose request past `max_request_chars` opens with the note while the file on disk still has the first question, a conversation a live run switches to with `/resume` being *drawn* and not merely loaded, a saved prompt typed as `/name` whose words reach the request while the transcript keeps the line that was typed, `/skill <name>` doing the same for a skill's body, both of them offered to the page as a reading and a send, a follow-up queued mid-turn that waits for the turn it was typed into and survives a stop to become the next question, the same command with nothing running being that turn's message, a fork cut at a chosen question whose branch holds the prefix, records what it was cut from and leaves the original byte-identical, the same lineage written by `--fork` at startup, and a `--no-session` run: no file, a refused `--continue`/`--resume`/`--fork`/`--name`, and the same refusal from `/new`, `/resume`, `/import` and `/fork` typed into a live run, with `/import`'s refusal deliberately set against a file that *would* have imported, and a conversation exported as one HTML file: the island carries the session's own lines with the `cwd` taken out, a line containing `</script>` cannot close the element it is written into, stdout is the page itself when `--out` is absent, a session preceded by a byte-order mark still loses its directory rather than its whole `meta` line, and an empty conversation or a stray `--out` is refused), `json_output` (the `--json` stream: one object per line, the heartbeat, the stop channel, the exit codes and the turn's outcome, the balance preflight, the refusals a caller has to be able to read, and the cache split carried on `usage` and `turn.completed` only when the endpoint reported one), `term_capture` (byte-exact terminal), `search_tool` (stub search endpoint), `balance` (the preflight, stub provider), `who` (the presence record and the changes that name no author), `task` (one flint starting another: argv, the child's stream, the depth bound, a readonly parent that cannot be talked into a writing child, a profile deciding the child's instructions and model, a fan-out whose children are shown to have started together, a child's own progress arriving on the parent's status row, what a dropped turn says about the child it left running -- in a session and on a `--json` stream, a child's conversation being kept out of the person's list of conversations, a `--no-session` parent whose child keeps none either -- and whose handle, status line and collected answer all say so instead of promising a file that is not coming -- and the background handle: a parent that does not wait, a status that says where the child is, a wait that collects its answer, and a stop that ends it, a model that says so when the next step needs the answer, and a job that ends while its parent is working being reported to it exactly once), `say` (the mailbox: a peer's words reaching the person and the session file, and never a request body -- unless the run asked to hear peers, and then exactly once, in the request that followed, with `"heard":true` on the record), `web_view` (the page's policy), `tty_hangup` (Unix only, the one suite that needs a real pty: a child is given a terminal, the master is closed, and the run must end rather than spin -- the defect `watch_for_hangup` in `src/main.rs` exists for) |
| `scripts/` | Node replay tools: `vtscreen.js`, `term-layout-test.js`, `layout-trace.js`, and `browser-controls-test.js` (the one that needs a browser: it drives the page's own controls over the DevTools protocol and checks each press against the run's stdout, run by hand rather than in CI. It starts a scripted model of its own — the same SSE shape the Rust stub serves — so a claim can be about a real turn; the turn it drives writes a file, reads it back, greps it, and starts both kinds of job, which is what the path and jobs claims are made against) |
| `examples/python/` | the Python caller: `flint_call.py` (ask/ask_json, no dependencies), its stub and its checks |
| `examples/mcp/` | flint as an MCP tool for Codex, Claude Code and Cursor: `flint_server.py` (stdlib only, one tool) and `test_mcp.py`, which speaks the protocol at it |
| `docs/windows.md` | field notes on Windows terminal behaviour; read before touching layout |
| `docs/windows-tooling.md` | the plan for command-line escaping on Windows and the measured record of building it — all five steps are in the tree, the parts that needed a real machine were settled on one, and the Unix process-group kill that was the last open item (§6.1) is built and held by a test the ubuntu job watched fail first; read before touching `probe_shell` or adding a tool |
| `docs/session-format.md` | the session file format, for readers and for hand-editing |
| `docs/web-mode.md` | the browser view over a running flint: all three levels are built (read-only view, live feed, composer with a sidebar), and §11–§13 and §16 are the measured record from a real browser — the page's controls, the file preview behind `GET /file`, the jobs panel behind `GET /jobs`, and the OS-level open behind `POST /open` |
| `docs/pi-agent-harness.md` | a reading of Pi (pi.dev), the minimal TypeScript agent harness, for what flint can take from it: where the two already agree, the candidates worth building (a session fork, steering versus follow-up messages, a run-level tool allowlist, compaction written into the session file, cache and cost on screen), and where flint goes the other way on purpose (jobs instead of tmux, a child that is a readable file, no plugin ABI) |
| `docs/deepseek-search.md` | web search: what was measured about DeepSeek's search, and what it costs |
| `docs/decisions.md` | why flint is built this way, decision by decision |
| `docs/features.md` | the inventory of what this build does, door by door, for whoever is checking it: every flag, subcommand, slash command, tool, limit—and the exact string each one answers with. Read it before changing a user-visible surface, and update the row that moved |
| `docs/sandbox.md` | what replacing permission modes with grants would have looked like — **declined in writing on 2026-09-18: not built, not queued**, and it argues against the "Not doing, and why" entry in `ROADMAP.md` on purpose; read it as the record of an alternative that was weighed and refused, not as the state of the tree and not as a plan to start |
| `docs/research-permissions-prior-art.md` | how other coding-agent CLIs do permissions and sandboxing, read off their own pages for `docs/sandbox.md` — every claim is sourced from a page that was loaded, and the ones that could not be verified say so |
| `docs/sandbox-alternatives-research.md` | capability- and scope-based alternatives to the read-only / workspace-write / full-access ladder, read for `docs/sandbox.md` §3.4 — in its own words, not a proposal for the tree |
| `docs/agents.md` | the plan for runs that spawn, find and talk to each other (a `task` tool, presence, a mailbox, profiles) — stages 1–4 are built, including a `task` that hands back a handle by default and reports the job once when it ends, a background command that is the same job with a log file where a child has a conversation, `job_op` (`src/live.rs`, `TaskTool`/`TasksTool`/`JobOpTool` in `src/tools.rs`, the report in `Agent::with_jobs`, profiles in `src/context.rs`, `flint say` — or `/say` in a run's own prompt — and the `--hear-peers`/`/hear-peers` opt-in that lets a peer's words reach a model), and the `.flint/` project marker that lets two `FLINT_HOME`s see each other |
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
| `<FLINT_HOME>/sessions/<dir>/<stamp>-<ms>-<pid>.jsonl` | one conversation per file, append-only, in the directory belonging to the working directory it was held in (older sessions sit directly in `sessions/`, and are still found). A file is created by the first thing said in it, so opening flint and typing nothing leaves nothing; creating it is what claims the name, and the process id in it is what stops two runs that start in the same millisecond from proposing the same one. A run started with `--no-session` writes no file at all — and starts its `task` children with the same flag, so it cannot leave one behind through the door it opened itself |
| `<FLINT_HOME>/sessions/<dir>/children/<stamp>-<ms>-<pid>.jsonl` | the conversation of a run another run started (a `task`/`tasks` child, or the same thing from Python or MCP). A session file like any other — same format, readable, resumable by path — and one level deeper on purpose: no listing reads that directory, so `/sessions`, `--list-sessions`, the page's sidebar and `--continue` show a person their own conversations and nothing else. Its `meta` line names the parent in `parent`. `mv` it up a level to adopt it |
| `<FLINT_HOME>/sessions/archive/` | conversations filed away with `/archive` (a project's archive is `sessions/<dir>/archive/`) |
| `<FLINT_HOME>/spill/<session>/<n>.txt` | tool output too long for one request, in full. `<session>` is the session's file name, or `unattached-<pid>` for a run that keeps no conversation: such a run still has to spill, and a directory of its own is what stops two of them overwriting each other's `1.txt` |
| `<FLINT_HOME>/spill/<session>/background-<tool>-<n>.log` | what a background command (`bash`/`pwsh`/`exec` with `background: true`) has printed, both streams in one file, named in the handle it returns and readable while it is still being written |
| `<FLINT_HOME>/engines/<provider>.log` | a local engine's output, and the only place a failed start says why |
| `<FLINT_HOME>/live/<pid>-<n>.json` | one record per *running* flint, refreshed every 5 s and removed when it exits; what `flint who` reads. A record left behind by a killed process is reported as stale rather than deleted, because a killed process cannot clean up |
| `<FLINT_HOME>/mailbox/<dir-key>.jsonl` | one append-only file per working directory, where `flint say` — or `/say` typed into a run working there — leaves a message for whoever is working there. A run reads what arrives *after* it started and shows it to its person; reaching a model needs that run to have asked (`--hear-peers`, or `/hear-peers on`), because anything that can write a file would otherwise be able to steer a tool loop with no permission layer |
| `<FLINT_HOME>/AGENTS.md` | instructions that apply to every project |
| `<FLINT_HOME>/skills/<name>/SKILL.md` | skills available everywhere |
| `<FLINT_HOME>/prompts/<name>.md` | a saved prompt, sent by typing `/<name>` or `/prompt <name> [args]`. Nothing about it reaches the model until it is sent — no catalog line, no tool schema — so a directory of long templates costs a run nothing |
| `<project>/AGENTS.md` | instructions for that project |
| `<project>/.flint/prompts/<name>.md` | a saved prompt for that project, which wins over the user's copy of the same name |
| `<project>/.flint/skills/<name>/SKILL.md` | skills for that project |
| `<project>/.flint/agents/<name>.md` | an agent profile: front matter for `model`, `provider` and `readonly`, body for the instructions a `task`/`tasks` child starts from. `<FLINT_HOME>/agents/<name>.md` works too, and the project's copy wins on a name |
| `<project>/.flint/live/<pid>-<n>.json` | the same presence record by another route, written only when the project *already* has a `.flint/` directory — flint never creates that marker, so a checkout that has not asked for flint's project state gets none of this. It exists for one reason: two installations with different `FLINT_HOME`s cannot see each other through their homes, and this is the copy they can both read. `flint who` reads both directories and counts one run once, by the file name (pid plus nonce, identical in both places), keeping whichever copy said the later word |
| `<project>/.flint/mailbox.jsonl` | the same mailbox, one file for the whole project rather than one per working directory, so a run in `src/` and a run at the root can hear each other and two installations can too. It *replaces* `<FLINT_HOME>/mailbox/<dir-key>.jsonl` rather than being written beside it: a mailbox is a log of events, and one message in two logs would arrive twice with no way to tell a duplicate from a repetition |

Nothing else — and `live/` is the one directory here that is not a record of the past: a run that
ends removes its own file, and anything left in there is reported as stale rather than cleaned up by
somebody else. The two `<project>/.flint/` rows are the same state by another route and are written
only where a person already put a `.flint/` directory; **flint never creates that marker**, which is
what keeps a checkout nobody asked about untouched — and it means the project's copy can be deleted at
any time without losing anything, because the home's is the one that always exists. A web search keeps
no state on this machine at all: the request goes out, the
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
thinking_field = ""             # the JSON key a reasoning level goes in, e.g. "reasoning_effort".
                                # Empty (or absent) = this endpoint is never asked for reasoning,
                                # whatever level the run is at: vendors disagree about this field
                                # and a wrong guess is a request an endpoint may refuse
```

```toml
default_provider = "deepseek"   # which provider is used without --provider
shell = "cmd"                   # bash tool's shell program (Unix: "sh")
shell_args = ["/C"]             # arguments that run a command string (Unix: ["-c"])
max_tool_output = 30000         # characters of tool output per reply; the rest spills
max_request_chars = 400000      # characters of conversation per request; the oldest turns
                                # are left out of the request past it, never out of the file
max_steps = 100                 # runaway-loop guard, not a work ration
readonly = false                # refuse write/edit/apply_patch and mutating bash
proxy = ""                      # exported to bash children as HTTP(S)_PROXY
verbose = "on"                  # off|on|full: how much of the agent's activity to narrate
tool_detail = false             # print the output behind a tool result
lazy_tools = true               # every request carries the `tools` lookup and a one-line
                                # catalogue, and a tool joins the request from the turn after the
                                # model asks about it. Measured: 822 characters per request against
                                # 10,043 for the whole set
eager_tools = []                # tools to declare on every request anyway. Absent or empty is
                                # the shipped all-lazy shape; paste `bash`, `exec`, `read`,
                                # `write`, `edit`, `list`, `glob`, `grep` for a model that guesses
                                # instead of asking -- measured on a local 9B, which answered
                                # "name" for apply_patch's argument (the real one is `patch`).
                                # Declaring those eight costs 4,355 characters on every request
instructions = "hint"           # AGENTS.md: "hint" (name them), "paste", "off"
skill_dirs = []                 # extra skill directories, after the standard two
thinking = "off"                # off|low|medium|high: reasoning to ask for. "off" sends no
                                # reasoning parameter at all (the endpoint's own default applies).
                                # The conversation's file has the last word over this key, and
                                # --thinking / /thinking over both. Needs the provider's
                                # `thinking_field` to be set, or nothing is sent and flint says so
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
`flint --list-sessions` does the listing without a model (`--json` prints it as one object for a
program, each row carrying the session's path — the field a caller cannot reconstruct, since a
conversation lives in the subdirectory belonging to the directory it was held in). A session file is
one JSON object per line; an unknown event type is skipped in silence, and a line that names a *known*
type but cannot be parsed is reported as damage rather than ignored. `docs/session-format.md` is the
reference for the format itself, including what can be edited by hand.

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
`is_readonly_words` in `src/tools.rs`. **Every door is judged, including the person's two** —
a `!cmd` line typed at the prompt, and the `flint exec` subcommand (where the flag or a
read-only config asks for it) — and `tools::readonly_refusal` is the one sentence all four
print. Until 2026-09-18 only the model's tools were judged, so the banner a read-only run
prints about itself ("writes and mutating commands are refused") was falsified by the line
directly under it. Useful for a first look around an unfamiliar machine;
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
node scripts/web-view-test.js
python examples/python/test_call.py   # the Python caller, against its own stub
python examples/mcp/test_mcp.py        # the MCP server, spoken to over JSON-RPC
```

**Silent here is not silent everywhere, and that has cost a red CI twice.** A lint that depends on a
platform's *signature* cannot fire on the platform whose signature is the other one: `libc::openpty`
takes `*mut` on BSD and `*const` on glibc, so `&mut size` is required on one and
`clippy::unnecessary_mut_passed` on the other — and this machine is the one where it is required.
The same shape bit the tool-payload budget, which was measured on macOS and is 1,734 characters larger
on Windows because that is where `pwsh` lives — a 1,702-character schema plus its 32-character line in
the catalogue. (The delta was recorded as 880 for three commits, because it had been worked out by
subtracting a macOS figure from a stale one rather than measured on Windows at all. A real Windows
machine has now run the test, and its message quotes what that machine measures.) Before pushing a
change to anything under `#[cfg]`, anything calling `libc`, or any test that counts characters, ask
what the *other* two platforms make of it — or read the failing job's annotations first, which name the
test and the reason:

```bash
curl -s "https://api.github.com/repos/tedllll/flint/actions/runs/<run>/jobs"        # job ids
curl -s "https://api.github.com/repos/tedllll/flint/check-runs/<id>/annotations"    # the reason
```

The job log needs a token and is not worth chasing; the annotations carry the panic.

A push runs all six on Linux and Windows (`.github/workflows/ci.yml`), the last two as one step. Those
two resolve the binary `cargo test` just built for themselves — `flint_call._binary` and
`test_mcp.flint_binary` — print which one they ran, and refuse a pass that came from an installed
`flint` on `PATH`, so the log answers which flint it was; nothing is exported to them, because the
windows job's `$PWD` is an MSYS path a native Windows Python cannot open. Only
`scripts/browser-controls-test.js` is left to a person: it needs a real browser. The
runner's log
cannot be downloaded without a token, so a failing job re-emits the failing test's name and its
panic as check annotations — `GET /repos/tedllll/flint/actions/runs/<run>/jobs` for the job ids,
then `/check-runs/<id>/annotations` for the reason. Read that before guessing from the tree.

`flint debug prompt-input` prints the request body that would be sent — system prompt,
history, tool schemas — without sending it or needing a key. It is built by the same
`provider::request_body` the client posts, so it is the honest way to check what the model is
actually given after changing the prompt, a tool schema or the request-side bounds. The
request is *not* the transcript: the session file keeps every byte, pruning drops stale tool
output from the request only, and `trim_old_turns` drops the oldest turns past
`max_request_chars`, with a note in their place.

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
- **`cargo test --lib` does not rebuild `target/debug/flint.exe`**, and the Python and Node
  checks run that binary by preference (they are written against the build in this checkout,
  on purpose). Editing Rust and going straight to `python examples/python/test_call.py`
  tests the previous binary and reports its bugs — measured, and it cost one red herring
  during the session-id fix. Run `cargo build` first.

## Commits and pushes

One concern per commit. The subject is imperative and lower-case after the type
(`fix: ...`, `feat: ...`, `docs: ...`, `refactor: ...`); the body explains the problem and
the decision, not the diff. Commit messages in this repository are long on purpose — they
are where the reasoning lives for the next person.

`origin` fetches over HTTPS and pushes over SSH (`git@github.com:tedllll/flint.git`, a
deploy key named in this repository's `core.sshCommand`). Pushing to `main` is what the
project does; it is a small project with one author, and the commit log is the review.
