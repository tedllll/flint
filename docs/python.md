# Driving flint from Python

flint is a program with a machine interface, so Python does not need a library to use it: it needs
to start the process, read the JSON lines and hand them back. That is the whole of
[`examples/python/flint_call.py`](../examples/python/flint_call.py), which is a single file with no
dependencies, written to be copied into your own project and read before it is trusted.

```python
from flint_call import ask, ask_json

turn = ask("1+1 等于几？", cwd="/path/to/project")
print(turn.answer, turn.session)

day = ask_json("MA2610 的最后交易日是哪天？", cwd="/path/to/project", schema={
    "type": "object",
    "properties": {"last_trading_day": {"type": "string"}},
    "required": ["last_trading_day"],
})
print(day["last_trading_day"])
```

Everything below was measured on this machine rather than reasoned about. Where a claim is about
behaviour, there is a script under `examples/python/` that shows it.

## The four things a caller has to get right

**A call blocks, and it finishes.** `ask()` runs `flint -p ... --json` to completion and returns when
the process exits, so the answer is complete when you have it. There is no callback to register and
nothing to poll. A long turn is a long function call: `timeout=` is the only bound, and what it does
when it runs out is the next section.

**A failure does not raise.** This is the trap, and it is the reason `ask_json` exists. A run whose
provider has no key, or whose endpoint is unreachable, still exits with a `Turn` in hand: `turn.ok`
is `False`, `turn.error` says why, and `turn.answer` is `''`. A caller that does not check carries on
with an empty string — measured in `timing_demo.py`, which prints the line after the failure to prove
the script kept going. `ask_json` raises `SchemaError` instead, because a caller that asked for a
shape wants data or an exception, never a dict-shaped hole.

**The working directory is required, and it is not the process's directory.** `ask(prompt, cwd=...)`
passes `--cwd`, which is what flint records in the session and what `--continue` matches on. Two
projects under one `FLINT_HOME` therefore cannot be handed each other's history, and a continuation
finds its own conversation from a process started anywhere. Inheriting the interpreter's own
directory would make the answer depend on where the caller happened to be running.

**Two calls must not share one conversation.** Concurrent calls against the same session interleave
their appends; there is one writer per file by design. Parallel work belongs in threads with
`cwd=`-separated sessions, separate `home=` values, or a fork (`extra=["--fork"]`). `Chat` below is
how a worker names its own conversation instead of racing for the newest one.

## One conversation, many calls

`ask()` is one question, and `continue_last=True` is how a second question follows it. That works
until two callers work in one directory, which is exactly when "the newest conversation here" stops
naming the one you mean. `Chat` removes that from the caller's life:

```python
from flint_call import Chat

chat = Chat(cwd="/path/to/project")
chat.ask("read @rules.csv and tell me the columns", on_delta=lambda text: print(text, end=""))
chat.ask("and the third row?")          # the same conversation
print(chat.history()[-1])               # the record, as the file has it
```

**The session is decided once.** The first call creates it and flint reports the path on
`session.started`; every later call passes that path to `--resume`, so nothing re-derives the target
and two callers in one directory cannot land in each other's history. Nothing is cached either:
`history()` reads the file, `messages()` is the conversation as the model saw it, and a `Chat` built
with `session=<path>` continues a conversation another process was holding. If flint opens a file
other than the one that was asked for, `Chat` raises `SessionMoved` rather than handing back an answer
from a different conversation.

**Streaming is the same frames, delivered as they arrive.** `on_event` gets every frame, `on_delta`
gets each fragment's text, and both are called from the thread reading the stream while the run is
going. That is measured rather than asserted: `test_call.py` checks that the first fragment of a
stalled run reaches the callback about a second before the run is stopped, which a reader that
collected every line and parsed it at the end could not do. The `Turn` that comes back holds the same
frames, so a callback is a view and not a second channel. A callback that raises is the caller's bug,
and it still must not leave a flint running: the run is asked to stop like any other, and the
exception arrives once the process has ended.

## Asking a run to stop

`timeout=` is not "how long before this is killed". When it runs out, `ask()` writes `/stop` to
flint's stdin — the same word the interactive session takes, for the same reason: it is the interrupt
that works when there is no key to press, which is precisely the situation a program is in. flint then
drops the turn, **commits the answer it had already drawn to the session file**, says so on the stream
and exits **130**. You get a `Turn` with `stopped=True`, `outcome == "stopped"`, the partial `answer`,
and a `warning` explaining what happened. Only if the process ignores its own interrupt for another ten
seconds is it killed.

