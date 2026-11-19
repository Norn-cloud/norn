#!/usr/bin/env bash
# Run the golden-path probe against a fresh local `norn dev` (from this checkout).
#
#   NORN_DEV_HOST_BIN=target/release/norn-dev-host apps/norn-cell-do/probe/dev.sh [project-dir]
#
# Needs bun, and tools/norn-ts installed (`bun install --frozen-lockfile`). The
# project directory (default: a new temporary one) gets the probe definitions;
# norn dev serves it on loopback, the probe runs once, norn dev stops. Set
# OTEL_EXPORTER_OTLP_ENDPOINT/HEADERS to send the result to OpenObserve.
# Exit status is the probe's: 0 pass, 1 fail, 2 could not start.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
: "${NORN_DEV_HOST_BIN:?build it with cargo build --release -p norn-cell-do --bin norn-dev-host}"
project=${1:-$(mktemp -d)/probe}
cli="$root/tools/norn-ts/src/cli.ts"
if [ ! -f "$project/norn.json" ]; then
  bun "$cli" init "$project" --template empty --no-install --namespace probe >/dev/null
  cp "$here/probe.ts" "$project/src/probe.ts"
fi
log="$project/.norn-probe-dev.log"
(cd "$project" && exec bun "$cli" dev --json --port 0) >"$log" 2>&1 &
dev=$!
trap 'kill "$dev" 2>/dev/null; wait "$dev" 2>/dev/null || true' EXIT
for _ in $(seq 1 240); do
  grep -q '"event":"ready"' "$log" 2>/dev/null && break
  kill -0 "$dev" 2>/dev/null || { tail -n 20 "$log" >&2; exit 2; }
  sleep 0.5
done
grep -q '"event":"ready"' "$log" || { echo "norn dev did not become ready" >&2; tail -n 20 "$log" >&2; exit 2; }
node "$here/golden-path.mjs" --dev "$project"
