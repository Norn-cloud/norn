#!/usr/bin/env python3
"""Fail closed on ambiguous or inflated Norn verification claims."""

from __future__ import annotations

import copy
import hashlib
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
import coverage_manifest  # noqa: E402

CENTRAL_OBLIGATIONS = (
    ROOT / "verification/stripmine-2026-08-22/obligations.json"
)
CORE_PHYSICS_OBLIGATIONS = ROOT / "verification/core-physics-v1/obligations.json"
REQUIRED_CORE_PHYSICS_OBLIGATION_IDS = frozenset([
    "INV-CORE-COMMITMENT-COMPLETION",
    "INV-CORE-ESCROW-NON-NEGATIVE",
    "INV-CORE-LINKED-REVERSAL",
    "INV-CORE-POSTED-IMMUTABLE",
    "INV-CORE-QUANTITY-CONSERVATION",
    "INV-CORE-VALUE-CONSERVATION",
])
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
    "assurance-certificates",
    "transition-authority",
    "artifact-governance",
    "authorization",
    "quantity-ledger",
    "rules-dmn",
    "ontology",
    "cala-boundary",
    "verification-integrity",
    "kernel-v2",
    "core-physics",
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


class WorkflowLoader(yaml.SafeLoader):
    def construct_mapping(self, node: yaml.MappingNode, deep: bool = False) -> dict:
        mapping = super().construct_mapping(node, deep=deep)
        require(len(mapping) == len(node.value), "workflow mapping keys must be unique")
        return mapping


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
    workflow = yaml.load(source, Loader=WorkflowLoader)
    require(workflow.get("on", workflow.get(True)) == {
        "pull_request": {"types": ["opened", "synchronize", "reopened", "ready_for_review"]},
        "pull_request_target": None,
        "issue_comment": {"types": ["created", "edited", "deleted"]},
        "push": {"branches": ["main"]}, "merge_group": None, "workflow_dispatch": None,
    }, "workflow must preserve every trigger and admit ready_for_review")
    require(workflow.get("concurrency") == {
        "group": "${{ github.workflow }}-${{ github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || github.event_name == 'push' && 'main-push' || format('run-{0}', github.run_id) }}",
        "cancel-in-progress": "${{ github.event_name == 'pull_request' || github.event_name == 'push' }}",
    }, "workflow must cancel only superseded PR and main runs")
    require(workflow.get("run-name") == "${{ github.event_name == 'pull_request' && github.event.action == 'ready_for_review' && format('READY {0} {1} {2}', github.event.pull_request.head.sha, github.event.pull_request.updated_at, github.sha) || github.event_name }}",
            "run name must attribute READY to the triggering event and head")
    validate_rights_contract(source)
    jobs = workflow["jobs"]
    require(set(jobs) == {"core", "plan", "test", "full", "trusted-policy", "rights-capture", "rights-revocation"}, "workflow job inventory changed")
    check_names = {"core": "core", "plan": "plan", "full": "full", "trusted-policy": "trusted-policy evaluation"}
    require(all(jobs[name].get("name", name) == expected for name, expected in check_names.items()),
            "control and required check names must remain stable")
    require(not workflow.get("env"), "workflow must not inherit an execution environment")
    trusted = jobs["trusted-policy"]
    require(trusted.get("concurrency") == {
        "group": "rights-authority-v4",
        "cancel-in-progress": False, "queue": "max",
    }, "trusted-policy: superseded approvals and revocations must share a cancelling group")
    require(" ".join(trusted.get("if", "").split()) == "!cancelled() && ( "
            "github.event_name == 'pull_request_target' || "
            "(github.event_name == 'issue_comment' && github.event.issue.pull_request && "
            "(contains(github.event.comment.body, '/approve-gates') || contains(github.event.comment.body, '/ack-rights') || contains(github.event.comment.body, '/revoke-rights') || "
            "(github.event.action == 'edited' && (contains(github.event.changes.body.from, '/approve-gates') || contains(github.event.changes.body.from, '/ack-rights') || contains(github.event.changes.body.from, '/revoke-rights'))))))",
            "trusted-policy: approval and rights creation, editing and deletion must re-evaluate")
    # Complete reporter byte/execution pins are enforced by validate_rights_contract above.
    merge_runner = "${{ github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat' && 'boat' || 'ubuntu-24.04' }}"
    for name, job in jobs.items():
        expected_runner = merge_runner if name in {"core", "test"} else "ubuntu-24.04"
        require(job.get("runs-on") == expected_runner,
                f"{name}: runner must preserve merge-only Boat opt-in and hosted fallback")
    cache_env = {"SCCACHE_GHA_ENABLED": "true", "RUSTC_WRAPPER": "sccache",
                 "NORN_INTEROP_CACHE": "~/.cache/norn-interop",
                 "NORN_CI_HEAD": "${{ github.event.merge_group.head_sha || github.sha }}"}
    job_env = {
        "core": cache_env,
        "plan": {},
        "test": {**cache_env,
                 "NORN_CI_PACKAGES": "${{ fromJSON(needs.plan.outputs.plan).packages }}",
                 "NORN_CI_REST_SHARDS": "${{ fromJSON(needs.plan.outputs.plan).rest_shards }}",
                 "NORN_CI_BASE": "${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha || github.event.before || 'origin/main' }}",
                 "PLAN": "${{ needs.plan.outputs.plan }}", "BUNDLE_ID": "${{ matrix.bundle.id }}"},
    }

    def has_execution_override(job: dict, step: dict) -> bool:
        return any(key in step or key in job.get("defaults", {}).get("run", {})
                   or key in workflow.get("defaults", {}).get("run", {})
                   for key in ("shell", "working-directory"))
    required_runs = {
        "core": "bash scripts/ci/core.sh cheap",
        "plan": '''if [ -n "$BASE" ]; then plan=$(bash scripts/ci/core.sh plan --affected "$BASE" "$HEAD_SHA"); else plan=$(bash scripts/ci/core.sh plan); fi
plan=$(jq -c --arg base "$BASE" --arg head "$HEAD_SHA" '. + {base_sha: $base, head_sha: $head}' <<<"$plan")
echo "plan=$plan" >> "$GITHUB_OUTPUT"
echo "$plan"''',
        "test": 'bash scripts/ci/core.sh bundle "$PLAN" "$BUNDLE_ID"',
    }
    checkout = {"uses": "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                "with": {"persist-credentials": False, "fetch-depth": 0}}
    setup = [
        {"uses": "dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c",
         "with": {"toolchain": "1.98.0", "components": "rustfmt, clippy", "targets": "wasm32-unknown-unknown"}},
        {"uses": "oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6", "with": {"bun-version": "1.3.14"}},
        {"uses": "mozilla-actions/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba",
         "with": {"version": "v0.19.0"}},
        {"uses": "Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0",
         "with": {"cache-targets": False, "shared-key": "cargo-registry",
                  "save-if": "${{ github.ref == 'refs/heads/main' }}"}},
        {"uses": "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
         "with": {"path": "~/.bun/install/cache",
                  "key": "bun-${{ hashFiles('tools/norn-ts/bun.lock', 'packages/norn-sdk/bun.lock', 'packages/norn-demo-ui/bun.lock') }}"}},
        # Digest-pinned interop schemas: erpnext_runtime.py re-verifies every cached file.
        {"uses": "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
         "with": {"path": "~/.cache/norn-interop",
                  "key": "norn-interop-${{ hashFiles('scripts/importers/erpnext-runtime.lock.json') }}"}},
    ]
    cleanup = {"name": "Reclaim runner disk space",
               "if": "${{ !(github.event_name == 'merge_group' && runner.environment == 'self-hosted') }}",
               "run": """set -euo pipefail
minimum=$((12 * 1024 * 1024))
available() { df -Pk / | awk 'NR == 2 {print $4}'; }
df -h /
if [ "$(available)" -lt "$minimum" ]; then
  sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc /usr/local/share/boost /opt/hostedtoolcache/CodeQL
fi
df -h /
[ "$(available)" -ge "$minimum" ] || { echo "Insufficient runner space: need 12 GiB free"; df -Pk /; exit 1; }
"""}
    nextest = {"uses": "taiki-e/install-action@86f7a0ffb25f40114606501a311ebcfaf882ea32",
               "with": {"tool": "nextest@0.9.150"}}
    candidate_condition = "github.event_name != 'pull_request_target' && github.event_name != 'issue_comment'"
    for name, command in required_runs.items():
        job = jobs.get(name, {})
        admission = candidate_condition + " && !(github.event_name == 'pull_request' && github.event.pull_request.draft)"
        condition = (candidate_condition if name == "core" else admission if name == "plan"
                     else "fromJSON(needs.plan.outputs.plan).bundle[0] != null")
        require(job.get("if") == condition,
                f"{name}: candidate gate condition changed")
        require(job.get("timeout-minutes") == {"core": 110, "plan": 5, "test": 110}[name],
                f"{name}: approved time budget changed")
        require(not job.get("continue-on-error"), f"{name}: gate must fail closed")
        require(job.get("env", {}) == job_env[name],
                f"{name}: environment must bind the gate plan")
        job_steps = job.get("steps", [])
        preparation = [checkout] + ([] if name == "plan" else copy.deepcopy(setup))
        if name == "test":
            preparation.append(nextest)
        if name != "plan":
            reclaim = copy.deepcopy(cleanup)
            if name == "test":
                reclaim["if"] = "${{ runner.environment == 'github-hosted' }}"
                reclaim["run"] = reclaim["run"].replace('if [ "$(available)" -lt "$minimum" ]; then\n  ', '').replace('\nfi\ndf -h /', '\ndf -h /')
            preparation.insert(0, reclaim)
        require(json.dumps(job_steps[:-1], sort_keys=True) == json.dumps(preparation, sort_keys=True),
                f"{name}: preparation actions and inputs must match the approved contract")
        checkouts = [step for step in job_steps if step.get("uses", "").startswith("actions/checkout@")]
        require(len(checkouts) == 1 and "if" not in checkouts[0]
                and not checkouts[0].get("continue-on-error")
                and checkouts[0].get("with") == {"persist-credentials": False, "fetch-depth": 0}
                and checkouts[0]["with"]["persist-credentials"] is False
                and type(checkouts[0]["with"]["fetch-depth"]) is int,
                f"{name}: checkout must bind the default candidate repository, ref and workspace")
        require(len([step for step in job_steps if "run" in step]) == (1 if name == "plan" else 2),
                f"{name}: unexpected executable preparation step")
        require(all(not step.get("env") for step in job_steps if "run" not in step),
                f"{name}: setup steps must not override the execution environment")
        steps = [step for step in job_steps if step.get("run", "").strip() == command]
        require(len(steps) == 1 and "if" not in steps[0] and not steps[0].get("continue-on-error")
                and not has_execution_override(job, steps[0]),
                f"{name}: required gate command missing or conditional")
        require(name == "plan" or not steps[0].get("env"),
                f"{name}: step environment must not override gate inputs")
        require(job_steps.index(checkouts[0]) < job_steps.index(steps[0]),
                f"{name}: candidate checkout must precede the gate")
        if name == "plan":
            require(steps[0].get("id") == "plan" and job.get("outputs", {}).get("plan") == "${{ steps.plan.outputs.plan }}",
                    "plan: gate plan output must bind the producer step")
            require(steps[0].get("env") == {"BASE":
                    "${{ github.event_name == 'pull_request' && github.event.pull_request.base.sha || github.event_name == 'merge_group' && github.event.merge_group.base_sha || '' }}",
                    "HEAD_SHA": "${{ github.event.merge_group.head_sha || github.sha }}"},
                    "plan: affected selection must bind the event base and head")
    job = jobs["test"]
    require(job.get("needs") == ["core", "plan"] and job.get("strategy") == {
        "fail-fast": False, "matrix": {"bundle": "${{ fromJSON(needs.plan.outputs.plan).bundle }}"}
    }, "test: matrix must execute the complete bundle plan without queued surplus jobs")
    require(job.get("name") == "bundle (${{ matrix.bundle.id }})" and job.get("timeout-minutes") == 110,
            "test: bundle identity and time budget must be pinned")
    full = jobs.get("full", {})
    require(full.get("needs") == ["core", "plan", "test"], "full: verdict must depend on every gate")
    require(len(full.get("steps", [])) == 1 and not full.get("env"),
            "full: verdict must be the only step and cannot inherit shell or gate-input overrides")
    require(" ".join(full.get("if", "").split()) == "always() && " + candidate_condition
            + " && !(cancelled() && github.event_name == 'pull_request' && !github.event.pull_request.draft)"
            and not full.get("continue-on-error"),
            "full: verdict must run and fail closed")
    verdict = '''set -euo pipefail
[ "$EVENT" != pull_request ] || [ "$DRAFT" = false ] || { echo "Draft head is not admitted for full qualification"; exit 1; }
jq -e --arg base "$BASE" --arg head "$HEAD_SHA" '
  type == "object" and (.mode == "full" or .mode == "affected") and
  .base_sha == $base and .head_sha == $head and
  (.shard | type == "array" and all(.[]; type == "string" and length > 0) and length == (unique | length)) and
  (.phase | type == "array" and all(.[]; type == "string" and length > 0) and length == (unique | length)) and
  (.bundle | type == "array" and length <= 6 and all(.[];
    type == "object" and (.id | type == "string" and length > 0) and
    (.tasks | type == "array" and length > 0 and all(.[]; type == "string"))) and
    (map(.id) | length) == (map(.id) | unique | length)) and
  ([.bundle[].tasks[]] | sort) == ([(.shard[] | "test/" + .), (.phase[] | "phase/" + .)] | sort)
' <<<"$PLAN" >/dev/null || { echo "Malformed or unbound gate plan"; exit 1; }
# A job may be skipped only when the plan gave it nothing to run.
ran() { [ "$1" = success ] || { [ "$1" = skipped ] && [ "$2" -eq 0 ]; }; }
[ "$CORE" = success ] && [ "$PLANNED" = success ] || { echo "core $CORE, plan $PLANNED"; exit 1; }
mode=$(jq -er .mode <<<"$PLAN")
[ "$EVENT" = pull_request ] || [ "$EVENT" = merge_group ] || [ "$mode" = full ] || { echo "$EVENT needs the full plan, got $mode"; exit 1; }
# An affected merge-group plan must keep the always-run phases (core.sh plan).
[ "$EVENT" != merge_group ] || [ "$mode" = full ] || jq -e '(["guards", "provenance", "clippy"] - .phase) == []' <<<"$PLAN" >/dev/null || { echo "merge_group affected plan dropped an always-run phase"; exit 1; }
ran "$TEST" "$(jq -er '.bundle | length' <<<"$PLAN")" || { echo "test ended $TEST"; exit 1; }
echo "$mode plan passed: $(jq -r '.reason // "every shard and phase"' <<<"$PLAN")"'''
    verdict_env = {
        "DRAFT": "${{ github.event.pull_request.draft || false }}",
        "BASE": "${{ github.event_name == 'pull_request' && github.event.pull_request.base.sha || github.event_name == 'merge_group' && github.event.merge_group.base_sha || '' }}",
        "HEAD_SHA": "${{ github.event.merge_group.head_sha || github.sha }}",
        "EVENT": "${{ github.event_name }}", "PLAN": "${{ needs.plan.outputs.plan }}",
        "CORE": "${{ needs.core.result }}", "PLANNED": "${{ needs.plan.result }}",
        "TEST": "${{ needs.test.result }}",
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


def validate_rights_contract(source: str, merge_source: str | None = None) -> None:
    """The API collector and binary invocation remain base-owned and head-bound."""
    if merge_source is None:
        merge_source = (ROOT / ".github/workflows/trusted-merge-group.yml").read_text()
    # Pin complete base-owned API programs, including append-only history reads
    # and fail-closed publishers; substring matches cannot establish execution.
    programs = {
        'Resolve the pull request': ('6a0efef21e1a4aa9ac444b976856fdb27f43ef1a07bdfb34e004cad59c241032', ['script', 'retries'], False, None),
        'Capture the saved rights payload': ('748661200b99a0ebf6859d5638816d6ba7b60af7fb1818d2aabe9d14ff46883d', ['script'], False, None),
        'Persist the target rights ledger': ('8b60fe98a60613841c5a345cb5860f8879635d37bede68583d372720c24e30cd', ['script', 'retries'], False, None),
        'Record admin approvals of the exact PR head': ('9b6055f62178edf8178a8a25339d01018fc796fbcdc08d233796d21aa59f9b02', ['script'], False, None),
        'Report trusted policy on the exact PR head': ('223c0970b6a8f6493fcca9e8a3964528a23908cd6fa9db55c373a9db8b52985d', ['script', 'retries'], False, 'always()'),
        'Record admin approvals of the queued heads': ('3ad13b76bc4b8a2dc0d4e19bb823863bd9a6fe3678b715e3241d8e2cfa46d82a', ['script'], False, None),
        'Report trusted policy on the group head': ('ec9e3099f381b8c71706ab6b1bd0b6c56cc3a280d67f66c956988f30650199c2', ['script', 'retries'], False, 'always()'),
    }
    def api_step(step: dict) -> None:
        digest, inputs, tolerated_failure, condition = programs[step["name"]]
        allowed = {"name", "uses", "with"} | ({"continue-on-error"} if tolerated_failure else set()) | ({"if"} if condition else set())
        if step["name"] in {"Resolve the pull request", "Capture the saved rights payload"}:
            allowed.add("id")
            require(step.get("id") == ("pr" if step["name"] == "Resolve the pull request" else "payload"),
                    "trusted event output identity changed")
        if step["name"] == "Persist the target rights ledger":
            allowed.add("env")
            require(step.get("env") == {"RIGHTS_EVENT": "${{ needs.rights-capture.outputs.payload }}"},
                    "ledger must consume the captured event as data")
        require(set(step) == allowed
                and step.get("uses") == "actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3"
                and step.get("continue-on-error", False) is tolerated_failure
                and step.get("if") == condition
                and set(step.get("with", {})) == set(inputs)
                and ("retries" not in inputs or step["with"]["retries"] == 3)
                and hashlib.sha256(step.get("with", {}).get("script", "").encode()).hexdigest() == digest,
                f"trusted API program or execution changed: {step.get('name')}")
    public = yaml.load(source, Loader=WorkflowLoader)
    capture = public["jobs"].get("rights-capture", {})
    expected_capture = {
        "name": "Capture immutable rights event ${{ github.event.issue.pull_request && 'pull' || 'issue' }} ${{ (contains(github.event.comment.body, '/ack-rights') || contains(github.event.comment.body, '/revoke-rights') || contains(github.event.changes.body.from, '/ack-rights') || contains(github.event.changes.body.from, '/revoke-rights')) && 'possible' || 'none' }}",
        "if": "github.event_name == 'issue_comment'",
        "permissions": {"contents": "read"}, "runs-on": "ubuntu-24.04", "timeout-minutes": 5,
        "outputs": {key: "${{ steps.payload.outputs." + key + " }}" for key in ("heads", "kind", "comment", "payload")},
    }
    require({k:v for k,v in capture.items() if k != "steps"} == expected_capture,
            "saved run payload must be captured before API writes, including plain issues")
    require(len(capture.get("steps", [])) == 2 and capture["steps"][1] == {
        "name": "Rights event ${{ steps.payload.outputs.heads }} comment ${{ steps.payload.outputs.comment }}", "run": "true"},
        "saved identity must be stamped before the authority queue")
    api_step(capture["steps"][0])
    ledger = public["jobs"].get("rights-revocation", {})
    expected_ledger = {
        "name": "Rights ledger ${{ needs.rights-capture.outputs.heads }} comment ${{ needs.rights-capture.outputs.comment }}",
        "needs": "rights-capture",
        "if": "!cancelled() && needs.rights-capture.result == 'success' && needs.rights-capture.outputs.kind == 'pull' && needs.rights-capture.outputs.heads != 'none'",
        "permissions": {"actions": "read", "contents": "read", "statuses": "write"},
        "concurrency": {"group": "rights-authority-v4", "cancel-in-progress": False, "queue": "max"},
        "runs-on": "ubuntu-24.04", "timeout-minutes": 5,
    }
    require({k:v for k,v in ledger.items() if k != "steps"} == expected_ledger,
            "target ledger must be base-owned and never superseded")
    require(len(ledger.get("steps", [])) == 1, "missing immutable rights writer")
    api_step(ledger["steps"][0])
    for text, head in ((source, "${{ steps.pr.outputs.head_sha }}"),
                       (merge_source, "${{ github.event.workflow_run.head_sha }}")):
        workflow = yaml.load(text, Loader=WorkflowLoader)
        job = workflow["jobs"]["trusted-policy"]
        expected = ('python3 trusted/scripts/ci/validate-runner-policy.py --root candidate '
                    '--maintainer-evidence "$RUNNER_TEMP/maintainer-approval.json" '
                    '--rights-evidence "$RUNNER_TEMP/maintainer-approval.json" '
                    f'--head "{head}" --repository "${{{{ github.repository }}}}"')
        steps = job["steps"]
        allowed_job = {"permissions", "if", "concurrency", "runs-on", "timeout-minutes", "steps"}
        if text == source:
            allowed_job |= {"name", "needs"}
        require(set(job) == allowed_job, "trusted validation cannot override its execution environment")
        checks = [step for step in steps if step.get("run", "").strip() == expected]
        require(len(checks) == 1 and set(checks[0]) == {"name", "run"}
                and not job.get("env") and not workflow.get("env")
                and all(key not in scope.get("defaults", {}).get("run", {})
                        for scope in (job, workflow) for key in ("shell", "working-directory")),
                "trusted rights: missing or wrong exact-head evidence/binding command")
        require(job.get("permissions", {}).get("actions") == "read", "trusted event reconciliation requires read-only Actions access")
        is_pr = text == source
        collector = "Record admin approvals of the exact PR head" if is_pr else "Record admin approvals of the queued heads"
        reporter = "Report trusted policy on the exact PR head" if is_pr else "Report trusted policy on the group head"
        for name in (collector, reporter, *(["Resolve the pull request"] if is_pr else [])):
            matching = [step for step in steps if step.get("name") == name]
            require(len(matching) == 1, f"missing trusted API step: {name}")
            api_step(matching[0])
        if is_pr:
            require(job.get("needs") == "rights-revocation", "policy must await durable revocation recording")
            require(steps[0].get("name") == "Resolve the pull request",
                    "resolve the head before recording refusal so revocation still dequeues it")
            require(steps[1] == {"name": "Require durable revocation recording",
                    "env": {"RESULT": "${{ needs.rights-revocation.result }}"},
                    "run": 'test "$RESULT" = success || test "$RESULT" = skipped'},
                    "failed/cancelled recording must fail policy before admission")
        scripts = "\n".join(step.get("with", {}).get("script", "") for step in steps)
        compact = re.sub(r"\s+", "", scripts)
        for contract in ("rights-authority-v4", "github.rest.git.createCommit", "force:false",
                         "rights-checkpoint/target/", "rights-checkpoint/publication/",
                         "Rights capture incomplete after bounded wait", "Rights authority queue deadline exceeded"):
            require(contract in scripts, f"trusted authority contract missing: {contract}")
        require(job.get("concurrency") == {"group": "rights-authority-v4", "cancel-in-progress": False, "queue": "max"},
                "all rights authority must serialize without cancelling active or queued work")
    condition = yaml.load(source, Loader=WorkflowLoader)["jobs"]["trusted-policy"]["if"]
    for command in ("/ack-rights", "/revoke-rights"):
        require(f"contains(github.event.comment.body, '{command}')" in condition
                and f"contains(github.event.changes.body.from, '{command}')" in condition,
                "trusted rights: edits/deletions must re-evaluate rights evidence")
    runner = (ROOT / "scripts/ci/core.sh").read_text()
    require('cargo xtask ci provenance --head "${NORN_CI_HEAD:-$(git rev-parse HEAD)}" --repository Norn-cloud/norn' in runner,
            "provenance must bind the event candidate")
    require('--distribution --base-root "$PWD" --source-root "$PWD" --head "${NORN_CI_HEAD:-$(git rev-parse HEAD)}" --repository Norn-cloud/norn' in runner,
            "distribution must retain an independent immutable source binding")


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
    coverage_path: Path = coverage_manifest.COVERAGE,
    gate_manifest_source: Path | None = None,
    central_obligations_path: Path = CENTRAL_OBLIGATIONS,
    core_physics_obligations_path: Path = CORE_PHYSICS_OBLIGATIONS,
    formal_workflow_path: Path = FORMAL_WORKFLOW,
) -> int:
    """Validate canonical inputs, with explicit paths only for in-process canaries.

    Production entrypoints call this function without arguments. Keeping mutation
    inputs as explicit Python parameters prevents ambient environment variables
    from silently selecting a different security registry or claim manifest.
    """

    try:
        coverage = coverage_manifest.load(coverage_path)
    except coverage_manifest.InvalidCoverageLayout as error:
        raise InvalidCoverage(f"coverage manifest: {error}") from error
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
    core_physics_registry = load(core_physics_obligations_path)
    core_physics_ids = obligation_ids(core_physics_registry)
    require(
        len(core_physics_registry.get("obligations", [])) == len(REQUIRED_CORE_PHYSICS_OBLIGATION_IDS)
        and core_physics_ids == REQUIRED_CORE_PHYSICS_OBLIGATION_IDS,
        "core-physics obligation registry must contain exactly the norn.core law closure; "
        f"missing={sorted(REQUIRED_CORE_PHYSICS_OBLIGATION_IDS - core_physics_ids)}, "
        f"unexpected={sorted(core_physics_ids - REQUIRED_CORE_PHYSICS_OBLIGATION_IDS)}",
    )
    certificate_registry = load(ROOT / "verification/coverage-v1/assurance-certificate-obligations.json")
    certificate_obligations = certificate_registry.get("obligations")
    require(
        certificate_registry.get("registry_version") == "norn.verification.obligations.v1"
        and isinstance(certificate_obligations, list) and certificate_obligations,
        "assurance certificate obligations must be a nonempty obligation registry",
    )
    certificate_ids = obligation_ids(certificate_registry)
    require(
        len(certificate_ids) == len(certificate_obligations)
        and all(isinstance(item.get("id"), str) and item["id"].startswith("INV-CERT-")
                and item.get("priority") == "P0"
                and isinstance(item.get("statement"), str) and item["statement"].strip()
                and isinstance(item.get("assumptions"), list) and item["assumptions"]
                and all(isinstance(assumption, str) and assumption.strip()
                        for assumption in item["assumptions"])
                for item in certificate_obligations),
        "assurance certificate obligations need unique INV-CERT IDs, P0 priority, statements and assumptions",
    )
    known_obligations = (
        central_ids
        | obligation_ids(phase1_registry)
        | core_physics_ids
        | certificate_ids
    )
    p0_obligations = (
        p0_obligation_ids(central_registry)
        | p0_obligation_ids(certificate_registry)
        | p0_obligation_ids(kernel_registry, default_priority="P0")
        | p0_obligation_ids(core_physics_registry)
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

    require(
        set(by_id.get("assurance-certificates", {}).get("obligations", [])) == certificate_ids,
        "assurance-certificates must own exactly the certificate registry obligations",
    )
    core_physics_claimed = set(by_id.get("core-physics", {}).get("obligations", []))
    require(
        core_physics_claimed == REQUIRED_CORE_PHYSICS_OBLIGATION_IDS,
        "core-physics coverage must claim exactly the norn.core law closure, whatever their priority; "
        f"missing={sorted(REQUIRED_CORE_PHYSICS_OBLIGATION_IDS - core_physics_claimed)}, "
        f"unexpected={sorted(core_physics_claimed - REQUIRED_CORE_PHYSICS_OBLIGATION_IDS)}",
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

    require(
        runner_registrations == [(gate["profile"], gate["id"]) for gate in gates],
        "promoted gate order must match the runner registrations",
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
