import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

import { ANALYTICS, questionCatalog, questionVerdict, questionsRoute } from "../worker/questions.ts"

// The Worker's checker is WebAssembly built at deploy; here a stand-in records what the route hands it.
const seen: string[] = []
const admit = (_artifacts: string, _metrics: string, question: string) => {
  seen.push(question)
  return JSON.stringify({ admitted: false, plan: null, diagnostics: { diagnostics: [{ code: "metric.unknown", message: "demo.purity is not a declared metric", severity: "error", span: { pointer: "/questions/0/aggregate/0/metric" } }] } })
}
const call = (path: string, init?: RequestInit) => questionsRoute(new Request(`https://studio.test${path}`, init), path, async () => admit)

test("the catalogue offers the world's object set, properties and metrics, and the IR's grains and operators", async () => {
  const response = await call("/api/questions/catalog")
  expect(response.status).toBe(200)
  const catalog = await response.json() as ReturnType<typeof questionCatalog>
  expect(catalog.objectSet).toBe("demo.Conversion")
  expect(catalog.properties.map(({ name }) => name)).toEqual(["input", "recovered", "loss", "occurred_at", "metal"])
  expect(catalog.properties.find(({ name }) => name === "input")).toEqual({ name: "input", kind: "decimal", unit: "g" })
  expect(catalog.metrics.map(({ name, label }) => `${name}:${label}`)).toEqual(["demo.input:Input", "demo.recovered:Recovered", "demo.yield:Yield"])
  expect(catalog.grains).toEqual(["day", "week", "month", "quarter", "year"])
  expect(catalog.operators).toEqual(["eq", "ne", "lt", "le", "gt", "ge"])
})

test("a built question goes to the checker as it was sent, and its verdict comes back as the checker wrote it", async () => {
  const question = { ...ANALYTICS.example, name: "built.purity", aggregate: [{ metric: "demo.purity" }] }
  const response = await call("/api/questions/check", { method: "POST", body: JSON.stringify({ question }) })
  expect(response.status).toBe(200)
  expect(JSON.parse(seen.at(-1) ?? "null")).toEqual(question)
  expect(await response.json()).toMatchObject({ kind: "question", admitted: false, plan: null, findings: [{ code: "metric.unknown", message: "demo.purity is not a declared metric", pointer: "/questions/0/aggregate/0/metric" }] })
})

test("the check route takes only a POSTed {question} object of bounded size", async () => {
  expect((await call("/api/questions/check")).status).toBe(405)
  expect((await call("/api/questions/catalog", { method: "POST", body: "{}" })).status).toBe(405)
  for (const body of ["not json", "null", "{}", '{"question":"demo.yield"}', '{"question":null}']) expect((await call("/api/questions/check", { method: "POST", body })).status).toBe(400)
  expect((await call("/api/questions/check", { method: "POST", body: JSON.stringify({ question: { name: "x".repeat(17 * 1024) } }) })).status).toBe(413)
})

// The real checker, from the built package when this checkout's checker exports it.
const pkg = process.env.NORN_CHECKER_WASM_PKG
const exported = pkg !== undefined && readFileSync(join(pkg, "norn_checker_wasm.d.ts"), "utf8").includes("admitQuestion")
test.skipIf(!exported)("the checker admits the weekly question and refuses the same question by month, naming why", async () => {
  const checker = await import(join(pkg ?? "", "norn_checker_wasm.js")) as { initSync: (m: { module: Uint8Array }) => void; admitQuestion: (a: string, m: string, q: string) => string }
  checker.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
  expect(questionVerdict(ANALYTICS.example, checker.admitQuestion).admitted).toBe(true)
  const monthly = questionVerdict({ ...ANALYTICS.example, groupBy: [{ path: { property: "occurred_at" }, grain: "month" }] }, checker.admitQuestion)
  expect(monthly.admitted).toBe(false)
  expect(monthly.findings.length).toBeGreaterThan(0)
  expect(monthly.findings.every(({ message }) => message.length > 0)).toBe(true)
})
