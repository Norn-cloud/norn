import { expect, test } from "bun:test"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
test("decimal action arguments lower to exact text wire contracts", () => {
  const result = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/decimal-input.source.ts"), {
    scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "decimal-input.source.ts",
  })
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.artifacts[1]).toMatchObject({ payload: { inputSchema: {
    properties: { amount: { type: "decimal", scale: 2 } },
  } } })
  if (checker) {
    const admission = checker.checkArtifactSet(result.artifacts)
    expect(admission.diagnostics.diagnostics).toEqual([])
    expect(admission.admitted).toBe(true)
  }
}, 30_000)
test("decimal output is refused before Rust admission", () => {
  const result = compileDefinitions(join(TOOL_ROOT, "fixtures/schema/invalid/decimal-output.ts"), {
    scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "decimal-output.ts",
  })
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.some((d) => d.code === "NORN-FLOW-SCHEMA-PROJECTION")).toBe(true)
})
