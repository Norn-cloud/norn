import type { Catalog, Concept, Relationship } from "./catalog.ts"
import type { Locale } from "./locale.tsx"
import { LIMITS, type Clause, type Predicate, type SortKey } from "./records-query.ts"

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

/** The cell caps every filter at `LIMITS.clauses` (8); the Timeline window needs 2 of them. */
export const TIMELINE_MAX_USER_CLAUSES = LIMITS.clauses - 2
/** Nearest-record queries add 1 timestamp clause. */
export const NEAREST_MAX_USER_CLAUSES = LIMITS.clauses - 1

const HOUR_MS = 3_600_000

/**
 * The BCP 47 tag for `Intl` date/time formatting under Studio's active locale: keeps the browser's
 * regional variant when its language matches Studio's active language (e.g. `en-GB` for `en`), and
 * switches to Studio's active tag (`ar` or `en`) when they differ.
 */
export function formatLocale(locale: Locale, browserLocale: string | undefined = globalThis.navigator?.language): string {
  if (browserLocale !== undefined && browserLocale.toLowerCase().split("-")[0] === locale) return browserLocale
  return locale
}

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

/**
 * The axis of a window: epoch hours over a day (so a 23-hour spring DST day emits no duplicate
 * tick, a 25-hour fall DST day includes both occurrences of the repeated hour, and half-hour DST
 * shifts like `Australia/Lord_Howe` include minutes when non-zero), and Gregorian calendar days
 * over a week or month, labelled in `locale`.
 */
export function ticksOf(zoom: Zoom, window: Span, locale?: string, timeZone?: string): readonly Tick[] {
  const ticks: Tick[] = []
  const tz = timeZone === undefined ? {} : { timeZone }
  if (zoom === "day") {
    for (let at = window.start; at < window.end; at += HOUR_MS) {
      const dt = new Date(at)
      const minute = timeZone === undefined
        ? dt.getMinutes()
        : Number(new Intl.DateTimeFormat("en-US", { minute: "numeric", timeZone }).format(dt))
      const label = dt.toLocaleTimeString(locale, {
        calendar: "gregory",
        hour: "numeric",
        ...minute !== 0 ? { minute: "2-digit" } : {},
        ...tz,
      })
      ticks.push({ at, label, day: at === window.start })
    }
    return ticks
  }
  const start = new Date(window.start)
  for (let i = 0; ; i += 1) {
    const at = new Date(start.getFullYear(), start.getMonth(), start.getDate() + i).getTime()
    if (at >= window.end) break
    if (ticks.at(-1)?.at === at) continue
    const label = zoom === "week"
      ? new Date(at).toLocaleDateString(locale, { calendar: "gregory", weekday: "short", day: "numeric", ...tz })
      : new Date(at).toLocaleDateString(locale, { calendar: "gregory", day: "numeric", ...tz })
    ticks.push({ at, label, day: true })
  }
  return ticks
}

/** How many clauses `filter` exceeds the Timeline window's user-clause budget by (`0` when it fits). */
export const timelineFilterOverflow = (filter: Predicate | null): number =>
  Math.max(0, (filter?.all.length ?? 0) - TIMELINE_MAX_USER_CLAUSES)

export type WindowQueryResult =
  | { readonly kind: "ok"; readonly filter: Predicate; readonly sort: readonly SortKey[] }
  | { readonly kind: "too_many"; readonly overflow: number; readonly maxUserClauses: number; readonly actual: number }

/**
 * The records query for a window, guarded against the cell's `LIMITS.clauses` (8) cap: returns
 * `{ kind: "too_many" }` when the user filter already has more than 6 clauses, so Studio never
 * emits an invalid 9+ clause filter or silently drops user conditions.
 */
