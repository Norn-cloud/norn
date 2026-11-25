/** Candidate questions lower to the existing QuestionIR; Rust admission is required. */
import { toJson, type Json } from "../canonical.ts"

export interface QuestionFilter {
  /** METRIC-GRAIN-LINK-1: the to-one links walked to `property` (empty: a direct property). */
  readonly links?: ReadonlyArray<string>
  readonly property: string
  readonly operator: "eq" | "ne"
  readonly value: string | number | boolean
}
export interface QuestionDimension {
  /** METRIC-GRAIN-LINK-1: the to-one links walked to `property` (empty: a direct property). */
  readonly links?: ReadonlyArray<string>
  readonly property: string
  readonly grain?: "day" | "week" | "month" | "quarter" | "year"
}
export type QuestionFreshness =
  | { readonly kind: "live"; readonly maxAgeMs: number }
  | { readonly kind: "delayed"; readonly maxAgeMs: number }
export interface QuestionSpec {
  readonly name: string
  readonly of: string
  readonly where: ReadonlyArray<QuestionFilter>
  /** Exact admitted metric identity, including an ontology-derived candidate. */
  readonly measure: string
  readonly by: ReadonlyArray<QuestionDimension>
  readonly asOf: {
    readonly basis: "effective"
    readonly from: string
    readonly to: string
    readonly freshness: QuestionFreshness
  }
  /** A declaration of intent; the host still resolves and authorizes this population. */
  readonly population: { readonly kind: "authorized_objects" }
}

/** A QuestionIR `DimensionPath`: `links` only when the path walks one. */
function path(target: { readonly links?: ReadonlyArray<string>; readonly property: string }): Json {
  return target.links?.length ? toJson({ links: [...target.links], property: target.property }) : toJson({ property: target.property })
}

/** QUESTION-LOWER-1: no callback, alternate calculation or execution authority is emitted. */
export function lowerQuestion(question: QuestionSpec): Json {
  return toJson({
    irVersion: "0.1", name: question.name, objectSet: question.of,
    aggregate: [{ metric: question.measure }],
    ...(question.where.length ? { filter: question.where.map(filter => ({
      path: path(filter), operator: filter.operator, value: filter.value,
    })) } : {}),
    ...(question.by.length ? { groupBy: question.by.map(dimension => ({
      path: path(dimension), ...(dimension.grain ? { grain: dimension.grain } : {}),
    })) } : {}),
    window: { start: question.asOf.from, end: question.asOf.to },
    calendar: "iso_utc", freshness: question.asOf.freshness,
  })
}

/**
 * Which ontology-derived metric (METRIC-DERIVE-1) a question measures:
 * `sum` of an additive property by a time (direct, or `links` + property one
 * hop away), `as_of` a semi-additive property's snapshot at its own time, or
 * the `ratio` of sums a ratio property declares.
 */
export interface MetricIdSpec {
  readonly kind: "sum" | "as_of" | "ratio"
  /** The measured object type's name. */
  readonly of: string
  readonly property: string
  readonly time: { readonly links?: ReadonlyArray<string>; readonly property: string }
}

const UTF8 = new TextEncoder()

/** `norn_checker::metric::derive::encoded_name`: the type name's UTF-8 bytes, lowercase hex. */
function encodedName(name: string): string {
  return [...UTF8.encode(name)].map(byte => byte.toString(16).padStart(2, "0")).join("")
}

/** `norn_checker::metric::field_name`: a dimension path's MetricFlow-safe name. */
function fieldName(time: MetricIdSpec["time"]): string {
  const links = time.links ?? []
  if (links.length === 0 && time.property.length >= 2 && !time.property.endsWith("_") && !time.property.includes("__")) return time.property
  const encoded = [...links, time.property].map(component => `${component.length}_${component.replaceAll("_", "_u")}`).join("_")
  return `norn_path_${encoded}_value`
}

/**
 * The deterministic id `norn_checker::metric::derive` gives the derived metric
 * `spec` names, so an author never spells `analysis.object_<hex>...` by hand.
 * The cell still admits the question against the metric space it derives; an
 * id naming no derived candidate refuses there.
 */
export function derivedMetricId(spec: MetricIdSpec): string {
  const object = `analysis.object_${encodedName(spec.of)}.${spec.property}`
  if (spec.kind === "as_of") {
    if (spec.time.links?.length) throw new Error("METRIC-SNAPSHOT-ASOF-1: a snapshot keeps its own valid time; it has no linked time")
    return `${object}.as_of_${spec.time.property}`
  }
  const sum = `${object}.by_${spec.time.links?.length ? fieldName(spec.time) : spec.time.property}`
  return spec.kind === "ratio" ? `${sum}.ratio_of_sums` : sum
}
