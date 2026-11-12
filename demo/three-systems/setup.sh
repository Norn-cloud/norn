#!/usr/bin/env bash
# Build the three-systems demo project: the core and the canon, ERPNext,
# Shopify and Odoo packs as their generated registry artifacts, which the
# project compiles with its own sources (`norn dev` admits artifact envelopes
# found in source directories).
#
#   demo/three-systems/setup.sh PROJECT_DIR
#   cd PROJECT_DIR && norn dev --load <snapshot.json> ...
#
# Snapshots: registry/packs/<system>/synthetic.json (7 rows each), or the
# generated set from demo/data/generate.py.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
project="${1:?usage: setup.sh PROJECT_DIR}"
[ -e "$project" ] && { echo "$project exists; choose a new directory" >&2; exit 2; }
bun "$repo/tools/norn-ts/src/cli.ts" init "$project" --no-install > /dev/null
rm -f "$project/src/product.ts"
for d in core packs/canon packs/erpnext packs/shopify packs/odoo packs/locale-ar; do
  mkdir -p "$project/definitions/$d"
  cp "$repo/registry/$d/artifacts/"*.json "$project/definitions/$d/"
done
# Outside the three-systems profile (as the retained explorer catalog leaves them out).
for x in commerce.SalesOrder.entity core.Order.interface core.Entitlement.entity identity.BusinessUnit.entity location.Region.entity location.Space.entity party.Brand.entity; do
  rm -f "$project/definitions/core/$x.json"
done
python3 - "$project/norn.json" <<'PY'
import json, sys
path = sys.argv[1]
manifest = json.load(open(path))
manifest["sources"] = ["src", "definitions/core", "definitions/packs/canon", "definitions/packs/erpnext",
                       "definitions/packs/shopify", "definitions/packs/odoo", "definitions/packs/locale-ar"]
open(path, "w").write(json.dumps(manifest, indent=2) + "\n")
PY
echo "project ready: $project"
