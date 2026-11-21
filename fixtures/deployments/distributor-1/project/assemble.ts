// Assemble the distributor-1 Norn project: compile src/ (the ontology and the
// generated mappings), pin each mapping to its source schema's envelope hash
// (the Rust checker computes it; SOURCE_HASH_MISMATCH names it), admit the set
// against the three-systems base, and write definitions/d1/*.json.
//
//   NORN_CHECKER_WASM_PKG=... bun assemble.ts PROJECT_DIR REPORT_JSON
//
// Modules that fail to compile (approvals.ts on main) are reported, not fatal.
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { join, relative, resolve } from "node:path"
import { compileDefinitionFiles } from "../../../../tools/norn-ts/src/compiler/definitions.ts"
import { loadChecker } from "../../../../tools/norn-ts/src/checker-abi.ts"

const here = import.meta.dir
const repo = resolve(here, "../../../..")
const [projectDir, reportPath] = process.argv.slice(2)
const modules = [...readdirSync(join(here, "src")).filter(f => f.endsWith(".ts")).map(f => join(here, "src", f)),
  ...readdirSync(join(here, "src/gen")).filter(f => f.endsWith(".ts")).map(f => join(here, "src/gen", f))].sort()

const report: Record<string, unknown> = { modules: {}, pins: {}, admission: null }
let artifacts: any[] = []
for (const file of modules) {
  const [result] = compileDefinitionFiles([file], path => ({ scope: {}, sourcePath: relative(repo, path), registryPackagePath: "fixtures/deployments/distributor-1/project" }))
  ;(report.modules as any)[relative(here, file)] = result.ok ? { ok: true, artifacts: result.artifacts.length } : { ok: false, diagnostics: (result as any).diagnostics.slice(0, 5) }
  if (result.ok) artifacts.push(...result.artifacts)
}
const core = JSON.parse(readFileSync(join(repo, "tools/norn-ts/templates/three-systems/template.json"), "utf8")) as { core: string[] }
const base = [...core.core.map(id => JSON.parse(readFileSync(join(repo, "registry/core/artifacts", `${id}.json`), "utf8"))),
  ...["canon", "locale-ar"].flatMap(pack => readdirSync(join(repo, "registry/packs", pack, "artifacts")).filter(f => f.endsWith(".json")).map(f => JSON.parse(readFileSync(join(repo, "registry/packs", pack, "artifacts", f), "utf8"))))]
const checker = await loadChecker()
if (!checker) throw new Error("NORN_CHECKER_WASM_PKG is required")
let checked: any
for (let round = 0; round < 3; round++) {
  checked = checker.checkArtifactSet(artifacts, { base })
  const mismatches = (checked.diagnostics.diagnostics ?? []).filter((d: any) => String(d.code).includes("source_hash") || String(d.message).includes("hashes to"))
  if (!mismatches.length) break
  for (const d of mismatches) {
    const m = /hashes to "([0-9a-f]{64})"/.exec(d.message)
    const art = artifacts.find(a => a.artifactId === d.artifactId)
    if (m && art) { art.payload.source.sha256 = m[1]; (report.pins as any)[art.artifactId] = m[1] }
  }
}
report.admission = { admitted: checked.admitted, diagnostics: (checked.diagnostics.diagnostics ?? []).map((d: any) => ({ code: d.code, severity: d.severity, artifactId: d.artifactId, message: String(d.message).slice(0, 400) })) }
report.artifacts = artifacts.map(a => ({ id: a.artifactId, kind: a.kind }))
// Admit what can be admitted: drop artifacts with error diagnostics (repeatedly, since a drop can
// orphan a dependent), so one bad mapping does not sink the deployment. Every drop is a reported gap.
const bad = new Set<string>()
for (let round = 0; round < 8 && !checked.admitted; round++) {
  const errs = (checked.diagnostics.diagnostics ?? []).filter((d: any) => d.severity === "error")
  const ids = errs.map((d: any) => d.artifactId).filter(Boolean)
  if (!ids.length) break
  for (const id of ids) bad.add(id)
  checked = checker.checkArtifactSet(artifacts.filter(a => !bad.has(a.artifactId)), { base })
}
report.dropped = [...bad]
report.finalAdmission = { admitted: checked.admitted, errors: (checked.diagnostics.diagnostics ?? []).filter((d: any) => d.severity === "error").slice(0, 20) }
const out = join(projectDir, "definitions/d1")
rmSync(out, { recursive: true, force: true })
mkdirSync(out, { recursive: true })
for (const a of artifacts.filter(a => !bad.has(a.artifactId))) writeFileSync(join(out, `${a.artifactId}.json`), `${JSON.stringify(a, null, 2)}\n`)
writeFileSync(reportPath, JSON.stringify(report, null, 1))
console.log(JSON.stringify({ admitted: checked.admitted, artifacts: artifacts.length, dropped: bad.size, pins: Object.keys(report.pins as any).length }))
