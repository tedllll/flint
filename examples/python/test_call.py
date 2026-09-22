"""Does calling flint from Python actually work? Run against the local stub, so it costs nothing.

    cargo build && python examples/python/test_call.py

It checks the properties a caller depends on: the answer comes back assembled, the tool round
happens and is visible, the session file is real and hand-readable, `--continue` keeps the same
conversation, two projects in one home cannot be handed each other's history, and a dead endpoint is
reported as an error rather than as an empty answer.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from flint_call import ask, FLINT  # noqa: E402

HERE = Path(__file__).parent
# Which binary these run is `flint_call._binary`'s business: the build in this checkout when there is
# one, `FLINT_BIN` over it, `PATH` otherwise. Written down once so a check cannot pass against a
# build that is not the one being changed.
FAILURES = []


def check(name, condition, detail=""):
    print(f"  {'ok  ' if condition else 'FAIL'} {name}" + (f"  -- {detail}" if detail else ""))
    if not condition:
        FAILURES.append(name)


def attempt(thunk):
    """Run a call and hand back `(result, exception)` rather than raising.

    A suite that stops at the first problem hides every problem after it, and the refusals this file
    checks are raised *out* of `ask` on purpose (`NotAttached`, `NotRead`): the check that follows
    reports the exception as the failure it is instead of a traceback ending the run.
    """
    try:
        return thunk(), None
    except Exception as exc:  # noqa: BLE001 -- the exception is the check's subject
        return None, exc


def why(result, blew_up):
    """The detail line for a check whose call may have raised."""
    if blew_up is not None:
        return f"{type(blew_up).__name__}: {blew_up}"
    return f"rc={result.returncode}" if result is not None else "no result"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def session_files(home):
    """Every session file under a home.

    One level down is included because a conversation lives in the subdirectory belonging to the
    working directory it was held in -- `sessions/<name>-<hash>/<id>.jsonl` -- with only the
    sessions written before that layout sitting directly in `sessions/`.
    """
    root = Path(home) / "sessions"
    return sorted(root.glob("*.jsonl")) + sorted(root.glob("*/*.jsonl"))


def request_log(home):
    """Every request body the stub was sent, in order.

    The stream says what flint did; this says what the *model* was given, which is the only place the
    difference between the three ways of putting a file in a prompt is visible: `attach=` puts the
    text in, `paths=` puts a name in, `inline=` is the prompt.
    """
    path = Path(home) / "requests.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def last_request(home):
    """The last request's messages, joined: what the model was given, as text.

    The messages rather than the whole body, because the body also holds the tool schemas and the JSON
    encoding of the path separators on Windows -- a check that searched the raw JSON would pass on a
    schema's mention of a word and fail on a path that really was in the prompt.
    """
    entries = request_log(home)
    if not entries:
        return ""
    parts = []
    for message in entries[-1].get("messages", []):
        content = message.get("content")
        if isinstance(content, str):
            parts.append(content)
    return "\n".join(parts)


def main():
    scratch = Path(tempfile.mkdtemp(prefix="flint-py-"))
    (scratch / "sessions").mkdir(parents=True, exist_ok=True)
    port = free_port()
    stub = subprocess.Popen(
        [sys.executable, str(HERE / "stub_provider.py"), str(port)],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        # Every request body the stub is sent, so a check can see what the *model* was given rather
        # than only what flint said it did. That is the difference between `attach=`, `paths=` and
        # `inline=`, and it is not visible in the stream.
        env={**os.environ, "FLINT_STUB_LOG": str(scratch / "requests.jsonl")},
    )
    stub.stdout.readline()  # "stub listening on ..."
    # The two settings that differ by platform are the *values*, not the file. Written as one
    # conditional expression they are not: `+` binds tighter than `if/else`, so the `else` branch was
    # the entire config and a Linux run got a file holding `shell_args = ["-c"]` and nothing else --
    # no provider, so nothing to default to. flint then exited 1 without writing a session file, and
    # every check after this one in the file read as a product failure. Windows never saw it and this
    # file was written on Windows (`f12fcaa`, 2026-09-15); the first CI run of this check on ubuntu
    # found it, which is what it was added to the workflow for.
    shell = "cmd" if os.name == "nt" else "sh"
    shell_args = ["/C"] if os.name == "nt" else ["-c"]
    (scratch / "config.toml").write_text(
        'default_provider = "stub"\n'
        f"shell = {json.dumps(shell)}\n"
        f"shell_args = {json.dumps(shell_args)}\n",
        encoding="utf-8",
    )
    with open(scratch / "config.toml", "a", encoding="utf-8") as f:
        f.write(
            "\n[[providers]]\n"
            'name = "stub"\n'
            f'base_url = "http://127.0.0.1:{port}/v1"\n'
            'model = "stub-model"\n'
            'api_key = "not-a-real-key"\n'
        )
    try:
        print(f"scratch FLINT_HOME: {scratch}")
        # Which flint this is, said out loud, for the reason `flint_call._binary` gives: a check that
        # ran an installed build passes for the wrong reason, and a log that does not name the binary
        # cannot tell the two apart afterwards. The claim is the mirror of the one in
        # `examples/mcp/test_mcp.py`, where the rule was wrong and this is what caught it.
        built = HERE.parents[1] / "target" / "debug" / ("flint.exe" if os.name == "nt" else "flint")
        print(f"flint under test: {FLINT}")
        check("a build in this checkout is what runs, not whichever flint is installed",
              Path(FLINT).is_file()
              and (not built.exists()
                   or os.environ.get("FLINT_BIN")
                   or Path(FLINT).resolve() == built.resolve()),
              f"ran {FLINT}; this checkout has {built}")
        print("\n1. one turn, with a tool round in the middle")
        turn = ask("[[bash]] 跑个命令看看", home=str(scratch), cwd=str(HERE))
        kinds = [e["type"] for e in turn.events]
        print(f"   events: {kinds}")
        check("exit code 0", turn.returncode == 0, f"rc={turn.returncode} stderr={turn.stderr[:200]}")
        # The deltas are the whole turn, which includes what the model said *before* deciding to
        # run a tool -- here "我先看一下。". `messages` is the per-message view: the final answer
        # alone. Both are useful and they are not the same string.
        check("deltas cover the whole turn, prose before the tool call included",
              turn.answer == "我先看一下。命令输出是 `flint-python-ok`。", repr(turn.answer))
        # `message.completed` is emitted once, at the end, and holds the same text: it is the
        # turn's answer, not one model message. So a caller can take either -- deltas if it wants
        # to render as it arrives, `message.completed` if it waited for the end anyway.
        check("message.completed is the whole turn's answer, once",
              turn.messages == ["我先看一下。命令输出是 `flint-python-ok`。"], repr(turn.messages))
        check("the session was named", bool(turn.session), str(turn.session))
        check("the model was named", turn.model == "stub-model", str(turn.model))
        check("tool.started seen", any(e["type"] == "tool.started" for e in turn.events))
        check("tool.completed seen and ok",
              any(e["type"] == "tool.completed" and e.get("ok") for e in turn.events))
        check("the tool actually ran",
              any("flint-python-ok" in (e.get("output") or "") for e in turn.events
                  if e["type"] == "tool.completed"))
        check("usage reported", bool(turn.usage), str(turn.usage))
        check("the turn says it finished complete", turn.outcome == "complete", str(turn.outcome))
        check("so `complete` is true for it", turn.complete, str(turn.returncode))

        print("\n2. the session file on disk")
        files = session_files(scratch)
        check("exactly one session file", len(files) == 1, str([f.name for f in files]))
        check("it is in the directory of the run it belongs to",
              files[0].parent != scratch / "sessions" and files[0].parent.parent == scratch / "sessions",
              str(files[0]))
        lines = files[0].read_text(encoding="utf-8").splitlines()
        parsed = [json.loads(line) for line in lines]
        check("every line is JSON", len(parsed) == len(lines))
        check("line one is the meta line", parsed[0]["type"] == "meta")
        check("it holds our prompt",
              any(p.get("type") == "chat" and "跑个命令看看" in json.dumps(p, ensure_ascii=False)
                  for p in parsed))

        print("\n3. a second call continues the same conversation")
        turn2 = ask("那刚才输出了什么？", continue_last=True, home=str(scratch), cwd=str(HERE))
        check("exit code 0", turn2.returncode == 0, f"rc={turn2.returncode}")
        check("same session file", turn2.session == turn.session, f"{turn2.session} vs {turn.session}")
        check("still one file", len(session_files(scratch)) == 1)
        check("the second answer came back", bool(turn2.answer), repr(turn2.answer))

        print("\n4. a dead endpoint is an error, not an empty answer")
        dead = Path(tempfile.mkdtemp(prefix="flint-py-dead-"))
        (dead / "sessions").mkdir(parents=True, exist_ok=True)
        (dead / "config.toml").write_text(
            'default_provider = "dead"\n\n[[providers]]\nname = "dead"\n'
            'base_url = "http://127.0.0.1:9/v1"\nmodel = "nope"\napi_key = "x"\n',
            encoding="utf-8",
        )
        bad = ask("hello", home=str(dead), cwd=str(HERE), timeout=120)
        check("non-zero exit", bad.returncode != 0, f"rc={bad.returncode}")
        # 75, `EX_TEMPFAIL`: the retries ran out and asking again later is the right advice. This is
        # what the typed provider failures bought -- a dead endpoint is retryable, an empty account is
        # not, and a caller can tell them apart without reading either message. The call takes about
        # fifteen seconds, which is the retry ladder (1+2+4+8) doing its job for the last time.
        check("a retryable failure says so", bad.returncode == 75, f"rc={bad.returncode}")
        check("and names its cause", bad.error_code == "network", str(bad.error_code))
        check("with `retryable` set", bad.error_retryable is True, str(bad.error_retryable))
        check("an error event arrived", bool(bad.error), repr(bad.error)[:160])
        check("and no answer was invented", bad.answer == "", repr(bad.answer))
        shutil.rmtree(dead, ignore_errors=True)

        print("\n5. a session can be resumed by name")
        sid = Path(turn.session).stem
        turn3 = ask("继续", resume=sid, home=str(scratch), cwd=str(HERE))
        check("resumed the same file", turn3.session == turn.session, str(turn3.session))

        print("\n6. two projects in one home do not share a conversation")
        one = Path(tempfile.mkdtemp(prefix="flint-py-one-"))
        two = Path(tempfile.mkdtemp(prefix="flint-py-two-"))
        try:
            first = ask("一号项目的问题", home=str(scratch), cwd=one)
            second = ask("二号项目的问题", home=str(scratch), cwd=two)
            check("each call recorded its own directory",
                  first.cwd != second.cwd and first.session != second.session,
                  f"{first.cwd} / {second.cwd}")
            check("and each session lives in its own directory",
                  Path(first.session).parent != Path(second.session).parent,
                  f"{Path(first.session).parent.name} / {Path(second.session).parent.name}")
            # The point of `--cwd` being recorded: continuing from one project finds *that*
            # project's conversation, from a process started anywhere.
            again_one = ask("接着说", continue_last=True, home=str(scratch), cwd=one)
            again_two = ask("接着说", continue_last=True, home=str(scratch), cwd=two)
            check("continuing project one found project one's conversation",
                  again_one.session == first.session, str(again_one.session))
            check("continuing project two found project two's conversation",
                  again_two.session == second.session, str(again_two.session))
        finally:
            shutil.rmtree(one, ignore_errors=True)
            shutil.rmtree(two, ignore_errors=True)

        print("\n8. a run that is asked to stop keeps the half-answer")
        # A stub that writes one fragment and then says nothing, which is the only shape in which
        # this can be tested: there has to be something drawn for the stop to keep.
        stall_port = free_port()
        stall = subprocess.Popen(
            [sys.executable, str(HERE / "stub_provider.py"), str(stall_port), "stall"],
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        stall.stdout.readline()
        try:
            with open(scratch / "config.toml", "a", encoding="utf-8") as f:
                f.write(
                    "\n[[providers]]\n"
                    'name = "stall"\n'
                    f'base_url = "http://127.0.0.1:{stall_port}/v1"\n'
                    'model = "stub-model"\n'
                    'api_key = "not-a-real-key"\n'
                )
            began = time.monotonic()
            stopped = ask("慢慢想", provider="stall", home=str(scratch), cwd=str(HERE), timeout=2.0)
            took = time.monotonic() - began
            # The point of `timeout` asking rather than killing: the run ends in about the time the
            # caller asked for, instead of being torn down with its writes half done.
            check("a run past its timeout is stopped, not left running", stopped.stopped, str(stopped.stopped))
            check("and it ended when it was asked, not minutes later", took < 15, f"{took:.1f}s")
            # A stopped run is not a failure and not a success. This check used to assert `ok` --
            # which was flint exiting 0 for a truncated answer, so a caller looping on `ok` treated
            # half an answer as a finished one. The stream was honest the whole time and the exit
            # code was not; both now say "stopped".
            check("a stopped run exits 130, not 0", stopped.returncode == 130, str(stopped.returncode))
            check("so it is not `ok` -- the answer is half of one", not stopped.ok, str(stopped.error))
            check("and not `complete` either", not stopped.complete, str(stopped.outcome))
            check("but it is not an error -- nothing went wrong",
                  stopped.error is None, str(stopped.error))
            check("the turn says how it ended", stopped.outcome == "stopped", str(stopped.outcome))
            check("what had been drawn is still in the answer",
                  "半句答案" in stopped.answer, repr(stopped.answer))
            check("the stream says it was stopped",
                  any("stopped" in w for w in stopped.warnings), str(stopped.warnings))
            # The promise behind all of it: the half-answer the caller read is in the record, so the
            # next call about it is not answered as if nothing had been written.
            recorded = "\n".join(p.read_text(encoding="utf-8") for p in session_files(scratch))
            check("and what the caller read is in the session file", "半句答案" in recorded)

            # Streaming, as opposed to "the callback ran eventually". The stub writes its fragment
            # immediately and then says nothing, and the run is only asked to stop after `timeout`, so
            # a reader that collected every line and parsed it once the process had ended could not
            # have called back a second early -- which is what makes this a measurement rather than a
            # restatement. The margin is a whole second on a run this short.
            fragments = []
            began = time.monotonic()
            streamed = ask(
                "慢慢想",
                provider="stall",
                home=str(scratch),
                cwd=str(HERE),
                timeout=3.0,
                on_delta=lambda text: fragments.append((time.monotonic() - began, text)),
            )
            ended = time.monotonic() - began
            check("a fragment reached the callback", bool(fragments), repr(fragments))
            check("while the run was still going, not after it ended",
                  bool(fragments) and fragments[0][0] < ended - 1.0,
                  f"first at {fragments[0][0]:.1f}s of {ended:.1f}s" if fragments else "never")
            check("and the stopped turn still holds what was drawn",
                  "半句答案" in streamed.answer, repr(streamed.answer))

            # A callback that raises is the caller's bug, and it must not leave a flint running: the
            # run is asked to stop like any other and the exception arrives once it has ended. Without
            # that, this call would sit here for the 30 seconds it was given.
            def refuse(event):
                raise ValueError("callback said no")

            raised = None
            began = time.monotonic()
            try:
                ask("慢慢想", provider="stall", home=str(scratch), cwd=str(HERE),
                    timeout=30.0, on_event=refuse)
            except ValueError as exc:
                raised = exc
            check("a callback that raises raises, and does not leave a flint running",
                  raised is not None and time.monotonic() - began < 15,
                  f"{raised} after {time.monotonic() - began:.1f}s")
        finally:
            stall.terminate()
            try:
                stall.wait(timeout=5)
            except subprocess.TimeoutExpired:
                stall.kill()

        print("\n9. the preflight: asking before spending")
        # The stub answers `/user/balance`, which is what makes flint ask the money question at all:
        # the check is behavioural, so a stub that 404s it would exercise the `/models` fallback
        # instead. The amounts are odd on purpose -- a round figure is one a default could fake.
        from flint_call import balance  # noqa: E402  (kept beside its use, like the rest)

        ready = balance(home=str(scratch))
        check("the provider is usable", ready.ok, f"rc={ready.returncode} err={ready.error}")
        check("and it says what was checked", ready.checked == "balance", str(ready.checked))
        check("with the amount, not a guess", ready.total == "41.50", str(ready.total))
        check("the currency comes with it", ready.currency == "CNY", str(ready.currency))
        check("the granted part is separate from the topped-up part",
              ready.granted == "1.50" and ready.topped_up == "40.00",
              f"{ready.granted} / {ready.topped_up}")
        check("and it says which provider answered", ready.provider == "stub", str(ready.provider))
        # A provider that cannot even be resolved answers nothing at all, and that must not read as
        # ready: the empty answer is the failure mode this command is written against.
        missing = balance(provider="not-a-provider", home=str(scratch))
        check("an unresolvable provider is not `ok`", not missing.ok, str(missing.returncode))
        check("and it says why", bool(missing.error), repr(missing.error))

        print("\n10. a refusal reaches the stream, not only stderr")
        # Before `ROADMAP.md` §10 B7 this was the one failure a reader of stdout could not see: the
        # command line is refused before the stream is opened, so the reason was on stderr and stdout
        # was empty. A `--json` caller reads one channel, and it now carries the refusal.
        refused = ask("any question", home=str(scratch), cwd=str(HERE), extra=["--nope"])
        check("a mistyped flag is refused", refused.returncode == 2, str(refused.returncode))
        check("and the reason is on the stream the caller reads",
              bool(refused.error) and "--nope" in refused.error, repr(refused.error))
        check("which is why stderr is silent about it", refused.stderr.strip() == "",
              repr(refused.stderr[:200]))
        check("and nothing pretended to be a session", refused.session is None, str(refused.session))

        print("\n11. a document too big for a command line travels as a name")
        # The wall this is written against was measured from here: a 33k prompt does not reach flint
        # at all, because Windows caps a command line at ~32k and Python's own `subprocess` raises
        # `FileNotFoundError [WinError 206]` -- the caller learns about it in the worst possible way,
        # and no amount of care inside flint could help. `@path` is the way through: the argument
        # stays four characters and flint puts the contents in the prompt itself, where the model
        # cannot decline to look at them.
        document = scratch / "rules.csv"
        document.write_text(
            "".join(f"row {n},2026-09-{(n % 28) + 1:02d}\n" for n in range(1, 12001)),
            encoding="utf-8",
        )
        size = document.stat().st_size
        check("the fixture really is too big for a command line", size > 32768, str(size))
        attached = ask(
            "apply @rules.csv to today",
            home=str(scratch),
            cwd=str(scratch),
        )
        check("the call is not refused by the operating system", attached.returncode == 0,
              f"rc={attached.returncode} stderr={attached.stderr[:200]}")
        started = next(e for e in attached.events if e["type"] == "turn.started")
        check("the turn says what was asked, in the caller's own words",
              started["prompt"] == "apply @rules.csv to today", repr(started.get("prompt")))
        files = started.get("attachments") or []
        check("and that the file went in", len(files) == 1, repr(started.get("attachments")))
        if files:
            check("with its real size, not the size of the name",
                  files[0]["bytes"] == size and files[0]["lines"] == 12000, repr(files[0]))
            check("and the path it was read from", Path(files[0]["path"]).is_file(), repr(files[0]))
        # A name that matches nothing is prose, and the empty list is how a caller sees that rather
        # than a model quietly answering about a path: this is the typo case.
        typo = ask("apply @rules.csvv to today", home=str(scratch), cwd=str(scratch))
        started = next(e for e in typo.events if e["type"] == "turn.started")
        check("a mistyped name is left as prose rather than inlined",
              not started.get("attachments"), repr(started.get("attachments")))
        check("and the run still works", typo.returncode == 0, str(typo.returncode))

        print("\n12. one conversation over many calls, by path")
        # `continue_last=True` re-derives "the newest conversation for this directory" on every call,
        # which is a race the moment two callers share a directory -- and one file has one writer by
        # design. `Chat` learns the path from the first call's `session.started` and passes it to
        # `--resume` afterwards, so the conversation is decided once.
        from flint_call import Chat  # noqa: E402  (kept beside its use, like the rest)

        chat = Chat(cwd=str(scratch), home=str(scratch))
        callback_events = []
        callback_deltas = []
        first_turn = chat.ask(
            "第一个问题",
            on_event=callback_events.append,
            on_delta=callback_deltas.append,
        )
        check("the first call named the conversation", bool(chat.session), str(chat.session))
        check("and it is the file flint said it opened",
              chat.session == first_turn.session, f"{chat.session} vs {first_turn.session}")
        check("every frame reached the callback, in order",
              [e["type"] for e in callback_events] == [e["type"] for e in first_turn.events],
              f"{len(callback_events)} vs {len(first_turn.events)}")
        check("and the fragments were handed over as text, not as frames",
              "".join(callback_deltas) == first_turn.answer, repr("".join(callback_deltas)))

        files_before = len(session_files(scratch))
        second_turn = chat.ask("第二个问题")
        check("the second call continued that file",
              second_turn.session == first_turn.session, str(second_turn.session))
        check("so no second conversation was created",
              len(session_files(scratch)) == files_before,
              f"{files_before} -> {len(session_files(scratch))}")

        history = chat.history()
        check("the record reads back", bool(history) and history[0]["type"] == "meta",
              str(history[:1])[:120])
        check("both questions are in it",
              sum(1 for e in history
                  if e.get("type") == "chat" and e.get("message", {}).get("role") == "user") == 2,
              str([e.get("type") for e in history]))
        check("and the conversation is what the model saw, in order",
              [m.get("role") for m in chat.messages()][:2] == ["user", "assistant"],
              str([m.get("role") for m in chat.messages()]))
        check("a second Chat on the same path reads the same record",
              Chat(cwd=str(scratch), home=str(scratch), session=chat.session).messages()
              == chat.messages())
        print("\n13. three ways to put a file in a prompt, and only two of them are promises")
        # `attach=` is a promise: flint reads the file and its text is in the prompt. `paths=` is a
        # hope: the names are in the prompt and the model decides whether to read them. `inline=` is a
        # promise by construction: the text *is* the prompt. Folding them into one argument would leave
        # the caller unable to say which is which, which is the whole reason they are three.
        from flint_call import (  # noqa: E402
            Chat, NotAttached, NotRead, Turn, attached, verify_attached)

        notes = scratch / "notes.txt"
        notes.write_text("THE-NOTES-MARKER\n", encoding="utf-8")

        promised, blew_up = attempt(
            lambda: ask("what does the attachment say?", attach=[notes],
                        home=str(scratch), cwd=str(scratch))
        )
        check("attach= ran", promised is not None and promised.returncode == 0,
              why(promised, blew_up))
        check("and flint really inlined it, as a `@` name",
              promised is not None and [os.path.normcase(a["path"]) for a in attached(promised)]
              == [os.path.normcase(str(notes))],
              repr(attached(promised) if promised else None))
        check("so the model was given the file's text, not its name",
              "THE-NOTES-MARKER" in last_request(scratch),
              last_request(scratch)[-300:])
        ask("maybe look at notes.txt?", paths=[notes], home=str(scratch), cwd=str(scratch))
        body = last_request(scratch)
        check("paths= names the path to the model", str(notes) in body, body[:200])
        check("and leaves the reading of it to the model", "THE-NOTES-MARKER" not in body,
              "the file's text was in the prompt, which is `attach=`'s job")

        ask("answer from this text", inline=["INLINE-MARKER"], home=str(scratch), cwd=str(scratch))
        check("inline= is in the prompt because it is the prompt",
              "INLINE-MARKER" in last_request(scratch), last_request(scratch)[-300:])

        before_refusal = len(request_log(scratch))
        try:
            ask("never mind", attach=[scratch / "nope.txt"], home=str(scratch), cwd=str(scratch))
            check("attach= refuses a file that is not there", False, "no exception")
        except FileNotFoundError as exc:
            check("attach= refuses a file that is not there", "nope.txt" in str(exc), str(exc))
        check("and refuses it without starting a run",
              len(request_log(scratch)) == before_refusal, "a request was sent anyway")

        # The wall is the operating system's, and it is the whole command line: a prompt that does not
        # fit fails before flint exists, with an error that names no argument (measured -- see §11).
        # `attach=` is the way through, so the refusal has to say so.
        try:
            ask("anything", inline=["x" * 40000], home=str(scratch), cwd=str(scratch))
            check("a prompt too big for a command line is refused here", False, "no exception")
        except ValueError as exc:
            check("a prompt too big for a command line is refused here",
                  "attach=" in str(exc), str(exc))
        check("and that refusal also spends nothing",
              len(request_log(scratch)) == before_refusal, "a request was sent anyway")

        # A file past flint's own inline cap is refused by the run itself, with the reason on the
        # stream a caller reads: the promise is kept by saying it cannot be, not by dropping the file.
        huge = scratch / "huge.txt"
        huge.write_text("y" * (300 * 1024), encoding="utf-8")
        over, blew_up = attempt(
            lambda: ask("read the attachment", attach=[huge],
                        home=str(scratch), cwd=str(scratch))
        )
        check("a file past the inline cap fails the run",
              over is not None and over.returncode != 0, why(over, blew_up))
        check("and the stream says why", over is not None and "bytes" in (over.error or ""),
              repr(over.error if over else None)[:200])
        huge.unlink()

        print("\n14. `require_read` is checked against what the run did")
        read_path = scratch / "to-read.txt"
        read_path.write_text("READ-MARKER\n", encoding="utf-8")
        read_turn, blew_up = attempt(
            lambda: ask(f"[[read: {read_path}]] what is in it?", require_read=[read_path],
                        home=str(scratch), cwd=str(scratch))
        )
        check("a run that read the file passes",
              read_turn is not None and read_turn.returncode == 0, why(read_turn, blew_up))
        check("and the stream is where that was seen",
              read_turn is not None and any(
                  e["type"] == "tool.args" and e.get("name") == "read" for e in read_turn.events),
              str([e.get("name") for e in (read_turn.events if read_turn else [])
                   if e["type"] == "tool.args"]))

        try:
            ask("answer without looking", require_read=[read_path],
                home=str(scratch), cwd=str(scratch))
            check("a run that did not read it is refused", False, "no exception")
        except NotRead as exc:
            check("a run that did not read it is refused", "to-read.txt" in str(exc), str(exc))
            check("and the refusal carries the turn, so the answer is not lost",
                  bool(exc.turn and exc.turn.answer), repr(exc.turn.answer if exc.turn else None))

        # The promise is checked against what flint *says* it attached, so a flint that stopped
        # inlining would be an error here rather than a model answering about a file it never saw.
        # Fabricated rather than driven, because there is no way to make today's flint stay silent:
        # that is the case the check exists for.
        silent = Turn()
        silent.events.append({"type": "turn.started", "prompt": "see @notes.txt"})
        try:
            verify_attached(silent, [str(notes)], cwd=str(scratch))
            check("an attachment that never arrived is refused", False, "no exception")
        except NotAttached as exc:
            check("an attachment that never arrived is refused",
                  "notes.txt" in str(exc), str(exc))

        # A path with a quote in it cannot be written as an `@` name at all -- flint's scanner ends a
        # quoted name at the first quote, so the token would mean a different file -- and it is refused
        # before a prompt is built rather than sent as something else. Through `_token` rather than
        # through `ask`, because Windows cannot have such a file and the guard is for the machines that
        # can: the alternative was no guard, and a prompt whose token names the wrong thing.
        import flint_call  # noqa: E402

        try:
            flint_call._token('/tmp/a"b.txt')
            check("a path a prompt cannot express is refused", False, "no exception")
        except ValueError as exc:
            check("a path a prompt cannot express is refused", "quote" in str(exc), str(exc))

        print("\n15. a refused promise still pins the conversation")
        # A promise that fails after the run is not a run that did not happen: the conversation is real
        # and the answer is in the file, so `Chat` records it before the exception goes on. Otherwise
        # the next call would start a *second* conversation while the first holds what was refused.
        chat_refused = Chat(cwd=str(scratch), home=str(scratch))
        try:
            chat_refused.ask("answer without looking", require_read=[read_path])
            check("the refusal reached the caller", False, "no exception")
        except NotRead:
            check("the refusal reached the caller", True)
        check("and the conversation was pinned anyway", bool(chat_refused.session),
              str(chat_refused.session))
        files_before = len(session_files(scratch))
        chat_refused.ask("and now answer anyway")
        check("so the next call continues it rather than starting one",
              len(session_files(scratch)) == files_before,
              f"{files_before} -> {len(session_files(scratch))}")

        print("\n16. a batch runs several calls at once, and each one has its own conversation")
        # A batch is the whole reason this module has a `map_calls`: six calls with a stub that takes a
        # second to answer take one second on six workers and six in a loop. The stub counts how many
        # requests were waiting at once, because a stopwatch on a busy machine is a weaker witness than
        # the endpoint saying "three of your calls were here together".
        from flint_call import map_calls  # noqa: E402

        slow_port = free_port()
        slow = subprocess.Popen(
            [sys.executable, str(HERE / "stub_provider.py"), str(slow_port), "delay=1.0"],
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        slow.stdout.readline()
        try:
            with open(scratch / "config.toml", "a", encoding="utf-8") as f:
                f.write(
                    "\n[[providers]]\n"
                    'name = "slow"\n'
                    f'base_url = "http://127.0.0.1:{slow_port}/v1"\n'
                    'model = "stub-model"\n'
                    'api_key = "not-a-real-key"\n'
                )
            questions = [f"batch job {n}" for n in range(1, 7)]
            began = time.monotonic()
            turns = map_calls(questions, cwd=str(scratch), home=str(scratch), provider="slow",
                              workers=6)
            took = time.monotonic() - began
            check("every call came back", len(turns) == len(questions), str(len(turns)))
            check("in the order they were asked, not the order they finished",
                  [t.answer for t in turns] == ["命令输出是 `flint-python-ok`。"] * len(questions),
                  repr([t.answer for t in turns])[:120])
            check("and each one is its own conversation",
                  len({t.session for t in turns}) == len(questions),
                  str(sorted(t.session for t in turns)))
            # Each turn says how long it took, and these were held a second each, so the number is
            # checked against something known rather than merely being present.
            check("and every turn says how long it waited",
                  all(isinstance(t.duration_ms, int) and t.duration_ms >= 900 for t in turns),
                  str([t.duration_ms for t in turns]))
            check("no call was asked twice", all(t.ok for t in turns),
                  str([t.error for t in turns if not t.ok]))
            stats = json.loads(
                urllib.request.urlopen(f"http://127.0.0.1:{slow_port}/stats", timeout=5).read()
            )
            check("the endpoint saw several calls at once, so this is really parallel",
                  stats["max_in_flight"] >= 3, str(stats))
            # Six calls of a second each: six in a loop, about one in parallel. The bound is loose on
            # purpose -- what a timing check may prove here is the order of magnitude, and the count
            # above is the precise part.
            check("and the batch took about one call's time, not six",
                  took < 4.0, f"{took:.1f}s for six 1s calls")

            # A dict entry is one call's own arguments, so a batch can ask the same question about
            # different files -- and the shared options still apply to the ones that say nothing.
            marked = [{"prompt": "batch job 7", "require_read": [notes]}, "batch job 8"]
            turns = map_calls(marked, cwd=str(scratch), home=str(scratch), provider="slow",
                              workers=2)
            check("a job that is a dict carries its own arguments", len(turns) == 2,
                  str(len(turns)))
            check("and its refusal is recorded on the turn rather than thrown at the caller",
                  isinstance(turns[0].refused, NotRead), repr(turns[0].refused))
            check("while the call beside it is unaffected", turns[1].refused is None,
                  repr(turns[1].refused))

            before = len(request_log(scratch))
            try:
                map_calls(questions, cwd=str(scratch), home=str(scratch), provider="slow",
                          workers=0)
                check("a worker count outside 1..8 is refused", False, "no exception")
            except ValueError as exc:
                check("a worker count outside 1..8 is refused", "workers=0" in str(exc), str(exc))
            check("and it refused before anything was asked",
                  len(request_log(scratch)) == before, "a request was sent anyway")

            print("\n17. the first empty account stops the batch")
            # The decision `ROADMAP.md` §10 B6 leaves to the caller, built: flint can say *what* went
            # wrong, but only the caller knows that nineteen other calls are queued behind the one that
            # failed -- and paying for nineteen more refusals teaches nobody anything.
            from flint_call import OutOfBalance  # noqa: E402

            many = [f"breaker job {n}" for n in range(1, 21)]
            many[2] = "breaker job 3 [[balance]]"
            before = len(request_log(scratch))
            broke = None
            try:
                map_calls(many, cwd=str(scratch), home=str(scratch), workers=2)
                check("the batch stops when the account is empty", False, "no exception")
            except OutOfBalance as exc:
                broke = exc
                check("the batch stops when the account is empty",
                      exc.turn.error_code == "insufficient_balance", repr(exc.turn.error_code))
                check("and it says how much of the batch did not happen",
                      exc.cancelled > 10 and exc.asked == len(many),
                      f"asked={exc.asked} ran={len(exc.turns)} cancelled={exc.cancelled}")
                check("the stopping call is one of the turns it hands back",
                      any(t is exc.turn for t in exc.turns), str(len(exc.turns)))
                check("the calls that had already run are kept, with their answers",
                      all(t.ok for t in exc.turns if t is not exc.turn),
                      str([t.error for t in exc.turns if not t.ok]))
                check("and the message says why, in the provider's own words",
                      "insufficient" in str(exc).lower(), str(exc))
            check("the caller really did get the exception", broke is not None, "no exception")
            asked = len(request_log(scratch)) - before
            check("and the calls that were never made were never sent",
                  asked < len(many), f"{asked} requests for {len(many)} jobs")

            # The breaker is the caller's choice, not a law: a caller that wants every answer, refusals
            # included, says so. Three jobs, one of them refused -- the other two still ran.
            wanted = ["breaker job a", "breaker job b [[balance]]", "breaker job c"]
            turns = map_calls(wanted, cwd=str(scratch), home=str(scratch), workers=2, stop_on=())
            check("`stop_on=()` runs the whole batch anyway", len(turns) == 3, str(len(turns)))
            check("with the refusal in the turn rather than an exception",
                  any(t.error_code == "insufficient_balance" for t in turns),
                  str([t.error_code for t in turns]))
            check("and the calls after it still answered",
                  sum(1 for t in turns if t.ok) == 2, str([t.ok for t in turns]))
        finally:
            slow.terminate()
            try:
                slow.wait(timeout=5)
            except subprocess.TimeoutExpired:
                slow.kill()
    finally:
        stub.terminate()
        try:
            stub.wait(timeout=5)
        except subprocess.TimeoutExpired:
            stub.kill()
        shutil.rmtree(scratch, ignore_errors=True)

    print()
    if FAILURES:
        print(f"{len(FAILURES)} failed: {FAILURES}")
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
