"""A tiny OpenAI-compatible stub, so the Python side can be tested without a key.

First request: the model "decides" to run a bash command (so the tool round is exercised).
Second request: the model "answers". Everything is served as SSE, the same shape
`tests/agent_loop.rs` uses with wiremock.
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CALLS = {"n": 0}
# Set by the `stall` argument: write one fragment and then say nothing, for ever. That is the shape a
# stop has to be tested against -- an answer drawn but unfinished -- and it cannot be produced by a
# response with a fixed body, which is what the rest of this file serves.
STALL = False


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep the test output readable
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        CALLS["n"] += 1
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
        if CALLS["n"] == 1:
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
    STALL = len(sys.argv) > 2 and sys.argv[2] == "stall"
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"stub listening on http://127.0.0.1:{port}/v1", flush=True)
    server.serve_forever()
