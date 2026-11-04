import * as E from "../src/builder/expr.ts"
import * as S from "../src/builder/schema.ts"
import { describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { spawnSync } from "node:child_process"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import type { MappingSourceContract } from "../src/builder/mapping.ts"
import { sourceMappedDiagnostics } from "../src/checker-abi.ts"
import { compileModule } from "../src/compile-stdin.ts"
import { artifactId, checkPack, compilePack, readArtifacts } from "../src/pack.ts"
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
test("TXN-RULE-1 authoring emits a required exact synchronous writer pin", () => {
  const fixture = readFileSync(join(TOOL_ROOT, "fixtures/definitions/create-linked.source.ts"), "utf8")
  const source = fixture.replace('  writes: [{ type: Order', '  synchronousRule: { artifactId: "demo.effects.rule", version: "1.0.0" },\n  writes: [{ type: Order')
  const result = compile(source)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.artifacts).toContainEqual(expect.objectContaining({kind: "action", dependencies: [{artifactId: "demo.effects.rule", version: "1.0.0", kind: "rule", required: true}], payload: expect.objectContaining({synchronousRule: {artifactId: "demo.effects.rule", version: "1.0.0"}})}))
  const combined = compile(source.replace('  synchronousRule:', '  dependencies: [{ artifactId: "demo.base.entity", version: "1.0.0" }],\n  synchronousRule:'))
  if (!combined.ok) throw new Error(JSON.stringify(combined.diagnostics))
  expect(combined.artifacts).toContainEqual(expect.objectContaining({kind: "action", dependencies: [{artifactId: "demo.base.entity", version: "1.0.0", kind: "entity", required: true}, {artifactId: "demo.effects.rule", version: "1.0.0", kind: "rule", required: true}]}))
})


test("TXN-RULE-1 execution fixture is generated from typed authoring", () => {
  const fixture = join(TOOL_ROOT, "fixtures/decisions/action-writer.source.ts")
  const pack = compilePack(fixture, { scope, root, modules: [fixture] })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  expect(pack.artifacts).toEqual(JSON.parse(readFileSync(join(root, "crates/norn-kernel/tests/fixtures/action-writer.json"), "utf8")))
})


test("TXN-ATOMIC-1 authoring retains explicit proposal link sources", () => {
  const fixture = readFileSync(join(TOOL_ROOT, "fixtures/definitions/create-linked.source.ts"), "utf8")
  const result = compile(fixture.replace('link: "customer",', 'source: { objectType: Order, idField: "number" }, link: "customer",'))
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.artifacts).toContainEqual(expect.objectContaining({kind: "action", payload: expect.objectContaining({createLinks: [expect.objectContaining({source: {objectType: "demo.Order", idField: "number"}})]})}))
})


test("declared array indexes refuse while bounded inline lists retain item typing", () => {
  const schema = S.record({rows: S.array(S.record({value: S.number}))})
  const scope = new Map([["input", {zen: "", schema}]])
  for (let index = 0; index < 32; index++) {
    const expression = E.ref("input", "rows", String(index), "value")
    expect(() => E.render(expression, scope)).toThrow("runtime array indexes")
    expect(() => E.typeOf(expression, scope)).toThrow("runtime array indexes")
  }
  const inline = new Map([["input", {inline: E.record({rows: E.list(E.record({value: E.lit(7)}))}), scope: new Map(), schema}]])
  expect(E.render(E.ref("input", "rows", "0", "value"), inline)).toBe("7")
  expect(E.typeOf(E.ref("input", "rows", "0", "value"), inline)).toEqual(S.number)
  for (const index of ["1", "-1", "01", "1.5", "9007199254740992", "length"]) expect(() => E.render(E.ref("input", "rows", index, "value"), inline)).toThrow()
})

// VAL-DECIMAL-BIND-1: compiling an interface preserves an unresolved group, never a scalar.
test("interface Decimal groups survive the admitted authoring profile", () => {
  const result = compile(`import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Conversion = Norn.interfaceType({name: "toy.Conversion", title: "Conversion", version: "1.0.0",
  properties: Schema.Struct({quantity: Norn.boundDecimal("conversion")})})`)
  expect(result).toMatchObject({ok: true, artifacts: [{payload: {interface: {
    properties: [{id: "quantity", type: {kind: "decimal-binding", group: "conversion"}}],
  }}}]})
})

