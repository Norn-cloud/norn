import type { Field, Scalar } from "./catalog.ts"

/**
 * The object-records query contract Studio proposes to the cell (`GET /v1/objects/{type}`).
 *
 * Studio only builds the request and reads what the server says it applied. The cell validates
 * every clause against the concept's admitted properties and the reader's access, orders and
 * filters on the server, and refuses anything it cannot do. Studio never sorts or filters a page
 * itself and never treats a clause as applied unless the page echoes it back.
 *
 * Wire grammar (query string, besides the `orgId` and `environmentId` scope):
 *
 *   limit  = 1..200                          default 50
 *   sort   = key *("," key)                  at most 3 keys; the object id breaks ties, ascending
 *   key    = property ":" ("asc" | "desc")   missing values sort last in both directions
 *   filter = JSON Predicate (below)          every clause must hold (conjunction)
 *   after  = opaque cursor from the previous page's `next`; it binds sort, filter, scope and
 *            snapshot. Malformed or mismatched: 400 `query.cursor_invalid` at `/after`. Replayed
 *            after any commit: 409 `object.snapshot_moved`, and Studio restarts from page one.
 *
 *   Predicate = { "all": Clause[] }          1..8 clauses
 *   Clause    = { "property": id, "op": Op, "value"?: Value }
 *
 * Operators by declared kind (anything else is refused with `query.op_invalid`):
 *
 *   text                  eq ne in contains prefix
 *   integer, decimal      eq ne in lt lte gt gte between
 *   date, timestamp       eq ne lt lte gt gte between
 *   duration              eq ne lt lte gt gte between
 *   boolean               eq
 *   enum                  eq ne in            (values must be declared)
 *   every sortable kind   empty present       (no `value`)
 *   record, json-ext      not sortable or filterable
 *
 * Values are JSON strings except booleans. Decimals are exact decimal text ("12.500000") and
 * compare exactly; integers are decimal text without a point; dates are `YYYY-MM-DD`; timestamps
 * are RFC 3339 with an offset; durations are ISO 8601. `in` takes a 1..50 array; `between` takes
 * `[low, high]`, both inclusive.
 *
 * Values grants are per row. A withheld row (`withheld: true`, `values: null`) never matches a
 * clause and sorts last; a reader who may see only the population gets zero matches and
 * `total: 0` from any filter, not a refusal.
 *
 * Refusals are the host's `Refusal` with `diagnostics: [{ code, message, pointer }]`, where each
 * pointer is into the request: `/sort/1`, `/filter/all/0/value`, `/after`.
 *
 * The page is `NornObjectPage` plus `query` (the normalized sort and filter the server applied),
 * `next` (the cursor, or null at the end), `total` (`{ count, approximate }` or null) and per
 * record `withheld` (a boolean). A page without `query` was not sorted or filtered
 * on the server, so Studio shows server ordering and filtering as unavailable.
 */

export type Direction = "asc" | "desc"
export interface SortKey { readonly property: string; readonly direction: Direction }

export type Op = "eq" | "ne" | "in" | "contains" | "prefix" | "lt" | "lte" | "gt" | "gte" | "between" | "empty" | "present"

export type Clause =
  | { readonly property: string; readonly op: "eq" | "ne" | "contains" | "prefix" | "lt" | "lte" | "gt" | "gte"; readonly value: string | boolean }
  | { readonly property: string; readonly op: "in"; readonly value: readonly string[] }
  | { readonly property: string; readonly op: "between"; readonly value: readonly [string, string] }
  | { readonly property: string; readonly op: "empty" | "present" }

export interface Predicate { readonly all: readonly Clause[] }

export interface RecordsQuery {
  readonly sort: readonly SortKey[]
  readonly filter: Predicate | null
  readonly limit: number
  readonly after?: string
}

export const LIMITS = { pageSize: 50, maxPageSize: 200, sortKeys: 3, clauses: 8, inValues: 50 } as const

export const EMPTY_QUERY: RecordsQuery = { sort: [], filter: null, limit: LIMITS.pageSize }

const ORDERED = ["eq", "ne", "lt", "lte", "gt", "gte", "between"] as const
const PRESENCE = ["empty", "present"] as const