It exits 130 rather than 0 on purpose, and `ok` is therefore `False` for a stopped run: a caller that
branched on `ok` — which is what a caller does — was treating half an answer as a finished one. A
stopped run is still *not an error*: `error` stays `None` and the answer is real. Ask `stopped` (or
`complete`) about whether the answer is whole, and `error` about whether anything went wrong.

The difference is not cosmetic. `subprocess.run(timeout=...)` kills, and a killed flint loses whatever
it had drawn: the words you just read exist on your screen and nowhere else, so the next call about
them is answered as if they had never been written. That is the fault this avoids, and
`test_call.py`'s last section pins it — a stub writes one fragment and then stalls, and the check
asserts the fragment is in the session file afterwards.

```python
turn = ask("仔细研究一下这个仓库", cwd="/path/to/project", timeout=120)
if turn.stopped:
    print("ran out of time; it had got this far:", turn.answer)
```

While a run is going, flint also says so every five seconds — `{"elapsed_secs":42,"restarted":
false,"text":"running bash","type":"status"}` — which is what makes a long run distinguishable from
a hung one if you are reading the stream yourself rather than waiting for `ask()`.

## Many calls at once

`map_calls` is the batch, and it exists because the naive version is wrong in three ways that are
easy to miss. It takes a list of prompts (or of job dicts) and returns one `Turn` per item, in the
order asked for:

```python
from flint_call import map_calls, OutOfBalance

turns = map_calls(
    ["summarise @a.md", "summarise @b.md", "summarise @c.md"],
    cwd="/path/to/project", workers=3, attach=["a.md", "b.md", "c.md"],
)
for turn in turns:
    print(turn.ok, turn.answer)
```

**Every call is its own conversation.** Not one per worker: with `workers=3` and six jobs the
threads take jobs as they finish, so which calls shared a worker would depend on the schedule, and
two conversations in one file is the thing `docs/session-format.md` says must not happen. Each job
gets its own run, its own session and its own bill, and `turn.session` names it — so a result can be
traced back to the conversation that produced it. (This is the check that found the real fault
underneath: six concurrent runs proposed the same session id, because they started in the same
millisecond, and five conversations went into one file. `new_id` now ends in the process id and
taking the name is a claim; see `HANDOFF.md`.)

**A promise made to one job is checked for that job.** `attach=`, `paths=`, `inline=` and
`require_read=` are arguments of `map_calls` and are applied to every job that does not override
them with its own dict:

```python
turns = map_calls(
    [{"prompt": "read @rules.csv and count the rows", "require_read": ["rules.csv"]},
     {"prompt": "just say hello"}],
    cwd="/path/to/project", attach=["rules.csv"],
)
```

A job whose promise was not kept on the stream does **not** raise: it comes back as a `Turn` whose
`refused` holds the reason, because one refused job out of twenty should not throw away the other
nineteen answers. `turn.refused` being set means the call ran but the file was not attached, or was
not read — the same two refusals `ask()` raises, with `.missing` and `.turn` still there.

**The first `insufficient_balance` stops the batch.** Twenty calls that each discover the account is
empty are twenty calls paid for; the breaker watches for `stop_on=("insufficient_balance",)` on the
turns that have already come back, cancels the jobs that have not started, waits for the ones in
flight (it never kills a run mid-answer) and then raises:

```python
try:
    turns = map_calls(questions, cwd="/path/to/project", workers=4)
except OutOfBalance as e:
    print(e)                     # the sentence flint gave, and how many were cancelled
    print(e.asked, e.turns)      # how many were asked, and the ones that did finish
```

`OutOfBalance.cancelled` is what was never spent, `OutOfBalance.turns` is what was, and
`OutOfBalance.turn` is the call that found the empty account. Pass `stop_on=()` to run the batch
anyway — for a provider that uses `402` for something other than money. A failed run is not an
error: a job whose flint exited non-zero, or whose promise was refused, is a `Turn` like any other,
and only the breaker and a bug in the caller raise. `on_turn=` gets `(index, turn)` from the worker
thread as each one settles, which is how a caller draws progress without waiting for the batch.

## What a `Turn` holds

