#!/usr/bin/env python3
"""Negative canaries for promoted-gate and result-attestation correspondence."""

from __future__ import annotations

import ast
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
CONCURRENCY_CASES = Path(__file__).with_name("fixtures") / "pull-request-concurrency.json"
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

def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


coverage_manifest = _sibling("coverage_manifest", ROOT / "scripts")
quint_apalache = _sibling("quint_apalache", ROOT / "scripts")


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


def condition_value(expression: str, names: dict | None = None):
    """Evaluate a workflow condition already rewritten to Python syntax, without `eval`
    (#513): only `and`/`or`/`not`, `==`/`!=`, literals, parentheses and calls to `names`."""
    names = names or {}

    def value(node):
        if isinstance(node, ast.Expression):
            return value(node.body)
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.BoolOp):
            result = None
            for operand in node.values:
                result = value(operand)
                if isinstance(node.op, ast.And) and not result or isinstance(node.op, ast.Or) and result:
                    return result
            return result
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
            return not value(node.operand)
        if isinstance(node, ast.Compare):
            left = value(node.left)
            for operator, right in zip(node.ops, node.comparators):
                right = value(right)
                if isinstance(operator, ast.Eq):
                    ok = left == right
                elif isinstance(operator, ast.NotEq):
                    ok = left != right
                else:
                    raise AssertionError(f"unsupported comparison in {expression!r}")
                if not ok:
                    return False
                left = right
            return True
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id in names and not node.keywords:
            return names[node.func.id](*(value(argument) for argument in node.args))
        raise AssertionError(f"unsupported expression {ast.dump(node)} in {expression!r}")

    return value(ast.parse(expression.strip(), mode="eval"))


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


def rust_parity_evidence_canaries() -> None:
    """The cheap phase requires the two Rust parity tests to have run and passed (nextest JUnit)."""
    validator = load_module("norn_rust_parity_evidence_canaries", VALIDATOR)
    core = validator.CORE_RUNNER.read_text(encoding="utf-8")
    first = "ci::tests::the_parity_fixture_keeps_every_gate_pinned_case"
    second = "ci::tests::pull_request_concurrency_matches_the_shared_parity_fixture"
    call = f"      require_passed \"$parity_report\" norn-skeleton-guard {first} {second}\n"
    run = f"      \"${{nextest[@]}}\" run --locked --ignore-default-filter \"${{guard[@]}}\" --lib -E 'test(={first}) | test(={second})'\n"
    with tempfile.TemporaryDirectory() as directory:
        mutated = Path(directory) / "core.sh"
        for label, before, after in (
                ("the parity tests are not run", run, ""),
                ("their run is not required", call, ""),
                ("a parity test renamed", call, call.replace(f" {first} ", " ci::tests::renamed ")),
                ("only one parity test required", call, call.replace(f" {second}", "")),
                ("a skipped test counts as passed", '"failure", "error", "skipped", ', '"failure", "error", '),
                ("the default filter decides", 'run --locked --ignore-default-filter "${guard[@]}" --lib', 'run --locked "${guard[@]}" --lib')):
            if core.count(before) != 1:
                raise AssertionError(f"Rust parity evidence canary target drifted: {label}")
            mutated.write_text(core.replace(before, after), encoding="utf-8")
            validator.CORE_RUNNER = mutated
            try:
                validator.validate_workflow_integrity([])
            except validator.InvalidCoverage as error:
                if "cheap phase must require the two Rust concurrency parity tests" in str(error):
                    continue
                raise AssertionError(f"Rust parity evidence mutation ({label}) refused for another reason: {error}")
            finally:
                validator.CORE_RUNNER = ROOT / "scripts/ci/core.sh"
            raise AssertionError(f"Rust parity evidence mutation passed: {label}")
    # The helper itself, run on nextest-shaped reports: only both tests passing passes.
    start = core.index("require_passed() {")
    helper = core[start:core.index("\nPY\n}\n", start) + len("\nPY\n}\n")]
    passed = f'<testcase name="{first}" classname="norn-skeleton-guard" time="0.1"/>'
    other = f'<testcase name="{second}" classname="norn-skeleton-guard" time="0.1"/>'
    with tempfile.TemporaryDirectory() as directory:
        report = Path(directory) / "junit.xml"
        for label, body, expected in (
                ("both ran and passed", f'<testsuite name="norn-skeleton-guard">{passed}{other}</testsuite>', 0),
                ("one ignored (absent)", f'<testsuite name="norn-skeleton-guard">{passed}</testsuite>', 1),
                ("one skipped", f'<testsuite name="norn-skeleton-guard">{passed}<testcase name="{second}"><skipped/></testcase></testsuite>', 1),
                ("one failed", f'<testsuite name="norn-skeleton-guard">{passed}<testcase name="{second}"><failure message="x"/></testcase></testsuite>', 1),
                ("another binary's tests", f'<testsuite name="norn-skeleton-guard::provenance">{passed}{other}</testsuite>', 1),
                ("one flaky", f'<testsuite name="norn-skeleton-guard">{passed}<testcase name="{second}"><flakyFailure message="x"/></testcase></testsuite>', 1),
                ("one passed on a rerun", f'<testsuite name="norn-skeleton-guard">{passed}<testcase name="{second}"><rerunFailure message="x"/></testcase></testsuite>', 1),
                ("an unreadable report", "not junit", 1),
                ("no report file", None, 1)):
            report.unlink(missing_ok=True)
            if body is not None:
                report.write_text(f"<testsuites>{body}</testsuites>" if body.startswith("<") else body, encoding="utf-8")
            result = subprocess.run(["bash", "-c", helper + f'\nrequire_passed "$1" norn-skeleton-guard {first} {second}', "-", str(report)],
                                    capture_output=True, text=True, timeout=30)
            if (result.returncode == 0) != (expected == 0):
                raise AssertionError(f"Rust parity evidence drift ({label}): {result.returncode} {result.stderr[-300:]}")


