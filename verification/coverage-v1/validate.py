#!/usr/bin/env python3
"""Fail closed on ambiguous or inflated Norn verification claims."""

from __future__ import annotations

import json
import re
import shlex
import sys
from collections import Counter
from pathlib import Path
from typing import Any

import jsonschema
import yaml


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from verification_attestation import selector_is_declared  # noqa: E402
from proof_enrollment import validate as validate_enrollment  # noqa: E402

COVERAGE = ROOT / "verification/coverage-v1/coverage.json"
CENTRAL_OBLIGATIONS = (
    ROOT / "verification/stripmine-2026-08-22/obligations.json"
)
KERNEL_OBLIGATIONS = ROOT / "verification/archive/kernel-v1/obligations.json"
KERNEL_OBLIGATIONS_SCHEMA = (
    ROOT / "verification/archive/kernel-v1/schemas/obligations.schema.json"
)
REQUIRED_CENTRAL_OBLIGATION_IDS = frozenset(['INV-ART-AUTHORITY-PROMOTION', 'INV-ART-DIRECT-WRITE-PREDICATE', 'INV-AUTH-ATOMIC-COMMIT', 'INV-AUTH-CAPABILITY-NOW', 'INV-AUTH-CLOSED-CUSTOM-EFFECT', 'INV-AUTH-DOMAIN-ADMISSION-PREDICATE', 'INV-AUTH-HARD-INVARIANT-OWNERSHIP', 'INV-AUTH-IDEMPOTENT-RESULT', 'INV-AUTH-SCOPE-CLOSED', 'INV-AUTH-STALE-REJECT', 'INV-ONT-MAPPING-PROVENANCE', 'INV-ONT-OPEN-CLOSED-SEPARATION', 'INV-ONT-RELATION-AUTHORITY', 'INV-QL-APPEND-REVERSAL', 'INV-QL-AUTHORITY-BRIDGE-ATOMIC', 'INV-QL-BATCH-ATOMIC', 'INV-QL-CERTIFICATE-POSTING-BINDING', 'INV-QL-CONSERVATION', 'INV-QL-EXACT-ARITHMETIC', 'INV-QL-PENDING-RESOLUTION', 'INV-QL-REFERENCE-BALANCE-ALGEBRA', 'INV-QL-UNIT-DIMENSION', 'INV-RULE-CLOSED-DETERMINISTIC-EVALUATION', 'INV-RULE-HIT-POLICY-ADMISSION-PREDICATE', 'INV-RULE-HIT-POLICY-SAFETY', 'INV-VER-DIFFERENTIAL-REFINEMENT', 'INV-VER-NO-PROOF-HOLES'])

REQUIRED_KERNEL_OBLIGATIONS = {
    "KERNEL-V1-T1": "The committed state and receipt trace are a pure function of the durable history; replaying that history reconstructs the committed state and `trace_sha256`.",
    "KERNEL-V1-T2": "An exact retry of a committed transition returns the recorded outcome and mutates nothing; a changed-input retry under the same idempotency key is rejected.",
    "KERNEL-V1-T3": "A transition commits only if the owner version it read is the owner version at commit; concurrent writers cannot both commit against the same version.",
    "KERNEL-V1-T4": "A crash at any point between durable writes leaves the run in a state from which the recover step reaches a committed or requeued outcome; no run, task, timer, or signal is stranded.",
    "KERNEL-V1-T5": "A closed action's external effect is recorded at most once per commit; replay/retry does not re-emit it.",
    "KERNEL-V1-T6": "Every committed transition records the exact policy/artifact versions it evaluated, and those were active at evaluation time.",
    "KERNEL-V1-T7": "Absent scope is never widened to a wildcard; a capability or scope mismatch denies; Allow requires an exact identity-tuple match.",
}
REQUIRED_KERNEL_OBLIGATION_IDS = frozenset(REQUIRED_KERNEL_OBLIGATIONS)
REQUIRED_KERNEL_MODEL_OBLIGATION_IDS = frozenset(
    obligation_id
    for obligation_id in REQUIRED_KERNEL_OBLIGATION_IDS
    if obligation_id != "KERNEL-V1-T7"
)
REQUIRED_KERNEL_INVARIANTS = frozenset(
    {
        "typeOk",
        "replayDeterministic",
        "idempotentAtMostOnce",
        "casSafe",
        "noStrandedAfterRecover",
        "effectAtMostOnce",
        "committedPolicyWasPinned",
    }
)
MANDATORY = {
    "transition-authority",
    "artifact-governance",
    "authorization",
    "quantity-ledger",
    "rules-dmn",
    "ontology",
    "cala-boundary",
    "verification-integrity",
    "kernel-v2",
}
LAYERS = ("runtime", "model", "proof")
GATE_ROLES = {
    "claim-evidence",
    "supporting-check",
    "typecheck",
    "manifest-validation",
    "toolchain-integrity",
    "evidence-integrity",
    "compatibility",
}
EVIDENCE_KINDS = {"command", "rust-tests", "bun-tests"}
BOUNDED = {"exhaustive_bounded", "bounded_model_checked", "bounded_static_checked"}
NO_EVIDENCE = {"missing", "deferred"}
NON_EVIDENCE = NO_EVIDENCE
SELECTOR = re.compile(r"^(test|theorem|invariant|predicate|harness|fixture|checker):.+$")
FORMAL_WORKFLOW = ROOT / ".github/workflows/ci-policy.yml"
CI_WORKFLOWS = (FORMAL_WORKFLOW,)
PROMOTED_RUNNER = ROOT / "scripts/verify-promoted-subsystems.sh"
GATE_PROFILES = {"fast", "full"}
QUINT_APALACHE_HARNESS = ROOT / "scripts/quint_apalache.py"
NEGATIVE_CANARIES = ROOT / "verification/coverage-v1/negative_canaries.py"
PROFILE_JOBS = {"fast": "core", "full": "core"}
# Nominal wall-clock of everything in a profile's job except its slowest
# Quint/Apalache check: setup, tool bootstrap, and every other gate at typical
# speed. Observed 2026-09-30 (#2830): fast setup ~2.5 min plus non-kernel gates
# 2-7 min; full nightly ~61 min in total including the ~8 min kernel check.
PROFILE_NOMINAL_REST_SECONDS = {"fast": 15 * 60, "full": 60 * 60}
# Policy cap: the PR lane must stay fast even in its worst case.
FAST_PROFILE_WORST_CASE_CAP_SECONDS = 30 * 60
SELECTOR_KINDS_BY_STATUS = {
    ("runtime", "tested"): {"test"},
    ("runtime", "tested-under-simulation"): {"test"},
    ("runtime", "executable_reference"): {"test"},
    ("runtime", "exhaustive_bounded"): {"test", "predicate"},
    ("runtime", "partial"): {"test", "checker"},
    ("model", "bounded_model_checked"): {"invariant"},
    ("model", "executable_reference"): {"fixture", "test"},
    ("model", "exhaustive_bounded"): {"test"},
    ("model", "partial"): {"invariant", "fixture", "checker"},
    ("proof", "implementation_proved"): {"theorem", "predicate"},
    ("proof", "reference_proved"): {"theorem", "harness"},
    ("proof", "partial"): {"theorem", "predicate", "harness"},
}
REQUIRED_SELECTOR_KINDS_BY_STATUS = {
    ("runtime", "tested"): {"test"},
    ("runtime", "tested-under-simulation"): {"test"},
    ("runtime", "executable_reference"): {"test"},
    ("runtime", "exhaustive_bounded"): {"test"},
    ("model", "bounded_model_checked"): {"invariant"},
    ("model", "exhaustive_bounded"): {"test"},
    ("proof", "reference_proved"): {"theorem"},
    ("proof", "implementation_proved"): {"theorem", "predicate"},
}


