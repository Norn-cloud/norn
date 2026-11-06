import type { Page, Route } from "@playwright/test"

/**
 * A stand-in for RUNS's decision and run routes (decisions contract v1, as RUNS approved it):
 * opaque `dec_…` ids, deadlines on the cell's clock, the refusal order 404 → 409 resolved → 410
 * expired → 403 → 422, pointers in `diagnostics[].span.pointer`, display names only, replay on the
 * same key and body, and runs whose steps move on as decisions land. It stands in until RUNS
 * deploys the routes; it is not client policy.
 */

const HOUR = 3_600_000
const START = Date.UTC(2026, 10, 6, 9, 0, 0)

interface Option { id: string; label: string; scope: string; effect: { action: string } | { binding: string } }
interface StoredDecision {
  id: string; runId: string; flow: { ruleId: string; version: number; title: string | null }; step: { id: string; label: string | null }
  question: string; risk: "low" | "medium" | "high"; options: Option[]; agentPick: string | null
  requestedAtMs: number; expiresAtMs: number
  /** A law this actor does not satisfy: deciding refuses 403 decision.law_not_satisfied. */
  lawRefuses?: boolean
  resolved?: { resolution: "decided" | "expired" | "withdrawn"; by: string | null; atMs: number }
}
interface Step { id: string; label: string | null; kind: "perform" | "decide" | "route" | "wait" | "succeed" | "fail"; status: "pending" | "running" | "waiting" | "done" | "failed" | "skipped"; startedAtMs: number | null; endedAtMs: number | null; receipts: { kind: string; receiptHash: string; commitSeq: number; recordedAtMs: number; executor?: string | null }[]; error: null | { code: string; message: string } }
interface StoredRun { id: string; generation: number; flow: { ruleId: string; version: number; title: string | null }; version: number; steps: Step[]; decision: { id: string; expiresAtMs: number } | null; createdAtMs: number; reads: number }

const hash = (n: number) => n.toString(16).padStart(64, "a")

export interface DecisionCell {
  /** Every decide call the inbox sent, with its body. */
  readonly calls: { readonly id: string; readonly body: string }[]
  /** Another decider decides first (the race). */
  resolveElsewhere(id: string, by: string): void
  /** Move the cell's clock on. */
  advance(ms: number): void
  /** The next call fails as an outage (500, no Norn code). */
  failNext(): void
  readonly runs: Map<string, StoredRun>
}

export function fixtures(now = START) {
  const decisions: StoredDecision[] = [
    {
      id: "dec_UmVsZWFzZS1nb2xk", runId: "run_restock_7", flow: { ruleId: "canon.restock.flow.rule", version: 3, title: "Restock gold" }, step: { id: "ask_manager", label: null },
      question: "Release the held shipment of 2.500 kg fine gold to Vault B?", risk: "high",
      options: [
        { id: "release", label: "Release it", scope: "This shipment only", effect: { action: "canon.shipment.release" } },
        { id: "hold", label: "Keep holding", scope: "Until the assay is back", effect: { binding: "shipment.hold" } },
      ],
      agentPick: "hold", requestedAtMs: now - 2 * HOUR, expiresAtMs: now + 45 * 60_000,
    },
    {
      id: "dec_QXBwcm92ZS1yZWZ1bmQ", runId: "run_refund_2", flow: { ruleId: "canon.refund_review.flow.rule", version: 1, title: null }, step: { id: "approve_refund", label: "Approve refund" },
      question: "Refund 1,240.00 USD to Al Noor Trading for the short delivery?", risk: "medium",
      options: [
        { id: "approve", label: "Approve", scope: "This refund", effect: { action: "canon.refund.issue" } },
        { id: "reject", label: "Reject", scope: "This refund", effect: { binding: "refund.rejected" } },
      ],
      agentPick: null, requestedAtMs: now - 26 * HOUR, expiresAtMs: now + 2 * 24 * HOUR, lawRefuses: true,
    },
    {
      id: "dec_UmVjb3VudC1iaW4", runId: "run_count_4", flow: { ruleId: "canon.cycle_count.flow.rule", version: 2, title: "Cycle count" }, step: { id: "recount", label: "Recount the bin?" },
      question: "The count for bin A-14 is off by 3 g. Recount it?", risk: "low",
      options: [
        { id: "recount", label: "Recount", scope: "Bin A-14", effect: { binding: "count.recount" } },
        { id: "accept", label: "Accept the count", scope: "Bin A-14", effect: { binding: "count.accept" } },
      ],
      agentPick: "recount", requestedAtMs: now - HOUR, expiresAtMs: now + 6 * 24 * HOUR,
    },
  ]
  const step = (id: string, kind: Step["kind"], status: Step["status"], label: string | null = null, extra: Partial<Step> = {}): Step =>
    ({ id, label, kind, status, startedAtMs: status === "pending" ? null : now - 2 * HOUR, endedAtMs: status === "done" ? now - 2 * HOUR + 4000 : null, receipts: [], error: null, ...extra })
  const runs = new Map<string, StoredRun>([
    ["run_restock_7", { id: "run_restock_7", generation: 1, flow: { ruleId: "canon.restock.flow.rule", version: 3, title: "Restock gold" }, version: 3, createdAtMs: now - 2 * HOUR, reads: 0, decision: { id: "dec_UmVsZWFzZS1nb2xk", expiresAtMs: now + 45 * 60_000 },
      steps: [
        step("check_stock", "perform", "done", "Check stock", { receipts: [{ kind: "function", receiptHash: hash(901), commitSeq: 901, recordedAtMs: now - 2 * HOUR + 3000, executor: "the cell" }] }),
        step("ask_manager", "decide", "waiting"),
        step("route_choice", "route", "pending", "Route on the choice"),
        step("release", "perform", "pending", "Release shipment"),
        step("done", "succeed", "pending", "Done"),
      ] }],
  ])
  return { decisions, runs }
}

