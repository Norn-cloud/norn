import type { Page, Route } from "@playwright/test"
import { INTERVAL_ACTIONS, INTERVAL_ARTIFACTS, intervalRecords, type IntervalRecord } from "./interval-fixtures.ts"

/**
 * A stand-in cell for the two interval worlds (interval-fixtures.ts): the definitions read that
 * makes Studio live, `GET /v1/objects/{type}` under the records query contract
 * (src/lib/records-query.ts) for the operators these rows use, a record's out-links, and the action
 * routes. It decides nothing Studio could: it filters, orders and refuses as a cell would.
 *
 * `pointed` serves input refusals with a diagnostic per field at `/input/<field>` (nested pointers
 * included), the refusal shape requested of the backend lane; the served route sends one
 * `action.invalid_input` without pointers today (packages/norn-sdk/src/client.ts).
 */

interface Clause { property: string; op: string; value?: unknown }
interface Problem { code: string; message: string; pointer: string }

const TIMES = new Set(["starts_at", "ends_at"])

function compare(property: string, a: string, b: string): number {
  if (TIMES.has(property)) return Date.parse(a) - Date.parse(b)
  return a < b ? -1 : a > b ? 1 : 0
}

function matches(record: IntervalRecord, clause: Clause): boolean {
  const value = record.values[clause.property]
  if (clause.op === "empty") return value === null || value === undefined
  if (clause.op === "present") return value !== null && value !== undefined
  if (value === null || value === undefined) return false
  const c = compare(clause.property, value, String(clause.value))
  switch (clause.op) {
    case "eq": return c === 0
    case "contains": return value.toLowerCase().includes(String(clause.value).toLowerCase())
    case "lt": return c < 0
    case "lte": return c <= 0
    case "gt": return c > 0
    case "gte": return c >= 0
    default: return false
  }
}

const OPS: Readonly<Record<string, readonly string[]>> = { time: ["eq", "lt", "lte", "gt", "gte", "empty", "present"], text: ["eq", "contains", "empty", "present"] }

export interface IntervalCell {
  readonly records: IntervalRecord[]
  /** Every objects request, for assertions on what Studio asked. */
  readonly requests: URL[]
  /** Every action call body. */
  readonly calls: { readonly actionId: string; readonly body: string }[]
}

