# Analytical tables

`TemporalPlan` resolves numeric fields from the effective ontology and admits a
bounded point-in-time snapshot. It refuses future feature knowledge, stale
features, unavailable outcomes, duplicate object/time grain and invalid values.
Chronological holdouts require training outcomes known before the boundary.

These checks cover supplied evidence only. The exporting host must authorize
reads and authenticate source receipts and ledger heads. No live ingestion,
incremental maintenance, statistical publication or formal proof is claimed.

Tests compile a synthetic typed declaration with the repository's Bun authoring
toolchain, then pass its generated artifacts to the Rust catalog.

For a focused run on Boat or CI, install the pinned authoring dependencies in
`tools/norn-ts`, then run this from the repository root. Keep fixture generation
and the test in the same shell; the CI wrapper removes its own fixtures on exit.

```sh
mkdir -p "${XDG_CACHE_HOME:-$HOME/.cache}/agent-scratch"
fixture_dir=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/agent-scratch/metrics.XXXXXX")
trap 'rm -r -- "$fixture_dir"' EXIT
bun tools/norn-ts/src/cli.ts compile crates/norn-metrics/tests/demand.source.ts --org org_demo --env dev --out "$fixture_dir/metric-demand.json"
NORN_UPSTREAM_FIXTURE_DIR="$fixture_dir" cargo test --locked -p norn-metrics
```

`bash scripts/ci/core.sh test` also generates these inputs and runs the workspace
test suite in its own shell.