class InvalidCoverage(Exception):
    pass


def load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise InvalidCoverage(f"{path.relative_to(ROOT)}: {error}") from error
    if not isinstance(value, dict):
        raise InvalidCoverage(f"{path.relative_to(ROOT)}: root must be an object")
    return value


def require(condition: bool, message: str) -> None:
    if not condition:
        raise InvalidCoverage(message)


def obligation_ids(document: dict[str, Any]) -> set[str]:
    values: set[str] = set()
    for item in document.get("obligations", []):
        require(isinstance(item, dict), "obligation registry entries must be objects")
        value = item.get("id", item.get("obligationId"))
        require(isinstance(value, str) and value, "obligation registry entry has no id")
        values.add(value)
    return values


def p0_obligation_ids(document: dict[str, Any], *, default_priority: str | None = None) -> set[str]:
    values: set[str] = set()
    for item in document.get("obligations", []):
        require(isinstance(item, dict), "obligation registry entries must be objects")
        value = item.get("id", item.get("obligationId"))
        require(isinstance(value, str) and value, "obligation registry entry has no id")
        priority = item.get("priority", default_priority)
        if priority == "P0":
            values.add(value)
    return values


def validate_kernel_obligation_registry(kernel_registry: dict[str, Any]) -> None:
    """Keep the V1 lifecycle registry closed over its checked model and plan IDs."""

    schema = load(KERNEL_OBLIGATIONS_SCHEMA)
    try:
        jsonschema.Draft202012Validator(schema).validate(kernel_registry)
    except jsonschema.ValidationError as error:
        location = "/".join(str(part) for part in error.absolute_path) or "<root>"
        raise InvalidCoverage(
            f"kernel obligation registry violates its schema at {location}: {error.message}"
        ) from error

    obligations = kernel_registry.get("obligations")
    require(
        isinstance(obligations, list),
        "kernel obligation registry obligations must be an array",
    )
    kernel_ids = obligation_ids(kernel_registry)
    require(
        len(obligations) == len(REQUIRED_KERNEL_OBLIGATION_IDS)
        and kernel_ids == REQUIRED_KERNEL_OBLIGATION_IDS,
        "kernel obligation registry must contain exactly the immutable T1-T7 closure; "
        f"missing={sorted(REQUIRED_KERNEL_OBLIGATION_IDS - kernel_ids)}, "
        f"unexpected={sorted(kernel_ids - REQUIRED_KERNEL_OBLIGATION_IDS)}",
    )
    by_id = {item["id"]: item for item in obligations}
    require(
        len(by_id) == len(obligations),
        "kernel obligation registry must not contain duplicate obligation IDs",
    )
    for obligation_id, statement in REQUIRED_KERNEL_OBLIGATIONS.items():
        obligation = by_id[obligation_id]
        require(
            obligation["statement"] == statement,
            f"kernel obligation statement drifted from PLAN.md §1: {obligation_id}",
        )
        evidence = obligation["evidence"]
        if obligation["referenceStatus"] == "none":
            require(
                not evidence,
                f"{obligation_id}: referenceStatus none must not cite evidence",
            )
        else:
            require(
                evidence,
                f"{obligation_id}: a specified/executed reference status needs evidence",
            )
        if obligation["modelStatus"] == "none":
            require(
                not obligation["modelEvidence"],
                f"{obligation_id}: modelStatus none must not cite a model",
            )
        else:
            require(
                obligation["modelEvidence"],
                f"{obligation_id}: model status needs model evidence",
            )

    models = kernel_registry.get("models")
    require(isinstance(models, list), "kernel obligation registry models must be an array")
    model_ids = [model["modelId"] for model in models]
    require(
        model_ids == ["KERNEL-MODEL-LIFECYCLE-V1"],
        "kernel obligation registry must name exactly the lifecycle model",
    )
    model = models[0]
    require(
        set(model["invariants"]) == REQUIRED_KERNEL_INVARIANTS,
        "kernel lifecycle model must declare exactly the promoted T1-T6 invariants",
    )
    require(
        set(model["obligationIds"]) == REQUIRED_KERNEL_MODEL_OBLIGATION_IDS,
        "kernel lifecycle model must cover exactly the T1-T6 obligations; T7 belongs to authz-v1",
    )
    model_path = KERNEL_OBLIGATIONS.parent / model["path"]
    require(model_path.is_file(), f"kernel lifecycle model is missing: {model['path']}")
    for obligation in obligations:
        if obligation["modelStatus"] != "none":
            require(
                obligation["modelEvidence"] == model_ids,
                f"{obligation['id']}: model evidence must name the lifecycle model",
            )
    scope_obligation = by_id["KERNEL-V1-T7"]
    require(
        scope_obligation["modelStatus"] == "none"
        and not scope_obligation["modelEvidence"],
        "KERNEL-V1-T7 must not claim kernel-lifecycle model evidence",
    )
    require(
        "authorization-aeneas-extraction" in scope_obligation["evidence"]
        and scope_obligation["referenceStatus"] == "executed"
        and "PR #2451" in scope_obligation["residual"],
        "KERNEL-V1-T7 must bind the executed Aeneas authorization proof from PR #2451",
    )


def validate_kernel_evidence_gate_ids(
    kernel_registry: dict[str, Any], gate_ids: set[str]
) -> None:
    for obligation in kernel_registry["obligations"]:
        unknown = set(obligation["evidence"]) - gate_ids
        require(
            not unknown,
            f"{obligation['id']}: evidence names unknown promoted gate IDs: {sorted(unknown)}",
        )


