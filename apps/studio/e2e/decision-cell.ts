import { readFileSync } from "node:fs"
import type { Page, WebSocketRoute } from "@playwright/test"

/**
 * The DO dispatch pins these envelopes in tests/decisions.rs and tests/approvals.rs. The browser
 * scenarios vary the records, clock and progress inside those envelopes.
 */
const SERVED: typeof import("../test/fixtures/decision-run.json") = JSON.parse(
  readFileSync(new URL("../test/fixtures/decision-run.json", import.meta.url), "utf8"),
)
const EXECUTOR: typeof import("../test/fixtures/run-executor.json") = JSON.parse(
  readFileSync(new URL("../test/fixtures/run-executor.json", import.meta.url), "utf8"),
)
function first<T>(items: readonly T[], name: string): T {
  const [item] = items
  if (item === undefined) throw new Error(`Missing served ${name} fixture`)
  return item
}

const decisionExample = first(SERVED.page.json.items, "decision")
const optionExample = first(decisionExample.options, "decision option")
const waitingStep = first(SERVED.runRead.json.steps, "waiting step")
const executorStep = first(EXECUTOR.json.steps.filter((step) => step.waitingOn === "executor"), "executor step")
const doneStep = first(SERVED.runCompleted.json.steps, "completed step")
const waitingDecision = SERVED.runRead.json.decision
if (waitingDecision === null) throw new Error("Missing served run decision fixture")

const HOUR = 3_600_000
const START = Date.UTC(2026, 10, 6, 9, 0, 0)

interface Option { id: string; label: string; scope: string; effect: { action: string } | { binding: string } }
interface StoredDecision {
  id: string; runId: string; flow: { ruleId: string; version: string; title: string | null }; step: { id: string; label: string | null }
  question: string; risk: "low" | "medium" | "high"; options: Option[]; agentPick: string | null
  requestedAtMs: number; expiresAtMs: number
  /** A law this actor does not satisfy: deciding refuses 403 decision.law_not_satisfied. */
  lawRefuses?: boolean
  resolved?: { by: string; atMs: number; choice: string | null }
}
interface Step { id: string; label: string | null; kind: "perform" | "decide" | "wait" | "succeed" | "fail"; status: "pending" | "waiting" | "done" | "failed"; startedAtMs: number | null; endedAtMs: number | null }
interface StoredRun { id: string; readonly objectType: string; generation: number; flow: { ruleId: string; version: string; title: string | null }; version: number; steps: Step[]; decision: { id: string; expiresAtMs: number } | null; createdAtMs: number; reads: number }

const hash = (n: number) => n.toString(16).padStart(64, "a")

export interface DecisionCell {
  /** Every decide call the inbox sent, with its body. */
  readonly calls: { readonly id: string; readonly body: string }[]
  /** Another decider decides first (the race); with `choice`, the reader may read the run and is told what they chose. */
  resolveElsewhere(id: string, by: string, choice?: string): void
  /** Move the cell's clock on. */
  advance(ms: number): void
  /** The next call fails as an outage (500, no Norn code). */
  failNext(): void
  watchesRun(id: string): boolean
  /** Complete the run after the test has observed the waiting timeline. */
  finishRun(id: string): void
}

