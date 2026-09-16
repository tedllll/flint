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

    # Or hold one conversation over many calls, streamed as it is written:
    from flint_call import Chat
    chat = Chat(cwd="/path/to/your/project")
    chat.ask("read @rules.csv", on_delta=lambda text: print(text, end="", flush=True))
    chat.ask("and the third row?")                           # the same file, by path
    print(chat.history()[-1])

    # Or ask for a shape and get the object, checked by flint before you see it:
    day = ask_json("MA2610 的最后交易日是哪天？", cwd="/path/to/project", schema={
        "type": "object",
        "properties": {"trading_day": {"type": "string"}},
        "required": ["trading_day"],
    })
    print(day["trading_day"])

    # Or ask many questions at once, four flint processes at a time, stopping the batch when the
    # account is empty rather than paying for the rest of the refusals:
    from flint_call import map_calls, OutOfBalance
    try:
        turns = map_calls([f"summarise row {n}" for n in range(1, 101)], cwd="/path/to/project")
    except OutOfBalance as broke:
        print(f"stopped after {len(broke.turns)}; {broke.cancelled} never asked")

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
    error            message, code?, retryable?
                                              the run failed; `code` names the cause when known
                                              (`insufficient_balance`, `rate_limit`, `auth`, `no_key`,
                                              `server`, `bad_request`, `network`), `retryable` says
                                              whether waiting could help
    command          input, text, panel?      a slash command's answer, not part of a turn

Notes that matter in practice:

* `-p` is required for `--json`: without it flint is interactive and writes no stream.
* `balance()` asks before a batch whether the provider can be used at all: one request, no
  completion, and `usable is None` when the endpoint answered neither its balance nor `/models` --
  which is not a yes.
* `turn.error` is set and `returncode` is not 0 when the run failed. The code classifies it: 2 is
  the command line, 65 an answer that is not usable, 69 something a person must fix (no key, an
  empty account), 75 worth trying again later, 130 a run this caller stopped, 1 unclassified.
  `turn.error_code` and `turn.error_retryable` carry the same two facts from the `error` line, and
  are `None` when flint has no cause to name. Ask `error_retryable` before looping: retrying an
  `insufficient_balance` costs money and returns the same nothing.
* `turn.outcome` is how the turn ended, and it is the only place that says whether the answer is
  whole. `incomplete` means flint stopped asking at the `max_steps` limit; `stopped` means this
  caller's timeout arrived first. `turn.complete` folds that together with `ok`.
* `Chat` is the same calls for a conversation: it pins the session path on the first call and passes
  it to `--resume` on every later one, so two callers in one directory cannot land in each other's
  history; `chat.history()` reads the record back from that file.
* `on_event` and `on_delta` are called as the frames arrive, so an answer can be rendered while it is
  written. Both are views on the same frames the returned `Turn` holds.
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
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

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

# The most command line this module will build. The prompt travels as an *argument*, so the prompt, the
# `@` names and the paths are one string to the operating system, and Windows cuts that string at 32767
# characters (`CreateProcess`). Measured rather than imagined: a 33k prompt fails before flint exists,
# with `FileNotFoundError [WinError 206]` and nothing about which argument caused it -- which is exactly
# the kind of failure a caller cannot act on. The bound is 30000 everywhere rather than the Windows
# number, because a bound a caller can rely on is worth more than a fact about one machine; Unix would
# carry far more, and `attach=` is the better door there too: flint reads the file itself and the model
# is given the same text, in the same `<file>` block a hand-typed `@` produces.
COMMAND_LINE_LIMIT = 30_000


@dataclass
class Turn:
    """One `flint -p ... --json` run, and everything it said."""

    returncode: int = 0
    events: list[dict] = field(default_factory=list)
    text: str = ""              # every `message.delta` fragment, concatenated
    messages: list[str] = field(default_factory=list)  # `message.completed`, once per turn
    error: str | None = None
    # The cause, when flint knows one: `insufficient_balance`, `rate_limit`, `auth`, `no_key`,
    # `server`, `bad_request`, `network`. `None` from an older flint, and `None` for a failure nothing
    # established a cause for -- which is not the same as a cause named wrongly, and is why the field
    # is absent rather than "unknown".
    error_code: str | None = None
    # Whether another attempt could plausibly help. The question a looping caller actually has: an
    # exhausted balance is `False`, and retrying it costs money to be told the same nothing.
    error_retryable: bool | None = None
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
    # The exception `map_calls` caught for this turn, or `None`. A batch records a refused promise
    # here instead of raising it, because one job's refusal must not discard ninety-nine answers --
    # and it is a field rather than a warning on purpose: `warnings` is what the *stream* said, and a
    # caller deciding whether to trust this answer should not have to read the two together.
    refused: BaseException | None = None

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


