#!/usr/bin/env bash
set -euo pipefail
source "$(git rev-parse --show-toplevel)/scripts/demo/drop-common.sh"
drop_main "$@"
if [[ ! -f crates/norn-cell/tests/restatement_activation.rs ]]; then
  printf 'not yet: #497 (governed restatement generations)\n'; exit 1
fi
python3 -c 'import sys; sys.version_info >= (3, 9) or sys.exit("Python 3.9 or newer is required")'
export PROPTEST_RNG_SEED=21320261103 PROPTEST_RNG_ALGORITHM=cc
printf 'Generated-input seed: %s; algorithm: %s\n' "$PROPTEST_RNG_SEED" "$PROPTEST_RNG_ALGORITHM"
binary=$(cargo test --locked -p norn-cell --test restatement_activation --no-run --message-format=json \
  | python3 -c 'import json,sys
paths = [d["executable"] for line in sys.stdin if (d := json.loads(line)).get("reason") == "compiler-artifact" and d.get("target", {}).get("name") == "restatement_activation" and d.get("executable")]
if len(paths) != 1: raise SystemExit("expected exactly one native question test binary")
print(paths[0])')
sha256sum "$binary"
scenarios=(
  clock_ahead_cutoff_survives_lower_receipts_and_failed_preparation_restart
  generated_clock_leads_preserve_pinned_restatement_and_recovery
  governed_activation_switches_all_slices_and_preserves_old_answers
  missing_incomplete_and_stale_candidates_never_replace_the_complete_output
  activated_deltas_equal_full_recompute_through_redelivery_and_restart
  declared_restatement_law_and_reviewer_actions_are_admitted_together
  prepared_candidates_stay_bound_to_their_native_host
  candidate_budget_includes_the_complete_staged_slot
  resumed_slice_declarations_keep_the_same_admission_bound
  approved_candidate_refuses_an_alternate_kernel_writer_before_activation
)
"$binary" --list --format terse | python3 -c 'import sys
names = {line.strip().removesuffix(": test") for line in sys.stdin}
missing = set(sys.argv[1:]) - names
if missing: raise SystemExit("missing native scenarios: " + ", ".join(sorted(missing)))' "${scenarios[@]}"
for scenario in "${scenarios[@]}"; do
  "$binary" --exact "$scenario" --nocapture
done
printf 'not yet: #341 (authorized aggregate-to-source-document lineage join)\n'
printf 'not yet: #375 (guarded statistical publication in this drop)\n'
