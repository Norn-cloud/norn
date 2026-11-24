import { cellCall, defaultFetch, humanize, type CellResult, type Diagnostic, type Fetch } from "./cell-read.ts"
import type { StudioScope } from "./definitions.ts"
import type { ActionSummary } from "../../../../packages/norn-sdk/src/index.ts"
import type { ParkedCall } from "./screen/call.ts"

/**
 * "Ask a person": the decisions a flow waits on, as RUNS serves them (decisions contract v1, on
 * `norn.decision@1`, parked commands PARKED-1 and the run routes).
 *
 *   GET  /v1/decisions?state=pending[&flow][&cursor][&limit=0..200]
 *        200 { items, total, totalCapped?, nextCursor, serverNowMs }: only open decisions this actor
 *        is a decider for (the cell decides; Studio filters nothing), soonest `expiresAtMs` first,
 *        ties to the oldest `requestedAtMs`. `limit=0` returns only the count.
 *   POST /v1/decisions/{id}:decide   { choice, note?, idempotencyKey }
 *        200 { outcome: "decided" | "replayed", decision, receipt, run, call? }; the same key and body
 *        replays. `call` (an approval of a parked action call) says where the call stands:
 *        "committed", "refused" (final, with `code`) or "not_executed" (approved, not yet executed:
 *        only the requester's own retry runs it, once). Refusals, in this order: 404 decision.not_found (also a non-decider: the cell
 *        does not say the decision exists), 409 decision.resolved (with `resolution`), 410
 *        decision.expired, 403 decision.not_a_decider | decision.law_not_satisfied, 422
 *        decision.unknown_choice (/choice) | decision.note_too_long (/note) | request.invalid,
 *        409 decision.idempotency_conflict. A 404 without a Norn code is a route not served.
 *
 * `id` is opaque (`dec_…`, bound to the run's generation). Names (`decidedBy`, `by`, a receipt's
 * `executor`) arrive as display words, never ids. @1 always sends `expiresAtMs` (7 days at most)
 * and `context.subjects: []`; `inputSchema` is reserved for @2.
 */

export type Risk = "low" | "medium" | "high"

export interface DecisionOption {
  readonly id: string
  readonly label: string
  readonly scope: string
  readonly effect: { readonly action: string } | { readonly binding: string }
}

export interface Decision {
  readonly id: string
  readonly run: { readonly id: string; readonly generation: number; readonly state: string | null }
  readonly flow: { readonly ruleId: string; readonly version: string | number; readonly title: string | null }
  readonly step: { readonly id: string; readonly label: string | null }
  readonly question: string
  readonly risk: Risk
  readonly options: readonly DecisionOption[]
  readonly agentPick: string | null
  readonly requestedAtMs: number
  readonly expiresAtMs: number
  readonly context: { readonly subjects: readonly { readonly objectType: string; readonly id: string; readonly label?: string | null }[] }
  readonly inputSchema: unknown
}

export interface DecisionsPage {
  readonly items: readonly Decision[]
  readonly total: number
  readonly totalCapped?: boolean
  readonly nextCursor: string | null
  readonly serverNowMs: number
}

/** Where an approved action call stands (APPROVAL-RECOVERY-1). */
export interface DecidedCall { readonly committed: boolean; readonly code?: string; readonly state: "committed" | "refused" | "not_executed" }

export interface Decided {
  readonly outcome: "decided" | "replayed"
  readonly decision: { readonly id: string; readonly choice: string; readonly note?: string | null; readonly decidedBy: string; readonly decidedAtMs: number }
  readonly receipt: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
  readonly run: { readonly id: string; readonly state: string | null }
  readonly call?: DecidedCall
  /**
   * PROVISIONAL CONTRACT (#1208, PR #1631 S2): the episode the decision projects to, when the cell
   * names it. Studio never derives it and only links to it when the cell advertises `learning.Episode`
   * (S1 capability gate), until the backend lane pins `episodeId` with route-produced fixtures.
   */
  readonly episodeId?: string | null
}

/**
 * Who got there first (409 `decision.resolved`). `choice` is the option they chose, sent only when
 * this reader may read the run; absent, Studio says who and when, not what.
 */
export interface Resolution { readonly resolution: "decided" | "expired" | "withdrawn"; readonly by: string | null; readonly atMs: number; readonly choice: string | null; readonly call?: DecidedCall }

export type DecideResult =
  | { readonly kind: "decided"; readonly decided: Decided }
  | { readonly kind: "resolved"; readonly resolution: Resolution; readonly message: string }
  | { readonly kind: "expired"; readonly message: string }
  /** The decision is gone, or this actor is not a decider for it (the cell does not say which). */
  | { readonly kind: "not-found"; readonly message: string }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly Diagnostic[] }
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }

export const DECISIONS_UNAVAILABLE = "This cell doesn't serve decisions yet."

