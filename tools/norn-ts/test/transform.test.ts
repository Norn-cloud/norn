// ViewIR v1 authoring (#341): `Norn.transform` compiles to a `derived`
// artifact whose relational plan the Rust checker admits.
import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const ROOT = join(TOOL_ROOT, "fixtures/transforms")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "fixtures/transforms/shop.source.ts" }

type Artifact = { readonly artifactId: string; readonly kind: string; readonly version: string; readonly dependencies: ReadonlyArray<Record<string, unknown>>; readonly payload: Record<string, unknown> }

test("a transform compiles to a derived relational artifact pinned to its input types", () => {
  const compiled = compileDefinitions(join(ROOT, "shop.source.ts"), options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const artifacts = compiled.artifacts as unknown as Artifact[]
  const transform = artifacts.find((artifact) => artifact.kind === "derived")!
  expect(transform.artifactId).toBe("shop.revenue_by_region")
  expect(transform.dependencies).toEqual([
    { artifactId: "shop.Order.entity", kind: "entity", version: "0.1.0", required: true },
    { artifactId: "shop.Customer.entity", kind: "entity", version: "0.1.0", required: true },
  ])
  expect(transform.payload.inputs).toEqual([
    { artifactId: "shop.Order.entity", kind: "entity", version: "0.1.0", alias: "order" },
    { artifactId: "shop.Customer.entity", kind: "entity", version: "0.1.0", alias: "customer" },
  ])
  expect((transform.payload.calc as { kind: string; output: string }).kind).toBe("relational")
  expect((transform.payload.calc as { kind: string; output: string }).output).toBe("ranked")
  expect(transform.payload.outputSchema).toEqual({})
  expect(transform.payload.maintenance).toBe("recompute_on_read")
  if (checker) {
    const checked = checker.checkArtifactSet(artifacts)
    expect(checked.diagnostics.diagnostics).toEqual([])
    expect(checked.admitted).toBe(true)
  }
}, 30_000)

test("the checker types the plan: a sum of an undeclared measure refuses", () => {
  if (!checker) return
  const compiled = compileDefinitions(join(ROOT, "shop.source.ts"), options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const artifacts = structuredClone(compiled.artifacts) as unknown as Artifact[]
  const transform = artifacts.find((artifact) => artifact.kind === "derived")!
  const nodes = (transform.payload.calc as { nodes: Record<string, { measures?: Array<{ column?: string }> }> }).nodes
  nodes.weekly!.measures![0]!.column = "region"
  const checked = checker.checkArtifactSet(artifacts)
  expect(checked.admitted).toBe(false)
  expect(checked.diagnostics.diagnostics.map((d: { code: string }) => d.code)).toContain("derived.validation.type_mismatch")
}, 30_000)

test("inputs must be object types and ids must be dotted lower-case", () => {
  const dir = mkdtempSync(join(ROOT, "bad-"))
  try {
    const source = (id: string) => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const T = Norn.type({ name: "shop.T", title: "T", version: "0.1.0", properties: Schema.Struct({ a: Schema.String }) })
export const V = Norn.transform({ id: ${JSON.stringify(id)}, title: "V", version: "0.1.0", inputs: { t: T }, nodes: { t: { op: "scan", input: "t" } }, output: "t" })
`
    const bad = join(dir, "bad.ts")
    writeFileSync(bad, source("Shop.V"))
    const refused = compileDefinitions(bad, options)
    expect(refused.ok).toBe(false)
    const good = join(dir, "good.ts")
    writeFileSync(good, source("shop.v"))
    const compiled = compileDefinitions(good, options)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    expect((compiled.artifacts as unknown as Artifact[]).some((artifact) => artifact.kind === "derived")).toBe(true)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}, 30_000)
