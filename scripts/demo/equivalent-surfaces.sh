#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
scratch_root=${NORN_SURFACE_SCRATCH:-$HOME/.cache/agent-scratch/norn-lane-I}
mkdir -p "$scratch_root"
scratch=$(mktemp -d "$scratch_root/surfaces.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
(cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile)
bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/melt-surfaces --org org_surfaces --env dev --out "$scratch/melt.json"
(cd packages/norn-surface-acceptance && bun install --frozen-lockfile --backend copyfile)
cargo build --locked -p norn-cli
export NORN_CLI="$(cd "${CARGO_TARGET_DIR:-target}" && pwd)/debug/norn"
printf '%s\n' '{}' > "$scratch/context.json"
if ! "$NORN_CLI" check --artifacts "$scratch/melt.json" --context "$scratch/context.json" > "$scratch/check.json"; then
  cat "$scratch/check.json"
  exit 1
fi
export NORN_SURFACE_DEFINITIONS="$scratch/melt.json"
export NORN_NODE=${NORN_NODE:-node}
cargo test --locked -p norn-api equivalent_governed_surfaces -- --ignored --nocapture
