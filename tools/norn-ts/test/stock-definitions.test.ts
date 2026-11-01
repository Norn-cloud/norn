import { expect, test } from "bun:test"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { canonicalJson } from "../src/canonical.ts"
import { checker } from "./checker.ts"

const file = join(TOOL_ROOT, "fixtures/stock/stock.source.ts")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "tools/norn-ts/fixtures/stock/stock.source.ts" }

test("stock schemas and lifecycle compile deterministically and are admitted", () => {
  if (!checker) throw new Error("Stock definition admission requires the real checker WASM")
  const compiled = compileDefinitions(file, options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const again = compileDefinitions(file, options)
  if (!again.ok) throw new Error(JSON.stringify(again.diagnostics))
  expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(again.artifacts))
  expect(compiled.artifacts).toHaveLength(16)
  const result = checker.checkArtifactSet(compiled.artifacts)
  expect(result.diagnostics.diagnostics).toEqual([])
  expect(result.admitted).toBe(true)
  const voucher = compiled.artifacts.find(value => value && typeof value === "object" && "artifactId" in value && value.artifactId === "stock.Voucher.entity")
  expect(voucher).toMatchObject({ payload: { objectType: { lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["cancelled"], transitions: [
      { id: "submit", from: ["draft"], to: "submitted" }, { id: "cancel", from: ["submitted"], to: "cancelled" },
    ],
  }] } } })
}, 60_000)