export async function decisionCell(page: Page, options: { readonly empty?: boolean; readonly capped?: boolean } = {}): Promise<DecisionCell> {
  const { decisions: all, runs } = fixtures()
  const decisions = options.empty ? [] : all
  let now = START
  let seq = 1000
  let fail = false
  const calls: { id: string; body: string }[] = []
  const keys = new Map<string, { id: string; body: string; result: unknown }>()
  const refuse = (route: Route, status: number, code: string, message: string, extra: Record<string, unknown> = {}, pointer: string | null = null) =>
    route.fulfill({ status, json: { code, category: "decision", message, diagnostics: [{ code, message, span: pointer === null ? null : { pointer } }], ...extra } })
  const open = () => decisions.filter((d) => d.resolved === undefined && now < d.expiresAtMs)
    .sort((a, b) => a.expiresAtMs - b.expiresAtMs || a.requestedAtMs - b.requestedAtMs)
  const scoped = (url: URL) => url.searchParams.get("orgId") === "org_demo" && url.searchParams.get("environmentId") === "analytics"

  await page.route("**/api/host/v1/decisions**", async (route) => {
    const url = new URL(route.request().url())
    if (!scoped(url)) return refuse(route, 403, "scope.denied", "Not your workspace.")
    if (fail) { fail = false; return route.fulfill({ status: 500, body: "upstream reset" }) }
    if (route.request().method() === "GET") {
      const items = open()
      const limit = Number(url.searchParams.get("limit") ?? "50")
      const view = (d: StoredDecision) => ({
        id: d.id, run: { id: d.runId, generation: 1, state: `awaiting_perform_${d.step.id}` }, flow: d.flow, step: d.step,
        question: d.question, risk: d.risk, options: d.options, agentPick: d.agentPick,
        requestedAtMs: d.requestedAtMs, expiresAtMs: d.expiresAtMs, context: { subjects: [] }, inputSchema: null,
      })
      return route.fulfill({ json: { items: items.slice(0, limit).map(view), total: options.capped ? 500 : items.length, ...options.capped ? { totalCapped: true } : {}, nextCursor: null, serverNowMs: now } })
    }
    const id = decodeURIComponent(url.pathname.split("/v1/decisions/")[1] ?? "").replace(/:decide$/, "")
    const body = route.request().postData() ?? ""
    calls.push({ id, body })
    let parsed: { choice?: unknown; note?: unknown; idempotencyKey?: unknown }
    try { parsed = JSON.parse(body) } catch { return refuse(route, 422, "request.invalid", "The request is not JSON.") }
    const key = String(parsed.idempotencyKey ?? "")
    const { idempotencyKey: _, ...rest } = parsed
    const known = keys.get(key)
    if (known) {
      if (known.id !== id || known.body !== JSON.stringify(rest)) return refuse(route, 409, "decision.idempotency_conflict", "That key was already used for a different decision.")
      return route.fulfill({ json: { ...(known.result as object), outcome: "replayed" } })
    }
    const decision = decisions.find((d) => d.id === id)
    // Refusal order: 404, 409 resolved, 410 expired, 403, 422.
    if (decision === undefined) return refuse(route, 404, "decision.not_found", "There is no such decision for you.")
    if (decision.resolved) return refuse(route, 409, "decision.resolved", "Someone already resolved this decision.", { resolution: decision.resolved })
    if (now >= decision.expiresAtMs) return refuse(route, 410, "decision.expired", "This decision expired before it was decided.")
    if (decision.lawRefuses) return refuse(route, 403, "decision.law_not_satisfied", "Refunds over 1,000.00 USD need a finance manager's approval.", {}, null)
    const problems: { code: string; message: string; span: { pointer: string } }[] = []
    if (!decision.options.some((o) => o.id === parsed.choice)) problems.push({ code: "decision.unknown_choice", message: "Choose one of the options.", span: { pointer: "/choice" } })
    if (typeof parsed.note === "string" && [...parsed.note].length > 500) problems.push({ code: "decision.note_too_long", message: "A note is at most 500 characters.", span: { pointer: "/note" } })
    if (problems.length > 0) return route.fulfill({ status: 422, json: { code: problems[0]!.code, category: "decision", message: "The cell refused this decision.", diagnostics: problems } })
    seq += 1
    decision.resolved = { resolution: "decided", by: "you", atMs: now }
    const run = runs.get(decision.runId)
    if (run) {
      run.version += 1
      run.decision = null
      const at = (stepId: string) => run.steps.find((s) => s.id === stepId)
      const ask = at(decision.step.id)
      if (ask) Object.assign(ask, { status: "done", endedAtMs: now, receipts: [{ kind: "decision", receiptHash: hash(seq), commitSeq: seq, recordedAtMs: now, executor: "you" }] })
      // The run goes on: the next steps run while Studio reads it again, then it succeeds.
      for (const s of run.steps) if (s.status === "pending") Object.assign(s, { status: "running", startedAtMs: now })
    }
    const result = { outcome: "decided", decision: { id, choice: parsed.choice, ...parsed.note ? { note: parsed.note } : {}, decidedBy: "you", decidedAtMs: now }, receipt: { receiptHash: hash(seq), commitSeq: seq, recordedAtMs: now }, run: { id: decision.runId, state: "running" } }
    keys.set(key, { id, body: JSON.stringify(rest), result })
    return route.fulfill({ json: result })
  })

  await page.route("**/api/host/v1/runs**", async (route) => {
    const url = new URL(route.request().url())
    if (!scoped(url)) return refuse(route, 403, "scope.denied", "Not your workspace.")
    if (route.request().method() === "POST") {
      const { flow, runId } = JSON.parse(route.request().postData() ?? "{}") as { flow: string; runId: string }
      const existing = runs.get(runId)
      if (!existing) {
        runs.set(runId, { id: runId, generation: 1, flow: { ruleId: `${flow}.flow.rule`, version: 1, title: null }, version: 1, createdAtMs: now, reads: 0, decision: null,
          steps: [
            { id: "fetch_orders", label: null, kind: "perform", status: "running", startedAtMs: now, endedAtMs: null, receipts: [], error: null },
            { id: "post_totals", label: "Post totals", kind: "perform", status: "pending", startedAtMs: null, endedAtMs: null, receipts: [], error: null },
            { id: "done", label: "Done", kind: "succeed", status: "pending", startedAtMs: null, endedAtMs: null, receipts: [], error: null },
          ] })
      }
      return route.fulfill({ status: existing ? 200 : 201, json: document(runs.get(runId)!) })
    }
    const id = decodeURIComponent(url.pathname.split("/v1/runs/")[1] ?? "")
    const run = runs.get(id)
    if (!run) return refuse(route, 404, "run.not_found", "There is no such run you can read.")
    // A running run moves one step per read, so live refresh has something to show.
    run.reads += 1
    const active = run.steps.find((s) => s.status === "running")
    if (active && run.reads > 1) {
      seq += 1
      Object.assign(active, { status: "done", endedAtMs: now + run.reads * 1000, receipts: [{ kind: active.kind === "succeed" ? "commit" : "function", receiptHash: hash(seq), commitSeq: seq, recordedAtMs: now + run.reads * 1000, executor: "the cell" }] })
      const next = run.steps.find((s) => s.status === "pending")
      if (next) Object.assign(next, { status: "running", startedAtMs: now + run.reads * 1000 })
      run.version += 1
    }
    const etag = `"${run.generation}:${run.version}"`
    if (route.request().headers()["if-none-match"] === etag) return route.fulfill({ status: 304, headers: { etag } })
    return route.fulfill({ json: document(run), headers: { etag } })
  })

  function document(run: StoredRun) {
    const waiting = run.steps.find((s) => s.status === "waiting")
    return {
      id: run.id, generation: run.generation, flow: run.flow, objectType: "core.flow.Run", state: waiting ? `awaiting_perform_${waiting.id}` : run.steps.every((s) => s.status === "done") ? "succeeded" : "running",
      version: run.version, createdAtMs: run.createdAtMs, createdCommitSeq: 900, updatedAtMs: now, updatedCommitSeq: seq, steps: run.steps, decision: run.decision,
    }
  }

  return {
    calls, runs,
    resolveElsewhere: (id, by) => { const d = decisions.find((x) => x.id === id); if (d) d.resolved = { resolution: "decided", by, atMs: now } },
    advance: (ms) => { now += ms },
    failNext: () => { fail = true },
  }
}
