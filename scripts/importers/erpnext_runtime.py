#!/usr/bin/env python3
"""Acquire checksum-pinned interoperability test schemas outside the source tree."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import time
import tempfile
import urllib.request
ROOT = Path(__file__).resolve().parents[2]

def acquire_interop(output):
    lock = json.loads((ROOT / "scripts/importers/erpnext-runtime.lock.json").read_text())
    for name, pin in lock["interop"].items():
        accept = "text/turtle" if name.endswith(".ttl") else "application/json, application/rdf+xml;q=0.9, */*;q=0.1"
        raw = cached(name, pin["sha256"])
        if raw is None:
            raw = fetch(pin["url"], accept)
            if hashlib.sha256(raw).hexdigest() != pin["sha256"]:
                raise ValueError(f"pinned interop digest mismatch: {name}")
            store(name, raw)
        (output / name).write_bytes(raw)


# Optional digest-checked cache: CI restores it so upstream DNS or host outages
# do not fail every job. A cached file is used only if its digest matches the pin,
# and cache I/O is best-effort: an unreadable or read-only cache is a miss.
def cache_dir():
    raw = os.environ.get("NORN_INTEROP_CACHE", "")
    if not raw:
        return None
    path = Path(os.path.expanduser(raw)).resolve()
    if path == ROOT or ROOT in path.parents:
        raise ValueError("NORN_INTEROP_CACHE must remain outside the source tree")
    return path


def cached(name, digest):
    directory = cache_dir()
    if directory is None:
        return None
    try:
        raw = (directory / name).read_bytes()
    except OSError:
        return None
    return raw if hashlib.sha256(raw).hexdigest() == digest else None


def store(name, raw):
    directory = cache_dir()
    if directory is None:
        return
    try:
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory, prefix=".norn-interop-") as temporary:
            pending = Path(temporary) / "schema"
            pending.write_bytes(raw)
            pending.replace(directory / name)
    except OSError:
        pass


def fetch(url, accept, attempts=5):
    request = urllib.request.Request(url, headers={
        "User-Agent": "Norn-runtime-test-inputs/1.0", "Accept": accept,
    })
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except (OSError, http.client.HTTPException):
            if attempt == attempts - 1:
                raise
            time.sleep(2 ** (attempt + 1))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--interop-only", action="store_true", required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if output == ROOT or ROOT in output.parents:
        raise ValueError("upstream test inputs must remain outside the source tree")
    output.mkdir(parents=True, exist_ok=True)
    acquire_interop(output)

if __name__ == "__main__":
    main()
