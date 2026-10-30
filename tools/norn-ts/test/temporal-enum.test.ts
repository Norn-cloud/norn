import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { compileModule } from "../src/compile-stdin.ts"
import * as SS from "../src/builder/source-schema.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const file = join(TOOL_ROOT, "fixtures/definitions/temporal-enum.source.ts")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "temporal-enum.source.ts" }
function compiled() {
  const result = compileDefinitions(file, options)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result
}

test("temporal and enum mapping syntax lowers without an escape or erased code set", () => {
  const result = compiled()
  expect(result.warnings).toEqual([])
  const artifacts = result.artifacts as any[]
  const source = artifacts.find(a => a.kind === "source_schema")
  expect(source.payload.codeSets).toEqual([{ id: "states", dataset: "items", field: "state", origin: "declared", values: [{ value: "open" }, { value: "closed", label: "Closed" }] }])
  const properties = artifacts.find(a => a.kind === "mapping").payload.maps[0].properties
  expect(properties.find((p: any) => p.property === "created").value).toContain("timestamp(input.created ?? '')")
  expect(properties.find((p: any) => p.property === "day").value).toBe("date(input.day)")
})

test("builder and compiler preserve declared and observed code-set origins", () => {
  for (const origin of ["declared", "observed_candidate"] as const) {
    const result = compileModule(readFileSync(file, "utf8").replace('origin: "declared", values:', `origin: "${origin}", values:`), options.scope)
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    const source = (result.artifacts as any[]).find(a => a.kind === "source_schema")
    const { format, ...data } = source.payload
    const twin = SS.lowerSourceSchema(SS.sourceSchema({ id: source.artifactId, title: source.name, version: source.version, ...data }), { ...options, sourcePath: "module.ts" })
    expect(twin.artifact).toEqual(source)
    expect(source.payload.codeSets[0].origin).toBe(origin)
  }
}, 30_000)

test.skipIf(!checker)("Rust admits the authored mapping with a receipt-pinned source and refuses candidate enums", () => {
  const artifacts = JSON.parse(JSON.stringify(compiled().artifacts)) as any[]
  const mapping = artifacts.find(a => a.kind === "mapping")
  const source = artifacts.find(a => a.kind === "source_schema")
  const pin = () => {
    const checked = checker!.checkArtifactSet(artifacts.filter(a => a !== mapping))
    if (source.payload.codeSets[0].origin === "inferred") {
      expect(checked.admitted).toBe(false)
      return
    }
    expect(checked.admitted).toBe(true)
    const receipt = checked.receipt as { artifactSet: { artifacts: { artifactId: string; sha256: string }[] } }
    mapping.payload.source.sha256 = receipt.artifactSet.artifacts.find(a => a.artifactId === source.artifactId)!.sha256
  }
  pin()
  expect(checker!.checkArtifactSet(artifacts).diagnostics.diagnostics).toEqual([])
  expect(checker!.checkArtifactSet(artifacts).admitted).toBe(true)
  for (const origin of ["inferred", "observed_candidate"]) {
    source.payload.codeSets[0].origin = origin
    pin()
    const checked = checker!.checkArtifactSet(artifacts)
    expect(checked.admitted).toBe(false)
    expect(checked.diagnostics.diagnostics.some(d => d.code === (origin === "inferred" ? "source_schema.validation.promoted_inference" : "mapping.model.expression_type"))).toBe(true)
  }
}, 30_000)

test("temporal conversion cannot extend closed contracts or admit unknown code-set keys", () => {
  const source = readFileSync(file, "utf8")
  const unknown = compileModule(source.replace('origin: "declared", values:', 'extra: true, origin: "declared", values:'), options.scope)
  expect(unknown.ok).toBe(false)
  const invalid = compileModule(`import { Schema } from "effect"; import { Norn } from "@norn/authoring";
export const Invalid = Norn.decision({ id: "temporal.invalid", title: "Invalid", version: "0.1.0", contract: "decide", scopeRequired: ["orgId", "environmentId"], input: Schema.Struct({ at: Norn.timestamp("utc") }), output: Schema.Struct({ facts: Schema.Array(Schema.Struct({})) }) }, input => ({ facts: [] }))`, options.scope)
  expect(invalid.ok).toBe(false)
  if (!invalid.ok && "diagnostics" in invalid) expect(invalid.diagnostics.some(d => d.code === "NORN-FLOW-SCHEMA-PROJECTION")).toBe(true)
})
