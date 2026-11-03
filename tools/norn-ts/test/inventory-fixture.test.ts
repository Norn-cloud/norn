import { test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"

test("INV-CP1 stock closure compiles independently of jewelry conversion", () => {
  const root = join(TOOL_ROOT, "../..")
  const source = join(TOOL_ROOT, "fixtures/jewelry/cp1-inventory.source.ts")
  const result = compilePack(source, {scope: {orgId: "org_demo", environmentId: "dev"}, root, modules: [source]})
  if (!result.ok) throw new Error(JSON.stringify(result.failures))
  expect(result.artifacts).toEqual(JSON.parse(readFileSync(join(root, "crates/norn-cell/tests/fixtures/rea-inventory.json"), "utf8")))
  expect(JSON.stringify(result.artifacts)).not.toContain("core.Conversion")
  expect(JSON.stringify(result.artifacts)).not.toContain("actor.subjectId")
}, 30_000)
