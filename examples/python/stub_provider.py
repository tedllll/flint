"""A tiny OpenAI-compatible stub, so the Python side can be tested without a key.

A prompt containing `[[bash]]` is answered with a bash tool call and then answered for real, so the
tool round is exercised; anything else is answered with prose that mentions the command output, since
that is what a request following the tool result contains. Everything is served as SSE, the same shape
`tests/agent_loop.rs` uses with wiremock.

The markers are what a *fake model* needs and a real one does not: a rule for what it "decides". They
are visible in the conversation rather than hidden in this file's argv, which is what makes the checks
that use them checks on a run rather than checks on a hand-built event. Three of them, plus the delay:

- `[[bash]]` is answered with a `bash` tool call (`echo flint-python-ok`) -- the tool round.
- `[[read: <path>]]` is answered with a real `read` tool call for that path, and then answered for
  real, so `require_read`'s positive case is a run that read something.
- `[[balance]]` is refused with the `402` an empty DeepSeek account produces, so the batch breaker can
  be tested without an empty account.
- `delay=<seconds>` makes every answer wait, and `/stats` reports how many requests were waiting at
  once. `max_in_flight` is measured rather than inferred from a stopwatch: a batch that ran six calls
  in parallel and one that ran them one after another differ in that number and in nothing else a test
  can see without timing the machine.
- `FLINT_STUB_LOG` names a file and every request body is appended to it, one JSON object per line.
  The stream says what flint did; the request says what the *model* was given, which is the only place
  the difference between a file attached, a file named, and a file inlined can be seen.
"""

import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CALLS = {"n": 0}
# How many answers this stub has given to each conversation, keyed by its first user message, so what
# it does for one conversation does not depend on how many others ran before it.
ANSWERS: dict[str, int] = {}
# Set by the `stall` argument: write one fragment and then say nothing, for ever. That is the shape a
# stop has to be tested against -- an answer drawn but unfinished -- and it cannot be produced by a
# response with a fixed body, which is what the rest of this file serves.
STALL = False
# Set by `delay=<seconds>`: how long every answer waits. A batch's parallelism is otherwise invisible.
DELAY = 0.0
# Requests waiting right now, and the most that ever were, for `/stats`. Counted around the delay,
# which is where all the waiting in this file happens.
IN_FLIGHT = {"now": 0, "max": 0}
IN_FLIGHT_LOCK = threading.Lock()
LOG = os.environ.get("FLINT_STUB_LOG")
READ_MARKER = re.compile(r"\[\[read:\s*(?P<path>[^\]]+?)\s*\]\]")
BALANCE_MARKER = "[[balance]]"
BASH_MARKER = "[[bash]]"


def in_flight(delta):
    with IN_FLIGHT_LOCK:
        IN_FLIGHT["now"] += delta
        IN_FLIGHT["max"] = max(IN_FLIGHT["max"], IN_FLIGHT["now"])


