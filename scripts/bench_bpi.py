#!/usr/bin/env python3
"""Fetch licensed BPI 2019 at test time; profile it or flatten it to CSV outside the checkout, never redistributing rows."""
import argparse
import collections
import csv
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.request
import xml.etree.ElementTree as ET

PAGE = "https://data.4tu.nl/articles/_/12715853/1"
URL = "https://data.4tu.nl/file/35ed7122-966a-484e-a0e1-749b64e3366d/864493d1-3a58-47f6-ad6f-27f95f995828"
LICENSE = "https://creativecommons.org/licenses/by/4.0/"
SHA256 = "af63bc687fc4152f2123b05c3af7772b37ef3fce2d3f67f812666c9e356baae7"
CITATION = "van Dongen, Boudewijn (2019): BPI Challenge 2019. Version 1. 4TU.ResearchData. doi:10.4121/uuid:d06aff4b-79f0-45e6-8ec8-e19730c248f1"


class _HttpsOnlyRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not newurl.startswith("https://"):
            raise ValueError(f"{req.full_url}: refused non-HTTPS redirect to {newurl}")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def fetch(path):
    opener = urllib.request.build_opener(_HttpsOnlyRedirects())
    # Check the publisher's machine-readable licence before retrieving bytes.
    with opener.open(PAGE, timeout=60) as response:
        page = response.read(1024 * 1024).decode()
    if '"license": "' + LICENSE + '"' not in page:
        raise ValueError("publisher licence changed or cannot be verified")
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=path.parent, suffix=".partial", delete=False) as out:
            partial = Path(out.name)
            try:
                hasher = hashlib.sha256()
                with opener.open(URL, timeout=60) as response:
                    size = 0
                    while chunk := response.read(1024 * 1024):
                        size += len(chunk)
                        if size > 800_000_000:
                            raise ValueError("download exceeds pinned size budget")
                        hasher.update(chunk)
                        out.write(chunk)
                out.flush()
            except BaseException:
                partial.unlink(missing_ok=True)
                raise
        try:
            if hasher.hexdigest() != SHA256:
                raise ValueError("download checksum mismatch")
            os.replace(partial, path)
        except BaseException:
            partial.unlink(missing_ok=True)
            raise
    if digest(path) != SHA256:
        raise ValueError("cached data checksum mismatch")
    print(json.dumps(dict(citation=CITATION, licence=LICENSE, sha256=SHA256, bytes=path.stat().st_size)))


def profile(path):
    if digest(path) != SHA256:
        raise ValueError("profile requires the pinned original bytes")
    counts = collections.Counter()
    keys = collections.Counter()
    trace_keys = collections.Counter()
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
                trace_keys.update(attr.get("key", "") for attr in node)
            node.clear()
            if parent is not None:
                parent.remove(node)
        stack.pop()
    print(json.dumps(dict(counts=counts, event_keys=keys, trace_keys=trace_keys, activities=activities, sha256=SHA256), sort_keys=True))


def flatten(path, out):
    """Stream the pinned log into one CSV row per event, with its case's attributes."""
    if digest(path) != SHA256:
        raise ValueError("csv requires the pinned original bytes")
    if out.resolve().is_relative_to(Path(__file__).resolve().parents[1]):
        raise ValueError("raw data must remain outside the source checkout")
    event_keys = {"concept:name": "activity", "time:timestamp": "occurred_at", "org:resource": "resource",
                  "User": "user", "Cumulative net worth (EUR)": "net_worth"}
    trace_keys = {"concept:name": "case_id", "Item Type": "item_type", "Document Type": "document_type",
                  "Company": "company", "Vendor": "vendor", "Spend area text": "spend_area",
                  "Item Category": "item_category"}
    columns = ["event_id", *trace_keys.values(), *event_keys.values()]
    rows = 0
    case = {}
    stack = []
    with out.open("x", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=columns, lineterminator="\n")
        writer.writeheader()
        for event, node in ET.iterparse(path, events=("start", "end")):
            tag = node.tag.rsplit("}", 1)[-1]
            if event == "start":
                stack.append(node)
                if tag == "trace":
                    case = {}
                continue
            parent = stack[-2] if len(stack) > 1 else None
            parent_tag = parent.tag.rsplit("}", 1)[-1] if parent is not None else ""
            if tag != "event" and parent_tag == "trace" and node.get("key") in trace_keys:
                case[trace_keys[node.get("key")]] = node.get("value", "")
            if tag == "event":
                rows += 1
                row = {"event_id": f"e{rows}", **case}
                for attr in node:
                    key = attr.get("key")
                    if key in event_keys:
                        row[event_keys[key]] = attr.get("value", "")
                writer.writerow(row)
            if tag in ("event", "trace"):
                node.clear()
                if parent is not None:
                    parent.remove(node)
            stack.pop()
    print(json.dumps(dict(rows=rows, columns=columns, sha256=SHA256, out=str(out))))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("fetch", "profile", "csv"))
    parser.add_argument("path", type=Path, help="data file path in a task scratch directory; never a repo path")
    parser.add_argument("out", type=Path, nargs="?", help="csv: the flattened events, outside the checkout")
    args = parser.parse_args()
    if args.path.resolve().is_relative_to(Path(__file__).resolve().parents[1]):
        parser.error("raw data must remain outside the source checkout")
    if args.command == "csv":
        if args.out is None:
            parser.error("csv needs an output path")
        flatten(args.path, args.out)
    else:
        {"fetch": fetch, "profile": profile}[args.command](args.path)
