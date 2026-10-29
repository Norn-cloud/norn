#!/usr/bin/env bash
# Build the checker ABI as an npm-consumable ES module package (T1-CHK-4,
# #2527) and print a size report.
#
# Usage: crates/norn-checker-wasm/scripts/build-pkg.sh [out-dir]
#   out-dir defaults to <target-dir>/norn-checker-wasm/pkg.
#
# Needs the wasm32-unknown-unknown target and the wasm-bindgen CLI at exactly
# the version pinned in crates/norn-checker-wasm/Cargo.toml. No wasm-pack,
# no global npm tooling. The output is `--target web`: an ES module whose
# default export `init()` fetches the .wasm next to it (bundlers such as Vite
# resolve it), plus `initSync({ module })` for Node or pre-fetched bytes.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-${root}/target}"
out="${1:-${target_dir}/norn-checker-wasm/pkg}"
wasm_manifest="${root}/crates/norn-checker-wasm/Cargo.toml"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "${wasm_manifest}" | head -n1)"
wasm_bindgen_pin="$(sed -n 's/^wasm-bindgen = "=\([^"]*\)"$/\1/p' "${wasm_manifest}")"

if [[ -z "${wasm_bindgen_pin}" ]]; then
  echo "error: no exact wasm-bindgen pin found in ${wasm_manifest}" >&2
  exit 1
fi
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "error: wasm-bindgen CLI ${wasm_bindgen_pin} is required but was not found on PATH" >&2
  exit 1
fi
cli_version="$(wasm-bindgen --version)"
expected_cli_version="wasm-bindgen ${wasm_bindgen_pin}"
if [[ "${cli_version}" != "${expected_cli_version}" ]]; then
  echo "error: ${cli_version} does not match the Cargo.toml pin (${expected_cli_version})" >&2
  exit 1
fi

cargo rustc --locked -p norn-checker-wasm --lib \
  --target wasm32-unknown-unknown --profile wasm-release --crate-type cdylib \
  --manifest-path "${root}/Cargo.toml"

wasm="${target_dir}/wasm32-unknown-unknown/wasm-release/norn_checker_wasm.wasm"
mkdir -p "${out}"
wasm-bindgen "${wasm}" --target web --out-dir "${out}" --out-name norn_checker_wasm \
  --remove-producers-section

cat > "${out}/package.json" <<JSON
{
  "name": "@norn/checker-wasm",
  "version": "${version}",
  "description": "Norn checker ABI (norn-checker-abi/1): JSON-in/JSON-out, byte-identical to the native checker",
  "type": "module",
  "main": "norn_checker_wasm.js",
  "module": "norn_checker_wasm.js",
  "types": "norn_checker_wasm.d.ts",
  "files": [
    "norn_checker_wasm.js",
    "norn_checker_wasm.d.ts",
    "norn_checker_wasm_bg.wasm",
    "norn_checker_wasm_bg.wasm.d.ts",
    "SIZE.txt"
  ],
  "sideEffects": false,
  "private": true
}
JSON

bg="${out}/norn_checker_wasm_bg.wasm"
raw_bytes="$(wc -c < "${bg}" | tr -d ' ')"
gzip_bytes="$(gzip -9 -n -c "${bg}" | wc -c | tr -d ' ')"
js_bytes="$(wc -c < "${out}/norn_checker_wasm.js" | tr -d ' ')"
raw_sha256="$(sha256sum < "${wasm}" | cut -d' ' -f1)"
bg_sha256="$(sha256sum < "${bg}" | cut -d' ' -f1)"
js_sha256="$(sha256sum < "${out}/norn_checker_wasm.js" | cut -d' ' -f1)"
{
  echo "norn-checker-wasm ${version} size report (profile wasm-release: opt-level=s, lto, codegen-units=1; no wasm-opt)"
  echo "norn_checker_wasm_bg.wasm: ${raw_bytes} bytes (gzip -9: ${gzip_bytes} bytes)"
  echo "norn_checker_wasm.js:      ${js_bytes} bytes"
  echo "raw WASM SHA-256:           ${raw_sha256}"
  echo "generated WASM SHA-256:     ${bg_sha256}"
  echo "generated JS SHA-256:       ${js_sha256}"
} | tee "${out}/SIZE.txt"
echo "package: ${out}"
