import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { ANALYTICS, questionVerdict } from "../worker/questions.ts"

// The checker's admitQuestion, from the built package when this checkout's checker exports it.
const pkg = process.env.NORN_CHECKER_WASM_PKG
const exported = pkg !== undefined && readFileSync(join(pkg, "norn_checker_wasm.d.ts"), "utf8").includes("admitQuestion")

test("a checker answer becomes a question verdict, never a result", () => {
  const admitted = questionVerdict(ANALYTICS.example, () => JSON.stringify({ admitted: true, plan: { grain: "demo.Conversion", question: {}, sums: [], metrics: [] }, diagnostics: { diagnostics: [] } }))
  expect(admitted).toMatchObject({ kind: "question", admitted: true, plan: { grain: "demo.Conversion" }, findings: [] })
  const refused = questionVerdict({}, () => JSON.stringify({ admitted: false, plan: null, diagnostics: { diagnostics: [{ code: "metric.unknown", message: "no metric demo.purity", severity: "error" }] } }))
  expect(refused).toMatchObject({ admitted: false, plan: null, findings: [{ code: "metric.unknown", message: "no metric demo.purity" }] })
  const malformed = questionVerdict({ name: 1 }, () => { throw new Error("question: invalid type") })
  expect(malformed).toMatchObject({ admitted: false, findings: [{ code: "question.shape", message: "question: invalid type" }] })
})

test.skipIf(!exported)("the checker admits the cell's weekly yield question and refuses one over an undeclared metric", async () => {
  const checker = await import(join(pkg ?? "", "norn_checker_wasm.js")) as { initSync: (m: { module: Uint8Array }) => void; admitQuestion: (a: string, m: string, q: string) => string }
  checker.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
  const yieldWeek = questionVerdict(ANALYTICS.example, checker.admitQuestion)
  expect(yieldWeek.admitted).toBe(true)
  expect(yieldWeek.plan?.grain).toBe("demo.Conversion")
  const purity = questionVerdict({ ...ANALYTICS.example, aggregate: [{ metric: "demo.purity" }] }, checker.admitQuestion)
  expect(purity.admitted).toBe(false)
  expect(purity.findings.length).toBeGreaterThan(0)
})