@dataclass
class Balance:
    """What `flint balance` answered: whether the provider can be used, and what is left.

    `usable` is `True`, `False` or **`None`** -- and the third is the one that has to be handled, not
    defaulted away. `None` means the endpoint answered neither its balance nor `/models`, which a
    local engine that serves only `/chat/completions` does all the time; treating that as "fine, go
    ahead" is exactly the wrong-value mistake the command exists to avoid. `checked` says what was
    actually asked, because "usable, 110.00 CNY left" and "usable, /models answered" are different
    statements and only one of them is about money."""

    returncode: int = 0
    usable: bool | None = None
    checked: str | None = None
    provider: str | None = None
    currency: str | None = None
    total: str | None = None
    granted: str | None = None
    topped_up: str | None = None
    error: str | None = None
    error_code: str | None = None

    @property
    def ok(self) -> bool:
        """Whether the provider is ready to be used, with no doubt left.

        `None` is not `ok`: the whole point of asking before a batch is that a batch which starts is
        expensive, so "could not tell" must not read as yes. Ask `usable is None` to treat it
        differently -- some callers will reasonably run anyway, and that is their decision to make
        explicitly."""
        return self.returncode == 0 and self.usable is True

    @property
    def broke(self) -> bool:
        """Whether a person has to do something: no key, rejected credentials, an empty account."""
        return self.returncode == 69


def balance(
    *,
    provider: str | None = None,
    home: str | os.PathLike | None = None,
) -> Balance:
    """Ask whether the provider can be used, before spending anything on a batch.

    The cause is the same one a failed run reports -- `insufficient_balance`, `no_key`, `auth` -- and
    it costs one request that sends no completion. The intended shape of a batch:

        ready = balance()
        if ready.broke:
            raise SystemExit(f"not starting: {ready.error}")
        if not ready.ok:
            print(f"warning: proceeding without knowing ({ready.checked}): {ready.error}")

    The exit codes are flint's own: `0` usable, `69` a person must act, `75` the check could not get
    out, `1` reached but undecidable. See `ROADMAP.md` §10 B6 for why a missing answer is not a yes.
    `home` does what it does in `ask`: another `FLINT_HOME`, which is how a test keeps its config."""
    argv = [FLINT, "balance", "--json"]
    if provider:
        argv += ["--provider", provider]

    env = dict(os.environ)
    if home:
        env["FLINT_HOME"] = home

    proc = subprocess.run(
        argv, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env
    )
    result = Balance(returncode=proc.returncode)
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("type") == "balance":
            result.usable = event.get("usable")
            result.checked = event.get("checked")
            result.provider = event.get("provider")
            result.currency = event.get("currency")
            result.total = event.get("total_balance")
            result.granted = event.get("granted_balance")
            result.topped_up = event.get("topped_up_balance")
        elif event.get("type") == "error":
            result.error = event.get("message")
            result.error_code = event.get("code")
    if result.error is None and not proc.stdout.strip():
        # Nothing on the stream at all. A current flint puts every refusal there -- including one
        # resolved before the stream would have been opened (an unresolvable `--provider`, an
        # unreadable config), which is what `ROADMAP.md` §10 B7 fixed -- so this is the belt for an
        # older binary and for the case where nothing was written anywhere. An empty answer must never
        # read as ready, so the fallback names the exit code rather than staying silent.
        result.error = proc.stderr.strip() or f"flint balance exited {proc.returncode} with no answer"
    return result


def _absorb(turn: Turn, line: str) -> dict | None:
    """Fold one line of the stream into `turn`, and hand it back for a callback.

    Split out of `ask` because it has to happen as the line arrives rather than after the run: the
    callbacks and the collected turn are the same frames, and parsing them twice -- or parsing them
    late -- is how a caller ends up with a `Turn` that disagrees with what it was shown.

    `None` means there was nothing to fold in: a blank line, or a line that is not JSON, which is a
    bug worth seeing rather than a frame to interpret (one object per line is the contract).
    """
    line = line.strip()
    if not line:
        return None
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        turn.warnings.append(f"unparsable line: {line[:200]}")
        return None
    turn.events.append(event)
    kind = event.get("type")
    if kind == "message.delta":
        turn.text += event.get("text", "")
    elif kind == "message.completed":
        turn.messages.append(event.get("text", ""))
    elif kind == "error":
        turn.error = event.get("message", "")
        # Absent, not null, when flint has no cause to name: `.get` keeps that distinction,
        # because "no classification" and "classified as nothing" are different answers.
        turn.error_code = event.get("code")
        turn.error_retryable = event.get("retryable")
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
    return event


