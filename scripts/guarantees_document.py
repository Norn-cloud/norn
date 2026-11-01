#!/usr/bin/env python3
"""Generate the public Guarantees payload; see verification/guarantees-v1/spec.md."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = "verification/guarantees-v1"
OUTPUT = f"{DIRECTORY}/document.json"
LAYERS = ("core primitives", "accounting/inventory", "jewelry", "workflow")


def read(root: Path, name: str) -> dict:
    return json.loads((root / name).read_text(encoding="utf-8"))


def unique(items: list[dict]) -> dict:
    result = {}
    for item in items:
        if item["id"] in result:
            raise ValueError(f"duplicate id: {item['id']}")
        result[item["id"]] = item
    return result


def facet(claim: dict, key: str, status: str, label: str) -> dict:
    recorded = claim.get(key, {})
    entries = recorded.get("evidence", [])
    valid = entries and all(
        item.get("artifact") and item.get("command") and item.get("selectors")
        for item in entries
    )
    return {"level": label if recorded.get("status") == status and valid else "unknown",
            "links": entries if recorded.get("status") == status and valid else []}


def public_text(value):
    if isinstance(value, list):
        return [public_text(item) for item in value]
    return re.sub(r"#\d+", "[historical reference]", value)


def project(coverage: dict, laws: list[dict]) -> dict:
    claims = unique([dict(claim, subsystem=system["id"],
                          limitations=system.get("limitations", []))
                     for system in coverage["subsystems"]
                     for claim in system["obligationClaims"]])
    rows = []
    for law in sorted(unique(laws).values(), key=lambda x: (LAYERS.index(x["lawLayer"]), x["id"])):
        claim = claims.get(law["id"], {})
        claim = {key: public_text(value) if key in {"residual", "limitations"} else value
                 for key, value in claim.items()}
        proof = claim.get("proof")
        model = facet({"modelProof": {"status": "proved" if proof in
                      {"reference_proved", "implementation_proved"} else "unknown",
                      "evidence": claim.get("evidence", {}).get("proof", [])}},
                      "modelProof", "proved", "proved on the model")
        rows.append({**law, "evidence": {
            "model": model,
            "execution": facet(claim, "execution", "enforced", "enforced in execution"),
            "parity": facet(claim, "parity", "tested", "tested for parity")},
            "recordedCoverage": claim,
            "residual": claim.get("residual", "No coverage claim is enrolled for this law."),
            "installation": "unknown", "artifactVersion": "unknown",
            "inheritedOrigins": "unknown"})
    return {"schemaVersion": "norn.guarantees.document.v1", "kind": "guarantees",
            "guarantees": rows}


def render(root: Path) -> str:
    catalog = read(root, f"{DIRECTORY}/demo-laws.json")
    statements = unique([item for source in catalog["statementRegistries"]
                         for item in read(root, source)["obligations"]])
    coverage = read(root, "verification/coverage-v1/coverage.json")
    laws = unique(catalog["laws"])
    for system in coverage["subsystems"]:
        for claim in system["obligationClaims"]:
            if claim["id"] not in laws:
                item = statements[claim["id"]]
                laws[item["id"]] = {"id": item["id"], "statement": item["statement"],
                                    "lawLayer": "core primitives",
                                    "law": item.get("law"),
                                    "assumptions": item.get("assumptions", [])}
    laws = list(laws.values())
    document = project(coverage, laws)
    inputs = {"coverage": coverage, "laws": laws}
    document["sourceDigest"] = hashlib.sha256(
        json.dumps(inputs, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return json.dumps(document, ensure_ascii=False, sort_keys=True, indent=2) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    target = args.root / OUTPUT
    payload = render(args.root)
    if args.check:
        if not target.is_file() or target.read_bytes() != payload.encode("utf-8"):
            raise SystemExit(f"{OUTPUT} is stale; run python3 scripts/guarantees_document.py")
    else:
        target.write_text(payload, encoding="utf-8")


if __name__ == "__main__":
    main()
