#!/usr/bin/env python3
"""Serve the built cell on a pinned runtime; read, refuse, restart, re-read.

NORN_CELL_DO_PROJECT holds worker.mjs, wrangler.json and pkg/ (outside the
checkout). Prints the definition document digest and request latencies.
"""
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(os.environ["NORN_CELL_DO_PROJECT"]).resolve()
URL = "http://127.0.0.1:9877"
RUNTIME = sys.argv[1] if len(sys.argv) > 1 else "celld"
COMMAND = {"celld": ["celld", "dev", "--port", "9877"],
           "wrangler": ["wrangler", "dev", "--local", "--port", "9877"]}[RUNTIME]
SCOPE = "orgId=org_demo&environmentId=dev"
ANALYTICS = "orgId=org_demo&environmentId=analytics"
ANSWER = "/v1/questions/demo.yield_week/answer"
TRACE = "/v1/questions/demo.yield_week/trace"
GUEST = "Bearer fixture:guest@demo"
VIEWER = "Bearer fixture:admin@demo"


# The analytics cell refuses to open without the stage's lineage key
# (NORN_LINEAGE_KEY, at least 32 bytes). The smoke gets a throwaway one in the
# project's wrangler.json copy, outside the checkout.
CONFIG = ROOT / "wrangler.json"
config = json.loads(CONFIG.read_text())
config.setdefault("vars", {})["NORN_LINEAGE_KEY"] = secrets.token_hex(32)
CONFIG.write_text(json.dumps(config))


def request(path, auth=None, method="GET"):
    req = urllib.request.Request(URL + path, method=method)
    if auth:
        req.add_header("authorization", auth)
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            status, body = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, body = error.code, error.read()
    return status, body, (time.perf_counter() - started) * 1000


def start():
    child = subprocess.Popen(COMMAND, cwd=ROOT, start_new_session=True)
    for _ in range(300):
        if child.poll() is not None:
            raise RuntimeError("runtime exited during startup")
        try:
            request("/v1/definitions")
            return child
        except OSError:
            time.sleep(0.2)
    stop(child)
    raise RuntimeError("runtime startup deadline exceeded")


def stop(child):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if child.poll() is None:
            os.killpg(child.pid, sig)
        try:
            child.wait(timeout=15)
            return
        except subprocess.TimeoutExpired:
            if sig == signal.SIGKILL:
                raise


def definitions():
    status, body, ms = request(f"/v1/definitions?{SCOPE}", VIEWER)
    assert status == 200, (status, body[:500])
    return json.loads(body), hashlib.sha256(body).hexdigest(), ms


child = start()
try:
    first, digest, cold = definitions()
    print(f"{RUNTIME} first read (provisions): {cold:.1f} ms, body sha256 {digest}")
    print("receiptSha256", first["receiptSha256"], "artifacts", len(first["artifacts"]))
    status, denied, _ = request(f"/v1/definitions?{SCOPE}")
    assert status == 403 and json.loads(denied)["code"] == "authz.denied", denied
    for path in ["/v1/definitions?orgId=foreign&environmentId=dev", "/v1/definitions"]:
        assert request(path, VIEWER)[:2] == (403, denied), path
    # Tenant cells exist only in declared environments (NORN_TENANT_ENVIRONMENTS,
    # default dev,prod). For a verified member of org_demo: an undeclared
    # environment reaches no cell of its own (the demo cell's opaque denial); a
    # declared one reaches its own cell, which says it is not provisioned.
    assert request("/v1/definitions?orgId=org_demo&environmentId=staging", VIEWER)[:2] == (403, denied), "undeclared"
    status, body, _ = request("/v1/definitions?orgId=org_demo&environmentId=prod", VIEWER)
    assert status == 404 and json.loads(body)["code"] == "cell.not_provisioned", (status, body[:300])
    assert request(f"/v1/definitions?{SCOPE}&sql=1", VIEWER)[0] == 400
    assert request(f"/v1/definitions?{SCOPE}", VIEWER, "POST")[0] == 405
    status, body, cold_answer = request(f"{ANSWER}?{ANALYTICS}", VIEWER)
    assert status == 200, (status, body[:500])
    answer = json.loads(body)
    assert answer["freshness"] == "exact" and answer["complete"], answer
    print(f"{RUNTIME} first answer (provisions analytics): {cold_answer:.1f} ms, body sha256 {hashlib.sha256(body).hexdigest()}")
    assert request(f"{ANSWER}?{ANALYTICS}")[:2] == (403, denied), "anonymous answer"
    guest = json.loads(request(f"{ANSWER}?{ANALYTICS}", GUEST)[1])
    bucket = next(iter(guest["rows"]))
    trace_query = urllib.parse.urlencode({"lineageRef": guest["lineageRef"], "bucket": bucket})
    status, body, _ = request(f"{TRACE}?{ANALYTICS}&{trace_query}", GUEST)
    assert status == 200, (status, body[:500])
    assert all(c["restricted"] and c["values"] is None for c in json.loads(body)["contributions"]), "guest saw values"
    answers = [request(f"{ANSWER}?{ANALYTICS}", VIEWER)[2] for _ in range(20)]
    print(f"{RUNTIME} warm answer p50 {statistics.median(answers):.1f} ms, max {max(answers):.1f} ms")
    warm = [definitions()[2] for _ in range(20)]
    print(f"{RUNTIME} warm read p50 {statistics.median(warm):.1f} ms, max {max(warm):.1f} ms")
    stop(child)
    child = start()
    again, again_digest, restart = definitions()
    assert again_digest == digest, "document changed across restart"
    status, body, _ = request(f"{ANSWER}?{ANALYTICS}", VIEWER)
    assert status == 200 and json.loads(body)["rows"] == answer["rows"], "answer changed across restart"
    print(f"{RUNTIME} first read after restart: {restart:.1f} ms; PASS")
    # Live reads (UI-LIVE-1): the hibernating WebSocket on this runtime.
    checkout = Path(__file__).resolve().parents[2]
    subprocess.run(["bun", str(Path(__file__).with_name("live-check.mjs")), URL, str(checkout)], check=True)
finally:
    stop(child)
