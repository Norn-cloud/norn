#!/usr/bin/env bash
set -euo pipefail
source "$(git rev-parse --show-toplevel)/scripts/demo/drop-common.sh"
drop_main "$@"
if [[ ! -f crates/norn-checker/tests/three_systems.rs ]]; then
  printf 'not yet: #548 (ERPNext, Shopify and Odoo mappings onto one REA ontology)\n'; exit 1
fi

bun test tools/norn-ts/test/registry-packs.test.ts \
  --test-name-pattern 'registry packs are generated from their typed sources'
binary=$(cargo test --locked -p norn-checker --test three_systems --no-run --message-format=json \
  | python3 -c 'import json,sys
paths = [d["executable"] for line in sys.stdin if (d := json.loads(line)).get("reason") == "compiler-artifact" and d.get("target", {}).get("name") == "three_systems" and d.get("executable")]
if len(paths) != 1: raise SystemExit("expected exactly one three-systems test binary")
print(paths[0])')
sha256sum "$binary"
scenarios=(
  mvp_map_three_artifact_set_is_admitted \
  mvp_map_three_aliases_roles_and_one_economic_identity \
  mvp_item_holding_catalog_and_owner_location_partitions \
  mvp_map_gap_shopify_price_is_never_stock_cost_or_gl \
  mvp_map_three_ambiguous_identity_refuses_without_sku_guessing \
  mvp_map_three_odoo_snapshot_incremental_and_replay_are_synthetic \
  mvp_map_three_unresolved_units_and_draft_postings_refuse \
  mvp_map_three_generated_aliases_never_guess_catalog_identity
)
"$binary" --list --format terse | python3 -c 'import sys
names = {line.strip().removesuffix(": test") for line in sys.stdin}
missing = set(sys.argv[1:]) - names
if missing: raise SystemExit("missing three-systems scenarios: " + ", ".join(sorted(missing)))' "${scenarios[@]}"
for scenario in "${scenarios[@]}"; do
  "$binary" --exact "$scenario" --nocapture
done
printf 'not yet: #485 #498 (durable promotion, conflict review and aggregate dedup through the Observed inbox)\n'
printf 'not yet: live connectors or production CDC (synthetic snapshot + incremental contract only)\n'
