#!/usr/bin/env python3
"""Install a lowered flow on a cell through the governed definitions path.

    NORN_TOKEN=<a definition writer's host token> \\
        seed_flow.py <cell-url> <orgId> <environmentId> [--dry-run]

The same path Studio uses (D59): POST /v1/definitions:admit returns the
server's restatement digest, and POST /v1/definitions:install signs exactly
that digest. Nothing here names a tenant; the flow is the checked-in
`lowerFlow` output of tools/norn-ts/fixtures/flows/ask.builder.ts ("Approve a
refund": one ask step, deciders admin or owner), scoped to the given cell.
Re-running is safe: a flow already installed at this version is left alone.
"""
import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.parse
import urllib.request

FIXTURE = Path(__file__).resolve().parents[2] / "tools/norn-ts/fixtures/flows/ask.ir.json"
FLOW = {
    "id": "ops.approve_refund",
    "version": "1.0.0",
    "title": "Approve a refund",
    "steps": [{"id": "ask_review", "label": "Review the refund"}],
}


def call(url, method, path, scope, token, body=None):
    """One request; returns (status, document)."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(
        f"{url}{path}?{scope}",
        data=data,
        method=method,
        headers={"authorization": f"Bearer {token}", "content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        text = error.read().decode() or "{}"
        return error.code, json.loads(text)


def main(argv):
    if len(argv) < 4:
        sys.exit(__doc__)
    url, org, environment = argv[1].rstrip("/"), argv[2], argv[3]
    dry_run = "--dry-run" in argv[4:]
    scope = urllib.parse.urlencode({"orgId": org, "environmentId": environment})
    artifacts = json.loads(FIXTURE.read_text())
    for artifact in artifacts:
        artifact["scope"] = {"orgId": org, "environmentId": environment}
    body = {"kind": "flow", "id": FLOW["id"], "payload": {"flow": FLOW, "artifacts": artifacts}}
    if dry_run:
        print(json.dumps(body, indent=2))
        return 0
    token = os.environ.get("NORN_TOKEN")
    if not token:
        sys.exit("NORN_TOKEN must hold a definition writer's host token")

    read, current = call(url, "GET", f"/v1/definitions/{FLOW['id']}", scope, token)
    if read == 200 and (current.get("flow") or {}).get("version") == FLOW["version"]:
        print(f"{FLOW['id']} {FLOW['version']} is already installed (revision {current.get('version')})")
        return 0
    if read not in (200, 404):
        sys.exit(f"reading {FLOW['id']} answered {read}: {json.dumps(current)}")

    status, admitted = call(url, "POST", "/v1/definitions:admit", scope, token, body)
    digest = admitted.get("restatementSha256")
    if status != 200 or not digest:
        sys.exit(f"admit answered {status}: {json.dumps(admitted)}")
    install = dict(body, signoff={"restatementSha256": digest})
    if read == 200 and current.get("version") is not None:
        install["expectedVersion"] = current["version"]
    status, installed = call(url, "POST", "/v1/definitions:install", scope, token, install)
    if status != 200:
        sys.exit(f"install answered {status}: {json.dumps(installed)}")
    print(f"installed {FLOW['id']} {FLOW['version']} as revision {installed.get('version')}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
