import { expect, test } from "bun:test"
import { additivityOf, metricHeading, splitMetrics } from "../src/lib/metrics.ts"

// Labels as the metric deriver writes them (crates/norn-checker/src/metric/derive.rs).
const SUM = { kind: "simple", expression: "quantity", label: "sum(quantity); conservation [law.a@1]" }
const AS_OF = { kind: "simple", expression: "quantity", label: "quantity, as of observed_at; last value per canon.Holding at each period's end" }
const RATIO = { kind: "ratio", expression: null, label: "sum(output_fine_g) / sum(input_fine_g); law melt@1" }

test("only a sum the cell may add across time is a total", () => {
  expect(metricHeading(SUM, "week")).toBe("Total quantity")
  expect(metricHeading({ ...SUM, expression: "total" }, "week")).toBe("Total")
  expect(metricHeading(AS_OF, "week")).toBe("Quantity, end of week")
  expect(metricHeading({ ...AS_OF, expression: "stock_cost" }, "week")).toBe("Stock cost, end of week")
})

test("the declared additivity wins over the label, and each reduction reads in words", () => {
  expect(metricHeading({ ...SUM, additivity: { kind: "semi_additive", over: "time", aggregate: "last" } }, "week")).toBe("Quantity, end of week")
  expect(metricHeading({ ...AS_OF, additivity: { kind: "additive" } }, "week")).toBe("Total quantity")
  expect(metricHeading({ ...SUM, additivity: { kind: "semi_additive", over: "time", aggregate: "first" } }, "day")).toBe("Quantity, start of day")
  expect(metricHeading({ ...SUM, additivity: { kind: "semi_additive", over: "time", aggregate: "max" } }, "week")).toBe("Quantity, highest in the week")
  expect(metricHeading({ ...SUM, additivity: { kind: "non_additive" } }, "week")).toBe("Quantity")
})

test("a metric that says nothing about how it combines is never called a total", () => {
  expect(additivityOf({ label: "Quantity" })).toBeUndefined()
  expect(metricHeading({ kind: "simple", expression: "quantity", label: "Quantity" })).toBe("Quantity")
  // A ratio reads in plain words in Business view (preferring server `display` over parsed formula)
  // and keeps the deriver's raw formula label in Technical view.
  expect(additivityOf(RATIO)).toBeUndefined()
  expect(metricHeading(RATIO)).toBe("Output fine g ÷ input fine g")
  expect(metricHeading(RATIO, "period", "technical")).toBe(RATIO.label)
  expect(metricHeading({ ...RATIO, display: "fine yield" })).toBe("Fine yield")
  expect(metricHeading({ ...RATIO, display: "fine yield" }, "period", "technical")).toBe(RATIO.label)
})

test("the assistant's suggested requests read a type's id in a metric label as its title, never the raw id", async () => {
  const { examples, inWords } = await import("../src/dashboards/assistant.tsx")
  expect(inWords(AS_OF.label)).toBe("quantity, as of observed_at; last value per Holding at each period's end")
  const catalog = { objectSet: "canon.Holding", properties: [{ name: "vault", kind: "text", unit: null }], metrics: [{ name: "m", label: AS_OF.label, type: "simple" }], grains: [], operators: [], example: { irVersion: "0.1", calendar: "iso_utc", freshness: null }, declarations: { metrics: [], objectType: null } }
  for (const request of examples(catalog)) expect(request).not.toContain("canon.")
  // The checker's display words, when it sends them, are how the request names the metric.
  const spokenCatalog = { ...catalog, properties: [{ name: "cost_currency", kind: "text", unit: null }], metrics: [{ name: "m", label: AS_OF.label, display: "quantity on hand", type: "simple" }] }
  expect(examples(spokenCatalog)).toEqual(["Weekly quantity on hand", "Which cost currency has the most quantity on hand?"])
})

test("a date's range sent as epoch milliseconds opens a window; with no range at all, the latest 52 ISO weeks are asked", async () => {
  const { windowOf, recentWindow } = await import("../src/lib/metrics.ts")
  const profile = { properties: [{ property: "observed_at", min: Date.UTC(2026, 9, 21, 9), max: Date.UTC(2026, 10, 12, 18) }] } as unknown as Parameters<typeof windowOf>[0]
  expect(windowOf(profile, "observed_at")).toEqual({ start: "2026-10-19T00:00:00Z", end: "2026-11-16T00:00:00Z", grain: "week" })
  expect(recentWindow(Date.UTC(2026, 10, 14, 4, 30))).toEqual({ start: "2025-11-17T00:00:00Z", end: "2026-11-16T00:00:00Z", grain: "week" })
})

test("a metric the cell lists over no single type is kept apart, never asked as if it had one", () => {
  const base = { label: "x", kind: "simple", expression: "qty", timeProperty: null, numerator: null, denominator: null }
  const split = splitMetrics([
    { ...base, name: "a.qty", objectType: "a.Thing" },
    { ...base, name: "a.rate", kind: "ratio", expression: null, objectType: null, numerator: "a.qty", denominator: "b.qty" },
  ])
  expect(split.metrics.map(({ name }) => name)).toEqual(["a.qty"])
  expect(split.combined.map(({ name }) => name)).toEqual(["a.rate"])
})
