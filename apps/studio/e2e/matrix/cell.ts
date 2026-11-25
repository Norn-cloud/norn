import type { Page, Route } from "@playwright/test"
import { NO_HOST_ERROR } from "../../../../packages/norn-sdk/src/client.ts"
import type { Msg } from "../../../../packages/norn-react/src/machine/index.ts"
import { ACTION_ID, OBJECT_TYPE, SCOPE } from "../../../../packages/norn-react/test/fixtures/matrix-page.ts"
import { session } from "../object-cell.ts"

/**
 * The matrix's stand-in cell (#1762 milestone C). It answers the fixture page's routes with the wire
 * envelopes the real cell sends, so the renderer is proven against what it will read:
 *
 * - a read: `NornObjectPage` with PRIVACY-1 projections (a redacted value, a refused one);
 * - a refusal: `norn/refusal@2` with its code and category;
 * - an approval: 202 `PendingAction` (APPROVAL-1);
 * - not served: a 404 with no refusal, or the no-host 503 (`{ "error": NO_HOST_ERROR }`);
 * - unavailable: a gateway's 503 page while the host restarts;
 * - an answer that moved: 409 `object.snapshot_moved`;
 * - a timeout: no answer at all, until the page's deadline passes on the test's clock.
 *
 * Each answer is held until the matrix releases it, so the page is always seen pending first.
 */

export interface Answer { readonly status: number; readonly body: unknown; readonly contentType?: string }

export const STATUS = { visibility: 404, permission: 403, precondition: 422, conflict: 409, invalid: 422, limit: 429, internal: 500 } as const

/** Two records: one with a redacted value (masked), one whose value the policy refuses (withheld). */
export const RECORDS = {
  objectType: OBJECT_TYPE,
  records: [
    { objectType: OBJECT_TYPE, id: "vp-1", scope: `${SCOPE.orgId}/${SCOPE.environmentId}`, version: 3, values: { id: "vp-1", label: "•••", valuation_method: "fifo" }, masked: { label: { mode: "redacted", markings: ["confidential"], revealable: false } } },
    { objectType: OBJECT_TYPE, id: "vp-2", scope: `${SCOPE.orgId}/${SCOPE.environmentId}`, version: 1, values: { id: "vp-2", label: "Moving average" }, masked: { valuation_method: { mode: "refuse", markings: ["restricted"], revealable: false } } },
  ],
  quarantineCount: 0,
  scannedThrough: null,
}

const refusal = (code: string, category: keyof typeof STATUS, message: string): Answer =>
  ({ status: STATUS[category], body: { format: "norn/refusal@2", code, category, retryable: false, reason: null, message, explanation: "" } })

/** How a not-served route answers: the cell's 404, or a deployment with no host bound. */
export type NotServed = "404" | "no-host"

/** The stand-in's answer to the call a message's outcome closes. `null` never answers (a timeout). */
export function answerFor(msg: Extract<Msg, { kind: "outcome" }>, call: "read" | "action", notServed: NotServed = "404"): Answer | null {
  switch (msg.outcome) {
    case "accepted":
      return call === "read"
        ? { status: 200, body: RECORDS }
        : { status: 200, body: { outcome: "committed", objectType: OBJECT_TYPE, objectId: "fixture-1", version: 4, receipt: { receiptHash: "b".repeat(64), commitSeq: 12, recordedAtMs: Date.UTC(2026, 10, 25, 9) } } }
    case "pending_approval":
      return { status: 202, body: { outcome: "pending", decision: { id: "apv_matrix", expiresAtMs: Date.UTC(2026, 10, 26) } } }
    case "refused": {
      const category = msg.category ?? "permission"
      return refusal(msg.code ?? (category === "permission" ? "authz.denied" : `fixture.${category}`), category, "The cell refused this.")
    }
    case "not_served":
      return notServed === "404" ? { status: 404, body: "" } : { status: 503, body: { error: NO_HOST_ERROR } }
    case "network_failure":
      return { status: 503, body: "<html><body>Service Unavailable</body></html>", contentType: "text/html" }
    case "answer_moved":
      return { status: 409, body: { format: "norn/refusal@2", code: "object.snapshot_moved", category: "conflict", retryable: true, reason: null, message: "The records moved while they were read.", explanation: "" } }
    case "timeout":
    case "cancelled":
    case "stale":
    case "subscription_refused":
      return null
  }
}

interface Held { readonly answer: Answer | null; readonly gate: Promise<void>; readonly open: () => void }

/** One route's queue of held answers: each call takes the next, and waits until it is released. */
function queue() {
  const pending: Held[] = []
  let inFlight: Held | undefined
  return {
    push(answer: Answer | null) {
      let open = () => {}
      const gate = new Promise<void>((resolve) => { open = resolve })
      pending.push({ answer, gate, open })
    },
    async take(route: Route) {
      const next = pending.shift()
      if (next === undefined) return route.fulfill({ status: 404, body: "" })
      inFlight = next
      await next.gate
      if (next.answer === null) return
      const { status, body, contentType } = next.answer
      await route.fulfill({ status, contentType: contentType ?? "application/json", body: typeof body === "string" ? body : JSON.stringify(body) })
    },
    get inFlight() { return inFlight !== undefined },
    release() {
      inFlight?.open()
      inFlight = undefined
    },
  }
}

/**
 * Stand in for the cell behind the matrix page: the session, the definitions (the demo set plus the
 * fixture widget), the action catalog, and the fixture's read and call, each answered from a queue.
 */
export async function matrixCell(page: Page, definitions: readonly unknown[]) {
  await session(page, SCOPE.environmentId)
  const reads = queue()
  const calls = queue()
  const sent: { readonly idempotencyKey: string; readonly input: unknown }[] = []
  // Lowest priority first: anything else the page asks is a route this cell does not serve.
  const escape = (text: string) => text.replaceAll(".", "\\.")
  await page.route(/\/api\/host\//, (route: Route) => route.fulfill({ status: 404, json: { code: "route.not_found", message: "Not served by this stand-in." } }))
  await page.route(/\/api\/host\/v1\/definitions(\?|$)/, (route: Route) => route.fulfill({ json: { artifacts: definitions } }))
  await page.route(/\/api\/host\/v1\/actions(\?|$)/, (route: Route) => route.fulfill({
    json: { definitionsVersion: 1, items: [{ actionId: ACTION_ID, title: "Set", objectKind: OBJECT_TYPE, effect: "write", objectIdField: "id", inputSchema: {}, uiEffects: { irreversible: false, destructive: false, money: [] } }] },
  }))
  await page.route(new RegExp(`/api/host/v1/objects/${escape(OBJECT_TYPE)}(\\?|$)`), (route: Route) => reads.take(route))
  await page.route(new RegExp(`/api/host/v1/actions/${escape(ACTION_ID)}(\\?|$)`), (route: Route) => {
    if (route.request().method() !== "POST") return route.fallback()
    const body = route.request().postDataJSON() as { idempotencyKey: string; input: unknown }
    sent.push({ idempotencyKey: body.idempotencyKey, input: body.input })
    return calls.take(route)
  })
  return { reads, calls, sent }
}
