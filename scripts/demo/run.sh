#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
case "${1:-}" in
  ''|--require-complete) ;;
  *) printf 'usage: bash scripts/demo/run.sh [--require-complete]\n' >&2; exit 2 ;;
esac
printf 'Source: %s\n' "$(git rev-parse HEAD)"
printf 'Target business values are not runtime results.\n'
printf 'Beat 1 — working stock/books app\nnot yet: #214 #341 #342 (declared valuation and atomic effect closure)\n'
printf 'Beat 2 — inspect authored definitions (schema fragment only)\n'
bun tools/norn-ts/src/cli.ts profile scripts/demo/jewelry.source.ts
bun tools/norn-ts/src/cli.ts compile scripts/demo/jewelry.source.ts --org org_demo --env dev \
  | cargo run --locked --quiet -p norn-checker --example jewelry_demo
printf 'checker-admitted / refused; obligations generated; runtime execution not yet (#341 #342 #343)\n'
printf 'not yet: #340 #218 (admitted catalog and equivalent governed UI/API/MCP/CLI)\n'
printf 'Beat 3 — sell → scrap → melt\nnot yet: #216 #341 #342 (see TARGET definitions in scripts/demo/README.md)\n'
printf 'Beat 4 — laws carry through (accepted measurements)\n'
cargo test --locked -p norn-kernel --test conversion_conservation
printf 'CURRENT: inherited core measurement law; atomic omitted-loss refusal scenarios and shared weakening-overlay refusal passed.\nnot yet: #343 #344 (stock/value effect closure; additive overlay SAT/progress evidence)\n'
printf 'Beat 5 — evidence (generated Guarantees matrix)\n'
bun tools/norn-ts/src/cli.ts compile scripts/demo/jewelry.source.ts --org org_demo --env dev \
  | cargo run --locked --quiet -p norn-checker --example jewelry_demo -- --challenge-json \
  | python3 scripts/guarantees_matrix.py --challenge -
printf 'not yet: #343 (generated solver counterexample); unchecked or stale evidence stays unknown.\n'
printf 'Beat 6 — weekly gold scrap yield\n'
for scenario in INGEST FRESH LINEAGE INCREMENTAL QUARANTINE ASOF RESTATEMENT-DELTA FORECAST PUBLISH-GUARD; do
  printf 'DATA-%s — not yet: #341 #345 #346 (integrated scrap-yield path)\n' "$scenario"
done
printf 'INCOMPLETE: six-beat acceptance has not passed; obligations above retain their actual assessments.\n'
if [[ ${1:-} == --require-complete ]]; then exit 1; fi
