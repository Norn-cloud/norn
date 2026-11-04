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
  shape: "The definition's shape",
  resolution: "How definitions depend on each other",
  model: "What the law or rule means",
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
export const SUGGESTIONS = [
  "Make the positive-quantity law check a field called unseen instead",
  "Let the order quantity law just be the ordered amount, not a yes/no test",
  "Let Receive start from a shipped state",
  "Let Receive start straight from draft",
] as const
