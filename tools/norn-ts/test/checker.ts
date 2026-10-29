import { CHECKER_PKG_ENV, loadChecker } from "../src/checker-abi.ts"

// Bun shares this module across suites: initialize the WASM checker once,
// outside the per-test timeout, while each admission still checks fresh input.
export const checker = await loadChecker()
export { CHECKER_PKG_ENV }