def workflow_path_blocks(source: str) -> list[set[str]]:
    """Extract every YAML `paths:` list without accepting unrelated `path:` inputs."""

    lines = source.splitlines()
    blocks: list[set[str]] = []
    index = 0
    while index < len(lines):
        match = re.match(r"^(\s*)paths:\s*$", lines[index])
        if match is None:
            index += 1
            continue
        indent = len(match.group(1))
        block: set[str] = set()
        index += 1
        while index < len(lines):
            line = lines[index]
            if not line.strip():
                index += 1
                continue
            line_indent = len(line) - len(line.lstrip())
            if line_indent <= indent:
                break
            item = re.match(r"^\s*-\s+(['\"])([^'\"]+)\1\s*$", line)
            require(item is not None, "workflow paths entries must be simple quoted literals")
            block.add(item.group(2))
            index += 1
        require(block, "workflow paths block must not be empty")
        blocks.append(block)
    return blocks


def path_filter_covers(root: str, pattern: str, *, is_directory: bool) -> bool:
    if pattern.endswith("/**"):
        prefix = pattern[:-3].rstrip("/")
        return root == prefix or root.startswith(f"{prefix}/")
    return not is_directory and pattern == root


def validate_rust_toolchain_steps(path: Path, source: str) -> None:
    """Bind the exact Rust version to each dtolnay action step.

    The checked-in workflows use a strict two-space mapping indentation. Keeping
    this parser deliberately narrow avoids adding a YAML runtime dependency while
    preventing unrelated or nested ``toolchain`` keys from satisfying the check.
    """

    lines = source.splitlines()
    action_count = 0
    for index, line in enumerate(lines):
        action = re.match(
            r"^(\s*)-\s+uses:\s*dtolnay/rust-toolchain@[0-9a-f]{40}(?:\s*#.*)?$",
            line,
        )
        if action is None:
            continue
        action_count += 1
        step_indent = len(action.group(1))
        with_index: int | None = None
        cursor = index + 1
        while cursor < len(lines):
            candidate = lines[cursor]
            if not candidate.strip() or candidate.lstrip().startswith("#"):
                cursor += 1
                continue
            candidate_indent = len(candidate) - len(candidate.lstrip())
            if candidate_indent <= step_indent:
                break
            if re.fullmatch(rf"\s{{{step_indent + 2}}}with:\s*(?:#.*)?", candidate):
                require(
                    with_index is None,
                    f"{path.name}:{index + 1}: Rust action has duplicate with mappings",
                )
                with_index = cursor
            cursor += 1

        require(
            with_index is not None,
            f"{path.name}:{index + 1}: Rust action must have a direct with mapping",
        )
        toolchain_lines: list[int] = []
        cursor = with_index + 1
        while cursor < len(lines):
            candidate = lines[cursor]
            if not candidate.strip() or candidate.lstrip().startswith("#"):
                cursor += 1
                continue
            candidate_indent = len(candidate) - len(candidate.lstrip())
            if candidate_indent <= step_indent + 2:
                break
            if re.match(rf"^\s{{{step_indent + 4}}}toolchain:\s*", candidate):
                toolchain_lines.append(cursor)
            cursor += 1
        require(
            len(toolchain_lines) == 1,
            f"{path.name}:{index + 1}: Rust action must have exactly one direct toolchain input",
        )
        toolchain_line = lines[toolchain_lines[0]]
        require(
            re.fullmatch(
                rf"\s{{{step_indent + 4}}}toolchain:\s*['\"]?1\.98\.0['\"]?\s*(?:#.*)?",
                toolchain_line,
            )
            is not None,
            f"{path.name}:{toolchain_lines[0] + 1}: Rust action toolchain must be exactly 1.98.0",
        )

    require(action_count > 0, f"{path.name}: workflow must contain a Rust toolchain action")


