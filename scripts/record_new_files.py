#!/usr/bin/env python3
"""Bring coverage enrollment and provenance records in line with the files on disk.

Run from any checkout after adding, editing, moving or deleting files:

    python3 scripts/record_new_files.py          # write the records
    python3 scripts/record_new_files.py --check  # list what is missing; exit 1 if anything is

What it does, and nothing more:

* Enrollment: every executable source that `proof_enrollment.discover_sources` finds (the
  gate's own discovery) and no subsystem lists is enrolled in the subsystem that owns most of
  its nearest enrolled siblings (same directory, then each parent in turn). Listed sources that
  no longer exist are removed. Writes go through `coverage_manifest.apply_entries`, which
  validates the staged manifest and rolls back on failure.
* Provenance: a file without a record gets one copied from the nearest directory with records,
  only when that directory is all `authored` and has siblings of the file's own suffix that agree
  on category and licence; its `sha256` is the new bytes. In a mixed-origin directory (any derived
  or vendored record), a new file is your own work only if you say so: `--authored <path>`.
  An `authored` record whose file changed gets its `sha256` refreshed; every other field is kept.
  Records of deleted files are removed.

It refuses, and changes nothing, when a choice belongs to a person: a new file with no enrolled
sibling or no same-suffix authored sibling, a new file in a mixed-origin directory that was not
declared, siblings that disagree on category or licence, a source enrolled twice, or a changed
file whose record is not `authored` (vendored, derived or third-party bytes need their metadata
reviewed by hand). Running it twice changes
nothing the second time.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import sys
from pathlib import Path, PurePosixPath

import coverage_manifest
from proof_enrollment import discover_sources, repository_files

PROVENANCE = Path(".github/ci/provenance/artifacts")
# Nothing under the provenance directory has a record of its own (AGENTS.md, Provenance).
UNRECORDED = ".github/ci/provenance/"
FIELD = "sourceFiles"


class Refused(Exception):
    """A record that needs a person's decision; nothing was written."""


def _ancestors(path: str):
    parent = PurePosixPath(path).parent
    while True:
        yield parent.as_posix()
        if parent == PurePosixPath("."):
            return
        parent = parent.parent


def _enrolled(root: Path) -> dict[str, tuple[str, str, int]]:
    """Each enrolled source: (subsystem, entry file relative to root, order)."""
    base = root / coverage_manifest.COVERAGE.relative_to(coverage_manifest.ROOT) / coverage_manifest.SUBSYSTEMS
    found = {}
    for entry in sorted(base.glob(f"*/{FIELD}/*.json")):
        body = json.loads(entry.read_text(encoding="utf-8"))
        place = (entry.parent.parent.name, entry.relative_to(root).as_posix(), body["order"])
        if body["value"] in found:
            # The enrollment gate refuses a source with two owners; a later entry must not hide the first.
            raise Refused(f"{body['value']}: enrolled twice ({found[body['value']][1]} and {place[1]}); remove one by hand")
        found[body["value"]] = place
    return found


def enrollment_updates(root: Path) -> dict[str, bytes | None]:
    enrolled = _enrolled(root)
    actual = discover_sources(root)
    updates: dict[str, bytes | None] = {}
    by_directory: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for source, (subsystem, _, _) in enrolled.items():
        if source in actual:
            by_directory[PurePosixPath(source).parent.as_posix()][subsystem] += 1
    next_order = collections.Counter()
    for subsystem, _, order in enrolled.values():
        next_order[subsystem] = max(next_order[subsystem], order + 1)
    for source in sorted(set(enrolled) - actual):
        updates[enrolled[source][1]] = None
    relative = coverage_manifest.COVERAGE.relative_to(coverage_manifest.ROOT).as_posix()
    for source in sorted(actual - set(enrolled)):
        owners = next((by_directory[d] for d in _ancestors(source) if by_directory[d]), None)
        if owners is None:
            raise Refused(f"{source}: no enrolled sibling to inherit a subsystem from; enroll it by hand")
        # Most siblings wins; ties go to the subsystem id that sorts first, so reruns agree.
        subsystem = min(owners, key=lambda name: (-owners[name], name))
        path = f"{relative}/{coverage_manifest.SUBSYSTEMS}/{subsystem}/{FIELD}/{coverage_manifest.entry_id(source)}.json"
        updates[path] = coverage_manifest.entry_bytes({"order": next_order[subsystem], "value": source})
        next_order[subsystem] += 1
    return updates


def _record_bytes(record: dict) -> bytes:
    return (json.dumps(record, indent=2) + "\n").encode("utf-8")


