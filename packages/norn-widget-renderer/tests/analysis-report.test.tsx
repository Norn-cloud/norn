import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { AnalysisReport } from "../src/analysis-report"
import { syntheticReport } from "./synthetic-report"

function show(value: Record<string, unknown> = syntheticReport(), locale = "en"): string {
  return renderToStaticMarkup(<AnalysisReport value={value} locale={locale} />)
}

test("exact tokens and supplied evidence survive presentation without deciding publication", () => {
  const value = syntheticReport()
  const html = show(value)
  expect(html).toContain("9007199254740993.125")
  expect(html).toContain("synthetic-analyst")
  expect(html.match(/<svg /g)).toHaveLength(3)
  expect(html).not.toMatch(/NaN|Infinity/)
  expect(show({ ...value, state: "published" })).toContain("published")
  expect(html).toContain("false")
})

test("bounded finite plot projection retains complete unavailable data", () => {
  const value = syntheticReport()
  const html = show({ ...value, result: { plots: { residuals: Array.from({ length: 257 }, () => ["0", "1"]), qq: [["1e999", "1"]], cooksDistance: [] } } })
  expect(html).not.toContain("<svg ")
  expect(html).toContain("plot data unavailable")
  expect(html).toContain("Inspect the complete server report")
  expect(html).toContain("1e999")
})

test("held-out points and locale labels remain server-driven", () => {
  const value = syntheticReport()
  const html = show({ ...value, result: { predictions: [["synthetic-row", "100", "4", "5"]] } })
  expect(html).toContain("Held-out predictions")
  expect(html).toContain("4, 5")
  expect(show(value, "ar")).toContain("التزامات الافتراضات")
  expect(show({ ...value, numericEncoding: "unknown" })).not.toContain("<svg ")
})
