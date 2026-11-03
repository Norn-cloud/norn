#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/drop-common.sh"
drop_main "$@"
printf 'MVP-CONSTITUTION / T4-C2: authored loss mutation and finite runtime evidence\n'
(cd tools/norn-ts && bun test --timeout 60000 test/constitution.test.ts)
cargo run --locked --quiet -p norn-cell --example constitution
