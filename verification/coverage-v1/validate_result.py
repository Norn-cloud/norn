#!/usr/bin/env python3
"""Validate a Norn verification result and recompute every content digest."""

from __future__ import annotations

import hashlib
import json
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Any

import jsonschema


ROOT = Path(__file__).resolve().parents[2]
SCHEMA = Path(__file__).with_name("result.schema.json")
IGNORED_EVIDENCE_DIRS = {".git", ".turbo", "__pycache__", "node_modules", "target"}
sys.path.insert(0, str(ROOT / "scripts"))
from verification_attestation import (  # noqa: E402
    canonical_command,
    collect_git_identity,
    collect_tool_versions,
    expected_evidence_paths,
    selector_is_declared,
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def aggregate(items: list[dict[str, str]], path_key: str, digest_key: str) -> str:
    digest = hashlib.sha256()
    for item in sorted(items, key=lambda value: value[path_key]):
        digest.update(item[path_key].encode("utf-8"))
        digest.update(b"\0")
        digest.update(item[digest_key].encode("ascii"))
        digest.update(b"\n")
    return digest.hexdigest()


def fail(message: str) -> None:
    raise ValueError(message)


def require_safe_repo_path(value: str, *, label: str) -> None:
    path = Path(value)
    if path.is_absolute() or ".." in path.parts or value in {"", "."}:
        fail(f"{label} is not a safe repository-relative path: {value!r}")


def run(*command: str) -> str:
    return subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def canonical_output_paths(value: str) -> list[str]:
    argv = shlex.split(value)
    values: list[str] = []
    for index, argument in enumerate(argv):
        if argument == "--out":
            if index + 1 >= len(argv):
                fail(f"command option --out has no value: {value}")
            raw_path = Path(argv[index + 1])
            resolved = raw_path.resolve() if raw_path.is_absolute() else (ROOT / raw_path).resolve()
            try:
                values.append(resolved.relative_to(ROOT).as_posix())
            except ValueError:
                fail(f"command output path escapes the repository: {raw_path}")
    return values


def expand_evidence_root(path: Path) -> set[str]:
    if path.is_file():
        return {path.resolve().relative_to(ROOT).as_posix()}
    if not path.is_dir():
        fail(f"evidence root is neither a file nor directory: {path}")
    expanded: set[str] = set()
    for candidate in path.rglob("*"):
        if any(part in IGNORED_EVIDENCE_DIRS for part in candidate.relative_to(path).parts):
            continue
        if candidate.is_file() and candidate.suffix != ".pyc":
            expanded.add(candidate.resolve().relative_to(ROOT).as_posix())
    return expanded


def validate_quint_result(path: Path) -> None:
    try:
        result = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        fail(f"invalid Quint result JSON: {path.relative_to(ROOT)}: {error}")
    if result.get("stage") != "verifying" or result.get("status") != "ok":
        fail(f"Quint result did not complete successfully: {path.relative_to(ROOT)}")
    if result.get("errors") != []:
        fail(f"Quint result contains errors: {path.relative_to(ROOT)}")


def reconcile_result_gates(
    expected_gate_list: list[dict[str, Any]], actual_gate_list: list[dict[str, Any]]
) -> dict[str, dict[str, Any]]:
    expected_gates = {gate["id"]: gate for gate in expected_gate_list}
    actual_gates = {gate["id"]: gate for gate in actual_gate_list}
    if len(expected_gates) != len(expected_gate_list):
        fail("promoted gate manifest contains duplicate gate ids")
    if len(actual_gates) != len(actual_gate_list):
        fail("result contains duplicate promoted gate ids")
    if set(expected_gates) != set(actual_gates):
        fail(
            "declared and executed promoted gate ids disagree: "
            f"missing={sorted(set(expected_gates) - set(actual_gates))}, "
            f"extra={sorted(set(actual_gates) - set(expected_gates))}"
        )
    if [gate["id"] for gate in actual_gate_list] != [gate["id"] for gate in expected_gate_list]:
        fail("executed promoted gate order disagrees with the manifest")
    for gate_id, actual in actual_gates.items():
        expected = expected_gates[gate_id]
        for field in (
            "command",
            "evidenceKind",
            "role",
            "classifications",
            "resultArtifacts",
            "generatedArtifacts",
        ):
            if actual[field] != expected[field]:
                fail(f"executed gate disagrees with promoted manifest: {gate_id}.{field}")
        if canonical_output_paths(actual["executedCommand"]) != expected["resultArtifacts"]:
            fail(f"executed gate output path disagrees with promoted manifest: {gate_id}")
    return actual_gates


def reconcile_evidence_paths(expected: set[str], actual: set[str]) -> None:
    if expected != actual:
        fail(
            "content-addressed evidence set is not the exact declared closure: "
            f"missing={sorted(expected - actual)}, extra={sorted(actual - expected)}"
        )


def reconcile_tool_versions(recorded: dict[str, str], current: dict[str, str]) -> None:
    if recorded != current:
        fail(
            "recorded tool identities do not match recomputed current identities: "
            f"missing={sorted(set(current) - set(recorded))}, "
            f"extra={sorted(set(recorded) - set(current))}, "
            f"changed={sorted(key for key in set(recorded) & set(current) if recorded[key] != current[key])}"
        )


def reconcile_git_identity(recorded: dict[str, object], current: dict[str, object]) -> None:
    if recorded != current:
        fail(
            "recorded Git/workspace identity does not match the exact current workspace: "
            f"missing={sorted(set(current) - set(recorded))}, "
            f"extra={sorted(set(recorded) - set(current))}, "
            f"changed={sorted(key for key in set(recorded) & set(current) if recorded[key] != current[key])}"
        )


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {Path(sys.argv[0]).name} RESULT.json", file=sys.stderr)
        return 2

    result_path = Path(sys.argv[1]).resolve()
    result: dict[str, Any] = json.loads(result_path.read_text(encoding="utf-8"))
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker()).validate(result)

    coverage = result["coverageManifest"]
    if coverage["path"] != "verification/coverage-v1/coverage.json":
        fail("result does not name the canonical coverage manifest")
    coverage_path = ROOT / coverage["path"]
    if not coverage_path.is_file() or sha256(coverage_path) != coverage["sha256"]:
        fail("coverage manifest digest does not match the current workspace")
    coverage_document = json.loads(coverage_path.read_text(encoding="utf-8"))
    run("python3", "verification/coverage-v1/validate.py")
    gate_manifest_record = result["gateManifest"]
    require_safe_repo_path(gate_manifest_record["path"], label="gate manifest path")
    gate_manifest_path = ROOT / gate_manifest_record["path"]
    if not gate_manifest_path.is_file() or sha256(gate_manifest_path) != gate_manifest_record["sha256"]:
        fail("promoted gate manifest digest does not match the current workspace")

    evidence_path_values = [item["path"] for item in result["evidence"]]
    if len(evidence_path_values) != len(set(evidence_path_values)):
        fail("result contains duplicate evidence paths")
    for item in result["evidence"]:
        require_safe_repo_path(item["path"], label="evidence path")
        path = ROOT / item["path"]
        if not path.is_file() or sha256(path) != item["sha256"]:
            fail(f"evidence digest mismatch: {item['path']}")
    if aggregate(result["evidence"], "path", "sha256") != result["evidenceRootSha256"]:
        fail("evidence aggregate digest mismatch")

    gate_manifest = json.loads(gate_manifest_path.read_text(encoding="utf-8"))
    if gate_manifest.get("schemaVersion") != "norn.verification.promoted-gates.v1":
        fail("wrong promoted gate manifest schemaVersion")
    reconcile_result_gates(gate_manifest["gates"], result["executedGates"])

    logs = []
    seen_logs: set[str] = set()
    for gate in result["executedGates"]:
        if canonical_command(gate["command"]) != canonical_command(gate["executedCommand"]):
            fail(f"declared/executed command mismatch: {gate['id']}")
        if gate["evidenceKind"].endswith("-tests") and gate["observedTestCount"] <= 0:
            fail(f"test gate executed zero tests: {gate['id']}")
        if gate["evidenceKind"] == "command" and gate["observedTestCount"] != 0:
            fail(f"non-test gate has nonzero observed test count: {gate['id']}")
        require_safe_repo_path(gate["log"], label=f"gate log {gate['id']}")
        expected_log = f"target/verification-results/logs/{gate['id']}.log"
        if gate["log"] != expected_log:
            fail(f"gate log path is not canonical: {gate['id']}")
        path = ROOT / gate["log"]
        if gate["log"] in seen_logs:
            fail(f"duplicate gate log path: {gate['log']}")
        seen_logs.add(gate["log"])
        if not path.is_file() or sha256(path) != gate["logSha256"]:
            fail(f"gate log digest mismatch: {gate['id']}")
        logs.append({"path": gate["log"], "sha256": gate["logSha256"]})
    if aggregate(logs, "path", "sha256") != result["gateLogRootSha256"]:
        fail("gate-log aggregate digest mismatch")

    if coverage_document["promotedGateManifest"] != gate_manifest_record["path"]:
        fail("result gate manifest is not the manifest selected by coverage")
    if result["claimPolicy"] != coverage_document["claimPolicy"]:
        fail("result claim policy disagrees with coverage manifest")
    if result["statusDefinitions"] != coverage_document["statusDefinitions"]:
        fail("result status definitions disagree with coverage manifest")
    reconcile_tool_versions(result["tools"], collect_tool_versions(ROOT))
    reconcile_git_identity(result["git"], collect_git_identity(ROOT))
    # Unconditional (#3472): no setting can admit a dirty-worktree attestation.
    if result["git"]["workspaceDirty"]:
        fail("clean verification attestation required")

    executed_commands = {gate["command"] for gate in result["executedGates"]}
    declared_commands = {
        command
        for subsystem in coverage_document["subsystems"]
        for layer_name in ("runtime", "model", "proof")
        for command in subsystem[layer_name]["commands"]
    }
    if declared_commands != executed_commands:
        fail(
            "declared and executed verification command sets disagree: "
            f"missing={sorted(declared_commands - executed_commands)}, "
            f"extra={sorted(executed_commands - declared_commands)}"
        )

    evidence_paths = {item["path"] for item in result["evidence"]}
    reconcile_evidence_paths(
        expected_evidence_paths(ROOT, coverage_document, coverage_path),
        evidence_paths,
    )
    rooted_evidence = {
        artifact
        for root in coverage_document["evidenceRoots"]
        for artifact in expand_evidence_root(ROOT / root)
    }
    if not rooted_evidence.issubset(evidence_paths):
        fail(f"declared evidence-root files are not content-addressed: {sorted(rooted_evidence - evidence_paths)}")
    declared_results = {
        artifact
        for subsystem in coverage_document["subsystems"]
        for layer_name in ("runtime", "model", "proof")
        for artifact in subsystem[layer_name].get("resultArtifacts", [])
    }
    if not declared_results.issubset(evidence_paths):
        fail(f"generated verification results are not content-addressed: {sorted(declared_results - evidence_paths)}")
    for artifact in declared_results:
        validate_quint_result(ROOT / artifact)
    declared_generated = {
        artifact
        for subsystem in coverage_document["subsystems"]
        for layer_name in ("runtime", "model", "proof")
        for artifact in subsystem[layer_name].get("generatedArtifacts", [])
    }
    if not declared_generated.issubset(evidence_paths):
        fail(
            "generated implementation evidence is not content-addressed: "
            f"{sorted(declared_generated - evidence_paths)}"
        )
    target_evidence = {
        path for path in evidence_paths if path.startswith("target/verification-results/")
    }
    expected_target_evidence = declared_results | declared_generated
    if target_evidence != expected_target_evidence:
        fail(
            "generated result/evidence attestation set is not exact: "
            f"missing={sorted(expected_target_evidence - target_evidence)}, "
            f"extra={sorted(target_evidence - expected_target_evidence)}"
        )

    declared = {subsystem["id"]: subsystem for subsystem in coverage_document["subsystems"]}
    if len(result["claims"]) != len(declared):
        fail("result claim count does not match the coverage manifest")
    seen: set[str] = set()
    for claim in result["claims"]:
        subsystem_id = claim["subsystem"]
        if subsystem_id in seen or subsystem_id not in declared:
            fail(f"unknown or duplicate result claim: {subsystem_id}")
        seen.add(subsystem_id)
        subsystem = declared[subsystem_id]
        for layer_name in ("runtime", "model", "proof"):
            if claim[layer_name] != subsystem[layer_name]["status"]:
                fail(f"claim status disagrees with coverage: {subsystem_id}.{layer_name}")
        expected_correspondence = any(
            obligation["proof"] == "implementation_proved"
            for obligation in subsystem["obligationClaims"]
        )
        if claim["productionCorrespondence"] != expected_correspondence:
            fail(f"production-correspondence claim disagrees with coverage: {subsystem_id}")
        if claim["obligations"] != subsystem["obligationClaims"]:
            fail(f"obligation claims disagree with coverage: {subsystem_id}")
        if claim["limitations"] != subsystem["limitations"]:
            fail(f"claim limitations disagree with coverage: {subsystem_id}")
        for obligation in claim["obligations"]:
            for layer_name in ("runtime", "model", "proof"):
                for entry in obligation["evidence"][layer_name]:
                    artifact_path = ROOT / entry["artifact"]
                    for selector in entry["selectors"]:
                        if not selector_is_declared(artifact_path, selector):
                            fail(
                                f"result selector is not an exact declaration/value: "
                                f"{subsystem_id}.{obligation['id']}.{layer_name} "
                                f"{entry['artifact']} {selector}"
                            )

    correspondence_count = sum(claim["productionCorrespondence"] for claim in result["claims"])
    print(
        f"verified passing result: {len(result['executedGates'])} gates, "
        f"{len(result['evidence'])} content-addressed artifacts, "
        f"{correspondence_count} subsystem(s) with exact production correspondence claims"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (
        OSError,
        ValueError,
        json.JSONDecodeError,
        jsonschema.ValidationError,
        subprocess.CalledProcessError,
    ) as error:
        print(f"verification result validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
