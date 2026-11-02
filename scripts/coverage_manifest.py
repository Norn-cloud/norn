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
    identities = set()
    for entry in sorted(subsystems.iterdir()):
        identity = entry.stem if entry.is_file() else entry.name
        if entry.is_symlink():
            raise InvalidCoverageLayout(f"{entry.name}: not a regular file")
        if not SUBSYSTEM_FILE.fullmatch(f"{identity}.json"):
            raise InvalidCoverageLayout(f"unknown subsystem files: {entry.name}")
        if identity in identities:
            raise InvalidCoverageLayout(f"duplicate subsystem: {identity}")
        identities.add(identity)
        if entry.is_file():
            if entry.suffix != ".json":
                raise InvalidCoverageLayout(f"unknown subsystem files: {entry.name}")
            result.append(entry)
        elif entry.is_dir():
            result.extend(_entry_files(entry))
        else:
            raise InvalidCoverageLayout(f"not a regular subsystem: {entry.name}")
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
                identity = re.fullmatch(r"([0-9a-f]{64})(?:dup[1-9][0-9]*)?\.json", path.name)
                if not identity or (keyed and identity[1] != entry_id(item)) or (field == "obligations" and path.stem != identity[1]):
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
    for entry in sorted((directory / SUBSYSTEMS).iterdir(), key=lambda path: path.name if path.is_file() else path.name + ".json"):
        body = _subsystem(entry) if entry.is_dir() else _object(entry)
        if "id" in body:
            raise InvalidCoverageLayout(f"{entry.name}: file name is the subsystem id; drop the id field")
        identity = entry.name if entry.is_dir() else entry.stem
        subsystems.append({"id": identity, **body})
    document["subsystems"] = subsystems
    return document


def write(document: dict[str, Any], directory: Path, *, split_entries: bool = False) -> None:
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
        if not split_entries:
            (directory / SUBSYSTEMS / f"{identity}.json").write_text(json.dumps(body, indent=2) + "\n", encoding="utf-8")
            continue
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
                seen = {}
                for order, item in enumerate(value):
                    name = entry_id(item)
                    occurrence = seen.get(name, 0)
                    seen[name] = occurrence + 1
                    if occurrence:
                        name += f"dup{occurrence}"
                    (folder / f"{name}.json").write_text(json.dumps({"order": order, "value": split(item, [*parts, "entry" + name])}, indent=2) + "\n")
                return {"$entries": field}
            if isinstance(value, dict):
                return {key: split(item, [*parts, key]) for key, item in value.items()}
            return value
        (target / INDEX).write_text(json.dumps(split(body, []), indent=2) + "\n")


def write_entries(document: dict[str, Any], directory: Path) -> None:
    """Write independently addressed entries for migrations and fixtures."""
    write(document, directory, split_entries=True)


def digest(directory: Path = COVERAGE) -> str:
    """SHA-256 over each file's relative path and SHA-256, in path order."""
    total = hashlib.sha256()
    for path in files(directory):
        total.update(path.relative_to(directory).as_posix().encode("utf-8"))
        total.update(b"\0")
        total.update(hashlib.sha256(path.read_bytes()).hexdigest().encode("ascii"))
        total.update(b"\n")
    return total.hexdigest()
