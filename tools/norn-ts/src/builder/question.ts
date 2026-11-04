/** Candidate questions lower to the existing QuestionIR; Rust admission is required. */
import { toJson, type Json } from "../canonical.ts"

export interface QuestionFilter {
  readonly property: string
  readonly operator: "eq" | "ne"
  readonly value: string | number | boolean
}
export interface QuestionDimension {
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

/** QUESTION-LOWER-1: no callback, alternate calculation or execution authority is emitted. */
export function lowerQuestion(question: QuestionSpec): Json {
  return toJson({
    irVersion: "0.1", name: question.name, objectSet: question.of,
    aggregate: [{ metric: question.measure }],
    ...(question.where.length ? { filter: question.where.map(filter => ({
      path: { property: filter.property }, operator: filter.operator, value: filter.value,
    })) } : {}),
    ...(question.by.length ? { groupBy: question.by.map(dimension => ({
      path: { property: dimension.property }, ...(dimension.grain ? { grain: dimension.grain } : {}),
    })) } : {}),
    window: { start: question.asOf.from, end: question.asOf.to },
    calendar: "iso_utc", freshness: question.asOf.freshness,
  })
}
