#!/usr/bin/env python3
"""Negative canaries for promoted-gate and result-attestation correspondence."""

from __future__ import annotations

import copy
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from types import ModuleType
from typing import Any, Callable

import yaml


ROOT = Path(__file__).resolve().parents[2]
GATES = Path(__file__).with_name("promoted-gates.json")
COVERAGE = Path(__file__).with_name("coverage.json")
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
    document = json.loads(COVERAGE.read_text(encoding="utf-8"))
    mutate(document)
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", encoding="utf-8") as candidate:
        json.dump(document, candidate)
        candidate.flush()
        expect_coverage_validator_rejection(
            label,
            coverage_path=Path(candidate.name),
        )


def manifest_canaries() -> None:
    subprocess.run(["python3", str(VALIDATOR)], cwd=ROOT, check=True)

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
        coverage = COVERAGE.read_text(encoding="utf-8").replace(
            json.dumps(original_command), json.dumps(gate["command"])
        )
        with tempfile.TemporaryDirectory() as directory:
            gate_path = Path(directory) / "gates.json"
            coverage_path = Path(directory) / "coverage.json"
            gate_path.write_text(json.dumps(document), encoding="utf-8")
            coverage_path.write_text(coverage, encoding="utf-8")
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
    coverage = json.loads(COVERAGE.read_text(encoding="utf-8"))
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
            coverage_path = candidate_root / "coverage.json"
            gate_path = candidate_root / "promoted-gates.json"
            obligation_path.write_text(
                json.dumps(mutated_obligations), encoding="utf-8"
            )
            coverage_path.write_text(json.dumps(mutated_coverage), encoding="utf-8")
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
        coverage_path = candidate_root / "coverage.json"
        gate_path = candidate_root / "promoted-gates.json"
        demoted_path.write_text(json.dumps(demoted_registry), encoding="utf-8")
        coverage_path.write_text(json.dumps(demoted_coverage), encoding="utf-8")
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


