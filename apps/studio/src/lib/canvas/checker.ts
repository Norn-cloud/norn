import type { CheckResult } from "./diagnostics.ts"

/** The Rust checker compiled to WebAssembly. Feedback while editing only: the server re-checks on save and decides. */
export type Checker = (artifacts: readonly unknown[]) => CheckResult

interface CheckerModule {
  readonly default: (input?: unknown) => Promise<unknown>
  readonly checkArtifactSet: (artifactsJson: string, contextJson: string) => string
}

// src/checker/ holds the checker ABI package (crates/norn-checker-wasm,
// `--target web`), built into place before `vite build`; it is not committed.
// A glob, so a build without it still succeeds and the canvas says the checker is absent.
const PACKAGE = import.meta.glob<CheckerModule>("../../checker/norn_checker_wasm.js")

/** Load the checker; rejects when the package is absent, so an edit is never shown as clean unchecked. */
export async function loadChecker(packages: Record<string, () => Promise<CheckerModule>> = PACKAGE): Promise<Checker> {
  const load = Object.values(packages)[0]
  if (!load) throw new Error("The checker is not part of this build")
  const module = await load()
  await module.default()
  return (artifacts) => JSON.parse(module.checkArtifactSet(JSON.stringify(artifacts), "null")) as CheckResult
}
