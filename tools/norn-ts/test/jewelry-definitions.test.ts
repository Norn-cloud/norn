import { expect, test } from "bun:test"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { canonicalJson } from "../src/canonical.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
import { fineMass, purity, MAX_FINE_TICKS } from "../fixtures/jewelry/exact.ts"

const files = ["stock/stock.source.ts", "jewelry/jewelry.source.ts"].map(path => join(TOOL_ROOT, "fixtures", path))
const options = (path: string) => ({ sourcePath: `tools/norn-ts/${relative(TOOL_ROOT, path)}`, scope: { orgId: "org_demo", environmentId: "dev" } })
function compile() {
  return compileDefinitionFiles(files, options).flatMap(result => {
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    return result.artifacts
  })
}

test("jewelry specializes the stock masters and voucher lifecycle deterministically", () => {
  const artifacts = compile()
  expect(canonicalJson(artifacts)).toBe(canonicalJson(compile()))
  expect(artifacts).toHaveLength(25)
  const find = (id: string) => artifacts.find(value => value && typeof value === "object" && "artifactId" in value && value.artifactId === id)
  expect(find("stock.Item.entity")).toMatchObject({ provenance: { sourcePath: "tools/norn-ts/fixtures/stock/stock.source.ts" } })
  expect(find("jewelry.MetalLot.entity")).toMatchObject({ provenance: { sourcePath: "tools/norn-ts/fixtures/jewelry/jewelry.source.ts" } })
  expect(find("jewelry.MetalLot.entity")).toMatchObject({ payload: { objectType: { extends: "stock.Item" } } })
  for (const kind of ["CustodyReceipt", "CustodyReturn", "Refining"]) {
    expect(find(`jewelry.${kind}.entity`)).toMatchObject({ payload: { objectType: { extends: "stock.Voucher" } } })
  }
}, 60_000)

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
test.skipIf(!checker)("stock and jewelry definitions pass real checker admission", () => {
  if (!checker) throw new Error("Checker required")
  const result = checker.checkArtifactSet(compile())
  expect(result.diagnostics.diagnostics).toEqual([])
  expect(result.admitted).toBe(true)
}, 60_000)

test("exactPurity: equivalent karat and parts-per-thousand inputs share fine ticks", () => {
  expect(purity({ karat: "18.000000", fineness: "750.000" })).toEqual({ ok: true, value: 18_000_000n })
  expect(purity({ karat: "0.000024", fineness: "0.001" })).toEqual({ ok: true, value: 24n })
  expect(fineMass({ grossMicrograms: "1000000000", karat: "24" })).toEqual({ ok: true, value: 24_000_000_000_000_000n })
  expect(fineMass({ grossMicrograms: "1", karat: "0.000001" })).toEqual({ ok: true, value: 1n })
})

test("exactPurityRefusal: excess precision, invalid ranges and conflicts never round", () => {
  for (const input of [{}, { karat: "18.0000001" }, { fineness: "750.0001" }, { karat: "25" },
    { fineness: "1000.001" }, { karat: "0" }, { karat: "-1" }, { karat: "1e1" }, { karat: " 18" }]) {
    expect(purity(input)).toEqual({ ok: false, code: "purity_unrepresentable" })
  }
  for (const input of [{ karat: "25", fineness: "750" }, { karat: "18", fineness: "1001" },
    { karat: "0", fineness: "750" }, { karat: "18", fineness: "0" }]) {
    expect(purity(input)).toEqual({ ok: false, code: "purity_unrepresentable" })
  }
  expect(purity({ karat: "18", fineness: "749.999" })).toEqual({ ok: false, code: "purity_conflict" })
})

test("checkedFineMass: runtime Decimal boundary is exact, overflow refuses", () => {
  expect(fineMass({ grossMicrograms: String(MAX_FINE_TICKS), karat: "0.000001" })).toEqual({ ok: true, value: MAX_FINE_TICKS })
  expect(fineMass({ grossMicrograms: String(MAX_FINE_TICKS + 1n), karat: "0.000001" })).toEqual({ ok: false, code: "fine_mass_overflow" })
  expect(fineMass({ grossMicrograms: "1".repeat(129), karat: "24" })).toEqual({ ok: false, code: "fine_mass_overflow" })
  expect(fineMass({ grossMicrograms: String(MAX_FINE_TICKS / 24_000_000n), karat: "24" })).toEqual({ ok: true, value: MAX_FINE_TICKS / 24_000_000n * 24_000_000n })
  expect(fineMass({ grossMicrograms: String(MAX_FINE_TICKS / 24_000_000n + 1n), karat: "24" })).toEqual({ ok: false, code: "fine_mass_overflow" })
  for (const grossMicrograms of ["0", "-1", "1.1", "1e9"]) {
    expect(fineMass({ grossMicrograms, karat: "24" })).toEqual({ ok: false, code: "invalid_gross_mass" })
  }
})

test("refining_conserves/fineConserved: mixed purity maps to exact owner-partition fine ticks", () => {
  const input = fineMass({ grossMicrograms: "8", karat: "18" })
  const output = fineMass({ grossMicrograms: "5", karat: "24" })
  const loss = fineMass({ grossMicrograms: "1", karat: "24" })
  if (!input.ok || !output.ok || !loss.ok) throw new Error("Invalid correspondence fixture")
  expect(input.value).toBe(output.value + loss.value)
  expect(input.value).not.toBe(output.value + loss.value + 1n)
})