def validate_workflow_integrity(evidence_roots: list[str], *, formal_path: Path = FORMAL_WORKFLOW) -> None:
    source = formal_path.read_text(encoding="utf-8")
    validate_rust_toolchain_steps(formal_path, source)
    for action in re.findall(r"^\s*- uses: (\S+)", source, re.MULTILINE):
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}", action) is not None, f"workflow action must be content-pinned: {action}")
    for trigger in ("pull_request", "pull_request_target", "push", "merge_group"):
        require(re.search(rf"^  {trigger}:", source, re.MULTILINE) is not None, f"missing required trigger {trigger}")
    # Bind commands to executable steps and the matrices that cover the plan;
    # comments, environment values and a cheap-only invocation earn no credit.
    workflow = yaml.safe_load(source)
    jobs = workflow["jobs"]

    def has_execution_override(job: dict, step: dict) -> bool:
        return any(key in step or key in job.get("defaults", {}).get("run", {})
                   or key in workflow.get("defaults", {}).get("run", {})
                   for key in ("shell", "working-directory"))
    required_runs = {
        "core": "bash scripts/ci/core.sh cheap",
        "plan": '''if [ -n "$BASE" ]; then plan=$(bash scripts/ci/core.sh plan --affected "$BASE"); else plan=$(bash scripts/ci/core.sh plan); fi
echo "plan=$plan" >> "$GITHUB_OUTPUT"
echo "$plan"''',
        "test": "bash scripts/ci/core.sh test ${{ matrix.shard }}",
        "phase": "bash scripts/ci/core.sh ${{ matrix.phase }}",
    }
    candidate_condition = "github.event_name != 'pull_request_target' && github.event_name != 'issue_comment'"
    for name, command in required_runs.items():
        job = jobs.get(name, {})
        condition = (candidate_condition if name in {"core", "plan"}
                     else f"fromJSON(needs.plan.outputs.plan).{('shard' if name == 'test' else 'phase')}[0] != null")
        require(job.get("if") == condition,
                f"{name}: candidate gate condition changed")
        require(not job.get("continue-on-error"), f"{name}: gate must fail closed")
        steps = [step for step in job.get("steps", []) if step.get("run", "").strip() == command]
        require(len(steps) == 1 and "if" not in steps[0] and not steps[0].get("continue-on-error")
                and not has_execution_override(job, steps[0]),
                f"{name}: required gate command missing or conditional")
        if name == "plan":
            require(steps[0].get("id") == "plan" and job.get("outputs", {}).get("plan") == "${{ steps.plan.outputs.plan }}",
                    "plan: gate plan output must bind the producer step")
            require(steps[0].get("env", {}).get("BASE") ==
                    "${{ github.event_name == 'pull_request' && github.event.pull_request.base.sha || '' }}",
                    "plan: affected selection must bind the pull request base")
    for name, axis in (("test", "shard"), ("phase", "phase")):
        job = jobs[name]
        require(job.get("needs") == "plan" and job.get("strategy", {}).get("matrix") == {
            axis: "${{ fromJSON(needs.plan.outputs.plan)." + axis + " }}"
        }, f"{name}: matrix must execute the complete gate plan")
    for key, field in (("NORN_CI_PACKAGES", "packages"), ("NORN_CI_REST_SHARDS", "rest_shards")):
        require(jobs["test"].get("env", {}).get(key) ==
                "${{ fromJSON(needs.plan.outputs.plan)." + field + " }}",
                f"test: {key} must bind the gate plan")
    full = jobs.get("full", {})
    require(full.get("needs") == ["core", "plan", "test", "phase"], "full: verdict must depend on every gate")
    require(" ".join(full.get("if", "").split()) == "always() && " + candidate_condition
            + " && !(cancelled() && github.event_name == 'pull_request')"
            and not full.get("continue-on-error"),
            "full: verdict must run and fail closed")
    verdict = '''set -euo pipefail
# A job may be skipped only when the plan gave it nothing to run.
ran() { [ "$1" = success ] || { [ "$1" = skipped ] && [ "$2" -eq 0 ]; }; }
[ "$CORE" = success ] && [ "$PLANNED" = success ] || { echo "core $CORE, plan $PLANNED"; exit 1; }
mode=$(jq -er .mode <<<"$PLAN")
[ "$EVENT" = pull_request ] || [ "$mode" = full ] || { echo "$EVENT needs the full plan, got $mode"; exit 1; }
ran "$TEST" "$(jq -er '.shard | length' <<<"$PLAN")" || { echo "test ended $TEST"; exit 1; }
ran "$PHASE" "$(jq -er '.phase | length' <<<"$PLAN")" || { echo "phase ended $PHASE"; exit 1; }
echo "$mode plan passed: $(jq -r '.reason // "every shard and phase"' <<<"$PLAN")"'''
    verdict_env = {
        "EVENT": "${{ github.event_name }}", "PLAN": "${{ needs.plan.outputs.plan }}",
        "CORE": "${{ needs.core.result }}", "PLANNED": "${{ needs.plan.result }}",
        "TEST": "${{ needs.test.result }}", "PHASE": "${{ needs.phase.result }}",
    }
    require(any(step.get("run", "").strip() == verdict and "if" not in step
                and not step.get("continue-on-error")
                and not has_execution_override(full, step)
                and step.get("env") == verdict_env
                for step in full.get("steps", [])), "full: every dependency must succeed")
    runner = (ROOT / "scripts/ci/core.sh").read_text()
    require("scripts/verify-promoted-subsystems.sh full" in runner, "full generic proof/model gates must run")
    require('"${nextest[@]}" run --no-fail-fast "${selection[@]}"' in runner and "--ignore-default-filter" in runner,
            "full workspace tests must run")
    require("cargo test --locked --workspace --doc" in runner, "workspace doctests must run")
    require("test-shards" in runner, "test shards must be proved to cover the full test set")
    require("cargo clippy --locked --workspace --all-targets -- -D warnings" in runner, "full workspace clippy must run")


def validate_timeout_budgets(
    gates: list[dict[str, Any]],
    *,
    formal_path: Path = FORMAL_WORKFLOW,
    runner_path: Path = PROMOTED_RUNNER,
) -> None:
    """Keep per-gate Quint timeouts consistent with the workflow job budgets.

    Worst case per profile = slowest per-attempt Quint timeout x MAX_ATTEMPTS
    (one retry without a verdict) + the profile's nominal rest. It must fit the
    job's `timeout-minutes`, and the fast profile must stay within 30 minutes,
    so a raised gate timeout can never silently outgrow its job (#2830).
    """

    runner = runner_path.read_text(encoding="utf-8")
    default_match = re.search(r"^quint_default_timeout_seconds=(\d+)$", runner, re.MULTILINE)
    require(default_match is not None, "promoted runner must set quint_default_timeout_seconds")
    default_timeout = int(default_match.group(1))
    overrides_match = re.search(
        r"^declare -A quint_gate_timeout_seconds=\((?P<body>.*?)^\)$",
        runner,
        re.MULTILINE | re.DOTALL,
    )
    require(overrides_match is not None, "promoted runner must declare quint_gate_timeout_seconds")
    overrides = {
        gate_id: int(seconds)
        for gate_id, seconds in re.findall(
            r"^\s*\[([a-z0-9-]+)\]=(\d+)\s*$", overrides_match.group("body"), re.MULTILINE
        )
    }
    quint_gate_ids = {gate["id"] for gate in gates if gate["command"].startswith("quint verify ")}
    for gate_id in overrides:
        require(
            gate_id in quint_gate_ids,
            f"quint timeout override names no promoted Quint verify gate: {gate_id}",
        )

    attempts_match = re.search(
        r"^MAX_ATTEMPTS = (\d+)$",
        QUINT_APALACHE_HARNESS.read_text(encoding="utf-8"),
        re.MULTILINE,
    )
    require(attempts_match is not None, "quint_apalache.py must define MAX_ATTEMPTS")
    max_attempts = int(attempts_match.group(1))
    formal_source = formal_path.read_text(encoding="utf-8")
    for profile, job in PROFILE_JOBS.items():
        job_match = re.search(
            rf"^  {re.escape(job)}:\n(?P<body>.*?)(?=^  [a-z][a-z0-9_-]*:\n|\Z)",
            formal_source,
            re.MULTILINE | re.DOTALL,
        )
        require(job_match is not None, f"formal verification workflow is missing {job}")
        minutes_match = re.search(
            r"^    timeout-minutes: (\d+)\s*(?:#.*)?$", job_match.group("body"), re.MULTILINE
        )
        require(minutes_match is not None, f"{job} must declare a job-level timeout-minutes")
        job_budget = int(minutes_match.group(1)) * 60

        timeouts = [
            overrides.get(gate["id"], default_timeout)
            for gate in gates
            if gate["command"].startswith("quint verify ")
            and (profile == "full" or gate["profile"] == "fast")
        ]
        require(timeouts, f"{profile} profile has no Quint verify gates")
        slowest = max(timeouts)
        worst_case = slowest * max_attempts + PROFILE_NOMINAL_REST_SECONDS[profile]
        require(
            worst_case <= job_budget,
            f"{job}: worst case {worst_case}s (slowest Quint timeout {slowest}s x "
            f"{max_attempts} attempts + {PROFILE_NOMINAL_REST_SECONDS[profile]}s nominal rest) "
            f"exceeds timeout-minutes budget {job_budget}s",
        )
        if profile == "fast":
            require(
                worst_case <= FAST_PROFILE_WORST_CASE_CAP_SECONDS,
                f"{job}: worst case {worst_case}s exceeds the "
                f"{FAST_PROFILE_WORST_CASE_CAP_SECONDS}s fast-lane cap; move the slow "
                "gate to the full profile or lower its timeout",
            )


