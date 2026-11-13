# norn-expr-wasm

The ZEN engine alone (D105, UI-MACHINE-1): the checker ABI's `evaluateExpression`, built
from `norn-expr` without the rest of the checker. `norn-checker-wasm` re-exports this very
function (`evaluate_expression_json`), so both packages evaluate with the same code; a page
machine's guards and assignments load this package (about 0.8 MB) instead of the whole
checker (about 10 MB). Ring 1; see `boundary.json`.

| JavaScript | Rust | Result |
|---|---|---|
| `evaluateExpression(expr, inputJson, profile?)` | `evaluate_expression_json` | `norn-checker-abi/evaluation@1`, exactly as `@norn/checker-wasm` returns it |

Build with `scripts/build-pkg.sh [out-dir]` (wasm32 target, the pinned wasm-bindgen CLI, then
`wasm-opt -Oz`; `NORN_WASM_OPT=0` skips the optimizer). The JS binding is the `bindings`
feature, which only this build enables. `tests/parity/engine-parity.mjs` holds the built
package to the checker package's answers on a shared corpus (CI's integration phase).
