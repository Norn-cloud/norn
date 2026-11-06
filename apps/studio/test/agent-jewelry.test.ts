import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { applyProposal, verdictOf, type CheckResult, type Envelope } from "../worker/proposal.ts"
import { WORLD } from "../worker/world.ts"

// The assistant's suggestion chips through the real checker, the way the agent Worker runs them.
// Needs the built checker package; Boat runs set NORN_CHECKER_WASM_PKG.
const pkg = process.env.NORN_CHECKER_WASM_PKG
const root = join(import.meta.dir, "../../..")
const read = (path: string) => readFileSync(join(root, path), "utf8")
const corpus = JSON.parse(read("fixtures/demo/jewelry-proposals.json")) as {
  source: string
  cases: { chip: string; artifact: string; pointer: string; value: unknown; admitted: boolean; code?: string; text?: string; law?: string }[]
}

test("the assistant proposes against the jewelry world, scoped to the demo cell", () => {
  expect(WORLD.map(({ artifactId }) => artifactId)).toEqual(["demo.Melt.entity", "demo.fine_mass_conservation.law"])
  expect(WORLD.every(({ scope }) => JSON.stringify(scope) === JSON.stringify({ environmentId: "dev", orgId: "org_demo" }))).toBe(true)
  expect(corpus.source).toBe("crates/norn-checker/tests/fixtures/jewelry_demo.json")
})

test("the chips include at least one admitted change and one refusal that names its law", () => {
  expect(corpus.cases.some(({ admitted }) => admitted)).toBe(true)
  expect(corpus.cases.some(({ admitted, law }) => !admitted && law !== undefined)).toBe(true)
})

test.skipIf(pkg === undefined)("each chip's change gets the decision the corpus records from the real checker", async () => {
  const checker = await import(join(pkg ?? "", "norn_checker_wasm.js")) as { initSync: (m: { module: Uint8Array }) => void; checkArtifactSet: (a: string, c: string) => string }
  checker.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
  const context = JSON.stringify({ base: ["core.Process.interface.json", "core.units.json"].map((file) => JSON.parse(read(`registry/core/artifacts/${file}`))), registry: [] })
  const check = (artifacts: readonly Envelope[]) => JSON.parse(checker.checkArtifactSet(JSON.stringify(artifacts), context)) as CheckResult
  expect(check(WORLD).admitted).toBe(true)
  for (const { chip, admitted, code, text, law, ...proposal } of corpus.cases) {
    const verdict = verdictOf(WORLD, proposal, check(applyProposal(WORLD, proposal)))
    expect({ chip, admitted: verdict.admitted }).toEqual({ chip, admitted })
    if (admitted) continue
    const refusal = verdict.refusals.find((found) => found.code === code && found.message.includes(text ?? ""))
    expect({ chip, found: refusal !== undefined }).toEqual({ chip, found: true })
    expect({ chip, law: verdict.refusals.some((found) => found.law?.artifactId === law) }).toEqual({ chip, law: true })
  }
})