function fixtures(now = START) {
  const decisions: StoredDecision[] = [
    {
      id: "dec_UmVsZWFzZS1nb2xk", runId: "run_restock_7", flow: { ruleId: "canon.restock.flow.rule", version: "3.0.0", title: "Restock gold" }, step: { id: "ask_manager", label: null },
      question: "Release the held shipment of 2.500 kg fine gold to Vault B?", risk: "high",
      options: [
        { id: "release", label: "Release it", scope: "This shipment only", effect: { action: "canon.shipment.release" } },
        { id: "hold", label: "Keep holding", scope: "Until the assay is back", effect: { binding: "shipment.hold" } },
      ],
      agentPick: "hold", requestedAtMs: now - 2 * HOUR, expiresAtMs: now + 45 * 60_000,
    },
    {
      id: "dec_QXBwcm92ZS1yZWZ1bmQ", runId: "run_refund_2", flow: { ruleId: "canon.refund_review.flow.rule", version: "1.0.0", title: null }, step: { id: "approve_refund", label: "Approve refund" },
      question: "Refund 1,240.00 USD to Al Noor Trading for the short delivery?", risk: "medium",
      options: [
        { id: "approve", label: "Approve", scope: "This refund", effect: { action: "canon.refund.issue" } },
        { id: "reject", label: "Reject", scope: "This refund", effect: { binding: "refund.rejected" } },
      ],
      agentPick: null, requestedAtMs: now - 26 * HOUR, expiresAtMs: now + 2 * 24 * HOUR, lawRefuses: true,
    },
    {
      id: "dec_UmVjb3VudC1iaW4", runId: "run_count_4", flow: { ruleId: "canon.cycle_count.flow.rule", version: "2.0.0", title: "Cycle count" }, step: { id: "recount", label: "Recount the bin?" },
      question: "The count for bin A-14 is off by 3 g. Recount it?", risk: "low",
      options: [
        { id: "recount", label: "Recount", scope: "Bin A-14", effect: { binding: "count.recount" } },
        { id: "accept", label: "Accept the count", scope: "Bin A-14", effect: { binding: "count.accept" } },
      ],
      agentPick: "recount", requestedAtMs: now - HOUR, expiresAtMs: now + 6 * 24 * HOUR,
    },
  ]
  const step = (id: string, kind: Step["kind"], status: Step["status"], label: string | null = null): Step =>
    ({ id, label, kind, status, startedAtMs: status === "pending" ? null : now - 2 * HOUR, endedAtMs: status === "done" ? now - 2 * HOUR + 4000 : null })
  const runs = new Map<string, StoredRun>([
    ["run_count_3", { id: "run_count_3", objectType: SERVED.runCompleted.json.objectType, generation: 1, flow: { ruleId: "canon.cycle_count.flow.rule", version: "2.0.0", title: "Cycle count" }, version: 6, createdAtMs: now - 30 * HOUR, reads: 0, decision: null,
      steps: [step("count", "perform", "done", "Count the bin"), step("done", "succeed", "done", "Done")] }],
    ["run_restock_7", { id: "run_restock_7", objectType: SERVED.runRead.json.objectType, generation: 1, flow: { ruleId: "canon.restock.flow.rule", version: "3.0.0", title: "Restock gold" }, version: 3, createdAtMs: now - 2 * HOUR, reads: 0, decision: { id: "dec_UmVsZWFzZS1nb2xk", expiresAtMs: now + 45 * 60_000 },
      steps: [
        step("check_stock", "perform", "done", "Check stock"),
        step("ask_manager", "decide", "waiting"),
        step("release", "perform", "pending", "Release shipment"),
        step("done", "succeed", "pending", "Done"),
      ] }],
  ])
  return { decisions, runs }
}

/** A decision whose server content is Arabic, for the right-to-left path end to end. */
const ARABIC: StoredDecision = {
  id: "dec_QXJhYmljLXJlY291bnQ", runId: "run_count_4", flow: { ruleId: "canon.cycle_count.flow.rule", version: "2.0.0", title: "جرد دوري" }, step: { id: "recount", label: "إعادة عدّ الصندوق؟" },
  question: "عدد الصندوق A-14 مختلف بمقدار 3 غرامات. هل نعيد العدّ؟", risk: "low",
  options: [
    { id: "recount", label: "أعد العدّ", scope: "الصندوق A-14", effect: { binding: "count.recount" } },
    { id: "accept", label: "اقبل العدد", scope: "الصندوق A-14", effect: { binding: "count.accept" } },
  ],
  agentPick: "recount", requestedAtMs: START - HOUR, expiresAtMs: START + 6 * 24 * HOUR,
}

function answerBody(raw: string): { choice: string; note?: string | null; idempotencyKey: string } | null {
  let body: unknown
  try { body = JSON.parse(raw) } catch { return null }
  if (typeof body !== "object" || body === null ||
      !("choice" in body) || typeof body.choice !== "string" ||
      !("idempotencyKey" in body) || typeof body.idempotencyKey !== "string") return null
  const note = "note" in body ? body.note : undefined
  if (note !== undefined && note !== null && typeof note !== "string") return null
  return { choice: body.choice, note, idempotencyKey: body.idempotencyKey }
}

