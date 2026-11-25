import { readFileSync } from "node:fs"
import type { Page, Route } from "@playwright/test"

/**
 * A stand-in for CELL-DO's object queries over an in-memory canon.Holding population.
 * Invalid requests use the responses pinned by the cell's route test.
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
// The DO dispatch pins the status and full refusal JSON in tests/server.rs.
const SERVED: typeof import("../test/fixtures/object-query.json") = JSON.parse(
  readFileSync(new URL("../test/fixtures/object-query.json", import.meta.url), "utf8"),
)
type Finding = (typeof SERVED.decimal.json.diagnostics)[number]
type QueryCase = Exclude<keyof typeof SERVED, "denied" | "unknownType" | "cursorInvalid" | "snapshotMoved">

const pinnedFilters = new Map([
  ['{"all":[{"property":"quantity","op":"eq","value":"12,5"}]}', SERVED.decimal],
  ['{"all":[{"property":"quantity","op":"between","value":["1","12,5"]}]}', SERVED.rangeUpperDecimal],
])

function finding(name: QueryCase, pointer?: string, argument?: { sample: string; value: string }): Finding {
  const [example] = SERVED[name].json.diagnostics
  if (!example) throw new Error(`Missing served diagnostic for ${name}`)
  if (argument !== undefined && !example.message.includes(argument.sample)) {
    throw new Error(`Served diagnostic for ${name} does not contain ${argument.sample}`)
  }
  return {
    ...example,
    message: argument === undefined ? example.message : example.message.replace(argument.sample, argument.value),
    span: pointer === undefined ? example.span : { ...example.span, pointer },
  }
}

function refuseQuery(route: Route, diagnostics: Finding[], facets = false) {
  const { status, json } = facets ? SERVED.facetsDecimal : SERVED.decimal
  return route.fulfill({ status, json: { ...json, diagnostics } })
}

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

function object(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

const OPERATORS = new Set(Object.values(OPS).flat())

function validate(url: URL, facets = false) {
  const problems: Finding[] = []
  const add = (name: QueryCase, pointer?: string, argument?: { sample: string; value: string }) =>
    problems.push(finding(name, pointer, argument))
  const limitText = facets ? "50" : url.searchParams.get("limit") ?? "50"
  const limit = Number(limitText)
  if (!/^\d+$/.test(limitText) || limit < 1 || limit > 200) add("limit")

  const sort: { property: string; direction: string }[] = []
  const sortText = facets ? null : url.searchParams.get("sort")
  const keys = sortText === null ? [] : sortText.split(",")
  if (keys.length > 3) add("sortCount")
  for (const [index, key] of keys.entries()) {
    const at = `/sort/${index}`
    const split = key.indexOf(":")
    if (split === -1) { add("sortSyntax", at); continue }
    const property = key.slice(0, split)
    const direction = key.slice(split + 1)
    if (direction !== "asc" && direction !== "desc") { add("sortDirection", at); continue }
    if (!PROPERTY.test(property)) { add("sortName", at); continue }
    if (!Object.hasOwn(KINDS, property)) { add("sortUnknown", at, { sample: "missing", value: property }); continue }
    if (sort.some((entry) => entry.property === property)) { add("sortDuplicate", at, { sample: "quantity", value: property }); continue }
    sort.push({ property, direction })
  }

  let filter: { all: Clause[] } | null = null
  const raw = url.searchParams.get("filter")
  if (raw !== null) {
    let parsed: unknown
    try { parsed = JSON.parse(raw) } catch { /* The served grammar refuses below. */ }
    if (!object(parsed)) add("filterJson")
    else {
      if (Object.keys(parsed).some((key) => key !== "all")) add("filterMembers")
      if (!Array.isArray(parsed.all)) add("filterArray")
      else {
        if (parsed.all.length === 0) add("filterEmpty")
        if (parsed.all.length > 8) add("filterCount")
        filter = { all: [] }
        for (const [index, clause] of parsed.all.entries()) {
          const at = `/filter/all/${index}`
          if (!object(clause)) { add("clauseObject", at); continue }
          if (Object.keys(clause).some((key) => !["property", "op", "value"].includes(key))) add("clauseMembers", at)
          const { property, op, value } = clause
          if (typeof property !== "string" || !PROPERTY.test(property)) { add("propertyName", `${at}/property`); continue }
          const kind = Object.hasOwn(KINDS, property) ? KINDS[property] : undefined
          if (kind === undefined) { add("propertyUnknown", `${at}/property`, { sample: "missing", value: property }); continue }
          if (typeof op !== "string" || !OPERATORS.has(op)) { add("opUnknown", `${at}/op`); continue }
          if (!(OPS[kind] ?? []).includes(op)) { add("opKind", `${at}/op`, { sample: "quantity", value: property }); continue }
          const pointer = `${at}/value`
          const scalar = (operand: unknown, path: string): operand is string => {
            if (typeof operand !== "string") { add("scalarType", path); return false }
            if (kind === "decimal" && !DECIMAL.test(operand)) { add("decimal", path); return false }
            if (kind === "enum" && !(ENUMS[property] ?? []).includes(operand)) { add("enumValue", path); return false }
            return true
          }
          if (op === "empty" || op === "present") {
            if (Object.hasOwn(clause, "value")) add("presenceValue", pointer)
          } else if (op === "in") {
            if (!Array.isArray(value)) add("inArray", pointer)
            else if (value.length === 0) add("inEmpty", pointer)
            else if (value.length > 50) add("inCount", pointer)
            else value.forEach((item, i) => scalar(item, `${pointer}/${i}`))
          } else if (op === "between") {
            if (!Array.isArray(value) || value.length !== 2) add("rangeArray", pointer)
            else {
              const [lo, hi] = value
              const validLo = scalar(lo, `${pointer}/0`)
              const validHi = scalar(hi, `${pointer}/1`)
              if (validLo && validHi && compare(kind, lo, hi) > 0) add("rangeOrder", pointer)
            }
          } else if (value === undefined) add("missingValue", pointer)
          else scalar(value, pointer)
          filter.all.push({ property, op, value })
        }
      }
    }
  }
  const after = facets ? null : url.searchParams.get("after")
  if (after === "") add("cursorEmpty")
  return { sort, filter, limit, problems }
}