def _inherit(path: str, recorded: set[str], record_of, declared: bool) -> tuple[dict | None, str]:
    """The record a new file takes from the nearest directory that has records, or why it takes none.

    Inheritance needs evidence and no doubt about origin:
    - at least one sibling with the new file's suffix (no fallback to other kinds of file);
    - every sibling in the directory `authored`. One derived or vendored record, of any suffix,
      makes the directory mixed, and a new file there could be upstream code, so only its author
      can say otherwise: `--authored <path>` declares it (the record is then taken from the
      authored same-suffix siblings alone);
    - the deciding siblings agree on category and licence.
    """
    for directory in _ancestors(path):
        siblings = sorted(p for p in recorded if PurePosixPath(p).parent.as_posix() == directory)
        if not siblings:
            continue
        suffix = PurePosixPath(path).suffix
        found = {p: record_of(p) for p in siblings}
        same = [p for p in siblings if PurePosixPath(p).suffix == suffix]
        if not same:
            return None, f"no sibling in {directory} has its suffix ({suffix or 'none'}) to take a record from"
        origins = {str(r.get("origin")) for r in found.values()}
        if origins != {"authored"} and not declared:
            return None, f"{directory} mixes origins ({', '.join(sorted(origins))}); if you wrote it, pass --authored {path}"
        deciding = [found[p] for p in same if found[p].get("origin") == "authored"]
        if not deciding:
            return None, f"no authored sibling in {directory} has its suffix ({suffix or 'none'})"
        if len({(r.get("category"), r.get("license")) for r in deciding}) != 1:
            return None, f"its {suffix or 'suffixless'} siblings in {directory} declare different categories or licences"
        return deciding[0], ""
    return None, "no sibling record to inherit from"


def provenance_updates(root: Path, files: set[str], read, authored: frozenset[str] = frozenset()) -> dict[str, bytes | None]:
    """`files` is every repository path after enrollment, records excluded; `read(path)` returns its bytes then."""
    store = root / PROVENANCE
    records = {p.relative_to(store).as_posix()[: -len(".json")] for p in store.rglob("*.json")} if store.exists() else set()
    record_of = lambda path: json.loads((store / f"{path}.json").read_text(encoding="utf-8"))
    updates: dict[str, bytes | None] = {}
    refused = []
    for path in sorted(records - files):
        updates[(PROVENANCE / f"{path}.json").as_posix()] = None
    for path in sorted(files):
        digest = hashlib.sha256(read(path)).hexdigest()
        if path in records:
            record = record_of(path)
            if record.get("sha256") == digest:
                continue
            if record.get("origin") != "authored":
                refused.append(f"{path}: changed, but its record is {record.get('origin')!r}; review its metadata by hand")
                continue
        else:
            record, why = _inherit(path, records & files, record_of, path in authored)
            if record is None:
                refused.append(f"{path}: {why}; or write its record by hand")
                continue
        updates[(PROVENANCE / f"{path}.json").as_posix()] = _record_bytes({"sha256": digest, **{k: v for k, v in record.items() if k != "sha256"}})
    if refused:
        raise Refused("\n".join(refused))
    return updates


def plan(root: Path, authored: frozenset[str] = frozenset()) -> dict[str, bytes | None]:
    """Every write needed, computed against the tree as it will be after enrollment."""
    enrollment = enrollment_updates(root)
    files = {p.relative_to(root).as_posix() for p in repository_files(root, ".") if p.is_file()}
    files = {p for p in files if not p.startswith(UNRECORDED)}
    files = (files | {p for p, body in enrollment.items() if body is not None}) - {p for p, body in enrollment.items() if body is None}
    read = lambda path: enrollment[path] if enrollment.get(path) is not None else (root / path).read_bytes()
    return {**enrollment, **provenance_updates(root, files, read, authored)}


def apply(root: Path, updates: dict[str, bytes | None]) -> None:
    """Write every update through `apply_entries`: it validates the staged coverage and rolls back all of them together."""
    if updates:
        coverage_manifest.apply_entries(root, updates)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=coverage_manifest.ROOT)
    parser.add_argument("--check", action="store_true", help="list missing or stale records and exit 1 if any; write nothing")
    parser.add_argument("--authored", action="append", default=[], metavar="PATH",
                        help="declare a new file in a mixed-origin directory as your own work (repeatable)")
    args = parser.parse_args(argv)
    authored = frozenset(PurePosixPath(p).as_posix() for p in args.authored)
    try:
        for path in sorted(authored):
            # A declaration is for a new file only: never a way to relabel an existing record.
            if not (args.root / path).is_file():
                raise Refused(f"--authored {path}: no such file")
            if (args.root / PROVENANCE / f"{path}.json").exists():
                raise Refused(f"--authored {path}: it already has a record; edit that by hand")
        updates = plan(args.root, authored)
    except Refused as refusal:
        print(refusal, file=sys.stderr)
        return 2
    for path, content in sorted(updates.items()):
        print(f"{'remove' if content is None else 'write '} {path}")
    if args.check:
        return 1 if updates else 0
    apply(args.root, updates)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
