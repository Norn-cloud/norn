import { expect, mock, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import type { Bucket } from "../src/lib/answer.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { DrawnAnswer, VIEWS, defaultView, viewKey, viewOf } = await import("../src/dashboards/tile-view.tsx")

const bucket = (key: string, value: string, totals: readonly string[], members = 3): Bucket =>
  ({ key, label: key, value, totals, members: Array.from({ length: members }, (_, index) => ({ id: `${key}-${index}`, version: 1, amounts: null })) })

test("every view the picker offers is one the definition stores, and reads back as the same choice", () => {
  for (const [key] of VIEWS) expect(viewKey(viewOf(key))).toBe(key)
  expect(viewOf("number")).toEqual({ kind: "number" })
  expect(viewOf("chart:pie")).toEqual({ kind: "chart", chart: "pie" })
  expect(viewKey({ kind: "chart" })).toBe("chart:line")
})

test("a new tile starts as a number for one value, a line for a time series and a bar for a breakdown", () => {
  expect(defaultView({})).toEqual({ kind: "number" })
  expect(defaultView({ groupBy: [{ grain: "week" }] })).toEqual({ kind: "chart", chart: "line" })
  expect(defaultView({ groupBy: [{}] })).toEqual({ kind: "chart", chart: "bar" })
})

test("a number shows the server's value, a ratio's exact operands and its record count; more than one bucket is a table", () => {
  const ratio = renderToStaticMarkup(<DrawnAnswer view={{ kind: "number" }} rows={[bucket("All", "0.979685", ["240.148 g", "245.13 g"], 12)]} name="Yield" />)
  for (const text of ['data-view="number"', "97.97%", "240.148 g / 245.13 g = 0.979685 exactly", "12 records"]) expect(ratio).toContain(text)
  const two = renderToStaticMarkup(<DrawnAnswer view={{ kind: "number" }} rows={[bucket("gold", "3", ["3 g"]), bucket("silver", "4", ["4 g"])]} name="Recovered" />)
  expect(two).not.toContain('data-view="number"')
  expect(two).toContain("silver")
})

test("a pie that is not a whole says why and lists the parts instead; a table lists every bucket", () => {
  const pie = renderToStaticMarkup(<DrawnAnswer view={{ kind: "chart", chart: "pie" }} rows={[bucket("gold", "3", ["3 g"]), bucket("loss", "-1", ["-1 g"])]} name="Recovered" />)
  expect(pie).toContain("negative")
  expect(pie).toContain("loss")
  expect(pie).not.toContain('data-view="pie"')
  const whole = renderToStaticMarkup(<DrawnAnswer view={{ kind: "chart", chart: "pie" }} rows={[bucket("gold", "3", ["3 g"]), bucket("silver", "1", ["1 g"])]} name="Recovered" />)
  expect(whole).toContain('data-view="pie"')
  const table = renderToStaticMarkup(<DrawnAnswer view={{ kind: "table" }} rows={[bucket("gold", "3", ["3 g"], 5)]} name="Recovered" />)
  for (const text of ["gold", ">3<", ">5<"]) expect(table).toContain(text)
})

test("a chart draws its declared kind, and says why when no bucket has a value", () => {
  for (const chart of ["line", "area", "bar"] as const) {
    expect(renderToStaticMarkup(<DrawnAnswer view={{ kind: "chart", chart }} rows={[bucket("w1", "3", ["3 g"]), bucket("w2", "4", ["4 g"])]} name="Recovered" />)).toContain(`data-view="${chart}"`)
  }
  expect(renderToStaticMarkup(<DrawnAnswer view={{ kind: "chart", chart: "bar" }} rows={[]} name="Recovered" />)).toContain("no answers")
})