def studio_browser_pin_canaries() -> None:
    """Weakened browser phases are rejected by the coverage validator."""
    validator = load_module("norn_browser_pin_canaries", VALIDATOR)
    core = validator.CORE_RUNNER.read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory() as directory:
        mutated = Path(directory) / "core.sh"
        for label, before, after in (
                ("passes with no tests", "bunx playwright test --reporter=list,json\n", "bunx playwright test --reporter=list,json --pass-with-no-tests\n"),
                ("all skipped passes", "        [[ $ran -gt 0 ]] || { printf 'browser: Playwright ran no tests (every row skipped)\\n' >&2; exit 1; }\n", ""),
                ("skips while tests exist", "          exit 1\n        fi\n        printf '::notice::browser", "        fi\n        printf '::notice::browser"),
                ("no listing check", "        [[ ${listed:-0} -gt 0 ]] || { printf 'browser: Playwright lists no tests\\n' >&2; exit 1; }\n", ""),
                ("not in the plan", " integration browser formal ", " integration formal "),
                ("no checker-admission test", "        decision=$(NORN_CHECKER_WASM_REQUIRED=1 bun test test/decision.test.ts 2>&1) || { printf '%s\\n' \"$decision\"; exit 1; }\n", ""),
                ("no named-case check", "        grep -qxE '\\(pass\\) the checker admits what a drawn decision lowers to \\[[0-9.]+m?s\\]' <<< \"$decision\" || { printf 'browser: the checker-admission case did not run and pass\\n' >&2; exit 1; }\n", ""),
                ("named case matched as a substring", "grep -qxE '\\(pass\\) the checker admits", "grep -qE '\\(pass\\) the checker admits"),
                ("checker-admission test may skip", "NORN_CHECKER_WASM_REQUIRED=1 bun test", "bun test")):
            if core.count(before) != 1:
                raise AssertionError(f"browser pin canary target drifted: {label}")
            mutated.write_text(core.replace(before, after), encoding="utf-8")
            validator.CORE_RUNNER = mutated
            try:
                validator.validate_workflow_integrity([])
            except validator.InvalidCoverage as error:
                if "browser phase must run the Studio Playwright rows" in str(error):
                    continue
                raise AssertionError(f"browser phase mutation ({label}) refused for another reason: {error}")
            finally:
                validator.CORE_RUNNER = ROOT / "scripts/ci/core.sh"
            raise AssertionError(f"browser phase mutation passed: {label}")


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



def studio_browser_phase_canaries() -> None:
    """The browser phase never passes silently: missing or empty Studio browser tests fail."""
    with tempfile.TemporaryDirectory() as directory:
        tree, bin_dir = Path(directory) / "tree", Path(directory) / "bin"
        (tree / "crates/norn-checker-wasm/scripts").mkdir(parents=True)
        (tree / "apps/studio").mkdir(parents=True)
        bin_dir.mkdir()
        tools = {
            "cargo": "exit 0",
            "bun": 'if [ "$1" = test ]; then printf "%s\\n" "$BUN_TEST_OUTPUT"; exit "$BUN_TEST_STATUS"; fi; exit 0',
            "rustc": 'if [ "$1" = --print ]; then echo "$WASM_LIBDIR"; else echo "rustc 1.98.0"; fi',
            "bunx": 'if [ "$3" = --list ]; then echo "Total: $LISTED tests in 1 file"; exit 0; fi; '
                    'printf "{\\"stats\\": {\\"expected\\": %s, \\"skipped\\": %s}}" "$RAN" "$SKIPPED" > "$PLAYWRIGHT_JSON_OUTPUT_NAME"; '
                    'exit "$RUN_STATUS"',
        }
        for name, body in tools.items():
            (bin_dir / name).write_text("#!/bin/sh\n" + body + "\n")
            (bin_dir / name).chmod(0o755)
        build = tree / "crates/norn-checker-wasm/scripts/build-pkg.sh"
        build.write_text("#!/bin/sh\nmkdir -p \"$1\"\n")
        build.chmod(0o755)
        config, e2e = tree / "apps/studio/playwright.config.ts", tree / "apps/studio/e2e"
        wasm_libdir = Path(directory) / "wasm32-lib"
        wasm_libdir.mkdir()
        for label, has_config, has_tests, listed, run_status, expected, libdir, ran in (
                ("no config, no tests", False, False, "0", 0, 0, "", "0"),
                ("tests without a config", False, True, "0", 0, 1, str(wasm_libdir), "0"),
                ("config, no wasm32 target", True, True, "99", 0, 1, str(Path(directory) / "absent"), "0"),
                ("config, no tests found", True, True, "0", 0, 1, str(wasm_libdir), "0"),
                ("config, every row skipped", True, True, "99", 0, 1, str(wasm_libdir), "0"),
                ("config, rows pass", True, True, "99", 0, 0, str(wasm_libdir), "99"),
                ("config, a row fails", True, True, "99", 1, 1, str(wasm_libdir), "98"),
                ("config, checker-admission test fails", True, True, "99", 0, 1, str(wasm_libdir), "99"),
                ("config, checker-admission case skipped", True, True, "99", 0, 1, str(wasm_libdir), "99"),
                ("config, checker-admission case missing", True, True, "99", 0, 1, str(wasm_libdir), "99"),
                ("config, only a longer name passed", True, True, "99", 0, 1, str(wasm_libdir), "99")):
            config.unlink(missing_ok=True)
            if has_config:
                config.write_text("export default {}\n")
            if has_tests:
                e2e.mkdir(exist_ok=True)
                (e2e / "row.e2e.ts").write_text("")
            elif e2e.exists():
                for entry in e2e.iterdir():
                    entry.unlink()
                e2e.rmdir()
            result = subprocess.run(["bash", str(ROOT / "scripts/ci/core.sh"), "browser"], cwd=tree, capture_output=True,
                                    text=True, timeout=60, env={"PATH": f"{bin_dir}:{os.environ.get('PATH', '')}",
                                                                "CARGO_HOME": str(Path(directory) / "cargo-home"),
                                                                "HOME": directory, "LISTED": listed, "RUN_STATUS": str(run_status),
                                                                "WASM_LIBDIR": libdir, "RAN": ran,
                                                                "BUN_TEST_STATUS": "1" if label.endswith("test fails") else "0",
                                                                "BUN_TEST_OUTPUT": {
                                                                    "config, checker-admission case skipped": "(skip) the checker admits what a drawn decision lowers to",
                                                                    "config, checker-admission case missing": "(pass) the decider reads the real time limit",
                                                                    "config, only a longer name passed": "(pass) the checker admits what a drawn decision lowers to, twice [1.00ms]",
                                                                }.get(label, "(pass) the checker admits what a drawn decision lowers to [1.00ms]"),
                                                                "SKIPPED": str(int(listed) - int(ran))})
            if result.returncode != expected:
                raise AssertionError(f"browser phase drift ({label}): {result.returncode} {result.stdout[-300:]} {result.stderr[-300:]}")
            if label == "config, no wasm32 target" and "run under the pinned CI toolchain" not in result.stderr:
                raise AssertionError(f"browser phase must name the pinned toolchain when wasm32 is missing: {result.stderr[-300:]}")


