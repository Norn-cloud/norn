#!/usr/bin/env python3
"""Validate the standards atlas and render its coverage table.

The atlas lives in registry/standards/atlas/<domain>.json. This script checks
every file against registry/standards/atlas.schema.json (a stdlib interpreter
of the JSON Schema keywords that schema uses) and then the cross-file rules
the schema cannot express:

- the file name is its domain, and each entry's primary domain is that file;
- ids are unique across the atlas, and entries are sorted by id;
- every Norn concept names an artifact in registry/core or registry/packs;
- a planned mapping names its importer issue; a partial or done mapping names
  a concept or an IR target and cites repository paths that exist;
- code-reusable needs a verified, asserted licence; NOASSERTION is unverified;
- a machine-readable source has an artifact URL.

`--write` regenerates registry/standards/COVERAGE.md; `--check` fails when it
is stale. Standard library only, so it runs in the cheap CI phase.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ATLAS = Path("registry/standards/atlas")
SCHEMA = Path("registry/standards/atlas.schema.json")
COVERAGE = Path("registry/standards/COVERAGE.md")
ARTIFACT_GLOBS = ("registry/core/artifacts/*.json", "registry/packs/*/artifacts/*.json")
KIND_SUFFIXES = (".entity", ".interface")
PRIORITY_RANK = {"P0": 0, "P1": 1, "P2": 2, "watch": 3, "ignore": 4}
MAPPED = ("partial", "done")
TOP_N = 10

TYPES = {
    "object": dict,
    "array": list,
    "string": str,
    "null": type(None),
    "boolean": bool,
}


class AtlasError(Exception):
    """One or more atlas rules failed; the message lists every failure."""


def _resolve(schema: dict, root: dict) -> dict:
    while "$ref" in schema:
        ref = schema["$ref"]
        if not ref.startswith("#/"):
            raise AtlasError(f"schema: only local $ref is supported, got {ref}")
        node = root
        for part in ref[2:].split("/"):
            node = node[part]
        schema = node
    return schema


def _type_ok(value, name: str) -> bool:
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    expected = TYPES[name]
    if expected is not bool and isinstance(value, bool):
        return False
    return isinstance(value, expected)


def validate(value, schema: dict, root: dict, where: str, errors: list[str]) -> None:
    """Check `value` against the JSON Schema subset the atlas schema uses."""
    schema = _resolve(schema, root)
    if "anyOf" in schema:
        for option in schema["anyOf"]:
            trial: list[str] = []
            validate(value, option, root, where, trial)
            if not trial:
                break
        else:
            errors.append(f"{where}: matches no allowed form")
        return
    if "const" in schema and value != schema["const"]:
        errors.append(f"{where}: must be {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{where}: {value!r} is not one of {schema['enum']}")
    if "type" in schema:
        names = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(_type_ok(value, name) for name in names):
            errors.append(f"{where}: expected {' or '.join(names)}")
            return
    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            errors.append(f"{where}: must not be empty")
        if "pattern" in schema and not re.search(schema["pattern"], value):
            errors.append(f"{where}: {value!r} does not match {schema['pattern']}")
    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            errors.append(f"{where}: needs at least {schema['minItems']} item(s)")
        if schema.get("uniqueItems"):
            seen = [json.dumps(item, sort_keys=True) for item in value]
            if len(seen) != len(set(seen)):
                errors.append(f"{where}: items must be unique")
        if "items" in schema:
            for index, item in enumerate(value):
                validate(item, schema["items"], root, f"{where}[{index}]", errors)
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        for key in schema.get("required", []):
            if key not in value:
                errors.append(f"{where}: missing {key}")
        for key, item in value.items():
            if key in properties:
                validate(item, properties[key], root, f"{where}.{key}", errors)
            elif schema.get("additionalProperties") is False:
                errors.append(f"{where}: unknown field {key}")


def known_concepts(root: Path) -> set[str]:
    concepts = set()
    for pattern in ARTIFACT_GLOBS:
        for path in root.glob(pattern):
            artifact_id = json.loads(path.read_text(encoding="utf-8")).get("artifactId")
            if not isinstance(artifact_id, str):
                continue
            for suffix in KIND_SUFFIXES:
                if artifact_id.endswith(suffix):
                    artifact_id = artifact_id[: -len(suffix)]
            concepts.add(artifact_id)
    return concepts


def load(root: Path = ROOT) -> dict[str, list[dict]]:
    """Return {domain: entries}, raising AtlasError listing every violation."""
    schema = json.loads((root / SCHEMA).read_text(encoding="utf-8"))
    concepts = known_concepts(root)
    errors: list[str] = []
    atlas: dict[str, list[dict]] = {}
    owners: dict[str, str] = {}
    files = sorted((root / ATLAS).glob("*.json"))
    if not files:
        raise AtlasError(f"{ATLAS}: no atlas files")
    for path in files:
        rel = path.relative_to(root).as_posix()
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            errors.append(f"{rel}: invalid JSON: {error}")
            continue
        before = len(errors)
        validate(document, schema, schema, rel, errors)
        if len(errors) != before:
            continue
        domain = document["domain"]
        if path.stem != domain:
            errors.append(f"{rel}: file must be named {domain}.json")
        entries = document["entries"]
        ids = [entry["id"] for entry in entries]
        if ids != sorted(ids):
            errors.append(f"{rel}: entries must be sorted by id")
        for entry in entries:
            where = f"{rel}:{entry['id']}"
            if entry["id"] in owners:
                errors.append(f"{where}: id also used in {owners[entry['id']]}")
            owners[entry["id"]] = rel
            if entry["domains"][0] != domain:
                errors.append(f"{where}: primary domain {entry['domains'][0]} belongs in {entry['domains'][0]}.json")
            errors.extend(entry_rules(entry, where, concepts, root))
        atlas[domain] = entries
    if errors:
        raise AtlasError("\n".join(errors))
    return atlas


def entry_rules(entry: dict, where: str, concepts: set[str], root: Path) -> list[str]:
    errors = []
    norn = entry["norn"]
    for concept in norn["concepts"]:
        if concept not in concepts:
            errors.append(f"{where}: concept {concept} is not a registry artifact; use proposedConcepts")
    mapping = norn["mapping"]
    if mapping == "planned" and norn["importerIssue"] is None:
        errors.append(f"{where}: a planned mapping names its importerIssue")
    if mapping in MAPPED:
        if not norn["concepts"] and not norn["irTargets"]:
            errors.append(f"{where}: a {mapping} mapping names a concept or an IR target")
        if not norn["mappingEvidence"]:
            errors.append(f"{where}: a {mapping} mapping cites mappingEvidence")
    elif norn["mappingEvidence"]:
        errors.append(f"{where}: mappingEvidence is only for partial or done mappings")
    for evidence in norn["mappingEvidence"]:
        if ".." in Path(evidence).parts or not (root / evidence).exists():
            errors.append(f"{where}: mappingEvidence {evidence} does not exist")
    licence = entry["licence"]
    if "NOASSERTION" in licence["spdx"] and licence["status"] == "verified":
        errors.append(f"{where}: NOASSERTION cannot be a verified licence")
    if entry["reuse"] == "code-reusable" and (
        licence["status"] != "verified" or "NOASSERTION" in licence["spdx"]
    ):
        errors.append(f"{where}: code-reusable needs a verified, asserted licence")
    if licence["status"] == "verified" and not (licence["termsUrl"] or entry["evidenceUrls"]):
        errors.append(f"{where}: a verified licence cites termsUrl or evidenceUrls")
    if entry["machineReadable"] == "yes" and entry["artifactUrl"] is None:
        errors.append(f"{where}: a machine-readable source names its artifactUrl")
    return errors


def _rank(entry: dict) -> tuple:
    priority = entry["priority"]
    customer, sap = PRIORITY_RANK[priority["customer1"]], PRIORITY_RANK[priority["sapParity"]]
    planned = 0 if entry["norn"]["mapping"] == "planned" else 1
    return (min(customer, sap), customer + sap, planned, entry["id"])


def next_mappings(atlas: dict[str, list[dict]], limit: int = TOP_N) -> list[dict]:
    """Unmapped entries, best first: priority, then planned work, then id."""
    pending = [
        entry
        for entries in atlas.values()
        for entry in entries
        if entry["norn"]["mapping"] in ("none", "planned")
        and entry["machineReadable"] != "no"
        and entry["reuse"] != "reference-only"
    ]
    return sorted(pending, key=_rank)[:limit]


def _percent(part: int, whole: int) -> str:
    return f"{round(100 * part / whole)}%" if whole else "0%"


def render(atlas: dict[str, list[dict]]) -> str:
    entries = [entry for domain in sorted(atlas) for entry in atlas[domain]]
    total = len(entries)
    lines = [
        "# Standards atlas coverage",
        "",
        "Generated by `python3 scripts/standards_atlas.py --write`; do not edit by hand.",
        "`--check` fails CI when this file is stale.",
        "",
        "Mapped means a `partial` or `done` mapping with repository evidence. Planned means",
        "an open importer or design issue owns the mapping.",
        "",
        f"Total: **{total}** entries in **{len(atlas)}** domains.",
        "",
        "| Domain | Entries | Mapped | Mapped % | Planned | Licence verified | Code-reusable |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    sums = Counter()
    for domain in sorted(atlas):
        rows = atlas[domain]
        mapped = sum(entry["norn"]["mapping"] in MAPPED for entry in rows)
        planned = sum(entry["norn"]["mapping"] == "planned" for entry in rows)
        verified = sum(entry["licence"]["status"] == "verified" for entry in rows)
        reusable = sum(entry["reuse"] == "code-reusable" for entry in rows)
        sums.update(entries=len(rows), mapped=mapped, planned=planned, verified=verified, reusable=reusable)
        lines.append(
            f"| {domain} | {len(rows)} | {mapped} | {_percent(mapped, len(rows))} | {planned} | {verified} | {reusable} |"
        )
    lines.append(
        f"| **all** | **{sums['entries']}** | **{sums['mapped']}** | **{_percent(sums['mapped'], sums['entries'])}** "
        f"| **{sums['planned']}** | **{sums['verified']}** | **{sums['reusable']}** |"
    )
    by_status = Counter(entry["norn"]["mapping"] for entry in entries)
    by_reuse = Counter(entry["reuse"] for entry in entries)
    lines += [
        "",
        "| Mapping | Entries |",
        "| --- | ---: |",
        *(f"| {status} | {by_status[status]} |" for status in ("done", "partial", "planned", "none")),
        "",
        "| Reuse | Entries |",
        "| --- | ---: |",
        *(f"| {reuse} | {by_reuse[reuse]} |" for reuse in ("code-reusable", "vocabulary-only", "reference-only")),
        "",
        "## Next mappings",
        "",
        "Unmapped, machine-readable and reusable entries, ranked by the better of the",
        "customer #1 and SAP-parity priorities, then by combined priority, then planned first.",
        "",
        "| # | Entry | Domain | Customer #1 | SAP parity | Mapping | Issue | Norn targets |",
        "| ---: | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for number, entry in enumerate(next_mappings(atlas), start=1):
        norn = entry["norn"]
        targets = ", ".join(
            norn["concepts"] + norn["irTargets"] + [f"*{name}*" for name in norn["proposedConcepts"]]
        ) or "—"
        lines.append(
            f"| {number} | `{entry['id']}` | {entry['domains'][0]} | {entry['priority']['customer1']} "
            f"| {entry['priority']['sapParity']} | {norn['mapping']} | {norn['importerIssue'] or '—'} | {targets} |"
        )
    concept_use = defaultdict(int)
    for entry in entries:
        for concept in entry["norn"]["concepts"]:
            concept_use[concept] += 1
    proposed = Counter(name for entry in entries for name in entry["norn"]["proposedConcepts"])
    lines += [
        "",
        "## Norn concepts by atlas demand",
        "",
        "| Registry concept | Entries |",
        "| --- | ---: |",
        *(f"| `{name}` | {count} |" for name, count in sorted(concept_use.items(), key=lambda item: (-item[1], item[0]))),
        "",
        "| Proposed concept (not yet in the registry) | Entries |",
        "| --- | ---: |",
        *(f"| *{name}* | {count} |" for name, count in sorted(proposed.items(), key=lambda item: (-item[1], item[0]))),
        "",
    ]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="validate and fail if COVERAGE.md is stale")
    mode.add_argument("--write", action="store_true", help="validate and regenerate COVERAGE.md")
    args = parser.parse_args(argv)
    try:
        atlas = load(args.root)
    except AtlasError as error:
        print(error, file=sys.stderr)
        return 1
    rendered = render(atlas)
    target = args.root / COVERAGE
    if args.write:
        target.write_text(rendered, encoding="utf-8")
    elif args.check:
        if not target.exists() or target.read_text(encoding="utf-8") != rendered:
            print(f"{COVERAGE} is stale; run python3 scripts/standards_atlas.py --write", file=sys.stderr)
            return 1
    count = sum(len(entries) for entries in atlas.values())
    print(f"standards atlas: {count} entries in {len(atlas)} domains")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
