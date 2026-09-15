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
nothing to poll. A long turn is a long function call: `timeout=` is the only bound, and it is on the
subprocess.

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
| `turns` | how many turns the run asked for (one, unless a schema needed repairs) |
| `ok` | `returncode == 0 and error is None` |
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

## Running the checks here

```console
$ cargo build
$ python examples/python/test_call.py       # 21 checks against a local stub, no key, no cost
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