def workflow_expression_cap_canaries() -> None:
    """An interpolated script over the expression cap is refused before GitHub silently rejects it."""
    validator = load_module("norn_expression_cap_canaries", VALIDATOR)
    if validator.actions_expression_length("a{b}'c ${{ x.y }} d") != len("format('a{{b}}''c {0} d', x.y)"):
        raise AssertionError("expression length no longer models GitHub's format() escaping")
    merge = (ROOT / ".github/workflows/trusted-merge-group.yml").read_text(encoding="utf-8")
    source = (ROOT / ".github/workflows/ci-policy.yml").read_text(encoding="utf-8")
    group = yaml.safe_load(merge)
    name = "Record admin approvals of the queued heads"
    collector = next(step for step in group["jobs"]["trusted-policy"]["steps"] if step.get("name") == name)
    current = validator.actions_expression_length(collector["with"]["script"])
    for target, refused in ((validator.EXPRESSION_CAP, False), (validator.EXPRESSION_CAP + 1, True), (21_034, True)):
        # Pad with a comment of plain characters up to the target length.
        document = copy.deepcopy(group)
        step = next(step for step in document["jobs"]["trusted-policy"]["steps"] if step.get("name") == name)
        step["with"]["script"] += "//" + "x" * (target - current - 3) + "\n"
        if validator.actions_expression_length(step["with"]["script"]) != target:
            raise AssertionError(f"expression cap canary padding drifted for {target}")
        try:
            validator.validate_expression_lengths({"trusted-merge-group.yml": yaml.safe_dump(document, sort_keys=False),
                                                   "ci-policy.yml": source})
        except validator.InvalidCoverage:
            if not refused:
                raise AssertionError(f"a {target}-character collector expression was refused")
            continue
        if refused:
            raise AssertionError(f"a {target}-character collector expression passed the cap")


def workflow_concurrency_canaries() -> None:
    """A pull_request workflow cancels its own superseded PR runs and nothing else."""
    validator = load_module("norn_workflow_concurrency_canaries", VALIDATOR)
    workflows = ROOT / ".github/workflows"
    sources = {f".github/workflows/{path.name}": path.read_text(encoding="utf-8")
               for path in sorted([*workflows.glob("*.yml"), *workflows.glob("*.yaml")])}
    # Every checked-in workflow passes, including the out-of-scope pull_request_target
    # (cla.yml) and workflow_run (trusted-merge-group.yml) lanes.
    validator.validate_pull_request_concurrency(sources)


def workflow_concurrency_parity() -> None:
    """validate.py reaches the shared verdicts the skeleton guard's own test asserts.

    crates/norn-skeleton-guard/src/ci/mod.rs checks `concurrency_error` against the same
    file, so the two implementations of the rule cannot drift apart unnoticed. The fixture
    may gain cases anywhere; it cannot lose, flip or change a case validate.py pins.
    """
    validator = load_module("norn_workflow_concurrency_parity", VALIDATOR)
    cases = json.loads(CONCURRENCY_CASES.read_text(encoding="utf-8"))["cases"]
    check_concurrency_cases(validator, cases)
    # Correct, unpinned cases added first and last still pass, and every canary below runs
    # on that grown list too, choosing its case by name from the pin, never by position.
    extra = {**cases[0], "name": "an unpinned case added later"}
    grown = [extra, *cases, extra]
    check_concurrency_cases(validator, grown)
    for listed in (cases, grown):
        for name in validator.PULL_REQUEST_CONCURRENCY_CASES:
            def named(case: dict) -> bool:
                return case.get("name") == name
            for problem, mutated in (
                    ("lost", [case for case in listed if not named(case)]),
                    ("flipped", [{**case, "expect": "reject" if case["expect"] == "accept" else "accept"}
                                 if named(case) else case for case in listed]),
                    *(("input changed", [{**case, field: None if case.get(field) is not None else "changed\n"}
                                         if named(case) else case for case in listed])
                      for field in ("on", "concurrency", "source"))):
                try:
                    check_concurrency_cases(validator, mutated)
                except PinnedConcurrencyCase as error:
                    if error.problems == [f"{name}: {problem}"]:
                        continue
                    raise AssertionError(f"concurrency pin canary ({name}: {problem}) reported {error.problems}")
                raise AssertionError(f"concurrency parity accepted a {problem} pinned case: {name}")


class PinnedConcurrencyCase(AssertionError):
    """The parity fixture lost, flipped or changed a case validate.py pins."""

    def __init__(self, problems: list[str]) -> None:
        super().__init__(f"concurrency parity fixture lost, flipped or changed gate-pinned cases: {problems}")
        self.problems = problems


