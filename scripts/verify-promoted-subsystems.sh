#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
verification_profile="${NORN_VERIFICATION_PROFILE:-full}"
if [[ $# -gt 0 ]]; then
  verification_profile="$1"
  shift
fi
if [[ $# -ne 0 || ( "$verification_profile" != "fast" && "$verification_profile" != "full" ) ]]; then
  printf 'usage: %s [fast|full] (or set NORN_VERIFICATION_PROFILE)\n' "$0" >&2
  exit 2
fi
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
result_root="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}"
ontology_tool_root="${NORN_ONTOLOGY_TOOL_ROOT:-$repo_root/target/ontology-tools}"
gate_log_root="$result_root/logs"
passed_commands="$result_root/passed-commands.tsv"
canonical_tool_root="$(realpath -m "$repo_root/target/verification-tools")"
canonical_result_root="$(realpath -m "$repo_root/target/verification-results")"
canonical_ontology_tool_root="$(realpath -m "$repo_root/target/ontology-tools")"

# Full attestations retain the repository-local tool root so their evidence is
# reproducible from the checked-out tree. The PR fast profile may reuse the
# shared, checksum-pinned tool cache supplied by the verification environment.
if [[ "$verification_profile" == "full" && "$(realpath -m "$tool_root")" != "$canonical_tool_root" ]]; then
  printf 'promoted verification requires NORN_VERIFICATION_TOOL_ROOT=%s\n' \
    "$canonical_tool_root" >&2
  exit 1
fi
if [[ "$(realpath -m "$result_root")" != "$canonical_result_root" ]]; then
  printf 'promoted verification requires NORN_VERIFICATION_RESULT_ROOT=%s\n' \
    "$canonical_result_root" >&2
  exit 1
fi
if [[ "$(realpath -m "$ontology_tool_root")" != "$canonical_ontology_tool_root" ]]; then
  printf 'promoted verification requires NORN_ONTOLOGY_TOOL_ROOT=%s\n' \
    "$canonical_ontology_tool_root" >&2
  exit 1
fi
for input_override in \
  NORN_VERIFICATION_CENTRAL_OBLIGATIONS \
  NORN_VERIFICATION_COVERAGE \
  NORN_VERIFICATION_GATE_MANIFEST \
  NORN_VERIFICATION_WORKFLOW; do
  if [[ -v "$input_override" ]]; then
    printf 'promoted verification rejects ambient canonical-input override: %s\n' \
      "$input_override" >&2
    exit 1
  fi
done

python3 "$repo_root/scripts/proof_enrollment.py"

export PATH="$tool_root/bin:$PATH"
export ELAN_HOME="$tool_root/elan"
export KANI_HOME="$tool_root/kani-home"
export QUINT_HOME="$tool_root/quint-home"
# Apalache's launcher accepts several environment overrides that can replace
# the attested JAR/JVM behavior or alter checker configuration. Promoted model
# gates use only their declared argv and the pinned QUINT_HOME distribution.
unset APALACHE_JAR CONFIG_FILE JVM_ARGS JVM_GC_ARGS JAVA_TOOL_OPTIONS \
  JDK_JAVA_OPTIONS _JAVA_OPTIONS OUT_DIR RUN_DIR SMT_ENCODING WRITE_INTERMEDIATE

mkdir -p "$gate_log_root" "$QUINT_HOME" "$result_root/kani-target"
: > "$passed_commands"

# Every Quint/Apalache model gate runs through scripts/quint_apalache.py: one
# harness-owned pinned Apalache server per check on a fresh loopback port, a
# check-start watchdog, a per-attempt timeout, and at most one retry that is
# limited to server-side startup/connection/deadline failures or a missing
# verdict. Counterexamples and timeouts of a started check are never retried.
# Policy: verification/README.md ("Quint/Apalache execution policy").
apalache_launcher="$QUINT_HOME/apalache-dist-0.56.1/apalache/bin/apalache-mc"
quint_apalache_runner="$repo_root/scripts/quint_apalache.py"
# Per-attempt wall-clock budget (seconds). Typical fast-lane model checks
# finish in under 20 s on CI. INVARIANT (enforced by validate_timeout_budgets in
# verification/coverage-v1/validate.py): for each profile, the
# slowest per-attempt timeout x MAX_ATTEMPTS plus the profile's nominal rest
# must fit the workflow job's timeout-minutes, and the fast profile's worst
# case must stay within 30 minutes. Raising a timeout here, or moving a gate
# with an override into the fast profile, fails the coverage-manifest gate
# unless the budgets are updated together.
quint_default_timeout_seconds=300
declare -A quint_gate_timeout_seconds=(
  # full profile only; 380-680 s nominal on a 4-vCPU runner (max-steps 13).
)

if [[ ! -x "$apalache_launcher" ]]; then
  printf 'missing pinned Apalache launcher: %s\n' "$apalache_launcher" >&2
  exit 1
fi
if [[ ! -x "$tool_root/bin/java" ]]; then
  printf 'missing pinned Java launcher: %s\n' "$tool_root/bin/java" >&2
  exit 1
fi

dump_log_tail() {
  local label="$1"
  local path="$2"
  printf '%s\n' "--- $label (last 100 lines; full log: $path) ---" >&2
  if [[ -s "$path" ]]; then
    tail -n 100 "$path" >&2 || true
  else
    printf '%s\n' '(empty or unavailable)' >&2
  fi
}

dump_gate_result_diagnostics() {
  local result_path="$1"
  if [[ -z "$result_path" ]]; then
    return
  fi
  printf '%s\n' "--- gate --out diagnostics (last 100 lines / 64 KiB; full output: $result_path) ---" >&2
  if [[ ! -s "$result_path" ]]; then
    printf '%s\n' '(empty or unavailable)' >&2
    return
  fi
  python3 - "$result_path" <<'PY' >&2
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    payload = json.loads(path.read_text(encoding="utf-8"))
except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
    print(f"could not parse gate --out JSON: {error}")
    raw = path.read_bytes()[-65536:]
    print(raw.decode("utf-8", errors="replace"))
    raise SystemExit(0)

errors = payload.get("errors") if isinstance(payload, dict) else None
if not errors:
    print("(gate --out JSON contains no errors field)")
    raise SystemExit(0)

text = json.dumps(errors, ensure_ascii=False, indent=2)
text = "\n".join(text.splitlines()[-100:])
encoded = text.encode("utf-8")
if len(encoded) > 65536:
    text = encoded[-65536:].decode("utf-8", errors="replace")
print(text)
PY
}

report_gate_failure() {
  local gate_id="$1"
  local log_path="$2"
  local command_status="$3"
  local tee_status="$4"
  local pipeline_status="$5"
  local result_path="$6"

  printf '[fail] %s exited (command=%s, tee=%s, pipeline=%s)\n' \
    "$gate_id" "$command_status" "$tee_status" "$pipeline_status" >&2
  dump_log_tail "gate stdout+stderr" "$log_path"
  dump_gate_result_diagnostics "$result_path"
}

verify_gate_artifacts() {
  local gate_id="$1"
  python3 - \
    "$repo_root/verification/coverage-v1/promoted-gates.json" \
    "$repo_root" \
    "$gate_id" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
repo_root = pathlib.Path(sys.argv[2]).resolve()
gate_id = sys.argv[3]
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))["gates"]
matches = [gate for gate in manifest if gate["id"] == gate_id]
if len(matches) != 1:
    raise SystemExit(f"promoted gate identity is not unique: {gate_id}")
for field in ("resultArtifacts", "generatedArtifacts"):
    for relative in matches[0][field]:
        artifact = repo_root / relative
        if not artifact.is_file() or artifact.stat().st_size == 0:
            raise SystemExit(
                f"promoted gate {gate_id} did not create nonempty {field} artifact: {relative}"
            )
PY
}

run_gate() {
  local gate_profile="$1"
  local gate_id="$2"
  local declared_command="$3"
  local evidence_kind="$4"
  shift 4
  if [[ "$verification_profile" == "fast" && "$gate_profile" == "full" ]]; then
    return
  fi
  local base_command=("$@")
  local -a gate_command
  local log_path="$gate_log_root/$gate_id.log"
  local result_path=""
  local executed_command
  local observed_test_count=0
  local -a pipeline_statuses
  local command_status
  local tee_status
  local pipeline_status
  local argument_index

  for (( argument_index = 0; argument_index < ${#base_command[@]}; argument_index += 1 )); do
    if [[ "${base_command[argument_index]}" == "--out" ]] && (( argument_index + 1 < ${#base_command[@]} )); then
      result_path="${base_command[argument_index + 1]}"
      break
    fi
  done

  printf -v executed_command '%q ' "$@"
  executed_command="${executed_command% }"

  gate_command=("${base_command[@]}")
  if [[ "${base_command[0]:-}" == "quint" && "${base_command[1]:-}" == "verify" ]]; then
    gate_command=(
      python3 "$quint_apalache_runner"
      --label "$gate_id"
      --timeout "${quint_gate_timeout_seconds[$gate_id]:-$quint_default_timeout_seconds}"
      --result-root "$result_root"
      -- "${base_command[@]}"
    )
  fi

  printf '\n[%s] %s\n' "$gate_id" "$declared_command"
  local gate_started=$SECONDS
  if "${gate_command[@]}" 2>&1 | tee "$log_path"; then
    pipeline_statuses=("${PIPESTATUS[@]}")
  else
    pipeline_statuses=("${PIPESTATUS[@]}")
  fi
  command_status="${pipeline_statuses[0]:-125}"
  tee_status="${pipeline_statuses[1]:-125}"
  pipeline_status=0
  if (( command_status != 0 )); then
    pipeline_status="$command_status"
  fi
  if (( tee_status != 0 )); then
    pipeline_status="$tee_status"
  fi
  if (( pipeline_status != 0 )); then
    report_gate_failure \
      "$gate_id" "$log_path" "$command_status" "$tee_status" "$pipeline_status" "$result_path"
    return "$pipeline_status"
  fi

  # A successful producer process is not evidence unless it created every
  # artifact the canonical gate manifest assigns to that exact gate. Check at
  # the producer boundary so a missing/suppressed `--out` fails immediately,
  # while the result writer still rechecks the complete set after all gates.
  if ! verify_gate_artifacts "$gate_id"; then
    printf '[fail] %s did not produce its required artifacts\n' "$gate_id" >&2
    dump_log_tail "gate stdout+stderr" "$log_path"
    dump_gate_result_diagnostics "$result_path"
    return 1
  fi

  case "$evidence_kind" in
    command)
      observed_test_count=0
      ;;
    rust-tests)
      observed_test_count="$({
        # Strip terminal colour sequences first: CI runs cargo with CARGO_TERM_COLOR=always.
        sed -E $'s/\x1b\\[[0-9;?]*[ -\\/]*[@-~]//g' "$log_path" \
          | sed -nE 's/.*test result: ok\. ([0-9]+) passed.*/\1/p' || true
      } | awk '{ total += $1 } END { print total + 0 }')"
      if (( observed_test_count == 0 )); then
        printf '[fail] %s executed zero Rust tests\n' "$gate_id" | tee -a "$log_path" >&2
        return 1
      fi
      ;;
    bun-tests)
      observed_test_count="$({
        sed -nE 's/^[[:space:]]*([0-9]+)[[:space:]]+pass([[:space:]].*)?$/\1/p' "$log_path" || true
      } | awk '{ total += $1 } END { print total + 0 }')"
      if (( observed_test_count == 0 )); then
        printf '[fail] %s executed zero Bun tests\n' "$gate_id" | tee -a "$log_path" >&2
        return 1
      fi
      ;;
    *)
      printf '[fail] %s has unsupported evidence kind %s\n' "$gate_id" "$evidence_kind" | tee -a "$log_path" >&2
      return 1
      ;;
  esac

  printf '[pass] %s (%ss)\n' "$gate_id" "$((SECONDS - gate_started))" | tee -a "$log_path"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$gate_id" \
    "$declared_command" \
    "$executed_command" \
    "$evidence_kind" \
    "$observed_test_count" \
    "${log_path#"$repo_root/"}" >> "$passed_commands"
}

cd "$repo_root"

# Cached and repeated local runs must never attest a prior producer's output.
# Remove only the exact manifest-owned evidence/result files and gate logs;
# every producing gate must recreate its outputs during this invocation.
python3 - \
  verification/coverage-v1/coverage.json \
  verification/coverage-v1/promoted-gates.json \
  "$result_root" <<'PY'
import json
import pathlib
import sys

coverage = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
gates = json.loads(pathlib.Path(sys.argv[2]).read_text(encoding="utf-8"))["gates"]
result_root = pathlib.Path(sys.argv[3]).resolve()
canonical_prefix = pathlib.PurePosixPath("target/verification-results")
owned = {
    artifact
    for subsystem in coverage["subsystems"]
    for layer in ("runtime", "model", "proof")
    for field in ("resultArtifacts", "generatedArtifacts")
    for artifact in subsystem[layer].get(field, [])
}
owned.add("target/verification-results/formal-results.json")
for artifact in sorted(owned):
    relative = pathlib.PurePosixPath(artifact)
    try:
        suffix = relative.relative_to(canonical_prefix)
    except ValueError as error:
        raise SystemExit(f"refusing to clean non-result artifact: {artifact}") from error
    path = result_root.joinpath(*suffix.parts)
    if path.is_dir() and not path.is_symlink():
        raise SystemExit(f"refusing to clean a directory as one evidence file: {path}")
    path.unlink(missing_ok=True)

for gate in gates:
    log = result_root / "logs" / f"{gate['id']}.log"
    if log.is_dir() and not log.is_symlink():
        raise SystemExit(f"refusing to clean a directory as one gate log: {log}")
    log.unlink(missing_ok=True)
PY

run_gate fast tool-versions "scripts/check-verification-tools.sh" command \
  scripts/check-verification-tools.sh
run_gate full extraction-tool-versions "scripts/check-rust-extraction-tools.sh" command \
  scripts/check-rust-extraction-tools.sh
run_gate fast source-enrollment-regressions "python3 scripts/test_proof_enrollment.py" command \
  python3 scripts/test_proof_enrollment.py
run_gate fast coverage-manifest "python3 verification/coverage-v1/validate.py" command \
  python3 verification/coverage-v1/validate.py

run_gate fast authority-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/authority-commit.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/authority-commit.qnt
run_gate fast authority-quint \
  "quint verify verification/stripmine-2026-08-22/models/authority-commit.qnt --backend apalache --invariants typeOk atomicCommit idempotentAtMostOnce acceptedOnlyWithCurrentCapability committedPolicyWasPinned noOpPreservesCanonical --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/authority-commit.qnt \
    --backend apalache \
    --invariants typeOk atomicCommit idempotentAtMostOnce \
      acceptedOnlyWithCurrentCapability committedPolicyWasPinned \
      noOpPreservesCanonical \
    --max-steps 8 \
    --out "$result_root/authority-commit-quint.json"
run_gate fast transition-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/OwnerVersionCas.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/OwnerVersionCas.lean
run_gate fast transition-closed-custom-effect-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ClosedActionEffect.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ClosedActionEffect.lean
run_gate fast transition-closed-custom-effect-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/closed-action-effect.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/closed-action-effect.qnt
run_gate fast transition-closed-custom-effect-quint \
  "quint verify verification/stripmine-2026-08-22/models/closed-action-effect.qnt --backend apalache --invariants typeOk oneClosedEffectPerCommit acceptedBoundExactDefinitions acceptedPassedClosedChecks noOpPreservesAuthority --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/closed-action-effect.qnt \
    --backend apalache \
    --invariants typeOk oneClosedEffectPerCommit acceptedBoundExactDefinitions \
      acceptedPassedClosedChecks noOpPreservesAuthority \
    --max-steps 8 \
    --out "$result_root/closed-action-effect-quint.json"
run_gate fast transition-domain-authority-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/domain-engine-boundary.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/domain-engine-boundary.qnt
run_gate fast transition-domain-authority-quint \
  "quint verify verification/stripmine-2026-08-22/models/domain-engine-boundary.qnt --backend apalache --invariants typeOk genericNeverCommitsDomainOwned domainNeverCommitsGenericOwned domainCommitMatchesExactOwner rejectionIsNoOp --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/domain-engine-boundary.qnt \
    --backend apalache \
    --invariants typeOk genericNeverCommitsDomainOwned \
      domainNeverCommitsGenericOwned domainCommitMatchesExactOwner rejectionIsNoOp \
    --max-steps 8 \
    --out "$result_root/domain-engine-boundary-quint.json"
run_gate fast transition-domain-authority-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/DomainEngineBoundary.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/DomainEngineBoundary.lean
run_gate full artifact-direct-write-predicate-runtime \
  "cargo test --locked -p norn-ir --test artifact_governance" rust-tests \
  cargo test --locked -p norn-ir --test artifact_governance
run_gate fast artifact-governance-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/artifact-promotion.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/artifact-promotion.qnt
run_gate fast artifact-governance-quint \
  "quint verify verification/stripmine-2026-08-22/models/artifact-promotion.qnt --backend apalache --invariants typeOk authorityRequiresExactAppliedPlan --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/artifact-promotion.qnt \
    --backend apalache \
    --invariants typeOk authorityRequiresExactAppliedPlan \
    --max-steps 8 \
    --out "$result_root/artifact-promotion-quint.json"
run_gate fast artifact-governance-active-pointer-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/active-artifact-authority.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/active-artifact-authority.qnt
run_gate fast artifact-governance-active-pointer-quint \
  "quint verify verification/stripmine-2026-08-22/models/active-artifact-authority.qnt --backend apalache --invariants typeOk activePointerReferencesEnvelope immutableV1History exactlyOneExecutable competingV1PlansCannotBothPublish --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/active-artifact-authority.qnt \
    --backend apalache \
    --invariants typeOk activePointerReferencesEnvelope immutableV1History \
      exactlyOneExecutable competingV1PlansCannotBothPublish \
    --max-steps 8 \
    --out "$result_root/active-artifact-authority-quint.json"
run_gate fast artifact-governance-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ArtifactPromotion.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ArtifactPromotion.lean
run_gate fast artifact-governance-active-pointer-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ActiveArtifactAuthority.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ActiveArtifactAuthority.lean
run_gate fast artifact-governance-checker-receipt-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ArtifactVerificationReceipt.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/ArtifactVerificationReceipt.lean
run_gate full artifact-governance-rust-extraction \
  "scripts/verify-artifact-governance-extraction.sh" \
  command \
  scripts/verify-artifact-governance-extraction.sh

run_gate fast authorization-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/AuthorizationBoundary.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/AuthorizationBoundary.lean
run_gate fast authorization-authority-commit-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/AuthorizationAuthorityCommit.lean" \
  command \
  lean -DwarningAsError=true \
    verification/stripmine-2026-08-22/proofs/lean/AuthorizationAuthorityCommit.lean
run_gate fast authorization-scope-exactness-lean \
  "lean -DwarningAsError=true verification/authz-v1/AuthorizationScopeExactness.lean" \
  command \
  lean -DwarningAsError=true verification/authz-v1/AuthorizationScopeExactness.lean
run_gate fast authorization-commit-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/authorization-commit-boundary.qnt" \
  command \
  quint typecheck \
    verification/stripmine-2026-08-22/models/authorization-commit-boundary.qnt
run_gate fast authorization-commit-quint \
  "quint verify verification/stripmine-2026-08-22/models/authorization-commit-boundary.qnt --backend apalache --invariants typeOk atMostOnce acceptedOnlyWithExactBindings acceptedOnlyWithFreshVersions acceptedOnlyWithinValidityWindow rejectionPreservesCommitAuthority --max-steps 8" \
  command \
  quint verify \
    verification/stripmine-2026-08-22/models/authorization-commit-boundary.qnt \
    --backend apalache \
    --invariants typeOk atMostOnce acceptedOnlyWithExactBindings \
      acceptedOnlyWithFreshVersions acceptedOnlyWithinValidityWindow \
      rejectionPreservesCommitAuthority \
    --max-steps 8 \
    --out "$result_root/authorization-commit-boundary-quint.json"
run_gate fast authorization-scope-exactness-quint-typecheck \
  "quint typecheck verification/authz-v1/models/authorization-scope-exactness.qnt" \
  command \
  quint typecheck verification/authz-v1/models/authorization-scope-exactness.qnt
run_gate fast authorization-scope-exactness-quint \
  "quint verify verification/authz-v1/models/authorization-scope-exactness.qnt --backend apalache --invariants typeOk allowIffExact omittedGrantTenantRejectsPresentRequest omittedGrantSessionRejectsPresentRequest --max-steps 4" \
  command \
  quint verify verification/authz-v1/models/authorization-scope-exactness.qnt \
    --backend apalache \
    --invariants typeOk allowIffExact omittedGrantTenantRejectsPresentRequest \
      omittedGrantSessionRejectsPresentRequest \
    --max-steps 4 \
    --out "$result_root/authorization-scope-exactness-quint.json"

run_gate full quantity-runtime \
  "cargo test --locked -p norn-checker --test quantity_ledger --test quantity_ledger_properties --test quantity_ledger_expression --test quantity_ledger_boundary" \
  rust-tests \
  cargo test --locked -p norn-checker \
    --test quantity_ledger \
    --test quantity_ledger_properties \
    --test quantity_ledger_expression \
    --test quantity_ledger_boundary
run_gate full generic-posting-runtime \
  "cargo test --locked -p norn-kernel --test accounting_commit" rust-tests \
  cargo test --locked -p norn-kernel --test accounting_commit
run_gate full core-physics-inheritance-runtime \
  "cargo test --locked -p norn-kernel --test core_physics_inheritance" rust-tests \
  cargo test --locked -p norn-kernel --test core_physics_inheritance

run_gate full quantity-authority-runtime-unit \
  "cargo test --locked -p norn-cell --test quantity_reservations" \
  rust-tests \
  cargo test --locked -p norn-cell --test quantity_reservations
run_gate fast quantity-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/quantity-ledger.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/quantity-ledger.qnt
run_gate fast quantity-quint \
  "quint verify verification/stripmine-2026-08-22/models/quantity-ledger.qnt --backend apalache --invariants typeOk conserved terminalResolvedOnce noTerminalReservation --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/quantity-ledger.qnt \
    --backend apalache \
    --invariants typeOk conserved terminalResolvedOnce noTerminalReservation \
    --max-steps 8 \
    --out "$result_root/quantity-ledger-quint.json"
run_gate fast quantity-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/QuantityLedger.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/QuantityLedger.lean
run_gate fast quantity-authority-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/quantity-authority.qnt" \
  command \
  quint typecheck verification/stripmine-2026-08-22/models/quantity-authority.qnt
run_gate fast quantity-authority-quint \
  "quint verify verification/stripmine-2026-08-22/models/quantity-authority.qnt --backend apalache --invariants typeOk perUnitLayerConserved atomicAuthorityRecords phaseMatchesCommitCount reversalRestoresBalances noOpPreservesAuthority --max-steps 8" \
  command \
  quint verify verification/stripmine-2026-08-22/models/quantity-authority.qnt \
    --backend apalache \
    --invariants typeOk perUnitLayerConserved atomicAuthorityRecords \
      phaseMatchesCommitCount reversalRestoresBalances noOpPreservesAuthority \
    --max-steps 8 \
    --out "$result_root/quantity-authority-quint.json"
run_gate fast quantity-authority-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/QuantityAuthority.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/QuantityAuthority.lean
run_gate fast quantity-reservation-quint-typecheck \
  "quint typecheck verification/stripmine-2026-08-22/models/quantity-reservation-authority.qnt" \
  command \
  quint typecheck \
    verification/stripmine-2026-08-22/models/quantity-reservation-authority.qnt
run_gate fast quantity-reservation-quint \
  "quint verify verification/stripmine-2026-08-22/models/quantity-reservation-authority.qnt --backend apalache --invariants typeOk conserved recordsAligned terminalResolvedOnce noTerminalReservation pendingHasOneCreate noOpPreservesAuthority --max-steps 8" \
  command \
  quint verify \
    verification/stripmine-2026-08-22/models/quantity-reservation-authority.qnt \
    --backend apalache \
    --invariants typeOk conserved recordsAligned terminalResolvedOnce \
      noTerminalReservation pendingHasOneCreate noOpPreservesAuthority \
    --max-steps 8 \
    --out "$result_root/quantity-reservation-authority-quint.json"
run_gate fast quantity-reservation-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/QuantityReservationAuthority.lean" \
  command \
  lean -DwarningAsError=true \
    verification/stripmine-2026-08-22/proofs/lean/QuantityReservationAuthority.lean
run_gate fast quantity-batch-quint-typecheck \
  "quint typecheck verification/quantity-batch-atomicity-v1/models/quantity-batch-atomicity.qnt" \
  command \
  quint typecheck \
    verification/quantity-batch-atomicity-v1/models/quantity-batch-atomicity.qnt
run_gate fast quantity-batch-quint \
  "quint verify verification/quantity-batch-atomicity-v1/models/quantity-batch-atomicity.qnt --backend apalache --invariants typeOk sequentialVersions batchAllOrNone stableReceipt --max-steps 8" \
  command \
  quint verify \
    verification/quantity-batch-atomicity-v1/models/quantity-batch-atomicity.qnt \
    --backend apalache \
    --invariants typeOk sequentialVersions batchAllOrNone stableReceipt \
    --max-steps 8 \
    --out "$result_root/quantity-batch-atomicity-quint.json"
run_gate fast quantity-batch-lean \
  "lean -DwarningAsError=true verification/quantity-batch-atomicity-v1/proofs/QuantityBatchAtomicity.lean" \
  command \
  lean -DwarningAsError=true \
    verification/quantity-batch-atomicity-v1/proofs/QuantityBatchAtomicity.lean
run_gate fast quantity-unit-dimension-quint-typecheck \
  "quint typecheck verification/quantity-unit-dimension-v1/models/quantity-unit-dimension.qnt" \
  command \
  quint typecheck \
    verification/quantity-unit-dimension-v1/models/quantity-unit-dimension.qnt
run_gate fast quantity-unit-dimension-quint \
  "quint verify verification/quantity-unit-dimension-v1/models/quantity-unit-dimension.qnt --backend apalache --invariants typeOk certificateRemainsPinned acceptedPostingHasExactBinding differentDimensionCannotSubstitute differentDimensionsCannotBalance --max-steps 8" \
  command \
  quint verify \
    verification/quantity-unit-dimension-v1/models/quantity-unit-dimension.qnt \
    --backend apalache \
    --invariants typeOk certificateRemainsPinned acceptedPostingHasExactBinding \
      differentDimensionCannotSubstitute differentDimensionsCannotBalance \
    --max-steps 8 \
    --out "$result_root/quantity-unit-dimension-quint.json"
run_gate fast quantity-unit-dimension-lean \
  "lean -DwarningAsError=true verification/quantity-unit-dimension-v1/proofs/QuantityUnitDimension.lean" \
  command \
  lean -DwarningAsError=true \
    verification/quantity-unit-dimension-v1/proofs/QuantityUnitDimension.lean
run_gate fast core-value-ledger-quint-typecheck \
  "quint typecheck verification/core-physics-v1/models/core-value-ledger.qnt" \
  command \
  quint typecheck \
    verification/core-physics-v1/models/core-value-ledger.qnt
run_gate fast core-value-ledger-quint \
  "quint verify verification/core-physics-v1/models/core-value-ledger.qnt --backend apalache --invariants typeOk allPostedBalanced postedImmutable reversalLinkedExact reversedAtMostOnce --max-steps 4" \
  command \
  quint verify \
    verification/core-physics-v1/models/core-value-ledger.qnt \
    --backend apalache \
    --invariants typeOk allPostedBalanced postedImmutable reversalLinkedExact \
      reversedAtMostOnce \
    --max-steps 4 \
    --out "$result_root/core-value-ledger-quint.json"
run_gate fast core-value-ledger-lean \
  "lean -DwarningAsError=true verification/core-physics-v1/proofs/CoreValueLedger.lean" \
  command \
  lean -DwarningAsError=true \
    verification/core-physics-v1/proofs/CoreValueLedger.lean
run_gate fast core-quantity-commitment-quint-typecheck \
  "quint typecheck verification/core-physics-v1/models/core-quantity-commitment.qnt" \
  command \
  quint typecheck \
    verification/core-physics-v1/models/core-quantity-commitment.qnt
run_gate fast core-quantity-commitment-quint \
  "quint verify verification/core-physics-v1/models/core-quantity-commitment.qnt --backend apalache --invariants typeOk quantityConserved escrowNonNegative neverDropped terminalAbsorbing endsByDue --max-steps 4" \
  command \
  quint verify \
    verification/core-physics-v1/models/core-quantity-commitment.qnt \
    --backend apalache \
    --invariants typeOk quantityConserved escrowNonNegative neverDropped \
      terminalAbsorbing endsByDue \
    --max-steps 4 \
    --out "$result_root/core-quantity-commitment-quint.json"
run_gate fast core-quantity-lean \
  "lean -DwarningAsError=true verification/core-physics-v1/proofs/CoreQuantity.lean" \
  command \
  lean -DwarningAsError=true \
    verification/core-physics-v1/proofs/CoreQuantity.lean
run_gate full quantity-kani \
  "env RUSTUP_HOME=target/verification-tools/rustup KANI_HOME=target/verification-tools/kani-home kani --fail-fast --output-format terse verification/stripmine-2026-08-22/proofs/kani/quantity_ledger_harness.rs" \
  command \
  env RUSTUP_HOME=target/verification-tools/rustup \
    KANI_HOME=target/verification-tools/kani-home \
    kani --fail-fast --output-format terse --target-dir "$result_root/kani-target" \
    verification/stripmine-2026-08-22/proofs/kani/quantity_ledger_harness.rs

run_gate full ontology-runtime-rust \
  "cargo test --locked -p norn-checker --test ontology_pack --test ontology_registry --test quantity_unit_ontology_pack" \
  rust-tests \
  cargo test --locked -p norn-checker \
    --test ontology_pack \
    --test ontology_registry \
    --test quantity_unit_ontology_pack
run_gate full ontology-closed-entity-ir-rust \
  "cargo test --locked -p norn-ir -p norn-checker entity_" \
  rust-tests \
  cargo test --locked -p norn-ir -p norn-checker entity_
run_gate fast ontology-model "python3 verification/ontology/scripts/validate.py" command \
  python3 verification/ontology/scripts/validate.py
run_gate full ontology-semantic-differential "scripts/check-ontology-semantics.sh" command \
  scripts/check-ontology-semantics.sh
run_gate fast ontology-mapping-provenance-quint-typecheck \
  "quint typecheck verification/ontology-v1/models/ontology-mapping-provenance.qnt" \
  command \
  quint typecheck verification/ontology-v1/models/ontology-mapping-provenance.qnt
run_gate fast ontology-mapping-provenance-quint \
  "quint verify verification/ontology-v1/models/ontology-mapping-provenance.qnt --backend apalache --invariants typeOk replacementIsDirect replacementPreservesIdentity replacementHasReviewedBoundedProvenance exactlyOneActivePerIdentity rejectedAttemptIsNoOp --max-steps 8" \
  command \
  quint verify verification/ontology-v1/models/ontology-mapping-provenance.qnt \
    --backend apalache \
    --invariants typeOk replacementIsDirect replacementPreservesIdentity \
      replacementHasReviewedBoundedProvenance exactlyOneActivePerIdentity \
      rejectedAttemptIsNoOp \
    --max-steps 8 \
    --out "$result_root/ontology-mapping-provenance-quint.json"
run_gate fast ontology-relation-authority-quint-typecheck \
  "quint typecheck verification/ontology-v1/models/ontology-relation-authority.qnt" \
  command \
  quint typecheck verification/ontology-v1/models/ontology-relation-authority.qnt
run_gate fast ontology-relation-authority-quint \
  "quint verify verification/ontology-v1/models/ontology-relation-authority.qnt --backend apalache --invariants typeOk acceptedVersionWasExact acceptedEndpointsHadExactKinds cardinalityPreserved rejectedAttemptIsNoOp --max-steps 8" \
  command \
  quint verify verification/ontology-v1/models/ontology-relation-authority.qnt \
    --backend apalache \
    --invariants typeOk acceptedVersionWasExact acceptedEndpointsHadExactKinds \
      cardinalityPreserved rejectedAttemptIsNoOp \
    --max-steps 8 \
    --out "$result_root/ontology-relation-authority-quint.json"
run_gate fast ontology-mapping-provenance-lean \
  "lean -DwarningAsError=true verification/ontology-v1/OntologyMappingProvenance.lean" \
  command \
  lean -DwarningAsError=true verification/ontology-v1/OntologyMappingProvenance.lean
run_gate fast ontology-authority-lean \
  "lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/OntologyAuthority.lean" \
  command \
  lean -DwarningAsError=true verification/stripmine-2026-08-22/proofs/lean/OntologyAuthority.lean

run_gate full cala-boundary "cargo test --locked -p norn-checker --test quantity_ledger_boundary" rust-tests \
  cargo test --locked -p norn-checker --test quantity_ledger_boundary

run_gate full verification-executed-test-selectors \
  "python3 scripts/validate-executed-test-selectors.py" command \
  python3 scripts/validate-executed-test-selectors.py
run_gate fast verification-executed-test-selectors-fast \
  "python3 scripts/validate-executed-test-selectors.py --profile fast" command \
  python3 scripts/validate-executed-test-selectors.py --profile fast
run_gate fast verification-proof-hole-audit-fast \
  "python3 scripts/check-proof-holes.py" command \
  python3 scripts/check-proof-holes.py
run_gate fast verification-attestation-negative-canaries \
  "python3 verification/coverage-v1/negative_canaries.py" command \
  python3 verification/coverage-v1/negative_canaries.py

run_gate full kernel-v2-commit-admission-typecheck \
  "quint typecheck verification/kernel-v2/models/commit-admission.qnt" command \
  quint typecheck verification/kernel-v2/models/commit-admission.qnt

run_gate full kernel-v2-commit-admission \
  "quint verify verification/kernel-v2/models/commit-admission.qnt --backend apalache --invariants invariant --max-steps 8" command \
  quint verify verification/kernel-v2/models/commit-admission.qnt --backend apalache --invariants invariant --max-steps 8 --out "$result_root/kernel-v2-commit-admission.json"

run_gate full kernel-v2-quantity-ledger-typecheck \
  "quint typecheck verification/kernel-v2/models/quantity-ledger.qnt" command \
  quint typecheck verification/kernel-v2/models/quantity-ledger.qnt

run_gate full kernel-v2-quantity-ledger \
  "quint test verification/kernel-v2/models/quantity-ledger.qnt --backend rust --match conservation.*Test --max-samples 1 --seed 1" command \
  quint test verification/kernel-v2/models/quantity-ledger.qnt --backend rust --match "conservation.*Test" --max-samples 1 --seed 1

run_gate full kernel-v2-projection-trust-typecheck \
  "quint typecheck verification/kernel-v2/models/projection-trust.qnt" command \
  quint typecheck verification/kernel-v2/models/projection-trust.qnt

run_gate full kernel-v2-projection-trust \
  "quint verify verification/kernel-v2/models/projection-trust.qnt --backend apalache --invariants invariant --max-steps 8" command \
  quint verify verification/kernel-v2/models/projection-trust.qnt --backend apalache --invariants invariant --max-steps 8 --out "$result_root/kernel-v2-projection-trust.json"

run_gate full kernel-v2-commit-conformance \
  "cargo test --locked -p norn-testkit --test kernel_quint" rust-tests \
  cargo test --locked -p norn-testkit --test kernel_quint

mapfile -t declared_quint_results < <(
  python3 - verification/coverage-v1/coverage.json <<'PY'
import json
import pathlib
import sys

coverage = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
artifacts = {
    artifact
    for subsystem in coverage["subsystems"]
    for layer in ("runtime", "model", "proof")
    for artifact in subsystem[layer].get("resultArtifacts", [])
}
print("\n".join(sorted(artifacts)))
PY
)
mapfile -t declared_generated_evidence < <(
  python3 - verification/coverage-v1/coverage.json <<'PY'
import json
import pathlib
import sys

coverage = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
artifacts = {
    artifact
    for subsystem in coverage["subsystems"]
    for layer in ("runtime", "model", "proof")
    for artifact in subsystem[layer].get("generatedArtifacts", [])
}
print("\n".join(sorted(artifacts)))
PY
)
verification_result_args=(
  --coverage verification/coverage-v1/coverage.json
  --passed-commands "$passed_commands"
)
for artifact in "${declared_quint_results[@]}"; do
  verification_result_args+=(--quint-result "$repo_root/$artifact")
done
for artifact in "${declared_generated_evidence[@]}"; do
  verification_result_args+=(--generated-evidence "$repo_root/$artifact")
done
verification_result_args+=(--output "$result_root/formal-results.json")
if [[ "$verification_profile" == "full" ]]; then
  python3 scripts/write-verification-result.py "${verification_result_args[@]}"
  python3 verification/coverage-v1/validate_result.py \
    "$result_root/formal-results.json"
  printf '\nVerification result: %s\n' "$result_root/formal-results.json"
else
  python3 scripts/write-verification-result.py \
    --coverage verification/coverage-v1/coverage.json \
    --passed-commands "$passed_commands" \
    --profile fast \
    --validate-only
  printf '%s\n' \
    '' \
    'Fast verification profile completed: every fast manifest gate ran and its registration was checked against the passed-command log.' \
    'It does not write a formal-results record or attest full-only gates, generated evidence, complete evidence hashes, or a merge-blocking GitHub status.'
fi