export interface ObjectCell {
  /** Every request the grid sent, for assertions on what it asked. */
  readonly requests: URL[]
  /** A commit lands: every older cursor now answers 409 `object.snapshot_moved`. */
  commit(): void
  /** The next page read fails as a cell outage (500, no refusal document). */
  failNext(): void
  /** The population, which the action stand-in writes to. */
  readonly rows: Holding[]
}

/**
 * The cell's definitions read (`GET /v1/definitions`), as a cell that serves records answers it: the three
 * systems' artifacts, so the page's mode is live and its records are that cell's (#1754: a mode decided
 * recorded reads only the recording). Every other definitions route falls through to what a test or
 * `viewsCell` routes, and any of them registered later answers the read instead.
 */
export async function servedDefinitions(page: Page) {
  await page.route("**/api/host/v1/definitions**", (route: Route) => new URL(route.request().url()).pathname.endsWith("/v1/definitions")
    ? route.fulfill({ json: { artifacts: JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown } })
    : route.fallback())
}

/** Serve `rows` as canon.Holding on the Studio Worker's host route, as CELL-DO will, with the cell's definitions read. */
export async function objectCell(page: Page, initial: readonly Holding[]): Promise<ObjectCell> {
  const rows = [...initial]
  const requests: URL[] = []
  let snapshot = 1
  let fail = false
  await servedDefinitions(page)
  await page.route("**/api/host/v1/objects/**", async (route: Route) => {
    const url = new URL(route.request().url())
    requests.push(url)
    // The outage hits the next page read; a facets read in flight (a refresh after a commit) does not take it.
    if (fail && !url.pathname.endsWith(":facets")) { fail = false; return route.fulfill({ status: 500, body: "upstream reset" }) }
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") return route.fulfill(SERVED.denied)
    // HOST-READ-7: exact value counts of the categorical properties over the rows the filter keeps.
    if (url.pathname.endsWith("/v1/objects/canon.Holding:facets")) {
      const { filter, problems } = validate(url, true)
      if (problems.length > 0) return refuseQuery(route, problems, true)
      const asked = url.searchParams.get("properties")?.split(",") ?? Object.keys(KINDS).filter((property) => KINDS[property] !== "decimal")
      const counted = rows.filter((row) => !row.withheld && (filter?.all ?? []).every((clause) => matches(row, clause)))
      const facets = asked.map((property) => {
        const held = new Map<string, number>()
        let nulls = 0
        for (const row of counted) {
          const value = row.withheldProperties.includes(property) ? undefined : row.values[property]
          if (value === undefined || value === null) nulls += 1
          else held.set(String(value), (held.get(String(value)) ?? 0) + 1)
        }
        const values = [...held].sort(([a, x], [b, y]) => y - x || (a < b ? -1 : 1)).map(([value, count]) => ({ value, count }))
        return { property, values: values.slice(0, 20), otherRows: values.slice(20).reduce((sum, { count }) => sum + count, 0), nulls }
      })
      return route.fulfill({ json: { objectType: "canon.Holding", rows: counted.length, withheldRows: rows.filter((row) => row.withheld).length, quarantinedRows: 0, lowerBound: false, scanLimit: 10000, facets } })
    }
    if (!url.pathname.endsWith("/v1/objects/canon.Holding")) return route.fulfill(SERVED.unknownType)
    const pinned = pinnedFilters.get(url.searchParams.get("filter") ?? "")
    if (pinned !== undefined) return route.fulfill(pinned)
    const { sort, filter, limit, problems } = validate(url)
    if (problems.length > 0) return refuseQuery(route, problems)
    const shape = JSON.stringify({ sort, filter })
    let offset = 0
    const after = url.searchParams.get("after")
    if (after !== null) {
      let cursor: unknown
      try { cursor = JSON.parse(atob(after)) } catch { return route.fulfill(SERVED.cursorInvalid) }
      if (!object(cursor) || cursor.shape !== shape || typeof cursor.offset !== "number" || typeof cursor.snapshot !== "number") {
        return route.fulfill(SERVED.cursorInvalid)
      }
      if (cursor.snapshot !== snapshot) return route.fulfill(SERVED.snapshotMoved)
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

/** A stand-in for the cell's retained definitions, for saved views (VIEW-DEF-1): list, read, install with version checks, retire. */
export async function viewsCell(page: Page) {
  const views = new Map<string, { version: number; payload: unknown; retired: boolean }>()
  const calls: { readonly method: string; readonly path: string; readonly body: unknown }[] = []
  await page.route("**/api/host/v1/definitions**", async (route: Route) => {
    const url = new URL(route.request().url())
    const method = route.request().method()
    const body = route.request().postData() === null ? null : JSON.parse(route.request().postData() ?? "null") as { id?: string; payload?: unknown; expectedVersion?: number } | null
    calls.push({ method, path: url.pathname, body })
    const tail = url.pathname.split("/v1/definitions")[1] ?? ""
    // The definitions read that makes Studio live: the three systems' recorded artifacts.
    if (tail === "") return route.fulfill({ json: { artifacts: JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown } })
    if (tail === ":list") return route.fulfill({ json: { items: [...views].filter(([, view]) => !view.retired).map(([id, view]) => ({ id, kind: "view", version: view.version })), nextCursor: null } })
    if (tail === ":install") {
      const id = body?.id ?? ""
      const current = views.get(id)
      if (body?.expectedVersion !== undefined && current?.version !== body.expectedVersion) return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Someone changed this view first." } })
      const version = (current?.version ?? 0) + 1
      views.set(id, { version, payload: body?.payload, retired: false })
      return route.fulfill({ json: { installed: true, id, version } })
    }
    const retire = /^\/(.+):retire$/.exec(tail)
    if (retire !== null) {
      const id = decodeURIComponent(retire[1] ?? "")
      const current = views.get(id)
      if (current === undefined || current.retired || current.version !== body?.expectedVersion) return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Someone changed this view first." } })
      current.retired = true
      current.version += 1
      return route.fulfill({ json: { installed: true, id, version: current.version } })
    }
    const id = decodeURIComponent(tail.slice(1))
    const view = views.get(id)
    if (view === undefined || view.retired) return route.fulfill({ status: 404, json: { code: "definition.not_found", message: "No such definition." } })
    return route.fulfill({ json: { id, kind: "view", version: view.version, payload: view.payload } })
  })
  return { views, calls }
}
