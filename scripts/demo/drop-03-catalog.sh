#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/drop-common.sh"
drop_main "$@"
if [[ ! -f crates/norn-cell/src/catalog.rs ]]; then
  printf 'not yet: #467 #455 #457 (bounded kernel-authorized catalog seam)\n'; exit 1
fi
bun tools/norn-ts/src/cli.ts profile scripts/demo/jewelry.source.ts
bun tools/norn-ts/src/cli.ts compile scripts/demo/jewelry.source.ts --org org_demo --env dev \
  | cargo run --locked --quiet -p norn-cell --example catalog_demo
printf 'not yet: #341 #218 (analytical QuestionIR evaluation and catalog UI)\n'