class NotAttached(RuntimeError):
    """A file the caller asked for with `attach=` is not in the prompt.

    Raised after the run, and it is a refusal on purpose. `attach=` is a *promise* -- the file's text is
    in the prompt because flint read it -- and the alternative failure is the one that cannot be lived
    with: a model answering about a file it was never given, in the same confident voice it uses when it
    has read one. `.missing` is what did not arrive and `.turn` is the run that happened anyway.
    """

    def __init__(self, message: str, missing: list[str], turn: "Turn"):
        super().__init__(message)
        self.missing = missing
        self.turn = turn


class NotRead(RuntimeError):
    """A file the caller named in `require_read=` was never read in this turn.

    `paths=` mentions a path and hopes; `require_read=` is the case where hoping is not enough and the
    caller wants to *know*. What is checked is the tool call: the stream's `tool.args` for the `read`
    tool, resolved against the working directory. A file read through `bash` -- `cat`, a script that
    opens it -- does not count, and that is the honest boundary rather than a limitation to paper over:
    the check is evidence of a read, and a command line containing a path is not evidence of anything.
    `.missing` is what was not read and `.turn` is the run, answer included.
    """

    def __init__(self, message: str, missing: list[str], turn: "Turn"):
        super().__init__(message)
        self.missing = missing
        self.turn = turn


def _norm(path: str) -> str:
    """One path in the form two sides can be compared in.

    Case-folded and absolute, because the same file is `C:\\work\\A.TXT` to one side and
    `c:\\work\\a.txt` to the other, and a comparison that says they differ is worse than no check: it
    refuses a promise that was kept.
    """
    return os.path.normcase(os.path.abspath(path))


def _resolve(cwd: str, path: str | os.PathLike) -> str:
    """A path as flint will resolve it: against `cwd`, not this interpreter's own directory."""
    return os.path.normpath(os.path.join(cwd, os.fspath(path)))


def _token(path: str) -> str:
    """The `@` name for a path, quoted when it has to be.

    flint's scanner ends an unquoted name at whitespace (`src/attach.rs`), so a path with a space in it
    is only a name when it is quoted -- and a path containing a quote is not expressible at all, which
    is worth refusing here rather than sending a prompt whose token means something else.
    """
    if '"' in path:
        raise ValueError(
            f"a path with a quote in it cannot be written as an `@` name: {path!r}. "
            "Rename it, or read it into `inline=` yourself"
        )
    return f'@"{path}"' if any(c.isspace() for c in path) else f"@{path}"


def attached(turn: "Turn") -> list[dict]:
    """What flint inlined into this turn's prompt: `token`, `path`, `bytes` and `lines` per file.

    Read from `turn.started`, which is the only place the fact lives -- the stream deliberately does not
    carry the expanded prompt, because the caller already has those bytes on disk. An empty list is the
    useful answer: it is how a name that matched nothing looks, rather than a model quietly answering
    about a path.
    """
    for event in turn.of_type("turn.started"):
        return list(event.get("attachments") or [])
    return []


def verify_attached(turn: "Turn", paths: list[str], *, cwd: str) -> None:
    """Refuse a turn whose prompt did not get files the caller promised it would.

    The check `attach=` performs, exposed for a caller that assembled its own `@` names. Skipped when
    the run never started a turn, because then there is nothing to have attached and the run's own error
    is the answer the caller needs -- a missing key is not a missing attachment.
    """
    if not paths or not turn.of_type("turn.started"):
        return
    arrived = {_norm(os.path.join(cwd, a.get("path", ""))) for a in attached(turn)}
    missing = [p for p in paths if _norm(_resolve(cwd, p)) not in arrived]
    if missing:
        raise NotAttached(
            f"attach= promised {missing} and flint did not inline "
            f"{'it' if len(missing) == 1 else 'them'}"
            + (f" (it inlined {[a.get('path') for a in attached(turn)]})" if attached(turn) else "")
            + f"; the run ended as {turn.outcome or turn.error or turn.returncode}",
            missing,
            turn,
        )


