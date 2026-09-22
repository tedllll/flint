# What this build does, door by door

A test reference for the current tree — the checklist to work through when checking a build.
`README.md` is the narrative — what the thing is
for; `docs/decisions.md` is why it is built this way; this file is the **inventory**: every
door a person or a program can come through, what to do at it, what comes back, and what is
refused. It is written to be worked through top to bottom with a keyboard.

Every fact here was read off the tree or observed from a run in this checkout: the source for
the command table, the dispatcher, the tool box, the config, the session reader and the web
listener, plus `flint --help`, `flint debug prompt-input` and live runs against a scratch
`FLINT_HOME`. Where a claim is a measured limit rather than a rule it says so, and where
something is deliberately absent it is in §15 rather than left to be discovered.

Two conventions, and they are the whole point of the file:

- **"Held by"** names the test that would fail if the behaviour changed. A claim with no
  test behind it says so; there are a few, and they are marked rather than smoothed over.
- **Exact strings are quoted.** Where a message is shown in `code`, it is the text the
  program actually prints (with `…` for an elided middle). Messages were observed on
  Windows against this checkout's release build; a message that differs on Unix is called
  out where it is known to.

The version this describes is whatever `flint --version` prints in your checkout. The
counts below (commands, tools, flags) are exact for this tree and are the first thing to
re-check after a change.

---

## 1. The three ways in

| Surface | How it starts | For | Keeps a conversation? | Ends when |
|---|---|---|---|---|
| Interactive REPL | `flint` (no arguments) | a person working in a directory | yes, unless `--no-session` | `/exit`, `/quit`, `/q`, Ctrl-D, or a second Ctrl-C within 1.5 s |
| One-shot | `flint -p "<prompt>"`, or `flint <words…>` | a script, a caller, `xargs`, a Makefile | yes, unless `--no-session` | the turn ends; the process exits with a code that says how it ended |
| A subcommand | `flint exec`, `flint who`, `flint say`, `flint balance`, `flint export`, `flint --list-sessions`, `flint debug prompt-input` | doing one thing and stopping | only `exec` and `export` read one; none write one | the subcommand finishes |

A fourth surface — the browser view — is not a fourth program: `--web` (or `/web` inside a
run) serves *this process's* conversation on `127.0.0.1`. It is §12.

The distinction a tester must keep straight throughout: **the answer** is what the model
said; **the transcript** is everything the run drew — your line, each tool call and its
result, notices, and the answer. On the terminal both go to **stdout**; the run's own
errors and warnings go to **stderr** (`flint: error: …`, `flint: warning: …`). Notices that
belong to the conversation are transcript lines on stdout even when they are about the run —
`search: …` at startup and `warning: …` for a provider that could not be configured (§14.2).
Under `--json`, stdout is *only* the NDJSON stream — see §3.4.

---

## 2. The command line

### 2.1 What is parsed, and in what order

`flint` reads the flags, then the config file, then decides which surface to run. Two
things happen before the config is read on purpose and are worth knowing when testing:

- `--version` / `-V` prints the build's number and exits **before** the config is read, so
  a script can ask which flint this is on a machine whose config is broken.
- A flag that cannot be combined with what else was given is refused **at the command
  line**, with `EXIT_USAGE` (2), before anything is sent to a model or written anywhere.

### 2.2 Every flag

| Flag | What it does | Notes |
|---|---|---|
| `-p "<prompt>"`, or bare words | one-shot: ask, print, exit | `flint hello there` is the same as `-p "hello there"` |
| `--continue` | resume the most recent conversation in this directory | refused with `--no-session` |
| `--resume <n\|id>` | resume a particular conversation | `n` is the number `/sessions` prints; a session file path also works |
| `--fork [<n\|id>]` | copy a conversation and continue the copy, leaving the original alone | the copy gets a new file; the original is byte-identical afterwards |
| `--name <text>` | name this conversation | same as `/name`; refused with `--no-session` |
| `--archive <n\|id>` | file a conversation away, out of the list | moves the file into `archive/` |
| `--delete <n\|id>` | delete a conversation file | irreversible; it is a `rm` |
| `--list-sessions` | list conversations, numbered for `--resume` | `--json` prints one object with each session's path |
| `--provider <name>` | use a specific provider | config key `default_provider` is the default here |
| `--model <name>` | override the model for this run | does not write the config |
| `--readonly` | refuse writes and mutating commands | one switch, all-or-nothing: §7.6. `--no-edit` is an accepted alias and is not in `--help` |
| `--thinking <off\|low\|medium\|high>` | ask the provider for reasoning | needs the provider's `thinking_field` to be set, or nothing is sent and flint says so |
| `--hear-peers` | let what `flint say` leaves in this directory reach the model on the next request | off unless asked; needs a session (a one-shot run has no next turn) |
| `--json` | with `-p`: write the run as one JSON object per line | refused without a prompt |
| `--web` | also serve a browser view of this run on `127.0.0.1` | `/web` inside a run is the same thing |
| `--port <n>` | the port for `--web` | default 0 = any free one; refused without `--web` |
| `--cwd <dir>` | working directory for tools | a directory that does not exist is refused rather than created |
| `--schema <file\|json>` | with `-p --json`: require the answer to match this schema | recorded in the session, so `--resume` holds the conversation to the same shape |
| `--no-schema` | answer in prose even if this session's file says otherwise | the in-session counterpart of the key above |
| `--max-seconds <n>` | with `-p`: stop asking after `n` seconds and report `incomplete` | exit 65; bounds the whole run |
| `--result-file <path>` | with `-p --json`: also write the answer there | the file is emptied when the run starts, so it never holds an earlier answer |
| `--no-session` | write no conversation for this run | refuses `--continue`, `--resume`, `--fork`, `--name`, and `/new`/`/resume` inside the run |
| `--no-color` | disable ANSI colour | `NO_COLOR` is honoured too |
| `-h`, `--help` | the usage text | exit 0 |
| `-V`, `--version` | print the build's version and exit | exit 0, before the config is read |

### 2.3 Every subcommand

| Command | What it does | Needs a key? | What it prints |
|---|---|---|---|
| `flint exec <command>` | run a command directly — no model, no network | no | the command's own output; exit code is the command's own |
| `flint balance [--json]` | ask the provider whether it can be used and what is left | yes | the provider's answer; `--json` for a program |
| `flint who [--all] [--json]` | who else is working in this directory (flint only), and what changed | no | the presence list and a changes note that names no author |
| `flint say <text> [--to <pid>]` | leave a message for whoever is working here | no | `said: <text>`, the mailbox file it went into, and whether anybody is here right now |
| `flint export <n\|id\|path>` | write a conversation out as one HTML page and stop | no | the page on stdout, or into `--out <file>` |
| `flint --list-sessions` | list conversations without a model | no | one numbered line each, or one JSON object |
| `flint --archive` / `--delete` | file away / delete a conversation | no | one line naming the file |
| `flint debug prompt-input [msg]` | print the request that *would* be sent, and send nothing | no | the JSON body: system prompt, history, tool schemas |

`flint exec` is the one subcommand that is also a tool the model can call (`exec` in §6.1),
and both go through the same readonly judgement — as do the `!` line a person types and a
read-only *config* read by the subcommand. Four doors, one rule (`tools::readonly_refusal` is
the one sentence they all print; it was not always, and two of the four used to skip the
judgement entirely — see §7.6).

### 2.4 Refusals at the command line — observed, with exit codes

Every one of these is `EXIT_USAGE` (2) unless noted, and goes to **stderr** as
`flint: error: …` — except under `--json`, where the refusal is a JSON object on stdout.

| Invocation | Message observed | Exit |
|---|---|---|
| `flint --nope` | `unknown flag '--nope'. Try --help.` | 2 |
| `flint --json` | `--json needs a prompt: `flint -p "..." --json`. Try --help.` (as `{"message":…,"type":"error"}` on stdout — keys alphabetical, like every frame in §13.5) | 2 |
| `flint --result-file x.txt` | `--result-file needs a prompt and --json: it exists so a caller does not have to read the stream, and with no --json the answer is already everything on stdout (redirect it instead).` | 2 |
| `flint --max-seconds 5` | `--max-seconds bounds a call, and a call is a prompt: give one with -p, or leave the flag off. An interactive session is bounded by whoever is typing at it.` | 2 |
| `flint -p hi --hear-peers` | `--hear-peers relays what a peer leaves between turns, and a one-shot run has no next turn to relay it in: start a session (no -p) and use /hear-peers there.` | 2 |
| `flint --no-session --continue` | `--no-session writes no conversation, and --continue, --resume, --fork and --name all open or name one: give one or the other. A run that keeps nothing cannot continue, copy or name anything.` | 2 |
| `flint --port 4321` | `--port needs --web: it chooses the port the browser view listens on` | 2 |
| `flint --cwd C:\definitely\missing -p hi` | `--cwd C:\definitely\missing: no such directory` | 2 |
| `flint exec` | `exec requires a command` | 2 |
| `flint --readonly exec "echo hi > out.txt"` | `readonly mode is ON: refusing to run `echo hi > out.txt`. `--readonly` and the config's `readonly` key both ask for this; leave them off, or run a program that only inspects.` — nothing runs, and the file is not created | 2 |
| `flint export` | `export requires a session: `flint export <n\|id\|path> [--out <file>]`` | 2 |
| `flint say` | `say requires a message. `flint say "I am editing src/provider.rs"`, with `--to <pid>` to address one run` | 2 |
| `flint --schema '{bad json}'` | `cannot use the schema from '{bad json}': the schema is not valid JSON: …` | 2 |
| `flint --resume 424242 -p hi` | `no session 424242: /sessions lists 0 (1 is the most recent)` | 1 |
| `flint export 999` (no such session) | `no session 999: /sessions lists 0 (1 is the most recent)` | 1 |

The last two are **1**, not 2, and that is deliberate: the command line was fine, the
world was not. `EXIT_USAGE` means "changing the arguments fixes this".

---

## 3. A one-shot run (`-p`)

### 3.1 What happens

1. The prompt is expanded: `@name` in it is replaced by that file's contents (§3.3).
2. The run is built: provider, model, tools, working directory, the readonly guard, a
   session unless `--no-session`.
3. One turn runs: the model is asked, any tools it calls are run, its answer streams.
4. The process exits with a code from §14.1.

