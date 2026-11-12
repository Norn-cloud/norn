import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDashboards, compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"

const fixture = join(TOOL_ROOT, "fixtures/dashboards/refinery.ts")
const expected = JSON.parse(readFileSync(join(TOOL_ROOT, "fixtures/dashboards/refinery.dashboard.json"), "utf8"))

function withModule<T>(source: string, run: (path: string) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "norn-dashboard-"))
  try { const path = join(dir, "dashboard.ts"); writeFileSync(path, source); return run(path) }
  finally { rmSync(dir, { recursive: true, force: true }) }
}

test("DASHBOARD-LOWER-1: a code dashboard lowers to the cell's dashboard payload, its questions resolved from the module, in reading order", () => {
  const result = compileDashboards(fixture)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  // The cell-do suite admits this same document (a_code_dashboard_is_admitted_like_one_saved_in_studio).
  expect(result.dashboards).toEqual(expected)
})

test("a tile names a Norn.question from its own module, a known view and width; anything else is refused, and dashboards are not artifacts", () => {
  const source = readFileSync(fixture, "utf8")
  for (const candidate of [
    source.replace("question: Recovered,", 'question: "demo.recovered_total",'),
    source.replace("question: Recovered,", "question: Missing,"),
    source.replace('view: "number"', 'view: "gauge"'),
    source.replace('view: "number" }', 'view: "number", width: "wide" }'),
    source.replace('view: "number" }', 'view: "number", colour: "red" }'),
    source.replace("export const Refinery =", "export default"),
  ]) expect(withModule(candidate, (path) => compileDashboards(path)).ok).toBe(false)
  expect(withModule(source, (path) => compileDefinitions(path, { scope: { orgId: "org_demo", environmentId: "analytics" }, sourcePath: "dashboard.ts" })).ok).toBe(false)
})