export async function listDecisions(options: { readonly flow?: string; readonly cursor?: string; readonly limit?: number } = {}, fetcher: Fetch = defaultFetch): Promise<CellResult<DecisionsPage>> {
  const query = new URLSearchParams({ state: "pending" })
  if (options.flow !== undefined) query.set("flow", options.flow)
  if (options.cursor !== undefined) query.set("cursor", options.cursor)
  if (options.limit !== undefined) query.set("limit", String(options.limit))
  return cellCall<DecisionsPage>(`/v1/decisions?${query}`, {}, fetcher)
}

/** The count for the shell's badge: `limit=0`. */
export const countDecisions = (fetcher: Fetch = defaultFetch) => listDecisions({ limit: 0 }, fetcher)

export async function decide(id: string, body: { readonly choice: string; readonly note?: string }, idempotencyKey: string, fetcher: Fetch = defaultFetch, boundScope?: StudioScope | null): Promise<DecideResult> {
  const payload = { choice: body.choice, ...body.note ? { note: body.note } : {}, idempotencyKey }
  const result = await cellCall<Decided>(`/v1/decisions/${encodeURIComponent(id)}:decide`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(payload),
  }, fetcher, boundScope)
  switch (result.kind) {
    case "ok": return { kind: "decided", decided: result.value }
    case "refused": {
      const { refusal } = result
      if (refusal.code === "decision.not_found") return { kind: "not-found", message: refusal.message }
      if (refusal.code === "decision.resolved") {
        const r = refusal.body["resolution"] as { resolution?: unknown; by?: unknown; atMs?: unknown; choice?: unknown; call?: unknown } | undefined
        const kind = r?.resolution === "expired" || r?.resolution === "withdrawn" ? r.resolution : "decided"
        const call = decidedCall(r?.call)
        return { kind: "resolved", message: refusal.message, resolution: { resolution: kind, by: typeof r?.by === "string" ? r.by : null, atMs: typeof r?.atMs === "number" ? r.atMs : 0, choice: typeof r?.choice === "string" ? r.choice : null, ...call ? { call } : {} } }
      }
      if (refusal.code === "decision.expired") return { kind: "expired", message: refusal.message }
      return { kind: "refused", status: refusal.status, code: refusal.code, message: refusal.message, diagnostics: refusal.diagnostics }
    }
    case "not-modified": return { kind: "unavailable" }
    default: return result
  }
}

/** An approved call's state as the cell sent it; anything else is absent. */
function decidedCall(value: unknown): DecidedCall | undefined {
  if (typeof value !== "object" || value === null) return undefined
  const v = value as { committed?: unknown; code?: unknown; state?: unknown }
  if (typeof v.committed !== "boolean" || (v.state !== "committed" && v.state !== "refused" && v.state !== "not_executed")) return undefined
  return { committed: v.committed, state: v.state, ...typeof v.code === "string" ? { code: v.code } : {} }
}

// ---- The requester's own calls (APPROVAL-REQUESTED-1) ------------------------------------------

/**
 *   GET /v1/decisions:requested[?id]
 *        200 { items, totalCapped?, serverNowMs }: the caller's own direct approval-gated calls, never a
 *        decider's view. Without `id`, those pending a decider and those approved and not yet run; with
 *        `id`, that decision in any state, or no item when it is not the caller's. Each carries the call
 *        exactly as it waits (`encodedInput`, digit for digit, and `idempotencyKey`) and `canRun`: the
 *        cell's offer to run it now. Studio shows "Approved, run it" only when the cell sets `canRun`,
 *        and running it sends back exactly the served call; the cell decides that retry again.
 */
export type RequestedStatus = "pending" | "approved" | "rejected" | "expired" | "withdrawn"

export interface RequestedCall {
  readonly action: string
  readonly definition: string
  readonly input: unknown
  readonly encodedInput: string
  readonly idempotencyKey: string
  readonly state?: DecidedCall
}

export interface RequestedDecision {
  readonly id: string
  readonly question: string
  readonly risk: Risk
  readonly requestedAtMs: number
  readonly expiresAtMs: number
  readonly status: RequestedStatus
  readonly resolvedAtMs?: number
  readonly call: RequestedCall
  readonly canRun: boolean
}

export interface RequestedPage { readonly items: readonly RequestedDecision[]; readonly totalCapped?: boolean; readonly serverNowMs: number }

export const requestedPath = (id?: string) => `/v1/decisions:requested${id === undefined ? "" : `?${new URLSearchParams({ id })}`}`

/** One served requested call, or null: an item without the call's exact text and key is not offered. */
function requestedItem(value: unknown): RequestedDecision | null {
  if (typeof value !== "object" || value === null) return null
  const v = value as Partial<RequestedDecision>
  const call = v.call as Partial<RequestedCall> | undefined
  if (typeof v.id !== "string" || typeof v.status !== "string" || typeof v.canRun !== "boolean" || typeof v.expiresAtMs !== "number") return null
  if (typeof call !== "object" || call === null || typeof call.action !== "string" || typeof call.encodedInput !== "string" || typeof call.idempotencyKey !== "string") return null
  return v as RequestedDecision
}