def read_paths(turn: "Turn", *, cwd: str) -> set[str]:
    """Every path this turn read with the `read` tool, normalised for comparison.

    From the stream's own tool frames -- `tool.args` carries the arguments as JSON, under `file_path`
    (the name the schema asks for) or `path` (the older name flint still accepts).
    """
    read = set()
    for event in turn.of_type("tool.args"):
        if event.get("name") != "read":
            continue
        try:
            arguments = json.loads(event.get("arguments") or "{}")
        except ValueError:
            continue
        named = arguments.get("file_path") or arguments.get("path")
        if isinstance(named, str) and named:
            read.add(_norm(_resolve(cwd, named)))
    return read


def verify_read(turn: "Turn", paths: list[str], *, cwd: str) -> None:
    """Refuse a turn that did not read files the caller required it to. `require_read=`'s check."""
    if not paths or not turn.of_type("turn.started"):
        return
    read = read_paths(turn, cwd=cwd)
    missing = [os.fspath(p) for p in paths if _norm(_resolve(cwd, p)) not in read]
    if missing:
        raise NotRead(
            f"require_read= asked for {missing} to be read and this turn never read "
            f"{'it' if len(missing) == 1 else 'them'}"
            + (f" (it read {sorted(read)})" if read else " (it read nothing)")
            + f"; the run ended as {turn.outcome or turn.error or turn.returncode}",
            missing,
            turn,
        )


def _prepare_prompt(
    prompt: str,
    *,
    cwd: str,
    attach: list | None,
    paths: list | None,
    inline: list | None,
) -> tuple[str, list[str]]:
    """The prompt flint will be given, and the files `attach=` promised.

    Three arguments, three different promises, and folding them into one would leave the caller unable
    to say which is which:

    - `attach=[path]` is a **promise**: flint reads the file and its text is in the prompt. It travels
      as a real `@` name, so the model sees the same `<file>` block a hand-typed `@` produces, and the
      caller can check the fact afterwards (`attached`, and `ask` checks it for them).
    - `paths=[path]` is a **hope**: the names go into the prompt and the model decides whether to read
      them. It costs nothing up front and one tool call if the model takes the hint, which is the right
      door for "these might matter" rather than "read this".
    - `inline=[text]` is a **promise by construction**: the text is in the prompt because it *is* the
      prompt. Nothing is read from disk, so there is no `@` name and no `<file>` block; it is for text
      the caller has in memory, and `attach=` is better for anything already on disk.
    """
    parts = [prompt]
    for index, text in enumerate(inline or []):
        if not isinstance(text, str):
            raise TypeError(f"inline[{index}] is {type(text).__name__}, not str")
        parts.append(
            "The caller put this text into the prompt themselves (there is no file behind it):\n"
            f"<inline>\n{text}\n</inline>"
        )
    if paths:
        parts.append(
            "Paths that may be relevant, named by the caller: "
            + ", ".join(os.fspath(p) for p in paths)
        )
    promised: list[str] = []
    if attach:
        names = []
        for path in attach:
            resolved = _resolve(cwd, path)
            if not os.path.isfile(resolved):
                raise FileNotFoundError(
                    f"attach= names a file that is not there: {resolved}"
                )
            promised.append(resolved)
            names.append(_token(resolved))
        parts.append("Files the caller attached: " + " ".join(names))
    return "\n\n".join(parts), promised


