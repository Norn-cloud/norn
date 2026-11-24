import type { Page, Route } from "@playwright/test"
import type { ActionSummary } from "../src/lib/actions.ts"
import type { AutomationDefinition, AutomationPreviewMatch, AutomationRunRecord } from "../src/lib/automations.ts"
import type { Decision } from "../src/lib/decisions.ts"
import type { Clause, Predicate } from "../src/lib/records-query.ts"

/**
 * Deterministic two-domain stand-in for CELL-DO's view, object, action, automation and inbox
 * routes so Playwright exercises Studio's complete Workspace Views and Builder Automations flows
 * across two unrelated domains:
 *   1. Application Funnel (`funnel.Application`) — issue #1424's exact scenario
 *   2. Equipment Fleet Tickets (`fleet.Ticket`) — unrelated second domain proving genericity
 */

export interface DemoRecord {
  readonly objectType: string
  readonly id: string
  version: number
  values: Record<string, string | number | boolean>
}

interface StoredDef {
  readonly kind: "view" | "automation"
  version: number
  payload: Record<string, unknown>
  retired: boolean
  updatedAtMs: number
}

const SCOPE = { orgId: "org_demo", environmentId: "prod" }
const BASE_NOW = 1_800_000_000_000

const ARTIFACTS = [
  {
    artifactId: "funnel.Application.entity",
    kind: "entity",
    version: "1.0.0",
    name: "Application",
    namespace: "funnel",
    payload: {
      entityVersion: "2.0",
      objectType: {
        name: "funnel.Application",
        properties: [
          { id: "id", type: { kind: "text" } },
          { id: "applicant_name", type: { kind: "text" } },
          {
            id: "stage",
            type: {
              kind: "enum",
              values: [
                { value: "started" },
                { value: "in_review" },
                { value: "stalled" },
                { value: "assisted" },
                { value: "completed" },
              ],
            },
          },
          { id: "score", type: { kind: "integer" } },
          { id: "channel", type: { kind: "text" } },
        ],
        lifecycles: [
          {
            property: "stage",
            initial: ["started"],
            terminal: ["completed"],
            transitions: [
              { id: "mark_stalled", from: ["started", "in_review"], to: "stalled" },
              { id: "offer_assistance", from: ["in_review", "stalled"], to: "assisted" },
              { id: "complete", from: ["in_review", "assisted"], to: "completed" },
            ],
          },
        ],
      },
    },
  },
  {
    artifactId: "fleet.Ticket.entity",
    kind: "entity",
    version: "1.0.0",
    name: "Fleet ticket",
    namespace: "fleet",
    payload: {
      entityVersion: "2.0",
      objectType: {
        name: "fleet.Ticket",
        properties: [
          { id: "id", type: { kind: "text" } },
          { id: "asset_code", type: { kind: "text" } },
          {
            id: "status",
            type: {
              kind: "enum",
              values: [
                { value: "open" },
                { value: "waiting_parts" },
                { value: "escalated" },
                { value: "closed" },
              ],
            },
          },
          { id: "downtime_hours", type: { kind: "integer" } },
        ],
        lifecycles: [
          {
            property: "status",
            initial: ["open"],
            terminal: ["closed"],
            transitions: [
              { id: "wait_parts", from: ["open"], to: "waiting_parts" },
              { id: "escalate", from: ["open", "waiting_parts"], to: "escalated" },
            ],
          },
        ],
      },
    },
  },
]

const ACTIONS: readonly ActionSummary[] = [
  {
    actionId: "funnel.application.offer_assistance",
    title: "Offer assistance",
    description: "Assign concierge assistance to a stalled or in-review application.",
    objectKind: "funnel.Application",
    effect: "fire_transition",
    objectIdField: "id",
    toState: "assisted",
    uiEffects: { irreversible: false, destructive: false, money: [] },
    inputSchema: {
      type: "object",
      required: ["id"],
      additionalProperties: false,
      properties: {
        id: { type: "string", title: "Application ID" },
        note: { type: "string", title: "Outreach note" },
      },
    },
  },
  {
    actionId: "funnel.application.mark_stalled",
    title: "Mark stalled",
    description: "Flag an application as stalled.",
    objectKind: "funnel.Application",
    effect: "fire_transition",
    objectIdField: "id",
    toState: "stalled",
    uiEffects: { irreversible: false, destructive: false, money: [] },
    inputSchema: {
      type: "object",
      required: ["id"],
      additionalProperties: false,
      properties: {
        id: { type: "string", title: "Application ID" },
      },
    },
  },
  {
    actionId: "fleet.ticket.escalate",
    title: "Escalate ticket",
    description: "Escalate an equipment fleet ticket to regional dispatch.",
    objectKind: "fleet.Ticket",
    effect: "fire_transition",
    objectIdField: "id",
    toState: "escalated",
    uiEffects: { irreversible: false, destructive: false, money: [] },
    inputSchema: {
      type: "object",
      required: ["id"],
      additionalProperties: false,
      properties: {
        id: { type: "string", title: "Ticket ID" },
      },
    },
  },
]

