import { CHECKER_PKG_ENV, loadChecker, staleCheckerWarning } from "../src/checker-abi.ts"

// Bun shares this module across suites: initialize the WASM checker once,
// outside the per-test timeout, while each admission still checks fresh input.
// A cached checker this checkout did not build is never used: its refusals
// would be false failures (the Rust-checker tests skip instead, with why).
const stale = staleCheckerWarning()
if (stale) console.warn(`${stale}; Rust-checker tests skip`)
export const checker = stale ? undefined : await loadChecker()
export { CHECKER_PKG_ENV }
