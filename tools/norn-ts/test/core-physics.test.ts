import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { checkPack, compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

// Regenerate the artifacts after changing registry/core/src: write each
// compiled artifact as `JSON.stringify(artifact, null, 2) + "\n"` to
// registry/core/artifacts/<artifactId>.json.
const REPO = resolve(TOOL_ROOT, "../..")

function pack() {
  const result = compilePack(join(REPO, "registry/core/src"), { scope: {}, registry: "registry/core", root: REPO })
  if (!result.ok) throw new Error(JSON.stringify(result.failures))
  return result
}

const PRIMITIVES = ["core.Agent", "core.AgentRole", "core.Claim", "core.Commitment", "core.EconomicEvent", "core.Resource"]

test("the core physics pack declares exactly the six primitives", () => {
  const ids = pack().artifacts.map((artifact) => (artifact as { artifactId: string }).artifactId).sort()
  expect(ids).toEqual(PRIMITIVES.map((name) => `${name}.interface`))
})

test("checked-in core physics artifacts are exactly the compiler's output", () => {
  for (const artifact of pack().artifacts) {
    const id = (artifact as { artifactId: string }).artifactId
    expect(readFileSync(join(REPO, "registry/core/artifacts", `${id}.json`), "utf8")).toBe(`${JSON.stringify(artifact, null, 2)}\n`)
  }
})

// The primitives depend only on the shared value types. The rest of the public
// registry is not a closed set yet (core.Order names core.Party), so it is not
// part of this admission.
const VALUE_TYPES = new Set(["core.value_types.entity", "std.value_types.entity"])

test.skipIf(!checker)("Rust admits the core physics interfaces over the shared value types", () => {
  const compiled = pack()
  const base = compiled.baseArtifacts.filter((artifact) => VALUE_TYPES.has((artifact as { artifactId: string }).artifactId))
  expect(base.length).toBe(VALUE_TYPES.size)
  const report = checkPack(checker!, JSON.parse(JSON.stringify(compiled.artifacts)), JSON.parse(JSON.stringify(base)))
  expect(report.diagnostics.diagnostics).toEqual([])
  expect(report.admitted).toBe(true)
})
