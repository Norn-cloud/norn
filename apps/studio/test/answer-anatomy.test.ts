import { expect, test } from "bun:test"
import { breakOuts, figure, headlineOf, openPeriod, unitGroups } from "../src/lib/answer-anatomy.ts"
import { groupDigits, readableTime } from "../src/lib/format.ts"
import type { Bucket } from "../src/lib/answer.ts"
import type { Question } from "../src/lib/transport.ts"

const bucket = (label: string, value: string, members = 2, raw: unknown = null, extra: Partial<Bucket> = {}): Bucket =>
  ({ key: JSON.stringify(raw), bucket: raw, label, value, totals: [value], members: Array.from({ length: members }, (_, index) => ({ id: `r${index}`, version: 1, amounts: [value] })), ...extra })

const question = (groupBy?: Question["groupBy"]) => ({
  irVersion: "0.1", name: "studio.metric", objectSet: "canon.OrderLine", aggregate: [{ metric: "m" }],
  ...groupBy ? { groupBy } : {},
  filter: [{ path: { property: "unit" }, operator: "eq", value: "g" }],
  window: { start: "2026-10-26T00:00:00Z", end: "2026-11-09T00:00:00Z" },
  freshness: { kind: "live", maxAgeMs: 60_000 }, calendar: "iso_utc",
}) as unknown as Question

const weekly = question([{ path: { property: "due_at" }, grain: "week" }])

const later = Date.UTC(2026, 11, 1)

test("a time series leads with its latest bucket and names the one before, side by side, with no arithmetic", () => {
  const rows = [bucket("Week of 26 Oct 2026", "15", 1, "2026-10-26"), bucket("Week of 2 Nov 2026", "7905", 2, "2026-11-02")]
  const lead = headlineOf("Total quantity", weekly, rows, later, "g")
  expect(lead.bucket?.value).toBe("7905")
  expect(lead.label).toBe("Week of 2 Nov 2026")
  expect(lead.sentence).toBe("Total quantity for the week of 2 Nov 2026 is 7,905 g, from 2 records. The week of 26 Oct 2026: 15 g.")
})

test("a period still open at the cell's clock reads as so far", () => {
  const rows = [bucket("Week of 9 Nov 2026", "6", 6, "2026-11-09")]
  const during = Date.UTC(2026, 10, 12)
  expect(openPeriod({ bucket: "2026-11-09" }, "week", during)).toBe(true)
  expect(openPeriod({ bucket: "2026-11-09" }, "week", Date.UTC(2026, 10, 16))).toBe(false)
  expect(openPeriod({ bucket: "2026-11-09" }, undefined, during)).toBe(false)
  const lead = headlineOf("Total quantity", weekly, rows, during, "g")
  expect(lead.label).toBe("Week of 9 Nov 2026, so far")
  expect(lead.sentence).toBe("Total quantity for the week of 9 Nov 2026, so far is 6 g, from 6 records.")
})

test("a figure groups an amount's digits and names its unit; a ratio stays its percentage", () => {
  expect(figure({ value: "7905", totals: ["7905"] }, "g")).toBe("7,905 g")
  expect(figure({ value: "1240.00", totals: ["1240.00"] }, "USD")).toBe("1,240.00 USD")
  expect(figure({ value: "0.979685", totals: ["240.148", "245.13"] }, "g")).toBe("97.97%")
  expect(figure({ value: "12", totals: ["12"] })).toBe("12")
  expect(groupDigits("-1234567.891 kg")).toBe("-1,234,567.891 kg")
  expect(groupDigits("unavailable")).toBe("unavailable")
  expect(readableTime("2026-11-01")).toBe("1 Nov 2026")
  expect(readableTime("2026-11-01T09:30:00Z")).toBe("1 Nov 2026, 09:30 UTC")
  expect(readableTime("SO-1001")).toBe("SO-1001")
})

test("unit groups are the combinations the cell returned, labelled by their first set source, most records first", () => {
  const rows = [bucket("x", "1240.00", 2, [null, "USD"]), bucket("y", "7905", 6, ["g", null]), bucket("z", "3", 1, [null, null])]
  const groups = unitGroups(["unit", "currency"], rows)
  expect(groups.map(({ unit, values }) => ({ unit, values }))).toEqual([
    { unit: "g", values: { unit: "g", currency: null } },
    { unit: "USD", values: { unit: null, currency: "USD" } },
    { unit: null, values: { unit: null, currency: null } },
  ])
  expect(unitGroups(["currency"], [bucket("USD", "5", 1, "USD")])[0]?.unit).toBe("USD")
})

test("one bucket is the answer; a breakdown by values leads with no single number; a suppressed bucket says so", () => {
  expect(headlineOf("Total quantity", question(), [bucket("All", "35", 1)], later).sentence).toBe("Total quantity is 35, from 1 record.")
  const byUnit = headlineOf("Total quantity", question([{ path: { property: "currency" } }]), [bucket("USD", "1"), bucket("EGP", "2")], later)
  expect(byUnit).toEqual({ bucket: undefined, label: "", sentence: "Total quantity by currency, in 2 groups." })
  const hidden = bucket("Week of 2 Nov 2026", "Suppressed: fewer than 3 records", 2, "2026-11-02", { suppressed: true, totals: [] })
  expect(headlineOf("Total quantity", weekly, [hidden], later).sentence).toBe("Total quantity for the week of 2 Nov 2026 is suppressed: fewer than 3 records.")
  expect(headlineOf("Total quantity", weekly, [], later).bucket).toBeUndefined()
})

const properties = [
  { id: "status", type: { kind: "enum" } },
  { id: "unit", type: { kind: "text" } },
  { id: "due_at", type: { kind: "timestamp" } },
  { id: "quantity", type: { kind: "decimal" } },
  { id: "currency", type: { kind: "text" } },
  { id: "registry", type: null },
]

test("a week's drills hold the question to that week and break it out by each categorical property it does not already fix", () => {
  const drills = breakOuts(weekly, { bucket: "2026-11-02" }, properties)
  expect(drills.map(({ property }) => property)).toEqual(["status", "currency"])
  const [first] = drills
  expect(first?.question.window).toEqual({ start: "2026-11-02T00:00:00Z", end: "2026-11-09T00:00:00Z" })
  expect(first?.question.groupBy).toEqual([{ path: { property: "status" } }])
  expect(first?.question.filter).toEqual(weekly.filter)
  expect(first?.question.aggregate).toEqual(weekly.aggregate)
  // A week running past the question's window stops at its end.
  expect(breakOuts({ ...weekly, window: { ...weekly.window, end: "2026-11-05T00:00:00Z" } }, { bucket: "2026-11-02" }, properties)[0]?.question.window.end).toBe("2026-11-05T00:00:00Z")
})

test("a value's drills fix that value; an unknown grain or bucket offers none", () => {
  const byCurrency = question([{ path: { property: "currency" } }])
  const [drill] = breakOuts(byCurrency, { bucket: "USD" }, properties)
  expect(drill?.property).toBe("status")
  expect(drill?.question.filter).toEqual([...byCurrency.filter ?? [], { path: { property: "currency" }, operator: "eq", value: "USD" }])
  expect(breakOuts(question([{ path: { property: "due_at" }, grain: "month" as never }]), { bucket: "2026-11-01" }, properties)).toEqual([])
  expect(breakOuts(weekly, { bucket: null }, properties)).toEqual([])
})
