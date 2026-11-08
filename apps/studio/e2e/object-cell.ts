import type { Page, Route } from "@playwright/test"

/**
 * A stand-in for CELL-DO's `GET /v1/objects/{type}` that implements the relayed query contract
 * (apps/studio/src/lib/records-query.ts) exactly, over an in-memory canon.Holding population.
 * It exists so the grid's rows run before the cell deploys the route; it is not a client policy.
 */

export interface Holding { readonly id: string; readonly withheld: boolean; readonly withheldProperties: readonly string[]; readonly values: Readonly<Record<string, string>>; readonly version?: number }

const METHODS = ["fifo", "moving_average"] as const
const BASES = ["on_hand", "available"] as const
const KINDS: Readonly<Record<string, "text" | "decimal" | "enum">> = {
  resource_code: "text", quantity: "decimal", unit: "text", currency: "text", valuation_method: "enum",
  location: "text", quantity_basis: "enum", stock_cost: "decimal", cost_currency: "text",
}
const ENUMS: Readonly<Record<string, readonly string[]>> = { valuation_method: METHODS, quantity_basis: BASES }
const OPS: Readonly<Record<string, readonly string[]>> = {
  text: ["eq", "ne", "in", "contains", "prefix", "empty", "present"],
  decimal: ["eq", "ne", "in", "lt", "lte", "gt", "gte", "between", "empty", "present"],
  enum: ["eq", "ne", "in", "empty", "present"],
}
const DECIMAL = /^-?\d+(\.\d+)?$/
const PROPERTY = /^[a-z][a-z0-9_]*$/

/** `count` holdings; every `withheldEvery`th row is withheld, and every `costHiddenEvery`th withholds only its stock cost. */
export function population(count: number, withheldEvery = 0, costHiddenEvery = 0): Holding[] {
  return Array.from({ length: count }, (_, index) => {
    const n = index + 1
    const quantity = `${(n * 37) % 1000}.${String((n * 7) % 1000).padStart(3, "0")}`
    return {
      id: `demo/holding/${String(n).padStart(4, "0")}`,
      withheld: withheldEvery > 0 && n % withheldEvery === 0,
      withheldProperties: costHiddenEvery > 0 && n % costHiddenEvery === 0 ? ["stock_cost"] : [],
      values: {
        resource_code: `R-${String(n).padStart(4, "0")}`, quantity, unit: "g", currency: "USD",
        valuation_method: METHODS[n % 2] ?? "fifo", location: n % 3 === 0 ? "north" : "south",
        quantity_basis: BASES[n % 2] ?? "on_hand", stock_cost: `${(n * 13) % 500}.${String(n % 100).padStart(2, "0")}0000`, cost_currency: "USD",
      },
    }
  })
}

/** Exact decimal comparison on the text, no floats. */
function compareDecimal(a: string, b: string): number {
  const [aw = "", af = ""] = a.replace("-", "").split(".")
  const [bw = "", bf = ""] = b.replace("-", "").split(".")
  const an = a.startsWith("-"), bn = b.startsWith("-")
  if (an !== bn) return an ? -1 : 1
  const width = Math.max(af.length, bf.length)
  const x = aw.replace(/^0+(?=\d)/, "") + "." + af.padEnd(width, "0"), y = bw.replace(/^0+(?=\d)/, "") + "." + bf.padEnd(width, "0")
  const magnitude = x.length !== y.length ? x.length - y.length : x < y ? -1 : x > y ? 1 : 0
  return an ? -magnitude : magnitude
}

const compare = (kind: string, a: string, b: string) => kind === "decimal" ? compareDecimal(a, b) : a < b ? -1 : a > b ? 1 : 0

interface Clause { property: string; op: string; value?: unknown }
interface Problem { code: string; message: string; pointer: string }