def validate_selector_gate_order(
    registrations: list[tuple[str, str]], gates: list[dict[str, Any]]
) -> None:
    positions = {gate_id: index for index, (_, gate_id) in enumerate(registrations)}
    for profile, selector_gate in (
        ("full", "verification-executed-test-selectors"),
        ("fast", "verification-executed-test-selectors-fast"),
    ):
        require(selector_gate in positions, f"missing selector validator: {selector_gate}")
        for gate in gates:
            if gate["evidenceKind"] == "rust-tests" and gate["profile"] == profile:
                require(
                    gate["id"] in positions
                    and positions[gate["id"]] < positions[selector_gate],
                    f"{selector_gate}: producing gate {gate['id']} must run first",
                )


def validate_claim_evidence_shape(
    subsystem_id: str,
    claim_id: str,
    claim: dict[str, Any],
) -> None:
    evidence = claim.get("evidence")
    require(
        isinstance(evidence, dict) and set(evidence) == set(LAYERS),
        f"{subsystem_id}.{claim_id}: evidence must define exactly runtime, model, and proof",
    )
    for layer_name in LAYERS:
        entries = evidence[layer_name]
        require(
            isinstance(entries, list),
            f"{subsystem_id}.{claim_id}.evidence.{layer_name}: must be an array",
        )
        status = claim[layer_name]
        if status in NO_EVIDENCE:
            require(
                not entries,
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: {status} must have empty evidence",
            )
        else:
            require(
                entries,
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: {status} needs exact evidence",
            )
        identities: set[tuple[str, str, tuple[str, ...]]] = set()
        observed_selector_kinds: set[str] = set()
        for entry in entries:
            require(
                isinstance(entry, dict)
                and set(entry) == {"artifact", "command", "selectors"},
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: malformed evidence entry",
            )
            artifact = entry["artifact"]
            command = entry["command"]
            selectors = entry["selectors"]
            require(
                isinstance(artifact, str) and artifact,
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: artifact is required",
            )
            require(
                isinstance(command, str)
                and command.strip()
                and "\n" not in command
                and "\t" not in command,
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: command is required",
            )
            require(
                isinstance(selectors, list) and selectors,
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: named selectors are required",
            )
            require(
                len(selectors) == len(set(selectors)),
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: duplicate selector",
            )
            for selector in selectors:
                require(
                    isinstance(selector, str)
                    and SELECTOR.fullmatch(selector) is not None
                    and "\n" not in selector,
                    f"{subsystem_id}.{claim_id}.evidence.{layer_name}: invalid selector {selector!r}",
                )
                selector_kind = selector.split(":", 1)[0]
                observed_selector_kinds.add(selector_kind)
                allowed_kinds = SELECTOR_KINDS_BY_STATUS.get((layer_name, status))
                require(
                    allowed_kinds is not None and selector_kind in allowed_kinds,
                    f"{subsystem_id}.{claim_id}.evidence.{layer_name}: selector kind "
                    f"{selector_kind!r} cannot support status {status!r}",
                )
            identity = (artifact, command, tuple(selectors))
            require(
                identity not in identities,
                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: duplicate evidence entry",
            )
            identities.add(identity)
        required_kinds = REQUIRED_SELECTOR_KINDS_BY_STATUS.get((layer_name, status), set())
        require(
            required_kinds.issubset(observed_selector_kinds),
            f"{subsystem_id}.{claim_id}.evidence.{layer_name}: status {status!r} "
            f"requires selector kinds {sorted(required_kinds)}, got {sorted(observed_selector_kinds)}",
        )


