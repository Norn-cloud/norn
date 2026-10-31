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

const PRIMITIVES = ["core.Agent", "core.AgentRole", "core.Claim", "core.Commitment", "core.EconomicEvent", "core.Hold", "core.LedgerEffect", "core.Resource"]

test("the core physics pack declares exactly its primitives", () => {
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

// Commitment ⊓ Claim = ∅. Until the checker enforces disjoint primitives, this
// holds the published registry to it: no type implements both, directly or
// through interface parents.
type Payload = { payload?: { objectType?: { name: string; extends?: string; implements?: string[] }; interface?: { name: string; extends?: string[] } } }

function bothCommitmentAndClaim(artifacts: ReadonlyArray<Payload>): string[] {
  const parents = new Map<string, string[]>()
  for (const { payload } of artifacts) {
    if (payload?.interface) parents.set(payload.interface.name, payload.interface.extends ?? [])
    if (payload?.objectType) {
      const { name, extends: supertype, implements: implemented = [] } = payload.objectType
      parents.set(name, supertype === undefined ? [...implemented] : [supertype, ...implemented])
    }
  }
  const ancestors = (name: string, seen: Set<string>): Set<string> => {
    for (const parent of parents.get(name) ?? []) if (!seen.has(parent)) ancestors(parent, seen.add(parent))
    return seen
  }
  return [...parents.keys()].filter((name) => {
    const kinds = ancestors(name, new Set([name]))
    return kinds.has("core.Commitment") && kinds.has("core.Claim")
  })
}

test("no registry type is both a core.Commitment and a core.Claim", () => {
  const compiled = pack()
  const all = [...compiled.artifacts, ...compiled.baseArtifacts] as Payload[]
  expect(bothCommitmentAndClaim(all)).toEqual([])
  // The check bites: a type implementing both, even through a parent, is found.
  const bad: Payload[] = [
    { payload: { interface: { name: "x.Promise", extends: ["core.Commitment"] } } },
    { payload: { objectType: { name: "x.Both", implements: ["x.Promise", "core.Claim"] } } },
    { payload: { objectType: { name: "x.Order", implements: ["core.Commitment"] } } },
    { payload: { objectType: { name: "x.InvoicedOrder", extends: "x.Order", implements: ["core.Claim"] } } },
  ]
  expect(bothCommitmentAndClaim([...all, ...bad])).toEqual(["x.Both", "x.InvoicedOrder"])
})