function initialRecords(): DemoRecord[] {
  return [
    { objectType: "funnel.Application", id: "app-1", version: 2, values: { id: "app-1", applicant_name: "Ada Reyes", stage: "stalled", score: 42, channel: "web" } },
    { objectType: "funnel.Application", id: "app-2", version: 2, values: { id: "app-2", applicant_name: "Ben Park", stage: "stalled", score: 35, channel: "partner" } },
    { objectType: "funnel.Application", id: "app-3", version: 2, values: { id: "app-3", applicant_name: "Cid Patel", stage: "stalled", score: 88, channel: "web" } },
    { objectType: "funnel.Application", id: "app-4", version: 1, values: { id: "app-4", applicant_name: "Dia Chen", stage: "in_review", score: 51, channel: "web" } },
    { objectType: "funnel.Application", id: "app-5", version: 3, values: { id: "app-5", applicant_name: "Eli Gomez", stage: "assisted", score: 49, channel: "partner" } },
    { objectType: "fleet.Ticket", id: "tkt-1", version: 2, values: { id: "tkt-1", asset_code: "CRANE-07", status: "waiting_parts", downtime_hours: 36 } },
    { objectType: "fleet.Ticket", id: "tkt-2", version: 2, values: { id: "tkt-2", asset_code: "PUMP-12", status: "waiting_parts", downtime_hours: 8 } },
    { objectType: "fleet.Ticket", id: "tkt-3", version: 3, values: { id: "tkt-3", asset_code: "LOADER-04", status: "escalated", downtime_hours: 48 } },
  ]
}

function offersForRecord(record: DemoRecord): string[] {
  if (record.objectType === "funnel.Application") {
    const stage = String(record.values.stage ?? "")
    const out: string[] = []
    if (stage === "started" || stage === "in_review") out.push("funnel.application.mark_stalled")
    if (stage === "in_review" || stage === "stalled") out.push("funnel.application.offer_assistance")
    return out
  }
  if (record.objectType === "fleet.Ticket") {
    const status = String(record.values.status ?? "")
    return status === "open" || status === "waiting_parts" ? ["fleet.ticket.escalate"] : []
  }
  return []
}

function matchClause(record: DemoRecord, clause: Clause): boolean {
  const raw = record.values[clause.property]
  if (clause.op === "empty") return raw === undefined || raw === null || raw === ""
  if (clause.op === "present") return raw !== undefined && raw !== null && raw !== ""
  if (raw === undefined || raw === null || !("value" in clause)) return false
  const val = clause.value
  const cmp = typeof raw === "number"
    ? raw - Number(val)
    : String(raw).localeCompare(String(val))
  switch (clause.op) {
    case "eq": return cmp === 0
    case "ne": return cmp !== 0
    case "lt": return cmp < 0
    case "lte": return cmp <= 0
    case "gt": return cmp > 0
    case "gte": return cmp >= 0
    case "in": return Array.isArray(val) && val.some((v) => String(v) === String(raw))
    case "contains": return String(raw).includes(String(val))
    case "prefix": return String(raw).startsWith(String(val))
    case "between": {
      const [lo, hi] = clause.value
      return typeof raw === "number" ? raw >= Number(lo) && raw <= Number(hi) : String(raw) >= lo && String(raw) <= hi
    }
    default: return false
  }
}

