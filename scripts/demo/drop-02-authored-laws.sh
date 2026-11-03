#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/drop-common.sh"
drop_main "$@"
if [[ ! -f scripts/demo/jewelry.source.ts || ! -f crates/norn-checker/examples/jewelry_demo.rs ]]; then
  printf 'not yet: #359 #366 (TS-authored law admission rehearsal)\n'; exit 1
fi
if ! grep -Fq 'Norn.law(' scripts/demo/jewelry.source.ts; then
  printf 'not yet: #366 (law authored in Norn)\n'; exit 1
fi
if [[ ! -f crates/norn-ir/src/requirement_display.rs ]] \
  || ! grep -Fq 'RequirementView' crates/norn-checker/examples/jewelry_demo.rs; then
  printf 'not yet: #322 #366 (real EN/AR rendering wired to the authored law)\n'; exit 1
fi
printf 'Authored conservation law, checker admission/refusals and EN/AR views\n'
bun tools/norn-ts/src/cli.ts profile scripts/demo/jewelry.source.ts
bun tools/norn-ts/src/cli.ts compile scripts/demo/jewelry.source.ts --org org_demo --env dev \
  | cargo run --locked --quiet -p norn-checker --example jewelry_demo
printf 'checker-admitted / refused; obligations generated; runtime execution not yet (#341 #342 #343)\n'
