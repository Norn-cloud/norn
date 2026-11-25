import type { Page, Route } from "@playwright/test"
import type { ActionSummary } from "../../../packages/norn-sdk/src/index.ts"
import type { Clause, Predicate } from "../src/lib/records-query.ts"

/**
 * Deterministic two-domain stand-in for CELL-DO's view, object, and per-record action routes so
 * Playwright exercises Studio's Workspace Views flows across two unrelated domains:
 *   1. Application Funnel (`funnel.Application`) — issue #1424's scenario
 *   2. Equipment Fleet Tickets (`fleet.Ticket`) — unrelated second domain proving genericity
 */

export interface DemoRecord {
  readonly objectType: string
  readonly id: string
  version: number
  values: Record<string, string | number | boolean>
}

interface StoredDef {
  readonly kind: string
  version: number
  payload: Record<string, unknown>
  retired: boolean
  updatedAtMs: number
}

interface ActionReceiptDocument {
  readonly outcome: "committed" | "replayed"
  readonly objectType: string
  readonly objectId: string
  readonly version: number
  readonly receipt: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
}

export interface RecordedActionCall {
  readonly actionId: string
  readonly idempotencyKey: string
  readonly input: Record<string, unknown>
  /** The workspace the call was sent to (query) and expected (headers), and whether the Worker refused it as stale. */
  readonly orgId: string | null
  readonly expectedOrg: string | null
  readonly workspaceChanged: boolean
}

export interface RecordedInstallCall {
  readonly id: string
  readonly orgId: string | null
  readonly environmentId: string | null
  readonly expectedVersion: number | undefined
}

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

