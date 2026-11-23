import type { Catalog, Concept, Relationship } from "./catalog.ts"
import type { Clause, Predicate, SortKey } from "./records-query.ts"

/**
 * The Timeline view of an object type's records: each record a bar from its `starts_at` to its
 * `ends_at`, the contract `core.TimeInterval` declares (#1584). Only a type that implements that
 * interface, directly or through its parents, offers the view. Which records fall in the window is
 * the cell's: the view asks for them with the records query's own timestamp clauses and places
 * only what the page returns. Nothing here decides anything about a record.
 */

export const TIME_INTERVAL = "core.TimeInterval"
export const STARTS_AT = "starts_at"
export const ENDS_AT = "ends_at"
export const TIMEZONE = "timezone"

/** Whether `name` implements or extends `ancestor`, through any chain of declared parents. */
export function descendsFrom(catalog: Catalog, name: string, ancestor: string): boolean {
  const seen = new Set<string>()
  const pending = [...catalog.byName.get(name)?.parents ?? []]
  while (pending.length > 0) {
    const next = pending.pop()!
    if (next === ancestor) return true
    if (seen.has(next)) continue
    seen.add(next)
    pending.push(...catalog.byName.get(next)?.parents ?? [])
  }
  return false
}

/** Whether the Timeline view is offered: an object type that is a `core.TimeInterval`. */
export const offersTimeline = (catalog: Catalog, concept: Concept): boolean =>
  concept.definition === "object type" && descendsFrom(catalog, concept.name, TIME_INTERVAL)

/** The links a lane can follow: each declared to-one link (one target per record at most). */
export const laneLinks = (concept: Concept): readonly Relationship[] => concept.relationships.filter(({ count }) => count !== "many")

export type Zoom = "day" | "week" | "month"
export const ZOOMS: readonly Zoom[] = ["day", "week", "month"]

/** A half-open window `[start, end)` in epoch milliseconds. */
export interface Span { readonly start: number; readonly end: number }

/** The window of `zoom` that holds `anchor`, in the reader's zone: its day, its Monday-first week, or its month. */
export function windowOf(zoom: Zoom, anchor: Date): Span {
  const y = anchor.getFullYear(), m = anchor.getMonth(), d = anchor.getDate()
  if (zoom === "day") return { start: new Date(y, m, d).getTime(), end: new Date(y, m, d + 1).getTime() }
  if (zoom === "week") {
    const monday = d - ((anchor.getDay() + 6) % 7)
    return { start: new Date(y, m, monday).getTime(), end: new Date(y, m, monday + 7).getTime() }
  }
  return { start: new Date(y, m, 1).getTime(), end: new Date(y, m + 1, 1).getTime() }
}

/** The anchor `by` windows of `zoom` away (a calendar step, so a DST day is still one day). */
export function stepAnchor(zoom: Zoom, anchor: Date, by: number): Date {
  const y = anchor.getFullYear(), m = anchor.getMonth(), d = anchor.getDate()
  if (zoom === "day") return new Date(y, m, d + by)
  if (zoom === "week") return new Date(y, m, d + 7 * by)
  return new Date(y, m + by, 1)
}

/** One axis mark: where it falls, its words, and whether it starts a day. */
export interface Tick { readonly at: number; readonly label: string; readonly day: boolean }

/** The axis of a window: hours over a day, days over a week or a month, labelled in `locale`. */
export function ticksOf(zoom: Zoom, window: Span, locale?: string): readonly Tick[] {
  const ticks: Tick[] = []
  const start = new Date(window.start)
  if (zoom === "day") {
    for (let h = 0; ; h += 1) {
      const at = new Date(start.getFullYear(), start.getMonth(), start.getDate(), h).getTime()
      if (at >= window.end) break
      ticks.push({ at, label: new Date(at).toLocaleTimeString(locale, { hour: "numeric" }), day: h === 0 })
    }
    return ticks
  }
  for (let i = 0; ; i += 1) {
    const at = new Date(start.getFullYear(), start.getMonth(), start.getDate() + i).getTime()
    if (at >= window.end) break
    const label = zoom === "week"
      ? new Date(at).toLocaleDateString(locale, { weekday: "short", day: "numeric" })
      : new Date(at).toLocaleDateString(locale, { day: "numeric" })
    ticks.push({ at, label, day: true })
  }
  return ticks
}

/**
 * The records query for a window: the reader's own filter, and the overlap test `starts_at <
 * end && ends_at > start` (a half-open interval meets the window) as two clauses the cell
 * applies, in start order. Timestamps go as RFC 3339 UTC, as the query contract takes them.
 */
