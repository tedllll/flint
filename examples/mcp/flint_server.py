"""flint as an MCP tool, for agents that speak MCP instead of shell.

Codex, Claude Code, Cursor and anything else with MCP support gets one tool, `flint_ask`, which
runs a one-shot flint and returns its answer. That direction is the point: flint deliberately
consumes no MCP and spawns no agents, but *being* callable is what lets a parent agent use it
without inventing a shell pipeline -- and the answer comes back with the exit code, the cause and
the session path attached, so the caller can tell a finished answer from a half one and can point
at the conversation that produced it.

Stdio transport, newline-delimited JSON-RPC 2.0, standard library only -- the same rules as
`examples/python/flint_call.py`, for the same reasons: no dependencies, and the file is short
enough to read in one sitting before you point an agent at it.

Wire it into Codex (`~/.codex/config.toml`):

    [mcp_servers.flint]
    command = "python"
    args = ["C:\\\\path\\\\to\\\\flint\\\\examples\\\\mcp\\\\flint_server.py"]

Claude Code takes the same shape with `claude mcp add flint -- python <path>`. Set `FLINT_BIN` if
`flint` is not on PATH.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import threading

# JSON-RPC over stdio is UTF-8 by specification, and a client's own words are not ours to transcode: a
# prompt typed in Chinese reaches this file as UTF-8 and must reach flint as UTF-8, whatever the console
# code page on the machine running it. Without this, `sys.stdout` is encoded by that code page -- cp1252
# on a Windows runner, or the `gbk` a Chinese Windows install uses -- so a reply carrying a character the
# page lacks raises `UnicodeEncodeError` *inside the protocol*, which the client sees as the server
# dying. `reconfigure` is Python 3.7+; the guard is there because a copied example should not depend on
# the reader's interpreter being current, and the default (`ensure_ascii=True` in `json.dumps` below)
# keeps the wire ASCII-escaped either way.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

PROTOCOL_VERSION = "2024-11-05"

# What the tool tells the parent about itself. The wording matters more than it looks: an agent
# choosing a tool reads this, and the two facts worth stating are that flint may act on the machine
# (there is no permission layer) and that `readonly` is how you ask it not to.
TOOL = {
    "name": "flint_ask",
    "description": (
        "Ask flint -- a coding agent in one binary -- one question about a directory and get its "
        "answer. Use this for work that needs a real look at the files: reading and searching a "
        "codebase, running commands, editing files. By default it can run commands and write files "
        "in `cwd`; pass `readonly` to refuse both. Give a JSON Schema as `schema` to get a checked "
        "object back as well as prose. The answer is a summary, not a transcript, and it comes with "
        "flint's exit code and the path of the session file that holds the full record."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "What to ask, in the same words you would type at a terminal.",
            },
            "cwd": {
                "type": "string",
                "description": (
                    "The directory to work in, absolute. Defaults to the process's own directory, "
                    "which for an MCP server is usually not the project."
                ),
            },
            "schema": {
                "type": "object",
                "description": (
                    "A JSON Schema for the answer. The object flint validated is returned as "
                    "structured content instead of prose, and its exit code is 65 if no answer ever "
                    "matched."
                ),
            },
            "readonly": {
                "type": "boolean",
                "description": "Refuse writes, edits and mutating commands. Use for inspection only.",
            },
            "model": {"type": "string", "description": "Override the model for this call."},
            "provider": {"type": "string", "description": "Override the provider for this call."},
            "timeout_secs": {
                "type": "number",
                "description": (
                    "How long to wait before stopping the run (default 600). Stopping is graceful: "
                    "flint keeps the half-answer it had drawn."
                ),
            },
        },
        "required": ["prompt"],
    },
}

# Exit codes, spelled out for the parent agent: it cannot branch on a number it has to look up, and
# the difference between 69 and 75 is the difference between "stop the batch" and "try again".
MEANING = {
    0: "finished",
    1: "failed, cause not classified",
    2: "the arguments were wrong -- nothing was asked",
    65: "the answer is not usable: a schema that never matched, or a run out of steps",
    69: "a person must act: no key, rejected credentials, or an account with no balance",
    75: "the retries ran out; asking again later is right",
    130: "the run was stopped",
}


def flint_binary() -> str:
    return os.environ.get("FLINT_BIN") or "flint"


def run_flint(args: dict) -> dict:
    """Run one flint and turn its stream into an MCP tool result.

    The stream is read as it arrives rather than through `subprocess.run`, for one reason: a long
    run has to be stoppable, and flint's stdin is the channel that stops it without killing it -- the
    half-answer stays in the session file. A timeout therefore writes `/stop` and waits, and only
    kills if flint ignores it, which it should not.
    """
    prompt = args.get("prompt")
    if not isinstance(prompt, str) or not prompt.strip():
        return error_result("`prompt` is required and must be a non-empty string")

    cwd = args.get("cwd") or os.getcwd()
    if not os.path.isdir(cwd):
        return error_result(f"`cwd` is not a directory: {cwd}")

    timeout = float(args.get("timeout_secs") or 600)
    argv = [flint_binary(), "-p", prompt, "--json", "--cwd", os.path.abspath(cwd)]
    if args.get("readonly"):
        argv.append("--readonly")
    if args.get("model"):
        argv += ["--model", str(args["model"])]
    if args.get("provider"):
        argv += ["--provider", str(args["provider"])]

    schema_file = None
    if isinstance(args.get("schema"), dict):
        # Through a file: a schema inline has to survive the command line, and a path always does.
        handle = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8")
        json.dump(args["schema"], handle)
        handle.close()
        schema_file = handle.name
        argv += ["--schema", schema_file]

    try:
        proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        )

        collected: dict = {"text": [], "messages": [], "result": None, "error": None,
                           "error_code": None, "outcome": None, "session": None, "warnings": []}
        stopped = threading.Event()

        def read_stream() -> None:
            for line in proc.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    # Not a frame: flint promises one object per line, so this is damage rather than
                    # something to guess at, and it is worth passing on instead of dropping.
                    collected["warnings"].append(f"unparseable line: {line[:200]}")
                    continue
                kind = event.get("type")
                if kind == "message.delta":
                    collected["text"].append(event.get("text", ""))
                elif kind == "message.completed":
                    collected["messages"].append(event.get("text", ""))
                elif kind == "result":
                    collected["result"] = event.get("json")
                elif kind == "error":
                    collected["error"] = event.get("message")
                    collected["error_code"] = event.get("code")
                elif kind == "turn.completed":
                    collected["outcome"] = event.get("outcome")
                elif kind == "session.started":
                    collected["session"] = event.get("session")
                elif kind == "warning":
                    collected["warnings"].append(event.get("message", ""))

        reader = threading.Thread(target=read_stream, daemon=True)
        reader.start()
        # stderr is read as well as the stream, and kept: when a run ends without a single frame --
        # a provider that cannot be resolved, an unreadable config -- stderr is the only place the
        # reason exists, and an empty answer must not read as a clean one.
        complaints: list[str] = []

        def read_errors() -> None:
            if proc.stderr:
                complaints.append(proc.stderr.read())

        errors = threading.Thread(target=read_errors, daemon=True)
        errors.start()

        try:
            proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            # Graceful first: `/stop` is the same word the terminal takes, and it leaves the drawn
            # half-answer in the session file. Killing is the fallback, not the first move.
            try:
                if proc.stdin:
                    proc.stdin.write("/stop\n")
                    proc.stdin.flush()
                proc.wait(timeout=20)
            except (subprocess.TimeoutExpired, OSError):
                proc.kill()
                proc.wait()
            collected["warnings"].append(f"stopped after {timeout:g}s")
        finally:
            reader.join(timeout=5)
            errors.join(timeout=5)

        answer = "".join(collected["text"]) or "\n".join(collected["messages"])
        if not answer.strip() and complaints and complaints[0].strip():
            # Nothing on the stream at all: the reason is on stderr, and passing it on is the
            # difference between "flint failed" and "flint refused before it started".
            answer = complaints[0].strip()
        code = proc.returncode if proc.returncode is not None else -1
        failed = code != 0 or collected["error"] is not None

        # The answer first, because that is what the parent asked for, then the facts it needs to
        # decide what to do with it. An agent reading only the text still gets the cause.
        lines = [answer.strip() or "(no answer)"]
        lines.append("")
        lines.append(f"exit code: {code} ({MEANING.get(code, 'unknown')})")
        if collected["outcome"]:
            lines.append(f"outcome: {collected['outcome']}")
        if collected["error_code"]:
            lines.append(f"cause: {collected['error_code']}")
        if collected["error"]:
            lines.append(f"error: {collected['error'].strip().splitlines()[0]}")
        if collected["session"]:
            lines.append(f"session: {collected['session']}")
        if collected["warnings"]:
            lines.append("warnings: " + "; ".join(collected["warnings"][:5]))

        result: dict = {
            "content": [{"type": "text", "text": "\n".join(lines)}],
            "isError": failed,
        }
        if collected["result"] is not None:
            # MCP's structured content beside the prose: the same object `result` carried, already
            # validated by flint against the schema the caller passed.
            result["structuredContent"] = collected["result"]
        return result
    finally:
        if schema_file:
            try:
                os.unlink(schema_file)
            except OSError:
                pass


def error_result(message: str) -> dict:
    return {"content": [{"type": "text", "text": message}], "isError": True}


def handle(request: dict) -> dict | None:
    """One JSON-RPC message in, one out. `None` for a notification, which takes no answer."""
    method = request.get("method")
    ident = request.get("id")

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "flint", "version": "0.1.0"},
            },
        }
    if method in ("notifications/initialized", "notifications/cancelled"):
        return None
    if method == "ping":
        return {"jsonrpc": "2.0", "id": ident, "result": {}}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": ident, "result": {"tools": [TOOL]}}
    if method == "tools/call":
        params = request.get("params") or {}
        if params.get("name") != TOOL["name"]:
            return {
                "jsonrpc": "2.0",
                "id": ident,
                "error": {"code": -32602, "message": f"unknown tool '{params.get('name')}'"},
            }
        try:
            return {"jsonrpc": "2.0", "id": ident, "result": run_flint(params.get("arguments") or {})}
        except Exception as exc:  # a tool that raises must answer, not vanish
            return {"jsonrpc": "2.0", "id": ident, "result": error_result(f"{type(exc).__name__}: {exc}")}
    return {
        "jsonrpc": "2.0",
        "id": ident,
        "error": {"code": -32601, "message": f"method not found: {method}"},
    }


def main() -> int:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            # No id to answer, so this cannot be a JSON-RPC error object; dropping it in silence
            # would hide a broken parent, so it goes to stderr where a human can see it.
            print(f"flint-mcp: not JSON: {line[:200]}", file=sys.stderr, flush=True)
            continue
        reply = handle(request)
        if reply is not None:
            sys.stdout.write(json.dumps(reply) + "\n")
            sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
