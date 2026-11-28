import { beforeAll, describe, expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/native-compiler.ts"
import { runExamples, type CompiledExample } from "../src/examples.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const REPO = join(TOOL_ROOT, "../..")
const DATA_DIR = join(REPO, "registry/packs/data")
const DATA_FILE = join(DATA_DIR, "data.ts")
const ARTIFACTS_DIR = join(DATA_DIR, "artifacts")

function compileDataPack() {
  const results = compileDefinitionFiles(
    [DATA_FILE],
    (path) => ({
      scope: {},
      sourcePath: relative(REPO, path),
      registryPackagePath: relative(REPO, DATA_DIR).split("\\").join("/"),
    }),
    {},
    { forbidUnsafeZen: true },
  )
  const artifacts: any[] = []
  const examples = []
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
  return [
    "core.Agent.interface.json",
    "core.TimeInterval.interface.json",
    "core.value_types.entity.json",
    "core.time.json",
  ].map((name) => JSON.parse(readFileSync(join(core, name), "utf8")))
}

describe("data constitutional ontology pack", () => {
  let artifacts: any[]
  let examples: CompiledExample[]
  let byId: Map<string, any>

  beforeAll(() => {
    const compiled = compileDataPack()
    artifacts = compiled.artifacts
    examples = compiled.examples
    byId = new Map(artifacts.map((a) => [a.artifactId as string, a]))
  }, 30_000)

  test("committed registry/packs/data/artifacts/*.json match compiler output byte-for-byte", () => {
    const committed = readdirSync(ARTIFACTS_DIR).sort()
    const expected = artifacts.map((a) => `${a.artifactId}.json`).sort()
    expect(committed).toEqual(expected)
    for (const artifact of artifacts) {
      const raw = readFileSync(join(ARTIFACTS_DIR, `${artifact.artifactId}.json`), "utf8")
      expect(raw).toBe(`${JSON.stringify(artifact, null, 2)}\n`)
    }
  })

  test("declares all 6 W3C/ISO constitutional interfaces with BFO roles and invariants", () => {
    const dataset = byId.get("data.Dataset.interface")!.payload.interface
    expect(dataset.role).toBe("thing")
    expect(dataset.invariants.map((i: any) => i.id)).toEqual(["positive_freshness_sla", "non_empty_schema_uri"])
    expect(dataset.links).toEqual([
      {
        id: "owner",
        description: expect.any(String),
        target: "core.Agent",
        cardinality: { to: "one", from: "many" },
      },
    ])

    const snapshot = byId.get("data.DatasetSnapshot.interface")!.payload.interface
    expect(snapshot.role).toBe("thing")
    expect(snapshot.invariants.map((i: any) => i.id)).toEqual(["non_negative_counts", "non_empty_digests"])
    expect(snapshot.links.map((l: any) => [l.id, l.target, Boolean(l.cardinality.required)])).toEqual([
      ["dataset", "data.Dataset", true],
      ["supersedes", "data.DatasetSnapshot", false],
      ["generated_by", "data.Activity", false],
    ])

    const activity = byId.get("data.Activity.interface")!.payload.interface
    expect(activity.role).toBe("happening")
    expect(activity.extends).toEqual(["core.TimeInterval"])

    const derivation = byId.get("data.Derivation.interface")!.payload.interface
    expect(derivation.role).toBe("happening")
    expect(derivation.links.map((l: any) => [l.id, l.target, Boolean(l.cardinality.required)])).toEqual([
      ["input_snapshot", "data.DatasetSnapshot", true],
      ["output_snapshot", "data.DatasetSnapshot", true],
      ["activity", "data.Activity", true],
    ])

    const observation = byId.get("data.Observation.interface")!.payload.interface
    expect(observation.role).toBe("happening")
    expect(observation.invariants.map((i: any) => i.id)).toEqual([
      "non_empty_digest",
      "observed_at_or_after_occurred",
    ])

    const finding = byId.get("data.QualityFinding.interface")!.payload.interface
    expect(finding.role).toBe("happening")
    expect(finding.properties.find((p: any) => p.id === "dimension").type).toEqual({
      kind: "enum",
      values: [
        { value: "accuracy" },
        { value: "completeness" },
        { value: "consistency" },
        { value: "credibility" },
        { value: "currentness" },
      ],
    })
  })

  test("declares concrete reference object types exercising acyclic supersedes and LIFE-SOUND-1..8", () => {
    const snapRecord = byId.get("data.SnapshotRecord.entity")!.payload.objectType
    expect(snapRecord.implements).toEqual(["data.DatasetSnapshot"])
    expect(snapRecord.role).toBe("thing")
    const supersedes = snapRecord.links.find((l: any) => l.id === "supersedes")
    expect(supersedes).toMatchObject({
      id: "supersedes",
      target: "data.SnapshotRecord",
      acyclic: true,
    })

    const pipelineRun = byId.get("data.PipelineRun.entity")!.payload.objectType
    expect(pipelineRun.implements).toEqual(["data.Activity"])
    expect(pipelineRun.role).toBe("happening")

    const qualityCase = byId.get("data.QualityCase.entity")!.payload.objectType
    expect(qualityCase.implements).toEqual(["data.QualityFinding"])
    expect(qualityCase.role).toBe("happening")
    expect(qualityCase.lifecycles).toEqual([
      {
        property: "status",
        initial: ["open"],
        terminal: ["resolved"],
        immutable: ["resolved"],
        transitions: [
          { id: "acknowledge", from: ["open"], to: "acknowledged" },
          { id: "quarantine", from: ["open", "acknowledged"], to: "quarantined" },
          { id: "resolve", from: ["open", "acknowledged", "quarantined"], to: "resolved" },
        ],
        endings: [{ state: "resolved", outcome: "completed" }],
      },
    ])
  })

  test("lowers all DATA-CAUSAL-1 LAW-LINK-1 temporal causality laws on data.Derivation", () => {
    expect(byId.get("data.Derivation.input_known_before_activity_end.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "timestamp(self.input_snapshot.known_at) <= timestamp(self.activity.ends_at)",
    })
    expect(byId.get("data.Derivation.output_known_after_activity_start.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "timestamp(self.activity.starts_at) <= timestamp(self.output_snapshot.known_at)",
    })
    expect(byId.get("data.Derivation.monotonic_knowledge_time.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "timestamp(self.input_snapshot.known_at) <= timestamp(self.output_snapshot.known_at)",
    })
    expect(byId.get("data.Derivation.distinct_snapshots.law")!.payload.response).toEqual({
      kind: "predicate",
      expression: "self.input_snapshot.snapshot_id != self.output_snapshot.snapshot_id",
    })
  })

  test.skipIf(!checker)("Rust checker admits registry/packs/data cleanly and verifies all LAW-EXAMPLE-1 cases", () => {
    const checked = checker!.checkArtifactSet(artifacts, { base: loadCoreBase() })
    if (!checked.admitted) throw new Error(JSON.stringify(checked.diagnostics, null, 2))
    expect(checked.diagnostics.diagnostics).toEqual([])
    expect(examples.length).toBeGreaterThanOrEqual(10)
    expect(runExamples(checker!, artifacts, examples)).toEqual([])
  }, 30_000)
})
