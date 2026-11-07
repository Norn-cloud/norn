import type { UIMessage } from "ai"

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

/** The id this page uses, fixed at its first call: from storage when it holds one, else new; written back whenever storage works. */
let memoryId: string | undefined

/** One id per browser tab, for the server's per-session budget. */
export function sessionId(storage: Pick<Storage, "getItem" | "setItem"> | undefined): string {
  // The page keeps the first id it used, even if storage later starts working with another one.
  try {
    const existing = storage?.getItem("norn-agent-session")
    if (memoryId === undefined && existing && /^[0-9a-f-]{36}$/.test(existing)) memoryId = existing
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

/** What a turn is doing while the server works, in the order it happens. */
export const STEPS = ["Drafting a proposal", "Checking it against every law", "Reading the checker's answer"] as const

/** How far the current turn has got: drafting until it proposes, checking until the checker answers. */
export function stepOf(message: UIMessage | undefined): number {
  const proposal = message?.role === "assistant" ? message.parts.filter((part) => part.type === "tool-propose").at(-1) : undefined
  if (proposal === undefined) return 0
  const state = "state" in proposal ? proposal.state : undefined
  return state === "output-available" || state === "output-error" ? 2 : 1
}