def _check_command_line(argv: list[str]) -> None:
    """Refuse a command line this machine cannot carry, before flint is started.

    Called with the arguments as they will be passed and before any temporary file is written, so a
    refusal leaves nothing behind. See `COMMAND_LINE_LIMIT` for why the number is what it is and what
    the caller is meant to do instead.
    """
    total = sum(len(arg) + 1 for arg in argv)
    if total > COMMAND_LINE_LIMIT:
        raise ValueError(
            f"this call's command line is {total} characters, over the {COMMAND_LINE_LIMIT} this "
            "module allows: put the text in a file and pass it as attach=[path], which flint reads "
            "itself, instead of putting it in the prompt or in inline=[...]"
        )


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
    on_event: Callable[[dict], None] | None = None,
    on_delta: Callable[[str], None] | None = None,
    attach: list | None = None,
    paths: list | None = None,
    inline: list | None = None,
    require_read: list | None = None,
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
    `Chat` is the wrapper for a caller that wants one conversation over many calls: it passes a
    path rather than `--continue`, so two callers in one directory cannot land in each other's
    history.

    `on_event` is called with every frame as it arrives, and `on_delta` with each `message.delta`
    fragment's text -- so an answer can be rendered while it is being written instead of after the
    process has ended. Both are called from the thread reading the stream, one frame at a time, and
    the `Turn` that is returned holds the same frames: a callback is a view, not a second channel.
    A callback that raises is the caller's bug and is not allowed to leave a flint running -- the run
    is asked to stop like any other and the exception is raised once it has ended.

    `schema` asks for a checked answer: a dict (or a JSON string) is written to a temporary file
    and passed as `--schema`, because a schema handed over inline has to survive the shell on some
    machines and a temp file always does. `no_schema` is `--no-schema`, how a resumed conversation
    is asked for prose again. `ask_json` is the shorthand for the case where you want the object.

    `attach`, `paths` and `inline` are three ways to put a file in the prompt, and they make three
    different promises -- see `_prepare_prompt`. `attach=[path]` is the one to reach for: flint reads
    the file and its text is in the prompt, which is the only one of the three that does not depend on
    the model agreeing. It raises `NotAttached` if the promise did not hold. `require_read=[path]`
    checks the other direction -- what the run *did* -- and raises `NotRead` unless the file was read
    with the `read` tool. Both refusals happen after the run, and both carry the `Turn`, because the
    conversation is real and its answer may be worth keeping.
    """
    project = os.path.abspath(os.fspath(cwd))
    if not os.path.isdir(project):
        raise NotADirectoryError(f"cwd is not a directory: {project}")

    text, promised = _prepare_prompt(
        prompt, cwd=project, attach=attach, paths=paths, inline=inline
    )

    argv = [FLINT, "-p", text, "--json", "--cwd", project]
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

    # Checked before the schema file below, so a refusal leaves no temporary file behind, and before
    # the process, so it costs nothing: the schema's path is thirty characters and never what makes a
    # prompt too long.
    _check_command_line(argv)

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

        errors: list[str] = []
        threading.Thread(target=pump, args=(proc.stdout, out.put), daemon=True).start()
        threading.Thread(target=pump, args=(proc.stderr, errors.append), daemon=True).start()

        # Built before the run rather than after it, because the frames are folded in as they arrive:
        # that is what lets a caller render an answer while it is being written, and it is why the
        # returned `Turn` and the callbacks cannot disagree -- they are the same objects.
        turn = Turn()
        callback_error: BaseException | None = None

        def deliver(event: dict) -> None:
            """Hand one frame to the caller's callbacks, if it has any.

            A callback that raises is the caller's bug, and it still must not leave a flint running:
            the run is asked to stop, exactly as a timeout asks, and the exception is raised once the
            process has ended. Stopping rather than killing for the same reason `timeout` does -- the
            half-answer is committed to the session and a person reading it later finds work.
            """
            nonlocal callback_error
            if callback_error is not None or (on_event is None and on_delta is None):
                return
            try:
                if on_event is not None:
                    on_event(event)
                if on_delta is not None and event.get("type") == "message.delta":
                    on_delta(event.get("text", ""))
            except BaseException as exc:
                callback_error = exc
                try:
                    proc.stdin.write("/stop\n")
                    proc.stdin.flush()
                except (BrokenPipeError, ValueError):
                    pass

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
            event = _absorb(turn, line)
            if event is not None:
                deliver(event)
        proc.wait()
        returncode = proc.returncode
        # The end marker the pump sends is not stderr: the same pump feeds both pipes.
        stderr = "".join(e for e in errors if e)
    finally:
        if schema_file:
            os.unlink(schema_file)

    turn.returncode = returncode
    turn.stderr = stderr
    turn.stopped = stopped
    if callback_error is not None:
        # Raised now rather than at the frame that caused it: by here the run has ended and the
        # process is reaped, so nothing of this call is left behind on the machine.
        raise callback_error
    # The promises, checked against what the run reported rather than against what was asked for. This
    # is the order they belong in: the answer is collected first, so a refusal carries it instead of
    # throwing away work that is already committed to the session and already paid for.
    verify_attached(turn, promised, cwd=project)
    verify_read(turn, list(require_read or []), cwd=project)
    return turn


class SessionMoved(RuntimeError):
    """A pinned conversation was resumed, and flint opened a different session file.

    Raised by `Chat`, and it is a refusal rather than a warning on purpose: the whole reason `Chat`
    pins a path is that "the newest session for this directory" is a race the moment two callers share
    a directory. If the file that was opened is not the file that was asked for, the answer about to
    come back belongs to another conversation, and a caller that carries on has silently mixed two.
    `SessionMoved.session` is what flint actually opened, for a caller that wants to look at it.
    """

    def __init__(self, message: str, session: str | None):
        super().__init__(message)
        self.session = session


class DamagedSession(ValueError):
    """A session file has a line in it that is not an event.

    The reader rules are `docs/session-format.md`'s: an unknown `type` is skipped in silence -- that
    is what lets the format grow -- while a line that is not JSON at all, or an event with no `type`,
    is damage and is reported. Skipping it would hand back a conversation with a hole in it, which is
    the shape of bug this whole project is written against.
    """

    def __init__(self, message: str, path: str, line: int):
        super().__init__(message)
        self.path = path
        self.line = line


class Chat:
    """One conversation over many calls, pinned to one session file.

    `ask` is enough for a single question, and `continue_last=True` is enough for a few in a row --
    until two callers work in one directory, which is exactly when "the newest session here" stops
    being a name for the conversation you mean. This class removes that from the caller's life: the
    first call creates the session and reports its path on `session.started`, and every call after it
    passes that path to `--resume`, so the file is decided once rather than re-derived every time.

        chat = Chat(cwd="/path/to/project", home="/tmp/scratch")
        chat.ask("read @rules.csv and tell me the columns")
        chat.ask("now the third row")            # same conversation, by path
        print(chat.history()[-1])                # the record, as the file has it

    It is not a service and not a cache: there is no background process, every call is a fresh flint,
    and `history()` reads the file rather than remembering anything. A `Chat` that is dropped and
    recreated with `session=` continues the same conversation, because the session file is the state.
    """

    def __init__(
        self,
        *,
        cwd: str | os.PathLike,
        session: str | None = None,
        provider: str | None = None,
        model: str | None = None,
        home: str | None = None,
        timeout: float = 600.0,
        extra: list[str] | None = None,
    ):
        self.cwd = os.path.abspath(os.fspath(cwd))
        if not os.path.isdir(self.cwd):
            raise NotADirectoryError(f"cwd is not a directory: {self.cwd}")
        # A path flint will accept as `--resume`: an existing conversation this Chat continues, or
        # `None` for a new one whose path is learned from the first call's `session.started`.
        self.session = session
        self.provider = provider
        self.model = model
        self.home = home
        self.timeout = timeout
        self.extra = list(extra or [])
        self.turns: list[Turn] = []

    @property
    def answer(self) -> str:
        """What the last turn said, or "" before the first one."""
        return self.turns[-1].answer if self.turns else ""

    def ask(
        self,
        prompt: str,
        *,
        schema: dict | str | None = None,
        no_schema: bool = False,
        timeout: float | None = None,
        extra: list[str] | None = None,
        on_event: Callable[[dict], None] | None = None,
        on_delta: Callable[[str], None] | None = None,
        attach: list | None = None,
        paths: list | None = None,
        inline: list | None = None,
        require_read: list | None = None,
    ) -> Turn:
        """Say one thing in this conversation and return the turn.

        Everything `ask` takes that describes *this call* is here; everything that describes the
        conversation was settled in the constructor. `schema` and `no_schema` are per call because
        flint records the shape in force in the session and one question in a conversation may want a
        checked answer while the next wants prose. `attach`, `paths`, `inline` and `require_read` are
        per call for the same reason: what this question needs is not what the last one needed.

        A refusal (`NotAttached`, `NotRead`) is raised *after* the conversation has been recorded and
        the session pinned, because the run happened: the answer is on disk, and a second call has to
        continue this conversation rather than start another one beside it.
        """
        try:
            turn = ask(
                prompt,
                cwd=self.cwd,
                resume=self.session,
                provider=self.provider,
                model=self.model,
                home=self.home,
                timeout=self.timeout if timeout is None else timeout,
                extra=self.extra + list(extra or []),
                schema=schema,
                no_schema=no_schema,
                on_event=on_event,
                on_delta=on_delta,
                attach=attach,
                paths=paths,
                inline=inline,
                require_read=require_read,
            )
        except (NotAttached, NotRead) as refused:
            # The run happened even though the promise did not, so the conversation exists and its
            # answer is worth keeping. Recording it before the exception goes on is what stops the
            # *next* call from writing a second conversation while this one holds the answer the
            # caller was refused.
            self.turns.append(refused.turn)
            if self.session is None:
                self.session = refused.turn.session
            raise
        if self.session is None:
            # The first call is the one that names the conversation. `turn.session` is what flint
            # opened, taken from `session.started` -- the file, not a guess about where it would be.
            self.session = turn.session
        elif turn.session != self.session:
            raise SessionMoved(
                f"asked to continue {self.session}, and flint opened {turn.session}: refusing to "
                "treat the answer as part of this conversation",
                turn.session,
            )
        self.turns.append(turn)
        return turn

    def history(self) -> list[dict]:
        """Every event in the session file, in order.

        The file is the state, so this is a read and not a memory: it works for a `Chat` created with
        `session=` a moment ago and sees what a *different* process appended to the same conversation.
        Unknown event types are passed through -- the caller filters -- while a line that is not an
        event raises `DamagedSession`, because a conversation with a hole in it is worse than an
        exception. An empty list means the file is empty or the conversation has not been written to
        yet, which is the state of a `Chat` before its first `ask`.
        """
        if not self.session:
            return []
        return read_session(self.session)

    def messages(self) -> list[dict]:
        """The conversation as the model sees it: the `chat` events' messages, in order.

        Two things are missing from it on purpose, and both are in `history()`: the tool results are
        in the message's `tool_calls`/`tool_call_id` fields rather than flattened here, and a peer's
        message is a `peer` event and *not* a chat message -- which is the rule that keeps a mailbox
        from reaching a model, enforced by the file's shape rather than by a check.
        """
        return [e["message"] for e in self.history() if e.get("type") == "chat" and "message" in e]


def read_session(path: str | os.PathLike) -> list[dict]:
    """Read a session file into events. `Chat.history` is this, and it is public because a caller with
    a path and no wish to build a `Chat` should not have to reimplement the reader."""
    events: list[dict] = []
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError as exc:
                raise DamagedSession(
                    f"{path}:{number} is not JSON: {exc}", str(path), number
                ) from None
            if not isinstance(event, dict) or "type" not in event:
                raise DamagedSession(f"{path}:{number} has no type", str(path), number)
            events.append(event)
    return events


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

    Everything else (`home`, `continue_last`, `resume`, `timeout`, `extra`, `attach`, `paths`,
    `inline`, `require_read`) is `ask`'s -- and so is every check it makes, which are worth having
    here too: a structured answer about a file that was never attached is exactly as wrong as a prose
    one, and harder to notice.
    """
    turn = ask(prompt, cwd=cwd, schema=schema, **options)
    if turn.result is None:
        raise SchemaError(
            turn.error or f"the run produced no checked answer (exit {turn.returncode})", turn
        )
    return turn.result


