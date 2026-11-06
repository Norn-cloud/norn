import { cellCall, defaultFetch, humanize, type CellResult, type Diagnostic, type Fetch } from "./cell-read.ts"

/**
 * "Ask a person": the decisions a flow waits on, as RUNS serves them (decisions contract v1, on
 * `norn.decision@1`, parked commands PARKED-1 and the run routes).
 *
 *   GET  /v1/decisions?state=pending[&flow][&cursor][&limit=0..200]
 *        200 { items, total, totalCapped?, nextCursor, serverNowMs }: only open decisions this actor
 *        is a decider for (the cell decides; Studio filters nothing), soonest `expiresAtMs` first,
 *        ties to the oldest `requestedAtMs`. `limit=0` returns only the count.
 *   POST /v1/decisions/{id}:decide   { choice, note?, idempotencyKey }
 *        200 { outcome: "decided" | "replayed", decision, receipt, run }; the same key and body
 *        replays. Refusals, in this order: 404 decision.not_found (also a non-decider: the cell
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

export interface Decided {
  readonly outcome: "decided" | "replayed"
  readonly decision: { readonly id: string; readonly choice: string; readonly note?: string | null; readonly decidedBy: string; readonly decidedAtMs: number }
  readonly receipt: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
  readonly run: { readonly id: string; readonly state: string | null }
}

/** Who got there first (409 `decision.resolved`). */
export interface Resolution { readonly resolution: "decided" | "expired" | "withdrawn"; readonly by: string | null; readonly atMs: number }

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

export async function decide(id: string, body: { readonly choice: string; readonly note?: string }, idempotencyKey: string, fetcher: Fetch = defaultFetch): Promise<DecideResult> {
  const payload = { choice: body.choice, ...body.note ? { note: body.note } : {}, idempotencyKey }
  const result = await cellCall<Decided>(`/v1/decisions/${encodeURIComponent(id)}:decide`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(payload),
  }, fetcher)
  switch (result.kind) {
    case "ok": return { kind: "decided", decided: result.value }
    case "refused": {
      const { refusal } = result
      if (refusal.code === "decision.not_found") return { kind: "not-found", message: refusal.message }
      if (refusal.code === "decision.resolved") {
        const r = refusal.body["resolution"] as { resolution?: unknown; by?: unknown; atMs?: unknown } | undefined
        const kind = r?.resolution === "expired" || r?.resolution === "withdrawn" ? r.resolution : "decided"
        return { kind: "resolved", message: refusal.message, resolution: { resolution: kind, by: typeof r?.by === "string" ? r.by : null, atMs: typeof r?.atMs === "number" ? r.atMs : 0 } }
      }
      if (refusal.code === "decision.expired") return { kind: "expired", message: refusal.message }
      return { kind: "refused", status: refusal.status, code: refusal.code, message: refusal.message, diagnostics: refusal.diagnostics }
    }
    case "not-modified": return { kind: "unavailable" }
    default: return result
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

/** "2 d 4 h", "3 h 05 min", "12 min", "45 s", or null once the deadline has passed. */
export function remaining(expiresAtMs: number, cellNowMs: number): string | null {
  const ms = expiresAtMs - cellNowMs
  if (ms <= 0) return null
  const s = Math.floor(ms / 1000)
  const d = Math.floor(s / 86_400)
  const h = Math.floor((s % 86_400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return `${d} d ${h} h`
  if (h > 0) return `${h} h ${String(m).padStart(2, "0")} min`
  if (m > 0) return `${m} min`
  return `${s % 60} s`
}

/** Under an hour left: the countdown reads as urgent. */
export const urgent = (expiresAtMs: number, cellNowMs: number) => expiresAtMs - cellNowMs < 3_600_000
