// How a server answer becomes one of the declared call outcomes (UI-MACHINE-1). The classification
// reads only what the server said: its status, and the `code` and `category` of a coded refusal
// (`norn/refusal@2`). Nothing here judges permission; a refusal is shown as the server sent it.
//
// A route the host does not serve is `not_served`, never a network failure (the SDK's
// `NornUnavailable.notServed`, `packages/norn-sdk/src/client.ts`): a 404 that is no coded refusal,
// the coded `route.not_found`, and the 503 a deployment with no host bound answers
// (`{ "error": NO_HOST_ERROR }`). Any other uncoded answer is a cell that did not answer.

import { NO_HOST_ERROR } from "../../../norn-sdk/src/client.ts"
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
  if (notServed(status, document, code)) return { outcome: "not_served", payload: {}, body }
  if (status === 409 && code !== null && ANSWER_MOVED_CODES.has(code)) {
    return { outcome: "answer_moved", payload: { code }, body }
  }
  if (code !== null) {
    const declared = typeof document?.category === "string" && CATEGORIES.has(document.category) ? (document.category as RefusalCategory) : null
    const category = declared ?? categoryOfStatus(status)
    const retryable = document?.retryable === true
    return { outcome: "refused", category, payload: { code, retryable }, body }
  }
  // No coded refusal and a served route: a proxy failed, or the cell did not answer.
  return { outcome: "network_failure", payload: {}, body }
}

/**
 * Classify one HTTP answer from its body text, parsed by `parse` (an exact parser keeps every number's
 * text). A body that is no JSON, such as a proxy's HTML error page, is classified as no body: a 404
 * page is still not served, a gateway's 503 page still no answer. A success whose body is no JSON
 * (an HTML page a host served in place of the route) is no answer from the cell either.
 */
export function classifyText(status: number, text: string, parse: (text: string) => unknown = JSON.parse): Classified {
  let body: unknown
  if (text !== "") {
    try {
      body = parse(text)
    } catch {
      if (status >= 200 && status < 300) return { outcome: "network_failure", payload: {} }
      body = undefined
    }
  }
  return classifyResponse(status, body)
}

/** The code with which a host says it has no such route at all. */
export const ROUTE_NOT_FOUND = "route.not_found"

/** Whether the answer says the host does not serve the route (see the module comment). */
function notServed(status: number, document: Record<string, unknown> | null, code: string | null): boolean {
  if (code === ROUTE_NOT_FOUND) return true
  if (status === 404) return code === null
  return status === 503 && code === null && document?.error === NO_HOST_ERROR
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
