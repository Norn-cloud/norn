import { expect, test } from "bun:test"
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { join, relative, sep } from "node:path"
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
  const scratch = join(homedir(), ".cache/agent-scratch/stress")
  mkdirSync(scratch, { recursive: true })
  const dir = mkdtempSync(join(scratch, "mutant-"))
  try {
    const source = readFileSync(join(root, sourcePath), "utf8").replace(/\r\n/g, "\n")
    const derive = '  derive: [{ field: "rejected", inputRequired: false,\n    expression: Norn.expression(LineInput, x => x.delivered - x.accepted) }],\n'
    expect(source.split(derive).length).toBe(2)
    const mutant = join(dir, "purchase.ts")
    writeFileSync(mutant, source.replace(derive, "").replace('"../../registry/core/src/physics"', JSON.stringify(relative(dir, join(root, "registry/core/src/physics")).split(sep).join("/"))))
    const compiled = compileDefinitions(mutant, { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath })
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const mutantPath = join(root, "fixtures/stress/purchase-mutant.json")
    if (process.env.NORN_UPDATE_STRESS === "1") writeFileSync(mutantPath, JSON.stringify(compiled.artifacts) + "\n")
    expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(JSON.parse(readFileSync(mutantPath, "utf8"))))
  } finally { rmSync(dir, { recursive: true, force: true }) }
  expect(canonicalJson(result.artifacts)).toBe(canonicalJson(JSON.parse(readFileSync(path, "utf8"))))
})
