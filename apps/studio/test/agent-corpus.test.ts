import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { applyProposal, verdictOf, type CheckResult, type Envelope } from "../worker/proposal.ts"

// The stress corpus through the real checker, the way the agent Worker runs it.
// Needs the built checker package (crates/norn-checker-wasm/scripts/build-pkg.sh);
// CI's integration phase builds it, and Boat runs set NORN_CHECKER_WASM_PKG.
const pkg = process.env.NORN_CHECKER_WASM_PKG
const root = join(import.meta.dir, "../../..")
const read = (path: string) => readFileSync(join(root, path), "utf8")

test.skipIf(pkg === undefined)("every invalid proposal in the stress corpus is refused with its readable reason", async () => {
  const checker = await import(join(pkg ?? "", "norn_checker_wasm.js")) as { initSync: (m: { module: Uint8Array }) => void; checkArtifactSet: (a: string, c: string) => string }
  checker.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
  const corpus = JSON.parse(read("fixtures/stress/invalid-proposals.json")) as { source: string; cases: { name: string; artifact: string; pointer: string; value: unknown; code: string; text: string }[] }
  const world = JSON.parse(read(corpus.source)) as Envelope[]
  const context = JSON.stringify({ base: ["core.Process.interface.json", "core.units.json"].map((file) => JSON.parse(read(`registry/core/artifacts/${file}`))), registry: [] })
  const check = (artifacts: readonly Envelope[]) => JSON.parse(checker.checkArtifactSet(JSON.stringify(artifacts), context)) as CheckResult
  expect(check(world).admitted).toBe(true)
  for (const { name, code, text, ...proposal } of corpus.cases) {
    const verdict = verdictOf(world, proposal, check(applyProposal(world, proposal)))
    expect({ name, admitted: verdict.admitted }).toEqual({ name, admitted: false })
    const refusal = verdict.refusals.find((found) => found.code === code && found.message.includes(text))
    expect({ name, code, found: refusal !== undefined }).toEqual({ name, code, found: true })
    if (proposal.artifact.endsWith(".law") && refusal?.artifactId === proposal.artifact) expect(refusal.law?.artifactId).toBe(proposal.artifact)
  }
})
