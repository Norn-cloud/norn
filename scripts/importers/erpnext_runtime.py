#!/usr/bin/env python3
"""Acquire checksum-pinned interoperability test schemas outside the source tree."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time
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
# do not fail every job. A cached file is used only if its digest matches the pin.
CACHE = os.path.expanduser(os.environ.get("NORN_INTEROP_CACHE", "")) or None


def cached(name, digest):
    path = Path(CACHE, name) if CACHE else None
    if path is not None and path.is_file():
        raw = path.read_bytes()
        if hashlib.sha256(raw).hexdigest() == digest:
            return raw
    return None


def store(name, raw):
    if CACHE:
        Path(CACHE).mkdir(parents=True, exist_ok=True)
        Path(CACHE, name).write_bytes(raw)


def fetch(url, accept, attempts=5):
    request = urllib.request.Request(url, headers={
        "User-Agent": "Norn-runtime-test-inputs/1.0", "Accept": accept,
    })
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except OSError:
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
