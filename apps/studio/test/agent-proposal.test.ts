import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { ProposalError, applyProposal, stageOf, verdictOf, worldIndex, type CheckResult, type Envelope, type Finding } from "../worker/proposal.ts"

const root = join(import.meta.dir, "../../..")
const world = JSON.parse(readFileSync(join(root, "fixtures/stress/purchase.json"), "utf8")) as Envelope[]
const LAW = "stress.purchase_positive.law"

const finding = (code: string, artifactId: string | null): Finding =>
  ({ code, tier: null, severity: "error", message: "m", span: { pointer: "/payload", source: null }, artifactId, remediation: null })

test("a proposal changes one artifact of a copy of the world", () => {
  const next = applyProposal(world, { artifact: LAW, pointer: "/payload/response/expression", value: "self.ordered >= 0" })
  expect((next.find(({ artifactId }) => artifactId === LAW)?.payload.response as { expression: string }).expression).toBe("self.ordered >= 0")
  expect((world.find(({ artifactId }) => artifactId === LAW)?.payload.response as { expression: string }).expression).toBe("self.ordered > 0")
})

test("a proposal at a place that does not exist is refused before the checker", () => {
  expect(() => applyProposal(world, { artifact: "stress.Absent", pointer: "/payload", value: 1 })).toThrow(ProposalError)
  expect(() => applyProposal(world, { artifact: LAW, pointer: "/payload/absent/expression", value: 1 })).toThrow(ProposalError)
  expect(() => applyProposal(world, { artifact: LAW, pointer: "payload", value: 1 })).toThrow(ProposalError)
})

test("a refusal names the law it came from in the law's own words", () => {
  const result: CheckResult = { admitted: false, diagnostics: { diagnostics: [finding("law.model.predicate_type", LAW)] }, receiptSha256: null }
  const verdict = verdictOf(world, { artifact: LAW, pointer: "/x", value: 1 }, result)
  expect(verdict.admitted).toBe(false)
  expect(verdict.stage).toBe("model")
  expect(verdict.refusals[0]?.law).toEqual({ artifactId: LAW, name: "Order quantity must be positive", rationale: "A purchase promises a positive quantity.", expression: "self.ordered > 0" })
})

test("the stage is read from the codes the checker returned", () => {
  expect(stageOf([finding("artifact_set.resolution.dependency_cycle", null)], false)).toBe("resolution")
  expect(stageOf([finding("envelope.shape.unknown_field", null)], false)).toBe("shape")
  expect(stageOf([], true)).toBe("admitted")
})

test("the world index lists laws with their statements", () => {
  const law = worldIndex(world).find(({ artifactId }) => artifactId === LAW)
  expect(law).toMatchObject({ kind: "law", expression: "self.ordered > 0" })
})
