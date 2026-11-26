import { describe, expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"
import { runExamples, type CompiledExample } from "../src/examples.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const REPO = join(TOOL_ROOT, "../..")
const COMPUTE_UNITS_DIR = join(REPO, "registry/packs/compute-units")
const ANALYTICS_DIR = join(REPO, "registry/packs/analytics")

function compilePackDir(dir: string): { readonly artifacts: any[]; readonly examples: CompiledExample[] } {
  const files = readdirSync(dir)
    .filter((f) => f.endsWith(".ts"))
    .sort()
    .map((f) => join(dir, f))
  const registryPackagePath = relative(REPO, dir).split("\\").join("/")
  const results = compileDefinitionFiles(
    files,
    (path) => ({
      scope: {},
      sourcePath: relative(REPO, path),
      registryPackagePath,
    }),
    {},
    { forbidUnsafeZen: true },
  )
  const artifacts: any[] = []
  const examples: CompiledExample[] = []
  for (const result of results) {
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
    expect(result.warnings).toEqual([])
    artifacts.push(...(result.artifacts as any[]))
    examples.push(...result.examples)
  }
  return { artifacts, examples }
}

function loadCoreBase(): any[] {
  const core = join(REPO, "registry/core/artifacts")
  const physics = ["Agent", "AgentRole", "Resource", "Commitment", "Hold", "EconomicEvent", "LedgerEffect", "Claim"]
  return [
    ...physics.map((name) => `core.${name}.interface.json`),
    "core.value_types.entity.json",
    "std.value_types.entity.json",
    "core.units.json",
    "core.time.json",
  ].map((name) => JSON.parse(readFileSync(join(core, name), "utf8")))
}

describe("compute-units and analytics packs", () => {
  const computeUnits = compilePackDir(COMPUTE_UNITS_DIR)
  const analytics = compilePackDir(ANALYTICS_DIR)
  const allArtifacts = [...computeUnits.artifacts, ...analytics.artifacts]
  const allExamples = [...computeUnits.examples, ...analytics.examples]
  const byId = new Map(allArtifacts.map((a) => [a.artifactId as string, a]))

  test("compiled artifacts match checked-in JSON files byte-for-byte", () => {
    for (const [dir, compiled] of [
      [COMPUTE_UNITS_DIR, computeUnits],
      [ANALYTICS_DIR, analytics],
    ] as const) {
      const outDir = join(dir, "artifacts")
      const committed = readdirSync(outDir).sort()
      const expectedFiles = compiled.artifacts.map((a) => `${a.artifactId}.json`).sort()
      expect(committed).toEqual(expectedFiles)
      for (const artifact of compiled.artifacts) {
        const rendered = `${JSON.stringify(artifact, null, 2)}\n`
        const onDisk = readFileSync(join(outDir, `${artifact.artifactId}.json`), "utf8")
        expect(onDisk).toBe(rendered)
      }
    }
  })

  test("compute-units catalog covers all required UCUM, IEC 80000-13, AI, and ISO 80000 codes", () => {
    const computeDef = byId.get("units.compute.UnitDefinition.entity")!.payload.objectType
    const computeCodes = computeDef.properties.find((p: any) => p.id === "code").type.values.map((v: any) => v.value)
    expect(computeCodes).toEqual([
      "ms", "us", "ns",
      "bit", "By", "KiBy", "MiBy", "GiBy", "TiBy", "kBy", "MBy", "GBy", "TBy",
      "{token}", "{input_token}", "{output_token}", "{request}",
      "{vCPU}.s", "{vCPU}.h", "{GPU}.s", "{GPU}.h", "{seat}",
    ])

    const physicalDef = byId.get("units.physical.UnitDefinition.entity")!.payload.objectType
    const physicalCodes = physicalDef.properties.find((p: any) => p.id === "code").type.values.map((v: any) => v.value)
    expect(physicalCodes).toEqual(["m", "cm", "mm", "km", "m2", "L", "mL", "m3", "J", "kW.h"])
  })

  test("compute REA types implement core interfaces and bind exact Norn.rate pricing", () => {
    expect(byId.get("compute.TokenPool.entity")!.payload.objectType.implements).toEqual(["core.Resource"])
    expect(byId.get("compute.UsageEvent.entity")!.payload.objectType.implements).toEqual(["core.EconomicEvent"])
    expect(byId.get("compute.QuotaHold.entity")!.payload.objectType.implements).toEqual(["core.Hold"])
    expect(byId.get("compute.CapacityCommitment.entity")!.payload.objectType.implements).toEqual(["core.Commitment"])

    const priceAction = byId.get("compute.usage.price.action")!
    expect(priceAction.dependencies).toEqual([
      { artifactId: "units.compute.UnitDefinition.entity", kind: "entity", required: true, version: "0.1.0" },
    ])
    expect(priceAction.payload.derive).toEqual([
      { field: "billed_amount", expression: "input.rate_per_token * input.token_quantity", inputRequired: false },
    ])
    expect(priceAction.payload.inputSchema.properties.rate_per_token.nornType).toEqual({
      kind: "decimal",
      scale: 6,
      currencySource: { links: [], property: "billing_currency" },
      perUnit: { system: "ucum", code: "{token}" },
    })
    expect(priceAction.payload.inputSchema.properties.token_quantity.nornType).toEqual({
      kind: "decimal",
      scale: 0,
      unit: { system: "ucum", code: "{token}" },
      additivity: { kind: "additive" },
    })
  })

  test("bi.CertifiedMetric and analytics.Study declare LIFE-SOUND-1..8 lifecycles and preregistration laws", () => {
    const metric = byId.get("bi.CertifiedMetric.entity")!.payload.objectType
    expect(metric.lifecycles[0]).toMatchObject({
      property: "status",
      initial: ["draft"],
      terminal: ["deprecated"],
      immutable: ["deprecated"],
      endings: [
        { state: "certified", outcome: "resting" },
        { state: "deprecated", outcome: "cancelled" },
      ],
    })

    const study = byId.get("analytics.Study.entity")!.payload.objectType
    expect(study.lifecycles[0]).toMatchObject({
      property: "publication_status",
      initial: ["preregistered"],
      terminal: ["published", "withheld"],
      immutable: ["published", "withheld"],
      endings: [
        { state: "published", outcome: "completed" },
        { state: "withheld", outcome: "cancelled" },
      ],
    })

    expect(byId.get("analytics.Study.temporal_split_ordered.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "timestamp(self.train_cutoff_at) <= timestamp(self.holdout_start_at)",
    })
    expect(byId.get("analytics.Study.publish_requires_diagnostics.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "(self.publication_status != 'published') or (self.diagnostics_passed == true)",
    })
    expect(byId.get("bi.MetricSlice.denominator_non_zero.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "self.denominator != 0",
    })
    expect(byId.get("bi.MetricSlice.non_additive_ratio_consistent.law")!.payload).toMatchObject({
      condition: "self.additivity == 'non_additive'",
      response: {
        kind: "predicate",
        expression:
          "((self.denominator > 0) and (((self.numerator - (self.value * self.denominator)) * 2000000) <= self.denominator)) and ((((self.value * self.denominator) - self.numerator) * 2000000) <= self.denominator)",
      },
    })
    expect(byId.get("compute.UsageEvent.token_usage_billed_exact.law")!.payload).toMatchObject({
      condition: "self.unit == '{token}'",
      response: {
        kind: "predicate",
        expression: "self.billed_amount == (self.rate_per_token * self.token_quantity)",
      },
    })
  })

  test.skipIf(!checker)("Rust checker admits both packs over core base and verifies all LAW-EXAMPLE-1 cases", () => {
    const checked = checker!.checkArtifactSet(allArtifacts, { base: loadCoreBase() })
    if (!checked.admitted) throw new Error(JSON.stringify(checked.diagnostics, null, 2))
    const laws = new Set(allArtifacts.filter((a) => a.kind === "law").map((a) => a.artifactId as string))
    expect(checked.diagnostics.diagnostics.filter((d) => laws.has(d.artifactId ?? ""))).toEqual([])
    expect(allExamples.length).toBeGreaterThanOrEqual(20)
    expect(runExamples(checker!, allArtifacts, allExamples)).toEqual([])
  }, 30_000)
})
