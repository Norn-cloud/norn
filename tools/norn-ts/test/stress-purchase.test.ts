import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { canonicalJson } from "../src/canonical.ts"

const root = join(TOOL_ROOT, "../..")
test("STRESS-PURCHASE compiles the complete workflow and immutable inspection", () => {
  const sourcePath = "fixtures/stress/purchase.ts"
  const result = compileDefinitions(join(root, sourcePath), {
    scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath,
  })
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.warnings).toEqual([])
  const path = join(root, "fixtures/stress/purchase.json")
  if (process.env.NORN_UPDATE_STRESS === "1") writeFileSync(path, JSON.stringify(result.artifacts) + "\n")
  expect(canonicalJson(result.artifacts)).toBe(canonicalJson(JSON.parse(readFileSync(path, "utf8"))))
})
