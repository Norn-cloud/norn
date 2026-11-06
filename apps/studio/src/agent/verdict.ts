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

const isText = (value: unknown): value is string => typeof value === "string"
const optionalText = (value: unknown) => value === undefined || value === null || isText(value)

const isProposal = (value: unknown) => isObject(value) && isText(value.artifact) && isText(value.pointer)

/** A law as the card reads it: its artifact, and plain-text name, rationale and expression where given. */
const isLaw = (value: unknown) =>
  isObject(value) && isText(value.artifactId) && optionalText(value.name) && optionalText(value.rationale) && optionalText(value.expression)

const isRefusal = (value: unknown) =>
  isObject(value) && isText(value.code) && isText(value.message) && optionalText(value.artifactId) &&
  (value.law === undefined || value.law === null || isLaw(value.law))

/**
 * A tool output is a verdict only if every field the cards read has the shape
 * the Worker writes (proposal, stage, receipt, each refusal and its law);
 * anything else is shown as unreadable rather than crashing the card.
 */
export function verdictOf(output: unknown): Verdict | undefined {
  if (!isObject(output) || !isProposal(output.proposal)) return undefined
  if ("error" in output) return isText(output.error) ? output as unknown as Verdict : undefined
  if (typeof output.admitted !== "boolean" || !isText(output.stage) || !Object.hasOwn(STAGE, output.stage)) return undefined
  if (!(output.receiptSha256 === null || output.receiptSha256 === undefined || isText(output.receiptSha256))) return undefined
  if (!Array.isArray(output.refusals) || !output.refusals.every(isRefusal)) return undefined
  return output as unknown as Verdict
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
  "Make the positive-quantity law check a field called unseen instead",
  "Let the order quantity law just be the ordered amount, not a yes/no test",
  "Let Receive start from a shipped state",
  "Let Receive start straight from draft",
] as const
