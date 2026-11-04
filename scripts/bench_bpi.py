#!/usr/bin/env python3
"""Fetch licensed BPI 2019 at test time; stream schema/counts without redistributing rows."""
import argparse
import collections
import hashlib
import json
from pathlib import Path
import urllib.request
import xml.etree.ElementTree as ET

PAGE = "https://data.4tu.nl/articles/_/12715853/1"
URL = "https://data.4tu.nl/file/35ed7122-966a-484e-a0e1-749b64e3366d/864493d1-3a58-47f6-ad6f-27f95f995828"
LICENSE = "https://creativecommons.org/licenses/by/4.0/"
SHA256 = "af63bc687fc4152f2123b05c3af7772b37ef3fce2d3f67f812666c9e356baae7"
CITATION = "van Dongen, Boudewijn (2019): BPI Challenge 2019. Version 1. 4TU.ResearchData. doi:10.4121/uuid:d06aff4b-79f0-45e6-8ec8-e19730c248f1"


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def fetch(path):
    # Check the publisher's machine-readable licence before retrieving bytes.
    with urllib.request.urlopen(PAGE, timeout=60) as response:
        page = response.read(1024 * 1024).decode()
    if '"license": "' + LICENSE + '"' not in page:
        raise ValueError("publisher licence changed or cannot be verified")
    if not path.exists():
        partial = path.with_suffix(".partial")
        try:
            with urllib.request.urlopen(URL, timeout=60) as response, partial.open("wb") as out:
                size = 0
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if size > 800_000_000:
                        raise ValueError("download exceeds pinned size budget")
                    out.write(chunk)
            if digest(partial) != SHA256:
                raise ValueError("download checksum mismatch")
            partial.replace(path)
        finally:
            partial.unlink(missing_ok=True)
    if digest(path) != SHA256:
        raise ValueError("cached data checksum mismatch")
    print(json.dumps(dict(citation=CITATION, licence=LICENSE, sha256=SHA256, bytes=path.stat().st_size)))


def profile(path):
    if digest(path) != SHA256:
        raise ValueError("profile requires the pinned original bytes")
    counts = collections.Counter()
    keys = collections.Counter()
    activities = collections.Counter()
    stack = []
    for event, node in ET.iterparse(path, events=("start", "end")):
        tag = node.tag.rsplit("}", 1)[-1]
        if event == "start":
            stack.append(node)
            continue
        parent = stack[-2] if len(stack) > 1 else None
        if tag == "event":
            counts["events"] += 1
            for attr in node:
                keys[attr.get("key", "")] += 1
                if attr.get("key") == "concept:name":
                    activities[attr.get("value", "")] += 1
        if tag in ("event", "trace"):
            if tag == "trace":
                counts["traces"] += 1
            node.clear()
            if parent is not None:
                parent.remove(node)
        stack.pop()
    print(json.dumps(dict(counts=counts, event_keys=keys, activities=activities, sha256=SHA256), sort_keys=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("fetch", "profile"))
    parser.add_argument("path", type=Path, help="existing task scratch directory; never a repo path")
    args = parser.parse_args()
    {"fetch": fetch, "profile": profile}[args.command](args.path)
