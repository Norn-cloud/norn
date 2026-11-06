import type { CheckResult } from "./diagnostics.ts"

/** The Rust checker compiled to WebAssembly. Feedback while editing only: the server re-checks on save and decides. */
export type Checker = (artifacts: readonly unknown[]) => CheckResult

interface CheckerModule {
  readonly default: (input?: unknown) => Promise<unknown>
  readonly checkArtifactSet: (artifactsJson: string, contextJson: string) => string
}

// The checker ABI package (crates/norn-checker-wasm, `--target web`), built into
// place before `vite build` and not committed: worker/checker/ where the Studio
// Worker's deploy puts it, or src/checker/ for a build without the Worker.
// A glob, so a build without it still succeeds and the canvas says the checker is absent.
const PACKAGE = import.meta.glob<CheckerModule>(["../../../worker/checker/norn_checker_wasm.js", "../../checker/norn_checker_wasm.js"])

/** Load the checker; rejects when the package is absent, so an edit is never shown as clean unchecked. */
export async function loadChecker(packages: Record<string, () => Promise<CheckerModule>> = PACKAGE): Promise<Checker> {
  const load = Object.values(packages)[0]
  if (!load) throw new Error("The checker is not part of this build")
  const module = await load()
  await module.default()
  return (artifacts) => JSON.parse(module.checkArtifactSet(JSON.stringify(artifacts), "null")) as CheckResult
}

/** The context an agent is checked in: the cell's model allowlist, the org's bound tools and the admitted laws. */
export interface AgentContext {
  readonly models: readonly string[]
  readonly bindings: readonly string[]
  readonly laws: readonly string[]
}

/** The checker's agent admission (`checkAgent`, `norn-checker-abi/agent@1`). */
export type AgentChecker = (agent: unknown, context: AgentContext) => { readonly admitted: boolean; readonly diagnostics: CheckResult["diagnostics"] }

/** Load the agent check; rejects when the build's checker predates it, so an agent is never shown as clean unchecked. */
export async function loadAgentChecker(packages: Record<string, () => Promise<CheckerModule & { readonly checkAgent?: (agentJson: string, contextJson: string) => string }>> = PACKAGE): Promise<AgentChecker> {
  const load = Object.values(packages)[0]
  if (!load) throw new Error("The checker is not part of this build")
  const module = await load()
  await module.default()
  const check = module.checkAgent
  if (!check) throw new Error("This build's checker has no agent check yet")
  return (agent, context) => JSON.parse(check(JSON.stringify(agent), JSON.stringify(context))) as ReturnType<AgentChecker>
}