test("FOLD-AUTHORING compiles closed step schemas and pins the helper", () => {
  const fixture = join(TOOL_ROOT, "fixtures/decisions/ordered-fold.source.ts")
  const source = readFileSync(fixture, "utf8")
  const result = compile(source)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  const encoded = JSON.stringify(result.artifacts)
  expect(encoded).toContain('"semanticsVersion":"1.0"')
  expect(encoded).toContain('"itemSchema":')
  expect(encoded).toContain('"accumulatorSchema":')
  expect(encoded).toContain('"artifactId":"example.fold.step.rule"')
  expect(result.warnings).toEqual([])
  const pack = compilePack(fixture, { scope, root, modules: [fixture] })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const golden = JSON.parse(readFileSync(join(root, "crates/norn-decisions/tests/fixtures/typed-ordered-fold.json"), "utf8"))
  expect(pack.artifacts).toEqual(golden)
  if (checker) expect(checker.checkArtifactSet(result.artifacts).admitted).toBe(true)
  for (const invalid of [
    source.replace('identity: "id"', 'identity: "missing"'),
    source.replace('rounding: "refuse"', 'rounding: "implicit"'),
    source.replace('initial: input.slice.initial', 'initial: { total: "wrong", count: 0 }'),
    source.replace('maxItems: 32', 'extra: true, maxItems: 32'),
    source.replace('maxItems: 32', 'maxItems: input.slice.initial.count'),
  ]) expect(compile(invalid).ok).toBe(false)
}, 60_000)

test("FOLD-OPERANDS refuses structural extra fields through const aliases", () => {
  const source = readFileSync(join(TOOL_ROOT, "fixtures/decisions/ordered-fold.source.ts"), "utf8")
  const literalItem = source.replace('id: Schema.String, amount:', 'id: Schema.Literal("row"), amount:').replace("items: input.slice.items", 'items: [{ id: "row", amount: 1 }]')
  expect(compile(literalItem).ok).toBe(true)
  expect(compile(literalItem.replace('id: "row"', 'id: "other"')).ok).toBe(false)
  const wideItems = source.replace("items: Schema.Array(Item)", "items: Schema.Array(Schema.Struct({ id: Schema.String, amount: Schema.Finite, source: Schema.String }))")
  const wideState = source.replace("initial: State", "initial: Schema.Struct({ total: Schema.Finite, count: Schema.Int, ids: Schema.Array(Schema.String), segments: Schema.Array(Schema.Array(Schema.Finite)), source: Schema.String })")
  const alias = (text: string) => text.replace("const state = Norn.orderedFold", "const rows = input.slice.items; const seed = input.slice.initial; const state = Norn.orderedFold").replace("items: input.slice.items, initial: input.slice.initial", "items: rows, initial: seed")
  for (const invalid of [wideItems, wideState, alias(wideItems), alias(wideState), alias(wideState).replace("items: rows", "items: []")]) {
    const result = compile(invalid)
    expect(result.ok).toBe(false)
    if (!result.ok) expect(result.diagnostics.some(d => d.code === "NORN-FLOW-TYPE")).toBe(true)
  }
}, 60_000)

test("CP3 allocation goldens come from core-aligned typed declarations", () => {
  const fixture = join(TOOL_ROOT, "fixtures/jewelry/cp3-folds.source.ts")
  const pack = compilePack(fixture, { scope, root, modules: [join(TOOL_ROOT, "fixtures/stock/stock.source.ts"), fixture] })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const authored = pack.artifacts.filter(a => a !== null && typeof a === "object" && "namespace" in a && a.namespace === "jewelry")
  expect(authored).toEqual(JSON.parse(readFileSync(join(root, "crates/norn-decisions/tests/fixtures/jewelry-folds.json"), "utf8")))
  if (checker) {
    const checked = checkPack(checker, pack.artifacts, readArtifacts(join(root, "registry/core/artifacts")).map(entry => entry.artifact).filter(artifact => /^core\.(Agent|AgentRole|Resource|Commitment|Hold|EconomicEvent|LedgerEffect|Claim)\.interface$/.test(artifactId(artifact)) || ["core.units", "core.value_types.entity", "std.value_types.entity"].includes(artifactId(artifact))))
    expect(checked.diagnostics.diagnostics.filter(d => d.severity === "error")).toEqual([])
    expect(checked.admitted).toBe(true)
  }
}, 60_000)