| Field | What it is |
|---|---|
| `answer` | the turn's text: the deltas, or `message.completed`, whichever exists |
| `messages` | one entry per completed message — a turn that used tools has more than one |
| `tools` | `tool.started` / `tool.args` / `tool.completed`, in order, with output |
| `error`, `warnings` | the `error` line and any `warning` lines |
| `session`, `cwd`, `model` | from `session.started`, the first line of every run |
| `usage` | token counts from the provider |
| `result`, `attempts` | a schema run's checked object, and how many answers it took |
| `stopped` | true when `timeout` ran out and the run was asked to stop rather than killed |
| `outcome` | how the turn ended: `complete`, `incomplete` (the step limit), `stopped` |
| `duration_ms` | how long the turn took, as flint measured it (from `turn.started`), not including flint's start-up or your own reading of the stream |
| `turns` | how many turns the run asked for (one, unless a schema needed repairs) |
| `ok` | `returncode == 0 and error is None` — false for a stopped or unfinished run |
| `complete` | `ok` and the outcome is not `incomplete`/`stopped`: the answer is whole |
| `error_code`, `error_retryable` | the cause when flint knows one, and whether waiting could help |
| `returncode` | flint's exit code: 2 usage, 65 unusable answer, 69 a person must act, 75 retry later, 130 stopped, 1 unclassified |
| `events` | every line, untouched, for anything this dataclass does not name |

