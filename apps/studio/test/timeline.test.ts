import { expect, test } from "bun:test"
import { catalogOf } from "../src/lib/catalog.ts"
import type { Artifact } from "../src/lib/transport.ts"
import {
  TIMELINE_MAX_USER_CLAUSES, clampFocus, clockText, descendsFrom, formatLocale, geometry, groupLanes, instantOf, laneLinks, nearestJumpTarget, nearestQuery, offersTimeline, resolveBarFocus, stack, stepAnchor, ticksOf, timelineFilterOverflow, timeText, tryWindowQuery, windowOf, windowQuery, windowTitle, zoneOf,
} from "../src/lib/timeline.ts"
import { INTERVAL_ARTIFACTS } from "../e2e/interval-fixtures.ts"

const catalog = catalogOf(INTERVAL_ARTIFACTS as unknown as readonly Artifact[])
const concept = (name: string) => { const found = catalog.byName.get(name); if (!found) throw new Error(name); return found }

test("Timeline is offered for a TimeInterval directly or through an interface, never for another type or the interface itself", () => {
  expect(offersTimeline(catalog, concept("facilities.EquipmentBooking"))).toBe(true)
  expect(offersTimeline(catalog, concept("venues.RoomReservation"))).toBe(true)
  expect(descendsFrom(catalog, "venues.RoomReservation", "venues.Reservation")).toBe(true)
  expect(offersTimeline(catalog, concept("facilities.Equipment"))).toBe(false)
  expect(offersTimeline(catalog, concept("core.TimeInterval"))).toBe(false)
  expect(offersTimeline(catalog, concept("venues.Reservation"))).toBe(false)
})

test("lanes follow declared to-one links only", () => {
  expect(laneLinks(concept("facilities.EquipmentBooking")).map(({ id }) => id)).toEqual(["equipment", "booked_by"])
  expect(laneLinks(concept("venues.RoomReservation")).map(({ id }) => id)).toEqual(["room"])
})

test("windows are the reader's day, Monday-first week and month, and step by the calendar", () => {
  const at = new Date(2026, 10, 25, 15, 30) // Wednesday 25 November 2026, local
  expect(windowOf("day", at)).toEqual({ start: new Date(2026, 10, 25).getTime(), end: new Date(2026, 10, 26).getTime() })
  expect(windowOf("week", at)).toEqual({ start: new Date(2026, 10, 23).getTime(), end: new Date(2026, 10, 30).getTime() })
  expect(windowOf("week", new Date(2026, 10, 29))).toEqual(windowOf("week", at)) // Sunday belongs to the week before it
  expect(windowOf("month", at)).toEqual({ start: new Date(2026, 10, 1).getTime(), end: new Date(2026, 11, 1).getTime() })
  expect(stepAnchor("month", new Date(2026, 0, 31), 1)).toEqual(new Date(2026, 1, 1))
  expect(stepAnchor("week", at, -1)).toEqual(new Date(2026, 10, 18))
  expect(ticksOf("day", windowOf("day", at))).toHaveLength(24)
  expect(ticksOf("week", windowOf("week", at))).toHaveLength(7)
  expect(ticksOf("month", windowOf("month", at))).toHaveLength(30)
})

test("the window is asked of the cell as the overlap test, after the reader's own filter, in start order", () => {
  const window = { start: Date.UTC(2026, 10, 23), end: Date.UTC(2026, 10, 30) }
  const own = { all: [{ property: "label", op: "contains" as const, value: "run" }] }
  expect(windowQuery(window, own)).toEqual({
    filter: { all: [...own.all, { property: "starts_at", op: "lt", value: "2026-11-30T00:00:00.000Z" }, { property: "ends_at", op: "gt", value: "2026-11-23T00:00:00.000Z" }] },
    sort: [{ property: "starts_at", direction: "asc" }],
  })
  expect(nearestQuery(window, null, "after")).toEqual({ filter: { all: [{ property: "starts_at", op: "gte", value: "2026-11-30T00:00:00.000Z" }] }, sort: [{ property: "starts_at", direction: "asc" }] })
  expect(nearestQuery(window, null, "before")).toEqual({ filter: { all: [{ property: "ends_at", op: "lte", value: "2026-11-23T00:00:00.000Z" }] }, sort: [{ property: "ends_at", direction: "desc" }] })
})

