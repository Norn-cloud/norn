#!/usr/bin/env python3
"""Discovery-only scripted checker task; no live-LLM or governed-call claim."""
import json
import selectors
import subprocess
import sys
import tempfile
from pathlib import Path

READ_TIMEOUT_S = 30.0


def _require(condition, detail="assertion failed"):
    if not condition:
        raise SystemExit(f"mcp-check-task: {detail}")


def _readline(stream, timeout=READ_TIMEOUT_S):
    with selectors.DefaultSelector() as selector:
        selector.register(stream, selectors.EVENT_READ)
        if not selector.select(timeout):
            raise SystemExit(f"mcp-check-task: timed out waiting {timeout}s for MCP response")
    line = stream.readline()
    if not line:
        raise SystemExit("mcp-check-task: MCP server closed stdout unexpectedly")
    return line


if len(sys.argv) != 3:
    raise SystemExit("usage: mcp-check-task.py NORN_BIN DEFINITIONS_PATH")
with tempfile.TemporaryDirectory() as temporary:
    context = Path(temporary) / "context.json"
    context.write_text("{}")
    process = subprocess.Popen([sys.argv[1], "mcp"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, text=True)
    sequence = 0
    def request(method, params):
        global sequence
        sequence += 1
        process.stdin.write(json.dumps(dict(jsonrpc="2.0", id=sequence,
                                           method=method, params=params)) + "\n")
        process.stdin.flush()
        response = json.loads(_readline(process.stdout))
        _require(response["id"] == sequence, response)
        return response["result"]
    try:
        _require(request("ping", {}) == {}, "initial ping failed")
        request("initialize", dict(protocolVersion="2025-11-25", capabilities={},
                                   clientInfo=dict(name="task-script", version="1")))
        catalog = request("tools/list", dict(limit=1))
        digest = catalog["catalogDigest"]
        first_cursor = catalog.get("nextCursor")
        tools = catalog["tools"]
        while "nextCursor" in catalog:
            catalog = request("tools/list", dict(cursor=catalog["nextCursor"], limit=1))
            _require(catalog["catalogDigest"] == digest, "catalog digest drifted across pages")
            tools += catalog["tools"]
        _require(len(tools) > 1, "task must traverse an actual catalog page boundary")
        discovery = next(t for t in tools if "list the available human operations" in t["description"].lower())
        discovered = request("tools/call", dict(name=discovery["name"], arguments={}))
        _require(discovered["isError"] is False, discovered)
        shared = discovered["structuredContent"]
        _require(shared["tools"] == tools and shared["catalogDigest"] == digest, "discovery mismatch")
        cursor_field = next(name for name, field in discovery["inputSchema"]["properties"].items()
                            if "nextcursor" in field["description"].lower())
        continued = request("tools/call", dict(name=discovery["name"], arguments={cursor_field: first_cursor}))
        _require(continued["isError"] is False, continued)
        next_page = continued["structuredContent"]
        _require(next_page["tools"] == tools[1:] and next_page["catalogDigest"] == digest, "continuation mismatch")
        stale = request("tools/call", dict(name=discovery["name"], arguments={cursor_field: "0" * 64 + ":" + first_cursor.rsplit(":", 1)[1]}))
        _require(stale["isError"] is True and stale["structuredContent"]["code"] == "request.invalid", stale)
        _require("catalog or authority changed" in stale["structuredContent"]["explanation"].lower(), stale)
        _require("restart" in stale["structuredContent"]["explanation"].lower(), stale)
        tool = next(t for t in tools if "checking never installs" in t["description"].lower())
        properties = tool["inputSchema"]["properties"]
        arguments = {}
        for name, field in properties.items():
            description = field["description"].lower()
            arguments[name] = sys.argv[2] if "array" in description else str(context)
        checked = request("tools/call", dict(name=tool["name"], arguments=arguments))
        _require(checked["isError"] is False, checked)
        _require(checked["structuredContent"]["admitted"] is True, checked)
        _require("receiptSha256" in checked["structuredContent"], checked)
        context.write_text("{malformed context")
        refused = request("tools/call", dict(name=tool["name"], arguments=arguments))
        _require(refused["isError"] is True, refused)
        failure = refused["structuredContent"]
        _require(failure["format"] == "norn/refusal@2", failure)
        _require(failure["code"] == "request.invalid", failure)
        _require("retry" in failure["explanation"].lower(), failure)
        _require(json.loads(refused["content"][0]["text"]) == failure, refused)
        context.write_text("{}")
        repaired = request("tools/call", dict(name=tool["name"], arguments=arguments))
        _require(repaired["isError"] is False, repaired)
        _require(request("ping", {}) == {}, "final ping failed")
        process.stdin.close()
        _require(process.wait(timeout=15) == 0, "MCP process exited non-zero")
    finally:
        if process.stdin and not process.stdin.closed:
            process.stdin.close()
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    print("MCP task: paginated shared human catalog, discovered checker, checked admitted definitions, explained repairable refusals")
