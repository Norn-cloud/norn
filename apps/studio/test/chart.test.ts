import { expect, test } from "bun:test"
import { chartEmpty, chartFrame } from "../src/lib/chart.ts"

test("ratios plot as whole percents padded by one point", () => {
  const frame = chartFrame([
    { label: "Week of 12 Oct 2026", value: "0.979165", totals: ["240.148 g", "245.258 g"] },
    { label: "Week of 19 Oct 2026", value: "0.98", totals: ["294 g", "300 g"] },
  ])
  expect(frame?.unit).toBe("%")
  expect(frame?.scale).toEqual([96, 99])
  expect(frame?.points.map(({ shown }) => shown)).toEqual(["97.92%", "98.00%"])
})

test("a sum plots at its own value, never as a percentage", () => {
  const frame = chartFrame([{ label: "All", value: "42", totals: ["42 g"] }, { label: "More", value: "40.5", totals: ["40.5 g"] }])
  expect(frame?.unit).toBe("")
  expect(frame?.scale).toEqual([40, 42])
  expect(frame?.points.map(({ shown, position }) => [shown, position])).toEqual([["42", 42], ["40.5", 40.5]])
})

test("no answers, or none with a value, give no chart instead of infinite bounds", () => {
  expect(chartFrame([])).toBeUndefined()
  expect(chartFrame([{ label: "Week", value: "unavailable", totals: ["0 g", "0 g"] }])).toBeUndefined()
})

test("an all-suppressed answer is not called empty", () => {
  const suppressed = { label: "Week", value: "unavailable", totals: ["1 g"], suppressed: true }
  expect(chartFrame([suppressed])).toBeUndefined()
  expect(chartEmpty([])).toContain("no answers")
  expect(chartEmpty([suppressed])).toContain("suppressed")
  expect(chartEmpty([suppressed])).not.toContain("no answers")
  expect(chartEmpty([{ label: "Week", value: "unavailable", totals: ["0 g", "0 g"] }])).toContain("No week")
})
