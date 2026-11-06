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

const isObject = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object"

/** A tool output is a verdict only if it has the fields the Worker writes; anything else is shown as unreadable. */
export function verdictOf(output: unknown): Verdict | undefined {
  if (!isObject(output) || !isObject(output.proposal)) return undefined
  if (typeof output.error === "string") return output as unknown as Verdict
  if (typeof output.admitted === "boolean" && Array.isArray(output.refusals) && output.refusals.every((refusal) => isObject(refusal) && typeof refusal.message === "string")) return output as unknown as Verdict
  return undefined
}

/** The tab's session storage, or undefined where the browser blocks it (reading the property can throw). */
export function tabStorage(): Pick<Storage, "getItem" | "setItem"> | undefined {
  try { return globalThis.sessionStorage } catch { return undefined }
}

/** The id this tab uses when its storage is missing or fails, so the id still holds for the page's lifetime. */
let memoryId: string | undefined

/** One id per browser tab, for the server's per-session budget. */
export function sessionId(storage: Pick<Storage, "getItem" | "setItem"> | undefined): string {
  try {
    const existing = storage?.getItem("norn-agent-session")
    if (existing && /^[0-9a-f-]{36}$/.test(existing)) return existing
  } catch { /* storage blocked: fall back to the in-memory id */ }
  memoryId ??= crypto.randomUUID()
  try { storage?.setItem("norn-agent-session", memoryId) } catch { /* storage blocked or full: keep the in-memory id */ }
  return memoryId
}

/**
 * Prompts from the stress corpus of invalid proposals (fixtures/stress/invalid-proposals.json),
 * worded as an economist would ask; the checker should refuse each one.
 */
export const SUGGESTIONS = [
  // Each is recorded with its change and the checker's decision in fixtures/demo/jewelry-proposals.json:
  // the first two are admitted, the last two refused, naming the fine-mass law.
  "Record each melt's purity in parts per thousand",
  "Make the fine-mass law a yield floor: output at least 95% of input",
  "Subtract a refining loss in the fine-mass law",
  "Let a melt post straight from measured, skipping approval",
] as const