The transcript of that turn — each tool row, each notice, the answer — is written to
stdout as it happens, which is why `flint -p "…" > out.txt` gives you both the work and the
answer. A caller that wants *only* the answer uses `--result-file`, and a caller that wants
structure uses `--json`.

### 3.2 Where the prompt comes from

| Input | Shape | Notes |
|---|---|---|
| `flint -p "ask this"` | one argument | the usual form |
| `flint ask this` | bare words | identical; flags may appear before or after |
| `flint -p "$(cat q.txt)"` | the shell's job | flint never reads a prompt from a file by itself |
| `flint -p "@report.md summarize this"` | `@file` inlined | §3.3 |
| `echo "ask this" \| flint` | a piped session | **not** a one-shot: the lines are read exactly as if they had been typed, so flint starts its REPL, answers, and ends at end-of-input. Its exit code is the REPL's (0), even if the turn it ran failed. A program that wants one answer uses `-p` |

### 3.3 `@file` attachments

- `@src/main.rs` is replaced, in the prompt sent to the model, by that file's contents in a
  `<file path="…">` block. The name is resolved against the working directory.
- A name **with spaces** is written quoted: `@"my notes.md"`.
- Trailing punctuation that is prose rather than a name is trimmed before the lookup
  (`. , ; : ! ? ) ] } > " ' \` *`), so `see @notes.txt.` attaches `notes.txt`.
- A name that is not a file is **left alone** — it stays literal text, and nothing is said.
  This is the rule that keeps `@media` and an email address from being an error.
- The file must be UTF-8 text; a binary is refused: `… is not text (it is not valid UTF-8),
  so it cannot go into a prompt.`
- All attachments together are capped at **256 KB**; past it the run is refused before
  anything is sent, naming the file that would have crossed the line.
- The transcript shows `  inlined @notes.txt (812 bytes, 24 lines)` — two spaces of indent
  and the verb, because it is a note about what was just sent rather than a line of either
  voice — and the **session file records the expanded form** — what the model was actually
  given — while the stream and transcript show what you typed.
- Held by: `src/attach.rs`'s unit tests (the cap, the trailing punctuation, the non-file
  case, the byte-order mark being stripped).

### 3.4 `--json`: one object per line

`flint -p "…" --json` writes NDJSON on stdout and nothing else — no status row, no warning
line, no trailing newline of prose. One object per line, each with a `"type"`. The stream
starts with what the run is and ends with what happened; a caller that reads only `type`
values can follow it without knowing flint's grammar. §13.5 is the field-by-field table.

The exit code is the honest summary of the run: `0` complete, `65` the answer did not
arrive whole (`--max-seconds` expired, or a `--schema` that never matched), `130`
interrupted (`/stop` or a Ctrl-C), `69` the provider cannot be used, `1` something else.

With `--schema`, the answer is validated by flint's own subset validator (`src/schema.rs`);
the `result` line carries the validated object, and `--result-file` writes the same thing —
the answer text, or the object — to a file.

---

## 4. The interactive terminal

### 4.1 What you see at startup

Two lines, then a blank one:

```
flint v0.1.0  <provider>/<model>  <working directory>
  /help for commands · type while it works to interrupt it
```

The second line is replaced by one of two others when they matter more:

- `readonly — writes and mutating commands are refused` (green) when the guard is on,
- `no API key for this provider — shell tools still work. Set one with /provider key <key>,
  or add a provider with /provider add` (yellow) when there is no key.

Below that is the input row, prefixed `> `. In a wizard (`/provider add` with no arguments,
`/config edit`) the same row carries that question's own prefix instead.

### 4.2 The keyboard

flint reads raw keys itself, so the terminal's own line editing is not in play. This is the
whole map:

| Key | What it does |
|---|---|
| any character | inserted at the cursor |
| `Backspace` / `Delete` | delete before / at the cursor |
| `Left` / `Right` / `Home` / `End` | move the cursor |
| `Tab` | inserts **a space** (there is no completion in the input row) |
| `Esc` | clears the line |
| `Enter` | submits the line |
| `Up` / `Down` | **nothing** — there is no input history in the row; the conversation above is the history |
| `Ctrl-C` on a non-empty line | clears the line |
| `Ctrl-C` on an empty line | stops the running turn, keeps the process (§4.3) |
| `Ctrl-C` twice within 1.5 s on an empty line | quits |
| `Ctrl-D` | quits |

Held by: `src/term.rs`'s key tests, and `tests/term_capture.rs` for the drawn result.

### 4.3 While a turn is running

- A clock appears **after 300 ms**, on its own row above the input, centred:
  `── 12s read ──` — the elapsed time and what is happening. What is happening is one of
  three words, and which one is not a guess: the tool's name while a tool runs (`bash`,
  `read`), `writing the answer` once the first token of the reply has arrived, and
  `waiting for the model` before that, when nothing is named yet. It repaints once a
  second. (`writing the answer` is the one that shows most of the time and the one this
  list was missing, for a while.)
- The answer streams into an **answer strip** of three rows just above that clock, and rows
  move up into the transcript as they are finished — so the transcript is never rewritten
  under you.
- Each finished tool call is one transcript line: `⏵ read` while it runs, then `✓ read 121
  lines` or `✗ read …` in red with the failure's own words. Output itself is not printed
  unless `/detail on`.
- A long tool result is summarised and the full text is written to
  `<FLINT_HOME>/spill/<session>/<n>.txt`. The transcript's one-line summary is
  `✓ read … 9 lines` — **it does not name the file**; what names it is the copy of the
  result the model is given, which keeps both ends and says
  `[435 characters; kept the first 200 and the last 100; full output: <path>]` in the
  middle. That line is printed here too when tool detail is on (`/detail on`), because it
  is part of the result rather than decoration on it — but the summary is not where to look
  for the path, and this section used to say it was.

### 4.4 Typing while it works

This is the interaction most worth testing, because it is the one that surprises people:

| What you type mid-turn | What happens |
|---|---|
| a plain sentence | the in-flight request is **cancelled**, `⏹ interrupted` is printed, your line is echoed as `> <text>`, and it becomes the next turn. This is steering. |
| `/queue <text>` | **does not interrupt**: it is held and sent when the current turn ends. The transcript shows `queued for after this turn: <text>`. A bare `/queue` prints a usage line instead. |
| `/stop` | stops the turn without clearing the process — the same as Ctrl-C on an empty line. Mid-turn it is deliberately *not* handed back to the command dispatcher, so you do not get "nothing is running" after it; with nothing running it *is* handed there, and answers `nothing is running`. |
| any `/command`, `!command`, or a saved-prompt name | the turn is dropped and the line is handed to the command dispatcher |
| a report the page asked for | answered after the model finishes, never mid-turn (one writer in the transcript) |

**A dropped turn does not lose what was written.** An interrupt is a dropped future — the code
that would have turned the step's text into a message never runs — so the run commits the
answer it had already drawn to the session file itself (`Agent::commit_drawn_answer`). That is
the reported fault it exists for: without it the person reads half an answer on screen, asks
about it, and the model answers as if it had never been written. A steering line then becomes
the next turn's question, recorded like any other.

