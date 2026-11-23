#!/usr/bin/env bash
# Generate the Python client from the frozen OpenAPI document and run the
# notebook demo against a local development host (#521).
#
#   sdks/python/run-demo.sh WORKDIR
#
# Needs python3 (3.11 or later), bun, and NORN_DEV_HOST_BIN (a built
# `norn-dev-host`), as core.sh's integration phase provides. The client is
# generated into WORKDIR, never checked in: drift is impossible by
# construction. Fails on any generator warning, any type error in the
# generated package, or any refusal the demo meets.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
work="${1:?usage: run-demo.sh WORKDIR}"
: "${NORN_DEV_HOST_BIN:?NORN_DEV_HOST_BIN must name a built norn-dev-host}"
mkdir -p "$work"
work="$(cd "$work" && pwd)"

# 1. Generate, from a derived copy of the document (prepare.py says what differs).
python3 -m venv "$work/gen"
"$work/gen/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$here/requirements.txt"
python3 -I "$here/prepare.py" "$repo/sdks/openapi/norn-api.openapi.json" "$work/client-input.json"
rm -rf "$work/client"
mkdir -p "$work/client"
# The generator's post-hooks run `ruff` from this environment.
PATH="$work/gen/bin:$PATH" "$work/gen/bin/openapi-python-client" generate --path "$work/client-input.json" \
  --output-path "$work/client/norn_client" --meta none > "$work/generate.log" 2>&1
if grep -Eq "Warning|WARNING|Unable to process" "$work/generate.log"; then
  cat "$work/generate.log"
  echo "the generator reported warnings; the client would be missing pieces" >&2
  exit 1
fi

# 2. The demo's environment, and a type check of the generated package.
python3 -m venv "$work/demo"
"$work/demo/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes -r "$here/examples/requirements.txt"
(cd "$work/client" && "$work/demo/bin/python" -m mypy --no-incremental --config-file "$here/mypy.ini" norn_client)

# 3. A project with two linked types, served by `norn dev`.
project="$work/shop"
rm -rf "$project"
bun "$repo/tools/norn-ts/src/cli.ts" init "$project" --no-install > /dev/null
cp "$here/examples/demo-project/order.ts" "$project/src/order.ts"
(cd "$project" && exec bun "$repo/tools/norn-ts/src/cli.ts" dev --json --port 0 > "$work/dev.ndjson" 2> "$work/dev.err") &
dev=$!
trap 'kill "$dev" 2>/dev/null || true; wait "$dev" 2>/dev/null || true' EXIT
for _ in $(seq 1 240); do
  grep -q '"event":"ready"' "$work/dev.ndjson" 2>/dev/null && break
  kill -0 "$dev" 2>/dev/null || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }
  sleep 0.5
done
grep -q '"event":"ready"' "$work/dev.ndjson" || { cat "$work/dev.ndjson" "$work/dev.err"; exit 1; }
url="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["url"])' "$project/.norn/dev.json")"
token="$(cat "$project/.norn/dev.token")"

# Seed through the governed action route, as any client would.
call() {
  local status
  status="$(curl -sS -o "$work/call.json" -w '%{http_code}' -X POST \
    -H "authorization: Bearer $token" -H 'content-type: application/json' \
    "$url/v1/actions/$1?orgId=local&environmentId=dev" --data "$2")"
  [ "$status" = 200 ] || { echo "seed call $1 refused ($status): $(cat "$work/call.json")" >&2; exit 1; }
}
call shop.product.add '{"input":{"sku":"SKU-1","name":"Ring","price":"19.99"},"idempotencyKey":"seed-1"}'
call shop.product.add '{"input":{"sku":"SKU-2","name":"Chain","price":"42.50"},"idempotencyKey":"seed-2"}'
call shop.orderline.add '{"input":{"line":"L-1","sku":"SKU-1","quantity":"2"},"idempotencyKey":"seed-3"}'
call shop.orderline.add '{"input":{"line":"L-2","sku":"SKU-1","quantity":"1"},"idempotencyKey":"seed-4"}'

# 4. The demo, and what it must have shown.
PYTHONPATH="$work/client" "$work/demo/bin/python" "$here/examples/explore_cell.py" "$project" | tee "$work/demo.out"
for expected in "SKU-1" "SKU-2" "19.99" "L-1" "links touching shop.Product/SKU-1" "product"; do
  grep -q -- "$expected" "$work/demo.out" || { echo "the demo output lacks: $expected" >&2; exit 1; }
done
echo "python client demo: ok"
