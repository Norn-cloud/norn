#!/usr/bin/env bash
set -euo pipefail
source "$(git rev-parse --show-toplevel)/scripts/demo/drop-common.sh"
drop_main "$@"
if [[ ! -f crates/norn-cell/tests/incremental_question.rs ]]; then
  printf 'not yet: #497 (incremental question evaluation)\n'; exit 1
fi
python3 -c 'import sys; sys.version_info >= (3, 9) or sys.exit("Python 3.9 or newer is required")'
export PROPTEST_RNG_SEED=21320261103 PROPTEST_RNG_ALGORITHM=cc
printf 'Generated-input seed: %s; algorithm: %s\n' "$PROPTEST_RNG_SEED" "$PROPTEST_RNG_ALGORITHM"
binary=$(cargo test --locked -p norn-cell --test incremental_question --no-run --message-format=json \
  | python3 -c 'import json,sys
paths = [d["executable"] for line in sys.stdin if (d := json.loads(line)).get("reason") == "compiler-artifact" and d.get("target", {}).get("name") == "incremental_question" and d.get("executable")]
if len(paths) != 1: raise SystemExit("expected exactly one native question test binary")
print(paths[0])')
sha256sum "$binary"
scenarios=(
  data_delta_incremental_equals_recompute_with_redelivery_and_restart
  empty_generation_refuses_a_new_foreign_scope_binding
  skipped_receipt_or_changed_population_refuses_without_partial_publication
)
"$binary" --list --format terse | python3 -c 'import sys
names = {line.strip().removesuffix(": test") for line in sys.stdin}
missing = set(sys.argv[1:]) - names
if missing: raise SystemExit("missing native scenarios: " + ", ".join(sorted(missing)))' "${scenarios[@]}"
for scenario in "${scenarios[@]}"; do
  "$binary" --exact "$scenario" --nocapture
done
printf 'not yet: #498 #485 (signed feed and authored quarantine integration)\n'
printf 'not yet: #341 (authorized aggregate-to-source-document lineage join)\n'