Held by: `tests/cli_output.rs` (a queued follow-up waiting for the turn and surviving a
stop; `/queue` with nothing running being that turn's message), `tests/agent_loop.rs`.

### 4.5 When the turn ends

The footer that says what the turn cost:

```
[ctx 12480 prompt + 512 completion = 12992 tokens, 71% cached]
```

and nothing is appended when the endpoint reported no cache split. `/usage` prints the same
numbers with a sentence about what they mean.

### 4.6 The status of a run that is a child

A run started by `task` shows the child's own progress on its status row, and a child that
ends while the parent is working is reported to the parent exactly once.

---

## 5. Slash commands

43 rows in the table that drives `/help`, the dispatcher and the page's menu — 41 slash
commands plus `!command` and `/stop`, which live in the table's second section because they
are about a *line typed while it works* rather than commands. Aliases: `/exit`, `/quit`, `/q`
are one row, and `/help`, `/?` are one row.

**The page class** column is what the browser view draws for that row (§12): `panel` shows
the listing rather than sending the command; `button` is one action with no argument;
`selector` takes its value from a list the page already has; `form` takes free text;
`toggle` is already on the page from another field; `danger` is a button behind a second
press; `terminal` is not offered on the page at all.

### 5.1 Models and settings

| Command | What it does | What you see | Page | Notes and refusals |
|---|---|---|---|---|
| `/provider` | list the providers | a listing, one per endpoint, with the active one marked | panel | — |
| `/provider <name>` | switch to one | the switch, and the engine is started/stopped if it is local | selector | an unknown name is refused |
| `/provider add <name> <base_url> [model]` | set up a provider | with no arguments it asks for each part, one question per line | form | a name that replaces an existing provider says so; a new non-local endpoint with no key warns `no key yet — set one with /provider key, or the request will fail` |
| `/provider edit <name>` | change one, interactively | the same questions | form | — |
| `/provider key <key>` | set the API key for the active provider | `saved …` | form | the key is **not** echoed; the row is redacted on the page. A blank key is refused |
| `/provider rm <name>` | delete one | one line naming what went | danger (second press) | refuses the last provider: `refusing to delete the last provider — there would be nothing left to talk to` |
| `/model` | show the model in force | four lines: which provider and endpoint the list belongs to, one line per model with the active one starred, the `/model <name>` usage, and where to add more | panel | — |
| `/model <name>` | switch to one | the switch | selector | the conversation is kept, and a `switch` event records the move |
| `/config` | show shell, steps, proxy | a listing of the settings that matter, plus the config path | panel | — |
| `/config edit` | change shell, steps, proxy | interactive questions | form | — |
| `/config set <key> <value>` | change one setting **and use it now** | `key = value` | form | an unknown key is refused by name, listing the keys it takes; `max_steps = 0` is refused; the running tools are rebuilt |
| `/thinking [off\|low\|medium\|high]` | how much reasoning to ask for | the level, and whether the endpoint has a field for it | toggle | a level with no `thinking_field` on the provider says nothing is sent |
| `/readonly [on\|off]` | toggle the write guard | the new state | toggle | bare toggles; the guard is all-or-nothing (§7.6) |
| `/verbose [off\|on\|full]` | how much of the agent's activity to narrate | the new level | toggle | — |
| `/detail [on\|off]` | print tool output, or one line per result | the new level | toggle | — |
| `/hear-peers [on\|off]` | send what a peer says here to the model | the new state, and the warning that a peer's words can steer a tool loop | toggle | anything other than `on`/`off` is refused |
| `/reload` | re-read the config file (after editing it yourself) | what changed | button | re-reads `AGENTS.md` and the skill catalog too; the agent is rebuilt around the new config, and the conversation is kept in the same file |

### 5.2 Context and accounting

| Command | What it does | What you see | Page | Notes |
|---|---|---|---|---|
| `/usage` | context and token accounting | `last request:  prompt N  completion N  total N tokens  cache N% (N of N prompt tokens)`, plus a sentence that prompt tokens are the current context size | panel | with nothing reported yet: `no usage reported yet by this provider` |
| `/compact` | fold the earlier part of the conversation into a summary the model writes | `asking <model> to summarize the N message(s) before the newest question…`, then `ok folded N message(s) into a N-character summary`, then a sentence saying every message is still in the file and deleting the `compact` line puts it back | panel | one request; with one question in the conversation: `nothing to compact yet…`; with `--no-session` it explains there is nowhere to record a fold |
| `/tools` | list the tools this run has | one line per tool | panel | the list is the run's own, so it is shorter without a search credential or skills |

### 5.3 Jobs and peers

| Command | What it does | What you see | Page | Notes |
|---|---|---|---|---|
| `/jobs` | the background work this run started | the same listing the model gets, one line per job, running first | panel | — |
| `/jobs stop <pid>` | end one of them | the stop's own words | danger (second press), candidates from the jobs panel | a non-numeric pid is refused; a bare `/jobs stop` is refused because a person naming a kill has to name the target; a second word is refused |
| `/say [--to <pid>] <text>` | leave a message for whoever else is working in this directory | `said: …`, the mailbox file, and whether anybody is here | form | `--to` addresses one run by pid; the flag is a flag rather than positional, so a pid cannot be mistaken for prose |

### 5.4 Instructions, skills, prompts, profiles

| Command | What it does | What you see | Page | Notes |
|---|---|---|---|---|
| `/skills [name]` | list skills, or print one **as the model would see it** | the catalog, or one body | panel | re-discovered at the moment you ask, so "did it load what I wrote" is answerable without asking the model |
| `/skill <name> [args]` | send a skill's instructions as your next message | the turn, and a note naming the file it came from | selector | — |
| `/prompts [name]` | list your saved prompts, or print one as it would be sent | the names, or one body | panel | — |
| `/prompt <name> [args]` | send a saved prompt | the turn | selector | typing `/<name>` at the prompt is the same thing |
| `/agents [name]` | list agent profiles (`.flint/agents/*.md`), or print one | the profiles, or one body | panel | — |
| `/<name>` (not a built-in) | a saved prompt, sent | the turn, and a note naming the file | — | a built-in always wins, so a template called `help` loses to `/help`. An unknown word: `unknown command '<x>'. /help for the commands, /prompts for your saved prompts.` |

### 5.5 Conversations

| Command | What it does | What you see | Page | Notes |
|---|---|---|---|---|
| `/sessions` | list conversations, numbered | `  1. <id>  <summary>` lines, then `/resume <number\|id> to continue one of these`; with none: `(no sessions yet)` | panel | reads only the ends of each file, so it is fast on a large history |
| `/resume <n\|id>` | switch to one | `resumed: <file> (N messages, model M)`, then the **transcript of that conversation is drawn**, then a note if it was a copy of another | selector (the sidebar's rows) | a bare `/resume` prints usage; `--no-session` refuses it; a fresh system prompt is rebuilt rather than the file's being trusted |
| `/fork [n]` | start a new conversation cut at question `n` | with no argument, the questions are listed so you can pick; then the new conversation, with the original left byte-identical | selector | a turn boundary is the only place a conversation can be cut |
| `/import <file>` | copy a conversation in from a session file | `imported: <file> (N messages) into <new file>`, then its transcript | form | the source is never written to; importing the file this run is writing is refused; a file with no messages is refused |
| `/export <file>` | write **this** conversation out as one HTML page | `wrote <path> (N bytes)` | form | the path is an argument because stdout is the terminal; a write that fails is reported and the run carries on; with nothing said yet: `nothing to export yet — this conversation has no file until something is said in it` |
| `/name [text]` | name this conversation | bare: `name: <title>` or `(unnamed)` — including for a conversation that has said nothing yet, which has no file to read a title from; with text: `named: <text>` | form | the page's sidebar is told, so a rename shows up there |
| `/archive <n\|id>` | file one away, out of the list | `archived <path>` | danger | refuses the conversation you are in; the page's list is refreshed |
| `/delete <n\|id>` | delete one | `deleted <path>` | danger | same refusal; irreversible |
| `/new` | start a fresh conversation | `started a new session` | button | `--no-session` refuses it with the one sentence every such door uses |

### 5.6 The terminal's own

| Command | What it does | Page | Notes |
|---|---|---|---|
| `/help` (or `/?`) | the command list | panel | — |
| `/web [port]` | open the browser view of this conversation | terminal | the page *is* this view |
| `!<command>` | run a shell command without the model | terminal | the output goes to the transcript; `!` is a shell escape, not a tool call; in a `readonly` run it is refused unless the command is inspection only, by the same rule as `bash` (§7.6) |
| `/stop` | the interrupt as a word | terminal | for ssh, a pipe, or the browser's composer, where a key is not available |
| `/exit` (`/quit`, `/q`) | quit | terminal | a misclick on the page must not end a session |

---

## 6. The tools the model can call

The set is built per run, so the model's list is not always the same:

| Always | Windows only | When available |
|---|---|---|
| `tools`, and whichever of the others the model has asked about | `pwsh` | `search` when a search credential resolves; `skill` when there is a skill to load |

**With `lazy_tools` on — the default — one request carries the `tools` lookup and a one-line
catalogue of everything else.** A tool joins the request from the turn after the model asks about
it. Measured, counting each tool's name, description and schema: **822 characters per request
against 10,043** for the whole set, in a run that offers `search` and on a machine that has no
`pwsh`. The catalogue is the only tool text paid for on every turn, so the budget test holds this
number to 1,000 — about a hundred and fifty characters of room on Windows, where the number is
largest, and no more.

The catalogue names each tool and gives it four or five words — that is the text paid for on every
turn, so it is deliberately not where detail goes. A tool's own description is paid for only from the
turn it is unlocked, and its schema arrives with it; the lookup's *answer* does not repeat either,
because the next request already carries them and repeating them would move words to the more
expensive place to save the model nothing.

A call to a tool this request did not declare is refused **and told where the arguments are** — the
refusal names `tools` and the tool — because a wrong-argument call is the evidence that the model
guessed, and the refusal is the moment it is willing to listen. Once the tool is declared the pointer
is not repeated: the schema is in front of it by then.

**Measured with `deepseek-flash`, against the shape that declares eight tools**, on four tasks:

| | all-lazy | eight declared |
|---|---|---|
| "what arguments does `apply_patch` take?" | 1,713 (it **called `tools`**, answered `patch`) | 2,874 (the same) |
| read a file | 1,401 | 2,651 |
| list + read + grep | 2,093 (it asked about all three) | 2,781 |
| **a real `apply_patch` edit** | **1,885 — the file came out right** | 3,099 — the file came out right |

Cheaper on every one of them, same results. `eager_tools` is the other answer for a model that
guesses rather than asking — a local 9B answered "`name`" for `apply_patch`'s argument (the real one
is `patch`) without looking it up — and `lazy_tools = false` sends everything every time, which is
what every version before this did.

`search` is *not* offered when it cannot work — a tool that always fails costs a schema on
every request — and flint says why at startup instead. `flint debug prompt-input "<anything>"`
prints the list this run would send, which is the honest way to check it.

### 6.1 Running things

| Tool | Arguments | What it does | Gating and limits |
|---|---|---|---|
| `bash` | `command` (required), `timeout_secs`, `background`, `download` | runs a shell command line, both streams combined | default timeout 120 s, or 900 s when the command installs, builds, downloads or is background; `download` means "not killed on a total budget, only if it stops producing output"; under `readonly` only inspection is allowed (§7.6) |
| `exec` | `program` (required), `args[]`, `stdin`, `timeout_secs`, `background` | runs one program with its arguments already separate — no shell, so quotes and `$` arrive intact | same timeouts; shell syntax does not work here; under `readonly` allowed only for inspection, judged from the program and its verb |
| `pwsh` | `script` (required), `args[]`, `stdin`, `timeout_secs`, `background` | runs a PowerShell script, written to a real `.ps1` and run with `-File` | **refused outright under `readonly`**: "a script is arbitrary code and flint cannot judge one" |
| `job_op` | `action` (`status`/`output`/`wait`/`stop`), `pid`, `timeout_secs` | sees what became of a job this run started | §8 |

### 6.2 Reading and writing files

Every path argument in this table resolves the same way (`config::resolve_path`, and the same rule
for the page's `GET /file`/`POST /open` and a person's `@name`): a leading `~/` or `~\` is the **home
directory**, an absolute path is used as written, and anything else is relative to the run's working
directory. Two shapes are deliberately *not* a home — `~user/x` is somebody else's, and `~notes.txt`
is a file whose name begins with a tilde — so both stay as written and the reader reports what it
looked for. A machine that will not say where home is leaves the path alone rather than resolving it
against the process's own directory.

| Tool | Arguments | What it does | Gating and limits |
|---|---|---|---|
| `read` | `file_path` (`path` is an alias), `offset` (1-based, default 1), `limit` (default 2000 lines) | the file with line numbers | a directory is not a file: the operating system's own refusal comes back wrapped as `cannot read <path>: …`; a large file is paged with `offset`/`limit` |
| `write` | `file_path`, `content` | creates or completely overwrites; parent directories are created | **read-before-mutate**: an existing file must have been read by this run, and is refused if it changed on disk since. Under `readonly` it is refused |
| `edit` | `file_path`, `old_string`, `new_string`, `replace_all` | replaces an exact string | `old_string` must appear exactly once unless `replace_all`; the file must have been read |
| `apply_patch` | `patch` | add/update/delete across files, all or nothing | same gate; the format is `*** Begin Patch` … `*** End Patch` |
| `list` | `path` | directory entries | — |
| `glob` | `pattern` (required), `path` | files by name pattern, recursively | — |
| `grep` | `pattern` (required), `path`, `glob`, `ignore_case` | file contents, with names and line numbers | a literal string, not a regex |

Windows-only traps the tools already know about and a tester should not be surprised by: a
name Win32 will not use literally (`NUL`, a trailing dot or space, `name:stream`, `a|b`) is
refused with the reason rather than silently writing somewhere else.

### 6.3 Reading the web

| Tool | Arguments | What it does | Boundary |
|---|---|---|---|
| `fetch` | `url` (required) | a page's text: markup stripped, length bounded, and the result says where it came from | **the public internet only** — loopback, the private ranges and link-local (where cloud metadata lives) are refused |
| `search` | `query` | a web search, returning sources | only when a credential resolves; billed as input tokens. `docs/deepseek-search.md` is the measured record |

`fetch`'s boundary is around *that tool*, not around flint: `bash` can reach whatever the
machine can.

### 6.4 Children

| Tool | Arguments | What it does |
|---|---|---|
| `task` | `prompt` (required), `background`, `cwd`, `model`, `provider`, `readonly`, `schema`, `timeout_secs` | one whole other flint, in its own context, that cannot see this conversation |
| `tasks` | `tasks[]` (required), `max_parallel`, `cwd`, `timeout_secs` | several of them at once, answers labelled in the order asked |

The properties a child inherits and the ones it cannot escape:

- **Endpoint and provider**: handed down, so a child talks to the same place.
- **`readonly`**: a readonly parent cannot be talked into a writable child.
- **`--no-session`**: travels, so a run that keeps no conversation starts children that keep
  none either.
- **Depth**: `MAX_TASK_DEPTH = 2` — past it the child is refused with the reason and told to
  do the work itself.
- **Profile**: `.flint/agents/<name>.md` (or `<FLINT_HOME>/agents/<name>.md`) can decide the
  child's instructions, model and readonly, and the project's copy wins on a name.
- A child's conversation lives in `children/` and is **not** in any listing of *your*
  conversations; `task` returns a handle, and `background: false` is for when the next step
  needs the answer.

---

## 7. The shape of a turn

### 7.1 Every call in one assistant message runs at once

The message is the unit: if the model asks for three tools in one reply, the three run
concurrently and their results are reported in the order they were asked for. This is why
the read-before-mutate gate exists — concurrent writers are safe because a writer has to
have read what it overwrites.

### 7.2 The runaway guard

`max_steps` (default 100) bounds whole model calls per turn. It is a guard against a loop
that never ends, not a ration; `/config set max_steps 0` is refused with that sentence.

### 7.3 Retries

A failed completion is retried up to **4 attempts** in total, waiting **1, 2 and 4 seconds**
between them. The waits are the ladder's first three rungs and not the whole of it: with four
attempts there is no fourth wait, so the `8` and `16` this section used to name could never
happen (measured: 1 s + 2 s + 4 s, about 7.7 s of wall clock, then the failure below).

Each retry is a notice, and it goes to **stderr** — `flint: provider 'x' returned HTTP 500 … --
retrying in 1s (attempt 2/4)`. It is not a transcript line and it is not a `--json` frame; the
only place a program sees that a retry happened is `provider_retries` on `turn.completed`,
which counts them over the whole run. The sentence that used to say both things said one of
them wrongly — "on the terminal it appears in the transcript; under `--json` it is a frame",
followed two clauses later by "which is the only place a retry is visible to a program". The
second half was the true one.

When the four attempts are gone the run fails with
`gave up after 4 attempts -- the network or the provider stayed unreachable: …` and exit **75**
(`EXIT_TEMPFAIL`: asking again later is right).

### 7.4 What bounds a request

| Bound | Default | What happens past it |
|---|---|---|
| `max_tool_output` | 30000 characters | the full output is written to `spill/<session>/<n>.txt`, the reply keeps both ends and names that file |
| `max_request_chars` | 400000 characters | the oldest turns are left out of the *request*, with a note in their place; the file keeps every byte |
| `--schema` | — | the answer is validated by flint's own subset validator; a schema that never matches ends the run as `incomplete` (65) |

### 7.5 `/compact` and the request

A fold is one line in the session file: a summary plus the **byte offset** of the first
`chat` line still sent. The newest such line wins. Nothing is deleted, and removing that
line by hand puts the conversation back whole.

### 7.6 `readonly`, exactly

All-or-nothing, and judged where it can be:

- `write`, `edit`, `apply_patch` are refused.
- `pwsh` is refused entirely, and the refusal is the whole sentence rather than a fragment of it:
  `readonly mode is ON: refusing to run a PowerShell script. A script is arbitrary code and flint
  cannot judge one; use a read-only `bash` or `exec` command, or turn readonly off with /readonly.`
  No classifier is asked about a language it does not know.
- `bash` is refused unless the command line is inspection only. The judgement is a string
  one: anything containing `>`, `>>`, `&&`, `||`, `;`, `|`, a backtick, `$(` or an
  elevation word (`sudo`, `doas`) disqualifies it, and what is left is judged by program and
  verb.
- `exec` is judged from the program and its verb, not from a command line.
- **The person's own doors are judged too**, which they were not until 2026-09-18: the `!` line
  typed at the prompt (by the `bash` rule above, because a line is what it is) and the
  `flint exec` subcommand (by the `exec` rule, from the flag or from a read-only config). The
  banner a read-only run prints — `readonly — writes and mutating commands are refused` — was
  true of the model's tools and false of the line directly under it, and its own words are the
  reason the two doors were brought in rather than the sentence being narrowed.

The refusal is a **guard against mistakes, not a boundary**: `readonly` is useful for a first
look around an unfamiliar machine, and it is not a sandbox. `docs/features.md` §15 lists
what is deliberately absent; `AGENTS.md` has the doctrine.

### 7.7 The commands a run starts

A command the run starts is told which run it is in: `FLINT_SESSION`, `FLINT_PROVIDER`,
`FLINT_MODEL` and the proxy variables are set on both spawn sites, and a run that is not a
conversation has them *taken away* rather than left as inherited. The proxy handed to
commands is the top-level `proxy` key, which is a different route from a provider table's
own `proxy`.

---

## 8. Jobs: work that outlives the call

A **job** is one of two things: a `bash`/`pwsh`/`exec` command started with
`background: true`, or a `task`/`tasks` child that was not waited for. Both are the same
record, read by three doors — the model (`job_op`), a person (`/jobs`, `/jobs stop <pid>`),
and the page (its jobs panel) — which is deliberate: one answer, three readers.

| Stage | What happens |
|---|---|
| started | the tool call returns at once with a pid and (for a command) a log file path |
| running | the page's jobs panel shows a row badged with its kind, what was asked, and a duration ticking once a second; the model can poll `job_op status` |
| ending | a job that ends leaves a notice in the parent's transcript exactly once, and its row keeps the exit code in words (`exit code 0 (finished)`) and how long it took |
| stopped by us | `ended_by_us` is recorded before the signal goes out, so a kill never reads as a failure — the page's state word is `killed`, and the words beside the exit code say `killed` rather than `unknown` for the signal code `-1` and for a child's own `130`. The three doors read one judgement: the page's `status`, the row `/jobs` draws, and the sentence `job_op`/`wait` hands back |
| being stopped | a flag set when the stop is asked for is read against `finished`, so a row can say "on its way out" rather than "still working" |

- A background command's log is `<FLINT_HOME>/spill/<session>/background-<tool>-<n>.log`,
  both streams in one file, readable while it is still being written.
- `job_op output` reads what a command has printed **since it was last asked**, without
  collecting it; `wait` collects; `stop` ends it — the fourth verb, and the one whose name is
  the same in all three doors.
- `task` children appear in the same list; pressing a child's row opens the child's own
  conversation rather than a log.
- What a job leaves behind when the parent's turn is dropped is recorded rather than lost:
  the child is still running, and the parent says so — with its pid, and with the path its answer
  will be written to once the child has named its conversation (`It has not named its conversation
  yet.`, plus `job_op`, while it still has not). One sentence, one full stop: it read `…again..`
  until the caller stopped punctuating a sentence that already ended in one.

---

## 9. Other runs in the same directory

Two flint runs can see each other and leave each other messages, and none of it needs a
server.

| Piece | Where it lives | Who writes it | Who reads it |
|---|---|---|---|
| presence | `<FLINT_HOME>/live/<pid>-<n>.json`, and `<project>/.flint/live/…` when the project already has a `.flint/` directory | each running flint, refreshed every 5 s, removed on exit | `flint who`, `/who`-style listings, the page's peer picker |
| mailbox | `<FLINT_HOME>/mailbox/<dir-key>.jsonl`, or `<project>/.flint/mailbox.jsonl` for the whole project | `flint say`, or `/say` in a run | the run's reader: shown to the person always, to the model only if that run asked |

- A presence record left by a killed process is reported as **stale**, not deleted: a killed
  process cannot clean up.
- A project's mailbox **replaces** the home's for that directory rather than being written
  beside it — one message must not arrive twice.
- `/say <text>` and `flint say <text>` are the same write.
- A message is **not** sent to the model unless that run has `/hear-peers on` (or was
  started with `--hear-peers`), and then exactly once, in the request that follows, with
  `"heard":true` on the record. Without it a peer's words are shown to the person and go no
  further. The reason is in the flag's own help: anything that can write a file would
  otherwise be able to steer a tool loop with no permission layer.
- `flint who` distinguishes what it can prove from what it cannot: `live`/`stale` are flint
  runs only, and `changed` is what the filesystem shows and names no author — another agent,
  an editor and a person all look the same.

---

## 10. Instructions, skills, saved prompts, profiles

| Thing | Discovery | How it reaches a run |
|---|---|---|
| `AGENTS.md` | `<project>/AGENTS.md` and `<FLINT_HOME>/AGENTS.md` | the `instructions` config key: `hint` (name them), `paste` (inline them), `off` |
| skills | `<project>/.flint/skills/<name>/SKILL.md`, `<FLINT_HOME>/skills/<name>/SKILL.md`, plus `skill_dirs` | the catalog (names and summaries) is in the system prompt; the **body** is loaded only by the `skill` tool or `/skill <name>` |
| saved prompts | `<project>/.flint/prompts/<name>.md`, `<FLINT_HOME>/prompts/<name>.md` | nothing about them reaches the model until sent — no catalog line, no schema, so a directory of long templates costs a run nothing. Sent by `/<name>` or `/prompt <name> [args]` |
| agent profiles | `<project>/.flint/agents/<name>.md`, `<FLINT_HOME>/agents/<name>.md` | a `task`/`tasks` child starts from that body, with the profile's `model`, `provider`, `readonly` |

A prompt file and an invoked skill use the **same** argument rule: the words typed after the
name go into `{args}`, or after the body when there is no placeholder.

---

## 11. Conversations

### 11.1 The file

One JSON object per line, append-only, at
`<FLINT_HOME>/sessions/<dir>/<stamp>-<ms>-<pid>.jsonl`, where `<dir>` belongs to the working
directory the conversation was held in. It is hand-editable on purpose: an unknown event
type is skipped in silence, and a line that names a *known* type but will not parse is
reported as damage rather than ignored. Creating the file is what claims the name, so
opening flint and typing nothing leaves nothing behind. `docs/session-format.md` is the
reference; §13.4 lists the events.

### 11.2 The lifecycle

| Act | Door | Effect |
|---|---|---|
| start | speaking any line | a new file with that first event |
| list | `/sessions`, `flint --list-sessions [--json]` | numbered, most recent first |
| continue | `--continue`, `--resume <n\|id>`, `/resume <n\|id>` | appends to the same file; `/resume` also draws the transcript |
| copy | `--fork [n]`, `/fork [n]`, `/import <file>` | a **new** file; the source is not written to. `--fork` copies the whole tail, `/fork n` cuts at question `n`, `/import` copies a file this run never started from |
| provenance | — | a copy records what it came from (`import`, `fork` events), so a reader who finds it later is not looking at a conversation that began from nothing |
| name | `--name <text>`, `/name [text]` | one appended line; the newest name is in force |
| fold | `/compact` | one `compact` line naming the summary and the offset; the file keeps everything |
| archive | `--archive <n\|id>`, `/archive <n\|id>` | the file moves into `archive/` — a `mv`, so undoing it by hand is the same operation backwards |
| delete | `--delete <n\|id>`, `/delete <n\|id>` | the file is removed. There is no undo |
| export | `flint export <n\|id\|path> [--out <file>]`, `/export <file>` | one self-contained HTML page with no network and nothing about your machine in it |
| none at all | `--no-session` | no file, no listing entry, nothing to continue from |

### 11.3 Children are not in your list

A conversation another run started lives one level deeper, in `children/`, and **no listing
reads that directory** — so `/sessions`, `--list-sessions`, the page's sidebar and
`--continue` mean "mine". The file is a session like any other: readable, resumable by
path, and `mv`-ing it up a level adopts it.

### 11.4 The export

An export is the same page `--web` serves, with the conversation welded into it as a JSON
island: one renderer, two ways to get it. Two things it deliberately does not carry: the
directory the conversation was held in, and any request to anywhere. A line containing
`</script>` cannot close the element it is written into. `flint export` owns stdout when
`--out` is absent — `flint export 3 > page.html` is the whole invocation — and needs no key.

### 11.5 What a one-shot run does with a session

`-p` writes a conversation by default, so `flint -p "fix the build"` can be followed by
`flint --continue`. `--no-session` is for a caller that wants a run and no trace of it, and
it still writes what it needs to work (spilled output, a background command's log) under a
name of its own in `spill/unattached-<pid>/`.

---

## 12. The browser view (`--web`)

### 12.1 Opening it

`flint --web` (or `/web [port]` inside a run) serves a view of **this process's** conversation on
`127.0.0.1`. What it prints is a URL with a token in it: `http://127.0.0.1:<port>/?token=<32 hex>`. A
browser opening on a machine with a terminal gets a tab opened for it; a run whose stdout is not a
terminal does not.

Two modes, and the difference decides half the controls:

| Mode | How it happens | What you get |
|---|---|---|
| **served** | the URL has a token and the scheme is not `file:` | the live view: composer, sidebar, controls, the event stream |
| **a dropped file** | open `view.html` itself, or an exported page | the conversation is drawn from the JSON island in the file; **no composer, no sidebar, no controls**, and nothing is fetched |

### 12.2 What the listener will and will not answer

Four checks run before any route, in this order, and each refusal is a `403` with one sentence:

1. **`Host`** must be `127.0.0.1:<port>` or `localhost:<port>`. An absent `Host` fails.
2. **`Origin`**, when present, must be `http://127.0.0.1:<port>` or `http://localhost:<port>`. Absent is
   allowed, because `curl` sends none.
3. **The token** must match: in the `x-flint-token` header on every route, or — **only on `GET /`** — as
   `?token=`. The token is in the URL exactly once, for the page to read; every route that can change
   something takes it in a header.
4. Every response carries `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
   `Referrer-Policy: no-referrer`, and a `Content-Security-Policy` of `default-src 'none'` with inline
   script and style, `connect-src 'self'`, and `img-src 'self' data: blob:` — the last of which is what
   lets the preview draw a picture the page fetched with its own token (§21 of `docs/web-mode.md`).

| Route | Body rules | Answer |
|---|---|---|
| `GET /` | — | the page (`text/html`); 405 for any other method |
| `GET /session` | — | the session file verbatim (`application/x-ndjson`), with `X-Flint-At` = the cursor **only when this run has a live feed *and* there is a file to point into** — a cursor is an offset in the session file, so a run whose conversation has not been written yet answers `200` with an empty body and no `X-Flint-At`, and the header appears the moment the first line does; `404` when the run keeps no conversation |
| `GET /sessions` | — | `{"sessions":[{"n","id","label","current"}]}` |
| `GET /file?path=<p>[:<line>]` | `path` required, percent-encoded; resolved by `config::resolve_path` — a leading `~/`/`~\` is the home directory, an absolute path is used as written, anything else is against the run's `cwd`; the literal path is tried first, then the `:N`-stripped one | the file's bytes (`text/plain`, cut at 512 KB with `X-Flint-Cut`/`X-Flint-Size`), or the route's own refusal: a directory, a file over 64 MB, a non-UTF-8 file, or nothing there |
| `GET /image?path=<p>` | `path` required, percent-encoded; resolved exactly as `GET /file` resolves it | the file's **bytes** with the type its own first bytes declare: `image/png`, `image/jpeg`, `image/gif`, `image/webp`, `image/bmp`, `image/x-icon`, `image/tiff`, `image/avif`, `image/heic`, `image/svg+xml`. The name is *not* evidence — a JPEG called `photo.png` is served as `image/jpeg`. Refusals: a directory, nothing there, bytes that are no picture at all (`<p> is not a picture this page can draw (png, jpeg, gif, webp, bmp, ico, tiff, avif, heic and svg are)`), and **over 24 MB** (`<p> is 41 MB -- too big to show here; open it where it lives`). The four security headers ride along; the body is bytes rather than text (the one route besides `/events` that is not a `Response`) |
| `GET /jobs` | — | `{"jobs":[…]}` — the same record `/jobs` and `job_op` read |
| `GET /peers` | — | `{"peers":[{pid,cwd,provider,model,readonly,session,age_secs}]}` |
| `GET /events?last=<cursor>&session=<file>` | the cursor may also arrive as `Last-Event-ID` | the SSE stream: unnamed frames for session lines, named `state`/`status`/`answer`/`reset`/`jobs`/`sessions`/`file`, `: ping` every 20 s. `404` when this run has no live feed |
| `POST /message` | a JSON body `{"text":"…"}`, at most 1 MiB | `202 {"queued":true}`; the text is typed into the run **exactly as the terminal would receive it** — the REPL decides whether it is a command, a `!` escape or a prompt. `409` when there is no prompt to type into (a one-shot run) or the run is shutting down; `400` for a non-JSON body, a missing `text`, or an empty one |
| `POST /report` | the same | the same, except the line is a **reading**: the page shows the answer and the terminal prints nothing |
| `POST /open` | a JSON body `{"path":"…"}` | hands the path to the program this machine uses for it: `200 {"opened":"<path>","with":"explorer"|"open"|"xdg-open"}`. `409` in a **`readonly`** run (the guard is read from the run, not from a copy taken at bind time); `404 nothing at <path>: …` when there is nothing there; `500` when the launcher itself fails; `400` for a non-JSON body, a missing `path`, or an empty one. The path resolves exactly as `GET /file` resolves it, `~` included |
| `POST /log` | a plain line, not JSON | `200 logged`; writes `<FLINT_HOME>/web.log`, whitespace-collapsed, cut to 500 chars, capped at 1 MB |
| anything else | — | `404 no such route` |

An unparseable request head is `400 not an HTTP/1.1 request`; a header block over 8 KiB is `431`; a body
over 1 MiB is `413`.

### 12.3 The controls

Everything the page offers is drawn **from the frames the run sends** — the page contains no command
name, no toggle name and no provider list of its own, so a control that appears is a control this run
really has. That is a policy (`tests/web_view.rs`), not a style: a second reader of the same state can
disagree with the process.

The header keeps **one** control — a door — and everything that changes the run is behind it, in a
settings dialog: the two pickers, the switches, the run's own action buttons, and the command list
(§22 of `docs/web-mode.md` is the reasoning and the measured record). A header holding the raw
controls is a header that takes the reading column's width for furniture, which is what this replaced.

| Area | Control | What pressing it does |
|---|---|---|
| header | the conversation's name, above everything | not a control: the session file's own newest `title` event, else the label `GET /sessions` chose (the newest name, else the first thing said, else `(empty)`). Clipped with the whole of it in the tooltip |
| header | the status chips | not controls either: `N running` (or `N stopping`), `N subagent(s)`, `N failed`, `N done`, counted from `GET /jobs` by kind. The whole line is **hidden when there is no work**, and a failure is never folded into `done` |
| header | `#settings-open` | the one door: opens the settings dialog, and is **hidden when no `state` frame has arrived** — a page opened from a dropped file has no run to describe |
| settings | `#settings-close`, the mask, `Escape` | three ways out of the same dialog: its own `close` button, a press anywhere on the mask, and `Escape`. Closing returns the keyboard to `#settings-open`, and opening moves it to `#settings-close` |
| settings | the rail (`run`, `commands`) | one section at a time; the one in force is marked `aria-current="true"`. A section is the page's own word for *a place it put things* — what is inside it still comes from the frame |
| settings | `#pick-provider`, `#pick-model` | sends `/provider <name>` / `/model <name>`; a refusal reverts the picker from the frame |
| settings | one `<select>` per toggle | sends `/<toggle> <value>` — `verbose`, `detail`, `readonly`, `hear-peers`, `thinking` |
| settings | an action button | sends the frame's own line, e.g. `/reload`, `/new` |
| settings | the run's `cwd`, session id and creation time | not controls: the `meta` line the header used to carry, in the run's own section |
| settings | the command list, in five groups — `reports`, `actions`, `selectors`, `forms`, `destructive` | a **report** row sends nothing: it asks for a *reading* (`POST /report`) and shows the answer in place of the list, with a `‹ commands` button back. Any other row sends the line the frame gives it |
| settings | a **destructive** row | first press opens its choices and sends nothing; the second press sends `<send> <value>`. The choices are the list the frame names: conversations (the sidebar's numbers), providers (their names), or jobs (the pids the jobs panel is showing). With nothing to choose from it says `nothing to choose from` |
| settings | a **form** row (`/provider add`, `/provider key`, `/config set`, `/import`, `/export`, `/name`, `/queue`) | one field per argument the frame declares; a `password` field is drawn masked and emptied after a send; an answer the frame does not mark optional must be filled or the line is not sent |
| settings | `/say`'s `--to` picker | the options are the live runs sharing this directory, fetched from `GET /peers`: `(everyone here)` first, then `pid N · <model> · <read-only> · here now` — or, with nobody here, the fact that it waits in the file |
| header | the jobs panel | the chips above it are its summary; the panel itself is one row per job this run started: kind, what was asked, how long it has been going (ticking once a second), and its exit code in words once it ends. Pressing a row opens its log or the child's own conversation in the preview. A job that has been asked to stop counts as `running`/`stopping` — it is still spending time — and is not offered in the `/jobs stop` menu a second time |
| sidebar | `+ new` | sends `/new`; disabled for the round trip so one click cannot start two conversations |
| sidebar | a conversation row | sends `/resume <n>` — the current row does nothing |
| sidebar | a row's `⋯` menu | that conversation's destructive rows (`/archive <n>`, `/delete <n>`), each behind the same two-press rule, and — on the current row only — a `/name` field |
| composer | the textarea + `#send` | sends the text; `Enter` sends, `Shift+Enter` is a newline. The box clears only on success, and only if it still holds what was sent |
| composer | typing `/` as the first character | opens the **`/` menu** above the box — the frame's own `state.commands`, drawn like the settings list (a heading per class, a row per command with the line, its label and its help), and **closed on a page with no run**. It filters as you type after the slash (a prefix first, then a subsequence of the command name, then a word in its help, the frame's order breaking ties), and it closes the moment the line has a space in it — a menu over a sentence being written is covering the words |
| composer | `ArrowDown` / `ArrowUp` in the menu | moves the mark one row at a time, wrapping at both ends; the marked row is the one `Enter` takes, and hovering a row with the pointer moves the mark to it. A query with no match says `no command matches /<q>` rather than drawing an empty box |
| composer | `Enter` in the menu | exactly what the marked row's **class** allows and no more. A **report** asks for its reading (`POST /report`) and shows it in the settings dialog's commands section: the query is **cleared** rather than left in the box (a report is read, and a leftover `/help` would send on the next `Enter` the very line the route exists to keep out of the transcript), the dialog opens as the reading is asked for, and nothing reaches the transcript. An **action** completes the line and sends nothing. A **selector** completes the line and, when the frame named values, offers them as a second list where taking one finishes the line. A **form** opens settings *at that row* and leaves the box **empty** — a credential typed into the composer is sent to the run and written into the session file, which is the whole reason that class has a dialog. A **destructive** row completes the line and stops there |
| composer | `Escape` in the menu | puts the menu away and leaves the text exactly as it was — a list closed is not a line cleared |
| composer | the stop button | shown only while a turn runs; sends `/stop`, so a half-written message in the box survives |
| transcript | a tool block | a `<details>`: the summary is the verb and the arguments, the body is the arguments and the output |
| transcript | a `button.path` | opens the preview panel at that file, and at that line when the token named one. The button **reads what the token said** — `src/web.rs:412`, `src/web.rs#L412`, `C:/x.js:42` — so a `grep` hit keeps the line it is worth reading for; a `file://` prefix is dropped on the way, since the scheme is the one part of the token the panel does not need (§18 of `docs/web-mode.md`) |
| transcript | a web address | a link in a **new tab** with `rel="noopener noreferrer"`; only `http`/`https` become links (§11 of `docs/web-mode.md`) |
| transcript | `thinking`, `instructions` | `<details>` blocks, both closed to begin with |
| preview | `reload`, `close` | re-reads the route that drew it — `GET /image` for a picture, `GET /file` for text; closing leaves the transcript and the reader's place alone, and lets go of the picture's blob URL |
| preview | a picture (`button#preview-image-button` around `img#preview-image`) | drawn from the bytes `GET /image` served, as a blob URL the page made from them — never a URL carrying the token, which is why the page fetches it itself. `max-width`/`max-height` with `object-fit: contain`, so the whole picture fits the column. A **press opens it where it lives** (`POST /open`, the same act as the header's `open` button and refused in `readonly` the same way). A type this browser cannot decode says so in words and points at `open` |
| preview | — | which route is tried is decided by the path's **name** (the extension set), because the page cannot sniff bytes it has not fetched: `/image` first for a name that looks like a picture, then `/file`. So a `.png` that is really a note reads as the note, and a `.svg`/`.heic`/`.tiff` that the browser will not draw offers the OS instead. The note is the route's own `Content-Type` and `Content-Length`, never the extension |
| preview | `open` | hands the panel's path to the program this machine uses for it (`POST /open`): a file opens in whatever its type is registered to, a directory in the file manager. The one control on this page that starts a process, so it is a **deliberate second press** and never the path itself. Disabled when the run is `readonly`, with the reason in its tooltip; the route refuses it there anyway. The answer — or the route's refusal — appears in the hint under the composer |
| preview | — | a refusal is shown in the route's own words with its status beside it, never as an empty panel |
| preview | — | every line of a text file is drawn with the **file's own number** in a gutter to its left, so the line a `grep` hit named can be found by eye. The number is furniture rather than text: `aria-hidden`, and `user-select: none`, so a block copied out of the panel does not come out with `12\t` in front of it. A wrapped line keeps its number beside its first visual line and its continuation in the code column, and the gutter widens for the whole file at once when the numbers need the room. Opening a path that named a line scrolls that line's **own row** to the top — the row's position, not `(line - 1) × line-height`, which was wrong as soon as a line above it wrapped |
| preview | `source`, `rendered` (`button#preview-render`) | switches a `.md`/`.markdown`/`.mdown`/`.mkd`/`.mdx` file between the page's **reading** of it and the file's own bytes. Only drawn for a file whose name says Markdown that was read as text; the label names the view a press would show, `aria-pressed` names the one in force, and the note beside the path ends in `· rendered` when a reading is on screen. A press **re-reads the route** rather than keeping a copy of the bytes (§24 of `docs/web-mode.md`) |
| preview | — | the reading is block-level only: headings (ATX and setext), fenced code with its language, nested bulleted/numbered lists, blockquotes, thematic breaks, pipe tables with the alignment their divider asks for, and paragraphs (their wrapped lines joined with a space). **Inline syntax is not parsed** — `**bold**`, `` `code` `` and `[text](url)` are the characters the file holds — and raw HTML is text, because this page never assigns markup. Anything the reader does not recognise is a paragraph, never a guess |
| preview | — | a `.md` file **without** a line opens rendered, and **with** a line opens raw (`previewView`): a `grep` hit or a compiler error names a line, and "line 412" has no meaning in a reading. A file with any other extension is text, and a picture is neither |
| layout | the two grip handles | drag to resize the sidebar and the reading column; `ArrowLeft`/`ArrowRight` move the boundary by 16 px (48 with Shift); a **double-click puts the width back to the stylesheet's**. Neither width is persisted |
| anywhere | `Escape` | puts away **one** thing, front to back: the `/` menu first (it lives in the composer, where the keyboard already is), then the settings dialog, then the preview panel, then the jobs list. One press per thing, and the order is a function rather than three listeners, so "which one does this close?" has an answer that can be read (`dismissTopmost`). With nothing open it does nothing |

The page handles exactly six keys itself: `Enter` in the textarea (and in the `/` menu, where the two
arrows move the mark), the two arrows on a focused grip, and that one `Escape` order. Everything else —
a `<summary>` opening, a `<select>`, `Tab`, typing — is the browser's own behaviour.

### 12.4 What the page deliberately cannot do

Held as policies over the served bytes in `tests/web_view.rs`. The scanner is blunt on purpose: a test
that understood JavaScript would be a second parser to be wrong about.

- **No markup assignment**: `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write` and `eval(`
  are all forbidden, as is any Markdown renderer (`marked(`). Every string goes through `textContent`.
- **Nothing external**: no absolute URL anywhere (`http://`, `https://`, `//cdn`), no `<script src`,
  no stylesheet link, and `XMLHttpRequest` — a *subresource* is the page itself reaching off the
  machine. The link this build added brought two spellings of the same mistake with it, and both are
  refused: `fetch("http`, `src="http`, `href="//`. A link the reader presses is not the page reaching
  out.
- **`window.open(` is forbidden**: an address opens in a new tab as a link, and a path opens in this
  page's own panel. The one thing that leaves the page and starts a program is `POST /open`, pressed
  deliberately (§12.3).
- **The token is never stored**: no `localStorage`, no origin-wide store; the only stored thing is the
  reading position, in `sessionStorage`.
- **Presence is read through `GET /peers`**, never derived from files by the page (`scan_in(` is
  forbidden).
- **A row the page cannot press says where its control is** (`this one is a button in settings`,
  `typed in the terminal`, …) rather than offering a control that would be refused.

### 12.5 Measured, and honestly not

- **Driven in a real browser**: `scripts/browser-controls-test.js`, **93 claims** held, each checked
  against the run's own stdout. It prints what it drives before it presses anything, and its "not driven
  here" list is part of the output: a report asked for mid-turn (measured in `tests/cli_output.rs`), the
  `/prompt` row's send button, an OS open that *succeeds* (it would start a program on this machine; the
  command lines are held by `src/web.rs`), a paste, an IME, a screen reader, two tabs, touch, a phone
  viewport.
- **Pure functions**: `scripts/web-view-test.js` (the splitter, the frames, the form composition, the
  peer picker's text, the Markdown reading — `markdownBlocks` is lines in and blocks out with no DOM, so
  a fence, a nested list and a pipe table are checked without a browser — and `POST /open`'s body and
  guard).
- **Two controls the current harness does not press**: `+ new` and the stop button were measured in
  earlier passes by other harnesses, and the `/say --to` picker is held by bytes and Node checks rather
  than by a browser press. All three are recorded as the narrower-but-true statement, not as coverage.
- **Residues worth knowing before filing anything**: a file being *previewed* is plain text, so a URL
  inside it is not pressable — and a `.md` file is *also* shown as plain text when the person presses
  `source`, or when it was opened at a line, so the reading is never the only thing the panel can show; a
  real OS-level launch is never performed by a test (the command lines are
  asserted, the spawn is not driven); a
  native `<select>`'s open popup belongs to the operating system and no test can reach into it; the tab's
  own title is not updated (the exported page's is); and `POST /log` writes a line on every boot of a
  served page, whether or not `?debug=1` is used.

## 13. Configuration, environment and state on disk

### 13.1 `config.toml`

At `<FLINT_HOME>/config.toml`, created on the first run that needs a config and rewritten as a whole
file every time a setting changes — so a comment you add by hand is lost the next time `/readonly`,
`/verbose`, `/detail`, `/config set`, `/model <name>` or any `/provider …` writes it. `flint exec`
loads the file **only if it exists** and never creates one.

Two keys are **required**: `default_provider` and `providers`. A file missing either fails to parse
(`cannot parse config <path>`) rather than taking a default — which is worth knowing because the
documentation says missing keys take their default. Inside a `[[providers]]` table, `name` and
`base_url` are required too.

| Key | Default | What reads it | Door, other than editing the file |
|---|---|---|---|
| `default_provider` | **required** | which provider a run uses | `/provider <name>` |
| `shell`, `shell_args` | `cmd` + `["/C"]` (Unix: `sh` + `["-c"]`) | the `bash` tool | `/config set shell …`, `/config edit` |
| `max_tool_output` | 30000 characters | where a tool result is cut and spilled | **file only** — no flag, no command, and `/config` does not print it |
| `max_request_chars` | 400000 | where old turns are left out of a request | **file only** |
| `max_steps` | 100 | the runaway guard | `/config set max_steps <n>` (`0` refused) |
| `readonly` | false | the write guard | `--readonly` for one run (**not saved**), `/readonly on\|off` (**saves it**) |
| `proxy` | none | handed to the commands a run starts | `/config set proxy <url>` |
| `verbose` | `on` | how much is narrated | `/verbose off\|on\|full` (**saves**) — and `--verbose` is **not** a command-line flag; typed at the prompt it is translated |
| `tool_detail` | false | whether tool output is printed | `/detail on\|off` (**saves**) |
| `instructions` | `hint` | how `AGENTS.md` reaches the prompt (`hint`, `paste`, `off`) | **file only** |
| `skill_dirs` | none | extra skill directories, after the standard two | **file only** |
| `thinking` | `off` | the level to ask for | **file only** — `--thinking` and `/thinking` write the *session*, not this key |
| `search` | absent | the `search` tool's credential and endpoint | **file only** |
| per provider: `name`, `base_url` | **required** | the endpoint | `/provider add`, `/provider edit` |
| per provider: `model` | `""`, and an empty one is refused at startup | the model | `/model <name>` (**saves**) |
| per provider: `models` | none | the extra choices the pickers offer | **file only** |
| per provider: `api_key`, `api_key_env` | none | the credential; the named variable wins over the literal key | `/provider key <key>` |
| per provider: `start`, `stop`, `start_timeout_secs` | absent / `0` (meaning 180 s) | a local engine being brought up and let go — absent means "derive it from the name", `""` means "not flint's to manage" | **file only** |
| per provider: `proxy` | none | how flint reaches the *model* (a different route from the top-level one) | file only |
| per provider: `thinking_field` | `""` | the JSON field a reasoning level goes in; with none, nothing is sent | file only |
| `[search]`: `enabled`, `provider`, `base_url`, `model`, `max_uses`, `api_key`, `api_key_env` | enabled, DeepSeek's Anthropic endpoint, `deepseek-flash`, 1 | the `search` tool | **file only** |

The keys with **no door** are the ones marked file only above; of them `max_tool_output` and
`max_request_chars` are the two a tester is most likely to want, and `/config` does not even print the
first. `/reload` is what makes a hand edit take effect in a running session.

### 13.2 Environment variables

| Variable | Read by flint | Notes |
|---|---|---|
| `FLINT_HOME` | every path helper, per call | the whole state root; the tests point it at a scratch directory. Empty is ignored |
| `NO_COLOR` | once per run | any value disables colour, as does `--no-color`; colour is also off when stdout is not a terminal |
| the variable named by a provider's `api_key_env` | every key resolution | the variable wins over `api_key` in the file; a blank value is not a key |
| the variable named by `[search] api_key_env` | every search | same rule |
| `FLINT_PARENT` | once, at startup | set **only** by the `task`/`tasks` tool on the child: names the parent in the child's `meta`, and files the child's conversation under `children/` |
| `FLINT_DEPTH` | per call | set only by the tool; the depth bound is 2 |
| `FLINT_BIN` | per child | which binary a child is started from, instead of `current_exe()` |
| `PATH` | per call | whether a recognised engine's program and the shell can be found |
| `FLINT_TERM_CAPTURE`, `FLINT_TERM_CAPTURE_FILE`, `FLINT_TERM_SIZE` | debug builds only | force the interactive path into a file, which is how the terminal tests see real bytes |

Written for the commands a run starts, and read by **those commands**, not by flint:
`FLINT_SESSION`, `FLINT_PROVIDER`, `FLINT_MODEL` (removed rather than inherited by a run that has none
of its own) and the proxy variables `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` in both cases. Two traps:
flint's own HTTP clients ignore the machine's proxy environment entirely — only the top-level `proxy`
key is used — and the `task` tool strips the three capture variables from a child.

### 13.3 Files under `FLINT_HOME`

| Path | What it is | Editable while flint runs? |
|---|---|---|
| `config.toml` | the configuration | **no** — every saving command rewrites the whole file |
| `sessions/<dir>/<stamp>-<ms>-<pid>.jsonl` | one conversation, append-only | yes — an appended line is picked up by the next load |
| `sessions/<dir>/children/…` | a conversation another run started | yes; `mv` it up a level to adopt it |
| `sessions/<dir>/archive/…` | filed-away conversations | yes; `--resume` searches archives |
| `spill/<session>/<n>.txt` | tool output too long for one request, in full | yes; **nothing reads these back automatically** |
| `spill/<session>/background-<tool>-<n>.log` | a background command's both streams | yes, while it is still being written |
| `spill/<session>/script-<n>.ps1` | the script a `pwsh` call ran (Windows) | yes |
| `spill/unattached-<pid>/…` | the same, for a run with `--no-session` | yes |
| `engines/<provider>.log` | a local engine's output — the only place a failed start says why | yes |
| `live/<pid>-<n>.json` | a running run's presence record, refreshed every 5 s, removed on exit | no — an edit lasts at most 5 s; a record whose writer died is reported **stale**, never cleaned |
| `mailbox/<dir-key>.jsonl` | messages for one directory | yes |
| `web.log` | what the served page logs about itself, capped at 1 MB | yes; nothing in flint reads it (not in `AGENTS.md`'s table) |
| `AGENTS.md`, `skills/`, `prompts/`, `agents/` | your own instructions, skills, prompts, profiles | yes — read per discovery, so `/reload` picks up an edit |
| `<project>/.flint/…` | the same presence, mailbox, skills, prompts and profiles, for a project that already has a `.flint/` directory | yes; **flint never creates that marker** |

### 13.4 The session file, event by event

One JSON object per line, `type`-tagged, append-only, `FORMAT_VERSION = 2`. Eleven types:

| `type` | Written when | Fields | What a reader does with it |
|---|---|---|---|
| `meta` | the first line of a new file | `v`, `id`, `created`, `cwd`, `provider`, `model`, `parent` | the file's identity; a second one wins |
| `chat` | every message added | `message`: `role`, `content`, `reasoning`, `tool_calls`, `tool_call_id` | becomes history |
| `import` | `/import <file>` | `from`, `from_id`, `messages` | provenance; **not** history |
| `fork` | `--fork`, `/fork <n>` | `from`, `from_id`, `kept` | provenance; **not** history |
| `peer` | a mailbox message arrived | `from`, `text`, `at`, `heard` | shown, recorded, and **kept out of history** on load |
| `usage` | the endpoint reported counts | `usage.{prompt_tokens, completion_tokens, total_tokens?, cache_hit_tokens?}` | `/usage` on resume |
| `title` | `--name`, `/name` | `name` | the conversation's name; last wins |
| `switch` | a provider or model switch | `provider`, `model` | so `--resume` believes the file; last wins |
| `schema` | `--schema` / `--no-schema` | `schema` (`null` clears) | holds a resumed conversation to the same shape |
| `thinking` | `--thinking`, `/thinking <level>` | `level` | the level in force |
| `compact` | `/compact` | `summary`, `from` (byte offset of the first `chat` line still sent) | the fold; last wins, and the folded lines stay in the file |

An **unknown** type is skipped in silence — a *well-formed* object naming a type this build has
never heard of, which is how the format grows. Everything else that will not parse is **damage**,
and it is reported (`N unreadable line(s) skipped in <path>`): a known type whose fields do not
fit, a torn tail, and a line that is not JSON at all — which is what a pretty-printed object
leaves behind. The page draws a damage notice rather than dropping the line.

Hand-editable, concretely: appending a well-formed known line is safe (`title`, `switch`, `schema`,
`thinking`, `usage`); deleting lines from the bottom is safe; deleting a `compact` line puts the
conversation back whole. Deleting the first `meta` line is **not** safe — the file then loads with no
`cwd` and no model, so `--continue` can no longer match it to a directory. Pretty-printing a line is
damage: the reader is line-oriented. A byte-order mark is **not** damage — `notepad` and
`Set-Content -Encoding utf8` put one on the first line, and it is stripped there and in the listing,
because an encoding's mark is not content.

### 13.5 The `--json` stream, frame by frame

One object per line on stdout, keys alphabetical, flushed per line. Fifteen types are in the
vocabulary; fourteen can appear on a `--json` run's stdout, and the fifteenth (`command`) belongs to the
page's stream:

`session.started` (`session`, `cwd`, `model`) → `warning` (`message`) → `turn.started` (`prompt`, plus
`attachments` when `@file` inlined something) → `message.delta` / `reasoning.delta` (`text`) →
`tool.started` (`id`, `name`) → `tool.args` (`id`, `name`, `arguments` — the raw string) →
`tool.completed` (`id`, `name`, `ok`, `output`) → `message.completed` (`text`) → `usage`
(`prompt_tokens`, `completion_tokens`, `total_tokens`, plus `cache_hit_tokens` only when the endpoint
reported a split) → `status` (`text`, `restarted`, and `elapsed_secs` on a heartbeat, every 5 s) →
`error` (`message`, plus `code`/`retryable` when it is classified) → `result` (`json`, `attempts`, only
with `--schema`) → `turn.completed` (`prompt_tokens`, `completion_tokens`, `outcome`, `duration_ms`,
`provider_retries`, plus `reason` for an unfinished turn (`"seconds"` or `"steps"`), plus
`cache_hit_tokens`).

Three details a caller has to know, and each has cost somebody a parser:

- **A run can carry more than one `turn.started` and more than one `message.completed`** — the schema
  repair loop re-asks. `result.attempts` says how many attempts there were.
- **A peer's message produces no frame**, and `command` frames belong to the page's stream, not to a
  `--json` run.
- **`flint say --json` is the one machine-readable object with no `type` field**
  (`{mailbox, from, to, text}`); `who`, `balance` and `--list-sessions` all carry one.

### 13.6 Exit codes

See §14.1. Under `--json` the code and the last `turn.completed.outcome` always agree: `complete` →
0, `incomplete` → 65, `stopped` → 130; a provider that cannot be used at all → 69.

## 14. Failure modes

### 14.1 Exit codes

| Code | Name | Meaning |
|---|---|---|
| 0 | `EXIT_OK` | the run finished with an answer |
| 1 | `EXIT_FAILURE` | the failure nothing else classifies: a session number that does not exist, a provider that could not be configured, `balance` when the endpoint's answer does not decide anything |
| 2 | `EXIT_USAGE` | the command line is wrong; nothing was asked of the model |
| 65 | `EXIT_DATAERR` | the answer is not usable as it stands: a schema that never matched, or a turn that stopped early (`--max-seconds`) |
| 69 | `EXIT_UNAVAILABLE` | the provider cannot be used at all (no key, auth, insufficient balance); retrying changes nothing |
| 75 | `EXIT_TEMPFAIL` | a failure worth retrying, by a caller that can wait |
| 130 | `EXIT_INTERRUPTED` | `128 + SIGINT`: the run was cut short |

`flint exec` is the exception: its exit code is the command's own.

### 14.2 The kinds of message

| Prefix / shape | Where | Meaning |
|---|---|---|
| `flint: error: …` | stderr | the run could not start or could not continue. A provider with no key is the common one, and it carries the fix: `error: provider '<name>' has no API key.` then `Fix it without leaving flint:` and three lines naming `/provider key`, `/provider add` and the environment variable to set. Exit 1 |
| `flint: warning: …` | stderr | something was not recorded (a presence record, a spill file) and the run carries on |
| `warning: …` | transcript | a provider that could not be configured, said once something can be read rather than taking the process down before the terminal existed. **Not** the same prefix as `flint: warning:` — this one is a line of the transcript |
| `search: …` | transcript | why the `search` tool is not on offer this run, said once at startup; otherwise a `[search]` block missing its key is indistinguishable from a flint with no search at all |
| `refused: <what was asked>` | transcript | a line the run would not take from the page's report route; the command is named |
| `unknown command '<x>'. /help for the commands, /prompts for your saved prompts.` | transcript | a word that is neither a command nor a saved prompt |
| `✗ <tool> …` | transcript | a tool failed; the failure's own words follow |
| `⏹ interrupted` | transcript | a turn stopped **by a line you typed**: the line is steering, so it interrupts and then becomes the next question |
| `stopped -- the model is not running any more` | stderr (a `notice`; in the transcript when the terminal is the one that was drawn) | a turn stopped by `/stop` or Ctrl-C. A different sentence for a different fact: nothing follows this one, where a steering line is the question that comes next |
| `queued for after this turn: <text>` | transcript | `/queue` did what it says |
| `this run keeps no conversation (--no-session), so <cmd> has nothing to open. Start flint without the flag to keep one.` | transcript | the one sentence every door that would open a conversation gives |

### 14.3 A cancellation is said out loud

A stopped turn says so, in the words the stop deserves — `⏹ interrupted` when a line you
typed did the stopping (that line is the next question), `stopped -- the model is not running
any more` when `/stop` or Ctrl-C did — and a stopped turn's queued follow-ups are named as
not coming (`dropped_queue`), because a cancelled turn and a turn that finished with
nothing to say look identical otherwise. Both sentences are in §14.2, and the difference
between them was measured: a `/stop` mid-answer printed the notice and no `⏹`, which this
section used to claim for every stop (2026-09-18).

---

## 15. Deliberately not in this build

Not bugs, and not to be filed as such. Each is a decision with a reason in the tree:

- **No permission layer.** No approval prompts, no sandbox, no allow-list, no undo. A
  command the model asks for runs as your user, immediately. `readonly` is the only switch
  and it is all-or-nothing (§7.6). `docs/sandbox.md` argues for replacing this; it is **not built and
  not queued**, and `ROADMAP.md`'s "Not doing, and why" is the position of record. That alternative was
  offered to the author and **declined in writing on 2026-09-18** — keep the default, which is all
  permissions — so the one contradiction this inventory used to record as an open item is closed in
  favour of the plan of record, and the plan document now labels itself an argument that lost.
- **One switch, not a ladder.** A narrower permission setting than `readonly` is refused in
  `docs/decisions.md`.
- **No OS-level open without a press.** A file preview is the page's own panel (`GET /file` for text,
  `GET /image` for a picture), and a path opens *outside* the page only through `POST /open` — a
  deliberate second press in the panel's head (§12.3) or a press on the picture itself (§21), refused
  outright in a `readonly` run, and never the plain click on a path. What the page still does not do is
  open the preview panel's *contents* anywhere: text there is plain text.
- **No image or PDF *viewer* beyond what a browser draws**, and no client build step: the page asks
  `GET /image` for the bytes and hands them to an `<img>`, so the formats it can show are the browser's
  own — the route serves tiff and heic too and says so when the tab cannot draw one.
- **No Markdown renderer *library*, and no client build step**: the page reads the block syntax itself
  (`markdownBlocks`, ~120 lines) and everything it draws is `textContent` in one file. **Inline syntax is
  deliberately not parsed** — `**bold**` and `` `code` `` come out as written, and a link in a previewed
  document is not pressable — so a rendered view is a reading, never a claim to be a Markdown
  implementation; `source` is one press away, and a file opened at a line opens raw (§20 of
  `docs/web-mode.md` is the assessment and §24 the build).
- **No history navigation in the input row** (`Up`/`Down`), and no tab completion.
- **No TUI.** The terminal view is inline — a scroll region, an answer strip and a status
  row — not a full-screen application, and that was decided rather than unfinished.
- **No plugin ABI and no MCP server inside flint** (flint is an MCP tool for other agents
  instead: `examples/mcp/`). A Unix pty test suite exists but cannot run on Windows without a
  pty, which is why CI runs it on Linux.
- **No index, cache or database.** Everything on disk is the truth, and every state file is
  hand-editable.

## 16. Where each claim is held

Work through this list and you have covered the surface; each file name is a suite whose
tests name the behaviour they hold.

| Area | Held by |
|---|---|
| the tool loop, concurrency, the gate | `tests/agent_loop.rs`, `src/agent.rs`'s tests |
| raw CLI bytes, refusals, queued follow-ups, exports, forks, imports, `--no-session` | `tests/cli_output.rs` |
| the `--json` stream, heartbeats, exit codes, the balance preflight | `tests/json_output.rs`, `tests/balance.rs` |
| the terminal: layout, scroll, the strip, the status row | `tests/term_capture.rs`, `scripts/term-layout-test.js`, `scripts/vtscreen.js` |
| children: argv, the child's stream, depth, profiles, background handles | `tests/task.rs` |
| the mailbox and presence | `tests/say.rs`, `tests/who.rs` |
| the page's policy | `tests/web_view.rs` |
| the page's routes, including the launcher's command lines (`POST /open`) | `src/web.rs`'s tests |
| the page's controls, driven in a real browser | `scripts/browser-controls-test.js` (93 claims) |
| the page's pure functions | `scripts/web-view-test.js` |
| the Python and MCP callers | `examples/python/test_call.py`, `examples/mcp/test_mcp.py` |
| the one suite that needs a real pty (Unix) | `tests/tty_hangup.rs` |
| the tool payload's cost, the catalogue's size, the unlock, and that no description carries source formatting | `src/tools.rs`'s tests (`the_tool_payload_stays_within_its_budget`, `no_description_carries_source_formatting`, `a_parameter_shared_by_two_tools_is_described_once`) |

## See also

- `README.md` — the same product, told as a story, section by section.
- `docs/web-mode.md` — §11–§16 are the measured record of the browser view, including §16's `POST /open`.
- `docs/session-format.md` — the session file, event by event, for readers and hand-editors.
- `docs/windows.md`, `docs/windows-tooling.md` — terminal and escaping field notes.
- `ROADMAP.md` — what is built, what is queued, and what is deliberately refused.
- `HANDOFF.md` — the state of the tree as the last session left it, with current counts.
