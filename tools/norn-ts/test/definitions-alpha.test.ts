import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const root = resolve(TOOL_ROOT, "../..")
const sourcePath = "tools/norn-ts/fixtures/engine-alpha/sales-order.source.ts"
const golden = join(root, "fixtures/engine-alpha/sales-order.definitions.json")
const compile = () => {
  const result = compileDefinitions(join(root, sourcePath), {
    scope: { orgId: "org_alpha", environmentId: "dev" }, sourcePath,
  })
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
  return result.artifacts
}

test("alpha SalesOrder compiles deterministically with read-only economic projections and checks its lifecycle", () => {
  const artifacts = compile()
  const bytes = canonicalJson(artifacts)
  if (process.env.UPDATE_GOLDENS === "1") writeFileSync(golden, bytes)
  expect(bytes).toBe(readFileSync(golden, "utf8"))
  expect(canonicalJson(compile())).toBe(bytes)
  if (!checker) return
  const admitted = checker.checkArtifactSet(JSON.parse(bytes))
  expect(admitted.diagnostics.diagnostics).toEqual([])
  expect(admitted.admitted).toBe(true)
  // A direct jump to a missing state is refused by lifecycle admission.
  const invalid = JSON.parse(bytes)
  invalid[1].payload.objectType.lifecycles[0].transitions[0].to = "missing"
  expect(checker.checkArtifactSet(invalid).admitted).toBe(false)
}, 30_000)
