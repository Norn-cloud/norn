import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import { compileDashboards, compileQuestions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"

// Supply lab (registry/packs/supply-lab): its questions and dashboard are not
// installable artifacts, so the registry-packs test does not compile them.
// This test holds their generated goldens, which the Rust stress harness
// (crates/norn-cell/tests/supply_lab_stress.rs) admits against the derived
// metric space; NORN_PACKS_UPDATE=1 regenerates them.
const DIR = join(TOOL_ROOT, "../../registry/packs/supply-lab/questions")
const SOURCE = join(DIR, "supply.questions.ts")
const update = process.env.NORN_PACKS_UPDATE === "1"

function golden(name: string, value: unknown): void {
  const path = join(DIR, name)
  if (update) writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`)
  expect(canonicalJson(JSON.parse(readFileSync(path, "utf8")))).toBe(canonicalJson(value as never))
}

test("supply-lab questions lower to their committed QuestionIR", () => {
  const result = compileQuestions(SOURCE)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.questions.map(q => (q as { name: string }).name)).toEqual([
    "supply.on_hand_weekly", "supply.movement_volume_weekly", "supply.fill_rate_weekly", "supply.received_weekly",
  ])
  golden("supply.questions.json", result.questions)
})

test("supply-lab dashboard lowers its four tiles", () => {
  const result = compileDashboards(SOURCE)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.dashboards).toHaveLength(1)
  golden("supply.dashboard.json", result.dashboards)
})
