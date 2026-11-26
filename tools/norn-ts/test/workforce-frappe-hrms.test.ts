import { describe, expect, test } from "bun:test"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const REPO = join(TOOL_ROOT, "../..")
const WORKFORCE_PACK = join(REPO, "registry/packs/org/workforce-canon.pack.ts")
const FRAPPE_PACK = join(REPO, "registry/packs/frappe-hrms/frappe-hrms.pack.ts")
const FRAPPE_LINK = join(REPO, "registry/packs/frappe-hrms/frappe-hrms.link.ts")

describe("workforce canon v0.2 + Frappe HRMS v16.60.0 map-only parity pack (#1592)", () => {
  const results = compileDefinitionFiles(
    [WORKFORCE_PACK, FRAPPE_PACK, FRAPPE_LINK],
    (path) => ({
      scope: {},
      sourcePath: relative(REPO, path),
      registryPackagePath: relative(REPO, join(path, "..")).split("\\").join("/"),
    }),
    {},
    { forbidUnsafeZen: true },
  )

  const artifacts: any[] = []
  for (const r of results) {
    if (!r.ok) throw new Error(JSON.stringify(r.diagnostics, null, 2))
    expect(r.warnings).toEqual([])
    artifacts.push(...(r.artifacts as any[]))
  }

  test.skipIf(!checker)("all three ontology artifacts pass norn-checker-wasm with zero diagnostics", () => {
    const result = checker!.checkArtifactSet(artifacts, { base: [], registry: [] })
    expect(result.admitted).toBe(true)
    expect(result.diagnostics.diagnostics).toEqual([])
  })

  test("workforce canon v0.2 pins W3C ORG, OWL-Time, W3C DPV 2.1, and HR Open 4.5R and maps all 6 HR PII markings", () => {
    const wf = artifacts.find((a) => a.artifactId === "norn.ontology-pack.workforce")!.payload
    const sourceIds = wf.sources.map((s: any) => s.sourceId)
    expect(sourceIds).toEqual([
      "source.w3c-org",
      "source.w3c-time",
      "source.w3c-dpv",
      "source.hr-open-standards",
    ])
    const dpvMappings = wf.mappings.filter((m: any) => m.sourceId === "source.w3c-dpv")
    expect(dpvMappings).toHaveLength(6)
    const hrOpenMappings = wf.mappings.filter((m: any) => m.sourceId === "source.hr-open-standards")
    expect(hrOpenMappings).toHaveLength(4)
  })

  test("Frappe HRMS pack pins 35 upstream files and maps 34/34 DocTypes to workforce canon v0.2 with 0 residuals", () => {
    const frappe = artifacts.find((a) => a.artifactId === "norn.ontology-pack.frappe-hrms")!.payload
    const link = artifacts.find((a) => a.artifactId === "norn.link.frappe-hrms.workforce")!.payload
    expect(frappe.sources[0].files).toHaveLength(35)
    expect(frappe.terms).toHaveLength(34)
    expect(link.correspondences).toHaveLength(34)
    expect(link.residuals ?? []).toEqual([])

    const fromSet = new Set(frappe.terms.map((t: any) => t.termId))
    const mappedSet = new Set(link.correspondences.map((c: any) => c.fromTermId))
    expect(mappedSet).toEqual(fromSet)
  })

  test("checked-in JSON artifacts match compiled output", () => {
    const wf = artifacts.find((a) => a.artifactId === "norn.ontology-pack.workforce")!
    const frappe = artifacts.find((a) => a.artifactId === "norn.ontology-pack.frappe-hrms")!
    const link = artifacts.find((a) => a.artifactId === "norn.link.frappe-hrms.workforce")!
    const wfPath = join(REPO, "registry/packs/org/artifacts/norn.ontology-pack.workforce.json")
    const frappeDir = join(REPO, "registry/packs/frappe-hrms/artifacts")
    mkdirSync(frappeDir, { recursive: true })
    if (process.env.NORN_UPDATE_ARTIFACTS === "1") {
      writeFileSync(wfPath, `${JSON.stringify(wf, null, 2)}\n`)
      writeFileSync(join(frappeDir, "norn.ontology-pack.frappe-hrms.json"), `${JSON.stringify(frappe, null, 2)}\n`)
      writeFileSync(join(frappeDir, "norn.link.frappe-hrms.workforce.json"), `${JSON.stringify(link, null, 2)}\n`)
    }
    expect(JSON.parse(readFileSync(wfPath, "utf8"))).toEqual(wf)
    expect(JSON.parse(readFileSync(join(frappeDir, "norn.ontology-pack.frappe-hrms.json"), "utf8"))).toEqual(frappe)
    expect(JSON.parse(readFileSync(join(frappeDir, "norn.link.frappe-hrms.workforce.json"), "utf8"))).toEqual(link)
  })
})
