#!/usr/bin/env bash
set -euo pipefail
source "$(git rev-parse --show-toplevel)/scripts/demo/drop-common.sh"
drop_main "$@"
if [[ ! -f crates/norn-cell/src/observation_quality.rs ]]; then
  printf 'not yet: #506 (durable law findings and quarantine)\n'; exit 1
fi
export PROPTEST_CASES=16 PROPTEST_RNG_SEED=21320261103
printf 'Sampled property cases: %s; seed: %s\n' "$PROPTEST_CASES" "$PROPTEST_RNG_SEED"
binary=$(cargo test --locked -p norn-cell --test definitions_install --no-run --message-format=json \
  | python3 -c 'import json,sys
paths = [d["executable"] for line in sys.stdin if (d := json.loads(line)).get("reason") == "compiler-artifact" and d.get("target", {}).get("name") == "definitions_install" and d.get("executable")]
if len(paths) != 1: raise SystemExit("expected exactly one native cell test binary")
print(paths[0])')
sha256sum "$binary"
scenarios=(
  obs_bad_scrap_yield_purity_120_is_captured_but_native_write_refuses
  data_quarantine_redelivery_persists_one_finding_and_correction_recovers
  obs_evidence_redelivery_persists_one_law_finding_after_restart
  obs_recovery_upgrade_uses_each_findings_retained_model
  obs_evidence_historical_governing_laws_do_not_follow_current_definitions
  obs_evidence_install_refuses_missing_or_substituted_governing_laws
  obs_evidence_snapshot_roundtrip_preserves_generated_governing_identity
  obs_evidence_idempotent_capture_reuses_the_original_finding_cutoff
  obs_evidence_legacy_candidates_do_not_block_a_verified_producer
  law_complete_inherited_and_effective_scopes_refuse_until_b2
  legacy_law_snapshot_refuses_without_effects
  legacy_version_zero_finding_refuses_without_effects
)
"$binary" --list --format terse | python3 -c 'import sys
names = {line.strip().removesuffix(": test") for line in sys.stdin}
missing = set(sys.argv[1:]) - names
if missing: raise SystemExit("missing native scenarios: " + ", ".join(sorted(missing)))' "${scenarios[@]}"
for scenario in "${scenarios[@]}"; do
  "$binary" --exact "$scenario" --nocapture
done
printf 'not yet: #498 (signed external feed capture in this drop)\n'
printf 'not yet: #432 #485 (inherited/effective laws and authored scrap feed integration)\n'