`of_type(*types)` picks events out by type. The vocabulary is flint's, and it is documented in the
[README](../README.md#machine-interface): dispatch on `type` and ignore what you do not know, so a
newer flint does not break a caller.

## Putting a file in the prompt, and knowing whether it got there

`@path` in a prompt is how flint inlines a file: the text goes into the prompt in a `<file>` block
before the model is called, so nothing the model does can leave it out. Typing the name yourself works,
and it has one failure mode a program cannot live with — a name that matched nothing stays in the prompt
as *prose*, and the model answers about a file it never saw in the same confident voice it uses when it
read one. So there are three arguments, and the difference between them is what they promise:

```python
turn = ask("what changed in @src/provider.rs?", cwd="/path/to/project")     # your own prompt
turn = ask("summarise this", cwd="/path/to/project", attach=["rules.csv"]) # a promise
turn = ask("check the numbers", cwd="/path/to/project", paths=["a.csv", "b.csv"])  # a hope
turn = ask("answer from this", cwd="/path/to/project", inline=["text in memory"])  # the prompt
```

| Argument | What it puts in the prompt | What it promises |
|---|---|---|
| `attach=[path]` | the file's text, as a real `@` name | **the text is there**, and `ask` refuses if it is not |
| `paths=[path]` | the names, as a sentence | nothing: the model may read them, or may not |
| `inline=[text]` | the text | **the text is there** — because it *is* the prompt |

`attach=` is the one to reach for, and it is checked in both directions. A path that is not a file
raises `FileNotFoundError` before flint is started, so nothing is spent on a typo; a file that flint
refuses to inline (past its 256 KB cap, or not valid UTF-8) fails the run with the reason on the
stream; and a prompt flint somehow did not inline raises `NotAttached`, checked against
`turn.started`'s `attachments` rather than against what was asked for. `attached(turn)` and
`verify_attached(turn, paths, cwd=...)` are that check, exposed, for a caller that assembled its own
`@` names.

`require_read=` is the other direction, for the case where hoping is not enough:

```python
turn = ask("…", cwd="/path/to/project", require_read=["src/provider.rs"])
```

It is checked against what the run *did* — the `read` tool's `tool.args` frames, resolved against the
working directory — and raises `NotRead` if the file was never read. Reading it through `bash`
(`cat`, a script) does **not** count: the check is evidence of a read, and a command line containing a
path is not evidence of anything.

Both refusals happen *after* the run, and both carry the `Turn` in `.turn`, because the run did happen:
the answer is on disk and already paid for. `Chat` records that turn and pins the session before the
exception goes on, so the next call continues the conversation rather than starting a second one beside
it.

Two limits are worth knowing before you hit them. flint inlines at most 256 KB per prompt, reported by
the run rather than silently dropped. And the prompt travels as a *command-line argument*, which Windows
cuts at 32767 characters: `flint_call` refuses a call over 30000 characters itself, and the message
says to use `attach=`, which is the way through — flint reads the file, so the argument stays four
characters no matter how big the document is. Without that check the caller gets
`FileNotFoundError [WinError 206]` from `subprocess`, naming no argument at all.

## Structured output

`ask_json(prompt, cwd=..., schema={...})` returns the object from flint's `result` line. flint
writes the schema into the system prompt, asks the provider for `response_format:
{"type":"json_object"}` — the only JSON mode DeepSeek accepts, which promises the reply parses and
nothing about its shape — and then validates the answer itself against a subset of JSON Schema. A
mismatch is quoted back to the model and asked again, up to three answers.

The schema is recorded in the session file, so a `continue_last=True` call is held to the same shape
without passing it again. `ask(..., no_schema=True)` records that the shape was dropped. Live proof
of all of it, including an unsatisfiable schema and the refusal of `--schema` with `--no-schema`:
`ask_schema.py` (needs a real key; it is not part of any test run).

Python dicts are written to a temporary file rather than passed inline. Inline works from Python —
there is no shell in the way — but it breaks the moment the same call is run through one, and a path
cannot be mangled by anything.

## When the money runs out

An exhausted balance is the failure worth writing code for, because it is the one that keeps looking
like something else. Three providers report it three ways — DeepSeek as `402`, OpenAI-shaped endpoints
as a `429` `insufficient_quota` (the same status as a rate limit), Anthropic as a `400` with a
sentence — and flint now reads the body and gives all three one name:

```python
turn = ask("…", cwd="/path/to/project")
if turn.error_code == "insufficient_balance":
    raise SystemExit("out of credit: top up before the next batch")
if turn.error_retryable:
    time.sleep(30)          # 75: the retries ran out, and asking again later is right
```

`error_retryable` is the field to loop on, and the exit code says the same thing to a shell (`69`
a person must act, `75` try later, `1` unclassified). In a batch, a balance failure stops the whole
batch rather than the one call, which is what `map_calls`'s `stop_on=` does — see
[Many calls at once](#many-calls-at-once). The decision belongs to the caller because only the caller
knows that nineteen other calls are queued behind this one.

**Better than discovering it on the first call is not discovering it at all.** `balance()` is the
preflight — `flint balance --json`, one request, no completion, no tokens:

```python
from flint_call import balance

ready = balance()
if ready.broke:                       # 69: no key, rejected credentials, an empty account
    raise SystemExit(f"not starting: {ready.error}")
if not ready.ok:                      # usable is None -- the endpoint answered neither question
    print(f"proceeding without knowing ({ready.checked}): {ready.error}")
print(f"{ready.total} {ready.currency} left")   # absent unless the provider publishes it
```

`ready.usable` is `True`, `False` or `None`, and the third is not a yes: `None` is what a local engine
that serves only `/chat/completions` produces, and it means nothing was checked — not the key, not the
account. `ready.checked` says what was actually asked (`balance`, `models`, or `none`), so "usable,
110.00 CNY left" is never confused with "usable, `/models` answered". `ready.ok` is true only for an
explicit yes.

## Running the checks here

```console
$ cargo build
$ python examples/python/test_call.py       # 117 checks against a local stub, no key, no cost
$ python examples/python/timing_demo.py     # what blocking and failure actually look like
$ python examples/python/ask_schema.py      # needs DEEPSEEK_API_KEY; spends real tokens
```

`test_call.py` starts `stub_provider.py` (a minimal OpenAI-compatible SSE server) and points a
scratch `FLINT_HOME` at it, which is the same trick `tests/agent_loop.rs` uses with `wiremock`. It
runs the binary **built from this checkout** when there is one — a check that runs yesterday's
installed flint passes for the wrong reason, which is not hypothetical: the session-layout
expectations here were first written while `PATH` still held a build from before the layout changed.

The stub is told to log every request body it is sent (`FLINT_STUB_LOG`), because the stream says what
flint did and the request says what the **model** was given — the only place `attach=`, `paths=` and
`inline=` are distinguishable from each other. A prompt containing `[[read: <path>]]` is answered with
a real `read` tool call and then answered for real, so `require_read=`'s positive case is a run that
read something rather than a hand-built event. `[[balance]]` makes the stub answer `402
insufficient_balance`, which is how the breaker is tested without an empty account; `delay=<secs>` makes
each request take that long, and `/stats` reports how many requests it has seen and how many were in
flight at once. The last two are what turn "it runs several at once" into a measurement rather than a
hope: six jobs with a one-second stub delay on six workers finish in about one call's time, and the
stub's own count says at least three of them overlapped.

On Windows, set `PYTHONIOENCODING=utf-8`: this machine's ANSI code page is CP936, and the default
would mangle the answer.
