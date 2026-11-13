import { cellRead } from "./live-transport.ts"
import type { ObjectProfile } from "./profile.ts"

/** One metric the cell derives from its admitted types (`GET /v1/questions:metrics`). */
export interface MetricSummary {
  readonly name: string
  readonly label: string
  readonly kind: string
  readonly objectType: string
  readonly expression: string | null
  readonly timeProperty: string | null
  /** The to-one links from the measured object to the one holding `timeProperty` (a posting's time is its event's); empty or absent when the time is the object's own. */
  readonly timeLinks?: readonly string[]
  readonly numerator: string | null
  readonly denominator: string | null
  /** The properties a question summing this metric must fix to one value each (mixed units are refused), when the cell names them. */
  readonly unitPartition?: readonly string[]
  /** How its values may be added up, when the cell says (the metric's declared additivity). */
  readonly additivity?: Additivity
}

/** A metric's declared additivity: summed anywhere, reduced along time, or computed per bucket from its records. */
export type Additivity =
  | { readonly kind: "additive" }
  | { readonly kind: "semi_additive"; readonly over: "time"; readonly aggregate: "first" | "last" | "min" | "max" }
  | { readonly kind: "non_additive" }

/**
 * How the metric's values combine, from its declared additivity, or, on a cell that does not send
 * it yet, from the label the metric deriver writes (`sum(p); …` sums; `p, as of t; last value …`
 * takes each period's last value). Undefined when neither says.
 */
export function additivityOf(metric: Pick<MetricSummary, "additivity" | "label">): Additivity | undefined {
  if (metric.additivity) return metric.additivity
  if (/^sum\(/.test(metric.label) && !metric.label.includes(" / ")) return { kind: "additive" }
  if (/, as of [^;]+; last value/.test(metric.label)) return { kind: "semi_additive", over: "time", aggregate: "last" }
  return undefined
}

const REDUCTION: Record<"first" | "last" | "min" | "max", (grain: string) => string> = {
  first: (grain) => `start of ${grain}`,
  last: (grain) => `end of ${grain}`,
  min: (grain) => `lowest in the ${grain}`,
  max: (grain) => `highest in the ${grain}`,
}

/**
 * A metric card's heading in plain words: "Total quantity" only for a sum the cell may add across
 * time; "Quantity, end of week" for a balance read at each period's end; the property alone when
 * the cell does not say how its values combine.
 */
/** The time a metric buckets by, in words from its definition: "occurred at", or "event's occurred at" through a link. */
export function timeLabel(metric: Pick<MetricSummary, "timeProperty" | "timeLinks">): string {
  if (metric.timeProperty === null) return ""
  return [...(metric.timeLinks ?? []).map((link) => `${link.replaceAll("_", " ")}'s`), metric.timeProperty.replaceAll("_", " ")].join(" ")
}

export function metricHeading(metric: Pick<MetricSummary, "additivity" | "label" | "expression" | "kind">, grain = "period"): string {
  const property = (metric.expression ?? "").replaceAll("_", " ")
  if (metric.kind !== "simple" || property === "") return metric.label
  const additivity = additivityOf(metric)
  const word = property.charAt(0).toUpperCase() + property.slice(1)
  if (additivity?.kind === "additive") return `Total ${property}`
  if (additivity?.kind === "semi_additive") return `${word}, ${REDUCTION[additivity.aggregate](grain)}`
  return word
}

/** The metrics this reader may ask about; a cell without the route says so. */
export const readMetrics = () => cellRead<{ readonly metrics: readonly MetricSummary[] }>("/v1/questions:metrics", "Questions are")

export interface Window { readonly start: string; readonly end: string; readonly grain: "day" | "week" }

const DAY = 86_400_000
const dayOf = (text: string): number | undefined => {
  const at = Date.parse(text.length === 10 ? `${text}T00:00:00Z` : text)
  return Number.isNaN(at) ? undefined : Math.floor(at / DAY) * DAY
}
const iso = (ms: number) => new Date(ms).toISOString().replace(".000Z", "Z")

/**
 * The window that covers every value the profile saw of `property`, by ISO week: from the Monday
 * of its first value to the Monday after its last. `undefined` when the profile has no range for it.
 */
export function windowOf(profile: ObjectProfile, property: string): Window | undefined {
  const profiled = profile.properties.find((candidate) => candidate.property === property)
  const first = typeof profiled?.min === "string" ? dayOf(profiled.min) : undefined
  const last = typeof profiled?.max === "string" ? dayOf(profiled.max) : undefined
  if (first === undefined || last === undefined) return undefined
  // The bounded engine groups dates by ISO week today; day grain follows (L-port), and then short spans go by day.
  // 1970-01-01 was a Thursday: days since the epoch plus 3, mod 7, counts days since Monday.
  const monday = (at: number) => at - (((at / DAY + 3) % 7) * DAY)
  return { start: iso(monday(first)), end: iso(monday(last) + 7 * DAY), grain: "week" }
}

/** One "only where" condition: a property equal to a value, or empty (`null`). */
export interface Condition { readonly property: string; readonly value: string | null }

/** The question asking `metric` per `grain` of its time property over `window`, as the cell's evaluate route takes it. */
export const metricQuestion = (metric: MetricSummary, window: Window, conditions: readonly Condition[] = []) => ({
  irVersion: "0.1",
  name: "studio.metric",
  objectSet: metric.objectType,
  aggregate: [{ metric: metric.name }],
  ...metric.timeProperty === null ? {} : { groupBy: [{ path: { ...(metric.timeLinks ?? []).length > 0 ? { links: metric.timeLinks } : {}, property: metric.timeProperty }, grain: window.grain }] },
  ...conditions.length === 0 ? {} : { filter: conditions.map(({ property, value }) => ({ path: { property }, operator: "eq", value })) },
  window: { start: window.start, end: window.end },
  freshness: { kind: "live", maxAgeMs: 60_000 },
  calendar: "iso_utc",
})
