"""The MCP server, spoken to the way a parent agent speaks to it.

What this checks is the contract a caller depends on and cannot see from reading the tool
description: the handshake, the tool listing, that a real answer arrives, that the exit code and the
cause travel with it, that structured output comes back structured, and -- the one that matters most
-- that a run which fails says so through `isError` instead of returning an empty answer that reads
like success.

Run it the way the README says: `python examples/mcp/test_mcp.py`. It needs a built `flint` (PATH or
`FLINT_BIN`) and spawns the same stub provider `examples/python/test_call.py` uses.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import shutil
from pathlib import Path

HERE = Path(__file__).resolve().parent
PYTHON_EXAMPLES = HERE.parent / "python"
sys.path.insert(0, str(PYTHON_EXAMPLES))

FAILURES: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  ok   {name}" + (f"  -- {detail}" if detail else ""))
    else:
        print(f"  FAIL {name}  -- {detail}")
        FAILURES.append(name)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    """The MCP server as a pipe: send a message, read the one answer that is not a notification."""

    def __init__(self, env: dict):
        self.proc = subprocess.Popen(
            [sys.executable, str(HERE / "flint_server.py")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            env=env,
        )

    def send(self, **message) -> dict | None:
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            return None
        return json.loads(line)

    def notify(self, method: str) -> None:
        """A notification has no answer by definition, so this must not read one.

        Waiting for a reply that the protocol says will not come is a hang, not a failure -- which is
        exactly what this test did before the method existed, and why it is named separately."""
        assert self.proc.stdin
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        self.proc.stdin.flush()

    def call(self, method: str, params: dict | None = None, ident: int = 1) -> dict:
        reply = self.send(jsonrpc="2.0", id=ident, method=method, params=params or {})
        assert reply is not None, f"no answer to {method}"
        return reply

    def close(self) -> None:
        if self.proc.stdin:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def main() -> int:
    scratch = Path(tempfile.mkdtemp(prefix="flint-mcp-"))
    port = free_port()
    stub = subprocess.Popen(
        [sys.executable, str(PYTHON_EXAMPLES / "stub_provider.py"), str(port)],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    stub.stdout.readline()  # "stub listening on ..."
    (scratch / "config.toml").write_text(
        'default_provider = "stub"\n'
        "\n[[providers]]\n"
        'name = "stub"\n'
        f'base_url = "http://127.0.0.1:{port}/v1"\n'
        'model = "stub-model"\n'
        'api_key = "not-a-real-key"\n',
        encoding="utf-8",
    )

    env = dict(os.environ)
    env["FLINT_HOME"] = str(scratch)
    env.setdefault("FLINT_BIN", shutil.which("flint") or "flint")

    server = Server(env)
    try:
        print("1. the handshake a parent agent makes first")
        ready = server.call(
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test"}},
        )
        check("initialize answers with a protocol version",
              ready["result"]["protocolVersion"] == "2024-11-05", str(ready))
        check("and says it has tools", "tools" in ready["result"]["capabilities"], str(ready))
        check("and names itself", ready["result"]["serverInfo"]["name"] == "flint", str(ready))
        server.notify("notifications/initialized")

        print("\n2. the tool listing, which is all an agent reads before choosing")
        listing = server.call("tools/list", ident=2)
        tools = listing["result"]["tools"]
        check("there is one tool", len(tools) == 1, str([t["name"] for t in tools]))
        tool = tools[0]
        check("named for what it does", tool["name"] == "flint_ask", tool["name"])
        check("its input schema is a JSON Schema object",
              tool["inputSchema"]["type"] == "object", str(tool["inputSchema"]["type"]))
        check("a prompt is the only required field",
              tool["inputSchema"]["required"] == ["prompt"], str(tool["inputSchema"]["required"]))
        check("and it says flint can act on the machine",
              "read" in tool["description"] and "readonly" in tool["description"], "")
        check("unknown methods are refused, not ignored",
              "error" in server.call("tools/nope", ident=3), "")

        print("\n3. a real run through the tool")
        called = server.call(
            "tools/call",
            {"name": "flint_ask", "arguments": {"prompt": "跑个命令看看", "cwd": str(HERE)}},
            ident=4,
        )
        result = called["result"]
        text = result["content"][0]["text"]
        check("not an error", result.get("isError") is False, text[:200])
        check("the answer arrived", "flint-python-ok" in text, text[:200])
        # The three facts that make this better than a shell pipeline, and the reason they are in the
        # text rather than only in metadata: an agent that reads just the text still gets them.
        check("the exit code travels with it", "exit code: 0" in text, text[-200:])
        check("so does the outcome", "outcome: complete" in text, text[-200:])
        check("and the session file it is recorded in", "session: " in text, text[-200:])

        print("\n4. structured output, when the caller asked for a shape")
        shaped = server.call(
            "tools/call",
            {
                "name": "flint_ask",
                "arguments": {
                    "prompt": "answer with JSON: {\"ok\": true}",
                    "cwd": str(HERE),
                    "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}},
                },
            },
            ident=5,
        )
        shaped_result = shaped["result"]
        check("the object comes back as structured content",
              isinstance(shaped_result.get("structuredContent"), dict), str(shaped_result)[:200])
        check("the object is the one flint validated",
              shaped_result.get("structuredContent", {}).get("ok") is True, str(shaped_result)[:200])
        check("and the prose is still there",
              bool(shaped_result["content"][0]["text"].strip()), "")

        # A schema that never matches is the case this whole vocabulary exists for: it must arrive as
        # a failure with exit 65 rather than as a success with an empty object. The stub answers
        # `{"ok": true}` and this schema asks for a string, so no attempt can satisfy it.
        unmatched = server.call(
            "tools/call",
            {
                "name": "flint_ask",
                "arguments": {
                    "prompt": "answer with JSON",
                    "cwd": str(HERE),
                    "schema": {"type": "object", "properties": {"ok": {"type": "string"}}},
                },
            },
            ident=51,
        )
        unmatched_text = unmatched["result"]["content"][0]["text"]
        check("a schema that never matched is a failure",
              unmatched["result"].get("isError") is True, unmatched_text[:200])
        check("with the exit code that says why",
              "exit code: 65" in unmatched_text, unmatched_text[-200:])

        print("\n5. a failure says so, and says why")
        broken_env = dict(env)
        broken_env["FLINT_HOME"] = str(scratch)
        broken = Server(broken_env)
        try:
            reply = broken.call(
                "tools/call",
                {"name": "flint_ask", "arguments": {"prompt": "hi", "provider": "not-a-provider"}},
                ident=6,
            )
            body = reply["result"]
            check("isError is set", body.get("isError") is True, str(body)[:200])
            check("the reason is in the text", "not-a-provider" in body["content"][0]["text"],
                  body["content"][0]["text"][:200])
        finally:
            broken.close()

        print("\n6. a call that could not even be built is an error result, not a crash")
        empty = server.call("tools/call", {"name": "flint_ask", "arguments": {}}, ident=7)
        check("a missing prompt is refused",
              empty["result"].get("isError") is True, str(empty)[:200])
        unknown = server.call("tools/call", {"name": "nope", "arguments": {}}, ident=8)
        check("an unknown tool is a JSON-RPC error", "error" in unknown, str(unknown)[:200])
    finally:
        server.close()
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
