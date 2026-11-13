import type { CheckResult } from "./diagnostics.ts"

/** The Rust checker compiled to WebAssembly. Feedback while editing only: the server re-checks on save and decides. */
export type Checker = (artifacts: readonly unknown[]) => CheckResult

interface CheckerModule {
  readonly default: (input?: unknown) => Promise<unknown>
  readonly checkArtifactSet: (artifactsJson: string, contextJson: string) => string
  readonly evaluateExpression: (expression: string, inputJson: string, profile?: string) => string
}

// src/checker/ holds the checker ABI package (crates/norn-checker-wasm,
// `--target web`), built into place by `bun run checker` (the deploy script runs it before `vite build`); it is not committed.
// A glob, so a build without it still succeeds and the canvas says the checker is absent.
// Called, not evaluated at import: only Vite provides import.meta.glob, and tests import this module through the pages.
const PACKAGE = () => import.meta.glob<CheckerModule>("../../checker/norn_checker_wasm.js")

/** Load the checker; rejects when the package is absent, so an edit is never shown as clean unchecked. */
export async function loadChecker(packages: Record<string, () => Promise<CheckerModule>> = PACKAGE()): Promise<Checker> {
  const load = Object.values(packages)[0]
  if (!load) throw new Error("The checker is not part of this build")
  const module = await load()
  await module.default()
  return (artifacts) => JSON.parse(module.checkArtifactSet(JSON.stringify(artifacts), "null")) as CheckResult
}

/**
 * The checker's ZEN engine, for page machines (UI-MACHINE-1): the guards and assignments the
 * checker type-checked at admission evaluate in the same engine here. Rejects when the package is
 * absent, so a page never runs a machine with a different evaluator.
 */
export async function loadEngine(packages: Record<string, () => Promise<CheckerModule>> = PACKAGE()): Promise<Pick<CheckerModule, "evaluateExpression">> {
  const load = Object.values(packages)[0]
  if (!load) throw new Error("The checker is not part of this build")
  const module = await load()
  await module.default()
  return module
}
