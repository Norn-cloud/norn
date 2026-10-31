#!/usr/bin/env python3
"""The coverage manifest: `index.json` plus one `subsystems/<id>.json` per subsystem.

A subsystem's identity is its file name, so its file has no `id` field and two
files can never claim the same subsystem. Pull requests that touch different
subsystems edit different files and different provenance records.
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
COVERAGE = ROOT / "verification/coverage-v1/manifest"
INDEX = "index.json"
SUBSYSTEMS = "subsystems"
SUBSYSTEM_FILE = re.compile(r"^([a-z0-9][a-z0-9-]*)\.json$")


class InvalidCoverageLayout(ValueError):
    pass


def _object(path: Path) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise InvalidCoverageLayout(f"{path.name}: not a regular file")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InvalidCoverageLayout(f"{path.name}: {error}") from error
    if not isinstance(value, dict):
        raise InvalidCoverageLayout(f"{path.name}: root must be an object")
    return value


def files(directory: Path = COVERAGE) -> list[Path]:
    """Every file of the manifest; anything else in the directory is refused."""
    if directory.is_symlink() or not directory.is_dir():
        raise InvalidCoverageLayout(f"{directory.name}: not a coverage directory")
    stray = sorted(entry.name for entry in directory.iterdir() if entry.name not in {INDEX, SUBSYSTEMS})
    if stray:
        raise InvalidCoverageLayout(f"unknown coverage entries: {stray}")
    subsystems = directory / SUBSYSTEMS
    if subsystems.is_symlink() or not subsystems.is_dir():
        raise InvalidCoverageLayout(f"{SUBSYSTEMS}: not a directory")
    entries = sorted(subsystems.iterdir())
    unknown = [entry.name for entry in entries if not SUBSYSTEM_FILE.match(entry.name)]
    if unknown:
        raise InvalidCoverageLayout(f"unknown subsystem files: {unknown}")
    return [directory / INDEX, *entries]


def load(directory: Path = COVERAGE) -> dict[str, Any]:
    """Assemble the manifest, its subsystems in id order."""
    index, *entries = files(directory)
    document = _object(index)
    if "subsystems" in document:
        raise InvalidCoverageLayout(f"{INDEX} must not list subsystems; each has its own file")
    subsystems = []
    for entry in entries:
        body = _object(entry)
        if "id" in body:
            raise InvalidCoverageLayout(f"{entry.name}: the file name is the subsystem id; drop the id field")
        subsystems.append({"id": SUBSYSTEM_FILE.match(entry.name)[1], **body})
    document["subsystems"] = subsystems
    return document


def write(document: dict[str, Any], directory: Path) -> None:
    """Replace `directory` with `document` in the split layout (codemod and canaries)."""
    index = {key: value for key, value in document.items() if key != "subsystems"}
    bodies: dict[str, dict[str, Any]] = {}
    for subsystem in document.get("subsystems", []):
        body = dict(subsystem)
        identity = body.pop("id")
        if not isinstance(identity, str) or not SUBSYSTEM_FILE.match(f"{identity}.json") or identity in bodies:
            raise InvalidCoverageLayout(f"invalid or duplicate subsystem id {identity!r}")
        bodies[identity] = body
    shutil.rmtree(directory, ignore_errors=True)
    (directory / SUBSYSTEMS).mkdir(parents=True)
    (directory / INDEX).write_text(json.dumps(index, indent=2) + "\n", encoding="utf-8")
    for identity, body in bodies.items():
        (directory / SUBSYSTEMS / f"{identity}.json").write_text(json.dumps(body, indent=2) + "\n", encoding="utf-8")


def digest(directory: Path = COVERAGE) -> str:
    """SHA-256 over each file's relative path and SHA-256, in path order."""
    total = hashlib.sha256()
    for path in files(directory):
        total.update(path.relative_to(directory).as_posix().encode("utf-8"))
        total.update(b"\0")
        total.update(hashlib.sha256(path.read_bytes()).hexdigest().encode("ascii"))
        total.update(b"\n")
    return total.hexdigest()
