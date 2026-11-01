import { expect, test } from "bun:test"
import { join, resolve } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { readArtifacts, checkPack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { canonicalJson } from "../src/canonical.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"

const file = join(TOOL_ROOT, "fixtures/stock/stock.source.ts")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "tools/norn-ts/fixtures/stock/stock.source.ts" }
const core = readArtifacts(join(resolve(TOOL_ROOT, "../.."), "registry/core/artifacts"))
  .map(({ artifact }) => artifact)
  .filter(artifact => typeof artifact === "object" && artifact !== null && !Array.isArray(artifact)
    && typeof artifact.artifactId === "string"
    && (/^core\.(Agent|AgentRole|Resource|Commitment|Hold|EconomicEvent|LedgerEffect|Claim)\.interface$/.test(artifact.artifactId)
      || ["core.value_types.entity", "std.value_types.entity"].includes(artifact.artifactId)))

function compile() {
  const result = compileDefinitions(file, options)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result.artifacts
}

test("stock schemas and lifecycle compile deterministically", () => {
  const compiled = compile()
  expect(canonicalJson(compiled)).toBe(canonicalJson(compile()))
  expect(compiled).toHaveLength(21)
  const voucher = compiled.find(value => value && typeof value === "object" && "artifactId" in value && value.artifactId === "stock.Voucher.entity")
  expect(voucher).toMatchObject({ payload: { objectType: { lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["cancelled"], immutable: ["submitted", "cancelled"], transitions: [
      { id: "submit", from: ["draft"], to: "submitted" }, { id: "cancel", from: ["submitted"], to: "cancelled" },
    ],
  }] } } })
}, 60_000)

test("application types implement core contracts; catalog items are not holdings", () => {
  const compiled = compile()
  expect(canonicalJson(compiled)).not.toContain("moving-average")
  for (const [name, contract] of [
    ["Company", "Agent"], ["Party", "Agent"], ["Holding", "Resource"],
    ["OrderLine", "Commitment"], ["Reservation", "Hold"], ["Movement", "EconomicEvent"], ["GLEntry", "LedgerEffect"],
  ]) {
    expect(compiled).toContainEqual(expect.objectContaining({
      artifactId: `stock.${name}.entity`,
      payload: expect.objectContaining({ objectType: expect.objectContaining({ implements: [`core.${contract}`] }) }),
    }))
  }
  for (const name of ["Movement", "GLEntry"]) {
    expect(compiled).toContainEqual(expect.objectContaining({
      artifactId: `stock.${name}.entity`,
      payload: expect.objectContaining({ objectType: expect.objectContaining({
        lifecycles: [{ property: "status", initial: ["posted"], terminal: ["posted"], immutable: ["posted"] }],
      }) }),
    }))
  }
  const item = compiled.find(value => value && typeof value === "object" && "artifactId" in value && value.artifactId === "stock.Item.entity")
  expect(item).not.toHaveProperty("payload.objectType.implements.0")
  const holding = compiled.find(value => value && typeof value === "object" && "artifactId" in value && value.artifactId === "stock.Holding.entity")
  expect(holding).toMatchObject({ payload: { objectType: { links: expect.arrayContaining(
    ["item", "location", "owner"].map(id => expect.objectContaining({ id, cardinality: { to: "one", from: "many", required: true } })),
  ) } } })
}, 60_000)

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

test.skipIf(!checker)("core-based application admits; omitted interface members refuse", () => {
  if (!checker) throw new Error("Stock definition admission requires the real checker WASM")
  expect(core).toHaveLength(10)
  const compiled = compile()
  const result = checkPack(checker, compiled, core)
  expect(result.diagnostics.diagnostics).toEqual([])
  expect(result.admitted).toBe(true)
  const malformed = JSON.parse(JSON.stringify(compiled))
  const holding = malformed.find((value: { artifactId: string }) => value.artifactId === "stock.Holding.entity")
  holding.payload.objectType.properties = holding.payload.objectType.properties.filter((value: { id: string }) => value.id !== "quantity")
  expect(checkPack(checker, malformed, core).admitted).toBe(false)
}, 60_000)
