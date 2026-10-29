#!/usr/bin/env python3
"""Acquire checksum-pinned interoperability test schemas outside the source tree."""
import argparse
import hashlib
import json
from pathlib import Path
import urllib.request
ROOT = Path(__file__).resolve().parents[2]

def acquire_interop(output):
    lock = json.loads((ROOT / "scripts/importers/erpnext-runtime.lock.json").read_text())
    for name, pin in lock["interop"].items():
        accept = "text/turtle" if name.endswith(".ttl") else "application/json, application/rdf+xml;q=0.9, */*;q=0.1"
        request = urllib.request.Request(pin["url"], headers={
            "User-Agent": "Norn-runtime-test-inputs/1.0", "Accept": accept,
        })
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
        if hashlib.sha256(raw).hexdigest() != pin["sha256"]:
            raise ValueError(f"pinned interop digest mismatch: {name}")
        (output / name).write_bytes(raw)


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