/** Which operators the contract offers for a property's declared kind; empty when it offers none. */
export function opsFor(scalar: Scalar): readonly Op[] {
  switch (scalar.kind) {
    case "text": return ["eq", "ne", "in", "contains", "prefix", ...PRESENCE]
    case "integer":
    case "decimal": return [...ORDERED.slice(0, 2), "in", ...ORDERED.slice(2), ...PRESENCE]
    case "date":
    case "timestamp":
    case "duration": return [...ORDERED, ...PRESENCE]
    case "boolean": return ["eq", ...PRESENCE]
    case "enum": return ["eq", "ne", "in", ...PRESENCE]
    default: return []
  }
}

export const sortable = (field: Field): boolean => opsFor(field.scalar).length > 0

/** The query string for one page, without scope; the scoped host fetch adds `orgId` and `environmentId`. */
export function recordsQueryParams(query: RecordsQuery): URLSearchParams {
  const params = new URLSearchParams({ limit: String(query.limit) })
  if (query.sort.length > 0) params.set("sort", query.sort.map(({ property, direction }) => `${property}:${direction}`).join(","))
  if (query.filter !== null && query.filter.all.length > 0) params.set("filter", JSON.stringify(query.filter))
  if (query.after !== undefined) params.set("after", query.after)
  return params
}

export const recordsPath = (objectType: string, query: RecordsQuery): string =>
  `/v1/objects/${encodeURIComponent(objectType)}?${recordsQueryParams(query)}`

/** What the server says it applied to a page, or null when the page carries no `query` echo. */
export interface Served { readonly sort: readonly SortKey[]; readonly filter: Predicate | null }

export function servedQuery(page: unknown): Served | null {
  const echo = (page as { query?: unknown } | null)?.query
  if (echo === null || typeof echo !== "object") return null
  const { sort, filter } = echo as { sort?: unknown; filter?: unknown }
  if (!Array.isArray(sort)) return null
  const keys = sort.filter((key): key is SortKey =>
    typeof key === "object" && key !== null && typeof (key as SortKey).property === "string" && ((key as SortKey).direction === "asc" || (key as SortKey).direction === "desc"))
  if (keys.length !== sort.length) return null
  const all = (filter as { all?: unknown } | null)?.all
  return { sort: keys, filter: Array.isArray(all) ? { all: all as Clause[] } : null }
}

/** Whether the server withheld this whole row from the reader. Only an explicit `true` is withheld; Studio never infers it. */
export const rowWithheld = (record: unknown): boolean => (record as { withheld?: unknown } | null)?.withheld === true

/** The server's count of matching records, or null when it sent none. */
export interface Total { readonly count: number; readonly approximate: boolean }

export function totalOf(page: unknown): Total | null {
  const total = (page as { total?: unknown } | null)?.total as { count?: unknown; approximate?: unknown } | null | undefined
  return typeof total?.count === "number" && Number.isSafeInteger(total.count) && total.count >= 0 && typeof total.approximate === "boolean"
    ? { count: total.count, approximate: total.approximate }
    : null
}

export const totalText = (total: Total): string =>
  `${total.approximate ? "About " : ""}${total.count.toLocaleString("en-US")} record${total.count === 1 ? "" : "s"}`

/** The refusal that means the data moved under a cursor: restart from page one and say so. */
export const SNAPSHOT_MOVED = "object.snapshot_moved"
export const DATA_CHANGED = "Data changed, reloaded."

export const snapshotMoved = (status: number, code: string | undefined): boolean => status === 409 && code === SNAPSHOT_MOVED

/** The cursor for the next page in this query, or undefined at the end. Falls back to the unsorted scan cursor. */
export function nextCursor(page: unknown): string | undefined {
  const doc = page as { next?: unknown; scannedThrough?: unknown } | null
  if (typeof doc?.next === "string") return doc.next
  return typeof doc?.scannedThrough === "string" ? doc.scannedThrough : undefined
}

export const SERVER_QUERY_UNAVAILABLE =
  "This cell doesn't sort or filter records on the server yet. Records show in the cell's own order."

/** Toggle one sort key: off → asc → desc → off; shift-adds a key instead of replacing (up to the limit). */
export function cycleSort(sort: readonly SortKey[], property: string, add: boolean): readonly SortKey[] {
  const current = sort.find((key) => key.property === property)
  const rest = sort.filter((key) => key.property !== property)
  const next: SortKey | null = current === undefined ? { property, direction: "asc" } : current.direction === "asc" ? { property, direction: "desc" } : null
  if (!add) return next === null ? [] : [next]
  if (next === null) return rest
  return current === undefined ? [...sort, next].slice(0, LIMITS.sortKeys) : sort.map((key) => key.property === property ? next : key)
}