def first_user(body):
    """The conversation's opening question, which is what this stub keys a script by."""
    for message in body.get("messages", []):
        if message.get("role") == "user":
            return message.get("content") or ""
    return ""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep the test output readable
        pass

    def do_GET(self):
        """The preflight's two questions, so `balance()` can be tested without a key.

        Answered here rather than not at all because the endpoint a DeepSeek-shaped provider is
        recognised by is *behaviour*: flint asks `/user/balance`, and a stub that returns 404 for it
        exercises the fallback instead of the money path. The amounts are deliberately odd numbers --
        a round figure is one a test can pass by printing a default.
        """
        if self.path.endswith("/user/balance"):
            body = json.dumps({
                "is_available": True,
                "balance_infos": [{
                    "currency": "CNY",
                    "total_balance": "41.50",
                    "granted_balance": "1.50",
                    "topped_up_balance": "40.00",
                }],
            }).encode("utf-8")
        elif self.path.endswith("/models"):
            body = json.dumps({"data": [{"id": "stub-model"}]}).encode("utf-8")
        elif self.path.endswith("/stats"):
            # What the batch checks read: how many calls this stub answered, and how many were waiting
            # at the same time at the busiest moment. `max_in_flight` is the parallelism evidence --
            # one means the caller's "parallel" batch was a loop in disguise.
            body = json.dumps({"calls": CALLS["n"], "max_in_flight": IN_FLIGHT["max"]}).encode("utf-8")
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        CALLS["n"] += 1
        if LOG:
            with open(LOG, "a", encoding="utf-8") as out:
                out.write(json.dumps(body, ensure_ascii=False) + "\n")
        prompt = first_user(body)
        answered = ANSWERS.get(prompt, 0)
        ANSWERS[prompt] = answered + 1
        if DELAY:
            in_flight(1)
            try:
                time.sleep(DELAY)
            finally:
                in_flight(-1)
        # The empty account, in the shape DeepSeek answers with: `402` and a sentence. flint reads the
        # body rather than the status -- a `429` says `insufficient_quota` in the same words -- and a
        # batch is the one caller that has to stop rather than count the refusals.
        if BALANCE_MARKER in prompt:
            raw = json.dumps({"error": {"message": "Insufficient Balance"}}).encode("utf-8")
            self.send_response(402)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            return
        # A schema run asks for JSON in the request itself (`response_format`), and the answer has to
        # oblige or the run is testing flint's retry ladder instead of its structured output. Checked
        # before the counter below, so this does not disturb the tool-call sequence the other checks
        # depend on -- a schema run and a prose run are different conversations.
        if body.get("response_format") and not STALL:
            raw = (
                'data: {"choices":[{"delta":{"content":"{\\"ok\\": true}"}}]}\n\n'
                'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\n'
                'data: {"choices":[],"usage":{"prompt_tokens":21,"completion_tokens":5}}\n\n'
                "data: [DONE]\n\n"
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            return
        if STALL:
            # No Content-Length and no chunking: the body ends when the connection does, and this one
            # does not end on its own. The client is left waiting, which is the state `/stop` exists
            # for -- and the fragment already written is what the caller has read when it stops.
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write('data: {"choices":[{"delta":{"content":"半句答案"}}]}\n\n'.encode("utf-8"))
            self.wfile.flush()
            time.sleep(120)
            return
        # The `[[read: …]]` rule first: it is a conversation of its own, and answering its second
        # request is what makes `require_read`'s positive case a run that really read a file.
        marker = READ_MARKER.search(prompt)
        if marker and answered == 0:
            chunks = [
                {"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "call_read_1", "type": "function",
                     "function": {"name": "read",
                                  "arguments": json.dumps(
                                      {"path": marker.group("path").strip()})}}]}}]},
                {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]},
                {"choices": [], "usage": {"prompt_tokens": 31, "completion_tokens": 7}},
            ]
        elif marker:
            chunks = [
                {"choices": [{"delta": {"content": "看过了。"}}]},
                {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                {"choices": [], "usage": {"prompt_tokens": 64, "completion_tokens": 4}},
            ]
        elif BASH_MARKER in prompt and answered == 0:
            # The tool round: the model "decides" to run a command, and its second request carries the
            # result and falls through to the prose below. Keyed by the question rather than by "the
            # first request this process ever saw", which is what it used to be -- that made the shape
            # of a *batch's* first call depend on which worker's request arrived first.
            chunks = [
                {"choices": [{"delta": {"content": "我先看一下。"}}]},
                {"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "call_1", "type": "function",
                     "function": {"name": "bash", "arguments": ""}}]}}]},
                {"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": '{"command":"echo flint-python-ok"}'}}]}}]},
                {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]},
                {"choices": [], "usage": {"prompt_tokens": 41, "completion_tokens": 9}},
            ]
        else:
            chunks = [
                {"choices": [{"delta": {"content": "命令输出是 "}}]},
                {"choices": [{"delta": {"content": "`flint-python-ok`"}}]},
                {"choices": [{"delta": {"content": "。"}}]},
                {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                {"choices": [], "usage": {"prompt_tokens": 88, "completion_tokens": 12}},
            ]
        payload = "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"
        raw = payload.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8791
    for argument in sys.argv[2:]:
        # `stall` keeps its bare form: it is the older argument and the check for it reads better as a
        # word than as `stall=true`.
        if argument == "stall":
            STALL = True
        elif argument.startswith("delay="):
            DELAY = float(argument.split("=", 1)[1])
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"stub listening on http://127.0.0.1:{port}/v1", flush=True)
    server.serve_forever()
