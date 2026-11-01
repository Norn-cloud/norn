#!/usr/bin/env python3
"""PROOF-BIND-1: project existing, current verification evidence; grant no authority."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

import coverage_manifest
from verification_attestation import canonical_command, collect_git_identity

ROOT = Path(__file__).resolve().parents[1]
REGISTRIES = (
    "verification/stripmine-2026-08-22/obligations.json",
    "verification/core-physics-v1/obligations.json",
    "verification/kernel-v2/manifest.json",
)


def digest(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     ensure_ascii=False).encode()).hexdigest()


def inputs() -> tuple[dict, dict, dict]:
    coverage = coverage_manifest.load()
    statements = {}
    paths = [*coverage_manifest.files(), *(ROOT / name for name in REGISTRIES), Path(__file__)]
    for name in REGISTRIES:
        for item in json.loads((ROOT / name).read_bytes())["obligations"]:
            if item["id"] in statements:
                raise ValueError(f"duplicate obligation: {item['id']}")
            statements[item["id"]] = item
    pins = {path.relative_to(ROOT).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in paths}
    return coverage, statements, pins


def checked_result(path: Path | None, pins: dict) -> dict | None:
    """Use the existing full result validator, including logs, tools and clean source."""
    if path is None:
        return None
    path = path.resolve()
    try:
        before = path.read_bytes()
        checked = subprocess.run(
            [sys.executable, str(ROOT / "verification/coverage-v1/validate_result.py"), str(path)],
            cwd=ROOT, capture_output=True, timeout=300, check=False,
        )
        if checked.returncode or path.read_bytes() != before or inputs()[2] != pins:
            return None
        result = json.loads(before)
        if collect_git_identity(ROOT) != result["git"]:
            return None
        return result
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        return None


def project(coverage: dict, statements: dict, pins: dict, checked: dict | None) -> dict:
    """Project a validator-checked result, never turn declared coverage into evidence."""
    gates = checked["executedGates"] if checked else []
    evidence = {item["path"]: item["sha256"] for item in checked["evidence"]} if checked else {}

    def supported(system: str, layer: str, entries: list) -> bool:
        return bool(entries) and all(
            entry["artifact"] in evidence and entry["selectors"]
            and (layer != "runtime" or all(selector.startswith("test:") for selector in entry["selectors"]))
            and any(
                gate["result"] == "pass" and gate["role"] == "claim-evidence"
                and {"subsystem": system, "layer": layer} in gate["classifications"]
                and canonical_command(gate["command"]) == canonical_command(entry["command"])
                for gate in gates
            ) for entry in entries
        )

    rows = []
    for system in sorted(coverage["subsystems"], key=lambda item: item["id"]):
        claims = {claim["id"]: claim for claim in system["obligationClaims"]}
        for identity in sorted(system["obligations"]):
            claim = claims.get(identity, {})
            statement = statements.get(identity, {})
            links = claim.get("evidence", {})
            proof = claim.get("proof")
            proved = proof in {"reference_proved", "implementation_proved"} and supported(
                system["id"], "proof", links.get("proof", []))
            tested = claim.get("runtime") in {"tested", "tested-under-simulation", "partial"} and supported(
                system["id"], "runtime", links.get("runtime", []))
            rows.append({
                "subsystem": system["id"], "law": statement.get("law"), "obligation": identity,
                "statement": statement.get("statement", "Unknown statement"),
                "level": "proved" if proved and proof == "implementation_proved" else "tested" if tested and claim.get("runtime") != "partial" else "unknown",
                "proof": {"level": "proved" if proved else "unknown", "scope": proof if proved else None},
                "tests": {"level": "tested" if tested else "unknown", "scope": claim.get("runtime") if tested else None},
                "evidence": {layer: links.get(layer, []) if valid else []
                             for layer, valid in [("proof", proved), ("runtime", tested)]},
                "assumptions": statement.get("assumptions", []),
                "residual": claim.get("residual", "No evidence claim is enrolled."),
                "limitations": system["limitations"],
            })
    return {"schemaVersion": "norn.guarantees.matrix/1", "authority": "projection_only",
            "inputDigest": digest(pins), "inputs": pins,
            "evidenceDigest": digest(checked) if checked else None,
            "toolPins": checked["tools"] if checked else {},
            "sourceIdentity": checked["git"] if checked else None,
            "evidenceStatus": "current" if checked else "unknown_or_stale", "rows": rows}


def challenge_rows(challenge: dict) -> list[dict]:
    """An obligation inventory or transition-frame assessment is never checked proof."""
    laws = challenge["lawStatements"]
    binding = digest(challenge)
    return [{"law": laws[goal["law"]] if type(goal.get("law")) is int and 0 <= goal["law"] < len(laws) else None, "obligation": index,
             "statement": goal["statement"], "goal": goal["goal"],
             "level": "unknown", "statementDigest": digest({"challenge": binding, "statement": goal["statement"]})}
            for index, goal in enumerate(challenge["obligations"])]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--result", type=Path, help="existing verification result; stale/missing means unknown")
    parser.add_argument("--challenge", type=Path, help="generated Challenge JSON; unsupported evidence stays unknown")
    args = parser.parse_args()
    coverage, statements, pins = inputs()
    source = collect_git_identity(ROOT)
    checked = checked_result(args.result, pins)
    if checked and checked["git"] != source:
        checked = None
    document = project(coverage, statements, pins, checked)
    document["sourceIdentity"] = source
    binding = {"inputs": pins, "source": source}
    document["inputDigest"] = digest(binding)
    if args.challenge:
        raw = sys.stdin.buffer.read() if str(args.challenge) == "-" else args.challenge.read_bytes()
        challenge = json.loads(raw)
        challenge_digest = hashlib.sha256(raw).hexdigest()
        document["challengeDigest"] = challenge_digest
        document["inputDigest"] = digest({**binding, "challenge": challenge_digest})
        document["generatedObligations"] = challenge_rows(challenge)
    os.set_blocking(sys.stdout.fileno(), True)
    print(json.dumps(document, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
