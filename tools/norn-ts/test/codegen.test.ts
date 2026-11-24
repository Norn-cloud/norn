import { expect, test } from "bun:test"
import { copyFileSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import ts from "typescript"
import { CodegenError, generateTypes, readArtifactSet } from "../src/codegen.ts"
import { admit, compilePack } from "../src/pack.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const ROOT = resolve(import.meta.dir, "..")
const REPO = resolve(ROOT, "../..")
const FIXTURES = join(ROOT, "fixtures/codegen")
const CORE = join(REPO, "registry/core/artifacts")
const DOMAINS = ["tracking", "ml"] as const
/** The shared value-type catalogs the fixture packs reference (`iso8601.*`, `core.time.*`). */
const CATALOGS = ["core.units", "core.value_types.entity", "std.value_types.entity", "core.time"]
const catalogs = () => CATALOGS.map((id) => JSON.parse(readFileSync(join(CORE, `${id}.json`), "utf8")) as unknown)

const packs = new Map<string, ReturnType<typeof compilePack>>()
function compiled(domain: string) {
  const cached = packs.get(domain)
  if (cached?.ok) return cached
  const pack = compilePack(join(FIXTURES, domain), { scope: { orgId: "org_a", environmentId: "dev" }, root: ROOT })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  packs.set(domain, pack)
  return pack
}

function cli(args: readonly string[]) {
  const run = Bun.spawnSync(["bun", join(ROOT, "src/cli.ts"), "codegen", ...args], { cwd: ROOT, stdout: "pipe", stderr: "pipe" })
  return { code: run.exitCode, stdout: run.stdout.toString(), stderr: run.stderr.toString() }
}

const refused = (artifacts: readonly unknown[], message: string, base: readonly unknown[] = catalogs()) => {
  expect(() => generateTypes(artifacts, { base })).toThrow(CodegenError)
  expect(() => generateTypes(artifacts, { base })).toThrow(message)
}

for (const domain of DOMAINS) {
  test(`CODEGEN-TS-1 (${domain}): one deterministic .d.ts per admitted set, byte for byte the committed golden`, () => {
    const pack = compiled(domain)
    if (checker) expect(admit(checker, { ...pack, baseArtifacts: catalogs() as typeof pack.baseArtifacts }).report.admitted).toBe(true)
    const text = generateTypes(pack.artifacts, { base: catalogs() })
    expect(text).toBe(readFileSync(join(FIXTURES, `${domain}.d.ts`), "utf8"))
    // Neither artifact order nor base order moves a byte.
    expect(generateTypes([...pack.artifacts].reverse(), { base: catalogs().reverse() })).toBe(text)
    // Types only: no runtime declaration of any kind.
    expect(text).not.toMatch(/^\s*export\s+(const|let|var|function|class|enum|default)\b/m)
    expect(text).not.toMatch(/\bdeclare\s+(const|let|var|function|class)\b/)
  })
}

test("CODEGEN-TS-2: --check passes on the golden and fails on drift or a missing file", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-codegen-"))
  try {
    const set = join(dir, "tracking.json"), base = join(dir, "base.json")
    writeFileSync(set, JSON.stringify(compiled("tracking").artifacts))
    writeFileSync(base, JSON.stringify(catalogs()))
    const out = join(dir, "tracking.d.ts")
    copyFileSync(join(FIXTURES, "tracking.d.ts"), out)
    expect(cli(["--ts", set, "--base", base, "--out", out, "--check"]).code).toBe(0)

    writeFileSync(out, readFileSync(out, "utf8").replace('"tracking.Parcel"', '"tracking.Parcel2"'))
    const drift = cli(["--ts", set, "--base", base, "--out", out, "--check"])
    expect(drift.code).toBe(1)
    expect(drift.stderr).toContain("is out of date")
    // --check never writes.
    expect(readFileSync(out, "utf8")).toContain('"tracking.Parcel2"')

    const missing = cli(["--ts", set, "--base", base, "--out", join(dir, "absent.d.ts"), "--check"])
    expect(missing.code).toBe(1)
    expect(missing.stderr).toContain("is missing")

    // Without --check the file is regenerated, after which --check passes again.
    expect(cli(["--ts", set, "--base", base, "--out", out]).code).toBe(0)
    expect(readFileSync(out, "utf8")).toBe(readFileSync(join(FIXTURES, "tracking.d.ts"), "utf8"))
    expect(cli(["--ts", set, "--base", base, "--out", out, "--check"]).code).toBe(0)

    // Without the base, a shared value type does not resolve: the run fails, it never guesses.
    const unresolved = cli(["--ts", set, "--out", out, "--check"])
    expect(unresolved.code).toBe(1)
    expect(unresolved.stderr).toMatch(/value type `iso8601\.[a-z]+` is in neither the set nor its base; pass its catalog with --base/)
    expect(cli(["--ts", set, "--check"]).code).toBe(2)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test("CODEGEN-TS-3: a shape outside the closed dialect, or two definitions of one name, fails the run", () => {
  const ml = () => structuredClone(compiled("ml").artifacts) as Array<{ artifactId: string; kind: string; payload: any }>
  const withInput = (field: string, schema: unknown) => {
    const set = ml()
    set.find((a) => a.artifactId === "ml.run.record.action")!.payload.inputSchema.properties[field] = schema
    return set
  }
  refused(withInput("epochs", { oneOf: [{ type: "integer" }, { type: "string" }] }), "ml.run.record.action#/inputSchema/properties/epochs: schema type undefined is not in the closed dialect")
  refused(withInput("epochs", { type: "integer", enum: [1, 2] }), "`enum` is not admitted in a closed `integer` schema")
  refused(withInput("epochs", { type: "integer", nornType: { kind: "decimal", scale: 0 } }), "`nornType` is not admitted in a closed `integer` schema")
  refused(withInput("model", { type: "string", nornType: { kind: "decimal", scale: 2 } }), "`nornType` is not admitted in a closed `string` schema")
  refused(withInput("learning_rate", { type: "number", nornType: { kind: "money" } }), "`nornType` is not a bounded Decimal refinement: it is not a decimal")
  // VAL-EXACT-1 bounds, as `decimal_refinement` admits them.
  const decimal = (nornType: object) => withInput("learning_rate", { type: "number", nornType: { kind: "decimal", ...nornType } })
  refused(decimal({ scale: 19 }), "scale is not an integer in 0..=18")
  refused(decimal({ scale: 1.5 }), "scale is not a whole number")
  refused(decimal({ scale: -1 }), "scale is not a whole number")
  refused(decimal({}), "scale is not a whole number")
  // Every member decodes as norn_ir::PropertyType::Decimal decodes it.
  refused(decimal({ scale: 2, unit: { code: "kg" } }), "unit names no unit system (ucum, qudt)")
  refused(decimal({ scale: 2, unit: { code: "kg", system: "si" } }), "unit names no unit system (ucum, qudt)")
  refused(decimal({ scale: 2, unit: { code: "kg", system: "ucum", label: "x" } }), "unit is not a unit reference")
  refused(decimal({ scale: 2, unit: { system: "ucum" } }), "unit has no code")
  refused(decimal({ scale: 2, rounding: "up" }), "rounding is not one of refuse, toward_zero, half_even, half_away_from_zero")
  refused(decimal({ scale: 2, rounding: null }), "rounding is not one of")
  refused(decimal({ scale: 2, additivity: { kind: "summable" } }), "additivity is not an aggregation contract")
  refused(decimal({ scale: 2, additivity: { kind: "ratio", numerator: "a" } }), "additivity is not an aggregation contract")
  refused(decimal({ scale: 2, currencySource: { property: "model", links: [1] } }), "currencySource is not a dimension path")
  refused(decimal({ scale: 2, unitSource: "model" }), "unitSource is not a list of property ids")
  expect(generateTypes(decimal({ scale: 2, rounding: "half_even", additivity: { kind: "ratio", numerator: "a", denominator: "b" } }), { base: catalogs() })).toContain('readonly "learning_rate": string')
  refused(decimal({ scale: 2, precision: 4 }), "`precision` is not a Decimal member")
  refused(decimal({ scale: 2, unit: { code: "kg", system: "ucum" }, currencySource: { property: "currency", links: [] } }), "a unit and a currency source are exclusive")
  refused(decimal({ scale: 2, perUnit: { code: "kg", system: "ucum" } }), "a per-unit rate needs a currency source and no unit")
  refused(decimal({ scale: 2, unit: { code: "k g", system: "ucum" } }), "unit has no code")
  refused(decimal({ scale: 2, currencySource: { property: " ", links: [] } }), "currencySource names no property")
  // The bounded JSON value leaf is an action-input form; an output may not declare one.
  const jsonOutput = ml()
  jsonOutput.find((a) => a.artifactId === "ml.run.record.action")!.payload.outputSchema.properties.run_id = { type: ["array", "boolean", "null", "number", "object", "string"] }
  refused(jsonOutput, "ml.run.record.action#/outputSchema/properties/run_id: the JSON value domain leaf is admitted in an action input only")
  // An entity decimal is bounded as norn_checker::property_types bounds it: 0..=28, or 0..=18 for
  // a per-unit rate or a declared rounding other than `refuse`.
  const entityDecimal = (type: object) => {
    const set = ml()
    const property = set.find((a) => a.artifactId === "ml.TrainingRun.entity")!.payload.objectType.properties.find((p: { id: string }) => p.id === "learning_rate")
    property.type = { kind: "decimal", ...type }
    return set
  }
  expect(generateTypes(entityDecimal({ scale: 28 }), { base: catalogs() })).toContain('readonly "learning_rate": string')
  expect(generateTypes(entityDecimal({ scale: 28, rounding: "refuse" }), { base: catalogs() })).toContain('readonly "learning_rate": string')
  refused(entityDecimal({ scale: 29 }), "a decimal's scale is not an integer in 0..=28")
  refused(entityDecimal({ scale: 2, unit: { code: "kg" } }), "a decimal is not well-formed: unit names no unit system")
  refused(entityDecimal({ scale: 2, unitSource: ["model"], unit: { system: "ucum", code: "kg" } }), "declares no unit source")
  refused(entityDecimal({ scale: 2, perUnit: { system: "ucum", code: "kg" } }), "a per-unit rate needs a currency source, no unit and a code")
  refused(entityDecimal({ scale: 19, rounding: "half_even" }), "a decimal's scale is not an integer in 0..=18 (a rate or a rounding decimal)")
  refused(entityDecimal({ scale: 19, perUnit: { system: "ucum", code: "kg" }, currencySource: { property: "model", links: [] } }), "a decimal's scale is not an integer in 0..=18 (a rate or a rounding decimal)")
  // The repository's own precision fixture declares an entity decimal of scale 28.
  const precision = readArtifactSet(join(ROOT, "fixtures/schema/precision.ir.json"))
  expect(generateTypes(precision, { base: readArtifactSet(CORE) })).toContain("export type ObjectMap")
  refused(withInput("model", { type: "string", format: "date" }), "`format` is not admitted in a closed `string` schema")
  refused(withInput("model", { type: ["object", "null"], properties: {}, additionalProperties: false }), "a nullable `object` is not admitted in an action input")
  refused(withInput("model", { type: "object", properties: {} }), "an object schema is not closed")
  refused(withInput("model", { type: "object", additionalProperties: false }), "an object schema has no `properties` object")
  // Display labels are an action-input form; an output schema is the bare dialect.
  const labelledOutput = ml()
  labelledOutput.find((a) => a.artifactId === "ml.run.record.action")!.payload.outputSchema.properties.run_id.title = "Run"
  refused(labelledOutput, "ml.run.record.action#/outputSchema/properties/run_id: `title` is not admitted in a closed `string` schema")

  const notObject = ml()
  notObject.find((a) => a.kind === "action")!.payload = ["not", "an", "object"]
  refused(notObject, "an action payload is not an object")
  const strayEntity = ml()
  strayEntity.find((a) => a.kind === "entity")!.payload.bogus = true
  refused(strayEntity, "entity payload member `bogus` is not EntityIR v2")
  const noShape = ml()
  noShape.find((a) => a.kind === "entity")!.payload = { entityVersion: "2.0" }
  refused(noShape, "an entity payload declares no object type, interface, extension or catalog")
  const v1 = ml()
  v1.find((a) => a.kind === "entity")!.payload = { fields: [], objectKind: "ml.TrainingRun" }
  refused(v1, "EntityIR v1 is not typed by codegen")

  // A second action or object type of one name, from any artifact, in any order.
  const twice = ml()
  const copy = structuredClone(twice.find((a) => a.artifactId === "ml.run.record.action")!)
  copy.artifactId = "ml.run.record.again.action"
  refused([...twice, copy], "action `ml.run.record` is also defined by")
  refused([copy, ...twice], "action `ml.run.record` is also defined by")

  // A value type defined twice with different content is refused, never resolved last-wins.
  const clash = structuredClone(catalogs()[2]) as { artifactId: string; payload: { propertyTypes: Array<{ name: string; type: unknown }> } }
  clash.artifactId = "fixture.value_types.entity"
  clash.payload.propertyTypes = clash.payload.propertyTypes.filter((t) => t.name === "iso8601.date").map((t) => ({ ...t, type: { kind: "text" } }))
  const tracking = compiled("tracking").artifacts
  refused(tracking, "value type `iso8601.date` is also defined by", [...catalogs(), clash])
  refused(tracking, "value type `iso8601.date` is also defined by", [clash, ...catalogs()])
  // An identical redefinition is the same definition: the same types (the digest names its source).
  const identical = structuredClone(catalogs()[2]) as { artifactId: string }
  identical.artifactId = "fixture.identical.entity"
  const types = (text: string) => text.replace(/^export type DefinitionsDigest = .*$/m, "")
  expect(types(generateTypes(tracking, { base: [...catalogs(), identical] }))).toBe(types(readFileSync(join(FIXTURES, "tracking.d.ts"), "utf8")))

  // Text interpolated into a JSDoc comment can never close it early.
  const hostile = ml()
  const record = hostile.find((a) => a.artifactId === "ml.run.record.action")!.payload
  record.title = "Close */ export const pwned = 1 /*"
  record.inputSchema.properties.learning_rate = { type: "number", title: "rate */ x", nornType: { kind: "decimal", scale: 2, unit: { system: "ucum", code: "*/kg" } } }
  const escaped = generateTypes(hostile, { base: catalogs() })
  for (const line of escaped.split("\n").filter((l) => l.includes("/**"))) expect(line.indexOf("*/")).toBe(line.length - 2)
  expect(escaped).toContain("Close *\\/ export const pwned = 1 /*")
  expect(escaped).toContain("unit *\\/kg")
  expect(escaped).not.toMatch(/^\s*export\s+const/m)

  // Extension members never shadow a member of their target, whichever artifact comes first.
  const entityV2 = readArtifactSet(join(REPO, "fixtures/ir/entity-v2/artifact-set")) as Array<{ artifactId: string; kind: string; payload: any }>
  const shadow = structuredClone(entityV2.find((a) => a.payload.objectTypeExtension !== undefined)!)
  shadow.artifactId = "acme.retail.Sku.extension.shadow"
  shadow.payload.objectTypeExtension.namespace = "beta"
  refused([...entityV2, shadow], "shadows a member of `retail.Sku`", [])
  refused([shadow, ...entityV2], "shadows a member of `retail.Sku`", [])
})

test("CODEGEN-TS-5: registry packs type over their base: shared value types, extensions and JSON leaves resolve", () => {
  const core = readArtifactSet(CORE)
  const packs = join(REPO, "registry/packs")
  const typed = readdirSync(packs, { withFileTypes: true }).filter((d) => d.isDirectory() && existsSync(join(packs, d.name, "artifacts"))).map((d) => d.name).sort()
  expect(typed).toContain("sources")
  expect(typed).toContain("time")
  for (const pack of typed) {
    const text = generateTypes(readArtifactSet(join(packs, pack, "artifacts")), { base: core })
    expect(text.startsWith("// Generated by `norn codegen --ts`.")).toBe(true)
  }
  // registry/packs/sources reads `iso8601.timestamp` from registry/core; without the base it cannot.
  expect(() => generateTypes(readArtifactSet(join(packs, "sources/artifacts")))).toThrow("value type `iso8601.timestamp` is in neither the set nor its base")
  // A `json-ext` property is any JSON value.
  const entitlement = core.filter((a) => (a as { artifactId: string }).artifactId === "core.Entitlement.entity")
  expect(entitlement).toHaveLength(1)
  expect(generateTypes(entitlement, { base: core })).toMatch(/readonly "[a-z_]+": NornJson/)
  // An extension's members join their target type.
  const sku = generateTypes(readArtifactSet(join(REPO, "fixtures/ir/entity-v2/artifact-set")))
  expect(sku).toContain('    readonly "acme__shelf_label": string | null')
  // A JSON value domain leaf in an action input.
  const ml = structuredClone(compiled("ml").artifacts) as Array<{ artifactId: string; payload: any }>
  ml.find((a) => a.artifactId === "ml.run.start.action")!.payload.inputSchema.properties.tags = { type: ["array", "boolean", "null", "number", "object", "string"] }
  expect(generateTypes(ml, { base: catalogs() })).toContain('readonly "tags": NornJson')
})

test("CODEGEN-TS-6: the caller's input excludes actor-derived fields and makes derived ones optional (ACTION-ACTOR-1)", () => {
  const text = readFileSync(join(FIXTURES, "tracking.d.ts"), "utf8")
  const input = text.slice(text.indexOf("readonly input: {"), text.indexOf("readonly output: {"))
  // `fromCaller: ["registered_by"]`: the kernel refuses any caller value, so the type offers none.
  expect(input).not.toContain('"registered_by"')
  // `inputRequired: false` derivation: the caller may leave it out.
  expect(input).toContain('readonly "label"?: string')
  // The stored object still carries both.
  const objects = text.slice(text.indexOf("export type ObjectMap"))
  expect(objects).toContain('readonly "registered_by": string')
  expect(objects).toContain('readonly "label": string')

  // `norn_expr::expression_read_paths` counts the root, closures, sequences and assignments as
  // possible actor reads; codegen fails closed on each of them lexically.
  const labelWith = (expression: string) => {
    const set = structuredClone(compiled("tracking").artifacts) as Array<{ artifactId: string; payload: any }>
    const action = set.find((a) => a.artifactId === "tracking.parcel.register.action")!
    action.payload.derive = action.payload.derive.map((step: { field: string }) => step.field === "label" ? { ...step, expression } : step)
    const generated = generateTypes(set, { base: catalogs() })
    const caller = generated.slice(generated.indexOf("readonly input: {"), generated.indexOf("readonly output: {"))
    return caller.includes('readonly "label"?: string') ? "optional" : caller.includes('"label"') ? "required" : "dropped"
  }
  // The kernel decides on the parsed ZEN AST (`collect_paths`); codegen parses the same way.
  // Drops: a literal `actor…` path anywhere (a computed member, an interval bound, a template
  // expression), `$root`, a closure builtin, an assignment, and anything that does not lex or parse:
  // ZEN strings have no escapes (`"a\"actor"` is an unterminated string), ZEN has no optional
  // chaining (`input?.actor`), no bare ranges (`0..10`), no postfix on an object literal, and only
  // its own builtins and date methods (`factor(…)`, `.notAMethod()` are parse errors).
  for (const expression of [
    "actor.subjectId", "actor", "$root.actor.subjectId", "sum(map(input.lines, #))", "count(input.lines, # > 0)",
    "filter(input.tags, true)", "flatMap(input.lines, [1])", "x = actor; x", "a = 1; a", "input.lines[actor.index]",
    "input.weight in [0..actor.max_weight]", "input.n in [0..count(input.lines, true)]", "`${actor.subjectId}`", "{[actor.k]: 1}",
    'input.label ?? "a\\"actor"', 'input.label ?? "unterminated', "input.label ?? @", "input.label +", "(input.label", "[1, 2",
    "factor(input.id)", "input?.actor", "0..10", "{ a: input.x }.a", "input.d.notAMethod()",
  ]) expect([expression, labelWith(expression)]).toEqual([expression, "dropped"])
  // Keeps: names inside strings, unquoted object keys (strings to ZEN), members named `actor`, a bare
  // `$` or top-level `#` (identifiers, not the root or a closure pointer), builtins, date methods,
  // intervals, slices, conditionals, and operators.
  for (const expression of [
    'input.label ?? "actor"', "input.label ?? 'actor'", 'input.label ?? "count("', "input.label ?? '$root #; ='", "`label ${input.parcel_id}`",
    "{actor: input.label}", "{ actor: { nested: [input.a, input.b] } }", "input.actor", "$.actor", "$", "#", "actor_id",
    "input.label ?? input.parcel_id", "input.tags.map", "upper(input.label)", "input.d.isBefore(input.e)", "input.weight in [0..10]",
    "[1, 2][0]", "input.list[1:2]", "(input.a + 1) * 2", "not input.flag", "!input.flag", "input.a ? input.b : input.c",
    "input.weight >= 1", "input.a == input.b", "input.a != 2", "input.a <= 3", "input.label ?? null", "input.x in ['a', 'b']",
    "input.label == 'x' and input.y > 2 or not input.z",
  ]) {
    expect([expression, labelWith(expression)]).toEqual([expression, "optional"])
  }
})

test("CODEGEN-TS-4: generated maps type the client: unknown actions, wrong fields and JS numbers for decimals do not compile", () => {
  const usage = join(FIXTURES, "usage.ts")
  const program = ts.createProgram([usage], {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler,
    lib: ["lib.esnext.d.ts", "lib.dom.d.ts"], types: [], strict: true, noUncheckedIndexedAccess: true,
    exactOptionalPropertyTypes: true, noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true, verbatimModuleSyntax: true,
    erasableSyntaxOnly: true,
  })
  const diagnostics = ts.getPreEmitDiagnostics(program).map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n"))
  // Zero diagnostics means every `@ts-expect-error` line is a real error (TS2578 otherwise).
  expect(diagnostics).toEqual([])
  expect(readFileSync(usage, "utf8").match(/^\/\/ @ts-expect-error/gm)?.length).toBe(22)
})
