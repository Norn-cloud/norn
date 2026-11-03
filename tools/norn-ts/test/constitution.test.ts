import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, relative, sep } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { canonicalJson } from "../src/canonical.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { artifactId, checkPack } from "../src/pack.ts"
import { checker } from "./checker.ts"

if (process.env.CI && !checker) throw new Error("MVP-CONSTITUTION requires the real Wasm checker in CI")

const root = join(TOOL_ROOT, "../..")
const sourcePath = "scripts/demo/constitution.source.ts"
const fixture = join(root, "crates/norn-cell/tests/fixtures/constitution.json")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath }

test("MVP-CONSTITUTION compiles removal and restoration of the loss writer", () => {
  const original = readFileSync(join(root, sourcePath), "utf8")
  const line = '    { field: "loss_fine_g", inputRequired: false, expression: Norn.expression(Measurements, x => x.input_fine_g - x.output_fine_g) },\n'
  expect(original.split(line).length).toBe(2)
  const dir = mkdtempSync(join(tmpdir(), "norn-constitution-"))
  try {
    const mutant = join(dir, "omitted.source.ts")
    const coreImport = relative(dir, join(root, "registry/core/src/physics")).split(sep).join("/")
    writeFileSync(mutant, original.replace(line, "").replace('"../../registry/core/src/physics"', JSON.stringify(coreImport)))
    const compile = (file: string) => {
      const result = compileDefinitions(file, options)
      if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
      expect(result.warnings).toEqual([])
      return result.artifacts
    }
    const withLoss = compile(join(root, sourcePath))
    const withoutLoss = compile(mutant)
    const contracts = (artifacts: typeof withLoss) => artifacts.filter(a => artifactId(a) !== "constitution.melt.convert.action")
    expect(canonicalJson(contracts(withoutLoss))).toBe(canonicalJson(contracts(withLoss)))
    expect(canonicalJson(withLoss)).not.toBe(canonicalJson(withoutLoss))
    expect(canonicalJson(compile(join(root, sourcePath)))).toBe(canonicalJson(withLoss))
    const generated = { withLoss, withoutLoss }
    if (process.env.NORN_UPDATE_CONSTITUTION_GOLDEN === "1") writeFileSync(fixture, JSON.stringify(generated) + "\n")
    expect(canonicalJson(generated)).toBe(canonicalJson(JSON.parse(readFileSync(fixture, "utf8"))))
    if (checker) {
      const base = ["core.units", "core.Process.interface", "core.Conversion.interface"].map(id => JSON.parse(readFileSync(join(root, `registry/core/artifacts/${id}.json`), "utf8")))
      for (const artifacts of [withLoss, withoutLoss]) {
        const checked = checkPack(checker, artifacts, base)
        expect(checked.diagnostics.diagnostics.filter(d => d.severity === "error")).toEqual([])
        expect(checked.admitted).toBe(true)
      }
    }
  } finally { rmSync(dir, { recursive: true, force: true }) }
}, 60_000)