def workflow_verdict_canaries(source: str) -> None:
    workflow = yaml.safe_load(source)
    verdict = next(step["run"] for step in workflow["jobs"]["full"]["steps"] if "run" in step)
    full_plan = {"mode": "full", "shard": ["rest-1"], "phase": ["clippy"]}
    affected = {"mode": "affected", "shard": ["rest-1"], "phase": []}
    passed = dict.fromkeys(("CORE", "PLANNED", "TEST", "PHASE"), "success")
    cases = [(event, full_plan, passed, True) for event in ("push", "merge_group", "workflow_dispatch")]
    cases.append(("pull_request", affected, {**passed, "PHASE": "skipped"}, True))
    cases.extend((event, affected, {**passed, "PHASE": "skipped"}, False)
                 for event in ("push", "merge_group", "workflow_dispatch"))
    cases.extend(("merge_group", full_plan, {**passed, key: result}, False)
                 for key in passed for result in ("failure", "cancelled", "skipped"))
    cases.append(("pull_request", affected, {**passed, "TEST": "skipped", "PHASE": "skipped"}, False))
    for index, (event, plan, results, accepted) in enumerate(cases):
        environment = {**os.environ, **results, "EVENT": event, "PLAN": json.dumps(plan)}
        result = subprocess.run(["bash", "-c", verdict], cwd=ROOT, env=environment,
                                capture_output=True, text=True, timeout=10, check=False)
        if (result.returncode == 0) != accepted:
            raise AssertionError(f"workflow verdict case {index} had unexpected status {result.returncode}")


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

        for name in ("core", "plan", "test", "phase"):
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
                document["jobs"][name]["steps"][0]["with"][key] = value
                rejected(f"{name} checkout {key}", document)
            for key in ("BASH_ENV", "BASH_FUNC_exit%%", "LD_PRELOAD", "NORN_CI_PACKAGES", "NORN_CI_REST_SHARDS", "NORN_CI_BASE"):
                document = copy.deepcopy(workflow)
                document["jobs"][name]["steps"][-1].setdefault("env", {})[key] = "override"
                rejected(f"{name} step environment {key}", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["test"]["steps"][2]["with"]["bun-version"] = "latest"
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
        document["jobs"]["core"]["steps"].append(document["jobs"]["core"]["steps"].pop(0))
        rejected("candidate checkout after the gate", document)
        document = copy.deepcopy(workflow)
        document["jobs"]["phase"]["env"]["NORN_CI_BASE"] = "HEAD"
        rejected("unbound shrink-only baseline", document)
        candidate.write_text(source.replace("      - run: bash scripts/ci/core.sh cheap",
                                            "      - run: echo ignored\n        run: bash scripts/ci/core.sh cheap", 1), encoding="utf-8")
        expect_coverage_validator_rejection("duplicate workflow run key", formal_workflow_path=candidate)


def workflow_canaries() -> None:
    source = (ROOT / ".github/workflows/ci-policy.yml").read_text(encoding="utf-8")
    workflow_verdict_canaries(source)
    workflow_context_canaries(source)
    mutations = [
        ("missing cheap gate", "run: bash scripts/ci/core.sh cheap", "run: echo skipped"),
        ("missing test shards", "run: bash scripts/ci/core.sh test ${{ matrix.shard }}", "run: echo skipped"),
        ("missing phases", "run: bash scripts/ci/core.sh ${{ matrix.phase }}", "run: echo skipped"),
        ("missing shard matrix", "shard: ${{ fromJSON(needs.plan.outputs.plan).shard }}", "shard: [rest-1]"),
        ("missing phase matrix", "phase: ${{ fromJSON(needs.plan.outputs.plan).phase }}", "phase: [cheap]"),
        ("missing full verdict dependency", "needs: [core, plan, test, phase]", "needs: [core, plan, test]"),
        ("disabled planned tests", "if: fromJSON(needs.plan.outputs.plan).shard[0] != null", "if: false"),
        ("disabled planned phases", "if: fromJSON(needs.plan.outputs.plan).phase[0] != null", "if: false"),
        ("wrong affected base", "github.event.pull_request.base.sha || ''", "github.sha || ''"),
        ("unbound affected packages", "NORN_CI_PACKAGES: ${{ fromJSON(needs.plan.outputs.plan).packages }}", "NORN_CI_PACKAGES: norn-skeleton-guard"),
        ("unbound rest partitions", "NORN_CI_REST_SHARDS: ${{ fromJSON(needs.plan.outputs.plan).rest_shards }}", "NORN_CI_REST_SHARDS: 1"),
        ("missing full-plan branch", 'else plan=$(bash scripts/ci/core.sh plan); fi', 'else plan="{}"; fi'),
        ("forged core verdict", "CORE: ${{ needs.core.result }}", "CORE: success"),
        ("missing full-plan assertion", '[ "$mode" = full ]', 'true'),
        ("planned skipped job accepted", '[ "$2" -eq 0 ]', 'true'),
        ("cancelled main verdict skipped", "!(cancelled() && github.event_name == 'pull_request')", "!cancelled()"),
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
        command = "bash scripts/ci/core.sh test ${{ matrix.shard }}"
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
                    timeout_seconds=30,
                    quint_home=root / "quint-home",
                    label=label,
                    emit=lambda _line: None,
                )
            except quint_apalache.HarnessError as error:
                raise AssertionError(f"retry canary could not run: {label}: {error}") from error
        classes = [attempt.classification for attempt in run.attempts]
        if should_pass:
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
    coverage_path = ROOT / "verification/coverage-v1/coverage.json"
    coverage = json.loads(coverage_path.read_text(encoding="utf-8"))
    paths = set(writer.evidence_paths(coverage, coverage_path, []))
    selector_binder = "scripts/validate-executed-test-selectors.py"
    if selector_binder not in paths:
        raise AssertionError(
            "executed-test selector binder is absent from content-addressed evidence"
        )
    helper = "scripts/verification_attestation.py"
    if helper not in paths:
        raise AssertionError("shared attestation helper is absent from content-addressed evidence")

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
    if len(sys.argv) != 1:
        raise SystemExit("usage: negative_canaries.py")
    manifest_canaries()
    central_obligation_closure_canaries()
    workflow_canaries()
    quint_apalache_retry_canaries()
    ontology_quint_semantic_mutation_canaries()
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