test("Latest before sorts by end descending and jumps so a long interval's end is visible inside the destination window", () => {
  const emptyWindow = { start: Date.UTC(2026, 10, 23), end: Date.UTC(2026, 10, 30) }
  const longEndingJustBefore = { id: "long", starts_at: "2026-11-01T00:00:00Z", ends_at: "2026-11-23T00:00:00Z" }
  const shortStartedLater = { id: "short", starts_at: "2026-11-10T00:00:00Z", ends_at: "2026-11-10T02:00:00Z" }
  // Sorting by `nearestQuery(..., "before").sort` (`ends_at:desc`) picks the long interval over the shorter one.
  const q = nearestQuery(emptyWindow, null, "before")
  expect(q.sort).toEqual([{ property: "ends_at", direction: "desc" }])
  const ordered = [shortStartedLater, longEndingJustBefore].sort((a, b) => Date.parse(b.ends_at) - Date.parse(a.ends_at))
  expect(ordered[0]!.id).toBe("long")
  // Jumping uses `end - 1` so even an interval ending at the exact midnight boundary (`window.start`)
  // lands in the preceding week where it satisfies `ends_at > dest.start` (not back in the empty window).
  const target = nearestJumpTarget(longEndingJustBefore, "before")
  expect(target).not.toBeNull()
  expect(target!.at).toBe(Date.UTC(2026, 10, 23, 0, 0, 0))
  expect(target!.jumpAt).toBe(Date.UTC(2026, 10, 23, 0, 0, 0) - 1)
  expect(target!.jumpAt).toBeLessThan(emptyWindow.start)
  expect(target!.jumpAt).toBeGreaterThanOrEqual(Date.parse(longEndingJustBefore.starts_at))
})

test("resolveBarFocus restores DOM focus after a lane-change remount or bar removal, and never steals focus from external controls", () => {
  const body = { tag: "BODY" }
  const externalInput = { tag: "INPUT" }
  let focusedNew = 0
  const remountedBtn = { id: "booking/1", focus: () => { focusedNew += 1 } }
  let focusedClamped = 0
  const remainingBtn = { id: "booking/2", focus: () => { focusedClamped += 1 } }

  // 1. Focused bar remounts under a new lane (same record id, old DOM button unmounted -> activeElement fell back to body).
  const onRemount = resolveBarFocus({
    orderIds: ["booking/2", "booking/1"],
    focusedIndex: 0,
    focusedId: "booking/1",
    wasInside: true,
    activeElement: body,
    bodyElement: body,
    activeInsideGroup: false,
    buttonFor: (id) => (id === "booking/1" ? remountedBtn : remainingBtn),
  })
  expect(onRemount.index).toBe(1)
  expect(onRemount.focusTarget).toBe(remountedBtn)
  onRemount.focusTarget?.focus()
  expect(focusedNew).toBe(1)

  // 2. Focused bar disappears completely while focus was in bars -> clamps and restores focus to replacement bar.
  const onRemoval = resolveBarFocus({
    orderIds: ["booking/2"],
    focusedIndex: 2,
    focusedId: "booking/9",
    wasInside: true,
    activeElement: body,
    bodyElement: body,
    activeInsideGroup: false,
    buttonFor: () => remainingBtn,
  })
  expect(onRemoval.index).toBe(0)
  expect(onRemoval.focusTarget).toBe(remainingBtn)
  onRemoval.focusTarget?.focus()
  expect(focusedClamped).toBe(1)

  // 3. Unrelated external control holds focus -> never steals focus even when bars change.
  const external = resolveBarFocus({
    orderIds: ["booking/1"],
    focusedIndex: 0,
    focusedId: "booking/1",
    wasInside: false,
    activeElement: externalInput,
    bodyElement: body,
    activeInsideGroup: false,
    buttonFor: () => remountedBtn,
  })
  expect(external.focusTarget).toBeNull()
})