function matches(row: Holding, clause: Clause): boolean {
  if (row.withheld) return false // A withheld value never matches, not even empty or present.
  const kind = KINDS[clause.property] ?? "text"
  const value = row.values[clause.property]
  if (clause.op === "empty") return value === undefined
  if (clause.op === "present") return value !== undefined
  if (value === undefined) return false
  const c = (other: unknown) => compare(kind, value, String(other))
  switch (clause.op) {
    case "eq": return c(clause.value) === 0
    case "ne": return c(clause.value) !== 0
    case "in": return (clause.value as unknown[]).some((v) => c(v) === 0)
    case "contains": return value.includes(String(clause.value))
    case "prefix": return value.startsWith(String(clause.value))
    case "lt": return c(clause.value) < 0
    case "lte": return c(clause.value) <= 0
    case "gt": return c(clause.value) > 0
    case "gte": return c(clause.value) >= 0
    case "between": { const [lo, hi] = clause.value as [string, string]; return c(lo) >= 0 && c(hi) <= 0 }
    default: return false
  }
}

function validate(url: URL): { sort: { property: string; direction: string }[]; filter: { all: Clause[] } | null; limit: number; problems: Problem[] } {
  const problems: Problem[] = []
  const limitText = url.searchParams.get("limit") ?? "50"
  const limit = Number(limitText)
  if (!Number.isInteger(limit) || limit < 1 || limit > 200) problems.push({ code: "query.limit_invalid", message: "A page holds 1 to 200 records.", pointer: "/limit" })
  const sort = (url.searchParams.get("sort") ?? "").split(",").filter(Boolean).map((key, index) => {
    const [property = "", direction = ""] = key.split(":")
    if (!PROPERTY.test(property) || !(property in KINDS)) problems.push({ code: "query.property_unknown", message: `Holdings have no property ${property}.`, pointer: `/sort/${index}` })
    if (direction !== "asc" && direction !== "desc") problems.push({ code: "query.sort_invalid", message: "Sort direction is asc or desc.", pointer: `/sort/${index}` })
    return { property, direction }
  })
  if (sort.length > 3) problems.push({ code: "query.too_many", message: "Sort by at most 3 properties.", pointer: "/sort/3" })
  let filter: { all: Clause[] } | null = null
  const raw = url.searchParams.get("filter")
  if (raw !== null) {
    try { filter = JSON.parse(raw) as { all: Clause[] } } catch { problems.push({ code: "query.value_invalid", message: "The filter is not JSON.", pointer: "/filter" }) }
    filter?.all.forEach((clause, index) => {
      const at = `/filter/all/${index}`
      const kind = KINDS[clause.property]
      if (kind === undefined) return void problems.push({ code: "query.property_unknown", message: `Holdings have no property ${clause.property}.`, pointer: `${at}/property` })
      if (!(OPS[kind] ?? []).includes(clause.op)) return void problems.push({ code: "query.op_invalid", message: `${clause.property.replaceAll("_", " ")} can't be tested with ${clause.op}.`, pointer: `${at}/op` })
      const values = clause.op === "empty" || clause.op === "present" ? [] : Array.isArray(clause.value) ? clause.value : [clause.value]
      for (const value of values) {
        if (typeof value !== "string" || value === "") problems.push({ code: "query.value_invalid", message: "Enter a value.", pointer: `${at}/value` })
        else if (kind === "decimal" && !DECIMAL.test(value)) problems.push({ code: "query.value_invalid", message: `${value} is not an exact decimal number, like 12.500.`, pointer: `${at}/value` })
        else if (kind === "enum" && !(ENUMS[clause.property] ?? []).includes(value)) problems.push({ code: "query.value_invalid", message: `${value} is not a declared value.`, pointer: `${at}/value` })
      }
    })
    if ((filter?.all.length ?? 0) > 8) problems.push({ code: "query.too_many", message: "A filter has at most 8 conditions.", pointer: "/filter/all/8" })
  }
  return { sort, filter, limit, problems }
}

export interface ObjectCell {
  /** Every request the grid sent, for assertions on what it asked. */
  readonly requests: URL[]
  /** A commit lands: every older cursor now answers 409 `object.snapshot_moved`. */
  commit(): void
  /** The next read fails as a cell outage (500, no refusal document). */
  failNext(): void
  /** The population, which the action stand-in writes to. */
  readonly rows: Holding[]
}

