/**
 * T7-EFX-4 (#2787): Effect Schema syntax → EntityIR v2 and closed schemas.
 *
 * Run: `NORN_CHECKER_WASM_PKG=/path/to/pkg bun --cwd tools/norn-ts test schema-lowering`.
 * Refresh goldens after an intentional change: `UPDATE_GOLDENS=1 bun --cwd tools/norn-ts test schema-lowering`,
 * then review the diff.
 */
import { describe, expect, test } from "bun:test"
import { readFileSync, readdirSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { E } from "../src/builder/index.ts"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitions, compileValueSchemas } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const UPDATE = process.env.UPDATE_GOLDENS === "1"
const DIR = join(TOOL_ROOT, "fixtures/schema")
// Independent text-valued stand-in resolves the fixture reference without a standards payload.
const REGISTRY = { base: [{
  irVersion: "0.1", artifactId: "fixture.currency.entity", kind: "entity", namespace: "fixture", name: "Synthetic currency", version: "0.1.0", lifecycle: "active", scope: {}, dependencies: [], provenance: { source: "system" },
  payload: { entityVersion: "2.0", propertyTypes: [{ name: "iso4217.currency_code", type: { kind: "text" } }] },
}] }
const options = (name: string) => ({
  scope: { orgId: "org_demo", environmentId: "dev" },
  sourcePath: `tools/norn-ts/fixtures/schema/${name}.source.ts`,
})

function golden(file: string, actual: string): void {
  const path = join(DIR, file)
  if (UPDATE) writeFileSync(path, actual)
  expect(actual).toBe(readFileSync(path, "utf8"))
}

function compiled(name: string) {
  const result = compileDefinitions(join(DIR, `${name}.source.ts`), options(name))
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
  return result
}

describe("Effect Schema -> EntityIR v2", () => {
  for (const name of ["invoice", "precision"]) {
    // Two independent compilations plus admission need cold-start headroom (#3340).
    test(`${name}: golden EntityIR, byte-stable, admitted by the Rust checker`, () => {
      const result = compiled(name)
      const json = canonicalJson(result.artifacts)
      golden(`${name}.ir.json`, json)
      // Round trip: the bytes re-parse to the same canonical text, and recompiling is byte-identical.
      expect(canonicalJson(JSON.parse(json))).toBe(json)
      expect(canonicalJson(compiled(name).artifacts)).toBe(json)
      if (checker) {
        const admitted = checker.checkArtifactSet(result.artifacts, REGISTRY)
        expect(admitted.diagnostics.diagnostics).toEqual([])
        expect(admitted.admitted).toBe(true)
      }
    }, 30_000)
  }

  test("the data-builder twin lowers to the same artifacts as the Effect Schema source", async () => {
    const twin = ((await import(join(DIR, "invoice.builder.ts"))) as { default: ReadonlyArray<E.EntityDef> }).default
    const built = twin.map((def) => {
      const r = E.lowerEntity(def, options("invoice"))
      if (!r.ok) throw new Error(JSON.stringify(r.diagnostics))
      return r.artifact
    })
    expect(canonicalJson(built)).toBe(canonicalJson(compiled("invoice").artifacts))
  })

  test("numeric precision: defaults keep their exact source text, never a binary float", () => {
    const text = canonicalJson(compiled("precision").artifacts)
    for (const exact of ['"0.1000000000000000000000000001"', '"9007199254740993"', '"12.5000"', '"-0.5"']) expect(text).toContain(exact)
    // The same values through a JS number would have been rounded.
    expect(String(0.1000000000000000000000000001)).not.toBe("0.1000000000000000000000000001")
    expect(String(9007199254740993)).not.toBe("9007199254740993")
  })

  test("missing vs null: Schema.NullOr is nullable; optionalKey and null defaults are refused (fixtures)", () => {
    const meter = JSON.parse(canonicalJson(compiled("precision").artifacts)) as Array<{ payload: { objectType: { properties: Array<{ id: string; constraints?: unknown }> } } }>
    const note = meter[0]!.payload.objectType.properties.find((p) => p.id === "note")
    expect(note?.constraints).toEqual({ nullable: true })
    const reading = meter[0]!.payload.objectType.properties.find((p) => p.id === "reading_id")
    expect(reading?.constraints).toBeUndefined()
  })

  test("unknown keys: the checker refuses an unknown member in the lowered IR (closed decode, #2819)", () => {
    if (!checker) return
    const [artifact] = JSON.parse(canonicalJson(compiled("precision").artifacts)) as Array<{ payload: { objectType: Record<string, unknown> } }>
    artifact!.payload.objectType.color = "blue"
    const refused = checker.checkArtifactSet([artifact], REGISTRY)
    expect(refused.admitted).toBe(false)
  })
})

describe("Effect Schema -> closed value schemas", () => {
  // Effect declarations are loaded into a fresh TypeScript program here too (#3340).
  test("golden closed schemas (Function I/O, action and RuleIR contracts)", () => {
    const result = compileValueSchemas(join(DIR, "closed.source.ts"))
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
    golden("closed.schemas.json", canonicalJson(result.schemas))
  }, 30_000)
})

const expected = (path: string): string => {
  const match = /^\/\/ expect: (NORN-FLOW-[A-Z-]+)$/m.exec(readFileSync(path, "utf8"))
  if (!match) throw new Error(`${path} has no "// expect: CODE" header`)
  return match[1]!
}

describe("rejected schema features (never approximated)", () => {
  const dir = join(DIR, "invalid")
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".ts")).sort()) {
    test(`entity: ${file}`, () => {
      const path = join(dir, file)
      const result = compileDefinitions(path, options("invalid"))
      expect(result.ok).toBe(false)
      if (result.ok) return
      expect(result.diagnostics.map((d) => d.code)).toContain(expected(path))
      for (const d of result.diagnostics) expect(d.start.line).toBeGreaterThan(0)
    })
  }
  const closedDir = join(DIR, "invalid-closed")
  for (const file of readdirSync(closedDir).filter((f) => f.endsWith(".ts")).sort()) {
    test(`closed: ${file}`, () => {
      const path = join(closedDir, file)
      const result = compileValueSchemas(path)
      expect(result.ok).toBe(false)
      if (result.ok) return
      expect(result.diagnostics.map((d) => d.code)).toContain(expected(path))
    })
  }
})
