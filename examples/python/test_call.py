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
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from flint_call import ask  # noqa: E402

HERE = Path(__file__).parent
# Which binary these run is `flint_call._binary`'s business: the build in this checkout when there is
# one, `FLINT_BIN` over it, `PATH` otherwise. Written down once so a check cannot pass against a
# build that is not the one being changed.
FAILURES = []


def check(name, condition, detail=""):
    print(f"  {'ok  ' if condition else 'FAIL'} {name}" + (f"  -- {detail}" if detail else ""))
    if not condition:
        FAILURES.append(name)


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


def main():
    scratch = Path(tempfile.mkdtemp(prefix="flint-py-"))
    (scratch / "sessions").mkdir(parents=True, exist_ok=True)
    port = free_port()
    stub = subprocess.Popen(
        [sys.executable, str(HERE / "stub_provider.py"), str(port)],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    stub.stdout.readline()  # "stub listening on ..."
    (scratch / "config.toml").write_text(
        'default_provider = "stub"\n'
        "shell = " + json.dumps("cmd" if os.name == "nt" else "sh") + "\n"
        'shell_args = ["/C"]\n' if os.name == "nt" else 'shell_args = ["-c"]\n',
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
        print("\n1. one turn, with a tool round in the middle")
        turn = ask("跑个命令看看", home=str(scratch), cwd=str(HERE))
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
