#!/usr/bin/env python3
"""Run the compiled adapter on pinned celld, including a daemon restart."""
import json
import os
from pathlib import Path
import signal
import subprocess
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent
URL = "http://127.0.0.1:9876"


def request(path):
    try:
        with urllib.request.urlopen(URL + path, timeout=3) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def start():
    child = subprocess.Popen(["celld", "dev", "--port", "9876"], cwd=ROOT,
                             start_new_session=True)
    try:
        for _ in range(100):
            if child.poll() is not None:
                raise RuntimeError("celld exited during startup")
            try:
                request("/state")  # May be 500 before the first schema creation.
                return child
            except urllib.error.URLError:
                time.sleep(0.2)
        raise RuntimeError("celld startup deadline exceeded")
    except BaseException:
        stop(child)
        raise


def stop(child):
    if child.poll() is None:
        if Path(os.readlink(f"/proc/{child.pid}/cwd")) != ROOT:
            raise RuntimeError("daemon working directory changed")
        os.killpg(child.pid, signal.SIGTERM)
    child.wait(timeout=15)


def state():
    status, body = request("/state")
    assert status == 200, (status, body)
    return json.loads(body)


assert subprocess.check_output(["celld", "--version"], text=True).strip() == "celld 0.6.3"
child = start()
try:
    status, body = request("/")
    assert status == 200 and body.endswith(":pass"), (status, body)
    before = state()
    assert before["rows"] == [[1, "9007199254740993.0001", [0, 255]]], before
    assert request("/alarm-refusal")[0] == 422
    assert state() == before, "SQL or alarm escaped rollback"
    stop(child)
    child = start()
    assert state() == before, "semantic SQLite/alarm state changed on restart"
    assert request("/")[0] == 200, "adapter unusable after restart"
    print("PASS: adapter checks, alarm rollback, semantic restart equality, retry")
finally:
    stop(child)
