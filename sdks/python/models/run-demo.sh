#!/usr/bin/env bash
# Model versions, their promotion and applying a model, end to end against a local development host (#1171).
#
#   sdks/python/models/run-demo.sh WORKDIR
#
# Needs what ../regression/run-demo.sh needs. Serves the tracking and ml packs
# with the demo pack in demo/ (customers, a churn model and its typed
# predictions), then runs e2e_promotion.py (registration from finished runs only
# and the promotion law) and e2e_apply.py (norn_models.apply_model: predictions
# written and linked to their version, refused before any write when a contract
# fails or the version is not in production). The approval gate needs a hosted cell's roles
# (crates/norn-cell-do/tests/ml_approval.rs).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdk="$(cd "$here/.." && pwd)"
repo="$(cd "$sdk/../.." && pwd)"
work="${1:?usage: run-demo.sh WORKDIR}"
: "${NORN_DEV_HOST_BIN:?NORN_DEV_HOST_BIN must name a built norn-dev-host}"
mkdir -p "$work"
work="$(cd "$work" && pwd)"

python3 -m venv "$work/gen"
"$work/gen/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$sdk/requirements.txt"
python3 -I "$sdk/prepare.py" "$repo/sdks/openapi/norn-api.openapi.json" "$work/client-input.json"
rm -rf "$work/client" && mkdir -p "$work/client"
PATH="$work/gen/bin:$PATH" "$work/gen/bin/openapi-python-client" generate --path "$work/client-input.json" \
  --output-path "$work/client/norn_client" --meta none > "$work/generate.log" 2>&1
if grep -Eq "Warning|WARNING|Unable to process" "$work/generate.log"; then cat "$work/generate.log"; exit 1; fi
python3 -m venv "$work/env"
"$work/env/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$sdk/regression/requirements.txt"
export PYTHONPATH="$work/client:$sdk/regression"

project="$work/models"
rm -rf "$project"
bun "$repo/tools/norn-ts/src/cli.ts" init "$project" --no-install --template empty > /dev/null
cp "$repo/registry/packs/tracking/tracking.ts" "$repo/registry/packs/ml/ml.ts" "$here/demo/sales.ts" "$project/src/"
(cd "$project" && exec bun "$repo/tools/norn-ts/src/cli.ts" dev --json --port 0 > "$work/dev.ndjson" 2> "$work/dev.err") &
dev=$!
trap 'kill "$dev" 2>/dev/null || true; wait "$dev" 2>/dev/null || true' EXIT
for _ in $(seq 1 240); do
  grep -q '"event":"ready"' "$work/dev.ndjson" 2>/dev/null && break
  kill -0 "$dev" 2>/dev/null || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }
  sleep 0.5
done
grep -q '"event":"ready"' "$work/dev.ndjson" || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }
"$work/env/bin/python" "$here/e2e_promotion.py" "$project"
"$work/env/bin/python" "$here/e2e_apply.py" "$project"