export function windowQuery(window: Span, filter: Predicate | null): { readonly filter: Predicate; readonly sort: readonly SortKey[] } {
  const clauses: Clause[] = [
    { property: STARTS_AT, op: "lt", value: new Date(window.end).toISOString() },
    { property: ENDS_AT, op: "gt", value: new Date(window.start).toISOString() },
  ]
  return { filter: { all: [...filter?.all ?? [], ...clauses] }, sort: [{ property: STARTS_AT, direction: "asc" }] }
}

/** The nearest records outside a window, for the empty state's "jump": the first after it, or the last before it. */
export function nearestQuery(window: Span, filter: Predicate | null, direction: "after" | "before"): { readonly filter: Predicate; readonly sort: readonly SortKey[] } {
  const clause: Clause = direction === "after"
    ? { property: STARTS_AT, op: "gte", value: new Date(window.end).toISOString() }
    : { property: ENDS_AT, op: "lte", value: new Date(window.start).toISOString() }
  return { filter: { all: [...filter?.all ?? [], clause] }, sort: [{ property: STARTS_AT, direction: direction === "after" ? "asc" : "desc" }] }
}

const DATE_ONLY = /^\d{4}-\d{2}-\d{2}$/

/** One stored instant in epoch ms: RFC 3339 with its offset, or a calendar date as the reader's midnight. Null when unreadable. */
export function instantOf(value: unknown): number | null {
  if (typeof value !== "string" || value === "") return null
  if (DATE_ONLY.test(value)) { const [y, m, d] = value.split("-").map(Number); return new Date(y!, m! - 1, d!).getTime() }
  const at = Date.parse(value)
  return Number.isNaN(at) ? null : at
}

/** Whether the runtime knows `zone` as an IANA time zone. */
export function knownZone(zone: string): boolean {
  try { new Intl.DateTimeFormat(undefined, { timeZone: zone }); return true } catch { return false }
}

/** A record's own zone, when it declares one the runtime knows; otherwise times read in the reader's zone. */
export function zoneOf(values: Readonly<Record<string, unknown>> | null | undefined): string | null {
  const zone = values?.[TIMEZONE]
  return typeof zone === "string" && zone !== "" && knownZone(zone) ? zone : null
}

/** "Mon 23 Nov, 09:00": an instant in `zone` (the reader's when null). */
export const timeText = (at: number, zone: string | null, locale?: string): string =>
  new Date(at).toLocaleString(locale, { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", ...zone === null ? {} : { timeZone: zone } })

/** A record placed on the timeline: its span, which sub-row of its lane it takes, and whether it shares time with another. */
export interface Placed<T> {
  readonly item: T
  readonly start: number
  readonly end: number
  readonly row: number
  readonly overlaps: boolean
}

/**
 * Stack a lane's spans into the fewest sub-rows where none share time (interval partitioning, in
 * start order: each span takes the first row free by its start). A span that meets another
 * (`a.start < b.end && b.start < a.end`) is marked; touching ends do not meet.
 */
export function stack<T>(spans: readonly { readonly item: T; readonly start: number; readonly end: number }[]): { readonly placed: readonly Placed<T>[]; readonly rows: number } {
  const order = [...spans].sort((a, b) => a.start - b.start || a.end - b.end)
  const rowEnds: number[] = []
  const placed = order.map((span) => {
    let row = rowEnds.findIndex((end) => end <= span.start)
    if (row < 0) { row = rowEnds.length; rowEnds.push(span.end) } else rowEnds[row] = span.end
    return { ...span, row }
  })
  return {
    placed: placed.map((span) => ({ ...span, overlaps: placed.some((other) => other !== span && span.start < other.end && other.start < span.end) })),
    rows: rowEnds.length,
  }
}

/** Where a span sits in a window, as percentages of its width, clipped to the window. */
export function geometry(span: Span, window: Span): { readonly left: number; readonly width: number; readonly clippedStart: boolean; readonly clippedEnd: boolean } {
  const total = window.end - window.start
  const start = Math.max(span.start, window.start), end = Math.min(span.end, window.end)
  return { left: ((start - window.start) / total) * 100, width: Math.max(0, ((end - start) / total) * 100), clippedStart: span.start < window.start, clippedEnd: span.end > window.end }
}

/** The heading of a window: "Monday 23 November 2026", "23 – 29 Nov 2026" or "November 2026". */
export function windowTitle(zoom: Zoom, window: Span, locale?: string): string {
  const start = new Date(window.start)
  if (zoom === "day") return start.toLocaleDateString(locale, { weekday: "long", day: "numeric", month: "long", year: "numeric" })
  if (zoom === "month") return start.toLocaleDateString(locale, { month: "long", year: "numeric" })
  const last = new Date(window.end - 1)
  return `${start.toLocaleDateString(locale, { day: "numeric", month: "short" })} – ${last.toLocaleDateString(locale, { day: "numeric", month: "short", year: "numeric" })}`
}
