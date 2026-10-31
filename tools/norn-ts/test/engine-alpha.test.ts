import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { readArtifacts } from "../src/pack.ts"
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

test("alpha SalesOrder compiles deterministically with read-only economic projections and refines Core", () => {
  const artifacts = compile()
  const bytes = canonicalJson(artifacts)
  if (process.env.UPDATE_GOLDENS === "1") writeFileSync(golden, bytes)
  expect(bytes).toBe(readFileSync(golden, "utf8"))
  expect(canonicalJson(compile())).toBe(bytes)
  expect(artifacts[0]).toMatchObject({ payload: { objectType: {
    extends: "commerce.SalesOrder",
    properties: expect.arrayContaining([
      { id: "invoiced", type: { kind: "boolean" }, computed: "self.invoice_count > 0" },
      { id: "completed", type: { kind: "boolean" }, computed: "self.invoice_count > 0 and self.allocated_total >= self.invoiced_total" },
    ]),
  } } })
  if (!checker) return
  const base = readArtifacts(join(root, "registry/core/artifacts")).map(({ artifact }) => artifact)
  const admitted = checker.checkArtifactSet(artifacts, { base })
  expect(admitted.diagnostics.diagnostics).toEqual([])
  expect(admitted.admitted).toBe(true)
  // A shortcut to approval is well-formed locally but widens Core's contract.
  const widened = JSON.parse(bytes)
  widened[0].payload.objectType.lifecycles[0].transitions[1].from = ["draft", "submitted"]
  expect(checker.checkArtifactSet(widened, { base }).admitted).toBe(false)
  const invalid = JSON.parse(bytes)
  invalid[0].payload.objectType.lifecycles[0].transitions[0].to = "missing"
  expect(checker.checkArtifactSet(invalid, { base }).admitted).toBe(false)
}, 30_000)