function drawerActionsForRecord(record: DemoRecord): string[] {
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

/** The type's text, choice and yes/no properties, as `crates/norn-api/src/object_facets.rs` counts them (`kind_of` is "text" or "boolean"). */
function categorical(objectType: string): string[] {
  const entity = ARTIFACTS.find((artifact) => (artifact.payload as { objectType?: { name?: string } }).objectType?.name === objectType)
  const properties = ((entity?.payload as { objectType?: { properties?: readonly { id: string; type: { kind: string } }[] } } | undefined)?.objectType?.properties ?? [])
  return properties.filter(({ type }) => type.kind === "text" || type.kind === "enum" || type.kind === "boolean").map(({ id }) => id)
}

const MAX_CATEGORY_CHARS = 64
const MAX_FACET_VALUES = 20

/** `free_text` in object_facets.rs: a long value, or, over three or more rows, a different value on every row that has one. */
function freeText(property: string, rows: readonly DemoRecord[]): boolean {
  const present = rows.map((row) => row.values[property]).filter((value) => value !== undefined && value !== null)
  if (present.some((value) => typeof value === "string" && value.length > MAX_CATEGORY_CHARS)) return true
  return rows.length >= 3 && present.length >= 3 && new Set(present.map(String)).size === present.length
}

/**
 * The key object_facets.rs groups and breaks ties by: the value's Rust `Debug` text (`RuntimeValue` derives
 * it), so `String("north")`, `Bool(true)`, `Number(3)`, with Debug's escapes for quotes, backslashes and
 * control characters (the stand-in's values hold no other character Debug escapes).
 */
function debugKey(value: string | number | boolean): string {
  if (typeof value === "boolean") return `Bool(${value})`
  if (typeof value === "number") return `Number(${value})`
  const escaped = [...value].map((char) => {
    const code = char.codePointAt(0)!
    if (char === "\\" || char === '"') return `\\${char}`
    if (char === "\n") return "\\n"
    if (char === "\r") return "\\r"
    if (char === "\t") return "\\t"
    if (char === "\0") return "\\0"
    return code < 0x20 || code === 0x7f ? `\\u{${code.toString(16)}}` : char
  }).join("")
  return `String("${escaped}")`
}

/** Rust's `String` order: byte by byte over the UTF-8 text (JavaScript's `<` compares UTF-16 units). */
const UTF8 = new TextEncoder()
function byteOrder(left: string, right: string): number {
  const a = UTF8.encode(left)
  const b = UTF8.encode(right)
  for (let at = 0; at < Math.min(a.length, b.length); at += 1) if (a[at] !== b[at]) return a[at]! - b[at]!
  return a.length - b.length
}

/** `facet` in object_facets.rs: each value's count, most rows first (ties by `debugKey`), the tail and the empties counted together. */
function facetOf(property: string, rows: readonly DemoRecord[]) {
  const held = new Map<string, { value: string | number | boolean; count: number }>()
  let nulls = 0
  for (const row of rows) {
    const value = row.values[property]
    if (value === undefined || value === null) { nulls += 1; continue }
    const key = debugKey(value)
    const entry = held.get(key) ?? { value, count: 0 }
    entry.count += 1
    held.set(key, entry)
  }
  const ordered = [...held.entries()].sort(([left, a], [right, b]) => b.count - a.count || byteOrder(left, right)).map(([, entry]) => entry)
  return {
    property,
    values: ordered.slice(0, MAX_FACET_VALUES),
    otherRows: ordered.slice(MAX_FACET_VALUES).reduce((sum, { count }) => sum + count, 0),
    nulls,
  }
}

export interface WorkspaceCell {
  readonly records: DemoRecord[]
  readonly defs: Map<string, StoredDef>
  readonly actionCalls: RecordedActionCall[]
  readonly installCalls: RecordedInstallCall[]
  /** Simulate switching the signed-in server session to another workspace/environment. */
  switchScope(orgId: string, environmentId: string): void
}

export async function workspaceCell(page: Page): Promise<WorkspaceCell> {
  let activeScope = { orgId: "org_demo", environmentId: "prod" }
  const records = initialRecords()
  const defs = new Map<string, StoredDef>()
  const actionCalls: RecordedActionCall[] = []
  const installCalls: RecordedInstallCall[] = []
  const receiptsByKey = new Map<string, { readonly inputJson: string; readonly receipt: ActionReceiptDocument }>()
  let commitSeq = 10

  const runActionOnRecord = (actionId: string, input: Record<string, unknown>, idempotencyKey: string) => {
    const inputJson = JSON.stringify(input)
    const keySlot = `${actionId}:${idempotencyKey}`
    const prior = receiptsByKey.get(keySlot)
    if (prior) {
      if (prior.inputJson !== inputJson) {
        return { ok: false as const, status: 409, code: "action.idempotency_conflict", message: "Idempotency key reused with different input." }
      }
      return { ok: true as const, receipt: { ...prior.receipt, outcome: "replayed" as const } }
    }
    const id = String(input.id ?? "")
    const action = ACTIONS.find((a) => a.actionId === actionId)
    const rec = records.find((r) => r.objectType === action?.objectKind && r.id === id)
    if (!action || !rec) return { ok: false as const, status: 403, code: "authz.denied", message: "Action not permitted." }
    if (!drawerActionsForRecord(rec).includes(actionId)) {
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
    const receipt: ActionReceiptDocument = {
      outcome: "committed",
      objectType: rec.objectType,
      objectId: rec.id,
      version: rec.version,
      receipt: { receiptHash: `h_${commitSeq}`, commitSeq, recordedAtMs: BASE_NOW + commitSeq * 1000 },
    }
    receiptsByKey.set(keySlot, { inputJson, receipt })
    return { ok: true as const, receipt }
  }

  await page.route("**/api/me", (route) => route.fulfill({
    json: {
      user: { id: "u_ops", name: "Operator Admin", email: "ops@example.com" },
      organization: { id: activeScope.orgId, name: "Workspace Demo", role: "admin" },
      environmentId: activeScope.environmentId,
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
      const payload = (body?.payload ?? {}) as Record<string, unknown>
      const diagnostics: { code: string; message: string; span: { pointer: string } }[] = []
      if (!String(payload.title ?? "").trim()) {
        diagnostics.push({ code: "definition.title_invalid", message: "Title is required.", span: { pointer: "/title" } })
      }
      if (diagnostics.length > 0) {
        return route.fulfill({ json: { admitted: false, diagnostics, restatement: null, restatementSha256: null } })
      }
      const restatement = `Saved view “${String(payload.title)}” over ${String(payload.objectType)}${payload.space ? ` in Space ${String(payload.space)}` : ""}.`
      return route.fulfill({ json: { admitted: true, diagnostics: [], restatement, restatementSha256: "a".repeat(64) } })
    }

    if (tail === ":install") {
      const id = String(body?.id ?? "")
      const payload = (body?.payload ?? {}) as Record<string, unknown>
      const expected = typeof body?.expectedVersion === "number" ? body.expectedVersion : undefined
      installCalls.push({
        id,
        orgId: url.searchParams.get("orgId"),
        environmentId: url.searchParams.get("environmentId"),
        expectedVersion: expected,
      })
      const existing = defs.get(id)
      // Matches crates/norn-api/src/definition_routes.rs:1376: omitting expectedVersion on an existing definition conflicts.
      if (existing?.version !== expected) {
        return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Version conflict." } })
      }
      const version = (existing?.version ?? 0) + 1
      defs.set(id, { kind: "view", version, payload, retired: false, updatedAtMs: Date.now() })
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
    if (!existing) return route.fulfill({ status: 404, json: { code: "definition.not_found", message: "Not found." } })
    return route.fulfill({
      json: {
        id,
        kind: existing.kind,
        version: existing.version,
        lifecycle: existing.retired ? "retired" : "active",
        payload: existing.payload,
        updatedAtMs: existing.updatedAtMs,
      },
    })
  })

  await page.route("**/api/host/v1/objects/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const rest = url.pathname.split("/v1/objects/")[1] ?? ""
    if (rest.endsWith(":facets")) {
      // The counts the cell serves (HOST-READ-7): over the rows the filter keeps, one facet per text, choice
      // or yes/no property (free text left out unless asked for by name).
      const objectType = decodeURIComponent(rest.slice(0, -":facets".length))
      const rawFilter = url.searchParams.get("filter")
      const filter = rawFilter ? JSON.parse(rawFilter) as Predicate : null
      const rows = records.filter((r) => r.objectType === objectType && (filter?.all ?? []).every((c) => matchClause(r, c)))
      // Named properties are taken as named (object_facets.rs trims, never drops), and each must be text, choice or yes/no.
      const named = url.searchParams.get("properties")?.split(",").map((name) => name.trim())
      const notCategorical = (named ?? []).filter((name) => !categorical(objectType).includes(name))
      if (notCategorical.length > 0) {
        return route.fulfill({ status: 400, json: {
          code: "query.invalid", category: "invalid", retryable: false, message: "the object query is not valid",
          explanation: "each diagnostic names the rule and where in the request it applies",
          diagnostics: notCategorical.map((name) => ({ code: "query.property_unknown", severity: "error", message: `${name} is not a text, choice or yes/no property of ${objectType}`, span: { pointer: "/properties" } })),
        } })
      }
      const asked = named ?? categorical(objectType).filter((property) => !freeText(property, rows))
      return route.fulfill({ json: { objectType, rows: rows.length, withheldRows: 0, quarantinedRows: 0, lowerBound: false, scanLimit: 10000, facets: asked.map((property) => facetOf(property, rows)) } })
    }
    const parts = rest.split("/")
    const objectType = decodeURIComponent(parts[0]!)
    if (parts.length === 2) {
      const rec = records.find((r) => r.objectType === objectType && r.id === decodeURIComponent(parts[1]!))
      if (!rec) return route.fulfill({ status: 404, json: { code: "object.not_found" } })
      return route.fulfill({ json: { objectType: rec.objectType, id: rec.id, scope: `${activeScope.orgId}/${activeScope.environmentId}`, version: rec.version, withheld: false, withheldProperties: [], values: rec.values } })
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
          scope: `${activeScope.orgId}/${activeScope.environmentId}`,
          version: r.version,
          withheld: false,
          withheldProperties: [],
          values: r.values,
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
        const allowed = rec ? drawerActionsForRecord(rec) : []
        items = items.filter((a) => allowed.includes(a.actionId))
      }
      return route.fulfill({ json: { definitionsVersion: 1, items } })
    }
    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const body = route.request().postDataJSON() as { input: Record<string, unknown>; idempotencyKey: string }
    const orgId = url.searchParams.get("orgId")
    const expectedOrg = route.request().headers()["x-norn-expected-org"] ?? null
    // As the Worker does: a write expecting another workspace than the session's is refused, never committed.
    const workspaceChanged = (orgId !== null && orgId !== activeScope.orgId) || (expectedOrg !== null && expectedOrg !== activeScope.orgId)
    actionCalls.push({ actionId, idempotencyKey: body.idempotencyKey, input: body.input, orgId, expectedOrg, workspaceChanged })
    if (workspaceChanged) return route.fulfill({ status: 409, json: { code: "workspace.changed", message: "This workspace changed; reload to edit this view.", error: "This workspace changed; reload to edit this view." } })
    const res = runActionOnRecord(actionId, body.input, body.idempotencyKey)
    if (!res.ok) return route.fulfill({ status: res.status, json: { format: "norn/refusal@2", code: res.code, category: res.status === 403 ? "permission" : res.status === 409 ? "conflict" : "precondition", retryable: false, reason: null, message: res.message, explanation: res.message } })
    return route.fulfill({ json: res.receipt })
  })

  await page.route("**/api/host/v1/decisions**", async (route: Route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith(":requested")) {
      return route.fulfill({ json: { items: [], serverNowMs: Date.now() } })
    }
    return route.fulfill({ json: { items: [], total: 0, nextCursor: null, serverNowMs: Date.now() } })
  })

  return {
    records,
    defs,
    actionCalls,
    installCalls,
    switchScope: (orgId: string, environmentId: string) => {
      activeScope = { orgId, environmentId }
    },
  }
}