def validate(
    *,
    coverage_path: Path = COVERAGE,
    gate_manifest_source: Path | None = None,
    central_obligations_path: Path = CENTRAL_OBLIGATIONS,
    formal_workflow_path: Path = FORMAL_WORKFLOW,
) -> int:
    """Validate canonical inputs, with explicit paths only for in-process canaries.

    Production entrypoints call this function without arguments. Keeping mutation
    inputs as explicit Python parameters prevents ambient environment variables
    from silently selecting a different security registry or claim manifest.
    """

    coverage = load(coverage_path)
    require(coverage.get("schemaVersion") == "norn.verification.coverage.v1", "wrong schemaVersion")
    gate_manifest_path = coverage.get("promotedGateManifest")
    require(
        isinstance(gate_manifest_path, str)
        and gate_manifest_path == "verification/coverage-v1/promoted-gates.json",
        "promotedGateManifest must name the canonical promoted gate manifest",
    )
    if gate_manifest_source is None:
        gate_manifest_source = ROOT / gate_manifest_path
    gate_manifest = load(gate_manifest_source)
    require(
        gate_manifest.get("schemaVersion") == "norn.verification.promoted-gates.v1",
        "wrong promoted gate manifest schemaVersion",
    )
    try:
        present_domains = validate_enrollment(ROOT, coverage, gate_manifest)
    except ValueError as error:
        raise InvalidCoverage(str(error)) from error
    vocabulary = coverage.get("statusVocabulary")
    require(isinstance(vocabulary, list) and vocabulary, "statusVocabulary must be non-empty")
    statuses = set(vocabulary)
    definitions = coverage.get("statusDefinitions")
    require(isinstance(definitions, dict), "statusDefinitions must be an object")
    require(
        set(definitions) == statuses,
        "statusDefinitions must define exactly every statusVocabulary value",
    )
    for status, definition in definitions.items():
        require(
            isinstance(definition, str) and definition.strip(),
            f"statusDefinitions.{status} must be non-empty",
        )
    evidence_roots = coverage.get("evidenceRoots")
    require(isinstance(evidence_roots, list) and evidence_roots, "evidenceRoots must be non-empty")
    require(len(evidence_roots) == len(set(evidence_roots)), "evidenceRoots must be unique")
    for root in evidence_roots:
        require(
            isinstance(root, str)
            and root
            and not root.startswith("/")
            and ".." not in Path(root).parts
            and not root.startswith("target/"),
            f"invalid evidence root {root}",
        )
        require((ROOT / root).exists(), f"missing evidence root {root}")
    validate_workflow_integrity(evidence_roots, formal_path=formal_workflow_path)
    central_registry = load(central_obligations_path)
    kernel_registry = {"obligations": []}
    import runpy
    runpy.run_path(str(ROOT / "verification/kernel-v2/validate.py"))["validate"]()
    central_obligations = central_registry.get("obligations")
    require(
        isinstance(central_obligations, list),
        "central obligation registry obligations must be an array",
    )
    central_ids = obligation_ids(central_registry)
    require(
        len(central_obligations) == len(REQUIRED_CENTRAL_OBLIGATION_IDS)
        and central_ids == REQUIRED_CENTRAL_OBLIGATION_IDS,
        "central obligation registry must contain exactly the retained public-core ID closure; "
        f"missing={sorted(REQUIRED_CENTRAL_OBLIGATION_IDS - central_ids)}, "
        f"unexpected={sorted(central_ids - REQUIRED_CENTRAL_OBLIGATION_IDS)}",
    )
    phase1_registry = load(ROOT / "verification/kernel-v2/manifest.json")
    known_obligations = central_ids | obligation_ids(phase1_registry)
    p0_obligations = p0_obligation_ids(central_registry) | p0_obligation_ids(
        kernel_registry, default_priority="P0"
    )

    subsystems = coverage.get("subsystems")
    require(isinstance(subsystems, list), "subsystems must be an array")
    by_id: dict[str, dict[str, Any]] = {}
    for subsystem in subsystems:
        require(isinstance(subsystem, dict), "subsystem entries must be objects")
        subsystem_id = subsystem.get("id")
        require(isinstance(subsystem_id, str) and subsystem_id, "subsystem id is required")
        require(subsystem_id not in by_id, f"duplicate subsystem {subsystem_id}")
        by_id[subsystem_id] = subsystem

        obligations = subsystem.get("obligations")
        require(isinstance(obligations, list) and obligations, f"{subsystem_id}: obligations must be non-empty")
        require(len(obligations) == len(set(obligations)), f"{subsystem_id}: duplicate obligation reference")
        unknown = set(obligations) - known_obligations
        require(not unknown, f"{subsystem_id}: unknown obligations: {sorted(unknown)}")

        obligation_claims = subsystem.get("obligationClaims")
        require(isinstance(obligation_claims, list), f"{subsystem_id}: obligationClaims must be an array")
        claims_by_id: dict[str, dict[str, Any]] = {}
        for claim in obligation_claims:
            require(isinstance(claim, dict), f"{subsystem_id}: obligation claim must be an object")
            claim_id = claim.get("id")
            require(isinstance(claim_id, str) and claim_id, f"{subsystem_id}: obligation claim id is required")
            require(claim_id not in claims_by_id, f"{subsystem_id}: duplicate obligation claim {claim_id}")
            claims_by_id[claim_id] = claim
            require(
                isinstance(claim.get("residual"), str) and claim["residual"].strip(),
                f"{subsystem_id}.{claim_id}: residual is required",
            )
            for layer_name in LAYERS:
                require(
                    claim.get(layer_name) in statuses,
                    f"{subsystem_id}.{claim_id}.{layer_name}: unsupported status {claim.get(layer_name)}",
                )
            validate_claim_evidence_shape(subsystem_id, claim_id, claim)
        require(
            set(claims_by_id) == set(obligations),
            f"{subsystem_id}: obligationClaims must cover exactly the declared obligations",
        )

        limitations = subsystem.get("limitations")
        require(isinstance(limitations, list) and limitations, f"{subsystem_id}: limitations must be explicit")

        for layer_name in LAYERS:
            layer = subsystem.get(layer_name)
            require(isinstance(layer, dict), f"{subsystem_id}.{layer_name}: layer is required")
            status = layer.get("status")
            require(status in statuses, f"{subsystem_id}.{layer_name}: unsupported status {status}")
            artifacts = layer.get("artifacts")
            commands = layer.get("commands")
            result_artifacts = layer.get("resultArtifacts", [])
            generated_artifacts = layer.get("generatedArtifacts")
            require(isinstance(artifacts, list), f"{subsystem_id}.{layer_name}: artifacts must be an array")
            require(isinstance(commands, list), f"{subsystem_id}.{layer_name}: commands must be an array")
            require(
                isinstance(result_artifacts, list),
                f"{subsystem_id}.{layer_name}: resultArtifacts must be an array",
            )
            require(
                isinstance(generated_artifacts, list),
                f"{subsystem_id}.{layer_name}: generatedArtifacts must be an array",
            )
            if status not in NON_EVIDENCE:
                require(artifacts and commands, f"{subsystem_id}.{layer_name}: evidence-bearing status needs artifacts and commands")
            else:
                require(
                    not artifacts and not commands and not result_artifacts and not generated_artifacts,
                    f"{subsystem_id}.{layer_name}: {status} must not declare positive evidence",
                )
            for artifact in artifacts:
                require(isinstance(artifact, str) and artifact, f"{subsystem_id}.{layer_name}: bad artifact path")
                require((ROOT / artifact).is_file(), f"{subsystem_id}.{layer_name}: missing artifact {artifact}")
            for command in commands:
                require(
                    isinstance(command, str)
                    and command.strip()
                    and "\n" not in command
                    and "\t" not in command,
                    f"{subsystem_id}.{layer_name}: invalid command",
                )
            require(
                len(result_artifacts) == len(set(result_artifacts)),
                f"{subsystem_id}.{layer_name}: duplicate result artifact",
            )
            for artifact in result_artifacts:
                require(
                    isinstance(artifact, str)
                    and artifact.startswith("target/verification-results/")
                    and artifact.endswith(".json"),
                    f"{subsystem_id}.{layer_name}: invalid generated result artifact {artifact}",
                )
            require(
                len(generated_artifacts) == len(set(generated_artifacts)),
                f"{subsystem_id}.{layer_name}: duplicate generated artifact",
            )
            for artifact in generated_artifacts:
                require(
                    isinstance(artifact, str)
                    and artifact.startswith("target/verification-results/")
                    and len(Path(artifact).parts) > 2
                    and ".." not in Path(artifact).parts,
                    f"{subsystem_id}.{layer_name}: invalid generated implementation artifact {artifact}",
                )
            has_quint_verify = any(command.startswith("quint verify ") for command in commands)
            require(
                bool(result_artifacts) == has_quint_verify,
                f"{subsystem_id}.{layer_name}: Quint verification and generated result artifacts must be declared together",
            )
            obligation_statuses = {claim[layer_name] for claim in obligation_claims}
            if status in BOUNDED or obligation_statuses & BOUNDED:
                require(
                    isinstance(layer.get("bound"), str) and layer["bound"],
                    f"{subsystem_id}.{layer_name}: bounded aggregate or obligation evidence needs a bound",
                )
            if status == "reference_proved":
                require(layer.get("scope") == "reference", f"{subsystem_id}.{layer_name}: reference proof must say scope=reference")
            implementation_claims = [
                claim_id
                for claim_id, claim in claims_by_id.items()
                if claim[layer_name] == "implementation_proved"
            ]
            correspondence = layer.get("correspondenceArtifact")
            if status == "implementation_proved" or implementation_claims:
                require(isinstance(correspondence, str), f"{subsystem_id}.{layer_name}: implementation proof needs correspondenceArtifact")
                require((ROOT / correspondence).is_file(), f"{subsystem_id}.{layer_name}: missing correspondenceArtifact")
            elif correspondence is not None:
                require(
                    isinstance(correspondence, str) and (ROOT / correspondence).is_file(),
                    f"{subsystem_id}.{layer_name}: invalid correspondenceArtifact",
                )

            declared_artifacts = set(artifacts) | set(generated_artifacts)
            declared_commands = set(commands)
            referenced_generated: set[str] = set()
            for claim_id, claim in claims_by_id.items():
                for entry in claim["evidence"][layer_name]:
                    require(
                        entry["artifact"] in declared_artifacts,
                        f"{subsystem_id}.{claim_id}.evidence.{layer_name}: "
                        f"artifact is not declared by the layer: {entry['artifact']}",
                    )
                    require(
                        entry["command"] in declared_commands,
                        f"{subsystem_id}.{claim_id}.evidence.{layer_name}: "
                        f"command is not declared by the layer: {entry['command']}",
                    )
                    if not entry["artifact"].startswith("target/verification-results/"):
                        artifact_path = ROOT / entry["artifact"]
                        for selector in entry["selectors"]:
                            require(
                                selector_is_declared(artifact_path, selector),
                                f"{subsystem_id}.{claim_id}.evidence.{layer_name}: "
                                f"selector {selector!r} is not a matching declaration/value "
                                f"in {entry['artifact']}",
                            )
                    if entry["artifact"] in generated_artifacts:
                        referenced_generated.add(entry["artifact"])
            require(
                referenced_generated == set(generated_artifacts),
                f"{subsystem_id}.{layer_name}: generated artifacts must be referenced by exact obligation evidence",
            )

            if status in NO_EVIDENCE:
                require(
                    obligation_statuses == {status},
                    f"{subsystem_id}.{layer_name}: aggregate {status} disagrees with obligation claims {sorted(obligation_statuses)}",
                )
            elif status == "partial":
                require(
                    obligation_statuses - NO_EVIDENCE,
                    f"{subsystem_id}.{layer_name}: partial needs at least one evidence-bearing obligation",
                )
                require(
                    len(obligation_statuses) > 1 or obligation_statuses == {"partial"},
                    f"{subsystem_id}.{layer_name}: partial must represent mixed or explicitly partial evidence",
                )
            else:
                require(
                    obligation_statuses == {status},
                    f"{subsystem_id}.{layer_name}: aggregate {status} disagrees with obligation claims {sorted(obligation_statuses)}",
                )

    require(set(by_id) == MANDATORY | present_domains, f"mandatory subsystem mismatch: expected {sorted(MANDATORY)}, got {sorted(by_id)}")

    lean_composite_gates = coverage.get("leanCompositeGates", {})
    require(isinstance(lean_composite_gates, dict), "leanCompositeGates must be an object")
    for proof_path, command in lean_composite_gates.items():
        require(
            isinstance(proof_path, str)
            and proof_path.startswith("verification/")
            and proof_path.endswith(".lean")
            and ".." not in Path(proof_path).parts
            and (ROOT / proof_path).is_file(),
            f"invalid composite Lean proof path {proof_path!r}",
        )
        require(
            isinstance(command, str)
            and command.startswith("scripts/")
            and command.endswith(".sh")
            and ".." not in Path(command).parts
            and (ROOT / command).is_file(),
            f"invalid composite Lean gate command {command!r}",
        )
        owners = [
            subsystem["id"]
            for subsystem in subsystems
            if proof_path in subsystem["proof"]["artifacts"]
            and command in subsystem["proof"]["commands"]
        ]
        require(
            len(owners) == 1,
            f"composite Lean gate must have exactly one proof-layer owner: {proof_path}",
        )

    gates = gate_manifest.get("gates")
    require(isinstance(gates, list) and gates, "promoted gate manifest gates must be non-empty")
    gate_ids: set[str] = set()
    classified_commands: Counter[tuple[str, str, str]] = Counter()
    classified_results: Counter[tuple[str, str, str]] = Counter()
    classified_generated: Counter[tuple[str, str, str]] = Counter()
    claim_evidence_commands = {
        (subsystem["id"], layer_name, entry["command"])
        for subsystem in subsystems
        for claim in subsystem["obligationClaims"]
        for layer_name in LAYERS
        for entry in claim["evidence"][layer_name]
    }
    for gate in gates:
        require(isinstance(gate, dict), "promoted gate entries must be objects")
        require(
            set(gate)
            == {
                "id",
                "profile",
                "command",
                "evidenceKind",
                "role",
                "classifications",
                "resultArtifacts",
                "generatedArtifacts",
            },
            "promoted gate entry has an unexpected or missing field",
        )
        gate_id = gate["id"]
        profile = gate["profile"]
        command = gate["command"]
        require(
            isinstance(gate_id, str) and re.fullmatch(r"[a-z0-9][a-z0-9-]*", gate_id),
            f"invalid promoted gate id {gate_id!r}",
        )
        require(gate_id not in gate_ids, f"duplicate promoted gate id {gate_id}")
        gate_ids.add(gate_id)
        require(profile in GATE_PROFILES, f"{gate_id}: invalid profile {profile!r}")
        require(
            isinstance(command, str)
            and command.strip()
            and "\n" not in command
            and "\t" not in command,
            f"{gate_id}: command is required and must fit one TSV field",
        )
        command_args = shlex.split(command)
        if command_args[0] == "cargo":
            cargo_args = command_args[:command_args.index("--")] if "--" in command_args else command_args
            require(
                "--locked" in cargo_args,
                f"{gate_id}: promoted Cargo commands must pass --locked before any -- boundary",
            )
        require(
            gate["evidenceKind"] in EVIDENCE_KINDS,
            f"{gate_id}: invalid evidenceKind {gate['evidenceKind']!r}",
        )
        require(gate["role"] in GATE_ROLES, f"{gate_id}: invalid role {gate['role']!r}")
        classifications = gate["classifications"]
        require(
            isinstance(classifications, list) and classifications,
            f"{gate_id}: at least one subsystem/layer classification is required",
        )
        classification_pairs: set[tuple[str, str]] = set()
        for classification in classifications:
            require(
                isinstance(classification, dict)
                and set(classification) == {"subsystem", "layer"},
                f"{gate_id}: malformed classification",
            )
            subsystem_id = classification["subsystem"]
            layer_name = classification["layer"]
            require(subsystem_id in by_id, f"{gate_id}: unknown subsystem {subsystem_id!r}")
            require(layer_name in LAYERS, f"{gate_id}: invalid layer {layer_name!r}")
            pair = (subsystem_id, layer_name)
            require(pair not in classification_pairs, f"{gate_id}: duplicate classification {pair}")
            classification_pairs.add(pair)
            require(
                command in by_id[subsystem_id][layer_name]["commands"],
                f"{gate_id}: command is not declared by {subsystem_id}.{layer_name}",
            )
            classified_commands[(subsystem_id, layer_name, command)] += 1

        backs_exact_claim = any(
            (subsystem_id, layer_name, command) in claim_evidence_commands
            for subsystem_id, layer_name in classification_pairs
        )
        if gate["role"] == "claim-evidence":
            require(backs_exact_claim, f"{gate_id}: claim-evidence role has no exact obligation evidence")
        if gate["role"] == "supporting-check":
            require(
                not backs_exact_claim,
                f"{gate_id}: supporting-check is cited as exact obligation evidence",
            )

        for field, counter, layer_field in (
            ("resultArtifacts", classified_results, "resultArtifacts"),
            ("generatedArtifacts", classified_generated, "generatedArtifacts"),
        ):
            artifacts = gate[field]
            require(isinstance(artifacts, list), f"{gate_id}.{field}: must be an array")
            require(len(artifacts) == len(set(artifacts)), f"{gate_id}.{field}: duplicates are forbidden")
            for artifact in artifacts:
                require(
                    isinstance(artifact, str) and artifact.startswith("target/verification-results/"),
                    f"{gate_id}.{field}: invalid artifact {artifact!r}",
                )
                for subsystem_id, layer_name in classification_pairs:
                    require(
                        artifact in by_id[subsystem_id][layer_name].get(layer_field, []),
                        f"{gate_id}.{field}: {artifact} is not declared by "
                        f"{subsystem_id}.{layer_name}",
                    )
                    counter[(subsystem_id, layer_name, artifact)] += 1
        require(
            not gate["resultArtifacts"] or command.startswith("quint verify "),
            f"{gate_id}: only an exact Quint verify gate may own Quint result artifacts",
        )

    runner_registrations = re.findall(
        r"^run_gate (fast|full) ([a-z0-9][a-z0-9-]*)\b",
        PROMOTED_RUNNER.read_text(encoding="utf-8"),
        re.MULTILINE,
    )
    runner_ids = [gate_id for _, gate_id in runner_registrations]
    require(
        len(runner_ids) == len(set(runner_ids)),
        "promoted runner must not register a gate id more than once",
    )
    runner_profiles = {gate_id: profile for profile, gate_id in runner_registrations}
    manifest_profiles = {gate["id"]: gate["profile"] for gate in gates}
    require(
        runner_profiles == manifest_profiles,
        "promoted gate profiles must match the runner registrations; "
        f"runner={runner_profiles}, manifest={manifest_profiles}",
    )

    validate_selector_gate_order(runner_registrations, gates)

    validate_timeout_budgets(gates, formal_path=formal_workflow_path)
    # Archived UPE gate IDs are historical, never promoted current evidence.
    require(not any(g["id"].startswith("kernel-lifecycle-") or g["id"].startswith("rules-dmn-") or g["id"].startswith("rules-hit-policy-") for g in gates),
            "archived UPE/RuleIR gate cannot be promoted")

    for proof_path, command in lean_composite_gates.items():
        matching = [
            gate
            for gate in gates
            if gate["command"] == command
            and gate["role"] == "claim-evidence"
            and any(
                classification["layer"] == "proof"
                for classification in gate["classifications"]
            )
        ]
        require(
            len(matching) == 1,
            f"composite Lean proof must have exactly one promoted proof claim gate: {proof_path}",
        )

    declared_commands = Counter(
        (subsystem["id"], layer_name, command)
        for subsystem in subsystems
        for layer_name in LAYERS
        for command in subsystem[layer_name]["commands"]
    )
    require(
        classified_commands == declared_commands,
        "promoted gate classifications must own every declared subsystem/layer command exactly once; "
        f"missing={sorted((declared_commands - classified_commands).elements())}, "
        f"extra={sorted((classified_commands - declared_commands).elements())}",
    )
    declared_results = Counter(
        (subsystem["id"], layer_name, artifact)
        for subsystem in subsystems
        for layer_name in LAYERS
        for artifact in subsystem[layer_name].get("resultArtifacts", [])
    )
    require(
        classified_results == declared_results,
        "promoted gates must own every generated result exactly at its declared layer; "
        f"missing={sorted((declared_results - classified_results).elements())}, "
        f"extra={sorted((classified_results - declared_results).elements())}",
    )
    declared_generated = Counter(
        (subsystem["id"], layer_name, artifact)
        for subsystem in subsystems
        for layer_name in LAYERS
        for artifact in subsystem[layer_name].get("generatedArtifacts", [])
    )
    require(
        classified_generated == declared_generated,
        "promoted gates must own every generated implementation artifact exactly at its declared layer; "
        f"missing={sorted((declared_generated - classified_generated).elements())}, "
        f"extra={sorted((classified_generated - declared_generated).elements())}",
    )
    obligation_owners = Counter(
        obligation
        for subsystem in subsystems
        for obligation in subsystem["obligations"]
    )
    duplicate_owners = sorted(
        obligation for obligation, count in obligation_owners.items() if count != 1
    )
    require(
        not duplicate_owners,
        f"obligations must have exactly one coverage owner: {duplicate_owners}",
    )
    p0_owners = Counter(
        obligation for obligation in obligation_owners if obligation in p0_obligations
    )
    missing_p0 = sorted(p0_obligations - set(p0_owners))
    duplicate_p0 = sorted(obligation for obligation, count in p0_owners.items() if count != 1)
    require(not missing_p0, f"unreferenced P0 obligations: {missing_p0}")
    require(not duplicate_p0, f"P0 obligations must have exactly one coverage owner: {duplicate_p0}")
    implementation_claim_count = sum(
        1
        for subsystem in subsystems
        for claim in subsystem["obligationClaims"]
        for layer_name in LAYERS
        if claim[layer_name] == "implementation_proved"
    )
    print(
        f"validated {len(by_id)} verification coverage domains; "
        f"{implementation_claim_count} exact implementation correspondence claim(s)"
    )
    return 0


def main() -> int:
    return validate()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except InvalidCoverage as error:
        print(f"coverage validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