def pinned_case_problems(validator, cases: list) -> list[str]:
    """Each pinned case the fixture lost, flipped or changed; every copy of a pinned name must match."""
    problems = []
    for name, (expect, digest) in validator.PULL_REQUEST_CONCURRENCY_CASES.items():
        found = [case for case in cases if isinstance(case, dict) and case.get("name") == name]
        if not found:
            problems.append(f"{name}: lost")
        elif any(case.get("expect") != expect for case in found):
            problems.append(f"{name}: flipped")
        elif any(validator.concurrency_case_digest(case) != digest for case in found):
            problems.append(f"{name}: input changed")
    return problems


def check_concurrency_cases(validator, cases: list) -> None:
    """Every gate-pinned case is present unchanged, and every fixture case reaches its verdict."""
    problems = pinned_case_problems(validator, cases)
    if problems:
        raise PinnedConcurrencyCase(problems)
    verdicts = set()
    for case in cases:
        if (not isinstance(case, dict) or not set(case) <= {"name", "on", "concurrency", "expect", "source"}
                or not case.get("name") or not isinstance(case.get("on"), str)
                or not isinstance(case.get("concurrency", ""), (str, type(None)))
                or case.get("expect") not in ("accept", "reject")):
            raise AssertionError(f"malformed parity case: {case!r}")
        concurrency = case.get("concurrency") or ""
        if "source" in case:
            checked_in = (ROOT / case["source"]).read_text(encoding="utf-8")
            if case["on"] not in checked_in or concurrency not in checked_in:
                raise AssertionError(f"{case['name']}: drifted from {case['source']}")
        try:
            validator.validate_pull_request_concurrency({case["name"]: case["on"] + concurrency})
            verdict = "accept"
        except validator.InvalidCoverage:
            verdict = "reject"
        if verdict != case["expect"]:
            raise AssertionError(f"concurrency parity: {case['name']} was {verdict}ed, expected {case['expect']}")
        verdicts.add(verdict)
    if verdicts != {"accept", "reject"}:
        raise AssertionError("the concurrency parity fixture needs accepted and rejected cases")