function evalGuardExpr(expr: string | undefined, record: DemoRecord): { ok: true; matched: boolean } | { ok: false; message: string } {
  const text = expr?.trim() ?? ""
  if (text === "") return { ok: true, matched: true }
  const segments = text.split(/\s+and\s+/)
  for (const segment of segments) {
    const m = /^self\.([A-Za-z_][A-Za-z0-9_]*)\s*(==|!=|<=|>=|<|>)\s*(.+)$/.exec(segment.trim())
    if (!m) return { ok: false, message: `guard expression "${text}" must return a boolean` }
    const prop = m[1]!
    const op = m[2]!
    const rhsRaw = m[3]!.trim()
    const lhs = record.values[prop]
    if (lhs === undefined) return { ok: true, matched: false }
    const rhs = rhsRaw.startsWith("'") && rhsRaw.endsWith("'")
      ? rhsRaw.slice(1, -1)
      : rhsRaw === "true"
        ? true
        : rhsRaw === "false"
          ? false
          : Number(rhsRaw)
    const cmp = typeof lhs === "number" && typeof rhs === "number" ? lhs - rhs : String(lhs).localeCompare(String(rhs))
    const pass = op === "==" ? cmp === 0 : op === "!=" ? cmp !== 0 : op === "<" ? cmp < 0 : op === "<=" ? cmp <= 0 : op === ">" ? cmp > 0 : cmp >= 0
    if (!pass) return { ok: true, matched: false }
  }
  return { ok: true, matched: true }
}

function validateGuardSyntax(expr: string | undefined): string | null {
  const text = expr?.trim() ?? ""
  if (text === "") return null
  for (const segment of text.split(/\s+and\s+/)) {
    if (!/^self\.([A-Za-z_][A-Za-z0-9_]*)\s*(==|!=|<=|>=|<|>)\s*('[^']*'|-?\d+(\.\d+)?|true|false)$/.test(segment.trim())) {
      return `guard expression must be a boolean comparison (got "${text}")`
    }
  }
  return null
}

function matchesAutomationWhen(def: AutomationDefinition, record: DemoRecord): string | null {
  if (def.when.kind === "lifecycle_entered") {
    return String(record.values[def.when.property] ?? "") === def.when.state
      ? `${def.when.property} is ${def.when.state}`
      : null
  }
  if (def.when.kind === "stale") {
    if (def.when.property && def.when.state && String(record.values[def.when.property] ?? "") !== def.when.state) return null
    return `unchanged past threshold (${Math.round(def.when.afterMs / 60_000)}m)`
  }
  return `record matches ${def.when.event} trigger`
}

export interface WorkspaceCell {
  readonly records: DemoRecord[]
  readonly defs: Map<string, StoredDef>
  readonly decisions: Map<string, { decision: Decision; automationId: string; objectType: string; objectId: string; objectVersion: number; actionId: string; resolved: boolean }>
  readonly runs: AutomationRunRecord[]
}