test("instants read with their offsets; a record's zone is used only when the runtime knows it", () => {
  expect(instantOf("2026-11-24T06:00:00+09:00")).toBe(Date.UTC(2026, 10, 23, 21))
  expect(instantOf("2026-11-24")).toBe(new Date(2026, 10, 24).getTime())
  expect(instantOf("not a time")).toBeNull()
  expect(instantOf(null)).toBeNull()
  expect(zoneOf({ timezone: "Asia/Tokyo" })).toBe("Asia/Tokyo")
  expect(zoneOf({ timezone: "Mars/Olympus" })).toBeNull()
  expect(zoneOf({ timezone: null })).toBeNull()
})

test("overlapping spans stack into the fewest rows and are marked; touching ends do not overlap", () => {
  const span = (item: string, start: number, end: number) => ({ item, start, end })
  const { placed, rows } = stack([span("a", 0, 10), span("b", 5, 15), span("c", 10, 20), span("d", 15, 25)])
  expect(rows).toBe(2)
  expect(placed.map(({ item, row, overlaps }) => [item, row, overlaps])).toEqual([["a", 0, true], ["b", 1, true], ["c", 0, true], ["d", 1, true]])
  const apart = stack([span("x", 0, 10), span("y", 10, 20)])
  expect(apart.rows).toBe(1)
  expect(apart.placed.every(({ overlaps }) => !overlaps)).toBe(true)
})

test("a bar is clipped to the window and says on which side", () => {
  expect(geometry({ start: -50, end: 50 }, { start: 0, end: 200 })).toEqual({ offset: 0, width: 25, clippedStart: true, clippedEnd: false })
  expect(geometry({ start: 100, end: 300 }, { start: 0, end: 200 })).toEqual({ offset: 50, width: 50, clippedStart: false, clippedEnd: true })
})

test("tryWindowQuery enforces the cell's 8-clause cap by reserving 2 clauses for the time window", () => {
  const window = { start: Date.UTC(2026, 10, 23), end: Date.UTC(2026, 10, 30) }
  const make = (n: number) => ({ all: Array.from({ length: n }, (_, i) => ({ property: "label", op: "contains" as const, value: `v${i}` })) })
  expect(TIMELINE_MAX_USER_CLAUSES).toBe(6)
  expect(timelineFilterOverflow(make(6))).toBe(0)
  expect(timelineFilterOverflow(make(7))).toBe(1)
  expect(timelineFilterOverflow(make(8))).toBe(2)
  const fits = tryWindowQuery(window, make(6))
  expect(fits.kind).toBe("ok")
  if (fits.kind === "ok") expect(fits.filter.all).toHaveLength(8)
  expect(tryWindowQuery(window, make(7))).toEqual({ kind: "too_many", overflow: 1, maxUserClauses: 6, actual: 7 })
  expect(tryWindowQuery(window, make(8))).toEqual({ kind: "too_many", overflow: 2, maxUserClauses: 6, actual: 8 })
})