def workflow_sccache_canaries(source: str) -> None:
    """A dead cache backend never fails a build; a real compile error always does."""
    workflow = yaml.safe_load(source)
    for name in ("core", "test"):
        job = workflow["jobs"][name]
        if "RUSTC_WRAPPER" in job.get("env", {}) or job["env"].get("SCCACHE_IGNORE_SERVER_IO_ERROR") != "1":
            raise AssertionError(f"{name}: sccache must be opt-in per run and tolerate server IO errors")
        start = next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")
        for runner, starts in itertools.product(("github-hosted", "self-hosted"), (True, False)):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "bin").mkdir()
                (root / "tmp").mkdir()
                # Fake sccache: `--start-server` succeeds or dies like the egress outage; a
                # compile either dies like a server that cannot restart, or runs the compiler.
                sccache = root / "bin" / "sccache"
                sccache.write_text("#!/bin/sh\n"
                    "if [ \"$1\" = --start-server ]; then\n"
                    + ("  exit 0\n" if starts else
                       "  echo 'sccache: error: Server startup failed: ServerBusy: Egress is over the account limit' >&2; exit 2\n")
                    + "fi\n"
                    "if [ \"$FAKE_SCCACHE\" = dead ]; then echo 'sccache: error: failed to start server: Address in use' >&2; exit 2; fi\n"
                    # SCCACHE_IGNORE_SERVER_IO_ERROR: sccache warns, then compiles locally and
                    # returns the compiler's own status.
                    "if [ \"$FAKE_SCCACHE\" = warn ]; then echo 'sccache: warning: The server looks like it shut down unexpectedly, compiling locally instead' >&2; fi\n"
                    "exec \"$@\"\n")
                # Fake rustc: records each run; fails like a type error when asked to.
                rustc = root / "bin" / "rustc"
                rustc.write_text("#!/bin/sh\necho run >> \"$RUSTC_LOG\"\n"
                    "if [ \"$FAKE_RUSTC\" = broken ]; then echo 'error[E0308]: mismatched types' >&2; exit 1; fi\necho compiled\n")
                for tool in (sccache, rustc):
                    tool.chmod(0o755)
                github_env = root / "env"
                github_env.write_text("")
                base = {"PATH": f"{root / 'bin'}:{os.environ.get('PATH', '')}", "RUNNER_TEMP": str(root / "tmp")}
                result = subprocess.run(["bash", "-c", start["run"]], capture_output=True, text=True, timeout=30,
                                        env={**base, "RUNNER_ENVIRONMENT": runner, "GITHUB_ENV": str(github_env)})
                exported = dict(line.split("=", 1) for line in github_env.read_text().splitlines() if line)
                wrapper = str(root / "tmp" / "sccache-rustc")
                expected = {"SCCACHE_GHA_ENABLED": "true" if runner == "github-hosted" else "false",
                            **({"RUSTC_WRAPPER": wrapper} if starts else {})}
                if result.returncode != 0 or exported != expected:
                    raise AssertionError(f"{name}: sccache start drift on {runner}/{starts}: {result.returncode} {exported}")
                if not starts:
                    continue
                # The wrapper's own bookkeeping never fails a build: without a temporary file
                # (disk full), it compiles with plain rustc and keeps the compiler's status.
                broken_tmp = root / "no-tmp"
                broken_tmp.mkdir()
                (broken_tmp / "mktemp").write_text("#!/bin/sh\necho 'mktemp: No space left on device' >&2\nexit 1\n")
                (broken_tmp / "mktemp").chmod(0o755)
                for compiler, status in (("ok", 0), ("broken", 1)):
                    log = root / f"rustc-no-tmp-{compiler}.log"
                    compiled = subprocess.run([wrapper, str(rustc), "--crate-name", "probe"], capture_output=True, text=True,
                                              timeout=30, env={**base, "PATH": f"{broken_tmp}:{base['PATH']}",
                                                               "FAKE_SCCACHE": "alive", "FAKE_RUSTC": compiler,
                                                               "RUSTC_LOG": str(log)})
                    calls = len(log.read_text().splitlines()) if log.exists() else 0
                    if compiled.returncode != status or calls != 1:
                        raise AssertionError(f"{name}: sccache wrapper without a temporary file drift {compiler}: "
                                             f"{compiled.returncode} runs={calls} {compiled.stderr!r}")
                for server, compiler, status, runs in (("alive", "ok", 0, 1), ("dead", "ok", 0, 1),
                                                       ("alive", "broken", 1, 1), ("dead", "broken", 1, 1),
                                                       ("warn", "ok", 0, 1), ("warn", "broken", 1, 1)):
                    log = root / f"rustc-{server}-{compiler}.log"
                    compiled = subprocess.run([wrapper, str(rustc), "--crate-name", "probe"], capture_output=True, text=True,
                                              timeout=30, env={**base, "FAKE_SCCACHE": server, "FAKE_RUSTC": compiler,
                                                               "RUSTC_LOG": str(log)})
                    calls = len(log.read_text().splitlines()) if log.exists() else 0
                    fell_back = "compiling with plain rustc" in compiled.stderr
                    # A warned local compile is never rerun: its diagnostics and the warning survive.
                    if compiled.returncode != status or calls != runs or fell_back != (server == "dead") \
                            or (compiler == "broken" and "E0308" not in compiled.stderr) \
                            or (server == "warn" and "sccache: warning:" not in compiled.stderr):
                        raise AssertionError(f"{name}: sccache wrapper drift {server}/{compiler}: "
                                             f"{compiled.returncode} runs={calls} {compiled.stderr!r}")

        # The shared Boat cache (R2): reviewed code writes, everything else reads. Run the start
        # step with the bucket's keys present for each event and record what the server gets.
        for runner, event, ref, mode in (
                ("github-hosted", "push", "refs/heads/main", None),
                ("github-hosted", "merge_group", "refs/heads/gh-readonly-queue/main/pr-1-a", None),
                ("self-hosted", "push", "refs/heads/main", "READ_WRITE"),
                ("self-hosted", "merge_group", "refs/heads/gh-readonly-queue/main/pr-1-a", "READ_WRITE"),
                ("self-hosted", "pull_request", "refs/pull/1/merge", "READ_ONLY"),
                ("self-hosted", "pull_request", "refs/heads/main", "READ_ONLY"),
                ("self-hosted", "push", "refs/heads/other", "READ_ONLY"),
                ("self-hosted", "push", "refs/heads/main-x", "READ_ONLY"),
                ("self-hosted", "merge_group", "refs/heads/gh-readonly-queue/other/pr-1-a", "READ_ONLY"),
                ("self-hosted", "workflow_dispatch", "refs/heads/main", "READ_ONLY"),
                ("self-hosted", "pull_request", "refs/heads/gh-readonly-queue/main/pr-1-a", "READ_ONLY")):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "bin").mkdir()
                (root / "tmp").mkdir()
                seen = root / "server.env"
                sccache = root / "bin" / "sccache"
                sccache.write_text("#!/bin/sh\nenv | grep -E '^(SCCACHE_|AWS_)' | sort > \"$SEEN\"\nexit 0\n")
                sccache.chmod(0o755)
                github_env = root / "env"
                github_env.write_text("")
                keys = {"NORN_SCCACHE_BUCKET": "bucket", "NORN_SCCACHE_ENDPOINT": "https://r2.invalid",
                        "NORN_SCCACHE_KEY_ID": "key-id", "NORN_SCCACHE_SECRET": "key-secret"}
                result = subprocess.run(["bash", "-c", start["run"]], capture_output=True, text=True, timeout=30,
                                        env={"PATH": f"{root / 'bin'}:{os.environ.get('PATH', '')}",
                                             "RUNNER_TEMP": str(root / "tmp"), "RUNNER_ENVIRONMENT": runner,
                                             "GITHUB_ENV": str(github_env), "GITHUB_EVENT_NAME": event,
                                             "GITHUB_REF": ref, "SEEN": str(seen), **keys})
                server = dict(line.split("=", 1) for line in seen.read_text().splitlines() if line)
                exported = github_env.read_text()
                got = server.get("SCCACHE_S3_RW_MODE") if server.get("SCCACHE_MULTILEVEL_CHAIN") == "disk,s3" else None
                if result.returncode != 0 or got != mode:
                    raise AssertionError(f"{name}: shared cache mode drift {runner}/{event}/{ref}: {got} != {mode}")
                if mode and (server.get("SCCACHE_BUCKET") != "bucket" or server.get("SCCACHE_REGION") != "auto"
                             or server.get("AWS_ACCESS_KEY_ID") != "key-id"):
                    raise AssertionError(f"{name}: shared cache server environment drift: {server}")
                if mode is None and any(key.startswith("AWS_") for key in server):
                    raise AssertionError(f"{name}: hosted runner received the shared cache keys")
                # The keys never outlive the step: nothing but the GHA switch and the wrapper is exported.
                if "key-" in exported or "SCCACHE_S3" in exported or "AWS_" in exported:
                    raise AssertionError(f"{name}: shared cache keys exported to later steps: {exported!r}")
        # Without the bucket's secrets (forks, or before the bucket exists) Boat keeps its local cache.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            (root / "tmp").mkdir()
            seen = root / "server.env"
            (root / "bin" / "sccache").write_text("#!/bin/sh\nenv | grep -E '^(SCCACHE_|AWS_)' | sort > \"$SEEN\"\nexit 0\n")
            (root / "bin" / "sccache").chmod(0o755)
            (root / "env").write_text("")
            subprocess.run(["bash", "-c", start["run"]], capture_output=True, text=True, timeout=30, check=True,
                           env={"PATH": f"{root / 'bin'}:{os.environ.get('PATH', '')}", "RUNNER_TEMP": str(root / "tmp"),
                                "RUNNER_ENVIRONMENT": "self-hosted", "GITHUB_ENV": str(root / "env"),
                                "GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "SEEN": str(seen)})
            if "SCCACHE_MULTILEVEL_CHAIN" in seen.read_text():
                raise AssertionError(f"{name}: shared cache enabled without the bucket's keys")


def bundle_prune_drift(function: str) -> str | None:
    """Run a `prune_target` definition against every CARGO_TARGET_DIR case; describe any drift.

    Only the workspace's own target dir is touched, and inside it only Cargo build outputs
    (profile and target-triple trees, `doc`); target/verification-* evidence and tools,
    other content and symlinks survive.
    """
    with tempfile.TemporaryDirectory() as directory:
        top = Path(directory).resolve()
        home, workspace = top / "home", top / "home" / "work" / "norn"
        outside = top / "outside"
        target = workspace / "target"
        cargo = [target / "debug", target / "wasm32-unknown-unknown", target / "doc"]
        kept = [workspace, workspace / "src", home, home / ".cargo", outside, target,
                target / "verification-results", target / "verification-tools", target / "norn-checker-wasm",
                outside / ".fingerprint"]
        for path in (workspace / "src", home / ".cargo", outside / ".fingerprint"):
            path.mkdir(parents=True, exist_ok=True)
        (workspace / "escape").symlink_to(outside)
        (workspace / "target-link").symlink_to(outside)
        cases = {
            None: True, "": True, "target": True, "target/": True, str(target): True,
            "target/../target": True,
            "/": False, str(home): False, str(workspace): False, str(workspace.parent): False, ".": False, "..": False,
            "../sibling": False, str(outside): False, "escape": False, "target-link": False, "target/debug": False,
            "src/../../norn": False,
        }
        for value, deletes in cases.items():
            for path in (target / "debug" / ".fingerprint", target / "wasm32-unknown-unknown" / "debug" / ".fingerprint",
                         # Verification tools may be Cargo-built in place: still never pruned.
                         target / "doc", target / "verification-results", target / "verification-tools" / "release" / ".fingerprint",
                         target / "norn-checker-wasm" / "pkg"):
                path.mkdir(parents=True, exist_ok=True)
            # A symlinked profile-looking tree out of target must never be followed.
            link = target / "linked"
            if not link.is_symlink():
                link.symlink_to(outside)
            env = {"PATH": os.environ.get("PATH", ""), "HOME": str(home)}
            if value is not None:
                env["CARGO_TARGET_DIR"] = value
            result = subprocess.run(["bash", "-c", function + "prune_target"], cwd=workspace, env=env,
                                    capture_output=True, text=True, timeout=30)
            if result.returncode:
                return f"{value!r}: exit {result.returncode} {result.stderr!r}"
            lost = [str(path) for path in kept + [link] if not (path.exists() or path.is_symlink())]
            if lost:
                return f"{value!r}: removed {lost}"
            remaining = [path.exists() for path in cargo]
            if remaining != [not deletes] * len(cargo):
                return f"{value!r}: Cargo outputs {remaining}, expected {'pruned' if deletes else 'kept'}"
            if not deletes and "refusing to prune" not in result.stdout:
                return f"{value!r}: refused silently"
    return None


def bundle_prune_containment_canaries() -> None:
    """The bundle prune deletes only Cargo build outputs in the workspace's own target dir."""
    core = (ROOT / "scripts/ci/core.sh").read_text(encoding="utf-8")
    start = core.index("prune_target() {\n")
    function = core[start:core.index("\n}\n", start) + 3]
    drift = bundle_prune_drift(function)
    if drift is not None:
        raise AssertionError(f"bundle prune containment drift: {drift}")
    # Pruning all of target, or every directory in it, takes verification evidence and tools.
    for label, before, after in (
            ("whole target", 'rm -rf -- "$entry"', 'rm -rf -- "$target"'),
            ("verification evidence", ' && $name != verification-*', ''),
            ("any directory", 'if [[ $name == doc || -d $entry/.fingerprint ]] || compgen -G "$entry/*/.fingerprint" > /dev/null; then',
             "if true; then"),
            ("followed symlinks", ' && ! -L $entry', '')):
        if function.count(before) != 1:
            raise AssertionError(f"bundle prune containment canary target drifted: {label}")
        if bundle_prune_drift(function.replace(before, after)) is None:
            raise AssertionError(f"bundle prune mutation passed: {label}")


def workflow_trusted_policy_canaries(source: str) -> None:
    job = yaml.safe_load(source)["jobs"]["trusted-policy"]
    expression = " ".join(job["if"].split())
    for event, action, body, previous, is_pr, expected in (
        ("pull_request_target", "synchronize", "", "", True, True),
        ("issue_comment", "created", "/approve-gates head", "", True, True),
        ("issue_comment", "edited", "revoked", "/approve-gates head", True, True),
        ("issue_comment", "deleted", "/approve-gates head", "", True, True),
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
            accepted = condition_value(condition, {"contains": lambda text, part: part in text})
            if bool(accepted) != (expected and not cancelled):
                raise AssertionError(f"trusted-policy event admission drift: {event}/{action}/{cancelled}")

    reporter = next(step for step in job["steps"]
                    if step.get("name") == "Report trusted policy on the exact PR head")

    def report(status: str, *, resolved: bool = True) -> dict[str, Any]:
        script = reporter["with"]["script"].replace("${{ steps.pr.outputs.head_sha }}", "head" if resolved else "").replace(
            "${{ job.status }}", status)
        # Execute the actual workflow reporter against an API double. No API writes.
        harness = """const context = {repo: {owner: 'owner', repo: 'repo'}, payload: {issue: {number: 7}},
            serverUrl: 'https://example.invalid', runId: 42};
const github = {rest: {pulls: {get: async () => ({data: {head: {sha: 'head'}}})},
    repos: {createCommitStatus: async value => console.log(JSON.stringify(value))}}};
(async () => {
""" + script + "\n})().catch(error => { console.error(error); process.exit(1); });"
        result = subprocess.run(["bun", "-e", harness], capture_output=True, text=True, timeout=10, check=True)
        value = json.loads(result.stdout)
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
            if bool(condition_value(expression)) != (event == "merge_group" and same_repository):
                raise AssertionError("merge-group publisher event/repository boundary drifted")
        command = 'python3 -I -B -X pycache_prefix="$(mktemp -d)" trusted/scripts/ci/validate-runner-policy.py --root candidate --maintainer-evidence "$RUNNER_TEMP/maintainer-approval.json"'
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
            # workflow_run checks belong to the default branch. The actual JS
            # must explicitly publish onto the triggering merge-group commit.
            harness = """const context = {repo: {owner: 'owner', repo: 'repo'},
    sha: 'default-branch-head', serverUrl: 'https://example.invalid', runId: 42};
const github = {rest: {repos: {createCommitStatus: async value => console.log(JSON.stringify(value))}}};
(async () => {
""" + script + "\n})().catch(error => { console.error(error); process.exit(1); });"
            result = subprocess.run(["bun", "-e", harness], check=True, capture_output=True, text=True, timeout=10)
            value = json.loads(result.stdout)
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
                "context": ("context: 'trusted-policy'", "context: 'different-status'"),
                "head": ("${{ github.event.workflow_run.head_sha }}", "${{ github.sha }}"),
                "state": ("'${{ job.status }}' === 'success' ? 'success' : 'failure'", "'success'"),
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
                        if bool(condition_value(candidate_expression)) != expected:
                            raise AssertionError(f"{name} admission drift: {event}/{action}/{draft}")
        for event in ("pull_request", "merge_group", "push", "workflow_dispatch"):
            for draft in (False, True):
                expression = workflow["jobs"]["full"]["if"].replace("github.event_name", repr(event)).replace(
                    "github.event.pull_request.draft", str(draft)).replace(
                    "always()", "True").replace("cancelled()", "True").replace("&&", " and ")
                expression = re.sub(r"!(?!=)", " not ", expression)
                if bool(condition_value(expression)) != (event != "pull_request" or draft):
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
                trusted["concurrency"]["cancel-in-progress"] = False
            elif change == "if":
                trusted["if"] = "!cancelled() && github.event_name == 'pull_request_target'"
            else:
                reporter = next(step for step in trusted["steps"]
                                if step.get("name") == "Report trusted policy on the exact PR head")
                reporter["with"]["script"] = reporter["with"]["script"].replace("context: 'trusted-policy'", "context: 'different-status'")
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
            # The Boat reclaim runs only on self-hosted VMs, frees only named image content,
            # and fails fast below 40 GiB: each weakening is rejected.
            boat = next(i for i, step in enumerate(workflow["jobs"][name]["steps"])
                        if step.get("name") == "Reclaim Boat runner disk space")
            for label, key, before, after in (
                    ("hosted", "if", "'self-hosted'", "'github-hosted'"),
                    ("always", "if", "${{ runner.environment == 'self-hosted' }}", "${{ always() }}"),
                    ("threshold", "run", "40 * 1024 * 1024", "1 * 1024 * 1024"),
                    ("check", "run", '[ "$(available)" -ge "$minimum" ]', "true"),
                    ("tolerated", "run", "docker system prune --all --force", "docker system prune --all --force || true"),
                    ("broad", "run", "rustup toolchain uninstall stable nightly-2025-03-01", "sudo rm -rf /usr/local/rustup /var/lib")):
                document = copy.deepcopy(workflow)
                step = document["jobs"][name]["steps"][boat]
                if before not in step[key]:
                    raise AssertionError(f"Boat reclaim canary target drifted: {label}")
                step[key] = step[key].replace(before, after)
                rejected(f"{name} Boat reclaim {label}", document)
            document = copy.deepcopy(workflow)
            del document["jobs"][name]["steps"][boat]
            rejected(f"{name} without Boat reclaim", document)
            for label, change in (
                    ("unconditional wrapper", lambda job: job["env"].__setitem__("RUSTC_WRAPPER", "sccache")),
                    ("fatal server IO", lambda job: job["env"].pop("SCCACHE_IGNORE_SERVER_IO_ERROR")),
                    ("unbounded local cache", lambda job: job["env"].pop("SCCACHE_CACHE_SIZE")),
                    ("shared cache on Boat", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace("gha=false", "gha=true"))),
                    ("masked compile failure", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace('exit "$status"', "exit 0"))),
                    ("fallback without compiling", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace('  exec "$@"\n', "  exit 0\n"))),
                    ("fatal temporary file", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace('log=$(mktemp) || exec "$@"', "log=$(mktemp)"))),
                    # Any `sccache:` line (a warning before a local compile) would rerun a failing compiler.
                    ("warnings as cache failures", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace("grep -q '^sccache: error: '", "grep -q '^sccache: '"))),
                    # The action's post-job stats call setFailed when the stats request fails.
                    ("fatal post-job stats", lambda job: next(step for step in job["steps"]
                        if step.get("uses", "").startswith("mozilla-actions/sccache-action@"))["with"].pop("disable_annotations")),
                    ("post-job stats re-enabled", lambda job: next(step for step in job["steps"]
                        if step.get("uses", "").startswith("mozilla-actions/sccache-action@"))["with"].__setitem__(
                        "disable_annotations", False)),
                    ("cache writer for every event", lambda job: job.__setitem__("environment", "ci-cache-writer")),
                    ("cache writer for pull requests", lambda job: job.__setitem__("environment",
                        job["environment"].replace("github.event_name == 'merge_group'",
                                                   "github.event_name == 'merge_group' || github.event_name == 'pull_request'"))),
                    ("cache keys in the job environment", lambda job: job["env"].update(
                        {"AWS_ACCESS_KEY_ID": "${{ secrets.NORN_SCCACHE_KEY_ID }}"})),
                    ("cache keys exported to later steps", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace('if sccache --start-server; then', 'echo "AWS_SECRET_ACCESS_KEY=$AWS_SECRET_ACCESS_KEY" >> "$GITHUB_ENV"\nif sccache --start-server; then'))),
                    ("pull requests write the shared cache", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace("mode=READ_ONLY", "mode=READ_WRITE"))),
                    ("shared cache on hosted runners", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        next(step for step in job["steps"] if step.get("name") == "Start sccache or compile without it")["run"]
                        .replace('if [ "$gha" = false ] && [ -n', 'if [ -n'))),
                    ("cache keys outside the start step", lambda job: next(step for step in job["steps"]
                        if step.get("uses", "").startswith("Swatinem/rust-cache@")).__setitem__("env",
                        {"NORN_SCCACHE_SECRET": "${{ secrets.NORN_SCCACHE_SECRET }}"})),
                    ("fatal start", lambda job: next(step for step in job["steps"]
                        if step.get("name") == "Start sccache or compile without it").__setitem__("run",
                        "set -e\nsccache --start-server\necho RUSTC_WRAPPER=sccache >> \"$GITHUB_ENV\"\n"))):
                document = copy.deepcopy(workflow)
                change(document["jobs"][name])
                rejected(f"{name} sccache {label}", document)
        # The bundle may drop only the cargo target dir, between tasks, when disk is low.
        source_runner = validator.CORE_RUNNER
        core = source_runner.read_text(encoding="utf-8")
        try:
            for label, before, after in (
                    ("broad", 'rm -rf -- "$entry"', 'rm -rf -- "$root"'),
                    ("whole target", 'rm -rf -- "$entry"', 'rm -rf -- "$target"'),
                    ("verification evidence", ' && $name != verification-*', ''),
                    ("uncontained", 'if [[ $target != "$root/target" ]]; then', "if false; then"),
                    ("uncanonical", 'target=$(realpath -m -- "${CARGO_TARGET_DIR:-$PWD/target}")', 'target=${CARGO_TARGET_DIR:-$PWD/target}'),
                    ("unconditional", "if [[ -n $pruned ]] && ((", "if true || [[ -n $pruned ]] && (("),
                    ("first task", "      pruned=1\n", ""),
                    ("extra deletion", "        prune_target\n", '        prune_target; rm -rf -- "$HOME/.cargo"\n')):
                if core.count(before) != 1:
                    raise AssertionError(f"bundle prune canary target drifted: {label}")
                mutated = Path(directory) / "core.sh"
                mutated.write_text(core.replace(before, after), encoding="utf-8")
                validator.CORE_RUNNER = mutated
                document = copy.deepcopy(workflow)
                rejected(f"{label} bundle prune", document)
        finally:
            validator.CORE_RUNNER = source_runner
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
        for name in ("core", "test"):
            # Boat's base image needs libclang made loadable before any bindgen build.
            document = copy.deepcopy(workflow)
            document["jobs"][name]["steps"] = [step for step in document["jobs"][name]["steps"]
                                               if step.get("name") != "Make libclang loadable on Boat runners"]
            if len(document["jobs"][name]["steps"]) != len(workflow["jobs"][name]["steps"]) - 1:
                raise AssertionError(f"{name}: libclang canary target drifted")
            rejected(f"{name} without the Boat libclang step", document)
        # A same-repository PR that fell back to hosted runners must say so on its checks.
        document = copy.deepcopy(workflow)
        document["jobs"]["core"]["steps"] = [step for step in document["jobs"]["core"]["steps"]
                                             if step.get("name") != "Warn that pull request CI fell back to hosted runners"]
        if len(document["jobs"]["core"]["steps"]) != len(workflow["jobs"]["core"]["steps"]) - 1:
            raise AssertionError("core: hosted-fallback warning canary target drifted")
        rejected("core without the hosted-fallback warning", document)

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
                if condition_value(concrete) != expected:
                    raise AssertionError("Boat runner evaluation drift: " + "/".join(
                        (event, merge_variable, pr_variable, head_repository, author, actor, rerunner)))
        for name in ("core", "plan", "test", "full", "trusted-policy"):
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
            if name in ("core", "plan", "test"):
                unsafe += ["ubuntu-24.04", merge_only]
            else:
                # The required verdict and the trusted-policy control jobs stay hosted in
                # every form: a verdict queued for a Boat VM that cannot start would never report.
                unsafe += [runner, merge_only]
            for value in unsafe:
                if value == workflow["jobs"][name]["runs-on"]:
                    raise AssertionError(f"{name}: runner mutation is a no-op: {value}")
                document = copy.deepcopy(workflow)
                document["jobs"][name]["runs-on"] = value
                rejected(f"{name} unsafe runner routing", document)
        document = copy.deepcopy(workflow)
        for name in ("core", "plan", "test"):
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
    workflow_concurrency_canaries()
    workflow_concurrency_parity()
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


def main() -> int:
    if sys.argv[1:] == ["--workflow-contract"]:
        source = (ROOT / ".github/workflows/ci-policy.yml").read_text(encoding="utf-8")
        workflow_verdict_canaries(source)
        workflow_context_canaries(source)
        workflow_cleanup_canaries(source)
        workflow_concurrency_canaries()
        workflow_concurrency_parity()
        bundle_prune_containment_canaries()
        workflow_sccache_canaries(source)
        studio_browser_phase_canaries()
        studio_browser_pin_canaries()
        rust_parity_evidence_canaries()
        workflow_trusted_policy_canaries(source)
        workflow_expression_cap_canaries()
        workflow_merge_group_policy_canaries(source)
        workflow_cache_canaries()
        print("workflow contract: admission, verdict, context, concurrency and merge-group publisher canaries passed")
        return 0
    if len(sys.argv) != 1:
        raise SystemExit("usage: negative_canaries.py [--workflow-contract]")
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
