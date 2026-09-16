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
`cwd=`-separated sessions, separate `home=` values, or a fork (`extra=["--fork"]`).

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
| `turns` | how many turns the run asked for (one, unless a schema needed repairs) |
| `ok` | `returncode == 0 and error is None` — false for a stopped or unfinished run |
| `complete` | `ok` and the outcome is not `incomplete`/`stopped`: the answer is whole |
| `error_code`, `error_retryable` | the cause when flint knows one, and whether waiting could help |
| `returncode` | flint's exit code: 2 usage, 65 unusable answer, 69 a person must act, 75 retry later, 130 stopped, 1 unclassified |
| `events` | every line, untouched, for anything this dataclass does not name |

`of_type(*types)` picks events out by type. The vocabulary is flint's, and it is documented in the
[README](../README.md#machine-interface): dispatch on `type` and ignore what you do not know, so a
newer flint does not break a caller.

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
a person must act, `75` try later, `1` unclassified). In a batch, a balance failure should stop the
whole batch rather than the one call: `map_calls` cannot know that twenty other calls are queued
behind this one, and flint cannot know either — so the caller is where that decision belongs.

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
$ python examples/python/test_call.py       # 49 checks against a local stub, no key, no cost
$ python examples/python/timing_demo.py     # what blocking and failure actually look like
$ python examples/python/ask_schema.py      # needs DEEPSEEK_API_KEY; spends real tokens
```

`test_call.py` starts `stub_provider.py` (a minimal OpenAI-compatible SSE server) and points a
scratch `FLINT_HOME` at it, which is the same trick `tests/agent_loop.rs` uses with `wiremock`. It
runs the binary **built from this checkout** when there is one — a check that runs yesterday's
installed flint passes for the wrong reason, which is not hypothetical: the session-layout
expectations here were first written while `PATH` still held a build from before the layout changed.

On Windows, set `PYTHONIOENCODING=utf-8`: this machine's ANSI code page is CP936, and the default
would mangle the answer.
