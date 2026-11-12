// How a server answer becomes one of the declared call outcomes (UI-MACHINE-1). The classification
// reads only what the server said: its status, and the `code` and `category` of a coded refusal
// (`norn/refusal@2`). Nothing here judges permission; a refusal is shown as the server sent it.

import type { CallOutcome, Fields, RefusalCategory } from "./machine.ts"

export interface Classified {
  readonly outcome: CallOutcome
  readonly category?: RefusalCategory
  readonly payload: Fields
  /** The parsed body, when there was one, for the page to show. */
  readonly body?: unknown
}

const CATEGORIES: ReadonlySet<string> = new Set(["visibility", "permission", "precondition", "conflict", "invalid", "limit", "internal"])

/** Codes with which the server says a read's answer or snapshot moved under it. */
export const ANSWER_MOVED_CODES: ReadonlySet<string> = new Set(["core.projection.answer_moved", "object.snapshot_moved"])

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null
}

/** Classify one HTTP answer. `body` is the parsed JSON body, or undefined when there was none. */
export function classifyResponse(status: number, body: unknown): Classified {
  const document = record(body)
  const code = typeof document?.code === "string" ? document.code : null
  if (status === 202) {
    const decision = record(document?.decision)
    const decisionId = typeof decision?.id === "string" ? decision.id : ""
    return { outcome: "pending_approval", payload: { decisionId }, body }
  }
  if (status >= 200 && status < 300) return { outcome: "accepted", payload: {}, body }
  if (status === 409 && code !== null && ANSWER_MOVED_CODES.has(code)) {
    return { outcome: "answer_moved", payload: { code }, body }
  }
  if (code !== null) {
    const declared = typeof document?.category === "string" && CATEGORIES.has(document.category) ? (document.category as RefusalCategory) : null
    const category = declared ?? categoryOfStatus(status)
    const retryable = document?.retryable === true
    return { outcome: "refused", category, payload: { code, retryable }, body }
  }
  // No coded refusal: the route is not served, a proxy failed, or the cell is unreachable.
  return { outcome: "network_failure", payload: {}, body }
}

/** The category a coded refusal without one belongs to: `RefusalCategory::of_status` in refusal.rs. */
function categoryOfStatus(status: number): RefusalCategory {
  if (status === 401 || status === 403) return "permission"
  if (status === 404) return "visibility"
  if (status === 409) return "conflict"
  if (status === 413 || status === 429) return "limit"
  if (status >= 400 && status <= 499) return "invalid"
  return "internal"
}
