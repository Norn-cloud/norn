#!/usr/bin/env bash
set -euo pipefail
source "$(git rev-parse --show-toplevel)/scripts/demo/drop-common.sh"
drop_main "$@"
if [[ ! -f crates/norn-cell/tests/jewelry_pricing.rs ]]; then
  printf 'not yet: #510 (checked pricing commits)\n'; exit 1
fi
(cd tools/norn-ts && bun test test/rate.test.ts --timeout 60000)
cargo test --locked -p norn-cell --test jewelry_pricing -- --test-threads=2
printf 'not yet: #213 (T1-C1 inventory valuation, balanced books, assays and full jewelry cycle)\n'
