#!/usr/bin/env python3
"""Coverage metadata plus independently addressed array entries.

Each subsystem has index.json metadata and <field>/<identity-hash>.json entries.
The entry envelope carries its original order and value. Concurrent additions
share neither source files nor provenance records; ties use identity order.
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
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise InvalidCoverageLayout(f"{path}: duplicate JSON key {key}")
            result[key] = value
        return result
    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique)
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
    result = [directory / INDEX]
    for entry in sorted(subsystems.iterdir()):
        identity = entry.name
        if entry.is_symlink() or not entry.is_dir():
            raise InvalidCoverageLayout(f"unknown subsystem files: {entry.name}; not a regular directory")
        if not SUBSYSTEM_FILE.fullmatch(f"{identity}.json"):
            raise InvalidCoverageLayout(f"unknown subsystem files: {entry.name}")
        result.extend(_entry_files(entry))
    return result


def _assemble(directory: Path) -> tuple[dict[str, Any], list[Path]]:
    fields, result = set(), [directory / INDEX]
    def restore(value, parts):
        if isinstance(value, dict):
            if "$entries" not in value:
                return {key: restore(item, [*parts, key]) for key, item in value.items()}
            field = ".".join(parts)
            if value != {"$entries": field} or not re.fullmatch(r"[A-Za-z][A-Za-z0-9]*(?:\.[A-Za-z][A-Za-z0-9]*)*", field) or field in fields:
                raise InvalidCoverageLayout(f"invalid or duplicate entry field {field}")
            fields.add(field)
            folder = directory / field
            if folder.is_symlink() or (folder.exists() and not folder.is_dir()):
                raise InvalidCoverageLayout(f"{field}: not an entry directory")
            entries = []
            for path in sorted(folder.iterdir()) if folder.exists() else []:
                entry = _object(path)
                if set(entry) != {"order", "value"} or type(entry["order"]) is not int or entry["order"] < 0:
                    raise InvalidCoverageLayout(f"{path}: invalid entry envelope")
                item = restore(entry["value"], [*parts, "entry" + path.stem])
                keyed = not isinstance(item, dict) or "id" in item
                if not re.fullmatch(r"[0-9a-f]{64}\.json", path.name) or (keyed and path.name != entry_id(item) + ".json"):
                    raise InvalidCoverageLayout(f"{path}: mismatched entry identity")
                result.append(path)
                entries.append((entry["order"], path.name, item))
            return [item for _, _, item in sorted(entries)]
        if isinstance(value, list):
            raise InvalidCoverageLayout(f"{directory}: shared arrays must be entry files")
        return value
    body = restore(_object(directory / INDEX), [])
    if not {p.name for p in directory.iterdir()}.issubset({INDEX, *fields}):
        raise InvalidCoverageLayout(f"{directory}: unknown entry fields")
    return body, sorted(result)


def _entry_files(directory: Path) -> list[Path]:
    return _assemble(directory)[1]


def entry_id(value: Any) -> str:
    """Assign a declared/scalar identity, or an opaque initial-value ID.

    Unkeyed object IDs stay fixed when their nested entry directories change.
    """
    if isinstance(value, str):
        return hashlib.sha256(b"entry:string\0" + value.encode("utf-8")).hexdigest()
    identity = {"id": value["id"]} if isinstance(value, dict) and "id" in value else value
    return hashlib.sha256(json.dumps(identity, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()


def _subsystem(directory: Path) -> dict[str, Any]:
    return _assemble(directory)[0]


def load(directory: Path = COVERAGE) -> dict[str, Any]:
    """Assemble the manifest, its subsystems in id order."""
    files(directory)
    document = _object(directory / INDEX)
    if "subsystems" in document:
        raise InvalidCoverageLayout(f"{INDEX} must not list subsystems; each has its own file")
    subsystems = []
    for entry in sorted((directory / SUBSYSTEMS).iterdir(), key=lambda path: path.name + ".json"):
        body = _subsystem(entry)
        if "id" in body:
            raise InvalidCoverageLayout(f"{entry.name}: file name is the subsystem id; drop the id field")
        identity = entry.name
        subsystems.append({"id": identity, **body})
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
        target = directory / SUBSYSTEMS / identity
        target.mkdir()
        fields = set()
        def split(value, parts):
            if isinstance(value, list):
                field = ".".join(parts)
                if not re.fullmatch(r"[A-Za-z][A-Za-z0-9]*(?:\.[A-Za-z][A-Za-z0-9]*)*", field) or field in fields:
                    raise InvalidCoverageLayout(f"invalid or duplicate entry field {field}")
                fields.add(field)
                folder = target / field
                folder.mkdir()
                seen = set()
                for order, item in enumerate(value):
                    name = entry_id(item)
                    if name in seen:
                        raise InvalidCoverageLayout(f"{field}: duplicate entry identity")
                    seen.add(name)
                    (folder / f"{name}.json").write_text(json.dumps({"order": order, "value": split(item, [*parts, "entry" + name])}, indent=2) + "\n")
                return {"$entries": field}
            if isinstance(value, dict):
                return {key: split(item, [*parts, key]) for key, item in value.items()}
            return value
        (target / INDEX).write_text(json.dumps(split(body, []), indent=2) + "\n")


def digest(directory: Path = COVERAGE) -> str:
    """SHA-256 over each file's relative path and SHA-256, in path order."""
    total = hashlib.sha256()
    for path in files(directory):
        total.update(path.relative_to(directory).as_posix().encode("utf-8"))
        total.update(b"\0")
        total.update(hashlib.sha256(path.read_bytes()).hexdigest().encode("ascii"))
        total.update(b"\n")
    return total.hexdigest()


def migrate_entries() -> None:
    """One-shot conversion of the previous layout and its exact provenance."""
    records = ROOT / ".github/ci/provenance/artifacts"
    old = sorted((COVERAGE / SUBSYSTEMS).glob("*.json"))
    if not old:
        raise InvalidCoverageLayout("no legacy subsystem files to migrate")
    document = _object(COVERAGE / INDEX)
    bodies = [(path, _object(path)) for path in old]
    if any("id" in body or not SUBSYSTEM_FILE.fullmatch(path.name) for path, body in bodies):
        raise InvalidCoverageLayout("invalid legacy subsystem identity")
    document["subsystems"] = [{"id": path.stem, **body} for path, body in bodies]
    templates = {path.stem: _object(records / (path.relative_to(ROOT).as_posix() + ".json")) for path in old}
    write(document, COVERAGE)
    for path in old:
        (records / (path.relative_to(ROOT).as_posix() + ".json")).unlink()
    for path in files():
        relative = path.relative_to(ROOT).as_posix()
        target = records / (relative + ".json")
        parts = path.relative_to(COVERAGE).parts
        value = dict(templates[parts[1]]) if parts[0] == SUBSYSTEMS else _object(target)
        value["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(value, indent=2) + "\n")


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--migrate-entries", action="store_true", required=True)
    parser.parse_args()
    migrate_entries()