export async function decisionCell(page: Page, options: { readonly empty?: boolean; readonly capped?: boolean; readonly legacy?: boolean; readonly arabic?: boolean } = {}): Promise<DecisionCell> {
  const { decisions: all, runs } = fixtures()
  const decisions = options.empty ? [] : options.arabic ? [...all, ARABIC] : all
  let now = START
  let seq = 1000
  let fail = false
  const calls: { id: string; body: string }[] = []
  const answers = new Map<string, { key: string; body: string; result: object }>()
  const open = () => decisions.filter((d) => d.resolved === undefined && now < d.expiresAtMs)
    .sort((a, b) => a.expiresAtMs - b.expiresAtMs || a.requestedAtMs - b.requestedAtMs)
  const scoped = (url: URL) => url.searchParams.get("orgId") === "org_demo" && url.searchParams.get("environmentId") === "analytics"

  await page.route("**/api/host/v1/decisions**", async (route) => {
    const url = new URL(route.request().url())
    if (!scoped(url)) return route.fulfill(SERVED.decisionDenied)
    if (fail) { fail = false; return route.fulfill({ status: 500, body: "upstream reset" }) }
    // This actor made no approval-gated call (APPROVAL-REQUESTED-1).
    if (url.pathname.endsWith("/v1/decisions:requested")) return route.fulfill({ ...SERVED.requested, json: { ...SERVED.requested.json, serverNowMs: now } })
    if (route.request().method() === "GET") {
      const items = open()
      const limit = Number(url.searchParams.get("limit") ?? "50")
      const view = (d: StoredDecision) => ({
        ...decisionExample,
        id: d.id, run: { ...decisionExample.run, id: d.runId, state: `awaiting_perform_${d.step.id}` }, flow: { ...decisionExample.flow, ...d.flow }, step: { ...decisionExample.step, ...d.step },
        question: d.question, risk: d.risk, options: d.options.map((option) => ({ ...optionExample, ...option })), agentPick: d.agentPick,
        requestedAtMs: d.requestedAtMs, expiresAtMs: d.expiresAtMs,
      })
      return route.fulfill({ ...SERVED.page, json: { ...SERVED.page.json, items: items.slice(0, limit).map(view), total: options.capped ? 500 : items.length, ...options.capped ? { totalCapped: true } : {}, serverNowMs: now } })
    }
    const id = decodeURIComponent(url.pathname.split("/v1/decisions/")[1] ?? "").replace(/:decide$/, "")
    const body = route.request().postData() ?? ""
    calls.push({ id, body })
    const parsed = answerBody(body)
    if (parsed === null) return route.fulfill(SERVED.invalidBody)
    const { idempotencyKey: key, choice, note } = parsed
    const answer = JSON.stringify({ choice, note: note ?? null })
    const known = answers.get(id)
    if (known?.key === key) {
      if (known.body !== answer) return route.fulfill(SERVED.conflict)
      return route.fulfill({ ...SERVED.replayed, json: { ...known.result, outcome: SERVED.replayed.json.outcome } })
    }
    const decision = decisions.find((d) => d.id === id)
    if (decision === undefined) return route.fulfill(SERVED.decisionMissing)
    if (decision.resolved) {
      const { status, json } = SERVED.resolved
      return route.fulfill({ status, json: { ...json, resolution: { ...json.resolution, ...decision.resolved } } })
    }
    if (now >= decision.expiresAtMs) return route.fulfill(SERVED.expired)
    if (decision.lawRefuses) return route.fulfill(SERVED.law)
    if (!decision.options.some((o) => o.id === parsed.choice)) return route.fulfill(SERVED.unknownChoice)
    if (typeof parsed.note === "string" && [...parsed.note].length > 500) return route.fulfill(SERVED.noteTooLong)
    if (/^\p{White_Space}*$/u.test(key) || Buffer.byteLength(key) > 128) return route.fulfill(SERVED.invalidKey)
    seq += 1
    decision.resolved = { by: SERVED.decided.json.decision.decidedBy, atMs: now, choice: parsed.choice }
    const run = runs.get(decision.runId)
    if (run) {
      run.version += 1
      run.decision = null
      const ask = run.steps.find((step) => step.id === decision.step.id)
      if (ask) Object.assign(ask, { status: "done", endedAtMs: now })
      const next = run.steps.find((step) => step.status === "pending")
      if (next) Object.assign(next, { status: "waiting", startedAtMs: now })
    }
    push()
    const served = SERVED.decided.json
    const result = {
      ...served,
      decision: { ...served.decision, id, choice: parsed.choice, note: parsed.note ?? null, decidedAtMs: now },
      receipt: { ...served.receipt, receiptHash: hash(seq), commitSeq: seq, recordedAtMs: now },
      run: { ...served.run, id: decision.runId, state: run ? stateOf(run) : served.run.state },
    }
    answers.set(id, { key, body: answer, result })
    return route.fulfill({ ...SERVED.decided, json: result })
  })

  await page.route("**/api/host/v1/runs**", async (route) => {
    const url = new URL(route.request().url())
    if (route.request().method() === "POST") {
      if (!scoped(url)) return route.fulfill(SERVED.runStartDenied)
      const { flow, runId } = JSON.parse(route.request().postData() ?? "{}") as { flow: string; runId: string }
      const existing = runs.get(runId)
      const run: StoredRun = existing ?? { id: runId, objectType: SERVED.runStarted.json.objectType, generation: 1, flow: { ruleId: `${flow}.flow.rule`, version: "1.0.0", title: null }, version: 1, createdAtMs: now, reads: 0, decision: null,
          steps: [
            { id: "fetch_orders", label: null, kind: "perform", status: "waiting", startedAtMs: now, endedAtMs: null },
            { id: "post_totals", label: "Post totals", kind: "perform", status: "pending", startedAtMs: null, endedAtMs: null },
            { id: "done", label: "Done", kind: "succeed", status: "pending", startedAtMs: null, endedAtMs: null },
          ],
      }
      runs.set(runId, run)
      const served = existing ? SERVED.runReplayed : SERVED.runStarted
      return route.fulfill({ ...served, json: document(run) })
    }
    if (!scoped(url)) return route.fulfill(SERVED.runDenied)
    const id = decodeURIComponent(url.pathname.split("/v1/runs/")[1] ?? "")
    const run = runs.get(id)
    if (!run) return route.fulfill(SERVED.runMissing)
    run.reads += 1
    if (options.legacy) {
      // Compatibility case for a cell that serves the run without its timeline.
      const { steps: _steps, decision: _decision, ...first } = document(run)
      return route.fulfill({ ...SERVED.runRead, json: first })
    }
    const served = run.decision === null && run.steps.some((step) => step.status === "waiting") ? EXECUTOR : SERVED.runRead
    return route.fulfill({ ...served, json: document(run) })
  })

  // Live reads (UI-LIVE-1): the stand-in's own socket. A subscription gets the route's current
  // version at once and again whenever a commit here changes it; nothing is sent otherwise.
  const subs = new Map<string, { ws: WebSocketRoute; path: string; digest: string }>()
  const versionOf = (path: string): string => {
    const url = new URL(path, "https://cell")
    if (url.pathname.startsWith("/v1/runs/")) {
      const run = runs.get(decodeURIComponent(url.pathname.slice("/v1/runs/".length)))
      return run ? `${run.version}:${run.steps.map((s) => s.status).join(",")}` : "404"
    }
    if (url.pathname === "/v1/decisions") return open().map((d) => d.id).join(",")
    return "404"
  }
  const push = () => {
    for (const [id, sub] of subs) {
      const digest = versionOf(sub.path)
      if (digest === sub.digest) continue
      sub.digest = digest
      sub.ws.send(JSON.stringify({ type: "snapshot", id, status: digest === "404" ? 404 : 200, digest, body: null }))
    }
  }
  await page.routeWebSocket("**/api/host/v1/subscribe**", (ws) => {
    ws.onMessage((raw) => {
      const message = JSON.parse(String(raw)) as { type: string; id: string; path: string }
      if (message.type === "unsubscribe") { subs.delete(message.id); return }
      subs.set(message.id, { ws, path: message.path, digest: "" })
      push()
    })
    ws.onClose(() => { for (const [id, sub] of subs) if (sub.ws === ws) subs.delete(id) })
  })
  function stateOf(run: StoredRun) {
    const waiting = run.steps.find((step) => step.status === "waiting")
    if (waiting === undefined) return SERVED.runCompleted.json.state
    return waiting.kind === "decide"
      ? SERVED.runRead.json.state.replace(waitingStep.id, waiting.id)
      : EXECUTOR.json.state.replace(executorStep.id, waiting.id)
  }

  function document(run: StoredRun) {
    const terminal = run.steps.every((step) => step.status === "done" || step.status === "failed")
    const template = terminal ? SERVED.runCompleted.json : run.decision === null ? EXECUTOR.json : SERVED.runRead.json
    return {
      ...template,
      id: run.id, objectType: run.objectType, generation: run.generation, flow: { ...template.flow, ...run.flow, id: run.flow.ruleId.slice(0, -".flow.rule".length) },
      state: stateOf(run), terminal, version: run.version, createdAtMs: run.createdAtMs, updatedAtMs: now, updatedCommitSeq: seq,
      steps: run.steps.filter((step) => step.status !== "pending").map((step) => {
        if (step.status !== "waiting") return { ...doneStep, ...step }
        const waiting = step.kind === "decide" ? waitingStep : executorStep
        return { ...waiting, id: step.id, label: step.label, startedAtMs: step.startedAtMs }
      }),
      decision: run.decision === null ? null : { ...waitingDecision, ...run.decision },
    }
  }

  return {
    calls,
    resolveElsewhere: (id, by, choice) => { const d = decisions.find((x) => x.id === id); if (d) d.resolved = { by, atMs: now, choice: choice ?? null }; push() },
    advance: (ms) => { now += ms },
    failNext: () => { fail = true },
    watchesRun: (id) => [...subs.values()].some(({ path }) => path.startsWith(`/v1/runs/${encodeURIComponent(id)}?`)),
    finishRun: (id) => {
      const run = runs.get(id)
      if (run === undefined) throw new Error(`No run to finish: ${id}`)
      for (const step of run.steps) {
        if (step.status === "done") continue
        step.status = "done"
        step.startedAtMs ??= now
        step.endedAtMs = now
      }
      run.decision = null
      run.version += 1
      seq += 1
      push()
    },
  }
}
