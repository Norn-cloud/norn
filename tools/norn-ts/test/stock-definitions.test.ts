import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { canonicalJson } from "../src/canonical.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"

const file = join(TOOL_ROOT, "fixtures/stock/stock.source.ts")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "tools/norn-ts/fixtures/stock/stock.source.ts" }

test("stock schemas and lifecycle compile deterministically", () => {
  const compiled = compileDefinitions(file, options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const again = compileDefinitions(file, options)
  if (!again.ok) throw new Error(JSON.stringify(again.diagnostics))
  expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(again.artifacts))
  expect(compiled.artifacts).toHaveLength(20)
  expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(JSON.parse(readFileSync(join(TOOL_ROOT, "fixtures/stock/stock.artifacts.json"), "utf8"))))
  const voucher = compiled.artifacts.find(value => value && typeof value === "object" && "artifactId" in value && value.artifactId === "stock.Voucher.entity")
  expect(voucher).toMatchObject({ payload: { objectType: { lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["cancelled"], transitions: [
      { id: "submit", from: ["draft"], to: "submitted" }, { id: "cancel", from: ["submitted"], to: "cancelled" },
    ],
  }] } } })
}, 60_000)


if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

test.skipIf(!checker)("compiled definitions are admitted by the real checker", () => {
    if (!checker) throw new Error("Stock definition admission requires the real checker WASM")
    const compiled = compileDefinitions(file, options)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const result = checker.checkArtifactSet(compiled.artifacts)
    expect(result.diagnostics.diagnostics).toEqual([])
    expect(result.admitted).toBe(true)
}, 60_000)

if (checker) test("POST-BINDING: unresolved, mistyped and open declarations refuse", () => {
  const compiled = compileDefinitions(file, options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  if (!checker) throw new Error("POST-BINDING requires checker WASM")
  const original = JSON.stringify(compiled.artifacts)
  for (const [before, after] of [
    ['"warehouse":["destination"]', '"warehouse":["missing"]'],
    ['"stock":["destination","stock_account"]', '"stock":["item","name"]'],
    ['"srbnb":["destination","company","srbnb_account"]', '"srbnb":["destination","company","currency"]'],
    ['"kind":"decimal","scale":6', '"kind":"decimal","scale":2'],
    ['"target":"stock.Item"', '"target":"stock.Missing"'],
    ['"required":true', '"required":false'],
    ['"collection":"lines"', '"collection":"missing"'],
    ['"operation":"receipt"', '"operation":"transfer"'],
    ['"operation":"receipt"', '"operation":"receipt","templateId":"caller"'],
  ] satisfies ReadonlyArray<readonly [string, string]>) {
    const changed = original.replace(before, after)
    expect(changed).not.toBe(original)
    const result = checker.checkArtifactSet(JSON.parse(changed))
    expect(result.admitted).toBe(false)
  }
  const nullPost = compiled.artifacts.map(artifact => {
    if (!artifact || typeof artifact !== "object" || !("payload" in artifact)) return artifact
    const payload = artifact.payload
    if (!payload || typeof payload !== "object" || Array.isArray(payload) || !("stockPost" in payload)) return artifact
    return { ...artifact, payload: { ...payload, stockPost: null } }
  })
  expect(checker.checkArtifactSet(nullPost).admitted).toBe(false)
})
