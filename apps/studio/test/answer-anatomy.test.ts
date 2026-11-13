import { expect, test } from "bun:test"
import { breakOuts, headlineOf } from "../src/lib/answer-anatomy.ts"
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

test("a time series leads with its latest bucket and names the one before, side by side, with no arithmetic", () => {
  const rows = [bucket("Week of 26 Oct 2026", "15", 1, "2026-10-26"), bucket("Week of 2 Nov 2026", "20", 2, "2026-11-02")]
  const lead = headlineOf("Total quantity", weekly, rows)
  expect(lead.bucket?.value).toBe("20")
  expect(lead.sentence).toBe("Total quantity for the week of 2 Nov 2026 is 20, from 2 records. The week of 26 Oct 2026: 15.")
})

test("one bucket is the answer; a breakdown by values leads with no single number; a suppressed bucket says so", () => {
  expect(headlineOf("Total quantity", question(), [bucket("All", "35", 1)]).sentence).toBe("Total quantity is 35, from 1 record.")
  const byUnit = headlineOf("Total quantity", question([{ path: { property: "currency" } }]), [bucket("USD", "1"), bucket("EGP", "2")])
  expect(byUnit).toEqual({ bucket: undefined, sentence: "Total quantity by currency, in 2 groups." })
  const hidden = bucket("Week of 2 Nov 2026", "Suppressed: fewer than 3 records", 2, "2026-11-02", { suppressed: true, totals: [] })
  expect(headlineOf("Total quantity", weekly, [hidden]).sentence).toBe("Total quantity for the week of 2 Nov 2026 is suppressed: fewer than 3 records.")
  expect(headlineOf("Total quantity", weekly, []).bucket).toBeUndefined()
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
