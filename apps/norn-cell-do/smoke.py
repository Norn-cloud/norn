#!/usr/bin/env python3
"""Serve the built cell on a pinned runtime; read, refuse, restart, re-read.

NORN_CELL_DO_PROJECT holds worker.mjs, wrangler.json and pkg/ (outside the
checkout). Prints the definition document digest and request latencies.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = Path(os.environ["NORN_CELL_DO_PROJECT"]).resolve()
URL = "http://127.0.0.1:9877"
RUNTIME = sys.argv[1] if len(sys.argv) > 1 else "celld"
COMMAND = {"celld": ["celld", "dev", "--port", "9877"],
           "wrangler": ["wrangler", "dev", "--local", "--port", "9877"]}[RUNTIME]
SCOPE = "orgId=org_demo&environmentId=dev"
VIEWER = "Bearer fixture:viewer@demo"


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
    assert request(f"/v1/definitions?{SCOPE}&sql=1", VIEWER)[0] == 400
    assert request(f"/v1/definitions?{SCOPE}", VIEWER, "POST")[0] == 405
    warm = [definitions()[2] for _ in range(20)]
    print(f"{RUNTIME} warm read p50 {statistics.median(warm):.1f} ms, max {max(warm):.1f} ms")
    stop(child)
    child = start()
    again, again_digest, restart = definitions()
    assert again_digest == digest, "document changed across restart"
    print(f"{RUNTIME} first read after restart: {restart:.1f} ms; PASS")
finally:
    stop(child)