/** Serve `rows` as canon.Holding on the Studio Worker's host route, as CELL-DO will. */
export async function objectCell(page: Page, initial: readonly Holding[]): Promise<ObjectCell> {
  const rows = [...initial]
  const requests: URL[] = []
  let snapshot = 1
  let fail = false
  await page.route("**/api/host/v1/objects/**", async (route: Route) => {
    const url = new URL(route.request().url())
    requests.push(url)
    if (fail) { fail = false; return route.fulfill({ status: 500, body: "upstream reset" }) }
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") return route.fulfill({ status: 403, json: { code: "scope.denied", message: "Not your workspace." } })
    if (!url.pathname.endsWith("/v1/objects/canon.Holding")) return route.fulfill({ status: 404, json: { code: "object.type_unknown", message: "No such object type." } })
    const { sort, filter, limit, problems } = validate(url)
    if (problems.length > 0) return route.fulfill({ status: 400, json: { code: "query.invalid", message: "The cell refused this query.", diagnostics: problems } })
    const shape = JSON.stringify({ sort, filter, limit })
    let offset = 0
    const after = url.searchParams.get("after")
    if (after !== null) {
      let cursor: { offset: number; shape: string; snapshot: number }
      try { cursor = JSON.parse(atob(after)) as typeof cursor } catch { return route.fulfill({ status: 400, json: { code: "query.cursor_invalid", message: "That page link isn't valid.", diagnostics: [{ code: "query.cursor_invalid", message: "Unknown cursor.", pointer: "/after" }] } }) }
      if (cursor.shape !== shape) return route.fulfill({ status: 400, json: { code: "query.cursor_invalid", message: "That page link belongs to another query.", diagnostics: [{ code: "query.cursor_invalid", message: "Cursor and query differ.", pointer: "/after" }] } })
      if (cursor.snapshot !== snapshot) return route.fulfill({ status: 409, json: { code: "object.snapshot_moved", message: "The records changed since this page was read." } })
      offset = cursor.offset
    }
    const kept = rows.filter((row) => (filter?.all ?? []).every((clause) => matches(row, clause)))
    const ordered = [...kept].sort((a, b) => {
      for (const { property, direction } of sort) {
        const [x, y] = [a.withheld ? undefined : a.values[property], b.withheld ? undefined : b.values[property]]
        if (x === undefined || y === undefined) { if (x !== y) return x === undefined ? 1 : -1; continue } // missing and withheld last, both directions
        const order = compare(KINDS[property] ?? "text", x, y) * (direction === "desc" ? -1 : 1)
        if (order !== 0) return order
      }
      return a.id < b.id ? -1 : a.id > b.id ? 1 : 0
    })
    const slice = ordered.slice(offset, offset + limit)
    const next = offset + limit < ordered.length ? btoa(JSON.stringify({ offset: offset + limit, shape, snapshot })) : null
    // Exact decimals go out as JSON number tokens, as the cell writes them.
    const records = slice.map((row) => `{"objectType":"canon.Holding","id":${JSON.stringify(row.id)},"scope":"org_demo/analytics","version":${row.version ?? 1},"withheld":${row.withheld},"withheldProperties":${JSON.stringify(row.withheld ? [] : row.withheldProperties)},"values":${row.withheld ? "null" : `{${Object.entries(row.values).filter(([k]) => !row.withheldProperties.includes(k)).map(([k, v]) => `${JSON.stringify(k)}:${KINDS[k] === "decimal" ? v : JSON.stringify(v)}`).join(",")}}`}}`)
    const body = `{"objectType":"canon.Holding","records":[${records.join(",")}],"quarantineCount":0,"scannedThrough":null,"query":${JSON.stringify({ sort, filter })},"next":${JSON.stringify(next)},"total":{"count":${ordered.length},"approximate":false}}`
    return route.fulfill({ status: 200, contentType: "application/json", body })
  })
  return { requests, rows, commit: () => { snapshot += 1 }, failNext: () => { fail = true } }
}

export async function session(page: Page, environmentId: string | null = "analytics") {
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" }, organization: { id: "org_demo", name: "Demo", role: "viewer" }, environmentId } }))
}
