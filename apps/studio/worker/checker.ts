// worker/checker/ holds the checker ABI package (crates/norn-checker-wasm,
// `--target web`), copied in by alchemy.run.ts at deploy; it is not committed.
import { checkArtifactSet, initSync } from "./checker/norn_checker_wasm.js"
import wasm from "./checker/norn_checker_wasm_bg.wasm"
import type { CheckResult } from "./proposal.ts"

let ready = false

/** Run the Rust checker (compiled to WebAssembly) over a candidate artifact set. */
export function check(artifacts: readonly unknown[], context: string): CheckResult {
  if (!ready) {
    initSync({ module: wasm })
    ready = true
  }
  return JSON.parse(checkArtifactSet(JSON.stringify(artifacts), context)) as CheckResult
}
