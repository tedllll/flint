"""Calling flint from Python as if it were a function.

The machine interface is one JSON object per line on stdout: `flint -p "<prompt>" --json`.
Nothing here parses the terminal transcript -- that rendering is for people, and `--json` is the
rendering for a program. This module is the whole of it: spawn the process, read the lines, hand
back the events.

    from flint_call import ask, ask_json
    turn = ask("1+1 等于几？", cwd="/path/to/your/project")   # cwd is required: a call has a context
    print(turn.answer)
    turn2 = ask("那刚才我问了什么？", cwd="/path/to/your/project",
                continue_last=True)                          # the same conversation
    print(turn2.answer, turn2.session)

    # Or ask for a shape and get the object, checked by flint before you see it:
    day = ask_json("MA2610 的最后交易日是哪天？", cwd="/path/to/project", schema={
        "type": "object",
        "properties": {"trading_day": {"type": "string"}},
        "required": ["trading_day"],
    })
    print(day["trading_day"])

`--cwd` is what names that context, and it is recorded in the session file: conversations live
in `<FLINT_HOME>/sessions/<dir>/`, one directory per working directory, so two projects in one
home cannot be handed each other's history. `continue_last=True` continues the most recent
conversation *held in the directory you pass* -- from a process started anywhere, including one
whose own working directory is somewhere else entirely.

Vocabulary (as of this flint; dispatch on `type` and ignore what you do not know, so a newer
flint adding a type does not break you):

    session.started  session, cwd, model      the first line: which file, where, which model
result           json, attempts           a schema run's checked answer, once it matches (there is
                                          no `result` line at all when none of the answers do)
    turn.started     prompt
    message.delta    text                     one streamed fragment -- not one per answer
    reasoning.delta  text                     the model thinking out loud
    message.completed text                    the turn's whole answer, emitted once at the end
    tool.started     id, name
    tool.args        id, name, arguments
    tool.completed   id, name, ok, output
    usage            prompt_tokens, completion_tokens, total_tokens
    status           text, restarted
    warning          message
    turn.completed   prompt_tokens, completion_tokens, outcome
                                              how the turn ended: complete | incomplete | stopped
    error            message                  the run failed; the exit code says which kind
    command          input, text, panel?      a slash command's answer, not part of a turn

Notes that matter in practice:

* `-p` is required for `--json`: without it flint is interactive and writes no stream.
* `turn.error` is set and `returncode` is not 0 when the run failed. The code classifies it:
  2 is the command line, 65 an answer that is not usable, 69 a provider that cannot be used,
  130 a run this caller stopped, 1 a failure flint has not classified. Check the code as well as
  `error`: a program branches on the code, and the message is for a person.
* `turn.outcome` is how the turn ended, and it is the only place that says whether the answer is
  whole. `incomplete` means flint stopped asking at the `max_steps` limit; `stopped` means this
  caller's timeout arrived first. `turn.complete` folds that together with `ok`.
* There is no approval hook. A tool runs when the model asks for it, so `tool.started` is a
  *record*, not a chance to object. `readonly` is the only switch, and it is all-or-nothing.
* Tool output longer than `max_tool_output` spills to `<FLINT_HOME>/spill/...` and the event
  carries the truncated text plus a note saying where the rest is.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

def _binary() -> str:
    """Which flint to run.

    `FLINT_BIN` if it is set, else the build in this checkout when this file is still inside it
    (`examples/python/` two levels down from the root), else `flint` on `PATH`.

    The middle case is the one worth explaining: the scripts here are *checks*, and a check that
    runs yesterday's installed flint passes for the wrong reason. That is not hypothetical -- this
    file's session-layout expectations were first written while `PATH` still held a build from
    before the layout changed, and they failed for a reason that had nothing to do with the code
    under test. Copy this file into your own project and it uses your `PATH`, which is what you want
    there.
    """
    if os.environ.get("FLINT_BIN"):
        return os.environ["FLINT_BIN"]
    built = Path(__file__).resolve().parents[2] / "target" / "debug" / (
        "flint.exe" if os.name == "nt" else "flint"
    )
    return str(built) if built.exists() else "flint"


FLINT = _binary()

# How long a run is given to finish after it has been asked to stop. Not a setting: a process that
# cannot end within a few seconds of its own interrupt is broken, and waiting for ever is not a
# kindness to the caller.
STOP_GRACE = 10.0


@dataclass
class Turn:
    """One `flint -p ... --json` run, and everything it said."""

    returncode: int = 0
    events: list[dict] = field(default_factory=list)
    text: str = ""              # every `message.delta` fragment, concatenated
    messages: list[str] = field(default_factory=list)  # `message.completed`, once per turn
    error: str | None = None
    warnings: list[str] = field(default_factory=list)
    session: str | None = None
    cwd: str | None = None
    model: str | None = None
    usage: dict | None = None
    tools: list[dict] = field(default_factory=list)    # started/args/completed, in order
    stderr: str = ""
    # True when the run outlived `timeout` and was asked to stop rather than killed. The answer is
    # then what had been written when the stop arrived -- partial, and still worth keeping: flint
    # commits it to the session, so the next call about it is answered with it in view.
    #
    # `ok` is False for a stopped run, deliberately: flint exits 130, not 0, because a caller that
    # branched on `ok` was treating half an answer as a finished one. A stopped run is not an error
    # either -- `error` stays None and the answer is real -- so a caller should ask `stopped` when it
    # cares whether the answer is whole and `error` when it cares whether anything went wrong.
    stopped: bool = False
    # How the turn ended, as flint's `turn.completed` line says it: "complete", "incomplete" (flint
    # stopped asking at the step limit, so the answer is half of one) or "stopped" (this caller cut it
    # short). `None` for a run that never reached the end of a turn -- the provider was unusable, or
    # the answer never matched its schema. An older flint has no such field, and this is then None for
    # every run: `ok` and `stopped` are the older signals and still work.
    outcome: str | None = None
    # The checked answer, when the run was given a schema: the `result` line's object. `None` for a
    # run with no schema, and `None` for a schema run whose answer never matched -- flint emits no
    # `result` line at all in that case, which is the point: see `ask_json`, which raises instead.
    result: dict | None = None
    # How many answers the model gave before one matched the schema (1 = first try). From flint's
    # `result` line when there is one, and `None` otherwise -- a run with no schema has no
    # "attempts", and a run whose answers never matched has no `result` to carry the count.
    attempts: int | None = None
    # How many turns this run asked for. One, unless a schema was in force and the answers had to be
    # asked for again: this is where the count comes from when there is no `result` line to read it
    # from, which is exactly the case a caller most wants to know about.
    turns: int = 0

    @property
    def answer(self) -> str:
        """What the turn said, in order.

        The deltas and `message.completed` hold the same text for a turn that finished -- including
        prose the model emitted *before* it decided to call a tool, which is part of what it said.
        Which to use is a matter of taste: deltas if you render as it arrives, `message.completed`
        if you waited for the end anyway. The fallback here covers a provider that streams no
        deltas at all and an interrupted turn, where only one of the two exists."""
        return self.text or "\n".join(self.messages)

    @property
    def ok(self) -> bool:
        return self.returncode == 0 and self.error is None

    @property
    def complete(self) -> bool:
        """Whether the answer is whole: the turn ended and flint did not stop asking.

        The same question `ok` answers for a run that reached its end, asked in a way that also
        covers a caller that cut the run short -- `ok` is False for a stopped run and for an
        unfinished one, and neither is an error. `None` (flint too old to say) counts as complete,
        because a turn that ended before the field existed is the only thing it could have been."""
        return self.ok and self.outcome in (None, "complete")

    def of_type(self, *types: str) -> list[dict]:
        return [e for e in self.events if e.get("type") in types]


def ask(
    prompt: str,
    *,
    cwd: str | os.PathLike,
    continue_last: bool = False,
    resume: str | None = None,
    provider: str | None = None,
    model: str | None = None,
    home: str | None = None,
    timeout: float = 600.0,
    extra: list[str] | None = None,
    schema: dict | str | None = None,
    no_schema: bool = False,
) -> Turn:
    """Run one turn in `cwd` and return it.

    `cwd` is required and is passed as `--cwd`, not as the subprocess's own directory. A Python
    call always has a context -- a project, a checkout, a directory of data -- and flint records
    that directory in the session and separates conversations by it, so the caller has to say
    which one it means rather than inheriting whatever directory this interpreter happens to run
    in. It is also why the answer can be continued later (`continue_last=True`) from anywhere.

    `continue_last` is flint's `--continue` (the most recent conversation for this directory);
    `resume` names one -- a session id or a path. `home` points flint at another `FLINT_HOME`,
    which is how a test keeps its sessions and config to itself; left out, flint uses its own.

    `schema` asks for a checked answer: a dict (or a JSON string) is written to a temporary file
    and passed as `--schema`, because a schema handed over inline has to survive the shell on some
    machines and a temp file always does. `no_schema` is `--no-schema`, how a resumed conversation
    is asked for prose again. `ask_json` is the shorthand for the case where you want the object.
    """
    project = os.path.abspath(os.fspath(cwd))
    if not os.path.isdir(project):
        raise NotADirectoryError(f"cwd is not a directory: {project}")

    argv = [FLINT, "-p", prompt, "--json", "--cwd", project]
    if continue_last:
        argv.append("--continue")
    if resume:
        argv += ["--resume", resume]
    if provider:
        argv += ["--provider", provider]
    if model:
        argv += ["--model", model]
    if no_schema:
        argv.append("--no-schema")
    argv += extra or []

    env = dict(os.environ)
    if home:
        env["FLINT_HOME"] = home

    # A schema goes through a file rather than inline. Inline looks simpler and works from Python
    # (there is no shell in the way here, unlike a hand-typed command) but it breaks the moment
    # somebody runs the same call through a shell, and a file has no such edge: what is passed is a
    # path, which cannot be mangled by anything.
    schema_file = None
    if schema is not None:
        text = schema if isinstance(schema, str) else json.dumps(schema, ensure_ascii=False)
        handle, schema_file = tempfile.mkstemp(prefix="flint-schema-", suffix=".json")
        with os.fdopen(handle, "w", encoding="utf-8") as out:
            out.write(text)
        argv += ["--schema", schema_file]

    try:
        proc = subprocess.Popen(
            argv,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            # This machine's ANSI code page is CP936: the default would mangle the answer.
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )
        out: "queue.Queue[str | None]" = queue.Queue()

        def pump(stream, sink):
            # Both pipes have to be drained while the run is going. A child whose stderr has filled
            # blocks on the write, and the run then looks hung for a reason that has nothing to do
            # with the model -- the classic way a wrapper invents a timeout.
            for line in stream:
                sink(line)
            sink(None)

        lines: list[str] = []
        errors: list[str] = []
        threading.Thread(target=pump, args=(proc.stdout, out.put), daemon=True).start()
        threading.Thread(target=pump, args=(proc.stderr, errors.append), daemon=True).start()

        # `timeout` is when the run is *asked to stop*, not when it is killed. flint takes `/stop` on
        # stdin -- the interrupt that works when there is no key to press -- and keeps what it had
        # already drawn: the words the caller has read are committed to the session and the process
        # exits cleanly. Killing it instead would lose exactly that, which is why `subprocess.run` was
        # the wrong tool here even though it is the shorter one.
        deadline = time.monotonic() + timeout
        stopped = False
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                if stopped:
                    # It was asked and did not go: a process that ignores its own interrupt is broken,
                    # and waiting for ever is not a kindness to the caller.
                    proc.kill()
                    break
                try:
                    proc.stdin.write("/stop\n")
                    proc.stdin.flush()
                except (BrokenPipeError, ValueError):
                    # Already gone; whatever it said is on its way through the queue.
                    pass
                stopped = True
                deadline = time.monotonic() + STOP_GRACE
                continue
            try:
                line = out.get(timeout=min(left, 0.5))
            except queue.Empty:
                continue
            if line is None:
                break
            lines.append(line)
        proc.wait()
        returncode = proc.returncode
        # The end marker the pump sends is not stderr: the same pump feeds both pipes.
        stderr = "".join(e for e in errors if e)
    finally:
        if schema_file:
            os.unlink(schema_file)

    turn = Turn(returncode=returncode, stderr=stderr, stopped=stopped)
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            # One object per line is the contract; anything else is a bug worth seeing.
            turn.warnings.append(f"unparsable line: {line[:200]}")
            continue
        turn.events.append(event)
        kind = event.get("type")
        if kind == "message.delta":
            turn.text += event.get("text", "")
        elif kind == "message.completed":
            turn.messages.append(event.get("text", ""))
        elif kind == "error":
            turn.error = event.get("message", "")
        elif kind == "warning":
            turn.warnings.append(event.get("message", ""))
        elif kind == "turn.completed":
            # The end of a turn is the only place that says how it ended and whether the answer is
            # whole, which is not something a caller can work out from the text it received. The
            # token counts live on the same line, so this branch is also where the usage goes.
            turn.outcome = event.get("outcome")
            turn.usage = {
                "prompt_tokens": event.get("prompt_tokens", 0),
                "completion_tokens": event.get("completion_tokens", 0),
            }
        elif kind == "session.started":
            turn.session = event.get("session")
            turn.cwd = event.get("cwd")
            turn.model = event.get("model")
        elif kind == "usage":
            turn.usage = event
        elif kind == "turn.started":
            turn.turns += 1
        elif kind == "result":
            turn.result = event.get("json")
            turn.attempts = event.get("attempts")
        elif kind in ("tool.started", "tool.args", "tool.completed"):
            turn.tools.append(event)
    return turn


class SchemaError(RuntimeError):
    """A run that was given a schema came back without a checked answer.

    Raised by `ask_json`, and it is the whole point of that function existing: `ask` never raises on
    a failed run -- the failure is in `turn.error` and a caller that does not look carries on with an
    empty answer, which is the trap measured in `timing_demo.py`. A caller that asked for a shape
    wants data or an exception, not a dict-shaped hole.
    """

    def __init__(self, message: str, turn: "Turn"):
        super().__init__(message)
        self.turn = turn
        # How many answers the model gave before flint gave up. Kept because "it failed" and "it
        # failed after three tries" lead to different decisions about the prompt and the model.
        # Counted from the turns that ran, since a failed run has no `result` line to read it from.
        self.attempts = turn.attempts if turn.attempts is not None else turn.turns


def ask_json(
    prompt: str,
    *,
    cwd: str | os.PathLike,
    schema: dict | str,
    **options,
) -> dict:
    """Ask for a checked answer and return the object, or raise.

    The schema is a Python dict or a JSON string. The answer is flint's `result` line, already
    validated against that schema by flint -- locally, because the only JSON mode the provider
    surface agrees on promises the reply parses and nothing about its shape.

    Everything else (`home`, `continue_last`, `resume`, `timeout`, `extra`) is `ask`'s.
    """
    turn = ask(prompt, cwd=cwd, schema=schema, **options)
    if turn.result is None:
        raise SchemaError(
            turn.error or f"the run produced no checked answer (exit {turn.returncode})", turn
        )
    return turn.result
