import { expect, test } from "bun:test"
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"
import { runExamples, type CompiledExample } from "../src/examples.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

/**
 * `registry/packs/<pack>/*.ts` compile to `registry/packs/<pack>/artifacts/<artifactId>.json`, and the test fixture packs
 * under `fixtures/packs/<pack>` the same way; NORN_PACKS_UPDATE=1 regenerates them.
 */
const REPO = join(TOOL_ROOT, "../..")
const PACK_ROOTS = [join(REPO, "registry/packs"), join(REPO, "fixtures/packs")]
const update = process.env.NORN_PACKS_UPDATE === "1"

function compiledPack(dir: string): { readonly artifacts: any[]; readonly examples: CompiledExample[] } {
  const files = readdirSync(dir).filter(f => f.endsWith(".ts")).sort().map(f => join(dir, f))
  const registryPackagePath = relative(REPO, dir).split("\\").join("/")
  const artifacts: any[] = []
  const examples: CompiledExample[] = []
  for (const result of compileDefinitionFiles(files, path => ({ scope: {}, sourcePath: relative(REPO, path), registryPackagePath }))) {
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    artifacts.push(...(result.artifacts as any[]))
    examples.push(...result.examples)
  }
  return { artifacts, examples }
}

function compiled(dir: string): any[] {
  return compiledPack(dir).artifacts
}

const packs = PACK_ROOTS.flatMap(root => readdirSync(root, { withFileTypes: true }).filter(d => d.isDirectory()).map(d => join(root, d.name)).sort())

// Minimal base-layer envelopes for core targets referenced by `identity.BusinessUnit.entity.json`
// (`identity.Directory` interface and `party.Organization` object type) which do not yet ship as
// standalone files under `registry/core/artifacts/`.
const BUSINESS_UNIT_CLOSURE = [
  {
    irVersion: "0.1",
    artifactId: "identity.Directory.interface",
    kind: "entity",
    namespace: "identity",
    name: "Directory",
    version: "0.1.0",
    lifecycle: "active",
    scope: {},
    dependencies: [],
    provenance: { source: "system" },
    payload: {
      entityVersion: "2.0",
      interface: {
        name: "identity.Directory",
        properties: [{ id: "name", type: { kind: "ref", name: "core.display_name" } }],
      },
    },
  },
  {
    irVersion: "0.1",
    artifactId: "party.Organization.entity",
    kind: "entity",
    namespace: "party",
    name: "Organization",
    version: "0.1.0",
    lifecycle: "active",
    scope: {},
    dependencies: [],
    provenance: { source: "system" },
    payload: {
      entityVersion: "2.0",
      objectType: {
        name: "party.Organization",
        properties: [{ id: "display_name", type: { kind: "ref", name: "core.display_name" } }],
      },
    },
  },
]

test("registry packs are generated from their typed sources", () => {
  for (const dir of packs) {
    const artifacts = compiled(dir)
    const out = join(dir, "artifacts")
    if (update) {
      rmSync(out, { recursive: true, force: true })
      mkdirSync(out)
      for (const artifact of artifacts) writeFileSync(join(out, `${artifact.artifactId}.json`), `${JSON.stringify(artifact, null, 2)}\n`)
    }
    const committed = readdirSync(out).sort()
    expect(committed).toEqual(artifacts.map(a => `${a.artifactId}.json`).sort())
    for (const artifact of artifacts) expect(JSON.parse(readFileSync(join(out, `${artifact.artifactId}.json`), "utf8"))).toEqual(artifact)
  }
}, 60_000)

test.skipIf(!checker)("the Rust checker admits every registry pack and link", () => {
  const results = packs.map(compiledPack)
  const artifacts = results.flatMap(r => r.artifacts)
  const examples = results.flatMap(r => r.examples)
  const core = join(REPO, "registry/core/artifacts")
  const physics = ["Agent", "AgentRole", "Resource", "Commitment", "Hold", "EconomicEvent", "LedgerEffect", "Claim"]
  // The core interfaces packs implement (physics, and the scanned-documents fixture pack's norn.SourceEvidence and norn.SourceDocument), the
  // claim type every mapped link is recorded as (norn.ObservedLinkClaim, OBS-LINK-CONVERGE-1), `identity.BusinessUnit` (referenced by
  // `org.OrgUnit.business_unit`) with its base closure, plus the value types and units. Not every core interface: some (core.Order) name registry standard
  // types (iso4217) this checker call does not install.
  const base = [
    ...[...physics.map(name => `core.${name}.interface.json`), "core.TimeInterval.interface.json", "identity.BusinessUnit.entity.json", "norn.SourceEvidence.interface.json", "norn.SourceDocument.interface.json", "norn.ObservedLinkClaim.entity.json", "core.value_types.entity.json", "std.value_types.entity.json", "core.units.json", "core.time.json"]
      .map(name => JSON.parse(readFileSync(join(core, name), "utf8"))),
    ...BUSINESS_UNIT_CLOSURE,
  ]
  const checked = checker!.checkArtifactSet(artifacts, { base })
  if (!checked.admitted) throw new Error(JSON.stringify(checked.diagnostics))
  // LAW-LINK-1 regression: every registered pack law stays admitted, with no finding against it.
  const laws = new Set(artifacts.filter(a => a.kind === "law").map(a => a.artifactId as string))
  expect(laws.size).toBeGreaterThan(0)
  expect(checked.diagnostics.diagnostics.filter(d => laws.has(d.artifactId ?? ""))).toEqual([])
  // LAW-EXAMPLE-1 regression: every registered pack example agrees with the Rust checker's law semantics.
  if (examples.length > 0) expect(runExamples(checker!, artifacts, examples)).toEqual([])
}, 60_000)
