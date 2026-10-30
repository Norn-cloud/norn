import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { compileModule } from "../src/compile-stdin.ts"
import * as OP from "../src/builder/ontology-pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const file = join(TOOL_ROOT, "fixtures/definitions/ontology-pack.source.ts")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "ontology-pack.source.ts" }
const scope = options.scope
function pack(): any {
  const result = compileDefinitions(file, options)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.artifacts).toHaveLength(1)
  return result.artifacts[0]
}

test("an ontology pack lowers to a business_pack artifact with its source manifest", () => {
  const artifact = pack()
  expect(artifact.kind).toBe("business_pack")
  expect(artifact.artifactId).toBe("norn.ontology-pack.fixture-commerce")
  expect(artifact.payload.packKind).toBe("ontology")
  expect(artifact.payload.packVersion).toBe("0.1")
  expect(artifact.payload.sources[0].importMode).toBe("map_only")
})

test("builder and compiler produce the same artifact", () => {
  const artifact = pack()
  const { packId, packVersion, packKind, ...data } = artifact.payload
  const twin = OP.lowerOntologyPack(OP.ontologyPack({ id: packId, title: artifact.name, version: artifact.version, ...data }), options)
  expect(twin.artifact).toEqual(artifact)
})

test("the compiler refuses members outside the closed source manifest", () => {
  const source = readFileSync(file, "utf8").replace('importMode: "map_only",', 'importMode: "map_only", checksum: "x",')
  const result = compileModule(source, scope)
  expect(result.ok).toBe(false)
  if (!result.ok) expect(JSON.stringify(result.diagnostics)).toContain("unknown member checksum")
}, 30_000)

test.skipIf(!checker)("Rust admits the pack and refuses a mapping that cites a study_only source", () => {
  expect(checker!.checkArtifactSet([pack()]).admitted).toBe(true)
  const studied = pack()
  studied.payload.sources[0].importMode = "study_only"
  const refused = checker!.checkArtifactSet([studied])
  expect(refused.admitted).toBe(false)
  expect(JSON.stringify(refused)).toContain("study_only")
})
