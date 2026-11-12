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
  readonly numerator: string | null
  readonly denominator: string | null
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

/** The question asking `metric` per `grain` of its time property over `window`, as the cell's evaluate route takes it. */
export const metricQuestion = (metric: MetricSummary, window: Window) => ({
  irVersion: "0.1",
  name: "studio.metric",
  objectSet: metric.objectType,
  aggregate: [{ metric: metric.name }],
  ...metric.timeProperty === null ? {} : { groupBy: [{ path: { property: metric.timeProperty }, grain: window.grain }] },
  window: { start: window.start, end: window.end },
  freshness: { kind: "live", maxAgeMs: 60_000 },
  calendar: "iso_utc",
})