export async function workspaceCell(page: Page): Promise<WorkspaceCell> {
  const records = initialRecords()
  const defs = new Map<string, StoredDef>()
  const decisions = new Map<string, { decision: Decision; automationId: string; objectType: string; objectId: string; objectVersion: number; actionId: string; resolved: boolean }>()
  const runs: AutomationRunRecord[] = []
  let commitSeq = 10
  let seqId = 1

  const runActionOnRecord = (actionId: string, input: Record<string, unknown>) => {
    const id = String(input.id ?? "")
    const action = ACTIONS.find((a) => a.actionId === actionId)
    const rec = records.find((r) => r.objectType === action?.objectKind && r.id === id)
    if (!action || !rec) return { ok: false as const, status: 403, code: "authz.denied", message: "Action not permitted." }
    if (!offersForRecord(rec).includes(actionId)) {
      return { ok: false as const, status: 409, code: "action.precondition_failed", message: `${action.title} is not offered in the current state.` }
    }
    if (actionId === "funnel.application.offer_assistance" && Number(rec.values.score ?? 0) >= 80) {
      return {
        ok: false as const,
        status: 422,
        code: "law.violated",
        message: "High-scoring applications (>= 80) do not qualify for assisted triage.",
      }
    }
    if (actionId === "funnel.application.offer_assistance") rec.values.stage = "assisted"
    else if (actionId === "funnel.application.mark_stalled") rec.values.stage = "stalled"
    else if (actionId === "fleet.ticket.escalate") rec.values.status = "escalated"
    rec.version += 1
    commitSeq += 1
    return {
      ok: true as const,
      receipt: {
        outcome: "committed",
        objectType: rec.objectType,
        objectId: rec.id,
        version: rec.version,
        receipt: { receiptHash: `h_${commitSeq}`, commitSeq, recordedAtMs: BASE_NOW + commitSeq * 1000 },
      },
    }
  }

  await page.route("**/api/me", (route) => route.fulfill({
    json: {
      user: { id: "u_ops", name: "Operator Admin", email: "ops@example.com" },
      organization: { id: SCOPE.orgId, name: "Workspace Demo", role: "admin" },
      environmentId: SCOPE.environmentId,
    },
  }))

  await page.route("**/api/host/v1/definitions**", async (route: Route) => {
    const url = new URL(route.request().url())
    const body = route.request().postData() ? JSON.parse(route.request().postData()!) as Record<string, unknown> : null
    const tail = url.pathname.split("/v1/definitions")[1] ?? ""

    if (tail === "") return route.fulfill({ json: { version: 1, artifacts: ARTIFACTS } })

    if (tail === ":list") {
      const kind = url.searchParams.get("kind")
      const items = [...defs.entries()]
        .filter(([, d]) => !d.retired && (!kind || d.kind === kind))
        .map(([id, d]) => ({ id, kind: d.kind, title: String(d.payload.title ?? ""), version: d.version, lifecycle: "active", updatedAtMs: d.updatedAtMs }))
      return route.fulfill({ json: { items, cursor: null } })
    }

    if (tail === ":admit") {
      const kind = String(body?.kind ?? "")
      const payload = (body?.payload ?? {}) as Record<string, unknown>
      const diagnostics: { code: string; message: string; span: { pointer: string } }[] = []
      if (!String(payload.title ?? "").trim()) {
        diagnostics.push({ code: "definition.title_invalid", message: "Title is required.", span: { pointer: "/title" } })
      }
      if (kind === "automation") {
        const guardErr = validateGuardSyntax((payload.if as { expression?: string } | undefined)?.expression)
        if (guardErr) diagnostics.push({ code: "definition.guard_invalid", message: guardErr, span: { pointer: "/if/expression" } })
      }
      if (diagnostics.length > 0) {
        return route.fulfill({ json: { admitted: false, diagnostics, restatement: null, restatementSha256: null } })
      }
      const restatement = kind === "view"
        ? `Saved view “${String(payload.title)}” over ${String(payload.objectType)}${payload.space ? ` in Space ${String(payload.space)}` : ""}.`
        : `Automation “${String(payload.title)}” on ${String(payload.objectType)}.`
      return route.fulfill({ json: { admitted: true, diagnostics: [], restatement, restatementSha256: "a".repeat(64) } })
    }

    if (tail === ":install") {
      const kind = String(body?.kind ?? "") as "view" | "automation"
      const id = String(body?.id ?? "")
      const payload = (body?.payload ?? {}) as Record<string, unknown>
      const expected = typeof body?.expectedVersion === "number" ? body.expectedVersion : undefined
      if (kind === "automation") {
        const guardErr = validateGuardSyntax((payload.if as { expression?: string } | undefined)?.expression)
        if (guardErr) {
          return route.fulfill({
            status: 422,
            json: { code: "definition.refused", diagnostics: [{ code: "definition.guard_invalid", message: guardErr, span: { pointer: "/if/expression" } }] },
          })
        }
      }
      const existing = defs.get(id)
      if (expected !== undefined && existing?.version !== expected) {
        return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Version conflict." } })
      }
      const version = (existing?.version ?? 0) + 1
      defs.set(id, { kind, version, payload, retired: false, updatedAtMs: Date.now() })
      commitSeq += 1
      return route.fulfill({ json: { installed: true, id, version, receipt: { receiptHash: `def_${commitSeq}`, commitSeq, recordedAtMs: Date.now() } } })
    }

    const retire = /^\/(.+):retire$/.exec(tail)
    if (retire) {
      const id = decodeURIComponent(retire[1]!)
      const existing = defs.get(id)
      if (!existing || existing.retired || existing.version !== body?.expectedVersion) {
        return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Version conflict." } })
      }
      existing.retired = true
      existing.version += 1
      return route.fulfill({ json: { installed: true, id, version: existing.version, receipt: { receiptHash: `ret_${existing.version}`, commitSeq: ++commitSeq, recordedAtMs: Date.now() } } })
    }

    const id = decodeURIComponent(tail.slice(1))
    const existing = defs.get(id)
    if (!existing || existing.retired) return route.fulfill({ status: 404, json: { code: "definition.not_found", message: "Not found." } })
    return route.fulfill({ json: { id, kind: existing.kind, version: existing.version, payload: existing.payload, updatedAtMs: existing.updatedAtMs } })
  })

  await page.route("**/api/host/v1/objects/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const rest = url.pathname.split("/v1/objects/")[1] ?? ""
    if (rest.endsWith(":facets")) {
      const objectType = decodeURIComponent(rest.slice(0, -":facets".length))
      const rows = records.filter((r) => r.objectType === objectType)
      return route.fulfill({ json: { objectType, rows: rows.length, withheldRows: 0, quarantinedRows: 0, lowerBound: false, scanLimit: 10000, facets: [] } })
    }
    const parts = rest.split("/")
    const objectType = decodeURIComponent(parts[0]!)
    if (parts.length === 2) {
      const rec = records.find((r) => r.objectType === objectType && r.id === decodeURIComponent(parts[1]!))
      if (!rec) return route.fulfill({ status: 404, json: { code: "object.not_found" } })
      return route.fulfill({ json: { objectType: rec.objectType, id: rec.id, scope: `${SCOPE.orgId}/${SCOPE.environmentId}`, version: rec.version, withheld: false, withheldProperties: [], values: rec.values, offers: offersForRecord(rec) } })
    }
    if (parts.length === 3 && parts[2] === "links") {
      return route.fulfill({ json: { objectType, id: decodeURIComponent(parts[1]!), items: [] } })
    }
    const rawFilter = url.searchParams.get("filter")
    const filter = rawFilter ? JSON.parse(rawFilter) as Predicate : null
    const sortRaw = (url.searchParams.get("sort") ?? "").split(",").filter(Boolean).map((p) => {
      const [property = "", direction = "asc"] = p.split(":")
      return { property, direction }
    })
    const matched = records
      .filter((r) => r.objectType === objectType && (filter?.all ?? []).every((c) => matchClause(r, c)))
      .sort((a, b) => {
        for (const s of sortRaw) {
          const av = a.values[s.property]
          const bv = b.values[s.property]
          const ord = typeof av === "number" && typeof bv === "number" ? av - bv : String(av ?? "").localeCompare(String(bv ?? ""))
          if (ord !== 0) return s.direction === "desc" ? -ord : ord
        }
        return a.id.localeCompare(b.id)
      })
    return route.fulfill({
      json: {
        objectType,
        records: matched.map((r) => ({
          objectType: r.objectType,
          id: r.id,
          scope: `${SCOPE.orgId}/${SCOPE.environmentId}`,
          version: r.version,
          withheld: false,
          withheldProperties: [],
          values: r.values,
          offers: offersForRecord(r),
        })),
        quarantineCount: 0,
        scannedThrough: null,
        query: { sort: sortRaw, filter },
        next: null,
        total: { count: matched.length, approximate: false },
      },
    })
  })

  await page.route("**/api/host/v1/actions**", async (route: Route) => {
    const url = new URL(route.request().url())
    if (route.request().method() === "GET") {
      const objectKind = url.searchParams.get("objectKind")
      const objectId = url.searchParams.get("objectId")
      let items = objectKind ? ACTIONS.filter((a) => a.objectKind === objectKind) : [...ACTIONS]
      if (objectKind && objectId) {
        const rec = records.find((r) => r.objectType === objectKind && r.id === objectId)
        const allowed = rec ? offersForRecord(rec) : []
        items = items.filter((a) => allowed.includes(a.actionId))
      }
      return route.fulfill({ json: { items } })
    }
    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const body = route.request().postDataJSON() as { input: Record<string, unknown> }
    const res = runActionOnRecord(actionId, body.input)
    if (!res.ok) return route.fulfill({ status: res.status, json: { code: res.code, category: "law", message: res.message, explanation: res.message } })
    return route.fulfill({ json: res.receipt })
  })

  await page.route("**/api/host/v1/automations**", async (route: Route) => {
    const url = new URL(route.request().url())
    const tail = url.pathname.split("/v1/automations")[1] ?? ""

    if (tail === ":preview") {
      const body = route.request().postDataJSON() as { id?: string; payload?: AutomationDefinition }
      const def = body.id ? (defs.get(body.id)?.payload as unknown as AutomationDefinition | undefined) : body.payload
      if (!def) return route.fulfill({ status: 404, json: { code: "definition.not_found", message: "Automation not found." } })
      const guardErr = validateGuardSyntax(def.if?.expression)
      if (guardErr) return route.fulfill({ status: 422, json: { code: "definition.refused", diagnostics: [{ code: "definition.guard_invalid", message: guardErr, span: { pointer: "/if/expression" } }] } })
      const scoped = records.filter((r) => r.objectType === def.objectType)
      const matches: AutomationPreviewMatch[] = []
      for (const rec of scoped) {
        const reason = matchesAutomationWhen(def, rec)
        if (!reason) continue
        const g = evalGuardExpr(def.if?.expression, rec)
        if (!g.ok || !g.matched) continue
        matches.push({
          objectType: rec.objectType,
          id: rec.id,
          version: rec.version,
          values: rec.values,
          reason: def.if?.expression ? `${reason}; guard ${def.if.expression} holds` : reason,
          proposedAction: { actionId: def.then.actionId, input: { ...(def.then.input ?? {}), id: rec.id } },
        })
      }
      return route.fulfill({
        json: {
          objectType: def.objectType,
          scannedCount: scoped.length,
          matchedCount: matches.length,
          approximate: false,
          restatement: `When ${def.when.kind} on ${def.objectType}${def.if?.expression ? `, if ${def.if.expression}` : ""}, then ${def.then.kind} ${def.then.actionId}.`,
          matches,
        },
      })
    }

    const runMatch = /^\/(.+):run$/.exec(tail)
    if (runMatch) {
      const id = decodeURIComponent(runMatch[1]!)
      const stored = defs.get(id)
      if (!stored || stored.retired) return route.fulfill({ status: 404, json: { code: "definition.not_found" } })
      const def = stored.payload as unknown as AutomationDefinition
      const scoped = records.filter((r) => r.objectType === def.objectType)
      if (!def.enabled) {
        return route.fulfill({ json: { id, version: stored.version, enabled: false, scannedCount: scoped.length, matchedCount: 0, proposed: 0, committed: 0, refused: 0, skipped: 0, runs: [] } })
      }
      const created: AutomationRunRecord[] = []
      let proposed = 0, committed = 0, refused = 0, skipped = 0
      for (const rec of scoped) {
        if (!matchesAutomationWhen(def, rec)) continue
        const g = evalGuardExpr(def.if?.expression, rec)
        if (!g.ok || !g.matched) continue
        const dedupeKey = `${id}:${stored.version}:${rec.id}:${rec.version}`
        if (runs.some((r) => r.dedupeKey === dedupeKey && r.outcome !== "skipped")) {
          skipped += 1
          continue
        }
        if (def.then.kind === "propose") {
          const decId = `dec_auto_${seqId++}`
          const actionTitle = ACTIONS.find((a) => a.actionId === def.then.actionId)?.title ?? def.then.actionId
          const decision: Decision = {
            id: decId,
            run: { id: `norn.automation/${decId}`, generation: 1, state: null },
            flow: { ruleId: id, version: stored.version, title: def.title },
            step: { id: "propose", label: "Automation proposal" },
            question: def.then.question.replaceAll("{id}", rec.id),
            risk: def.then.risk,
            options: [
              { id: "approve", label: `Run ${actionTitle}`, scope: `${rec.objectType} ${rec.id}`, effect: { action: def.then.actionId } },
              { id: "reject", label: "Dismiss", scope: `${rec.objectType} ${rec.id}`, effect: { binding: "automation.dismiss" } },
            ],
            agentPick: null,
            requestedAtMs: Date.now(),
            expiresAtMs: Date.now() + (def.then.expiresInMs ?? 86_400_000),
            context: { subjects: [{ objectType: rec.objectType, id: rec.id }] },
            inputSchema: null,
          }
          decisions.set(decId, { decision, automationId: id, objectType: rec.objectType, objectId: rec.id, objectVersion: rec.version, actionId: def.then.actionId, resolved: false })
          const entry: AutomationRunRecord = {
            id: `run_${seqId++}`,
            automationId: id,
            automationRevision: stored.version,
            dedupeKey,
            trigger: "manual_run",
            objectType: rec.objectType,
            objectId: rec.id,
            objectVersion: rec.version,
            actionId: def.then.actionId,
            outcome: "proposed",
            decisionId: decId,
            recordedAtMs: Date.now(),
          }
          runs.unshift(entry)
          created.push(entry)
          proposed += 1
        } else {
          const res = runActionOnRecord(def.then.actionId, { ...(def.then.input ?? {}), id: rec.id })
          const entry: AutomationRunRecord = res.ok
            ? {
                id: `run_${seqId++}`,
                automationId: id,
                automationRevision: stored.version,
                dedupeKey,
                trigger: "manual_run",
                objectType: rec.objectType,
                objectId: rec.id,
                objectVersion: rec.version,
                actionId: def.then.actionId,
                outcome: "committed",
                commitSeq: res.receipt.receipt.commitSeq,
                recordedAtMs: Date.now(),
              }
            : {
                id: `run_${seqId++}`,
                automationId: id,
                automationRevision: stored.version,
                dedupeKey,
                trigger: "manual_run",
                objectType: rec.objectType,
                objectId: rec.id,
                objectVersion: rec.version,
                actionId: def.then.actionId,
                outcome: "refused",
                refusalCode: res.code,
                refusalMessage: res.message,
                recordedAtMs: Date.now(),
              }
          runs.unshift(entry)
          created.push(entry)
          if (res.ok) committed += 1
          else refused += 1
        }
      }
      return route.fulfill({ json: { id, version: stored.version, enabled: true, scannedCount: scoped.length, matchedCount: created.length + skipped, proposed, committed, refused, skipped, runs: created } })
    }

    const runsMatch = /^\/(.+)\/runs$/.exec(tail)
    if (runsMatch) {
      const id = decodeURIComponent(runsMatch[1]!)
      return route.fulfill({ json: { id, items: runs.filter((r) => r.automationId === id) } })
    }

    return route.fulfill({ status: 404, json: { code: "route.not_found" } })
  })

  await page.route("**/api/host/v1/decisions**", async (route: Route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith(":requested")) {
      return route.fulfill({ json: { items: [], serverNowMs: Date.now() } })
    }
    const decideMatch = /\/v1\/decisions\/([^/:]+):decide$/.exec(url.pathname)
    if (decideMatch) {
      const decId = decodeURIComponent(decideMatch[1]!)
      const held = decisions.get(decId)
      if (!held) return route.fulfill({ status: 404, json: { code: "decision.not_found", message: "Not found." } })
      const body = route.request().postDataJSON() as { choice: string; note?: string }
      held.resolved = true
      commitSeq += 1
      let call: { committed: boolean; state: "committed" | "refused"; code?: string } | undefined
      if (body.choice === "approve") {
        const res = runActionOnRecord(held.actionId, { id: held.objectId })
        call = res.ok ? { committed: true, state: "committed" } : { committed: false, state: "refused", code: res.code }
        runs.unshift({
          id: `run_${seqId++}`,
          automationId: held.automationId,
          automationRevision: defs.get(held.automationId)?.version ?? 1,
          dedupeKey: `decided:${decId}`,
          trigger: "decision_approved",
          objectType: held.objectType,
          objectId: held.objectId,
          objectVersion: held.objectVersion,
          actionId: held.actionId,
          outcome: res.ok ? "committed" : "refused",
          decisionId: decId,
          ...(res.ok ? { commitSeq: res.receipt.receipt.commitSeq } : { refusalCode: res.code, refusalMessage: res.message }),
          recordedAtMs: Date.now(),
        })
      }
      return route.fulfill({
        json: {
          outcome: "decided",
          decision: { id: decId, choice: body.choice, note: body.note ?? null, decidedBy: "Operator Admin", decidedAtMs: Date.now() },
          receipt: { receiptHash: `dec_${commitSeq}`, commitSeq, recordedAtMs: Date.now() },
          run: { id: held.decision.run.id, state: null },
          ...(call ? { call } : {}),
        },
      })
    }
    const open = [...decisions.values()].filter((d) => !d.resolved).map((d) => d.decision)
    const limit = Number(url.searchParams.get("limit") ?? "50")
    return route.fulfill({
      json: {
        items: limit === 0 ? [] : open.slice(0, limit),
        total: open.length,
        nextCursor: null,
        serverNowMs: Date.now(),
      },
    })
  })

  return { records, defs, decisions, runs }
}
