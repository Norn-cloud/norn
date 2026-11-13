import { cellRead } from "./live-transport.ts"
import type { Clause, Predicate } from "./records-query.ts"

/** One value of a property and the counted rows holding it (`GET /v1/objects/{type}:facets`). */
export interface FacetValue { readonly value: string | boolean; readonly count: number }

export interface Facet {
  readonly property: string
  /** Most rows first, as the cell ordered them. */
  readonly values: readonly FacetValue[]
  /** Counted rows holding a value the cell did not list. */
  readonly otherRows: number
  readonly nulls: number
}

/** The cell's exact value counts over the rows a grid page with the same filter lists. */
export interface Facets {
  readonly rows: number
  readonly withheldRows: number
  /** The scan bound was reached: every count is exact over the rows scanned, so at most the true count ("≥ n"). */
  readonly lowerBound: boolean
  readonly facets: readonly Facet[]
}

/** The cell's facets for `filter`, of every categorical property or of `properties` only. */
export async function readFacets(objectType: string, filter: Predicate | null, properties?: readonly string[]) {
  const query = new URLSearchParams()
  if (filter !== null && filter.all.length > 0) query.set("filter", JSON.stringify(filter))
  if (properties !== undefined) query.set("properties", properties.join(","))
  const search = query.toString()
  const read = await cellRead<Facets>(`/v1/objects/${encodeURIComponent(objectType)}:facets${search === "" ? "" : `?${search}`}`, "Facets are")
  // An answer that is not a facets document is not counted from: it reads as not served.
  if (!isFacets(read.document)) throw new Error("The cell's answer is not a facets document, so nothing is counted here.")
  return read
}

/** Whether `value` has the facets document's shape: counts, a flag and one facet per property. */
export function isFacets(value: unknown): value is Facets {
  const document = value as Partial<Facets> | null
  return typeof document?.rows === "number" && typeof document.lowerBound === "boolean" && Array.isArray(document.facets)
    && document.facets.every((facet) => typeof facet?.property === "string" && Array.isArray(facet.values) && typeof facet.otherRows === "number" && typeof facet.nulls === "number")
}

/** A facet count as the cell gave it: exact, or "≥ n" past the scan bound. */
export const countText = (count: number, lowerBound: boolean): string => `${lowerBound ? "≥ " : ""}${count.toLocaleString("en")}`

/** The values a filter holds `property` to with `eq` or `in`. */
export function chosen(filter: Predicate | null, property: string): readonly (string | boolean)[] {
  const clause = filter?.all.find((candidate) => candidate.property === property && (candidate.op === "eq" || candidate.op === "in"))
  if (clause === undefined || !("value" in clause)) return []
  return Array.isArray(clause.value) ? clause.value as readonly string[] : [clause.value as string | boolean]
}

/** The filter without its `eq`/`in` clause on `property`: what that property's own facet counts under. */
export function without(filter: Predicate | null, property: string): Predicate | null {
  const rest = (filter?.all ?? []).filter((clause) => !(clause.property === property && (clause.op === "eq" || clause.op === "in")))
  return rest.length === 0 ? null : { all: rest }
}

/**
 * The filter with `value` of `property` chosen or, if it was, no longer: one value is `eq`, several
 * text values are `in`; a yes/no value replaces the other. Other clauses stay as they are.
 */
export function toggled(filter: Predicate | null, property: string, value: string | boolean): Predicate | null {
  const current = chosen(filter, property)
  const next = current.includes(value) ? current.filter((held) => held !== value)
    : typeof value === "boolean" ? [value] : [...current, value]
  const others = without(filter, property)?.all ?? []
  const own: Clause[] = next.length === 0 ? []
    : next.length === 1 ? [{ property, op: "eq", value: next[0] as string | boolean }]
    : [{ property, op: "in", value: next.map(String) }]
  const all = [...others, ...own]
  return all.length === 0 ? null : { all }
}
