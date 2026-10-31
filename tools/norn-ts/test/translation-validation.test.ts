import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { TRANSLATION_LIMITS, validateMarkingsTranslation } from "../src/translation-validation.ts"

const context = { orgId: "org_pilot", environmentId: "dev", sourcePath: "tools/norn-ts/fixtures/translation-validation/markings.source.ts" }
const path = join(TOOL_ROOT, "fixtures/translation-validation/markings.source.ts")
const source = readFileSync(path, "utf8")

// Only the candidate comes from the production compiler. Expected meaning is
// reconstructed by the validator from source; no expected projection is passed.
function compiledCandidate(): string {
  const compiled = compileDefinitions(path, { scope: { orgId: context.orgId, environmentId: context.environmentId }, sourcePath: context.sourcePath })
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  return JSON.stringify(compiled.artifacts)
}

interface Artifact {
  irVersion: string; artifactId: string; kind: string; namespace: string
  name: string; version: string; lifecycle: string
  scope: { orgId: string; environmentId: string }
  dependencies: unknown[]
  provenance: { source: string; sourcePath: string }
  payload: { entityVersion: string; markings: Array<{
    id: string; label: string; severity: string; category: string; mask: { kind: string; count?: number }
  }> }
}

describe("opt-in literal marking translation validation", () => {
  // Compile the candidate once; the validator reconstructs its own expected value.
  let cached: string | undefined
  const candidate = () => cached ??= compiledCandidate()

  test("accepts the complete compiler-produced artifacts and binds exact inputs", () => {
    const ir = candidate()
    const result = validateMarkingsTranslation(source, ir, context)
    expect(result.status).toBe("checked")
    if (result.status !== "checked") throw new Error(JSON.stringify(result))
    expect(result.evidence).toBe("structural")
    expect(result.artifacts).toBe(1)
    const commented = validateMarkingsTranslation(`${source}\n// same meaning, different source\n`, ir, context)
    const formatted = validateMarkingsTranslation(source, JSON.stringify(JSON.parse(ir), null, 2), context)
    for (const changed of [commented, formatted]) {
      expect(changed.status).toBe("checked")
      if (changed.status === "checked") expect(changed.binding).not.toBe(result.binding)
    }
    expect(validateMarkingsTranslation(source, ir, { ...context, sourcePath: "other.ts" }).status).toBe("mismatch")
    expect(validateMarkingsTranslation(source.replace('label: "Contact"', 'label: "Changed"'), ir, context).status).toBe("mismatch")
  }, 30_000)

  const mutations: Array<[string, (artifact: Artifact) => void]> = [
    ["IR version", (a) => { a.irVersion = "9" }],
    ["artifact id", (a) => { a.artifactId = "other.markings" }],
    ["artifact kind", (a) => { a.kind = "action" }],
    ["namespace", (a) => { a.namespace = "other" }],
    ["title", (a) => { a.name = "Other" }],
    ["version", (a) => { a.version = "2.0.0" }],
    ["lifecycle", (a) => { a.lifecycle = "active" }],
    ["org scope", (a) => { a.scope.orgId = "other" }],
    ["environment scope", (a) => { a.scope.environmentId = "other" }],
    ["dependencies", (a) => { a.dependencies.push({ artifactId: "extra" }) }],
    ["provenance source", (a) => { a.provenance.source = "system" }],
    ["provenance path", (a) => { a.provenance.sourcePath = "other.ts" }],
    ["entity version", (a) => { a.payload.entityVersion = "1.0" }],
    ["marking id", (a) => { a.payload.markings[0]!.id = "other" }],
    ["marking label", (a) => { a.payload.markings[0]!.label = "Other" }],
    ["marking severity", (a) => { a.payload.markings[0]!.severity = "low" }],
    ["marking category", (a) => { a.payload.markings[0]!.category = "other" }],
    ["mask kind", (a) => { a.payload.markings[0]!.mask.kind = "null" }],
    ["mask count", (a) => { a.payload.markings[1]!.mask.count = 5 }],
    ["omitted count", (a) => { delete a.payload.markings[1]!.mask.count }],
    ["extra count", (a) => { a.payload.markings[0]!.mask.count = 4 }],
    ["omitted title", (a) => { delete (a as Partial<Artifact>).name }],
    ["extra envelope field", (a) => { Object.assign(a, { sourceMap: { trusted: true } }) }],
    ["extra payload field", (a) => { Object.assign(a.payload, { objectType: {} }) }],
    ["omitted entry", (a) => { a.payload.markings.pop() }],
    ["entry order", (a) => { a.payload.markings.reverse() }],
    ["extra entry", (a) => { a.payload.markings.push(a.payload.markings[0]!) }],
  ]
  for (const [name, mutate] of mutations) test(`refutes changed ${name}`, () => {
    const artifacts = JSON.parse(candidate()) as Artifact[]
    mutate(artifacts[0]!)
    expect(validateMarkingsTranslation(source, JSON.stringify(artifacts), context).status).toBe("mismatch")
  }, 30_000)

  test("rejects wrong roots, omitted and extra artifacts; ignores object key order", () => {
    const artifacts = JSON.parse(candidate()) as Artifact[]
    for (const root of [null, {}, artifacts[0], [], [...artifacts, artifacts[0]]]) {
      expect(validateMarkingsTranslation(source, JSON.stringify(root), context).status).toBe("mismatch")
    }
    const reordered = Object.fromEntries(Object.entries(artifacts[0]!).reverse())
    expect(validateMarkingsTranslation(source, JSON.stringify([reordered]), context).status).toBe("checked")
  }, 30_000)

  test("compares decoded strings and exact integer boundaries", () => {
    const ir = candidate()
    expect(validateMarkingsTranslation(source.replace('label: "Contact"', 'label: "Cont\\u0061ct"'), ir, context).status).toBe("checked")
    for (const count of [1, 4_294_967_295]) {
      expect(validateMarkingsTranslation(source.replace("count: 4", `count: ${count}`), ir.replace('"count":4', `"count":${count}`), context).status).toBe("checked")
    }
  }, 30_000)

  for (const name of [
    "await", "private", "yield", "let", "eval", "arguments", "Norn", "async",
    "aw\\u0061it", "priv\\u0061te", "yi\\u0065ld", "l\\u0065t",
    "ev\\u0061l", "arg\\u0075ments", "N\\u006frn", "as\\u0079nc",
  ]) test(`declines reserved decoded binding ${name}`, () => {
    const changed = source.replace("const Privacy", `const ${name}`)
    expect(changed).not.toBe(source)
    expect(validateMarkingsTranslation(changed, candidate(), context).status).toBe("unsupported")
  }, 30_000)

  test("accepts ordinary Unicode and escaped binding names", () => {
    const ir = candidate()
    for (const name of ["Confidentialité", "Priv\\u0061cy"]) {
      const changed = source.replace("const Privacy", `const ${name}`)
      expect(changed).not.toBe(source)
      expect(validateMarkingsTranslation(changed, ir, context).status).toBe("checked")
    }
  }, 30_000)

  const decoratedMembers = [
    'label?: "Contact"',
    'label!: "Contact"',
    'label?!: "Contact"',
    '"label"?: "Contact"',
    '"label"!: "Contact"',
    ...["readonly", "public", "private", "protected", "static", "abstract", "override", "accessor", "async", "declare", "in", "out", "export"]
      .map((modifier) => `${modifier} label: "Contact"`),
  ]
  for (const member of decoratedMembers) test(`declines recovered property decoration ${member}`, () => {
    const changed = source.replace('label: "Contact"', member)
    expect(changed).not.toBe(source)
    // This exact reason also requires the pinned parser to retain the member
    // and reach its recovery-field guard, rather than decline a parse error.
    expect(validateMarkingsTranslation(changed, candidate(), context)).toMatchObject({
      status: "unsupported", reason: "decorated literal members are unsupported",
    })
  }, 30_000)

  test("declines malformed recovered property decoration as malformed syntax", () => {
    const changed = source.replace('label: "Contact"', 'readonly @decorate label: "Contact"')
    expect(changed).not.toBe(source)
    expect(validateMarkingsTranslation(changed, candidate(), context)).toMatchObject({
      status: "unsupported", evidence: "unknown", reason: "malformed syntax",
    })
  }, 30_000)

  test("retains ordinary comments and directive-like data", () => {
    const ir = candidate()
    for (const changed of [
      `\uFEFF\t// ordinary comment\n${source}`,
      `/* ordinary block comment\n/// <reference path="./not-a-directive.ts" />\n*/\n${source}`,
      `${source}\n// text mentioning @ts-ignore is ordinary prose\n`,
    ]) {
      expect(changed).not.toBe(source)
      expect(validateMarkingsTranslation(changed, ir, context).status).toBe("checked")
    }
    const text = '/// <reference path="./just-data.ts" />'
    const changedSource = source.replace('label: "Contact"', `label: ${JSON.stringify(text)}`)
    const artifacts = JSON.parse(ir) as Artifact[]
    artifacts[0]!.payload.markings[0]!.label = text
    expect(changedSource).not.toBe(source)
    expect(validateMarkingsTranslation(changedSource, JSON.stringify(artifacts), context).status).toBe("checked")
  }, 30_000)

  const unsupported: Array<[string, (s: string) => string]> = [
    ["missing import", (s) => s.replace('import { Norn } from "@norn/authoring"', "")],
    ["foreign module", (s) => s.replace("@norn/authoring", "./spoof")],
    ["import alias", (s) => s.replace("{ Norn }", "{ Norn as N }").replace("Norn.markings", "N.markings")],
    ["namespace import", (s) => s.replace("{ Norn }", "* as Norn")],
    ["extra import", (s) => `import { Schema } from "effect"\n${s}`],
    ["reference directive", (s) => `/// <reference path="./other.ts" />\n${s}`],
    ["BOM reference directive", (s) => `\uFEFF/// <reference path="./other.ts" />\n${s}`],
    ["Unicode whitespace reference directive", (s) => `\uFEFF\t\u00A0/// <reference path="./other.ts" />\n${s}`],
    ["Unicode line separator reference directive", (s) => `\u2028/// <reference path="./other.ts" />\n${s}`],
    ["BOM types directive", (s) => `\uFEFF/// <reference types="other" />\n${s}`],
    ["BOM lib directive", (s) => `\uFEFF/// <reference lib="esnext" />\n${s}`],
    ["BOM no-default-lib directive", (s) => `\uFEFF/// <reference no-default-lib="true" />\n${s}`],
    ["BOM AMD dependency directive", (s) => `\uFEFF/// <amd-dependency path="./other" />\n${s}`],
    ["BOM AMD module directive", (s) => `\uFEFF/// <amd-module name="other" />\n${s}`],
    ["non-leading reference comment", (s) => `${s}\n\uFEFF/// <reference path="./other.ts" />`],
    ["TS check pragma", (s) => `\uFEFF// @ts-check\n${s}`],
    ["TS nocheck pragma", (s) => `\uFEFF// @ts-nocheck\n${s}`],
    ["TS ignore directive", (s) => `\uFEFF// @ts-ignore\n${s}`],
    ["TS expect-error directive", (s) => `/* @ts-expect-error */\n${s}`],
    ["JSX pragma", (s) => `/** @jsx h */\n${s}`],
    ["JSX fragment pragma", (s) => `/** @jsxFrag Fragment */\n${s}`],
    ["JSX import source pragma", (s) => `/** @jsxImportSource other */\n${s}`],
    ["JSX runtime pragma", (s) => `/** @jsxRuntime automatic */\n${s}`],
    ["hashbang directive", (s) => `#!/usr/bin/env bun\n${s}`],
    ["type-only import", (s) => s.replace("import {", "import type {")],
    ["type-only import specifier", (s) => s.replace("{ Norn }", "{ type Norn }")],
    ["deferred import phase", (s) => s.replace("import {", "import defer {")],
    ["import modifiers", (s) => s.replace("import {", "export import {")],
    ["import attributes", (s) => s.replace('from "@norn/authoring"', 'from "@norn/authoring" with { type: "json" }')],
    ["shadow binding", (s) => s.replace("const Privacy", "const Norn")],
    ["duplicate binding", (s) => `${s}\nexport const Privacy = 1`],
    ["duplicate key", (s) => s.replace('title: "Privacy catalog",', 'title: "First", title: "Privacy catalog",')],
    ["escaped duplicate key", (s) => s.replace('title: "Privacy catalog",', '"\\u0074itle": "First", title: "Privacy catalog",')],
    ["duplicate marking id", (s) => s.replace('id: "pii.account"', 'id: "pii.contact"')],
    ["extra source member", (s) => s.replace('version: "0.1.0",', 'version: "0.1.0", ignored: true,')],
    ["wrong source root", (s) => s.replace("Norn.markings", "Norn.type")],
    ["mutable binding", (s) => s.replace("export const", "export let")],
    ["using binding", (s) => s.replace("export const", "export using")],
    ["await using binding", (s) => s.replace("export const", "export await using")],
    ["binding modifiers", (s) => s.replace("export const", "export declare const")],
    ["typed binding", (s) => s.replace("const Privacy", "const Privacy: unknown")],
    ["definite binding", (s) => s.replace("const Privacy", "const Privacy!")],
    ["optional call", (s) => s.replace("Norn.markings(", "Norn.markings?.(")],
    ["generic call", (s) => s.replace("Norn.markings(", "Norn.markings<unknown>(")],
    ["optional root", (s) => s.replace("Norn.markings", "Norn?.markings")],
    ["asserted root", (s) => s.replace("Norn.markings", "Norn!.markings")],
    ["private root", (s) => s.replace("Norn.markings", "Norn.#markings")],
    ["constant alias", (s) => s.replace("category: \"pii\"", "category: category")],
    ["spread", (s) => s.replace('mask: { kind: "null" }', 'mask: { ...{ kind: "null" } }')],
    ["shorthand", (s) => s.replace('label: "Contact"', "label")],
    ["computed key", (s) => s.replace('label: "Contact"', '["label"]: "Contact"')],
    ["property decorator", (s) => s.replace('label: "Contact"', '@decorate label: "Contact"')],
    ["optional metadata member", (s) => s.replace('title: "Privacy catalog"', 'title?: "Privacy catalog"')],
    ["definite mask member", (s) => s.replace('count: 4', 'count!: 4')],
    ["decorated mask member", (s) => s.replace('count: 4', 'readonly count: 4')],
    ["array hole", (s) => s.replace("markings: [", "markings: [, ")],
    ["function call", (s) => s.replace('label: "Contact"', 'label: dangerous()')],
    ["syntax recovery", (s) => s.replace('title: "Privacy catalog",', 'title: "Privacy catalog"')],
    ["prototype key", (s) => s.replace('label: "Contact"', '__proto__: "Contact"')],
    ["number rounding", (s) => s.replace("count: 4", "count: 9007199254740993")],
    ["fraction", (s) => s.replace("count: 4", "count: 4.0000000000000001")],
    ["exponent", (s) => s.replace("count: 4", "count: 4e0")],
    ["separator", (s) => s.replace("count: 4", "count: 0_4")],
    ["negative zero", (s) => s.replace("count: 4", "count: -0")],
    ["u32 overflow", (s) => s.replace("count: 4", "count: 4294967296")],
    ["zero count", (s) => s.replace("count: 4", "count: 0")],
  ]
  for (const [name, change] of unsupported) test(`declines unsupported ${name}`, () => {
    const changed = change(source)
    expect(changed).not.toBe(source)
    expect(validateMarkingsTranslation(changed, candidate(), context).status).toBe("unsupported")
  }, 30_000)

  test("inspects raw candidate keys and numeric spelling before comparing", () => {
    const ir = candidate()
    for (const changed of [
      ir.replace('"kind":"entity"', '"kind":"action","kind":"entity"'),
      ir.replace('"kind":"entity"', '"\\u006bind":"action","kind":"entity"'),
      ir.replace('"count":4', '"count":4.0000000000000001'),
      ir.replace('"count":4', '"count":9007199254740993'),
      ir.replace('"count":4', '"count":4e0'),
      ir.replace('"count":4', '"count":-0'),
      `${ir} trailing`,
    ]) expect(validateMarkingsTranslation(source, changed, context).status).toBe("unsupported")
  }, 30_000)

  test("budget exhaustion never becomes checked", () => {
    expect(validateMarkingsTranslation(" ".repeat(TRANSLATION_LIMITS.textUnits + 1), "[]", context).status).toBe("unknown")
    expect(validateMarkingsTranslation(source, " ".repeat(TRANSLATION_LIMITS.textUnits + 1), context).status).toBe("unknown")
    expect(validateMarkingsTranslation(";".repeat(TRANSLATION_LIMITS.tokens + 1), "[]", context).status).toBe("unknown")
    const nested = "[".repeat(TRANSLATION_LIMITS.depth + 1) + "0" + "]".repeat(TRANSLATION_LIMITS.depth + 1)
    expect(validateMarkingsTranslation(source, nested, context).status).toBe("unknown")
    expect(validateMarkingsTranslation(source, "[]", { ...context, sourcePath: "x".repeat(4_097) }).status).toBe("unknown")
  })
})
