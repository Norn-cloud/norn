#!/usr/bin/env python3
"""Discovery-only scripted checker task; no live-LLM or governed-call claim."""
import json, subprocess, sys, tempfile
from pathlib import Path
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
        response = json.loads(process.stdout.readline())
        assert response["id"] == sequence, response
        return response["result"]
    assert request("ping", {}) == {}
    request("initialize", dict(protocolVersion="2025-11-25", capabilities={},
                               clientInfo=dict(name="task-script", version="1")))
    catalog = request("tools/list", dict(limit=1))
    digest = catalog["catalogDigest"]
    first_cursor = catalog.get("nextCursor")
    tools = catalog["tools"]
    while "nextCursor" in catalog:
        catalog = request("tools/list", dict(cursor=catalog["nextCursor"], limit=1))
        assert catalog["catalogDigest"] == digest
        tools += catalog["tools"]
    assert len(tools) > 1, "task must traverse an actual catalog page boundary"
    discovery = next(t for t in tools if "list the available human operations" in t["description"].lower())
    discovered = request("tools/call", dict(name=discovery["name"], arguments={}))
    assert discovered["isError"] is False, discovered
    shared = discovered["structuredContent"]
    assert shared["tools"] == tools and shared["catalogDigest"] == digest
    cursor_field = next(name for name, field in discovery["inputSchema"]["properties"].items()
                        if "nextcursor" in field["description"].lower())
    continued = request("tools/call", dict(name=discovery["name"], arguments={cursor_field: first_cursor}))
    assert continued["isError"] is False, continued
    next_page = continued["structuredContent"]
    assert next_page["tools"] == tools[1:] and next_page["catalogDigest"] == digest
    stale = request("tools/call", dict(name=discovery["name"], arguments={cursor_field: "obsolete"}))
    assert stale["isError"] is True and stale["structuredContent"]["code"] == "request.invalid", stale
    assert "restart" in stale["structuredContent"]["explanation"].lower(), stale
    tool = next(t for t in tools if "checking never installs" in t["description"].lower())
    properties = tool["inputSchema"]["properties"]
    arguments = {}
    for name, field in properties.items():
        description = field["description"].lower()
        arguments[name] = sys.argv[2] if "array" in description else str(context)
    checked = request("tools/call", dict(name=tool["name"], arguments=arguments))
    assert checked["isError"] is False, checked
    assert checked["structuredContent"]["admitted"] is True
    assert "receiptSha256" in checked["structuredContent"]
    context.write_text("{malformed context")
    refused = request("tools/call", dict(name=tool["name"], arguments=arguments))
    assert refused["isError"] is True, refused
    failure = refused["structuredContent"]
    assert failure["format"] == "norn/refusal@2"
    assert failure["code"] == "request.invalid"
    assert "retry" in failure["explanation"].lower()
    assert json.loads(refused["content"][0]["text"]) == failure
    context.write_text("{}")
    repaired = request("tools/call", dict(name=tool["name"], arguments=arguments))
    assert repaired["isError"] is False, repaired
    assert request("ping", {}) == {}
    process.stdin.close()
    assert process.wait(timeout=15) == 0
    print("MCP task: paginated shared human catalog, discovered checker, checked admitted definitions, explained repairable refusals")
