#!/usr/bin/env bash
# Build the cell ABI (T4-CELL-4, #2678) as the ES module apps/cell-shim
# imports, and print a size report.
#
# Usage: crates/norn-cell-wasm/scripts/build-pkg.sh [out-dir]
#   out-dir defaults to apps/cell-shim/wasm (gitignored).
#
# Needs the wasm32-unknown-unknown target and the wasm-bindgen CLI at exactly
# the version pinned in Cargo.toml (0.2.129, the same pin as
# norn-checker-wasm). The output is `--target web`; the shim calls
# `initSync({ module })` with the compiled module (a Workers `.wasm` import
# on Durable Objects, `new WebAssembly.Module(bytes)` under Bun).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-${root}/target}"
out="${1:-${root}/apps/cell-shim/wasm}"

cargo rustc --locked -p norn-cell-wasm --lib \
  --target wasm32-unknown-unknown --profile wasm-release --crate-type cdylib \
  --manifest-path "${root}/Cargo.toml"

mkdir -p "${out}"
wasm-bindgen "${target_dir}/wasm32-unknown-unknown/wasm-release/norn_cell_wasm.wasm" \
  --target web --out-dir "${out}" --out-name norn_cell_wasm --remove-producers-section

bg="${out}/norn_cell_wasm_bg.wasm"
{
  echo "norn-cell-wasm size report (profile wasm-release; no wasm-opt)"
  echo "norn_cell_wasm_bg.wasm: $(wc -c < "${bg}" | tr -d ' ') bytes (gzip -9: $(gzip -9 -c "${bg}" | wc -c | tr -d ' ') bytes)"
} | tee "${out}/SIZE.txt"