export function tryWindowQuery(window: Span, filter: Predicate | null): WindowQueryResult {
  const actual = filter?.all.length ?? 0
  if (actual > TIMELINE_MAX_USER_CLAUSES) {
    return { kind: "too_many", overflow: actual - TIMELINE_MAX_USER_CLAUSES, maxUserClauses: TIMELINE_MAX_USER_CLAUSES, actual }
  }
  return { kind: "ok", ...windowQuery(window, filter) }
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

/**
 * The nearest records outside a window, for the empty state's "jump":
 * - `after`: the earliest interval starting at or after `window.end` (`starts_at:asc`),
 * - `before`: the latest interval ending at or before `window.start` (`ends_at:desc`), so a long
 *   interval ending just before the window wins over a shorter interval that started later but
 *   ended days earlier.
 */
export function nearestQuery(window: Span, filter: Predicate | null, direction: "after" | "before"): { readonly filter: Predicate; readonly sort: readonly SortKey[] } {
  const clause: Clause = direction === "after"
    ? { property: STARTS_AT, op: "gte", value: new Date(window.end).toISOString() }
    : { property: ENDS_AT, op: "lte", value: new Date(window.start).toISOString() }
  const sortProperty = direction === "after" ? STARTS_AT : ENDS_AT
  return { filter: { all: [...filter?.all ?? [], clause] }, sort: [{ property: sortProperty, direction: direction === "after" ? "asc" : "desc" }] }
}

/**
 * The display timestamp (`at`) and jump anchor (`jumpAt`) for the nearest record outside a window.
 * For `before`, jumps to `end - 1` (clamped to `>= start`) so the interval's end is guaranteed to
 * fall inside the destination half-open window even when `end === window.start`.
 */
export function nearestJumpTarget(
  values: Readonly<Record<string, unknown>> | null | undefined,
  direction: "after" | "before",
): { readonly at: number; readonly jumpAt: number } | null {
  if (values === null || values === undefined) return null
  const start = instantOf(values[STARTS_AT])
  const end = instantOf(values[ENDS_AT])
  if (direction === "after") return start === null ? null : { at: start, jumpAt: start }
  if (end === null) return start === null ? null : { at: start, jumpAt: start }
  const jumpAt = start !== null ? Math.max(start, end - 1) : end - 1
  return { at: end, jumpAt }
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

/** "Mon 23 Nov, 09:00": an instant in `zone` (the reader's when null), always on the Gregorian calendar. */
export const timeText = (at: number, zone: string | null, locale?: string): string =>
  new Date(at).toLocaleString(locale, { calendar: "gregory", weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", ...zone === null ? {} : { timeZone: zone } })

/** "09:00": the clock time of an instant in `zone`, formatted cleanly in any locale (including Arabic). */
export const clockText = (at: number, zone: string | null, locale?: string): string =>
  new Date(at).toLocaleTimeString(locale, { calendar: "gregory", hour: "2-digit", minute: "2-digit", ...zone === null ? {} : { timeZone: zone } })

/** A record placed on the timeline: its span, which sub-row of its lane it takes, and whether it shares time with another. */
export interface Placed<T> {
  readonly item: T
  readonly start: number
  readonly end: number
  readonly row: number
  readonly overlaps: boolean
}

/**
 * What the cell's link lookup resolved for one record on a chosen lane link:
 * - `target`: a resolved target id (records sharing this id genuinely share a resource),
 * - `pending`: the link lookup is still in flight,
 * - `unavailable`: the link lookup failed or was refused/unserved,
 * - `none`: the cell returned no visible link (either unlinked, or target hidden by access policy).
 */
export type LaneTarget =
  | { readonly kind: "target"; readonly id: string }
  | { readonly kind: "pending" }
  | { readonly kind: "unavailable" }
  | { readonly kind: "none" }

export type LaneKind = "ungrouped" | "target" | "pending" | "unavailable" | "none"

/** One timeline lane after grouping and interval stacking. */
export interface TimelineLane<T> {
  readonly key: string
  readonly kind: LaneKind
  readonly label: string
  readonly rows: number
  readonly placed: readonly Placed<T>[]
  /** Count of overlapping bars; always `0` for synthetic buckets (`pending`, `unavailable`, `none`) and ungrouped views. */
  readonly overlapping: number
}

/**
 * Group spans into timeline lanes and stack each lane's spans into non-colliding sub-rows.
 * Pending, unavailable, and unlinked/hidden records stay in separate buckets (never conflated into
 * one "No link" lane), and overlap warnings are raised only on resolved `target` lanes.
 */
export function groupLanes<T>(
  spans: readonly { readonly item: T; readonly start: number; readonly end: number }[],
  targets: readonly LaneTarget[],
  options: { readonly lane: string | null; readonly conceptTitle: string; readonly linkName?: string },
): readonly TimelineLane<T>[] {
  type Bucket = { kind: LaneKind; label: string; spans: { readonly item: T; readonly start: number; readonly end: number }[] }
  const groups = new Map<string, Bucket>()
  const linkLabel = (options.linkName ?? options.lane ?? "link").replaceAll("_", " ")
  spans.forEach((span, index) => {
    let key = ""
    let kind: LaneKind = "ungrouped"
    let label = options.conceptTitle
    if (options.lane !== null) {
      const target = targets[index] ?? { kind: "pending" as const }
      if (target.kind === "pending") { key = "\u00001:reading"; kind = "pending"; label = "Reading links…" }
      else if (target.kind === "unavailable") { key = "\u00002:unavailable"; kind = "unavailable"; label = "Link unavailable" }
      else if (target.kind === "none") { key = "\u00003:none"; kind = "none"; label = `No visible ${linkLabel}` }
      else { key = target.id; kind = "target"; label = target.id }
    }
    const group = groups.get(key) ?? { kind, label, spans: [] }
    groups.set(key, { kind, label, spans: [...group.spans, span] })
  })
  return [...groups]
    .sort(([a], [b]) => (a.startsWith("\u0000") ? 1 : 0) - (b.startsWith("\u0000") ? 1 : 0) || a.localeCompare(b))
    .map(([key, group]) => {
      const { placed, rows: height } = stack(group.spans)
      const markOverlaps = group.kind === "target"
      const finalPlaced = markOverlaps ? placed : placed.map((entry) => ({ ...entry, overlaps: false }))
      return {
        key, kind: group.kind, label: group.label, rows: Math.max(1, height),
        placed: finalPlaced, overlapping: markOverlaps ? finalPlaced.filter(({ overlaps }) => overlaps).length : 0,
      }
    })
}

/** Clamp a roving-focus index into `[0, length - 1]` (or `0` when empty). */
export const clampFocus = (focused: number, length: number): number =>
  length <= 0 ? 0 : Math.max(0, Math.min(length - 1, focused))

/**
 * Decide which bar index should hold roving focus after `orderIds` changes (e.g. when a link
 * resolves and remounts the focused bar into another lane, or when the focused bar disappears),
 * and whether DOM `.focus()` should be restored to that bar's current DOM button without stealing
 * focus from unrelated controls outside the timeline.
 */
export function resolveBarFocus<E extends { readonly focus: () => void }>(params: {
  readonly orderIds: readonly string[]
  readonly focusedIndex: number
  readonly focusedId: string | null
  readonly wasInside: boolean
  readonly activeElement: unknown
  readonly bodyElement: unknown
  readonly activeInsideGroup: boolean
  readonly buttonFor: (id: string) => E | undefined
}): { readonly index: number; readonly id: string | null; readonly focusTarget: E | null } {
  const { orderIds, focusedIndex, focusedId, wasInside, activeElement, bodyElement, activeInsideGroup, buttonFor } = params
  if (orderIds.length === 0) return { index: 0, id: null, focusTarget: null }
  const stillPresent = focusedId === null ? -1 : orderIds.indexOf(focusedId)
  const index = stillPresent >= 0 ? stillPresent : clampFocus(focusedIndex, orderIds.length)
  const id = orderIds[index] ?? null
  const button = id === null ? undefined : buttonFor(id)
  const inside = wasInside || activeInsideGroup
  const lostToRemountOrRemoval = activeElement === null || activeElement === bodyElement || activeInsideGroup
  const focusTarget = inside && lostToRemountOrRemoval && button !== undefined && activeElement !== button ? button : null
  return { index, id, focusTarget }
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

/** Where a span sits in a window, as percentages of its width from the window's start edge (inline-start, so it mirrors in RTL), clipped to the window. */
export function geometry(span: Span, window: Span): { readonly offset: number; readonly width: number; readonly clippedStart: boolean; readonly clippedEnd: boolean } {
  const total = window.end - window.start
  const start = Math.max(span.start, window.start), end = Math.min(span.end, window.end)
  return { offset: ((start - window.start) / total) * 100, width: Math.max(0, ((end - start) / total) * 100), clippedStart: span.start < window.start, clippedEnd: span.end > window.end }
}

/** The heading of a window: "Monday 23 November 2026", "23 – 29 Nov 2026" or "November 2026" (always Gregorian). */
export function windowTitle(zoom: Zoom, window: Span, locale?: string): string {
  const start = new Date(window.start)
  if (zoom === "day") return start.toLocaleDateString(locale, { calendar: "gregory", weekday: "long", day: "numeric", month: "long", year: "numeric" })
  if (zoom === "month") return start.toLocaleDateString(locale, { calendar: "gregory", month: "long", year: "numeric" })
  const last = new Date(window.end - 1)
  return `${start.toLocaleDateString(locale, { calendar: "gregory", day: "numeric", month: "short" })} – ${last.toLocaleDateString(locale, { calendar: "gregory", day: "numeric", month: "short", year: "numeric" })}`
}
