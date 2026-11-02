#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/drop-common.sh"
drop_main "$@"
printf "What's proved, what's not — admitted pack laws and checked evidence\n"
python3 scripts/demo/guarantees_drop.py
