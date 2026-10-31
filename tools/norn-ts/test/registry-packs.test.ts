import { expect, test } from "bun:test"
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

/** `registry/packs/<pack>/*.ts` compile to `registry/packs/<pack>/artifacts/<artifactId>.json`; NORN_PACKS_UPDATE=1 regenerates them. */
const REPO = join(TOOL_ROOT, "../..")
const PACKS = join(REPO, "registry/packs")
const update = process.env.NORN_PACKS_UPDATE === "1"

function compiled(dir: string): any[] {
  const files = readdirSync(dir).filter(f => f.endsWith(".ts")).sort().map(f => join(dir, f))
  const registryPackagePath = relative(REPO, dir).split("\\").join("/")
  return compileDefinitionFiles(files, path => ({ scope: {}, sourcePath: relative(REPO, path), registryPackagePath })).flatMap(result => {
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    return result.artifacts as any[]
  })
}

const packs = readdirSync(PACKS, { withFileTypes: true }).filter(d => d.isDirectory()).map(d => join(PACKS, d.name)).sort()

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
  const artifacts = packs.flatMap(compiled)
  const checked = checker!.checkArtifactSet(artifacts)
  if (!checked.admitted) throw new Error(JSON.stringify(checked.diagnostics))
}, 60_000)