# One flint process per job, so this is also the number of simultaneous bills. Eight is where flint's
# own `tasks` tool stops for the same reason, and the two should not disagree about what "a lot at once"
# means: a caller that wants more than eight can run two batches, which is a decision made on purpose.
MAX_WORKERS = 8
# The default, and four rather than eight: a batch is often the first thing a caller does with a new
# account, and being wrong about the price should cost a quarter of what being wrong about it costs at
# the cap. Raising it is one argument.
DEFAULT_WORKERS = 4


class OutOfBalance(RuntimeError):
    """A batch was stopped because the account is empty.

    The batch-level half of `ROADMAP.md` §10 B6, and it is the one failure a caller cannot handle one
    call at a time: flint can say *what* went wrong (`Turn.error_code`) but it cannot know that twenty
    other calls are queued behind the one that failed, and a caller that keeps going pays for nineteen
    more refusals. So the first `insufficient_balance` cancels the rest of the batch and raises this,
    which is a decision about the *batch* and therefore belongs here.

    `.turns` is what was asked and answered, in the order asked for, the stopping call included: its
    answer is empty and its `error_code` is the reason. `.asked` is how many the batch had and
    `.cancelled` is the difference -- the calls never made, so a caller can say precisely how much of
    the work did not happen. `.turn` is the call that ended it.
    """

    def __init__(self, message: str, turns: list["Turn"], asked: int, turn: "Turn"):
        super().__init__(message)
        self.turns = turns
        self.asked = asked
        self.turn = turn
        self.cancelled = asked - len(turns)


