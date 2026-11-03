#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
(cd tools/norn-ts && bun test test/rate.test.ts --timeout 60000)
cargo test --locked -p norn-cell --test jewelry_pricing -- --test-threads=2
