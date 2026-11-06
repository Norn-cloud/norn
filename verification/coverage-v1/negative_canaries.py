#!/usr/bin/env python3
"""Negative canaries for promoted-gate and result-attestation correspondence."""

from __future__ import annotations

import copy
import importlib.util
import itertools
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from types import ModuleType
from typing import Any, Callable

import yaml


ROOT = Path(__file__).resolve().parents[2]
GATES = Path(__file__).with_name("promoted-gates.json")
COVERAGE = Path(__file__).with_name("manifest")
VALIDATOR = Path(__file__).with_name("validate.py")
WRITER = ROOT / "scripts/write-verification-result.py"
RESULT_VALIDATOR = Path(__file__).with_name("validate_result.py")
PROOF_HOLE_CHECKER = ROOT / "scripts/check-proof-holes.py"
ATTESTATION_HELPER = ROOT / "scripts/verification_attestation.py"
CENTRAL_OBLIGATIONS = (
    ROOT / "verification/stripmine-2026-08-22/obligations.json"
)
CENTRAL_VALIDATOR = (
    ROOT / "verification/stripmine-2026-08-22/scripts/validate.sh"
)
ONTOLOGY_MAPPING_MODEL = (
    ROOT / "verification/ontology-v1/models/ontology-mapping-provenance.qnt"
)
ONTOLOGY_RELATION_MODEL = (
    ROOT / "verification/ontology-v1/models/ontology-relation-authority.qnt"
)
VERIFICATION_TOOL_ROOT = Path(
    os.environ.get(
        "NORN_VERIFICATION_TOOL_ROOT",
        ROOT / "target/verification-tools",
    )
)
QUINT = VERIFICATION_TOOL_ROOT / "bin/quint"
sys.path.insert(0, str(ROOT / "scripts"))

import coverage_manifest  # noqa: E402
import quint_apalache  # noqa: E402  (shared Quint/Apalache execution policy)


CANONICAL_CENTRAL_OBLIGATION_IDS = frozenset(
    {
        "INV-AUTH-ATOMIC-COMMIT",
        "INV-AUTH-STALE-REJECT",
        "INV-AUTH-IDEMPOTENT-RESULT",
        "INV-AUTH-CLOSED-CUSTOM-EFFECT",
        "INV-AUTH-HARD-INVARIANT-OWNERSHIP",
        "INV-AUTH-DOMAIN-ADMISSION-PREDICATE",
        "INV-AUTH-CAPABILITY-NOW",
        "INV-AUTH-SCOPE-CLOSED",
        "INV-ART-AUTHORITY-PROMOTION",
        "INV-ART-DIRECT-WRITE-PREDICATE",
        "INV-QL-CONSERVATION",
        "INV-QL-CERTIFICATE-POSTING-BINDING",
        "INV-QL-UNIT-DIMENSION",
        "INV-QL-EXACT-ARITHMETIC",
        "INV-QL-PENDING-RESOLUTION",
        "INV-QL-BATCH-ATOMIC",
        "INV-QL-AUTHORITY-BRIDGE-ATOMIC",
        "INV-QL-APPEND-REVERSAL",
        "INV-ONT-MAPPING-PROVENANCE",
        "INV-ONT-OPEN-CLOSED-SEPARATION",
        "INV-ONT-RELATION-AUTHORITY",
        "INV-QL-REFERENCE-BALANCE-ALGEBRA",
        "INV-RULE-CLOSED-DETERMINISTIC-EVALUATION",
        "INV-RULE-HIT-POLICY-SAFETY",
        "INV-RULE-HIT-POLICY-ADMISSION-PREDICATE",
        "INV-VER-DIFFERENTIAL-REFINEMENT",
        "INV-VER-NO-PROOF-HOLES",
    }
)
CENTRAL_CANARY_SUBSTITUTE_ID = "INV-CENTRAL-CANARY-SUBSTITUTE"


def load_module(name: str, path: Path) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def expect_value_error(label: str, operation: Callable[[], object]) -> None:
    try:
        operation()
    except ValueError:
        return
    raise AssertionError(f"negative canary unexpectedly passed: {label}")


def expect_coverage_validator_rejection(
    label: str,
    **paths: Path,
) -> str:
    validator = load_module(
        f"norn_coverage_rejection_{abs(hash(label))}", VALIDATOR
    )
    try:
        validator.validate(**paths)
    except validator.InvalidCoverage as error:
        return str(error)
    raise AssertionError(f"negative coverage canary unexpectedly passed: {label}")


def expect_manifest_rejection(label: str, mutate: Callable[[dict[str, Any]], None]) -> None:
    document = json.loads(GATES.read_text(encoding="utf-8"))
    mutate(document)
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", encoding="utf-8") as candidate:
        json.dump(document, candidate)
        candidate.flush()
        expect_coverage_validator_rejection(
            label,
            gate_manifest_source=Path(candidate.name),
        )


def expect_coverage_rejection(label: str, mutate: Callable[[dict[str, Any]], None]) -> None:
    document = coverage_manifest.load(COVERAGE)
    mutate(document)
    with tempfile.TemporaryDirectory() as directory:
        candidate = Path(directory) / "manifest"
        coverage_manifest.write(document, candidate)
        expect_coverage_validator_rejection(label, coverage_path=candidate)


def coverage_layout_canaries() -> None:
    """Each refusal fires through the per-subsystem files a pull request edits."""
    document = coverage_manifest.load(COVERAGE)

    def rejects(label: str, expected: str, arrange: Callable[[Path], object]) -> None:
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "manifest"
            coverage_manifest.write(document, candidate)
            arrange(candidate)
            error = expect_coverage_validator_rejection(label, coverage_path=candidate)
        if expected not in error:
            raise AssertionError(f"{label} failed for an unexpected reason: {error}")
        print(f"coverage layout canary refused {label}: {error[:160]}")

    def edit(name: str, change: Callable[[dict[str, Any]], object]) -> Callable[[Path], object]:
        def arrange(candidate: Path) -> None:
            coverage = coverage_manifest.load(candidate)
            body = next(item for item in coverage["subsystems"] if item["id"] == name)
            change(body)
            coverage_manifest.write(coverage, candidate)
        return arrange

    def wrong_status(body: dict[str, Any]) -> None:
        body["obligationClaims"][0]["runtime"] = "implementation_proven"

    rejects("missing source enrollment", "exact source enrollment mismatch",
            edit("ontology", lambda body: body["sourceFiles"].pop()))
    rejects("claim status outside the vocabulary", "implementation_proven", edit("ontology", wrong_status))
    rejects("unknown subsystem", "mandatory subsystem mismatch",
            lambda c: (c / "subsystems/ontology").rename(c / "subsystems/analytics-projection"))
    def drop_cala_boundary(candidate: Path) -> None:
        # Keep its sources enrolled elsewhere so the mandatory-set check is what fires.
        coverage = coverage_manifest.load(candidate)
        dropped = next(item for item in coverage["subsystems"] if item["id"] == "cala-boundary")
        next(item for item in coverage["subsystems"] if item["id"] == "ontology")["sourceFiles"].extend(dropped["sourceFiles"])
        coverage["subsystems"].remove(dropped)
        coverage_manifest.write(coverage, candidate)

    rejects("missing mandatory subsystem", "mandatory subsystem mismatch", drop_cala_boundary)
    rejects("subsystem file claims an id", "drop the id field",
            lambda c: (c / "subsystems/ontology/index.json").write_text(json.dumps({**json.loads((c / "subsystems/ontology/index.json").read_text()), "id": "ontology"})))
    rejects("index lists subsystems", "must not list subsystems",
            lambda c: (c / "index.json").write_text(json.dumps({**json.loads((c / "index.json").read_text()), "subsystems": []})))
    rejects("stray coverage file", "unknown coverage entries", lambda c: (c / "notes.json").write_text("{}"))
    rejects("misnamed subsystem file", "unknown subsystem files", lambda c: (c / "subsystems/Ontology.json").write_text("{}"))
    rejects("symlinked subsystem file", "not a regular directory", lambda c: (
        (c / "subsystems/ontology").rename(c.parent / "ontology.real"),
        (c / "subsystems/ontology").symlink_to(c.parent / "ontology.real"),
    ))


