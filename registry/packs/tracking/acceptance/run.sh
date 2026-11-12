#!/usr/bin/env bash
# The tracking pack's acceptance: the real `mlflow` client against `norn dev`.
#
#   registry/packs/tracking/acceptance/run.sh WORKDIR
#
# Needs python3 (3.11 or later), bun, and NORN_DEV_HOST_BIN (a built
# `norn-dev-host`), as core.sh's integration phase provides.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
work="${1:?usage: run.sh WORKDIR}"
: "${NORN_DEV_HOST_BIN:?NORN_DEV_HOST_BIN must name a built norn-dev-host}"
mkdir -p "$work"
work="$(cd "$work" && pwd)"

python3 -m venv "$work/venv"
"$work/venv/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$here/requirements.txt"

project="$work/tracking"
rm -rf "$project"
bun "$repo/tools/norn-ts/src/cli.ts" init "$project" --no-install > /dev/null
cp "$here/../tracking.ts" "$project/src/tracking.ts"
(cd "$project" && exec bun "$repo/tools/norn-ts/src/cli.ts" dev --json --port 0 > "$work/dev.ndjson" 2> "$work/dev.err") &
dev=$!
trap 'kill "$dev" 2>/dev/null || true; wait "$dev" 2>/dev/null || true' EXIT
for _ in $(seq 1 240); do
  grep -q '"event":"ready"' "$work/dev.ndjson" 2>/dev/null && break
  kill -0 "$dev" 2>/dev/null || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }
  sleep 0.5
done
grep -q '"event":"ready"' "$work/dev.ndjson" || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }

"$work/venv/bin/python" "$here/check_tracking.py" "$project"
