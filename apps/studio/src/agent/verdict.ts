/** The checker's answer to one proposal, as the agent Worker's propose tool returns it. */
export interface Law {
  readonly artifactId: string
  readonly name: string | undefined
  readonly rationale: string | undefined
  readonly expression: string | undefined
}

export interface Refusal {
  readonly code: string
  readonly message: string
  readonly artifactId: string | null
  readonly law: Law | undefined
}

export interface Proposal {
  readonly artifact: string
  readonly pointer: string
  readonly value: unknown
}

export type Verdict =
  | { readonly proposal: Proposal; readonly admitted: boolean; readonly stage: "shape" | "resolution" | "model" | "admitted"; readonly refusals: readonly Refusal[]; readonly receiptSha256: string | null }
  | { readonly proposal: Proposal; readonly error: string }

/** Where the checker stopped, in plain words. */
export const STAGE: Record<string, string> = {
  shape: "Stopped at: the definition's shape",
  resolution: "Stopped at: how definitions depend on each other",
  model: "Stopped at: what the law means",
  admitted: "Admitted",
}

/** A tool output is a verdict only if it has the fields the Worker writes; anything else is shown as unreadable. */
export function verdictOf(output: unknown): Verdict | undefined {
  if (output === null || typeof output !== "object" || !("proposal" in output)) return undefined
  if ("error" in output && typeof output.error === "string") return output as Verdict
  if ("admitted" in output && typeof output.admitted === "boolean" && "refusals" in output && Array.isArray(output.refusals)) return output as Verdict
  return undefined
}

/** One id per browser tab, for the server's per-session budget. */
export function sessionId(storage: Pick<Storage, "getItem" | "setItem"> | undefined): string {
  const existing = storage?.getItem("norn-agent-session")
  if (existing && /^[0-9a-f-]{36}$/.test(existing)) return existing
  const id = crypto.randomUUID()
  storage?.setItem("norn-agent-session", id)
  return id
}

/**
 * Prompts from the stress corpus of invalid proposals (fixtures/stress/invalid-proposals.json),
 * worded as an economist would ask; the checker should refuse each one.
 */
// Each is recorded with its change and the checker's decision in fixtures/demo/jewelry-proposals.json.
/** Requests the model turns into a proposal; the checker admits both. */
export const SUGGESTIONS = [
  "Record each melt's purity in parts per thousand",
  "Make the fine-mass law a yield floor: output at least 95% of input",
] as const

/** Example proposals: the server checks their recorded typed change directly, without the model; both are refused, naming the fine-mass law. */
export const EXAMPLES = [
  "Subtract a refining loss in the fine-mass law",
  "Let a melt post straight from measured, skipping approval",
] as const
