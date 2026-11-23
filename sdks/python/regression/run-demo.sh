#!/usr/bin/env bash
# One-click regression assumptions, end to end against a local development host.
#
#   sdks/python/regression/run-demo.sh WORKDIR
#
# Needs what ../run-demo.sh needs (python3 3.11+, bun, NORN_DEV_HOST_BIN).
# Generates the client, runs the suite's unit tests, serves a project with the
# lab and stats definitions, seeds the synthetic dataset through the governed
# action route, runs the notebook, and checks what it recorded in the cell.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdk="$(cd "$here/.." && pwd)"
repo="$(cd "$sdk/../.." && pwd)"
work="${1:?usage: run-demo.sh WORKDIR}"
: "${NORN_DEV_HOST_BIN:?NORN_DEV_HOST_BIN must name a built norn-dev-host}"
mkdir -p "$work"
work="$(cd "$work" && pwd)"

# 1. The client, generated as ../run-demo.sh does, and this demo's pinned environment.
python3 -m venv "$work/gen"
"$work/gen/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$sdk/requirements.txt"
python3 -I "$sdk/prepare.py" "$repo/sdks/openapi/norn-api.openapi.json" "$work/client-input.json"
rm -rf "$work/client" && mkdir -p "$work/client"
PATH="$work/gen/bin:$PATH" "$work/gen/bin/openapi-python-client" generate --path "$work/client-input.json" \
  --output-path "$work/client/norn_client" --meta none > "$work/generate.log" 2>&1
if grep -Eq "Warning|WARNING|Unable to process" "$work/generate.log"; then cat "$work/generate.log"; exit 1; fi
python3 -m venv "$work/env"
"$work/env/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$here/requirements.txt"
export PYTHONPATH="$work/client:$here"

# 2. The suite alone, on data with known violations.
"$work/env/bin/python" -m unittest discover -s "$here" -p 'test_*.py'

# 3. A project with the lab dataset and the stats definitions, served by `norn dev`.
project="$work/lab"
rm -rf "$project"
bun "$repo/tools/norn-ts/src/cli.ts" init "$project" --no-install > /dev/null
cp "$here/definitions/lab.ts" "$here/definitions/regression.ts" "$project/src/"
(cd "$project" && exec bun "$repo/tools/norn-ts/src/cli.ts" dev --json --port 0 > "$work/dev.ndjson" 2> "$work/dev.err") &
dev=$!
trap 'kill "$dev" 2>/dev/null || true; wait "$dev" 2>/dev/null || true' EXIT
for _ in $(seq 1 240); do
  grep -q '"event":"ready"' "$work/dev.ndjson" 2>/dev/null && break
  kill -0 "$dev" 2>/dev/null || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }
  sleep 0.5
done
grep -q '"event":"ready"' "$work/dev.ndjson" || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }

# 4. Seed, run the notebook, check what the cell recorded.
"$work/env/bin/python" "$here/seed.py" "$project" 2000
"$work/env/bin/python" "$here/check_regression.py" "$project" | tee "$work/notebook.out"
grep -q "Breusch-Pagan (LM)" "$work/notebook.out"
"$work/env/bin/python" "$here/e2e_check.py" "$project"