test("groupLanes separates pending, unavailable, and unlinked/hidden buckets and suppresses overlap markers outside resolved targets", () => {
  const spans = [
    { item: "a", start: 0, end: 10 },
    { item: "b", start: 5, end: 15 },
    { item: "c", start: 0, end: 10 },
    { item: "d", start: 5, end: 15 },
    { item: "e", start: 0, end: 10 },
    { item: "f", start: 5, end: 15 },
    { item: "g", start: 0, end: 10 },
    { item: "h", start: 5, end: 15 },
  ]
  const lanes = groupLanes(spans, [
    { kind: "target", id: "equipment/1" },
    { kind: "target", id: "equipment/1" },
    { kind: "pending" },
    { kind: "pending" },
    { kind: "unavailable" },
    { kind: "unavailable" },
    { kind: "none" },
    { kind: "none" },
  ], { lane: "equipment", conceptTitle: "Equipment booking", linkName: "equipment" })
  expect(lanes.map((l) => [l.kind, l.label, l.rows, l.overlapping])).toEqual([
    ["target", "equipment/1", 2, 2],
    ["pending", "Reading links…", 2, 0],
    ["unavailable", "Link unavailable", 2, 0],
    ["none", "No visible equipment", 2, 0],
  ])
  expect(lanes[0]!.placed.every((p) => p.overlaps)).toBe(true)
  expect(lanes.slice(1).every((l) => l.placed.every((p) => !p.overlaps))).toBe(true)
})

test("day ticks step in epoch hours, handle 23h/25h DST days, and include minutes on half-hour DST shifts", () => {
  const HOUR = 3_600_000
  const start = Date.UTC(2026, 2, 8, 5)
  const spring = ticksOf("day", { start, end: start + 23 * HOUR }, "en-US")
  expect(spring).toHaveLength(23)
  expect(new Set(spring.map((t) => t.at)).size).toBe(23)

  const fallStart = Date.UTC(2026, 10, 1, 4)
  const fall = ticksOf("day", { start: fallStart, end: fallStart + 25 * HOUR }, "en-US")
  expect(fall).toHaveLength(25)
  expect(new Set(fall.map((t) => t.at)).size).toBe(25)

  // Australia/Lord_Howe springs forward 30 minutes on 2026-10-04 (02:00+10:30 -> 02:30+11:00):
  // midnight is 2026-10-03T13:30:00Z, so hourly epoch ticks hit 00:00, 01:00, then 02:30, 03:30, ...
  const lordHoweMidnight = Date.UTC(2026, 9, 3, 13, 30)
  const lordHowe = ticksOf("day", { start: lordHoweMidnight, end: lordHoweMidnight + 23 * HOUR }, "en-GB", "Australia/Lord_Howe")
  expect(lordHowe[2]!.label).toContain("30")
  expect(lordHowe[0]!.label).not.toContain("30")
})

test("roving focus clamps when the bar list shrinks and timeline formatting honours Studio locale on Gregorian calendar", () => {
  expect(clampFocus(4, 2)).toBe(1)
  expect(clampFocus(3, 0)).toBe(0)
  expect(clampFocus(-1, 3)).toBe(0)
  expect(formatLocale("ar", "en-GB")).toBe("ar")
  expect(formatLocale("ar", "ar-EG")).toBe("ar-EG")
  expect(formatLocale("en", "en-GB")).toBe("en-GB")
  expect(formatLocale("en", "ar-EG")).toBe("en")

  const win = { start: Date.UTC(2026, 10, 23), end: Date.UTC(2026, 10, 30) }
  const arTitle = windowTitle("month", win, "ar")
  const enTitle = windowTitle("month", win, "en-GB")
  expect(arTitle).not.toBe(enTitle)
  // `ar-SA` defaults to Hijri unless `calendar: "gregory"` is passed: verify Gregorian November 2026.
  const arSaTitle = windowTitle("month", win, "ar-SA")
  expect(arSaTitle).toContain("نوفمبر")
  expect(arSaTitle).toMatch(/2026|٢٠٢٦/)
  expect(arSaTitle).not.toMatch(/1448|١٤٤٨/)
  expect(timeText(Date.UTC(2026, 10, 23, 9), "UTC", "ar")).not.toBe(timeText(Date.UTC(2026, 10, 23, 9), "UTC", "en-GB"))
  expect(clockText(Date.UTC(2026, 10, 23, 9, 30), "UTC", "ar").length).toBeGreaterThan(0)
})
