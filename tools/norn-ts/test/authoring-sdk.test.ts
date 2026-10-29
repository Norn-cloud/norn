import { describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { spawnSync } from "node:child_process"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import type { MappingSourceContract } from "../src/builder/mapping.ts"
import { sourceMappedDiagnostics } from "../src/checker-abi.ts"
import { compileModule } from "../src/compile-stdin.ts"
import { compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const root = join(TOOL_ROOT, "../..")
const scope = { orgId: "org_demo", environmentId: "dev" }
const native = process.env.NORN_BIN
if (process.env.CI && (!checker || !native)) throw new Error("SDK doc tests require NORN_CHECKER_WASM_PKG and NORN_BIN")

function compile(source: string) {
  const dir = mkdtempSync(join(tmpdir(), "norn-authoring-"))
  try {
    const file = join(dir, "rule.ts")
    writeFileSync(file, source)
    return compileDefinitions(file, { scope, sourcePath: "rule.ts" })
  } finally { rmSync(dir, { recursive: true, force: true }) }
}

const program = (expression: string) => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Task = Norn.type({name: "demo.Task", title: "Task", version: "1.0.0", properties: Schema.Struct({id: Schema.String}), identity: [{name: "primary", properties: ["id"]}]})
export const Rule = Norn.decision({
  id: "demo.rule.expression", title: "Expression", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({slice: Schema.Struct({}), event: Schema.Struct({kind: Schema.String})}),
  output: Schema.Struct({facts: Schema.Array(Schema.Struct({op: Schema.Literal("delete"), objectType: Schema.Literal("demo.Task"), key: Schema.String})), commands: Schema.Array(Schema.Struct({actionId: Schema.String, value: Schema.Boolean})), timers: Schema.Array(Schema.Struct({id: Schema.String}))}),
}, (input) => ({facts: [], commands: [{actionId: "probe", value: ${expression}}], timers: []}))`

function evaluate(artifacts: readonly unknown[], rule: string, inputs: readonly unknown[]): unknown[] {
  if (!native) throw new Error("NORN_BIN is required for evaluator parity")
  const dir = mkdtempSync(join(tmpdir(), "norn-eval-"))
  try {
    artifacts.forEach((a, i) => { writeFileSync(join(dir, `${i}.json`), JSON.stringify(a)) })
    const run = spawnSync(native, ["eval", "--defs", dir, "--org", scope.orgId, "--env", scope.environmentId, "--rule", rule], { input: inputs.map((i) => JSON.stringify(i)).join("\n") + "\n", encoding: "utf8", timeout: 30_000 })
    if (run.status !== 0) throw new Error(run.stderr + run.stdout)
    return run.stdout.trim().split("\n").map((line) => { const result = JSON.parse(line); if (!result.ok) throw new Error(line); return result.output })
  } finally { rmSync(dir, { recursive: true, force: true }) }
}

describe("Effect-profile business expressions through Rust admission and eval", () => {
  const examples = [
    ["arithmetic", "(2 + 3 * 4 - 1) % 5 === 3"],
    ["division with explicit null policy", "10 / 2 === 5"],
    ["strings", '"  Norn  ".trim().toLowerCase().startsWith("no") && "abc".toUpperCase().endsWith("BC")'],
    ["membership", '["ready", "open"].includes("open") && "norn".includes("or")'],
    ["nullable", '((true ? null : "value") ?? "fallback") === "fallback"'],
    ["date comparison", 'Norn.dateBefore("2026-11-06", "2026-11-08")'],
    ["conditional", 'input.event.kind === "start" ? true : false'],
  ] as const
  for (const [name, expression] of examples) test(name, () => {
    const result = compile(program(expression))
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    expect(result.sourceMap.length).toBeGreaterThan(0)
    if (checker) {
      const checked = checker.checkArtifactSet(result.artifacts)
      expect(checked.diagnostics.diagnostics.filter((d) => d.severity === "error")).toEqual([])
      expect(checked.admitted).toBe(true)
    }
    if (native) expect(evaluate(result.artifacts, "demo.rule.expression.rule", [{ slice: {}, event: { kind: "start" } }])).toEqual([{facts: [], commands: [{actionId: "probe", value: true}], timers: []}])
  }, 30_000)

  test("Rust errors map back to the exact returned TS expression", () => {
    const result = compile(program('Norn.unsafeZen<boolean>("(")'))
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    if (!checker) return
    const checked = checker.checkArtifactSet(result.artifacts)
    expect(checked.admitted).toBe(false)
    const mapped = sourceMappedDiagnostics(checked.diagnostics.diagnostics, result.sourceMap)
    expect(mapped.some((d) => d.source?.line === 8)).toBe(true)
  }, 30_000)

  test("rejected syntax explains the admitted alternative with its TS span", () => {
    const result = compile(program('Boolean(input.event.kind)'))
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.diagnostics[0]?.start.line).toBe(8)
      expect(result.diagnostics[0]?.code).toBe("NORN-FLOW-SYMBOL")
    }
  })
})

test("the public quickstart compiles through the SDK and Rust checker", () => {
  const pack = compilePack(join(TOOL_ROOT, "fixtures/quickstart"), { scope, root })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures, null, 2))
  expect(pack.artifacts).toHaveLength(1)
  expect(pack.flows).toHaveLength(0)
  if (checker) {
    const checked = checker.checkArtifactSet(pack.artifacts)
    expect(checked.diagnostics.diagnostics.filter((d) => d.severity === "error")).toEqual([])
    expect(checked.admitted).toBe(true)
  }
}, 60_000)

test("the MCP compiler rejects host imports and preserves profile diagnostics", () => {
  expect(compileModule('import x from "../../private.ts"', scope).ok).toBe(false)
  expect(compileModule('import x from "effect/../../private"', scope).ok).toBe(false)
  expect(compileModule('/// <reference path="/private.d.ts" />\nexport const x = 1', scope).ok).toBe(false)
  const result = compileModule(program('Norn.unsafeZen<boolean>("true")'), scope)
  expect(result.ok).toBe(true)
  if (result.ok && "warnings" in result) expect(result.warnings.some((w) => w.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
})

test("norn check admits a single module, warns on escapes, and maps Rust errors", () => {
  if (!checker) return
  const dir = mkdtempSync(join(tmpdir(), "norn-check-"))
  const file = join(dir, "rule.ts")
  const run = (source: string) => {
    writeFileSync(file, source)
    return spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "check", file, "--org", scope.orgId, "--env", scope.environmentId, "--json"], {cwd: root, encoding: "utf8", env: process.env, timeout: 30_000})
  }
  try {
    const good = run(program('Norn.unsafeZen<boolean>("true")'))
    expect(good.status).toBe(0)
    const admitted = JSON.parse(good.stdout)
    expect(admitted.admitted).toBe(true)
    expect(admitted.diagnostics.some((d: {code: string}) => d.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
    const bad = run(program('Norn.unsafeZen<boolean>("(")'))
    expect(bad.status).toBe(1)
    const rejected = JSON.parse(bad.stdout)
    expect(rejected.admitted).toBe(false)
    expect(rejected.diagnostics.some((d: {severity: string; source?: {line: number}}) => d.severity === "error" && d.source?.line === 8)).toBe(true)
  } finally { rmSync(dir, {recursive: true, force: true}) }
}, 60_000)

const sparseMappingProgram = (sourceContract: MappingSourceContract): string => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
const Row = Norn.type({name:"toy.Row",title:"Row",version:"1.0.0",properties:Schema.Struct({id:Schema.String}),identity:[{name:"primary",properties:["id"]}]})
const Identified = Norn.interfaceType({name:"toy.Identified",title:"Identified",version:"1.0.0",properties:Schema.Struct({id:Schema.String})})
export const Map = Norn.mapping({id:"toy.rows",title:"Rows",version:"1.0.0",source:{schemaId:"toy.schema",version:"1.0.0",sha256:"${"a".repeat(64)}"},sourceContract:${JSON.stringify(sourceContract)},interfaces:[Identified],maps:[{id:"row",source:{dataset:"Rows"},target:{type:Row,interface:Identified},subject:"input.id",sourceRecord:"input.id"}]})`

test("authored vendor mapping qualification survives syntax compilation", () => {
  const result = compile(sparseMappingProgram({kind:"vendor",system:"ToyVendor",versions:["1.2.3"],apiTerms:{uri:"https://toy.invalid/terms",sha256:"b".repeat(64)}}))
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.artifacts).toContainEqual(expect.objectContaining({kind:"mapping", payload:expect.objectContaining({extensions:{"norn.mapping.vendor/1.0.0":{system:"ToyVendor",versions:["1.2.3"],apiTerms:{uri:"https://toy.invalid/terms",sha256:"b".repeat(64)}}}})}))
})

test("explicitly generic authored packs do not assert vendor parity", () => {
  const result = compile(sparseMappingProgram({kind:"generic"}))
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.artifacts).toContainEqual(expect.objectContaining({kind:"mapping",payload:expect.objectContaining({extensions:{"norn.mapping.generic/1.0.0":{parity:"unqualified"}}})}))
})
