#!/usr/bin/env bash
set -euo pipefail
source "$(git rev-parse --show-toplevel)/scripts/demo/drop-common.sh"
drop_main "$@"
if [[ ! -f tools/norn-ts/fixtures/jewelry/cp3-folds.source.ts ]]; then
  printf 'not yet: #428 (declared ordered-fold evaluation)\n'; exit 1
fi

bun test tools/norn-ts/test/authoring-sdk.test.ts \
  --test-name-pattern 'CP3 allocation goldens come from core-aligned typed declarations'
binary=$(cargo test --locked -p norn-decisions --test evaluate --no-run --message-format=json \
  | python3 -c 'import json,sys
paths = [d["executable"] for line in sys.stdin if (d := json.loads(line)).get("reason") == "compiler-artifact" and d.get("target", {}).get("name") == "evaluate" and d.get("executable")]
if len(paths) != 1: raise SystemExit("expected exactly one native evaluator test binary")
print(paths[0])')
sha256sum "$binary"
scenarios=(
  fold_capacity_complete_allocate_matches_the_core_spec \
  declared_fifo_matches_all_retained_reference_histories_and_cp1 \
  declared_average_matches_every_retained_prefix_with_original_cost_returns \
  declared_average_cp1_and_delivery_to_transfer_identity_collision \
  fold_boundaries_refuse_without_a_partial_decision \
  fold_order_is_numeric_with_identity_ties_and_independent_of_enumeration \
  cp3_typed_declarations_preserve_sale_scrap_and_melt_allocations
)
"$binary" --list --format terse | python3 -c 'import sys
names = {line.strip().removesuffix(": test") for line in sys.stdin}
missing = set(sys.argv[1:]) - names
if missing: raise SystemExit("missing native scenarios: " + ", ".join(sorted(missing)))' "${scenarios[@]}"
for scenario in "${scenarios[@]}"; do
  "$binary" --exact "$scenario" --nocapture
done
printf 'not yet: #213 (T1-C1 native cell cycle, real approvals, reservations and balanced books)\n'
printf 'not yet: #478 (generated application proof result)\n'
