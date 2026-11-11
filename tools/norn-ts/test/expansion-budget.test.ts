import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { BudgetExceeded, ExpansionBudget } from "../src/builder/budget.ts"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"

// #1110: a pack of modules that each fit the work bound compiles; the output
// byte bound still spans the whole pack.
test("work is bounded per module while output stays pack-wide", () => {
  const pack = new ExpansionBudget({ maxWork: 100, maxOutputBytes: 1_000 })
  for (let module = 0; module < 3; module++) {
    pack.beginModule()
    pack.reserve(60)
  }
  const single = new ExpansionBudget({ maxWork: 100, maxOutputBytes: 1_000 })
  single.reserve(60)
  expect(() => single.reserve(60)).toThrow(BudgetExceeded)

  const output = new ExpansionBudget({ maxWork: 1_000_000, maxOutputBytes: 64 })
  output.beginModule()
  output.addOutput("x".repeat(40))
  output.beginModule()
  expect(() => output.addOutput("y".repeat(40))).toThrow(BudgetExceeded)
})

/** A synthetic public type with `width` text properties. */
const typeSource = (name: string, width: number): string => `export const ${name} = Norn.type({
  name: "packbudget.${name}", title: "${name}", version: "0.1.0",
  properties: Schema.Struct({ ${Array.from({ length: width }, (_, i) => `p${i}: Schema.String`).join(", ")} }),
})
`
const moduleSource = (types: ReadonlyArray<string>): string =>
  `import { Schema } from "effect"\nimport { Norn } from "@norn/authoring"\n${types.map((name) => typeSource(name, 24)).join("")}`

// #1110, pack level: modules that each fit the work bound compile together
// even when their sum is over it; one module over the bound fails, by itself.
test("a pack of modules is bounded per module, and an oversized module fails alone", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-pack-budget-"))
  try {
    const write = (file: string, types: ReadonlyArray<string>): string => {
      const path = join(dir, file)
      writeFileSync(path, moduleSource(types))
      return path
    }
    const modules = ["A", "B", "C"].map((name) => write(`${name}.ts`, [name]))
    const options = (path: string) => ({ scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: path })
    // The work one module charges, measured, not guessed.
    const work = new Map<string, number>()
    const alone = compileDefinitionFiles(modules, options, {}, { onModuleWork: (path, used) => work.set(path, used) })
    expect(alone.every((result) => result.ok)).toBe(true)
    const each = modules.map((path) => work.get(path)!)
    const bound = Math.floor(Math.max(...each) * 1.5)
    // The case the fix is for: every module fits, the pack does not.
    expect(each.every((used) => used > 0 && used <= bound)).toBe(true)
    expect(each.reduce((sum, used) => sum + used, 0)).toBeGreaterThan(bound)
    const pack = compileDefinitionFiles(modules, options, { maxExpansionWork: bound })
    expect(pack.map((result) => result.ok)).toEqual([true, true, true])
    // One module holding all three types is over the bound, and only it fails.
    const oversized = write("D.ts", ["D", "E", "F"])
    const mixed = compileDefinitionFiles([modules[0]!, oversized, modules[1]!], options, { maxExpansionWork: bound })
    expect(mixed.map((result) => result.ok)).toEqual([true, false, true])
    const refused = mixed[1]!
    if (refused.ok) throw new Error("the oversized module compiled")
    expect(refused.diagnostics.map((diagnostic) => diagnostic.code)).toEqual(["NORN-FLOW-UNBOUNDED"])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
  // Three batch compiles (three TypeScript programs): an explicit budget, not
  // bun's 5 s default, so a slow runner cannot flake it (#1102).
}, 60_000)
