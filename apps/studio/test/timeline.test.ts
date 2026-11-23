import { expect, test } from "bun:test"
import { catalogOf } from "../src/lib/catalog.ts"
import type { Artifact } from "../src/lib/transport.ts"
import { descendsFrom, geometry, instantOf, laneLinks, nearestQuery, offersTimeline, stack, stepAnchor, ticksOf, windowOf, windowQuery, zoneOf } from "../src/lib/timeline.ts"
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
  expect(nearestQuery(window, null, "before")).toEqual({ filter: { all: [{ property: "ends_at", op: "lte", value: "2026-11-23T00:00:00.000Z" }] }, sort: [{ property: "starts_at", direction: "desc" }] })
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
  expect(geometry({ start: -50, end: 50 }, { start: 0, end: 200 })).toEqual({ left: 0, width: 25, clippedStart: true, clippedEnd: false })
  expect(geometry({ start: 100, end: 300 }, { start: 0, end: 200 })).toEqual({ left: 50, width: 50, clippedStart: false, clippedEnd: true })
})