def manifest_canaries() -> None:
    subprocess.run(["python3", str(VALIDATOR)], cwd=ROOT, check=True)
    expect_manifest_rejection(
        "promoted gate execution order drift", lambda value: value["gates"].reverse()
    )

    with tempfile.TemporaryDirectory() as directory:
        environment = os.environ.copy()
        for name in (
            "NORN_VERIFICATION_COVERAGE",
            "NORN_VERIFICATION_GATE_MANIFEST",
            "NORN_VERIFICATION_WORKFLOW",
            "NORN_VERIFICATION_CENTRAL_OBLIGATIONS",
        ):
            environment[name] = str(Path(directory) / f"ignored-{name}.json")
        completed = subprocess.run(
            ["python3", str(VALIDATOR)],
            cwd=ROOT,
            env=environment,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise AssertionError(
                "production coverage validation still consumed an ambient input override: "
                f"{completed.stdout}{completed.stderr}"
            )

    expect_manifest_rejection("missing gate", lambda value: value["gates"].pop())

    # Change both declarations together so a command-ownership mismatch cannot
    # hide a missing or misplaced Cargo flag.
    for label, suffix in (("unlocked Cargo gate", ""), ("test-only locked flag", " -- --locked")):
        document = json.loads(GATES.read_text(encoding="utf-8"))
        gate = next(item for item in document["gates"] if item["command"].startswith("cargo "))
        original_command = gate["command"]
        gate["command"] = original_command.replace(" --locked", "", 1) + suffix
        coverage = json.loads(json.dumps(coverage_manifest.load(COVERAGE)).replace(
            json.dumps(original_command), json.dumps(gate["command"])
        ))
        with tempfile.TemporaryDirectory() as directory:
            gate_path = Path(directory) / "gates.json"
            coverage_path = Path(directory) / "manifest"
            gate_path.write_text(json.dumps(document), encoding="utf-8")
            coverage_manifest.write(coverage, coverage_path)
            diagnostic = expect_coverage_validator_rejection(
                label, gate_manifest_source=gate_path, coverage_path=coverage_path
            )
        if "promoted Cargo commands must pass --locked" not in diagnostic:
            raise AssertionError(f"{label} failed for an unexpected reason: {diagnostic}")

    def wrong_owner(value: dict[str, Any]) -> None:
        value["gates"][0]["classifications"] = [
            {"subsystem": "analytics-projection", "layer": "proof"}
        ]

    expect_manifest_rejection("wrong subsystem/layer owner", wrong_owner)

    def duplicate_result_owner(value: dict[str, Any]) -> None:
        value["gates"][0]["resultArtifacts"] = [
            "target/verification-results/authority-commit-quint.json"
        ]

    expect_manifest_rejection("result attached to wrong command", duplicate_result_owner)

    def hide_claim_evidence(value: dict[str, Any]) -> None:
        gate = next(item for item in value["gates"] if item["role"] == "claim-evidence")
        gate["role"] = "supporting-check"

    expect_manifest_rejection("claim evidence relabeled supporting", hide_claim_evidence)

    def wrong_selector_kind(value: dict[str, Any]) -> None:
        for subsystem in value["subsystems"]:
            for claim in subsystem["obligationClaims"]:
                if claim["runtime"] != "tested":
                    continue
                for evidence in claim["evidence"]["runtime"]:
                    for index, selector in enumerate(evidence["selectors"]):
                        if selector.startswith("test:"):
                            evidence["selectors"][index] = selector.replace(
                                "test:", "theorem:", 1
                            )
                            return
        raise AssertionError("no tested runtime selector found for canary")

    expect_coverage_rejection("selector kind cannot support claim status", wrong_selector_kind)


def coordinated_central_obligation_deletion(
    obligation_id: str,
    obligations: dict[str, Any],
    coverage: dict[str, Any],
    gates: dict[str, Any],
) -> None:
    """Remove one central obligation and all mutable coverage/gate references."""

    obligations["obligations"] = [
        item for item in obligations["obligations"] if item["id"] != obligation_id
    ]
    owner = next(
        (
            subsystem
            for subsystem in coverage["subsystems"]
            if obligation_id in subsystem["obligations"]
        ),
        None,
    )
    if owner is None:
        return
    removed_claim = next(
        claim for claim in owner["obligationClaims"] if claim["id"] == obligation_id
    )
    owner["obligations"] = [
        value for value in owner["obligations"] if value != obligation_id
    ]
    owner["obligationClaims"] = [
        claim for claim in owner["obligationClaims"] if claim["id"] != obligation_id
    ]

    removed_commands = {
        entry["command"]
        for layer in ("runtime", "model", "proof")
        for entry in removed_claim["evidence"][layer]
    }
    surviving_claim_commands = {
        entry["command"]
        for subsystem in coverage["subsystems"]
        for claim in subsystem["obligationClaims"]
        for layer in ("runtime", "model", "proof")
        for entry in claim["evidence"][layer]
    }
    for gate in gates["gates"]:
        if (
            gate["role"] == "claim-evidence"
            and gate["command"] in removed_commands
            and gate["command"] not in surviving_claim_commands
        ):
            gate["role"] = "supporting-check"


def coordinated_central_obligation_substitution(
    obligation_id: str,
    obligations: dict[str, Any],
    coverage: dict[str, Any],
) -> None:
    """Substitute one central ID everywhere cardinality/reference checks observe it."""

    for obligation in obligations["obligations"]:
        if obligation["id"] == obligation_id:
            obligation["id"] = CENTRAL_CANARY_SUBSTITUTE_ID
    owner = next(
        (
            subsystem
            for subsystem in coverage["subsystems"]
            if obligation_id in subsystem["obligations"]
        ),
        None,
    )
    if owner is None:
        return
    owner["obligations"] = [
        CENTRAL_CANARY_SUBSTITUTE_ID if value == obligation_id else value
        for value in owner["obligations"]
    ]
    for claim in owner["obligationClaims"]:
        if claim["id"] == obligation_id:
            claim["id"] = CENTRAL_CANARY_SUBSTITUTE_ID


def central_obligation_closure_canaries() -> None:
    coverage_validator = load_module(
        "norn_central_obligation_closure_canary", VALIDATOR
    )
    obligations = json.loads(CENTRAL_OBLIGATIONS.read_text(encoding="utf-8"))
    coverage = coverage_manifest.load(COVERAGE)
    gates = json.loads(GATES.read_text(encoding="utf-8"))
    registry_ids = frozenset(item["id"] for item in obligations["obligations"])
    if registry_ids != CANONICAL_CENTRAL_OBLIGATION_IDS:
        raise AssertionError("central registry drifted from the negative-canary closure")
    if (
        coverage_validator.REQUIRED_CENTRAL_OBLIGATION_IDS
        != CANONICAL_CENTRAL_OBLIGATION_IDS
    ):
        raise AssertionError("coverage validator drifted from the central closure")

    def expect_central_rejection(
        label: str,
        mutated_obligations: dict[str, Any],
        mutated_coverage: dict[str, Any],
        mutated_gates: dict[str, Any],
        *,
        check_central_pack: bool,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            candidate_root = Path(directory)
            obligation_path = candidate_root / "obligations.json"
            coverage_path = candidate_root / "manifest"
            gate_path = candidate_root / "promoted-gates.json"
            obligation_path.write_text(
                json.dumps(mutated_obligations), encoding="utf-8"
            )
            coverage_manifest.write(mutated_coverage, coverage_path)
            gate_path.write_text(json.dumps(mutated_gates), encoding="utf-8")
            validators = [
                (
                    "coverage",
                    None,
                )
            ]
            if check_central_pack:
                validators.insert(
                    0,
                    (
                        "central pack",
                        [
                            str(CENTRAL_VALIDATOR),
                            "--test-obligations-file",
                            str(obligation_path),
                        ],
                    ),
                )
            for validator_name, command in validators:
                if command is None:
                    expect_coverage_validator_rejection(
                        label,
                        coverage_path=coverage_path,
                        gate_manifest_source=gate_path,
                        central_obligations_path=obligation_path,
                    )
                    continue
                completed = subprocess.run(
                    command,
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                )
                if completed.returncode == 0:
                    raise AssertionError(
                        f"{validator_name} accepted coordinated central obligation mutation: {label}"
                    )

    # Equality with the independent retained-core constants applies uniformly to every
    # member. Exercise both bypass classes end-to-end with representative IDs,
    # including removal/substitution of all mutable coverage and gate references.
    deletion_id = "INV-QL-CONSERVATION"
    deleted_obligations = copy.deepcopy(obligations)
    deleted_coverage = copy.deepcopy(coverage)
    deleted_gates = copy.deepcopy(gates)
    coordinated_central_obligation_deletion(
        deletion_id,
        deleted_obligations,
        deleted_coverage,
        deleted_gates,
    )
    expect_central_rejection(
        f"delete {deletion_id}",
        deleted_obligations,
        deleted_coverage,
        deleted_gates,
        check_central_pack=True,
    )

    # Reproduce the ambient-env bypass that previously let a complete alternate
    # registry demote one canonical P0 while coverage and gate references were
    # removed. Production validation must ignore the alternate registry and
    # reject the missing owner according to the canonical P0 priority.
    demoted_id = "INV-QL-CONSERVATION"
    demoted_registry = copy.deepcopy(obligations)
    next(
        item for item in demoted_registry["obligations"] if item["id"] == demoted_id
    )["priority"] = "P1"
    demoted_coverage = copy.deepcopy(coverage)
    demoted_gates = copy.deepcopy(gates)
    ignored_obligations = copy.deepcopy(obligations)
    coordinated_central_obligation_deletion(
        demoted_id,
        ignored_obligations,
        demoted_coverage,
        demoted_gates,
    )
    with tempfile.TemporaryDirectory() as directory:
        candidate_root = Path(directory)
        demoted_path = candidate_root / "demoted-central.json"
        coverage_path = candidate_root / "manifest"
        gate_path = candidate_root / "promoted-gates.json"
        demoted_path.write_text(json.dumps(demoted_registry), encoding="utf-8")
        coverage_manifest.write(demoted_coverage, coverage_path)
        gate_path.write_text(json.dumps(demoted_gates), encoding="utf-8")
        previous = os.environ.get("NORN_VERIFICATION_CENTRAL_OBLIGATIONS")
        os.environ["NORN_VERIFICATION_CENTRAL_OBLIGATIONS"] = str(demoted_path)
        try:
            error = expect_coverage_validator_rejection(
                "ambient central P0 demotion and coverage removal",
                coverage_path=coverage_path,
                gate_manifest_source=gate_path,
            )
        finally:
            if previous is None:
                os.environ.pop("NORN_VERIFICATION_CENTRAL_OBLIGATIONS", None)
            else:
                os.environ["NORN_VERIFICATION_CENTRAL_OBLIGATIONS"] = previous
        if "unreferenced P0 obligations" not in error or demoted_id not in error:
            raise AssertionError(
                "ambient central-registry override was rejected for the wrong reason: "
                f"{error}"
            )

    substitution_id = "INV-AUTH-ATOMIC-COMMIT"
    substituted_obligations = copy.deepcopy(obligations)
    substituted_coverage = copy.deepcopy(coverage)
    substituted_gates = copy.deepcopy(gates)
    coordinated_central_obligation_substitution(
        substitution_id,
        substituted_obligations,
        substituted_coverage,
    )
    expect_central_rejection(
        f"substitute {substitution_id}",
        substituted_obligations,
        substituted_coverage,
        substituted_gates,
        check_central_pack=True,
    )


def core_physics_closure_canaries() -> None:
    """Deleting a norn.core law everywhere it is referenced must still be rejected."""
    registry_path = ROOT / "verification/core-physics-v1/obligations.json"
    removed = "INV-CORE-COMMITMENT-COMPLETION"
    registry = json.loads(registry_path.read_text(encoding="utf-8"))
    registry["obligations"] = [item for item in registry["obligations"] if item["id"] != removed]
    coverage = coverage_manifest.load(COVERAGE)
    for subsystem in coverage["subsystems"]:
        if subsystem["id"] == "core-physics":
            subsystem["obligations"].remove(removed)
            subsystem["obligationClaims"] = [
                claim for claim in subsystem["obligationClaims"] if claim["id"] != removed
            ]
    with tempfile.TemporaryDirectory() as directory:
        candidate_registry = Path(directory) / "obligations.json"
        candidate_coverage = Path(directory) / "manifest"
        candidate_registry.write_text(json.dumps(registry), encoding="utf-8")
        coverage_manifest.write(coverage, candidate_coverage)
        message = expect_coverage_validator_rejection(
            "coordinated core-physics law deletion",
            coverage_path=candidate_coverage,
            core_physics_obligations_path=candidate_registry,
        )
    if "norn.core law closure" not in message:
        raise AssertionError(f"core-physics law deletion was rejected for the wrong reason: {message}")

    # Demoting a law below P0 must not let its coverage claim disappear.
    registry = json.loads(registry_path.read_text(encoding="utf-8"))
    for item in registry["obligations"]:
        if item["id"] == removed:
            item["priority"] = "P1"
    with tempfile.TemporaryDirectory() as directory:
        candidate_registry = Path(directory) / "obligations.json"
        candidate_coverage = Path(directory) / "manifest"
        candidate_registry.write_text(json.dumps(registry), encoding="utf-8")
        coverage_manifest.write(coverage, candidate_coverage)
        message = expect_coverage_validator_rejection(
            "core-physics law demotion and coverage removal",
            coverage_path=candidate_coverage,
            core_physics_obligations_path=candidate_registry,
        )
    if "core-physics coverage must claim exactly" not in message:
        raise AssertionError(f"core-physics law demotion was rejected for the wrong reason: {message}")


def workflow_verdict_canaries(source: str) -> None:
    workflow = yaml.safe_load(source)
    verdict = next(step["run"] for step in workflow["jobs"]["full"]["steps"] if "run" in step)
    full_plan = json.loads(subprocess.run(["bash", "scripts/ci/core.sh", "plan"],
                                         cwd=ROOT, capture_output=True, text=True, timeout=10, check=True).stdout)
    full_plan.update(base_sha="base", head_sha="head")
    affected = {"mode": "affected", "base_sha": "base", "head_sha": "head", "shard": ["rest-1"], "phase": []}
    def packed(plan: dict[str, Any]) -> dict[str, Any]:
        return json.loads(subprocess.run(["bash", "scripts/ci/core.sh", "bundle-plan", json.dumps(plan)],
                                        cwd=ROOT, capture_output=True, text=True, timeout=10, check=True).stdout)
    full_plan = packed(full_plan)
    affected = packed(affected)
    passed = dict.fromkeys(("CORE", "PLANNED", "TEST"), "success")
    cases = [(event, full_plan, passed, True) for event in ("push", "merge_group", "workflow_dispatch")]
    cases.append(("pull_request", affected, passed, True))
    cases.extend((event, affected, passed, False)
                 for event in ("push", "merge_group", "workflow_dispatch"))
    cases.extend(("merge_group", full_plan, {**passed, key: result}, False)
                 for key in passed for result in ("failure", "cancelled", "skipped"))
    cases.append(("pull_request", affected, {**passed, "TEST": "skipped"}, False))
    # Affected merge groups pass only while the always-run phases stay in the plan.
    merge_affected = {"mode": "affected", "base_sha": "base", "head_sha": "head", "shard": ["rest-1"], "phase": ["guards", "provenance", "clippy"]}
    merge_affected = packed(merge_affected)
    cases.append(("merge_group", merge_affected, passed, True))
    cases.append(("merge_group", packed({**merge_affected, "phase": ["guards", "provenance"]}), passed, False))
    extra = copy.deepcopy(merge_affected)
    extra["phase"] = ["guards", "provenance", "clippy-extra"]
    for bundle in extra["bundle"]:
        bundle["tasks"] = ["phase/clippy-extra" if task == "phase/clippy" else task for task in bundle["tasks"]]
    cases.append(("merge_group", extra, passed, False))
    cases.append(("pull_request", full_plan, {**passed, "DRAFT": "true"}, False))
    cases.append(("pull_request", full_plan, passed, True))
    for plan in ({}, None, [], {**affected, "mode": "unknown"},
                 {**affected, "shard": "rest-1"}, {**affected, "phase": None},
                 {**affected, "base_sha": "old-base"}, {**affected, "head_sha": "old-head"}):
        cases.append(("pull_request", plan, passed, False))
    duplicate = copy.deepcopy(affected)
    duplicate["shard"] = ["rest-1", "rest-1"]
    duplicate["bundle"] = [{"id": "b0", "tasks": ["test/rest-1", "test/rest-1"]}]
    cases.append(("pull_request", duplicate, passed, False))
    cases.append(("pull_request", affected, {**passed, "CORE": "Success"}, False))
    for mutate in (
        lambda p: p.pop("bundle"),
        lambda p: p["bundle"][0]["tasks"].pop(),
        lambda p: p["bundle"][0]["tasks"].append(p["bundle"][0]["tasks"][0]),
        lambda p: p["bundle"].append(copy.deepcopy(p["bundle"][0])),
    ):
        broken = copy.deepcopy(full_plan)
        mutate(broken)
        cases.append(("pull_request", broken, passed, False))
    for index, (event, plan, results, accepted) in enumerate(cases):
        environment = {**os.environ, "DRAFT": "false", "BASE": "base", "HEAD_SHA": "head", **results, "EVENT": event, "PLAN": json.dumps(plan)}
        result = subprocess.run(["bash", "-c", verdict], cwd=ROOT, env=environment,
                                capture_output=True, text=True, timeout=10, check=False)
        if (result.returncode == 0) != accepted:
            raise AssertionError(f"workflow verdict case {index} had unexpected status {result.returncode}")


def workflow_cleanup_canaries(source: str) -> None:
    workflow = yaml.safe_load(source)
    for job in ("core", "test"):
        cleanup_command_canaries(workflow["jobs"][job]["steps"][0]["run"], unconditional=job == "test")


def cleanup_command_canaries(command: str, *, unconditional: bool) -> None:
    # Substitute commands rather than touching any runner SDK directory.
    with tempfile.TemporaryDirectory() as directory:
        for initial, after, success, reclaimed in ((12582912, 12582912, True, False),
                                                    (12582911, 12582912, True, True),
                                                    (1, 12582911, False, True)):
            marker = Path(directory) / "reclaimed"
            marker.unlink(missing_ok=True)
            preamble = f"""df() {{
  if [ "$1" = -Pk ]; then
    if [ -f '{marker}' ]; then free={after}; else free={initial}; fi
    printf 'Filesystem blocks Used Available Capacity Mounted\\nroot 99999999 0 %s 0 /\\n' "$free"
  fi
}}
sudo() {{
  [ "$*" = 'rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc /usr/local/share/boost /opt/hostedtoolcache/CodeQL' ] || return 99
  touch '{marker}'
}}
"""
            result = subprocess.run(["bash", "-c", preamble + command], capture_output=True, timeout=10)
            if (result.returncode == 0) != success or marker.exists() != (reclaimed or unconditional):
                raise AssertionError(f"cleanup preflight drift: {initial}/{after}: {result.stderr!r}")



def execute_rights_publisher(script: str) -> dict[str, Any]:
    """Run the actual publisher with an independently fixed, fresh decision."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        root.joinpath("rights-authority.cjs").write_text(
            "module.exports=async(g,c,k,p,probe)=>probe||({rights:{},"
            "publication:{digest:'" + "a" * 64 + "',failure_version:0},authority_deadline:Date.now()+600000});")
        root.joinpath("rights-parameters.json").write_text("{}")
        root.joinpath("rights-decision.json").write_text(json.dumps({"digest": "a" * 64}))
        harness = """const context={repo:{owner:'owner',repo:'repo'},payload:{issue:{number:7}},
serverUrl:'https://example.invalid',runId:42,sha:'default-branch-head'};
const core={warning:()=>{},setFailed:()=>{}};
const github={rest:{pulls:{get:async()=>({data:{head:{sha:'head'}}})},
repos:{getCommit:async()=>({data:{commit:{tree:{sha:'tree'}}}}),listCommitStatusesForRef:()=>{},
createCommitStatus:async row=>console.log(JSON.stringify(row))}},
paginate:async()=>[{id:1,state:'success',creator:{login:'github-actions[bot]'},
context:'rights-checkpoint/publication/tree/42/1',description:JSON.stringify({d:'a'.repeat(64)})}]};
(async()=>{
""" + script + "\n})().catch(error=>{console.error(error);process.exit(1)});"
        result = subprocess.run(["bun", "-e", harness], capture_output=True, text=True, timeout=10,
                                env={**os.environ, "RUNNER_TEMP": directory}, check=True)
    return json.loads(result.stdout)


def workflow_trusted_policy_canaries(source: str) -> None:
    job = yaml.safe_load(source)["jobs"]["trusted-policy"]
    expression = " ".join(job["if"].split())
    for event, action, body, previous, is_pr, expected in (
        ("pull_request_target", "synchronize", "", "", True, True),
        ("issue_comment", "created", "/approve-gates head", "", True, True),
        ("issue_comment", "edited", "revoked", "/approve-gates head", True, True),
        ("issue_comment", "deleted", "/approve-gates head", "", True, True),
        ("issue_comment", "created", "/ack-rights head", "", True, True),
        ("issue_comment", "created", "/revoke-rights head", "", True, True),
        ("issue_comment", "deleted", "/revoke-rights head", "", True, True),
        ("issue_comment", "edited", "ordinary text", "/revoke-rights head", True, True),
        ("issue_comment", "created", "unrelated", "", True, False),
        ("issue_comment", "created", "/approve-gates head", "", False, False),
        ("pull_request", "synchronize", "", "", True, False),
    ):
        for cancelled in (False, True):
            condition = expression
            for key, value in {
                "github.event_name": event, "github.event.action": action,
                "github.event.comment.body": body, "github.event.changes.body.from": previous,
                "github.event.issue.pull_request": is_pr,
            }.items():
                condition = condition.replace(key, repr(value))
            condition = condition.replace("cancelled()", str(cancelled)).replace("&&", " and ").replace("||", " or ")
            condition = re.sub(r"!(?!=)", " not ", condition)
            accepted = eval(condition, {"__builtins__": {}}, {"contains": lambda text, part: part in text})
            if bool(accepted) != (expected and not cancelled):
                raise AssertionError(f"trusted-policy event admission drift: {event}/{action}/{cancelled}")

    reporter = next(step for step in job["steps"]
                    if step.get("name") == "Report trusted policy on the exact PR head")

    def report(status: str, *, resolved: bool = True) -> dict[str, Any]:
        script = reporter["with"]["script"].replace("${{ steps.pr.outputs.head_sha }}", "head" if resolved else "").replace(
            "${{ job.status }}", status)
        value = execute_rights_publisher(script)
        if value["sha"] != "head" or value["context"] != "trusted-policy":
            raise AssertionError("reporter lost the required head/context binding")
        return value

    for status in ("success", "failure", "cancelled", "skipped"):
        for resolved in (False, True):
            if report(status, resolved=resolved)["state"] != ("success" if status == "success" else "failure"):
                raise AssertionError(f"trusted-policy reporter accepted {status}")

    def required_passes(status: dict[str, Any], check_name: str, check_result: str) -> bool:
        # GitHub requires both a check run and a commit status when names collide.
        return status["state"] == "success" and (check_name != status["context"] or check_result == "success")

    # An approval arrives while pull_request_target is running. Its cancellation
    # reports failure; the comment run then publishes a completed success on HEAD.
    # The comment's own check run belongs to the default branch, not that PR head.
    cancelled = report("cancelled")
    approved = report("success")
    if required_passes(cancelled, job["name"], "cancelled"):
        raise AssertionError("cancelled policy evaluation passed")
    if not required_passes(approved, job["name"], "cancelled"):
        raise AssertionError("cancelled PR-target check blocks completed approval")
    if required_passes(approved, "trusted-policy", "cancelled"):
        raise AssertionError("old duplicate-name cancellation defect was not reproduced")
    # Revocation cancels a running evaluation and its replacement fails policy.
    # Neither the cancellation nor that completed refusal may retain green.
    for status in (report("cancelled"), report("failure")):
        if required_passes(status, job["name"], "success"):
            raise AssertionError("revoked approval retained a successful verdict")



def workflow_merge_group_policy_canaries(source: str) -> None:
    public = yaml.safe_load(source)
    group = yaml.safe_load((ROOT / ".github/workflows/trusted-merge-group.yml").read_text())

    def check(document: dict[str, Any]) -> None:
        trigger = document.get("on", document.get(True))
        if trigger != {"workflow_run": {"workflows": [public["name"]], "types": ["requested"]}}:
            raise AssertionError("merge-group policy publisher lost its Public core trigger")
        job = document["jobs"]["trusted-policy"]
        if job.get("permissions", {}).get("statuses") != "write" or job.get("continue-on-error"):
            raise AssertionError("merge-group policy cannot publish a required verdict")
        condition = job.get("if", "")
        for event, same_repository in (("merge_group", True), ("merge_group", False),
                                       ("pull_request", True), ("push", True)):
            expression = condition.replace("github.event.workflow_run.event", repr(event)).replace(
                "github.event.workflow_run.head_repository.full_name", repr("owner/repo" if same_repository else "fork/repo")).replace(
                "github.repository", repr("owner/repo")).replace("&&", " and ").replace("||", " or ")
            if bool(eval(expression, {"__builtins__": {}}, {})) != (event == "merge_group" and same_repository):
                raise AssertionError("merge-group publisher event/repository boundary drifted")
        command = 'python3 trusted/scripts/ci/validate-runner-policy.py --root candidate --maintainer-evidence "$RUNNER_TEMP/maintainer-approval.json" --rights-evidence "$RUNNER_TEMP/maintainer-approval.json" --head "${{ github.event.workflow_run.head_sha }}" --repository "${{ github.repository }}"'
        validators = [step for step in job["steps"] if step.get("run") == command]
        if len(validators) != 1 or any(validators[0].get(key) for key in ("if", "continue-on-error", "env", "shell")):
            raise AssertionError("merge-group status lost its unconditional base-owned validation")
        reporters = [step for step in job["steps"] if step.get("name") == "Report trusted policy on the group head"]
        if len(reporters) != 1:
            raise AssertionError("merge-group required status publisher is missing")
        reporter = reporters[0]
        if (reporter.get("if") != "always()" or reporter.get("continue-on-error") or reporter.get("env")
                or reporter.get("uses") != "actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3"
                or set(reporter.get("with", {})) != {"retries", "script"} or reporter["with"]["retries"] != 3):
            raise AssertionError("merge-group reporter must always run with the default Actions token")
        for status in ("success", "failure", "cancelled", "skipped"):
            script = reporter["with"]["script"].replace("${{ github.event.workflow_run.head_sha }}", "group-head").replace(
                "${{ github.sha }}", "default-branch-head").replace("${{ job.status }}", status)
            value = execute_rights_publisher(script)
            if (value["sha"] != "group-head" or value["context"] != "trusted-policy"
                    or value["state"] != ("success" if status == "success" else "failure")):
                raise AssertionError("merge-group required context/head/fail-closed verdict was lost")

    check(group)
    for mutation in ("trigger", "event", "permission", "validation", "reporter", "reporter-if", "context", "head", "state"):
        document = copy.deepcopy(group)
        job = document["jobs"]["trusted-policy"]
        reporter = next(step for step in job["steps"] if step.get("name") == "Report trusted policy on the group head")
        if mutation == "trigger":
            document.get("on", document.get(True))["workflow_run"]["workflows"] = ["Different workflow"]
        elif mutation == "event":
            job["if"] = "False"
        elif mutation == "permission":
            job["permissions"]["statuses"] = "read"
        elif mutation == "validation":
            job["steps"] = [step for step in job["steps"] if step.get("name") != "Validate the group head as data with base-owned policy"]
        elif mutation == "reporter":
            job["steps"].remove(reporter)
        elif mutation == "reporter-if":
            reporter["if"] = "success()"
        else:
            before, after = {
                "context": ("context:'trusted-policy'", "context:'different-status'"),
                "head": ("${{ github.event.workflow_run.head_sha }}", "${{ github.sha }}"),
                "state": ("'${{ job.status }}'==='success'", "true"),
            }[mutation]
            if before not in reporter["with"]["script"]:
                raise AssertionError(f"merge-group mutation target drifted: {mutation}")
            reporter["with"]["script"] = reporter["with"]["script"].replace(before, after)
        try:
            check(document)
        except AssertionError:
            continue
        raise AssertionError(f"merge-group context loss mutation passed: {mutation}")


def workflow_cache_canaries() -> None:
    from unittest.mock import patch
    importer = load_module("workflow_interop_cache_canary", ROOT / "scripts/importers/erpnext_runtime.py")
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        cache = directory / "cache"
        cache.mkdir()
        source = directory / "source"
        source.mkdir()
        protected = source / "protected.py"
        protected.write_bytes(b"preserved source")
        entry = cache / "schema.json"
        entry.symlink_to(protected)
        with patch.object(importer, "ROOT", source), patch.dict(os.environ, NORN_INTEROP_CACHE=str(cache)):
            if importer.cached("schema.json", "wrong digest") is not None:
                raise AssertionError("cache accepted a digest mismatch")
            importer.store("schema.json", b"verified schema")
            if protected.read_bytes() != b"preserved source" or entry.is_symlink() or entry.read_bytes() != b"verified schema":
                raise AssertionError("cache replacement followed a symlink")
            with patch.object(Path, "replace", side_effect=OSError("replacement failed")):
                importer.store("schema.json", b"replacement")
            if list(cache.iterdir()) != [entry] or entry.read_bytes() != b"verified schema":
                raise AssertionError("failed cache replacement left temporary data")


def workflow_context_canaries(source: str) -> None:
    workflow = yaml.safe_load(source)
    if True in workflow:
        workflow["on"] = workflow.pop(True)
    validator = load_module("norn_workflow_context_canary", VALIDATOR)
    with tempfile.TemporaryDirectory() as directory:
        candidate = Path(directory) / "ci-policy.yml"
        candidate.write_text(yaml.safe_dump(workflow, sort_keys=False), encoding="utf-8")
        validator.validate_workflow_integrity([], formal_path=candidate)

        def rejected(label: str, document: dict[str, Any]) -> None:
            candidate.write_text(yaml.safe_dump(document, sort_keys=False), encoding="utf-8")
            try:
                validator.validate_workflow_integrity([], formal_path=candidate)
            except validator.InvalidCoverage:
                return
            raise AssertionError(f"workflow context mutation passed: {label}")

        for name in ("core", "plan", "full"):
            expression = workflow["jobs"][name]["if"]
            for event in ("pull_request", "push", "merge_group", "workflow_dispatch", "pull_request_target", "issue_comment"):
                for draft in (False, True):
                    for action in ("opened", "synchronize", "reopened", "ready_for_review"):
                        if event == "pull_request" and action not in workflow["on"]["pull_request"]["types"]:
                            raise AssertionError(f"missing PR activity: {action}")
                        candidate_expression = expression.replace("github.event_name", repr(event)).replace(
                            "github.event.pull_request.draft", str(draft)).replace("always()", "True").replace(
                            "cancelled()", "False").replace("&&", " and ").replace("||", " or ")
                        candidate_expression = re.sub(r"!(?!=)", " not ", candidate_expression)
                        expected = event not in ("pull_request_target", "issue_comment") and not (
                            name == "plan" and event == "pull_request" and draft)
                        if bool(eval(candidate_expression, {"__builtins__": {}}, {})) != expected:
                            raise AssertionError(f"{name} admission drift: {event}/{action}/{draft}")
        for event in ("pull_request", "merge_group", "push", "workflow_dispatch"):
            for draft in (False, True):
                expression = workflow["jobs"]["full"]["if"].replace("github.event_name", repr(event)).replace(
                    "github.event.pull_request.draft", str(draft)).replace(
                    "always()", "True").replace("cancelled()", "True").replace("&&", " and ")
                expression = re.sub(r"!(?!=)", " not ", expression)
                if bool(eval(expression, {"__builtins__": {}}, {})) != (event != "pull_request" or draft):
                    raise AssertionError(f"cancelled verdict scheduling drift: {event}/{draft}")
        document = copy.deepcopy(workflow)
        document["run-name"] = "${{ github.event.pull_request.title }}"
        rejected("forged READY attribution", document)
        for name in ("core", "plan", "full", "trusted-policy"):
            document = copy.deepcopy(workflow)
            document["jobs"][name]["name"] = "renamed"
            rejected(f"renamed control check {name}", document)
        for change in ("concurrency", "if", "reporter"):
            document = copy.deepcopy(workflow)
            trusted = document["jobs"]["trusted-policy"]
            if change == "concurrency":
                trusted["concurrency"]["cancel-in-progress"] = True
            elif change == "if":
                trusted["if"] = "!cancelled() && github.event_name == 'pull_request_target'"
            else:
                reporter = next(step for step in trusted["steps"]
                                if step.get("name") == "Report trusted policy on the exact PR head")
                reporter["with"]["script"] = reporter["with"]["script"].replace("context:'trusted-policy'", "context:'different-status'")
            rejected(f"trusted-policy {change} mutation", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["trusted-policy"]["name"] = "trusted-policy"
        rejected("duplicate required check name", document)
        for trigger in workflow["on"]:
            document = copy.deepcopy(workflow)
            del document["on"][trigger]
            rejected(f"removed trigger {trigger}", document)
        for name in ("core", "plan", "test", "full"):
            document = copy.deepcopy(workflow)
            document["jobs"][name]["steps"][-1]["shell"] = "/usr/bin/true {0}"
            rejected(f"{name} shell override", document)
        for key in ("DRAFT", "BASE", "HEAD_SHA", "CORE", "PLANNED", "TEST"):
            document = copy.deepcopy(workflow)
            document["jobs"]["full"]["steps"][0]["env"][key] = "forged"
            rejected(f"forged verdict input {key}", document)
        for name in ("plan", "full"):
            document = copy.deepcopy(workflow)
            document["jobs"][name]["if"] = "!cancelled()"
            rejected(f"{name} cancellation/admission mutation", document)
        document = copy.deepcopy(workflow)
        document["on"]["pull_request"]["types"].remove("ready_for_review")
        rejected("missing READY activity", document)
        for name in ("core", "test"):
            for before, after in (("12 * 1024 * 1024", "1 * 1024 * 1024"),
                                  ('[ "$(available)" -ge "$minimum" ]', "true"),
                                  ("df -h /", "sudo docker image prune -af")):
                document = copy.deepcopy(workflow)
                step = document["jobs"][name]["steps"][0]
                step["run"] = step["run"].replace(before, after)
                rejected(f"{name} cleanup mutation {before}", document)

        document = copy.deepcopy(workflow)
        document["jobs"]["core"]["steps"][0]["run"] = document["jobs"]["core"]["steps"][0]["run"].replace(
            'if [ "$(available)" -lt "$minimum" ]; then', "if true; then")
        rejected("unconditional core cleanup", document)
        for name in ("core", "test"):
            # A Boat pull-request VM is self-hosted: cleanup keys on the hosted image alone.
            for condition in ("${{ !(github.event_name == 'merge_group' && runner.environment == 'self-hosted') }}",
                              "${{ runner.environment != 'self-hosted' }}",
                              "${{ github.event_name != 'merge_group' }}"):
                document = copy.deepcopy(workflow)
                document["jobs"][name]["steps"][0]["if"] = condition
                rejected(f"{name} cleanup keyed off the hosted image", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["steps"][0]["run"] = workflow["jobs"]["core"]["steps"][0]["run"]
        rejected("threshold-only hosted bundle cleanup", document)

        def action_step(document: dict[str, Any], job: str, action: str) -> dict[str, Any]:
            return next(step for step in document["jobs"][job]["steps"]
                        if step.get("uses", "").startswith(action + "@"))

        runner = "${{ ((github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat') || (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26' && github.actor == 'Youssef-Tarek-Ali-26' && github.triggering_actor == 'Youssef-Tarek-Ali-26' && vars.NORN_PR_RUNNER == 'boat')) && 'boat' || 'ubuntu-24.04' }}"
        merge_only = "${{ github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat' && 'boat' || 'ubuntu-24.04' }}"
        same_repo = "github.event.pull_request.head.repo.full_name == github.repository && "
        legacy = '${{ github.event_name == \'merge_group\' && vars.NORN_MERGE_RUNNER == \'self-hosted\' && fromJSON(format(\'["self-hosted","norn-merge","norn-run-{0}"]\', github.run_id)) || \'ubuntu-24.04\' }}'
        expression = runner.removeprefix("${{ ").removesuffix(" }}")
        boat_users = ("Youssef-Tarek-Ali-26", "dependabot[bot]", "other-maintainer")
        for event in ("merge_group", "pull_request", "pull_request_target", "issue_comment", "push", "workflow_dispatch"):
            # Boat CI runs public-repository jobs only for its allowlisted user: any
            # other PR author, actor or re-runner (Dependabot included) stays hosted.
            for merge_variable, pr_variable, head_repository, author, actor, rerunner in itertools.product(
                    ("", "boat", "self-hosted", "other"), ("", "boat", "self-hosted", "other"),
                    ("Norn-cloud/norn", "attacker/norn", ""), *[boat_users] * 3):
                concrete = (expression.replace("github.event_name", repr(event))
                            .replace("vars.NORN_MERGE_RUNNER", repr(merge_variable))
                            .replace("vars.NORN_PR_RUNNER", repr(pr_variable))
                            .replace("github.event.pull_request.head.repo.full_name", repr(head_repository))
                            .replace("github.event.pull_request.user.login", repr(author))
                            .replace("github.triggering_actor", repr(rerunner))
                            .replace("github.actor", repr(actor))
                            .replace("github.repository", repr("Norn-cloud/norn"))
                            .replace("&&", " and ").replace("||", " or "))
                expected = ("boat" if (event == "merge_group" and merge_variable == "boat")
                            or (event == "pull_request" and pr_variable == "boat"
                                and head_repository == "Norn-cloud/norn"
                                and author == actor == rerunner == "Youssef-Tarek-Ali-26") else "ubuntu-24.04")
                if eval(concrete, {"__builtins__": {}}, {}) != expected:
                    raise AssertionError("Boat runner evaluation drift: " + "/".join(
                        (event, merge_variable, pr_variable, head_repository, author, actor, rerunner)))
        for name in ("core", "plan", "test", "full", "trusted-policy", "rights-capture", "rights-revocation"):
            unsafe = ["boat", "self-hosted", ["self-hosted", "norn-merge"], legacy,
                      "${{ vars.NORN_MERGE_RUNNER || 'ubuntu-24.04' }}",
                      "${{ vars.NORN_PR_RUNNER || 'ubuntu-24.04' }}",
                      runner.replace("github.event_name == 'merge_group' && ", ""),
                      runner.replace("github.event_name == 'pull_request' && ", ""),
                      runner.replace(" && vars.NORN_MERGE_RUNNER == 'boat'", ""),
                      runner.replace(" && vars.NORN_PR_RUNNER == 'boat'", ""),
                      # Fork pull requests must never reach a Boat VM.
                      runner.replace(same_repo, ""),
                      runner.replace(same_repo, "github.event.pull_request.head.repo.fork == false && "),
                      runner.replace("== github.repository", "!= ''"),
                      runner.replace("vars.NORN_PR_RUNNER", "vars.NORN_MERGE_RUNNER"),
                      # Bot- or other-user-triggered pull requests would queue forever on Boat.
                      runner.replace(" && github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26'", ""),
                      runner.replace(" && github.actor == 'Youssef-Tarek-Ali-26'", ""),
                      runner.replace(" && github.triggering_actor == 'Youssef-Tarek-Ali-26'", ""),
                      runner.replace("github.actor == 'Youssef-Tarek-Ali-26'", "github.actor != 'dependabot[bot]'"),
                      runner.replace("github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26'",
                                     "github.event.pull_request.user.type != 'Bot'"),
                      runner.replace("'Youssef-Tarek-Ali-26'", "'dependabot[bot]'"),
                      runner.replace("== 'boat'", "== 'self-hosted'"),
                      runner.replace("|| 'ubuntu-24.04'", "|| 'boat'")]
            for event in ("pull_request", "pull_request_target", "issue_comment", "push", "workflow_dispatch"):
                unsafe.append(runner.replace("'merge_group'", repr(event)))
            for event in ("pull_request_target", "issue_comment", "push", "workflow_dispatch"):
                unsafe.append(runner.replace("'pull_request'", repr(event)))
            if name in ("core", "plan", "test", "full"):
                unsafe += ["ubuntu-24.04", merge_only]
            else:
                # Rights authority stays on hosted runners in every form.
                unsafe += [runner, merge_only]
            for value in unsafe:
                if value == workflow["jobs"][name]["runs-on"]:
                    raise AssertionError(f"{name}: runner mutation is a no-op: {value}")
                document = copy.deepcopy(workflow)
                document["jobs"][name]["runs-on"] = value
                rejected(f"{name} unsafe runner routing", document)
        document = copy.deepcopy(workflow)
        for name in ("core", "plan", "test", "full"):
            document["jobs"][name]["runs-on"] = runner
        candidate.write_text(yaml.safe_dump(document, sort_keys=False), encoding="utf-8")
        validator.validate_workflow_integrity([], formal_path=candidate)
        for name in ("core", "plan", "test"):
            document = copy.deepcopy(workflow)
            document["jobs"][name]["steps"].insert(-1, {
                "uses": "actions/github-script@" + "a" * 40,
                "with": {"script": "core.exportVariable('NORN_CI_PACKAGES', 'norn-canonical')"},
            })
            rejected(f"{name} additional pinned preparation action", document)
            for key, value in (("ref", "main"), ("repository", "other/repository"),
                               ("path", "other-checkout"), ("persist-credentials", True),
                               ("persist-credentials", 0), ("fetch-depth", False)):
                document = copy.deepcopy(workflow)
                action_step(document, name, "actions/checkout")["with"][key] = value
                rejected(f"{name} checkout {key}", document)
            for key in ("BASH_ENV", "BASH_FUNC_exit%%", "LD_PRELOAD", "NORN_CI_PACKAGES", "NORN_CI_REST_SHARDS", "NORN_CI_BASE", "PLAN", "BUNDLE_ID"):
                document = copy.deepcopy(workflow)
                document["jobs"][name]["steps"][-1].setdefault("env", {})[key] = "override"
                rejected(f"{name} step environment {key}", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["needs"] = ["plan"]
        rejected("bundles admitted before the cheap gate", document)
        for key in ("PLAN", "BUNDLE_ID"):
            document = copy.deepcopy(workflow)
            document["jobs"]["test"]["env"][key] = "forged"
            rejected(f"forged bundle input {key}", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["strategy"]["max-parallel"] = 2
        rejected("surplus billed jobs waiting for matrix slots", document)
        document = copy.deepcopy(workflow)
        action_step(document, "test", "oven-sh/setup-bun")["with"]["bun-version"] = "latest"
        rejected("altered preparation action inputs", document)
        for location in ("workflow", "job", "step"):
            document = copy.deepcopy(workflow)
            target = (document if location == "workflow" else document["jobs"]["full"]
                      if location == "job" else document["jobs"]["full"]["steps"][0])
            target.setdefault("env", {})["BASH_ENV"] = "/tmp/early-success"
            rejected(f"{location} inherited BASH_ENV", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["env"]["BASH_FUNC_cargo%%"] = "() { return 0; }"
        rejected("job environment cannot replace gate commands", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["full"]["steps"].insert(0, {"run": "echo BASH_ENV=/tmp/early-success >> $GITHUB_ENV"})
        rejected("verdict preparation can change the shell environment", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["steps"].insert(-1, {"run": "echo NORN_CI_PACKAGES=norn-canonical >> $GITHUB_ENV"})
        rejected("test preparation can override plan inputs", document)
        document = copy.deepcopy(workflow)
        checkout = action_step(document, "core", "actions/checkout")
        document["jobs"]["core"]["steps"].remove(checkout)
        document["jobs"]["core"]["steps"].append(checkout)
        rejected("candidate checkout after the gate", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["env"]["NORN_CI_BASE"] = "HEAD"
        rejected("unbound shrink-only baseline", document)
        candidate.write_text(source.replace("      - run: bash scripts/ci/core.sh cheap",
                                            "      - run: echo ignored\n        run: bash scripts/ci/core.sh cheap", 1), encoding="utf-8")
        expect_coverage_validator_rejection("duplicate workflow run key", formal_workflow_path=candidate)


def workflow_canaries() -> None:
    source = (ROOT / ".github/workflows/ci-policy.yml").read_text(encoding="utf-8")
    workflow_verdict_canaries(source)
    workflow_context_canaries(source)
    workflow_cleanup_canaries(source)
    workflow_trusted_policy_canaries(source)
    workflow_merge_group_policy_canaries(source)
    workflow_cache_canaries()
    mutations = [
        ("missing cheap gate", "run: bash scripts/ci/core.sh cheap", "run: echo skipped"),
        ("missing bundle executor", 'run: bash scripts/ci/core.sh bundle "$PLAN" "$BUNDLE_ID"', "run: echo skipped"),
        ("missing bundle matrix", "bundle: ${{ fromJSON(needs.plan.outputs.plan).bundle }}", "bundle: []"),
        ("missing full verdict dependency", "needs: [core, plan, test]", "needs: [core, plan]"),
        ("disabled planned bundles", "if: fromJSON(needs.plan.outputs.plan).bundle[0] != null", "if: false"),
        ("wrong affected base", "github.event_name == 'pull_request' && github.event.pull_request.base.sha", "github.event_name == 'pull_request' && github.sha"),
        ("unbound affected packages", "NORN_CI_PACKAGES: ${{ fromJSON(needs.plan.outputs.plan).packages }}", "NORN_CI_PACKAGES: norn-skeleton-guard"),
        ("unbound rest partitions", "NORN_CI_REST_SHARDS: ${{ fromJSON(needs.plan.outputs.plan).rest_shards }}", "NORN_CI_REST_SHARDS: 1"),
        ("missing full-plan branch", 'else plan=$(bash scripts/ci/core.sh plan); fi', 'else plan="{}"; fi'),
        ("forged core verdict", "CORE: ${{ needs.core.result }}", "CORE: success"),
        ("missing full-plan assertion", '[ "$mode" = full ]', 'true'),
        ("planned skipped job accepted", '[ "$2" -eq 0 ]', 'true'),
        ("cancelled main verdict skipped", "!(cancelled() && github.event_name == 'pull_request' && !github.event.pull_request.draft)", "!cancelled()"),
        ("floating action", "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", "actions/checkout@v4"),
        ("floating Rust toolchain", "toolchain: 1.98.0", "toolchain: stable"),
        ("insufficient proof timeout", "timeout-minutes: 110", "timeout-minutes: 20"),
    ]
    mutations.extend((f"missing {trigger} trigger", f"  {trigger}:\n", "") for trigger in ("pull_request", "pull_request_target", "push", "merge_group"))
    with tempfile.TemporaryDirectory() as directory:
        candidate = Path(directory) / "ci-policy.yml"
        for label, original, replacement in mutations:
            if original not in source:
                raise AssertionError(f"workflow canary target drifted: {label}")
            candidate.write_text(source.replace(original, replacement, 1), encoding="utf-8")
            expect_coverage_validator_rejection(label, formal_workflow_path=candidate)
        decoy = source.replace("toolchain: 1.98.0", "toolchain: stable", 1).replace(
            "name: Public core\n", "name: Public core\nenv:\n  toolchain: 1.98.0\n", 1)
        candidate.write_text(decoy, encoding="utf-8")
        expect_coverage_validator_rejection("decoy toolchain key", formal_workflow_path=candidate)
        command = 'bash scripts/ci/core.sh bundle "$PLAN" "$BUNDLE_ID"'
        decoy = source.replace("run: " + command, "run: echo skipped", 1).replace(
            "name: Public core\n", "name: 'Public core " + command + "'\n", 1)
        candidate.write_text(decoy, encoding="utf-8")
        expect_coverage_validator_rejection("decoy shard command", formal_workflow_path=candidate)
        for option in ("if: false", "continue-on-error: true"):
            candidate.write_text(source.replace(
                "      - run: " + command,
                "      - " + option + "\n        run: " + command, 1), encoding="utf-8")
            expect_coverage_validator_rejection("disabled shard step: " + option,
                                               formal_workflow_path=candidate)

    shell_mutations = (
        source.replace("      - run: " + command,
                       "      - shell: /usr/bin/true {0}\n        run: " + command, 1),
        source.replace("  test:\n", "  test:\n    defaults:\n      run:\n        shell: /usr/bin/true {0}\n", 1),
        source.replace("jobs:\n", "defaults:\n  run:\n    shell: /usr/bin/true {0}\njobs:\n", 1),
        source.replace("      - env:\n          EVENT:",
                       "      - shell: /usr/bin/true {0}\n        env:\n          EVENT:", 1),
    )
    shell_mutations += tuple(mutation.replace("shell: /usr/bin/true {0}",
                                               "working-directory: /tmp/other-checkout")
                             for mutation in shell_mutations[:3])
    with tempfile.TemporaryDirectory() as directory:
        candidate = Path(directory) / "ci-policy.yml"
        for index, mutation in enumerate(shell_mutations):
            if mutation == source:
                raise AssertionError("shell override mutation target drifted")
            candidate.write_text(mutation, encoding="utf-8")
            expect_coverage_validator_rejection(f"shell override {index}", formal_workflow_path=candidate)


def counterexampled(run: quint_apalache.QuintRun) -> bool:
    """A mutation canary passes only on a real counterexample for the final attempt.

    Either Quint printed the counterexample (`[violation]` and `error: found a
    counterexample`), or the Apalache server that ran the check logged its
    counterexample outcome (`The outcome is: Error`) and Quint's transport
    failed before it could print the trace. A NoError outcome, a stall, a
    timeout, or a transport error without a server verdict never counts.
    """

    if run.returncode == 0 or not run.attempts:
        return False
    final = run.attempts[-1]
    if final.classification != "verdict":
        return False
    quint_trace = "error: found a counterexample" in run.output and "[violation]" in run.output
    return quint_trace or "Error" in final.server_outcomes


def run_quint_mutation(
    argv: list[str],
    *,
    cwd: Path,
    label: str,
    timeout_seconds: float = quint_apalache.DEFAULT_ATTEMPT_TIMEOUT_SECONDS,
) -> quint_apalache.QuintRun:
    """Run a mutated model under the shared Quint/Apalache execution policy.

    A counterexample is a verdict and is never retried; only a server-side
    startup/connection/deadline failure or a lost request is retried once.
    """

    environment = os.environ.copy()
    environment["PATH"] = f"{QUINT.parent}:{environment.get('PATH', '')}"
    environment["QUINT_HOME"] = str(VERIFICATION_TOOL_ROOT / "quint-home")
    try:
        return quint_apalache.run_quint_apalache(
            argv,
            cwd=cwd,
            environment=environment,
            timeout_seconds=timeout_seconds,
            label=label,
            emit=lambda line: print(line, file=sys.stderr, flush=True)
            if line.startswith(("[attempt", "[retry", "[timeout", "[apalache"))
            else None,
        )
    except quint_apalache.HarnessError as error:
        raise AssertionError(f"{label}: Quint/Apalache harness failed: {error}") from error


FAKE_APALACHE_SERVER = """#!/usr/bin/env python3
import socket, sys, threading
port = int(next(a for a in sys.argv if a.startswith("--port=")).split("=", 1)[1])
listener = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
listener.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
listener.bind(("::", port))
listener.listen(8)
print(f"The Apalache server is running on port {port}.", flush=True)
def serve(connection):
    with connection:
        data = b""
        while chunk := connection.recv(65536):
            data += chunk
    if data:
        sys.stdout.write(data.decode()); sys.stdout.flush()
while True:
    threading.Thread(target=serve, args=(listener.accept()[0],), daemon=True).start()
"""

FAKE_QUINT = """#!/usr/bin/env python3
import json, os, pathlib, socket, sys, time
argv = sys.argv[1:]
endpoint = argv[argv.index("--server-endpoint") + 1]
out = pathlib.Path(argv[argv.index("--out") + 1]) if "--out" in argv else None
state = pathlib.Path(os.environ["FAKE_QUINT_STATE"])
attempt = int(state.read_text()) if state.exists() else 0
state.write_text(str(attempt + 1))
plan = json.loads(os.environ["FAKE_QUINT_PLAN"])[attempt]
if plan.get("server_lines"):
    host, port = endpoint.rsplit(":", 1)
    with socket.create_connection((host, int(port))) as connection:
        connection.sendall(("\\n".join(plan["server_lines"]) + "\\n").encode())
    time.sleep(0.5)
if out is not None and plan.get("out_json") is not None:
    out.write_text(json.dumps(plan["out_json"]))
time.sleep(plan.get("sleep", 0))
print(plan.get("output", ""), flush=True)
sys.exit(plan.get("exit", 0))
"""


def quint_apalache_retry_canaries() -> None:
    """The one retry must never turn a checker verdict into a pass (#2830).

    Drives the real harness loop against a fake Apalache server and a fake
    Quint client whose per-attempt behavior is scripted. A second attempt that
    would pass is always available, so any unsound retry shows up as a pass.
    """

    success = {
        "server_lines": ["PASS #0: SanyParser", "The outcome is: NoError"],
        "out_json": {"stage": "verifying", "status": "ok", "errors": []},
        "output": "[ok] No violation found.",
        "exit": 0,
    }
    cases = (
        (
            "timeout after the checker started is unknown and never retried",
            [{"server_lines": ["PASS #0: SanyParser"], "sleep": 30}, success],
            False,
        ),
        (
            "transport error after a server-logged Error outcome",
            [
                {
                    "server_lines": ["PASS #0: SanyParser", "The outcome is: Error"],
                    "output": "error: 12 UNIMPLEMENTED: No message received",
                    "exit": 1,
                },
                success,
            ],
            False,
        ),
        (
            "violation status with empty errors beside deadline text",
            [
                {
                    "out_json": {"stage": "verifying", "status": "violation", "errors": []},
                    "output": "error: 4 DEADLINE_EXCEEDED: Deadline exceeded after 5.000s",
                    "exit": 1,
                },
                success,
            ],
            False,
        ),
        (
            "failure status with empty errors after a server-logged Error outcome",
            [
                {
                    "server_lines": ["PASS #0: SanyParser", "The outcome is: Error"],
                    "out_json": {"stage": "verifying", "status": "failure", "errors": []},
                    "exit": 1,
                },
                success,
            ],
            False,
        ),
        (
            "control: transport error after a server-logged NoError outcome is retried",
            [
                {
                    "server_lines": ["PASS #0: SanyParser", "The outcome is: NoError"],
                    "output": "error: 12 UNIMPLEMENTED: No message received",
                    "exit": 1,
                },
                success,
            ],
            True,
        ),
        (
            "control: reflection deadline before any check is retried",
            [
                {
                    "output": "error: Error querying reflection endpoint: Error: 4 DEADLINE_EXCEEDED",
                    "exit": 1,
                },
                success,
            ],
            True,
        ),
    )
    for label, plan, should_pass in cases:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            launcher = (
                root
                / "quint-home"
                / f"apalache-dist-{quint_apalache.APALACHE_VERSION}"
                / "apalache/bin/apalache-mc"
            )
            launcher.parent.mkdir(parents=True)
            launcher.write_text(FAKE_APALACHE_SERVER, encoding="utf-8")
            launcher.chmod(0o755)
            fake_quint = root / "bin/quint"
            fake_quint.parent.mkdir()
            fake_quint.write_text(FAKE_QUINT, encoding="utf-8")
            fake_quint.chmod(0o755)
            environment = os.environ.copy()
            environment["FAKE_QUINT_PLAN"] = json.dumps(plan)
            environment["FAKE_QUINT_STATE"] = str(root / "attempts")
            try:
                run = quint_apalache.run_quint_apalache(
                    [str(fake_quint), "verify", "model.qnt", "--out", str(root / "result.json")],
                    cwd=root,
                    environment=environment,
                    timeout_seconds=5.0 if label.startswith("timeout after") else 30,
                    quint_home=root / "quint-home",
                    label=label,
                    emit=lambda _line: None,
                )
            except quint_apalache.HarnessError as error:
                raise AssertionError(f"retry canary could not run: {label}: {error}") from error
        classes = [attempt.classification for attempt in run.attempts]
        if label.startswith("timeout after"):
            if run.returncode != 124 or classes != ["timeout"] or "[unknown]" not in run.output:
                raise AssertionError(f"timeout was retried or accepted as evidence: {classes}")
            if not run.attempts[0].check_started:
                raise AssertionError("timeout canary did not reach PASS #0")
        elif should_pass:
            if run.returncode != 0 or len(run.attempts) != 2:
                raise AssertionError(f"retry control did not recover once: {label}: {classes}")
            if counterexampled(run):
                raise AssertionError(f"a passing run counted as a counterexample: {label}")
        elif run.returncode == 0 or len(run.attempts) != 1 or classes != ["verdict"]:
            raise AssertionError(f"checker verdict was retried or passed: {label}: {classes}")
        elif ("server-logged Error" in label) != counterexampled(run):
            raise AssertionError(f"counterexample attribution is wrong: {label}")


def ontology_quint_semantic_mutation_canaries() -> None:
    if not QUINT.is_file():
        raise AssertionError(f"pinned Quint executable is unavailable: {QUINT}")

    mutations = (
        (
            "mapping rejection changes confidence",
            ONTOLOGY_MAPPING_MODEL,
            "    replacementConfidence' = replacementConfidence,\n"
            "    replacementReviewed' = replacementReviewed,\n"
            "    captureRejectedState,",
            "    replacementConfidence' = 9999,\n"
            "    replacementReviewed' = replacementReviewed,\n"
            "    captureRejectedState,",
        ),
        (
            "relation rejection changes the first endpoint",
            ONTOLOGY_RELATION_MODEL,
            "  action rejectStaleVersion = all {\n"
            "    activeVersion' = activeVersion,\n"
            "    cardinality' = cardinality,\n"
            "    firstExists' = firstExists,\n"
            "    secondExists' = secondExists,\n"
            "    firstFrom' = firstFrom,",
            "  action rejectStaleVersion = all {\n"
            "    activeVersion' = activeVersion,\n"
            "    cardinality' = cardinality,\n"
            "    firstExists' = firstExists,\n"
            "    secondExists' = secondExists,\n"
            "    firstFrom' = 2,",
        ),
    )
    for label, model, original, mutation in mutations:
        source = model.read_text(encoding="utf-8")
        if source.count(original) != 1:
            raise AssertionError(f"Quint semantic canary target drifted: {label}")
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / model.name
            candidate.write_text(source.replace(original, mutation), encoding="utf-8")
            completed = run_quint_mutation(
                [
                    str(QUINT),
                    "verify",
                    str(candidate),
                    "--backend",
                    "apalache",
                    "--invariants",
                    "rejectedAttemptIsNoOp",
                    "--max-steps",
                    "2",
                ],
                cwd=Path(directory),
                label=f"ontology canary: {label}",
            )
        output = completed.output
        if not counterexampled(completed):
            raise AssertionError(
                f"Quint rejection semantic mutation was not counterexampled: {label}; "
                f"exit={completed.returncode}; output={output[-1200:]!r}"
            )


def selector_gate_order_canaries() -> None:
    validator = load_module("norn_selector_order_canary", VALIDATOR)
    gates = json.loads(GATES.read_text(encoding="utf-8"))["gates"]
    registrations = re.findall(
        r"^run_gate (fast|full) ([a-z0-9][a-z0-9-]*)\b",
        validator.PROMOTED_RUNNER.read_text(encoding="utf-8"), re.MULTILINE,
    )
    validator.validate_selector_gate_order(registrations, gates)
    for profile, gate_id in (
        ("full", "verification-executed-test-selectors"),
        ("fast", "verification-executed-test-selectors-fast"),
    ):
        producer = {"id": "selector-order-canary-producer", "profile": profile,
                    "evidenceKind": "rust-tests"}
        baseline = [(profile, producer["id"]), *registrations]
        validator.validate_selector_gate_order(baseline, [*gates, producer])
        candidate = [(profile, gate_id)] + [
            item for item in baseline if item[1] != gate_id
        ]
        try:
            validator.validate_selector_gate_order(candidate, [*gates, producer])
        except validator.InvalidCoverage:
            continue
        raise AssertionError(f"{profile} selector check accepted before its producers")


def gate_binding_canaries() -> None:
    writer = load_module("norn_write_verification_result_canary", WRITER)
    result_validator = load_module("norn_validate_result_canary", RESULT_VALIDATOR)
    expected = [
        {
            "id": "alpha",
            "command": "check alpha",
            "evidenceKind": "command",
            "role": "supporting-check",
            "classifications": [{"subsystem": "kernel-v2", "layer": "runtime"}],
            "resultArtifacts": [],
            "generatedArtifacts": [],
        },
        {
            "id": "beta",
            "command": "check beta",
            "evidenceKind": "command",
            "role": "typecheck",
            "classifications": [{"subsystem": "kernel-v2", "layer": "model"}],
            "resultArtifacts": ["target/verification-results/beta.json"],
            "generatedArtifacts": [],
        },
    ]
    executed = [
        {
            "id": gate["id"],
            "command": gate["command"],
            "executedCommand": gate["command"]
            + (
                f" --out {gate['resultArtifacts'][0]}"
                if gate["resultArtifacts"]
                else ""
            ),
            "evidenceKind": gate["evidenceKind"],
        }
        for gate in expected
    ]
    bound = writer.bind_promoted_gate_classification(copy.deepcopy(executed), {"gates": expected})
    if [gate["id"] for gate in bound] != ["alpha", "beta"]:
        raise AssertionError("positive writer gate binding changed order or identity")

    expect_value_error(
        "writer missing gate",
        lambda: writer.bind_promoted_gate_classification(copy.deepcopy(executed[:-1]), {"gates": expected}),
    )
    changed = copy.deepcopy(executed)
    changed[0]["command"] = "check attacker"
    expect_value_error(
        "writer command substitution",
        lambda: writer.bind_promoted_gate_classification(changed, {"gates": expected}),
    )
    changed_output = copy.deepcopy(executed)
    changed_output[1]["executedCommand"] = "check beta --out target/verification-results/attacker.json"
    expect_value_error(
        "writer output substitution",
        lambda: writer.bind_promoted_gate_classification(changed_output, {"gates": expected}),
    )

    result_gates = []
    for gate in expected:
        result_gate = copy.deepcopy(gate)
        result_gate.update(
            {
                "executedCommand": gate["command"]
                + (
                    f" --out {gate['resultArtifacts'][0]}"
                    if gate["resultArtifacts"]
                    else ""
                ),
                "observedTestCount": 0,
                "result": "pass",
                "log": f"target/verification-results/logs/{gate['id']}.log",
                "logSha256": "0" * 64,
            }
        )
        result_gates.append(result_gate)
    result_validator.reconcile_result_gates(expected, result_gates)
    changed_result_output = copy.deepcopy(result_gates)
    changed_result_output[1]["executedCommand"] = (
        "check beta --out target/verification-results/attacker.json"
    )
    expect_value_error(
        "result executed output substitution",
        lambda: result_validator.reconcile_result_gates(expected, changed_result_output),
    )
    result_gates[1]["resultArtifacts"] = []
    expect_value_error(
        "result artifact ownership substitution",
        lambda: result_validator.reconcile_result_gates(expected, result_gates),
    )
    if writer.canonical_command("env NORN_MODE=safe check alpha") == writer.canonical_command(
        "env NORN_MODE=attacker check alpha"
    ):
        raise AssertionError("command identity erased a semantic env assignment")

    previous = os.environ.get("NORN_VERIFICATION_CENTRAL_OBLIGATIONS")
    os.environ["NORN_VERIFICATION_CENTRAL_OBLIGATIONS"] = "/tmp/attacker-registry.json"
    try:
        expect_value_error(
            "result writer ambient canonical-input substitution",
            writer.reject_ambient_input_overrides,
        )
    finally:
        if previous is None:
            os.environ.pop("NORN_VERIFICATION_CENTRAL_OBLIGATIONS", None)
        else:
            os.environ["NORN_VERIFICATION_CENTRAL_OBLIGATIONS"] = previous


def proof_hole_canaries() -> None:
    checker = load_module("norn_proof_hole_checker_canary", PROOF_HOLE_CHECKER)
    if checker.lean_proof_hole_locations(
        '-- constant native_decide unsafe ignored\n'
        '/- axiom implemented_by opaque ignoredToo -/\n'
        'def quoted := "constant native_decide implemented_by unsafe opaque"\n'
    ):
        raise AssertionError("clean Lean comments/strings were classified as proof holes")
    if not checker.lean_proof_hole_locations("constant forgedProof : False\n"):
        raise AssertionError("negative canary did not detect Lean constant axiom")
    for construct in (
        "example : True := by native_decide\n",
        "@[implemented_by alternate] def forged : True := True.intro\n",
        "unsafe def forged : True := True.intro\n",
        "opaque forged : True := True.intro\n",
    ):
        if not checker.lean_proof_hole_locations(construct):
            raise AssertionError(
                f"negative canary did not detect Lean trust escape {construct.strip()!r}"
            )
    if checker.extraction_admission_present("hax into fstar"):
        raise AssertionError("clean extraction command was classified as admitting SMT queries")
    for flag in ("--admit_smt_query", "--admit_smt_queries"):
        if not checker.extraction_admission_present(f"hax into fstar {flag} true"):
            raise AssertionError(f"negative canary did not detect forbidden flag {flag}")


def selector_declaration_canaries() -> None:
    helper = load_module("norn_verification_attestation_canary", ATTESTATION_HELPER)
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        lean = root / "Canary.lean"
        lean.write_text(
            "namespace Canary\n"
            "theorem real_theorem : True := by exact True.intro\n"
            "-- theorem comment_only : True := by exact True.intro\n"
            'def quoted := "theorem string_only : True"\n'
            "def inner.dotted_def := 0\n"
            "def _root_.RootOnly.escaped := 0\n"
            "end Canary\n",
            encoding="utf-8",
        )
        if not helper.selector_is_declared(lean, "theorem:Canary.real_theorem"):
            raise AssertionError("real Lean declaration was not recognized")
        for real in ("predicate:Canary.inner.dotted_def", "predicate:RootOnly.escaped"):
            if not helper.selector_is_declared(lean, real):
                raise AssertionError(f"namespaced dotted Lean declaration was not recognized: {real}")
        if helper.selector_is_declared(lean, "predicate:Canary.RootOnly.escaped"):
            raise AssertionError("_root_-escaped Lean declaration was qualified with its namespace")
        for forged in ("theorem:Canary.comment_only", "theorem:Canary.string_only"):
            if helper.selector_is_declared(lean, forged):
                raise AssertionError(f"comment/string-only Lean selector passed: {forged}")

        fstar = root / "Canary.fst"
        fstar.write_text(
            "module Canary\n"
            "let real_theorem (): Lemma (ensures True) = ()\n"
            "// let comment_only (): Lemma (ensures True) = ()\n"
            'let quoted = "let string_only (): Lemma (ensures True) = ()"\n',
            encoding="utf-8",
        )
        if not helper.selector_is_declared(fstar, "theorem:Canary.real_theorem"):
            raise AssertionError("real F* declaration was not recognized")
        for forged in ("theorem:Canary.comment_only", "theorem:Canary.string_only"):
            if helper.selector_is_declared(fstar, forged):
                raise AssertionError(f"comment/string-only F* selector passed: {forged}")

        quint = root / "canary.qnt"
        quint.write_text(
            "module Canary {\n"
            "  def realInvariant = true\n"
            "  // def commentInvariant = true\n"
            '  def quoted = "def stringInvariant = true"\n'
            "}\n",
            encoding="utf-8",
        )
        if not helper.selector_is_declared(quint, "invariant:realInvariant"):
            raise AssertionError("real Quint definition was not recognized")
        for forged in ("invariant:commentInvariant", "invariant:stringInvariant"):
            if helper.selector_is_declared(quint, forged):
                raise AssertionError(f"comment/string-only Quint selector passed: {forged}")

        unsupported = root / "canary.txt"
        unsupported.write_text("theorem forged : True", encoding="utf-8")
        if helper.selector_is_declared(unsupported, "theorem:forged"):
            raise AssertionError("unsupported selector/artifact pairing did not fail closed")

        rust = root / "canary.rs"
        rust.write_text(
            "pub const fn real_predicate() -> bool { true }\n"
            "fn caller() { forged_call_only(); }\n"
            "// fn comment_only() {}\n"
            'const QUOTED: &str = "fn string_only() {}";\n',
            encoding="utf-8",
        )
        if not helper.selector_is_declared(rust, "predicate:real_predicate"):
            raise AssertionError("real Rust predicate declaration was not recognized")
        for forged in ("predicate:forged_call_only", "predicate:comment_only", "predicate:string_only"):
            if helper.selector_is_declared(rust, forged):
                raise AssertionError(f"non-declaration Rust predicate selector passed: {forged}")

        llbc = root / "canary.llbc"
        llbc.write_bytes(b"binary")
        (root / "lean").mkdir()
        (root / "lean/translation.json").write_text(
            json.dumps({"functions": [{"rust_name": "crate::real_predicate"}]}),
            encoding="utf-8",
        )
        if not helper.selector_is_declared(llbc, "predicate:crate::real_predicate"):
            raise AssertionError("LLBC selector did not bind through its translation manifest")
        if helper.selector_is_declared(llbc, "predicate:crate::forged_predicate"):
            raise AssertionError("LLBC selector ignored its translation manifest")


def source_tree_canaries() -> None:
    helper = load_module("norn_verification_source_tree_canary", ATTESTATION_HELPER)
    with tempfile.TemporaryDirectory() as directory:
        source = Path(directory)
        subprocess.run(["git", "init", "--quiet", str(source)], check=True)
        helper.require_clean_source_tree(source, label="canary")
        (source / "untracked-proof-substitution").write_text("forged", encoding="utf-8")
        expect_value_error(
            "dirty pinned source tree",
            lambda: helper.require_clean_source_tree(source, label="canary"),
        )


def workspace_identity_canaries() -> None:
    helper = load_module("norn_verification_workspace_canary", ATTESTATION_HELPER)
    result_validator = load_module(
        "norn_validate_result_workspace_canary", RESULT_VALIDATOR
    )
    with tempfile.TemporaryDirectory() as directory:
        source = Path(directory)
        subprocess.run(["git", "init", "--quiet", str(source)], check=True)
        subprocess.run(
            ["git", "config", "user.email", "verification-canary@norn.invalid"],
            cwd=source,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Norn verification canary"],
            cwd=source,
            check=True,
        )
        tracked = source / "tracked.txt"
        tracked.write_text("canonical\n", encoding="utf-8")
        subprocess.run(["git", "add", "tracked.txt"], cwd=source, check=True)
        subprocess.run(
            ["git", "commit", "--quiet", "-m", "canary"], cwd=source, check=True
        )
        clean = helper.collect_git_identity(source)
        tracked.write_text("substituted\n", encoding="utf-8")
        changed = helper.collect_git_identity(source)
        if clean["workspaceStateSha256"] == changed["workspaceStateSha256"]:
            raise AssertionError("tracked workspace mutation did not change attested identity")
        expect_value_error(
            "result workspace digest substitution",
            lambda: result_validator.reconcile_git_identity(clean, changed),
        )
        tracked.write_text("canonical\n", encoding="utf-8")
        (source / "untracked.txt").write_text("hidden input\n", encoding="utf-8")
        untracked = helper.collect_git_identity(source)
        if clean["workspaceStateSha256"] == untracked["workspaceStateSha256"]:
            raise AssertionError("untracked workspace mutation did not change attested identity")


def evidence_inventory_canaries() -> None:
    writer = load_module("norn_write_verification_result_evidence_canary", WRITER)
    coverage_path = coverage_manifest.COVERAGE
    coverage = coverage_manifest.load(coverage_path)
    paths = set(writer.evidence_paths(coverage, coverage_path, []))
    selector_binder = "scripts/validate-executed-test-selectors.py"
    if selector_binder not in paths:
        raise AssertionError(
            "executed-test selector binder is absent from content-addressed evidence"
        )
    helper = "scripts/verification_attestation.py"
    if helper not in paths:
        raise AssertionError("shared attestation helper is absent from content-addressed evidence")
    if "scripts/coverage_manifest.py" not in paths:
        raise AssertionError("coverage manifest loader is absent from content-addressed evidence")

    result_validator = load_module("norn_validate_result_inventory_canary", RESULT_VALIDATOR)
    result_validator.reconcile_evidence_paths(paths, set(paths))
    omitted = set(paths)
    omitted.remove(helper)
    expect_value_error(
        "result omitted declared evidence",
        lambda: result_validator.reconcile_evidence_paths(paths, omitted),
    )
    result_validator.reconcile_tool_versions({"lean": "current"}, {"lean": "current"})
    expect_value_error(
        "result mutated recorded tool version",
        lambda: result_validator.reconcile_tool_versions(
            {"lean": "attacker-controlled"}, {"lean": "current"}
        ),
    )


def rights_recovery_evidence(head: str, base: str, group_head: str, tree: str) -> dict:
    if not all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (head, base, group_head, tree)):
        raise ValueError("full lowercase commit/tree identities required")
    public = yaml.safe_load((ROOT / ".github/workflows/ci-policy.yml").read_text())
    group = yaml.safe_load((ROOT / ".github/workflows/trusted-merge-group.yml").read_text())
    def program(job: dict, name: str) -> str:
        return next(step["with"]["script"] for step in job["steps"] if step.get("name") == name)
    pr_job, group_job = public["jobs"]["trusted-policy"], group["jobs"]["trusted-policy"]
    programs = {
        "capture": public["jobs"]["rights-capture"]["steps"][0]["with"]["script"],
        "writer": public["jobs"]["rights-revocation"]["steps"][0]["with"]["script"],
        "pr": program(pr_job, "Record admin approvals of the exact PR head"),
        "group": program(group_job, "Record admin approvals of the queued heads"),
        "publisher": program(pr_job, "Report trusted policy on the exact PR head"),
        "groupPublisher": program(group_job, "Report trusted policy on the group head"),
    }
    replacements = {"${{ steps.pr.outputs.number }}": "7", "${{ steps.pr.outputs.head_sha }}": head,
                    "${{ steps.pr.outputs.base_sha }}": base,
                    "${{ steps.pr.outputs.head_repository }}": "Norn-cloud/norn",
                    "${{ steps.group.outputs.base_sha }}": base,
                    "${{ github.event.workflow_run.head_sha }}": group_head,
                    "${{ job.status }}": "failure"}
    programs["prSuccess"] = programs["publisher"].replace("${{ job.status }}", "success")
    programs["groupSuccess"] = programs["groupPublisher"].replace("${{ job.status }}", "success")
    programs["prB"] = programs["pr"].replace("${{ steps.pr.outputs.number }}", "8")
    for key, script in programs.items():
        for before, after in replacements.items(): script = script.replace(before, after)
        programs[key] = script
    harness = r"""(async function harnessMain() {
  const assert=require('assert'), fs=require('fs');
  const {head,base,groupHead,tree,programs}=JSON.parse(process.env.RIGHTS_CASE);
  const context={repo:{owner:'Norn-cloud',repo:'norn'},payload:{},runId:105,serverUrl:'https://github.com'};
  process.env.GITHUB_WORKFLOW_SHA='e'.repeat(40);
  const path=process.env.RUNNER_TEMP+'/maintainer-approval.json', evidence={};
  let runs,jobs,history,live,payloads,clock,nextRun,modeShared,unavailable=false,readFailure=false,paging=false,shifted=false;
  let outputs={}, admission=0, groupAdmission=0, virtualNow=0;
  let objects,retained,objectCounter,jobReads=0,largeHistory=false,releasing=0,releaseReads=0;
  Date.now=()=>virtualNow;
  globalThis.setTimeout=(callback,ms)=>{virtualNow+=ms;queueMicrotask(callback);return 0};
  const core={setOutput:(name,value)=>{outputs[name]=value},warning:()=>{},setFailed:()=>{}};
  const oid=value=>{const sha=(++objectCounter).toString(16).padStart(40,'0');objects.set(sha,value);return sha};
  const execute=code=>new (Object.getPrototypeOf(async function(){}).constructor)('github','context','core',code)(github,context,core);
  const prA={number:7,head:{sha:head,repo:{full_name:'Norn-cloud/norn'}},base:{sha:base}};
  const prB={...prA,number:8};
  const makeRun=(id,event='issue_comment',epoch='protocol')=>({id,run_number:id,run_attempt:1,event,head_sha:epoch,
    status:'completed',created_at:new Date(virtualNow).toISOString(),run_started_at:new Date(virtualNow).toISOString(),head_repository:{full_name:'Norn-cloud/norn'},repository:{full_name:'Norn-cloud/norn'}});
  const github={rest:{repos:{
    getCollaboratorPermissionLevel:async({username})=>({data:{permission:username==='owner'?'admin':'read'}}),
    createCommitStatus:async value=>{
      if(unavailable)throw {status:503};
      history.push({...value,id:history.length+1,created_at:clock,creator:{login:'github-actions[bot]'}});
    },
    listCommitStatusesForRef:async({ref,page=1})=>({data:history.filter(row=>row.sha===ref).slice().reverse().slice((page-1)*100,page*100)}),
    getContent:async({ref,path})=>{
      let content;
      if(objects.has(ref)){
        const commit=objects.get(ref),files=objects.get(commit.tree.sha);
        if(!files.has(path))throw {status:404};
        content=objects.get(files.get(path));
      }else content=ref==='legacy'?'name: Public core':'rights-authority-v4';
      return {data:{type:'file',encoding:'base64',content:Buffer.from(content).toString('base64')}};
    },
    compareCommits:async()=>({data:{status:'ahead'}}),
    getCommit:async({ref})=>({data:{commit:{tree:{sha:tree}},parents:ref===groupHead?[{sha:base},{sha:head}]:[{sha:base}]}}),
    listPullRequestsAssociatedWithCommit:async()=>({data:[modeShared?prB:prA]})
  },git:{
    createBlob:async({content})=>({data:{sha:oid(content)}}),
    createTree:async({base_tree,tree})=>{const files=new Map(base_tree?objects.get(base_tree):[]);for(const row of tree)files.set(row.path,row.sha);return {data:{sha:oid(files)}}},
    createCommit:async({tree,parents})=>{const data={tree:{sha:tree},parents:parents.map(sha=>({sha}))};const sha=oid(data);data.sha=sha;return {data}},
    getCommit:async({commit_sha})=>({data:objects.get(commit_sha)}),
    getRef:async()=>{if(!retained)throw {status:404};return {data:{object:{sha:retained}}}},
    createRef:async({sha})=>{assert(!retained);retained=sha},
    updateRef:async({sha,force})=>{assert.equal(force,false);assert(objects.get(sha).parents.some(p=>p.sha===retained));retained=sha}
  },pulls:{get:async()=>{throw Error('rights must not use a second PR snapshot')}},
  issues:{listComments:function comments(){},getComment:async({comment_id})=>{if(!live.has(comment_id))throw {status:404};return {data:live.get(comment_id)}}},
  actions:{
    getWorkflowRun:async({run_id})=>{const run=runs.find(r=>r.id===run_id);if(!run)throw {status:404};return {data:run}},
    getWorkflowRunAttempt:async({run_id,attempt_number})=>{const run=runs.find(r=>r.id===run_id);assert.equal(run?.run_attempt,attempt_number);return {data:run}},
    listWorkflowRuns:async({page})=>{
      if(readFailure)throw {status:503};
      if(paging&&page===2&&!shifted){runs.unshift(makeRun(nextRun++,'push'));shifted=true}
      const size=paging?2:100;
      return {data:{workflow_runs:runs.slice((page-1)*size,page*size)}};
    },
    listJobsForWorkflowRunAttempt:function attemptJobs(){}
  }},
  paginate:async(method,args)=>{
    if(method===github.rest.repos.listCommitStatusesForRef)return history.filter(row=>row.sha===args.ref).slice().reverse();
    if(method===github.rest.actions.listJobsForWorkflowRunAttempt){
      jobReads++;
      if(args.run_id===releasing&&++releaseReads===2){
        const value=jobs.get(releasing).get(1);
        value[0].status='completed';value[1].status='completed';value[1].conclusion='success';
        value[1].name='Rights ledger none comment 13';
        runs.find(r=>r.id===releasing).status='completed';
      }
      return jobs.get(args.run_id)?.get(args.attempt_number)||[];
    }
    assert.equal(method,github.rest.issues.listComments);return [];
  }};
  const payload=(comment,action='created',pull=true,prior,number=7)=>({action,
    issue:{number:pull?number:42,...(pull?{pull_request:{url:'synthetic'}}:{})},comment,
    ...(prior?{changes:{body:{from:prior}}}:{})});
  const comment=(id,body,created)=>({id,user:{login:'owner'},body,created_at:created,updated_at:created});
  async function record(id,p,attempt=1,fail=false) {
    context.payload=p;context.runId=id;outputs={};virtualNow=Math.max(virtualNow,Date.parse(clock));
    process.env.GITHUB_RUN_ATTEMPT=String(attempt);process.env.GITHUB_TRIGGERING_ACTOR='owner';
    await execute(programs.capture);
    const data=JSON.parse(outputs.payload);
    if(!payloads.has(id)){payloads.set(id,p);runs.unshift(makeRun(id));nextRun=Math.max(nextRun,id+1)}
    runs.find(r=>r.id===id).run_attempt=attempt;
    if(attempt>1)runs.find(r=>r.id===id).run_started_at=new Date(virtualNow).toISOString();
    const possible=[p.comment.body,p.changes?.body?.from].some(body=>(body||'').includes('/ack-rights')||(body||'').includes('/revoke-rights'));
    const capture={id:id*10+attempt,name:'Capture immutable rights event '+outputs.kind+' '+(possible?'possible':'none'),status:'completed',conclusion:'success',
      steps:[{name:'Rights event '+outputs.heads+' comment '+outputs.comment,status:'completed',conclusion:'success'}]};
    const journal={id:id*10+attempt+1,name:'Rights ledger '+outputs.heads+' comment '+outputs.comment,status:'completed',conclusion:'success'};
    if(!jobs.has(id))jobs.set(id,new Map());
    jobs.get(id).set(attempt,[capture,journal]);
    if(outputs.kind==='issue'){
      journal.name='Rights ledger ${{ needs.rights-capture.outputs.heads }} comment ${{ needs.rights-capture.outputs.comment }}';
      journal.conclusion='skipped';return;
    }
    process.env.RIGHTS_EVENT=outputs.payload;
    unavailable=fail;
    if(fail){
      for(let retry=0;retry<4;retry++)await assert.rejects(()=>execute(programs.writer));
      journal.conclusion='failure';
    } else await execute(programs.writer);
    unavailable=false;
  }
  async function collect(kind) {
    context.runId=admission;context.payload=kind==='group'?{workflow_run:{id:groupAdmission}}:{};
    process.env.GITHUB_RUN_ATTEMPT='1';
    jobReads=0;const hadCheckpoint=!!retained;
    await execute(programs[kind==='pr'?modeShared?'prB':'pr':'group']);
    if(largeHistory&&hadCheckpoint)assert(jobReads<50,'checkpoint must bound ordinary-comment history');
    return JSON.parse(fs.readFileSync(path));
  }
  async function snapshot(name) {
    virtualNow=Math.max(virtualNow,Date.parse(clock));admission=nextRun++;groupAdmission=nextRun++;runs.unshift(makeRun(groupAdmission,'merge_group'),makeRun(admission,'pull_request_target'));
    const pr=await collect('pr'),group=await collect('group');
    context.runId=admission;context.payload={};await execute(programs.pr);
    const a=JSON.parse(fs.readFileSync(path));
    const substantive=doc=>Object.fromEntries(Object.entries(doc.rights).map(([head,trees])=>[head,
      Object.values(trees).map(row=>({head_sha:row.head_sha,tree_sha:row.tree_sha,pending:row.pending,entries:row.entries}))]));
    assert.deepEqual(substantive(a),substantive(pr));assert.deepEqual(substantive(group),substantive(pr));
    evidence[name]={pr,group,a};
  }
  function pending(doc){return doc.rights[head][tree].pending}
  async function publishFailure(){context.runId=admission;context.payload={};await execute(programs.publisher)}
  async function publishSaved(kind,doc,runId,groupId){
    context.runId=runId;context.payload=kind==='group'?{workflow_run:{id:groupId}}:{};
    process.env.GITHUB_RUN_ATTEMPT='1';
    fs.writeFileSync(process.env.RUNNER_TEMP+'/rights-parameters.json',JSON.stringify(
      {kind:kind==='group'?'group':'pr',head:kind==='group'?groupHead:head,base,repository:'Norn-cloud/norn',number:modeShared?8:7}));
    fs.writeFileSync(process.env.RUNNER_TEMP+'/rights-decision.json',JSON.stringify(doc.publication));
    await execute(kind==='group'?programs.groupSuccess:programs.prSuccess);
    const row=history.filter(row=>row.sha===(kind==='group'?groupHead:head)&&row.context==='trusted-policy').at(-1);
    assert.equal(row.state,'failure','stale or expired saved success must never replace failure');
  }

  for(const layout of ['single','shared'])for(const removal of ['edited','deleted']){
    modeShared=layout==='shared';history=[];live=new Map();jobs=new Map();payloads=new Map();nextRun=101;
    clock='2026-11-03T00:00:00Z';virtualNow=Date.parse(clock);objects=new Map();retained=null;objectCounter=10000;
    largeHistory=layout==='single'&&removal==='edited';
    runs=[makeRun(largeHistory?10000:100,'issue_comment','legacy')];
    if(largeHistory){
      for(let id=10001;id<=20000;id++){
        runs.unshift(makeRun(id));
        jobs.set(id,new Map([[1,[
          {name:'Capture immutable rights event pull none',status:'completed',conclusion:'success'},
          {name:'Rights ledger none comment '+id,status:'completed',conclusion:'success'}]]]));
      }
      nextRun=20001;paging=true;shifted=false;
    }
    const firstRun=nextRun;
    const first=comment(1,'/ack-rights '+head,clock);live.set(1,first);await record(nextRun++,payload(first));
    if(modeShared){clock='2026-11-03T00:00:02Z';const second=comment(2,'/ack-rights '+head,clock);live.set(2,second);await record(nextRun++,payload(second,'created',true,undefined,8))}

    const revoke=comment(9,'/revoke-rights '+head,'2026-11-03T00:01:00Z');
    // Admission and group runs are immutable fences, independent of the source PR.

    const prefix=layout+'-'+removal+'-';
    await snapshot(prefix+'initial');
    if(largeHistory){assert(shifted);paging=false}
    const savedAdmission=admission,savedGroup=groupAdmission,savedInitial=evidence[prefix+'initial'];
    clock='2026-11-03T00:01:00Z';const revokeRun=nextRun++;await record(revokeRun,payload(revoke),1,true);await publishFailure();
    const removed=removal==='edited'?payload({...revoke,body:'ordinary text'},'edited',true,revoke.body):payload(revoke,'deleted');
    const removedRun=nextRun++;await record(removedRun,removed,1,true);await publishFailure();
    runs.sort((a,b)=>b.run_number-a.run_number);
    assert(!history.filter(s=>s.context.toLowerCase().startsWith('rights-ledger/')).some(s=>JSON.parse(s.description).k==='revoke'));
    await publishSaved('pr',savedInitial.pr,savedAdmission,savedGroup);
    await publishSaved('group',savedInitial.group,savedAdmission,savedGroup);
    await snapshot(prefix+'interleaving');
    await snapshot(prefix+'pending');assert.deepEqual(pending(evidence[prefix+'pending'].pr).sort(),[revokeRun,removedRun]);
    clock='2026-11-03T00:01:30Z';
    const premature=comment(3,'/ack-rights '+head,clock);live.set(3,premature);await record(nextRun++,payload(premature,'created',true,undefined,modeShared?8:7));
    await snapshot(prefix+'premature');assert.equal(pending(evidence[prefix+'premature'].pr).length,2);
    clock='2026-11-03T03:02:00Z';await record(revokeRun,payloads.get(revokeRun),2);
    await snapshot(prefix+'partial');assert.deepEqual(pending(evidence[prefix+'partial'].pr),[removedRun]);
    clock='2026-11-03T03:02:01Z';await record(removedRun,payloads.get(removedRun),2);
    await snapshot(prefix+'reconciled');assert.deepEqual(pending(evidence[prefix+'reconciled'].pr),[]);
    clock='2026-11-03T03:03:00Z';
    const fresh=comment(4,'/ack-rights '+head,clock);live.set(4,fresh);await record(nextRun++,payload(fresh,'created',true,undefined,modeShared?8:7));
    await snapshot(prefix+'fresh');
    const freshAdmission=admission,freshGroup=groupAdmission,freshDocs=evidence[prefix+'fresh'];
    const normalNow=virtualNow;virtualNow+=7200001;
    await publishSaved('pr',freshDocs.pr,freshAdmission,freshGroup);
    await publishSaved('group',freshDocs.group,freshAdmission,freshGroup);
    virtualNow=normalNow;
    const waitingRun=nextRun++;
    await record(waitingRun,payload(comment(13,'ordinary mention of /revoke-rights, not a command',clock)));
    const waitingJobs=jobs.get(waitingRun).get(1);
    waitingJobs[0].status='queued';waitingJobs[1].status='queued';
    waitingJobs[1].name='Rights ledger ${{ needs.rights-capture.outputs.heads }} comment ${{ needs.rights-capture.outputs.comment }}';
    runs.find(r=>r.id===waitingRun).status='in_progress';
    releasing=waitingRun;releaseReads=0;
    await snapshot(prefix+'inflight');
    assert(releaseReads>=2,'an in-flight capture must be awaited');
    releasing=0;
    // A failed cursor write leaves its retention commit unpublished. Recovery replays
    // from the last authenticated cursor and advances the retention ref without force.
    admission=nextRun++;groupAdmission=nextRun++;
    runs.unshift(makeRun(groupAdmission,'merge_group'),makeRun(admission,'pull_request_target'));
    unavailable=true;await assert.rejects(()=>collect('pr'));assert(!fs.existsSync(path));unavailable=false;
    await collect('pr');
    // A plain issue with a skipped, unexpanded journal is attributed by the saved capture payload.
    await record(nextRun++,payload(comment(12,'/revoke-rights '+head,clock),'created',false));
    await snapshot(prefix+'plain');assert.deepEqual(pending(evidence[prefix+'plain'].pr),[]);
    paging=true;shifted=false;await snapshot(prefix+'paging');paging=false;
    assert.deepEqual(pending(evidence[prefix+'paging'].pr),[]);
    // The latest recorded acknowledgment remains the frontier after its comment disappears.
    live.delete(4);await snapshot(prefix+'withdrawn');
    live.set(4,fresh);
    history.find(s=>s.context.startsWith('rights-ledger/')).context=
      history.find(s=>s.context.startsWith('rights-ledger/')).context.toUpperCase();
    await snapshot(prefix+'case');
    // Runs created in the same second can list (newest id first) in an order that
    // disagrees with run_number. The walk accepts any order but no missing number.
    const swapA=nextRun++,swapB=nextRun++;
    await record(swapA,payload(comment(14,'ordinary text',clock)));
    await record(swapB,payload(comment(15,'ordinary text',clock)));
    assert.deepEqual(runs.slice(0,2).map(r=>r.id),[swapB,swapA]);
    [runs[0],runs[1]]=[runs[1],runs[0]];
    const swapped=runs.find(r=>r.id===swapA);
    runs=runs.filter(r=>r!==swapped);
    for(const kind of ['pr','group']){await assert.rejects(()=>collect(kind),/Rights history checkpoint unavailable/);assert(!fs.existsSync(path))}
    runs.splice(0,0,swapped);
    await snapshot(prefix+'inverted');
    // A run numbered above the cursor can also list after the cursor run itself (here
    // a revocation whose ledger has not run). Each collector starts below its own one.
    for(const [index,kind] of ['pr','group'].entries()){
      const cursorRun=runs.reduce((a,b)=>b.run_number>a.run_number?b:a), straddle=nextRun++;
      await record(straddle,payload(comment(16+index,'/revoke-rights '+head,clock)),1,true);
      assert.equal(runs[0].id,straddle);
      const moved=runs.shift();runs.splice(runs.indexOf(cursorRun)+1,0,moved);
      // Its id sorts below the cursor run's id, as in a real id-ordered listing
      // (harness ids start at 100, so 50 and 49 are unused).
      const low=50-index;assert(low<cursorRun.id&&!runs.some(r=>r.id===low));jobs.set(low,jobs.get(moved.id));moved.id=low;
      assert(pending(await collect(kind)).includes(low),'a run listed after the cursor run must be read');
    }
    await snapshot(prefix+'straddle');
    // A complete first page is no upper bound: a run numbered above every run seen so
    // far (here a revocation whose ledger has not run) can list on a later page.
    paging=true;shifted=true;
    for(const [index,kind] of ['pr','group'].entries()){
      const lowA=nextRun++,lowB=nextRun++,high=nextRun++,ids=20+index*3;
      await record(lowA,payload(comment(ids,'ordinary text',clock)));
      await record(lowB,payload(comment(ids+1,'ordinary text',clock)));
      await record(high,payload(comment(ids+2,'/revoke-rights '+head,clock)),1,true);
      assert.deepEqual(runs.slice(0,3).map(r=>r.id),[high,lowB,lowA]);
      runs.splice(2,0,runs.shift());
      assert(pending(await collect(kind)).includes(high),'a later-page run above the first page must be read');
    }
    paging=false;
    // A repeated run_number must not stand in for a missing one.
    const n=Math.max(...runs.map(r=>r.run_number));
    const dupes=[makeRun(nextRun++,'push'),makeRun(nextRun++,'push'),makeRun(nextRun++,'push')];
    dupes[0].run_number=dupes[1].run_number=n+1;dupes[2].run_number=n+3;
    runs.unshift(dupes[2],dupes[1],dupes[0]);
    for(const kind of ['pr','group']){await assert.rejects(()=>collect(kind),/Incomplete rights event history/);assert(!fs.existsSync(path))}
    runs=runs.filter(r=>!dupes.includes(r));nextRun-=3;
    const complete=[...runs];
    for(const id of [revokeRun,admission,firstRun]){
      runs=complete.filter(r=>r.id!==id);
      for(const kind of ['pr','group']){await assert.rejects(()=>collect(kind));assert(!fs.existsSync(path))}
    }
    runs=complete;readFailure=true;
    for(const kind of ['pr','group']){await assert.rejects(()=>collect(kind));assert(!fs.existsSync(path))}
    readFailure=false;
    const original=jobs.get(removedRun).get(2);
    jobs.get(removedRun).set(2,[original[0],{...original[1],conclusion:'skipped'}]);
    assert(pending(await collect('pr')).includes(removedRun));
    jobs.get(removedRun).set(2,original);
  }
  console.log(JSON.stringify(evidence));
})().catch(error=>{console.error(error);process.exit(1)});"""
    with tempfile.TemporaryDirectory() as directory:
        result = subprocess.run(["bun", "-e", harness], capture_output=True, text=True,
                                env={**os.environ, "RUNNER_TEMP": directory, "RIGHTS_CASE": json.dumps({
                                    "head": head, "base": base, "groupHead": group_head, "tree": tree, "programs": programs})}, timeout=40)
    if result.returncode:
        raise AssertionError(result.stderr[-4000:])
    return json.loads(result.stdout)


def durable_rights_history_canaries(source: str, merge: str) -> None:
    rights_recovery_evidence("1" * 40, "2" * 40, "3" * 40, "4" * 40)
    print("global target ledger: single/shared SHA recovery, plain-issue payloads and offset paging passed")


def rights_workflow_canaries() -> None:
    coverage_validator = load_module("norn_rights_workflow_canary", VALIDATOR)
    source = (ROOT / ".github/workflows/ci-policy.yml").read_text()
    merge = (ROOT / ".github/workflows/trusted-merge-group.yml").read_text()
    coverage_validator.validate_rights_contract(source, merge)
    for label, before, after in [
        ("missing rights evidence", '--rights-evidence "$RUNNER_TEMP/maintainer-approval.json"', ""),
        ("stale PR binding", '--head "${{ steps.pr.outputs.head_sha }}"', '--head "old"'),
        ("missing revocation", "(ack|revoke)", "(ack|ignored)"),
        ("missing rights collector", "(ack|revoke)", "(ignored|revoke)"),
        ("missing reviewer identity", "permission:await permission(live.user.login)", "permission:'admin'"),
    ]:
        if before not in source:
            raise AssertionError(f"inactive rights canary: {label}")
        try:
            coverage_validator.validate_rights_contract(source.replace(before, after), merge)
        except coverage_validator.InvalidCoverage:
            pass
        else:
            raise AssertionError(f"accepted {label}")
    for before in ['--rights-evidence "$RUNNER_TEMP/maintainer-approval.json"',
                   '--head "${{ github.event.workflow_run.head_sha }}"']:
        try:
            coverage_validator.validate_rights_contract(source, merge.replace(before, ""))
        except coverage_validator.InvalidCoverage:
            pass
        else:
            raise AssertionError("accepted uncovered merge-group rights")
    for which in ("pr", "group"):
        for mutation in ("shell", "job-shell", "workflow-shell", "working-directory", "validator-env", "container",
                         "success-reporter", "wrong-head", "disabled-reporter", "reporter-token",
                         "latest-history", "missing-history", "collector-failure"):
            public = yaml.safe_load(source)
            group = yaml.safe_load(merge)
            document = public if which == "pr" else group
            job = document["jobs"]["trusted-policy"]
            validator = next(step for step in job["steps"] if "--rights-evidence" in step.get("run", ""))
            reporter = next(step for step in job["steps"] if step.get("name", "").startswith("Report trusted policy"))
            collector = next(step for step in job["steps"] if step.get("name", "").startswith("Record admin approvals"))
            if mutation == "collector-failure": collector["continue-on-error"] = True
            elif mutation == "shell": validator["shell"] = "/usr/bin/true {0}"
            elif mutation == "job-shell": job["defaults"] = {"run": {"shell": "/usr/bin/true {0}"}}
            elif mutation == "workflow-shell": document["defaults"] = {"run": {"shell": "/usr/bin/true {0}"}}
            elif mutation == "working-directory": validator["working-directory"] = "candidate"
            elif mutation == "validator-env": validator["env"] = {"PATH": "candidate/bin"}
            elif mutation == "container": job["container"] = "untrusted.invalid/skip-validation:latest"
            elif mutation == "success-reporter": reporter["with"]["script"] = reporter["with"]["script"].replace("'${{ job.status }}'==='success'", "true")
            elif mutation == "wrong-head": reporter["with"]["script"] = reporter["with"]["script"].replace("${{ steps.pr.outputs.head_sha }}" if which == "pr" else "${{ github.event.workflow_run.head_sha }}", "wrong-head")
            elif mutation == "disabled-reporter": reporter["if"] = "false"
            elif mutation == "reporter-token": reporter["with"]["github-token"] = "candidate-token"
            elif mutation == "latest-history": collector["with"]["script"] = collector["with"]["script"].replace("listCommitStatusesForRef", "getCombinedStatusForRef")
            elif mutation == "missing-history": collector["with"]["script"] = "const revocations = [];"
            try:
                coverage_validator.validate_rights_contract(yaml.safe_dump(public), yaml.safe_dump(group))
            except coverage_validator.InvalidCoverage:
                continue
            raise AssertionError(f"accepted {which} {mutation}")
    for mutation in ("cancel", "pending-cancel", "disabled", "failure-bypass", "before-resolve", "deleted-body"):
        public = yaml.safe_load(source)
        writer = public["jobs"]["rights-revocation"]
        if mutation in {"cancel", "pending-cancel"}: writer["concurrency"] = {"group": "shared", "cancel-in-progress": mutation == "cancel"}
        elif mutation == "disabled": writer["if"] = "false"
        elif mutation == "failure-bypass": public["jobs"]["trusted-policy"]["steps"][1]["run"] = "true"
        elif mutation == "before-resolve":
            steps = public["jobs"]["trusted-policy"]["steps"]
            steps[0], steps[1] = steps[1], steps[0]
        elif mutation == "deleted-body": public["jobs"]["rights-capture"]["steps"][0]["with"]["script"] = "return;"
        try:
            coverage_validator.validate_rights_contract(yaml.safe_dump(public), merge)
        except coverage_validator.InvalidCoverage:
            continue
        raise AssertionError(f"accepted revocation recorder {mutation}")
    durable_rights_history_canaries(source, merge)
    print("rights workflow canaries: missing/stale evidence, durable history and execution overrides refused")


def main() -> int:
    if len(sys.argv) == 6 and sys.argv[1] == "--rights-recovery":
        print(json.dumps(rights_recovery_evidence(*sys.argv[2:])))
        return 0
    if sys.argv[1:] == ["--workflow-contract"]:
        source = (ROOT / ".github/workflows/ci-policy.yml").read_text(encoding="utf-8")
        workflow_verdict_canaries(source)
        workflow_context_canaries(source)
        workflow_cleanup_canaries(source)
        workflow_trusted_policy_canaries(source)
        workflow_merge_group_policy_canaries(source)
        workflow_cache_canaries()
        rights_workflow_canaries()
        print("workflow contract: admission, verdict, rights and merge-group publisher canaries passed")
        return 0
    if len(sys.argv) != 1:
        raise SystemExit("usage: negative_canaries.py [--workflow-contract]")
    rights_workflow_canaries()
    manifest_canaries()
    coverage_layout_canaries()
    central_obligation_closure_canaries()
    core_physics_closure_canaries()
    workflow_canaries()
    quint_apalache_retry_canaries()
    ontology_quint_semantic_mutation_canaries()
    selector_gate_order_canaries()
    gate_binding_canaries()
    proof_hole_canaries()
    selector_declaration_canaries()
    source_tree_canaries()
    workspace_identity_canaries()
    evidence_inventory_canaries()
    print("verification attestation negative canaries rejected all mutations")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
