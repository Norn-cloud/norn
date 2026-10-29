#!/usr/bin/env python3
"""Write a content-addressed result for successfully executed verification gates."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import shlex
import subprocess
from pathlib import Path
from typing import Any

from verification_attestation import (
    canonical_command,
    collect_git_identity,
    collect_tool_versions,
    expected_evidence_paths,
)


ROOT = Path(__file__).resolve().parents[1]
IGNORED_EVIDENCE_DIRS = {".git", ".turbo", "__pycache__", "node_modules", "target"}
CANONICAL_COVERAGE = ROOT / "verification/coverage-v1/coverage.json"
CANONICAL_GATE_MANIFEST = (
    ROOT / "verification/coverage-v1/promoted-gates.json"
)
CANONICAL_COVERAGE_VALIDATOR = (
    ROOT / "verification/coverage-v1/validate.py"
)
AMBIENT_INPUT_OVERRIDES = (
    "NORN_VERIFICATION_CENTRAL_OBLIGATIONS",
    "NORN_VERIFICATION_COVERAGE",
    "NORN_VERIFICATION_GATE_MANIFEST",
    "NORN_VERIFICATION_WORKFLOW",
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(*command: str) -> str:
    return subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def version(*command: str, extra_env: dict[str, str] | None = None) -> str:
    environment = os.environ.copy()
    if extra_env is not None:
        environment.update(extra_env)
    completed = subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        env=environment,
    )
    return "\n".join(part.strip() for part in (completed.stdout, completed.stderr) if part.strip())


def reject_ambient_input_overrides() -> None:
    configured = [name for name in AMBIENT_INPUT_OVERRIDES if name in os.environ]
    if configured:
        raise ValueError(
            "verification result construction rejects ambient canonical-input overrides: "
            f"{configured}"
        )


def relative(path: Path) -> str:
    return path.resolve().relative_to(ROOT).as_posix()


def expand_evidence_root(path: Path) -> set[str]:
    if path.is_file():
        return {relative(path)}
    if not path.is_dir():
        raise ValueError(f"evidence root is neither a file nor directory: {relative(path)}")
    expanded: set[str] = set()
    for candidate in path.rglob("*"):
        if any(part in IGNORED_EVIDENCE_DIRS for part in candidate.relative_to(path).parts):
            continue
        if candidate.is_file() and candidate.suffix != ".pyc":
            expanded.add(relative(candidate))
    return expanded


def canonical_output_paths(value: str) -> list[str]:
    argv = shlex.split(value)
    values: list[str] = []
    for index, argument in enumerate(argv):
        if argument == "--out":
            if index + 1 >= len(argv):
                raise ValueError(f"command option --out has no value: {value}")
            raw_path = Path(argv[index + 1])
            resolved = raw_path.resolve() if raw_path.is_absolute() else (ROOT / raw_path).resolve()
            try:
                values.append(resolved.relative_to(ROOT).as_posix())
            except ValueError as error:
                raise ValueError(f"command output path escapes the repository: {raw_path}") from error
    return values


def parse_passed(path: Path) -> list[dict[str, Any]]:
    gates: list[dict[str, Any]] = []
    seen_ids: set[str] = set()
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        parts = raw_line.split("\t")
        if len(parts) != 6 or not all(parts):
            raise ValueError(f"{relative(path)}:{line_number}: expected six non-empty tab-separated fields")
        gate_id, command, executed_command, evidence_kind, observed_raw, log = parts
        if gate_id in seen_ids:
            raise ValueError(f"duplicate executed gate id: {gate_id}")
        seen_ids.add(gate_id)
        log_path = ROOT / log
        expected_log = f"target/verification-results/logs/{gate_id}.log"
        if log != expected_log:
            raise ValueError(
                f"gate {gate_id} log path must be canonical: expected {expected_log}, got {log}"
            )
        if not log_path.is_file():
            raise ValueError(f"missing gate log: {log}")
        if canonical_command(command) != canonical_command(executed_command):
            raise ValueError(
                f"gate {gate_id} declared command does not match executed argv: "
                f"declared={command!r}, executed={executed_command!r}"
            )
        if evidence_kind not in {"command", "rust-tests", "bun-tests"}:
            raise ValueError(f"gate {gate_id} has unsupported evidence kind: {evidence_kind}")
        try:
            observed_test_count = int(observed_raw)
        except ValueError as error:
            raise ValueError(f"gate {gate_id} has invalid observed test count: {observed_raw}") from error
        if evidence_kind.endswith("-tests") and observed_test_count <= 0:
            raise ValueError(f"gate {gate_id} attests a test command that executed zero tests")
        if evidence_kind == "command" and observed_test_count != 0:
            raise ValueError(f"gate {gate_id} non-test command has a nonzero observed test count")
        gates.append(
            {
                "id": gate_id,
                "command": command,
                "executedCommand": executed_command,
                "evidenceKind": evidence_kind,
                "observedTestCount": observed_test_count,
                "result": "pass",
                "log": log,
                "logSha256": sha256(log_path),
            }
        )
    if not gates:
        raise ValueError("no executed gates were recorded")
    return gates


def bind_promoted_gate_classification(
    gates: list[dict[str, Any]], gate_manifest: dict[str, Any], profile: str | None = None
) -> list[dict[str, Any]]:
    expected_manifest_gates = [
        gate
        for gate in gate_manifest["gates"]
        if profile is None or gate.get("profile") == profile
    ]
    expected_gates = {gate["id"]: gate for gate in expected_manifest_gates}
    executed_gates = {gate["id"]: gate for gate in gates}
    if len(expected_gates) != len(expected_manifest_gates):
        raise ValueError("promoted gate manifest contains duplicate gate ids")
    if len(executed_gates) != len(gates):
        raise ValueError("executed gate records contain duplicate gate ids")
    if set(expected_gates) != set(executed_gates):
        raise ValueError(
            "declared and executed promoted gate ids disagree: "
            f"missing={sorted(set(expected_gates) - set(executed_gates))}, "
            f"extra={sorted(set(executed_gates) - set(expected_gates))}"
        )
    if [gate["id"] for gate in gates] != [gate["id"] for gate in expected_manifest_gates]:
        raise ValueError("executed promoted gate order disagrees with the manifest")
    for gate_id, gate in executed_gates.items():
        expected = expected_gates[gate_id]
        if gate["command"] != expected["command"]:
            raise ValueError(f"promoted gate {gate_id} executed an undeclared command")
        if gate["evidenceKind"] != expected["evidenceKind"]:
            raise ValueError(f"promoted gate {gate_id} executed with the wrong evidence kind")
        gate.update(
            {
                "role": expected["role"],
                "classifications": expected["classifications"],
                "resultArtifacts": expected["resultArtifacts"],
                "generatedArtifacts": expected["generatedArtifacts"],
            }
        )
        observed_outputs = canonical_output_paths(gate["executedCommand"])
        if observed_outputs != expected["resultArtifacts"]:
            raise ValueError(
                f"promoted gate {gate_id} output path disagrees with its exact result ownership: "
                f"expected={expected['resultArtifacts']}, observed={observed_outputs}"
            )
    return gates


def validate_quint_result(path: Path) -> None:
    if not path.is_file():
        raise ValueError(f"missing Quint result: {relative(path)}")
    try:
        result = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise ValueError(f"invalid Quint result JSON: {relative(path)}: {error}") from error
    if result.get("stage") != "verifying" or result.get("status") != "ok":
        raise ValueError(f"Quint result did not complete successfully: {relative(path)}")
    if result.get("errors") != []:
        raise ValueError(f"Quint result contains errors: {relative(path)}")


def evidence_paths(
    coverage: dict[str, Any], coverage_path: Path, generated_evidence: list[Path]
) -> list[str]:
    expected = expected_evidence_paths(ROOT, coverage, coverage_path)
    supplied_generated = {relative(path) for path in generated_evidence}
    if not supplied_generated.issubset(expected):
        raise ValueError(
            "supplied generated evidence is outside the declared evidence closure: "
            f"{sorted(supplied_generated - expected)}"
        )
    return sorted(expected)


def aggregate_digest(items: list[dict[str, str]], path_key: str, digest_key: str) -> str:
    digest = hashlib.sha256()
    for item in sorted(items, key=lambda value: value[path_key]):
        digest.update(item[path_key].encode("utf-8"))
        digest.update(b"\0")
        digest.update(item[digest_key].encode("ascii"))
        digest.update(b"\n")
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--coverage", required=True, type=Path)
    parser.add_argument("--passed-commands", required=True, type=Path)
    parser.add_argument("--quint-result", action="append", default=[], type=Path)
    parser.add_argument("--generated-evidence", action="append", default=[], type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--profile", choices=("fast", "full"))
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args()

    coverage_path = (ROOT / args.coverage).resolve() if not args.coverage.is_absolute() else args.coverage.resolve()
    passed_path = args.passed_commands.resolve()
    if args.output is None and not args.validate_only:
        parser.error("--output is required unless --validate-only is set")
    output_path = args.output.resolve() if args.output is not None else None
    quint_results = [path.resolve() for path in args.quint_result]
    generated_evidence = [path.resolve() for path in args.generated_evidence]
    if coverage_path != CANONICAL_COVERAGE.resolve():
        raise ValueError(
            "verification results require the canonical coverage manifest: "
            f"{relative(CANONICAL_COVERAGE)}"
        )
    reject_ambient_input_overrides()
    run("python3", relative(CANONICAL_COVERAGE_VALIDATOR))
    coverage = json.loads(coverage_path.read_text(encoding="utf-8"))
    gates = parse_passed(passed_path)
    gate_manifest_path = ROOT / coverage["promotedGateManifest"]
    if gate_manifest_path.resolve() != CANONICAL_GATE_MANIFEST.resolve():
        raise ValueError("coverage selected a non-canonical promoted gate manifest")
    gate_manifest = json.loads(gate_manifest_path.read_text(encoding="utf-8"))
    gates = bind_promoted_gate_classification(gates, gate_manifest, args.profile)
    if args.validate_only:
        print(f"validated {args.profile or 'all'} promoted gate registrations")
        return 0

    declared_quint_results = {
        result_path
        for subsystem in coverage["subsystems"]
        for layer_name in ("runtime", "model", "proof")
        for result_path in subsystem[layer_name].get("resultArtifacts", [])
    }
    supplied_quint_results = {relative(path) for path in quint_results}
    gate_quint_results = {
        artifact for gate in gate_manifest["gates"] for artifact in gate["resultArtifacts"]
    }
    if declared_quint_results != gate_quint_results:
        raise ValueError(
            "coverage and promoted-gate Quint result sets disagree: "
            f"coverage={sorted(declared_quint_results)}, gates={sorted(gate_quint_results)}"
        )
    if declared_quint_results != supplied_quint_results:
        raise ValueError(
            "declared and supplied Quint results disagree: "
            f"declared={sorted(declared_quint_results)}, supplied={sorted(supplied_quint_results)}"
        )
    for path in quint_results:
        validate_quint_result(path)

    declared_generated_evidence = {
        artifact
        for subsystem in coverage["subsystems"]
        for layer_name in ("runtime", "model", "proof")
        for artifact in subsystem[layer_name].get("generatedArtifacts", [])
    }
    supplied_generated_evidence = {relative(path) for path in generated_evidence}
    gate_generated_evidence = {
        artifact for gate in gate_manifest["gates"] for artifact in gate["generatedArtifacts"]
    }
    if declared_generated_evidence != gate_generated_evidence:
        raise ValueError(
            "coverage and promoted-gate generated evidence sets disagree: "
            f"coverage={sorted(declared_generated_evidence)}, "
            f"gates={sorted(gate_generated_evidence)}"
        )
    if declared_generated_evidence != supplied_generated_evidence:
        raise ValueError(
            "declared and supplied generated evidence disagree: "
            f"declared={sorted(declared_generated_evidence)}, "
            f"supplied={sorted(supplied_generated_evidence)}"
        )
    for path in generated_evidence:
        if not path.is_file():
            raise ValueError(f"missing generated evidence: {relative(path)}")

    evidence: list[dict[str, str]] = []
    for path_string in evidence_paths(
        coverage, coverage_path, [*quint_results, *generated_evidence]
    ):
        path = ROOT / path_string
        if not path.is_file():
            raise ValueError(f"declared evidence artifact is missing: {path_string}")
        evidence.append({"path": path_string, "sha256": sha256(path)})

    claims = []
    for subsystem in coverage["subsystems"]:
        claims.append(
            {
                "subsystem": subsystem["id"],
                "runtime": subsystem["runtime"]["status"],
                "model": subsystem["model"]["status"],
                "proof": subsystem["proof"]["status"],
                "productionCorrespondence": any(
                    claim["proof"] == "implementation_proved"
                    for claim in subsystem["obligationClaims"]
                ),
                "obligations": subsystem["obligationClaims"],
                "limitations": subsystem["limitations"],
            }
        )

    log_items = [{"path": gate["log"], "sha256": gate["logSha256"]} for gate in gates]
    ontology_tool_root = ROOT / "target/ontology-tools"
    ontology_python = ontology_tool_root / "venv/bin/python"
    if not ontology_python.is_file():
        raise ValueError("pinned ontology Python environment is missing")
    verification_tool_root = ROOT / "target/verification-tools"
    verification_rustup_home = verification_tool_root / "rustup"
    if not verification_rustup_home.is_dir():
        raise ValueError("pinned verification Rust toolchain is missing")
    tools = collect_tool_versions(ROOT)
    git_identity = collect_git_identity(ROOT)
    # Unconditional (#3472): an attestation is only ever produced from a clean worktree.
    if git_identity["workspaceDirty"]:
        raise ValueError("verification attestation requires a clean worktree")
    result = {
        "schemaVersion": "norn.verification.result.v2",
        "result": "pass",
        "generatedAt": dt.datetime.now(dt.UTC).isoformat(),
        "git": git_identity,
        "claimPolicy": coverage["claimPolicy"],
        "statusDefinitions": coverage["statusDefinitions"],
        "coverageManifest": {
            "path": relative(coverage_path),
            "sha256": sha256(coverage_path),
        },
        "gateManifest": {
            "path": relative(gate_manifest_path),
            "sha256": sha256(gate_manifest_path),
        },
        "tools": tools,
        "claims": claims,
        "executedGates": gates,
        "evidence": evidence,
        "evidenceRootSha256": aggregate_digest(evidence, "path", "sha256"),
        "gateLogRootSha256": aggregate_digest(log_items, "path", "sha256"),
    }

    assert output_path is not None
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote passing verification result to {relative(output_path)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
