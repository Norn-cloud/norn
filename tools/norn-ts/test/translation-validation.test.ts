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