export async function listRequested(id?: string, fetcher: Fetch = defaultFetch): Promise<CellResult<RequestedPage>> {
  const result = await cellCall<RequestedPage>(requestedPath(id), {}, fetcher)
  if (result.kind !== "ok") return result
  const items = Array.isArray(result.value.items) ? result.value.items.map(requestedItem).filter((item): item is RequestedDecision => item !== null) : []
  return { ...result, value: { ...result.value, items } }
}

/** The one requested call with this id, as the cell read it; null when the cell says it isn't the caller's. */
export const requestedOne = (page: RequestedPage, id: string): RequestedDecision | null => page.items.find((item) => item.id === id) ?? null

/** The call to send for "Approved, run it": only what the cell served, and only when it offers it. */
export function runnable(item: RequestedDecision): { readonly decisionId: string; readonly actionId: string; readonly input: string; readonly key: string } | null {
  if (!item.canRun || item.status !== "approved") return null
  return { decisionId: item.id, actionId: item.call.action, input: item.call.encodedInput, key: item.call.idempotencyKey }
}

/** The parked call a requested decision holds, as the cell serves it: what the action sheet opens on. */
export const parkedFrom = (item: RequestedDecision): ParkedCall =>
  ({ decision: { id: item.id, expiresAtMs: item.expiresAtMs }, input: item.call.encodedInput, key: item.call.idempotencyKey })

/**
 * The action a requested call names: the catalog's entry, or, when the catalog no longer offers it to
 * this reader (a role revoked since it parked), the call as the cell named it. Showing it grants
 * nothing: the cell decides the retry, and refuses one it no longer allows.
 */
export function requestedAction(catalog: readonly ActionSummary[], item: RequestedDecision): ActionSummary {
  return catalog.find((action) => action.actionId === item.call.action) ?? {
    actionId: item.call.action, title: humanize(item.call.action), description: null, objectKind: "", effect: "upsert_input_object", objectIdField: "id",
    inputSchema: { type: "object", properties: {} }, uiEffects: { irreversible: false, destructive: false, money: [] },
  }
}

/** Where a requested call stands, in the requester's words. */
export function requestedWords(item: RequestedDecision): string {
  switch (item.status) {
    case "pending": return "Waiting for a decider's approval."
    case "rejected": return "A decider rejected it. Nothing was changed."
    case "expired": return "No one decided in time, so it expired. Nothing was changed."
    case "withdrawn": return "It was withdrawn before anyone decided. Nothing was changed."
    case "approved": {
      const state = item.call.state?.state
      if (state === "committed") return "Approved and run."
      if (state === "refused") return `Approved, but the cell refused it when it ran${item.call.state?.code ? ` (${item.call.state.code})` : ""}. This approval never runs it again.`
      return item.canRun ? "Approved. Run it to make the change." : "Approved."
    }
  }
}

// ---- Reading a decision ------------------------------------------------------------------------

export const flowTitle = (d: Pick<Decision, "flow">) => d.flow.title ?? humanize(d.flow.ruleId.replace(/\.flow\.rule$/, ""))
export const stepTitle = (d: Pick<Decision, "step">) => d.step.label ?? humanize(d.step.id)

/** The badge's words: "500+" when the cell's count is a lower bound. */
export const badgeText = (page: Pick<DecisionsPage, "total" | "totalCapped">) => `${page.total}${page.totalCapped ? "+" : ""}`

/** Deciding this option asks for confirmation: a high-risk decision, or an option that starts an action. */
export const needsConfirm = (decision: Pick<Decision, "risk">, option: DecisionOption) => decision.risk === "high" || "action" in option.effect

/**
 * The cell's clock as seen from here: `serverNowMs` was true when the page arrived (`receivedAt`
 * on this machine), so "now" on the cell is that plus the time since. Countdowns never use the
 * browser's clock alone.
 */
export const cellNow = (serverNowMs: number, receivedAt: number, now = Date.now()) => serverNowMs + (now - receivedAt)

/**
 * What is left, in the two largest units, zero units dropped: "2 days", "1 day 4 h", "5 h 20 min",
 * "5 h", "45 min", "30 s"; null once the deadline has passed.
 */
export function remaining(expiresAtMs: number, cellNowMs: number): string | null {
  const ms = expiresAtMs - cellNowMs
  if (ms <= 0) return null
  const s = Math.floor(ms / 1000)
  const d = Math.floor(s / 86_400)
  const h = Math.floor((s % 86_400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return [`${d} ${d === 1 ? "day" : "days"}`, h > 0 ? `${h} h` : ""].filter(Boolean).join(" ")
  if (h > 0) return [`${h} h`, m > 0 ? `${m} min` : ""].filter(Boolean).join(" ")
  if (m > 0) return `${m} min`
  return `${s} s`
}

/** Under an hour left: the countdown reads as urgent. */
export const urgent = (expiresAtMs: number, cellNowMs: number) => expiresAtMs - cellNowMs < 3_600_000