export async function intervalCell(page: Page, options: { readonly pointed?: boolean } = {}): Promise<IntervalCell> {
  const records = intervalRecords()
  const requests: URL[] = []
  const calls: { actionId: string; body: string }[] = []
  const scoped = (url: URL) => url.searchParams.get("orgId") === "org_demo" && url.searchParams.get("environmentId") === "analytics"
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" }, organization: { id: "org_demo", name: "Demo", role: "viewer" }, environmentId: "analytics" } }))
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ json: { artifacts: INTERVAL_ARTIFACTS } }))
  await page.route("**/api/host/v1/objects/**", async (route: Route) => {
    const url = new URL(route.request().url())
    requests.push(url)
    if (!scoped(url)) return route.fulfill({ status: 403, json: { code: "scope.denied", message: "Not your workspace." } })
    const path = decodeURIComponent(url.pathname.split("/v1/objects/")[1] ?? "")
    if (path.endsWith(":facets")) return route.fulfill({ json: { objectType: path.slice(0, -":facets".length), rows: 0, withheldRows: 0, quarantinedRows: 0, lowerBound: false, scanLimit: 10000, facets: [] } })
    const [type = "", ...rest] = path.split("/")
    const tail = rest.join("/")
    if (tail.endsWith("/links")) {
      const id = tail.slice(0, -"/links".length)
      const record = records.find((r) => r.objectType === type && r.id === id)
      if (record === undefined) return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such record." } })
      const link = url.searchParams.get("link")
      const items = Object.entries(record.links).filter(([name]) => link === null || name === link)
        .map(([name, target]) => ({ link: name, source: id, sourceType: type, target, targetType: records.find((r) => r.id === target)?.objectType ?? "" }))
      return route.fulfill({ json: { objectType: type, id, items, hasMore: false } })
    }
    if (tail !== "") {
      // A record's own read: the drawer asks for it with its links; the list page already carried its values.
      return route.fulfill({ status: 404, json: { code: "route.not_found", message: "Not served by this stand-in." } })
    }
    const problems: Problem[] = []
    let filter: { all: Clause[] } | null = null
    const raw = url.searchParams.get("filter")
    if (raw !== null) filter = JSON.parse(raw) as { all: Clause[] }
    filter?.all.forEach((clause, index) => {
      const kind = TIMES.has(clause.property) ? "time" : "text"
      if (!(OPS[kind] ?? []).includes(clause.op)) problems.push({ code: "query.op_invalid", message: `${clause.property} can't be tested with ${clause.op}.`, pointer: `/filter/all/${index}/op` })
      if (kind === "time" && clause.value !== undefined && Number.isNaN(Date.parse(String(clause.value)))) problems.push({ code: "query.value_invalid", message: "Not an RFC 3339 timestamp.", pointer: `/filter/all/${index}/value` })
    })
    if ((filter?.all.length ?? 0) > 8) problems.push({ code: "query.too_many", message: "A filter has at most 8 conditions.", pointer: "/filter/all/8" })
    if (problems.length > 0) return route.fulfill({ status: 400, json: { code: "query.invalid", message: "The cell refused this query.", diagnostics: problems } })
    const sort = (url.searchParams.get("sort") ?? "").split(",").filter(Boolean).map((key) => { const [property = "", direction = "asc"] = key.split(":"); return { property, direction } })
    const limit = Number(url.searchParams.get("limit") ?? "50")
    const offset = Number(url.searchParams.get("after") ?? "0")
    const kept = records.filter((r) => r.objectType === type && (filter?.all ?? []).every((clause) => matches(r, clause)))
    kept.sort((a, b) => {
      for (const { property, direction } of sort) {
        const order = compare(property, a.values[property] ?? "", b.values[property] ?? "") * (direction === "desc" ? -1 : 1)
        if (order !== 0) return order
      }
      return a.id < b.id ? -1 : 1
    })
    const slice = kept.slice(offset, offset + limit)
    const offers = INTERVAL_ACTIONS.filter((action) => action.objectKind === type).map(({ actionId }) => actionId)
    return route.fulfill({
      json: {
        objectType: type, quarantineCount: 0, scannedThrough: null, query: { sort, filter }, next: offset + limit < kept.length ? String(offset + limit) : null, total: { count: kept.length, approximate: false },
        records: slice.map((r) => ({ objectType: r.objectType, id: r.id, scope: "org_demo/analytics", version: r.version, withheld: false, withheldProperties: [], values: r.values, offers })),
      },
    })
  })
  await page.route("**/api/host/v1/actions**", async (route: Route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (!scoped(url)) return route.fulfill({ status: 403, json: { format: "norn/refusal@2", code: "authz.denied", category: "permission", retryable: false, reason: null, message: "the action request is not authorized", explanation: "" } })
    if (request.method() === "GET") {
      const kind = url.searchParams.get("objectKind")
      return route.fulfill({ json: { definitionsVersion: 1, items: INTERVAL_ACTIONS.filter((a) => kind === null || a.objectKind === kind) } })
    }
    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const body = request.postData() ?? ""
    calls.push({ actionId, body })
    const action = INTERVAL_ACTIONS.find((a) => a.actionId === actionId)
    if (action === undefined) return route.fulfill({ status: 403, json: { format: "norn/refusal@2", code: "authz.denied", category: "permission", retryable: false, reason: null, message: "the action request is not authorized", explanation: "" } })
    const { input } = JSON.parse(body) as { input: Record<string, unknown> }
    const problems: { code: string; message: string; span: { pointer: string } }[] = []
    const schema = action.inputSchema as { required: readonly string[]; properties: Record<string, { format?: string; title?: string }> }
    for (const field of schema.required) if (!(field in input)) problems.push({ code: "action.input_missing", message: `${schema.properties[field]?.title ?? field} is required.`, span: { pointer: `/input/${field}` } })
    for (const [field, value] of Object.entries(input)) {
      const format = schema.properties[field]?.format
      if (format === "date-time" && typeof value === "string" && !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(Z|[+-]\d{2}:\d{2})$/.test(value)) problems.push({ code: "action.input_invalid", message: "Must be an instant with an offset.", span: { pointer: `/input/${field}` } })
    }
    const starts = typeof input["starts_at"] === "string" ? Date.parse(input["starts_at"]) : NaN, ends = typeof input["ends_at"] === "string" ? Date.parse(input["ends_at"]) : NaN
    // The interface's invariant, `starts_at < ends_at`, refused at the end that breaks it.
    if (!Number.isNaN(starts) && !Number.isNaN(ends) && starts >= ends) problems.push({ code: "core.time.improper_interval", message: "Ends must be after it starts.", span: { pointer: "/input/ends_at" } })
    const linked = (action as { inputLinks?: readonly { field: string; objectKind: string }[] }).inputLinks ?? []
    for (const { field, objectKind } of linked) {
      if (typeof input[field] === "string" && !records.some((r) => r.objectType === objectKind && r.id === input[field])) problems.push({ code: "action.slice_object_missing", message: "No such record.", span: { pointer: `/input/${field}` } })
    }
    if (problems.length > 0) {
      return route.fulfill({ status: 400, json: options.pointed
        ? { format: "norn/refusal@2", code: "action.invalid_input", category: "invalid", retryable: false, reason: null, message: "the input does not satisfy the declared schema", explanation: "", diagnostics: problems.map((p) => ({ ...p, severity: "error" })) }
        : { format: "norn/refusal@2", code: "action.invalid_input", category: "invalid", retryable: false, reason: null, message: "the input does not satisfy the declared schema", explanation: "" } })
    }
    const id = String(input["id"])
    const existing = records.find((r) => r.objectType === action.objectKind && r.id === id)
    const values = { ...existing?.values ?? {}, ...Object.fromEntries(Object.entries(input).filter(([k]) => k !== "id" && !linked.some((l) => l.field === k)).map(([k, v]) => [k, v === null ? null : String(v)])) } as Record<string, string | null>
    const links = Object.fromEntries(linked.map((l) => [l.field, String(input[l.field])]))
    const version = (existing?.version ?? 0) + 1
    if (existing) { existing.values = values; existing.links = { ...existing.links, ...links }; existing.version = version } else records.push({ objectType: action.objectKind, id, values, links, version })
    return route.fulfill({ json: { outcome: "committed", objectType: action.objectKind, objectId: id, version, receipt: { receiptHash: "c".repeat(64), commitSeq: 100 + calls.length, recordedAtMs: Date.now() } } })
  })
  return { records, requests, calls }
}
