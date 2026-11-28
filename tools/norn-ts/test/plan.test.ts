import { expect, test } from "bun:test"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import { compileDashboards, compileDefinitionFiles, compileQuestions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"

// S2 Plan pack (registry/packs/plan): versioned PlanVersion/PlanLine,
// Open-to-Buy (OTB) envelopes, and Actual/Forecast variance questions.
// Questions and the dashboard live beside the pack definitions and lower to
// committed QuestionIR goldens admitted by `crates/norn-cell/tests/plan_otb_slice.rs`.
// `NORN_PACKS_UPDATE=1` regenerates them.
const PACK = join(TOOL_ROOT, "../../registry/packs/plan")
const DIR = join(PACK, "questions")
const QUESTIONS_SOURCE = join(PACK, "questions.ts")
const update = process.env.NORN_PACKS_UPDATE === "1"

function golden(name: string, value: unknown): void {
  const path = join(DIR, name)
  if (update) {
    mkdirSync(DIR, { recursive: true })
    writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`)
  }
  expect(canonicalJson(JSON.parse(readFileSync(path, "utf8")))).toBe(canonicalJson(value as never))
}

test("plan pack questions lower to their committed QuestionIR", () => {
  const result = compileQuestions(QUESTIONS_SOURCE)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.questions.map(q => (q as { name: string }).name)).toEqual([
    "plan.planned_sales_retail_monthly",
    "plan.planned_receipts_cost_monthly",
    "plan.planned_markdowns_cost_monthly",
    "plan.planned_fine_metal_monthly",
    "plan.planned_eom_stock_cost_monthly",
    "plan.otb_approved_budget_monthly",
    "plan.otb_recognized_receipts_monthly",
    "plan.otb_unreceived_po_monthly",
    "plan.actual_pos_revenue_monthly",
    "plan.actual_pos_units_monthly",
    "plan.published_forecast_sales_monthly",
  ])
  golden("plan.questions.json", result.questions)
})

test("plan pack dashboard lowers its six tiles", () => {
  const result = compileDashboards(QUESTIONS_SOURCE)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.dashboards).toHaveLength(1)
  golden("plan.dashboard.json", result.dashboards)
})

test("plan pack compiles with non-vacuity allowed and refused OTB examples", () => {
  const files = ["plan.ts", "actions.ts", "analytics.ts", "plan.pack.ts", "plan.link.ts"].map(f => join(PACK, f))
  const compiled = compileDefinitionFiles(files, path => ({
    scope: {},
    sourcePath: `registry/packs/plan/${path.split("/").pop()}`,
    registryPackagePath: "registry/packs/plan",
  }))
  const artifacts: Array<{ artifactId: string; kind: string }> = []
  const examples: Array<{ name: string; artifactId: string; expect: unknown }> = []
  for (const res of compiled) {
    if (!res.ok) throw new Error(JSON.stringify(res.diagnostics))
    artifacts.push(...(res.artifacts as Array<{ artifactId: string; kind: string }>))
    examples.push(...res.examples)
  }
  const ids = new Set(artifacts.map(a => a.artifactId))
  expect(ids.has("plan.PlanTarget.interface")).toBe(true)
  expect(ids.has("plan.PlanVersion.entity")).toBe(true)
  expect(ids.has("plan.PlanSpreadTarget.entity")).toBe(true)
  expect(ids.has("plan.PlanLine.entity")).toBe(true)
  expect(ids.has("plan.OtbEnvelope.entity")).toBe(true)
  expect(ids.has("plan.OtbPoBinding.entity")).toBe(true)
  expect(ids.has("plan.PublishedForecast.entity")).toBe(true)
  expect(ids.has("plan.otb.authorize_po_line.action")).toBe(true)
  expect(ids.has("plan.otb_position")).toBe(true)
  expect(ids.has("plan.actual_sales_at_plan_grain")).toBe(true)
  expect(ids.has("plan.forecast_vs_plan_variance")).toBe(true)

  const otbCapExamples = examples.filter(e => e.artifactId === "plan.OtbPoBinding.authorization_within_cap.law")
  expect(otbCapExamples.some(e => e.expect === "holds")).toBe(true)
  expect(otbCapExamples.some(e => e.expect === "violated")).toBe(true)
})