def map_calls(
    prompts: list,
    *,
    cwd: str | os.PathLike,
    workers: int = DEFAULT_WORKERS,
    stop_on: str | tuple | list | None = ("insufficient_balance",),
    on_turn: Callable[[int, "Turn"], None] | None = None,
    **options,
) -> list["Turn"]:
    """Ask several questions at once, one flint process each, and return the turns in order.

    `prompts` is a list where each entry is a prompt string or a dict of `ask`'s arguments (so one
    batch can attach a different file to each question). `options` are `ask`'s, applied to every call;
    a dict entry overrides them for its own call. The result is one `Turn` per prompt, in the order
    asked for, so `[t.answer for t in map_calls(...)]` lines up with the input.

    **Every call gets its own conversation.** A worker does not hand its session to the next job, and
    that is a decision rather than an omission: the alternative -- one session per *worker*, which is
    what the roadmap first sketched -- would make which calls share a conversation depend on the thread
    schedule, so the same batch with `workers=2` and with `workers=8` would produce different answers
    from different contexts. A batch is a set of independent questions or it is not a batch; a caller
    that wants a conversation across a worker's jobs wants `Chat`, one per thread, which is four lines
    and no guessing.

    `on_turn(index, turn)` is called as each call ends, from the worker thread that made it (like
    `ask`'s own callbacks), so a hundred-call batch can report progress instead of looking hung.

    Two things are deliberately *not* raised:

    - a refused promise (`NotAttached`, `NotRead`) is recorded on that turn as `.refused` and the batch
      carries on, because one job's refusal must not throw away ninety-nine answers. `ask` raises it;
      `map_calls` records it, and `.refused` is how a caller finds it.
    - a failed *run* is not an error at all: `turn.ok` is `False` and `turn.error` says why, exactly as
      for one call. A batch of ten questions about a broken endpoint returns ten of those.

    An exception from `ask` that is neither of the above (a missing `cwd`, a command line over the
    limit, no flint binary) is re-raised when the batch ends: it is the caller's own argument, every job
    would fail the same way, and it happens before anything is spent.

    `stop_on` names the `error_code` values that stop the whole batch -- by default
    `insufficient_balance`, because paying for nineteen more refusals teaches nobody anything. A call
    already in flight is *not* killed; it is paid for and its answer is real, and killing it would
    throw that away to save nothing. `stop_on=()` disables the breaker for a caller that wants all of
    it, and raising `OutOfBalance` is how the caller finds out that the rest did not happen.
    """
    if not 1 <= workers <= MAX_WORKERS:
        raise ValueError(f"workers={workers} is outside 1..{MAX_WORKERS}")
    if isinstance(stop_on, str):
        stop_on = (stop_on,)
    codes = set(stop_on or ())
    jobs = list(prompts)
    if not jobs:
        return []
    stopped = threading.Event()
    stopping: list[Turn] = []
    stopping_lock = threading.Lock()
    skipped = object()

    def one(index: int, job) -> "Turn | object":
        # The breaker is checked here rather than by cancelling futures: a call that has not started is
        # the only thing "cancel the rest" can honestly mean, and asking before each job says exactly
        # that. Futures that are already running are left to finish.
        if stopped.is_set():
            return skipped
        fields = {"prompt": job} if isinstance(job, str) else dict(job)
        prompt = fields.pop("prompt")
        try:
            turn = ask(prompt, cwd=cwd, **{**options, **fields})
        except (NotAttached, NotRead) as refused:
            turn = refused.turn
            turn.refused = refused
        if turn.error_code in codes:
            # The first one wins: later calls can only repeat it, and the batch's reason should be the
            # call that was actually first, not whichever thread happened to finish soonest.
            with stopping_lock:
                if not stopping:
                    stopping.append(turn)
            stopped.set()
        if on_turn is not None:
            on_turn(index, turn)
        return turn

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(one, index, job) for index, job in enumerate(jobs)]
        # `.result()` re-raises whatever a job raised, after the block has waited for every worker --
        # so a broken argument never leaves a flint process behind.
        done = [future.result() for future in futures]

    turns = [turn for turn in done if turn is not skipped]
    if stopping:
        turn = stopping[0]
        raise OutOfBalance(
            f"the batch stopped after {len(turns)} of {len(jobs)} calls: "
            f"{turn.error or 'the account is empty'}"
            f" (exit {turn.returncode}); {len(jobs) - len(turns)} calls were never made",
            turns,
            len(jobs),
            turn,
        )
    return turns
