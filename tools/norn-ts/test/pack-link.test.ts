import { expect, test } from "bun:test"
import { compileModule } from "../src/compile-stdin.ts"

const link = (reason: string) => `import { Norn } from "@norn/authoring"
export const Link = Norn.packLink({
  id: "norn.link.fixture-shop.fixture-canon", title: "Fixture link", version: "0.1.0",
  fromPack: { packId: "norn.ontology-pack.fixture-shop", version: "0.1.0" },
  toPack: { packId: "norn.ontology-pack.fixture-canon", version: "0.1.0" },
  correspondences: [{
    correspondenceId: "corr.order", fromTermId: "norn.shop.order", relation: "subsumed_by", toTermIds: ["norn.canon.commitment"],
    justification: "Fixture.", evidenceRefs: ["source.shop-api"], confidenceBasisPoints: 5000, reviewedBy: "fixture", reviewedAt: "2026-10-31T00:00:00Z",
  }],
  residuals: [{ termId: "norn.shop.order.reset", reason: "${reason}", note: "Fixture." }],
})
`

test("a link may flag non_conforming source behaviour as a residual", () => {
  const result = compileModule(link("non_conforming"), {})
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect((result.artifacts as any[])[0].payload.residuals[0].reason).toBe("non_conforming")
}, 30_000)

test("residual reasons outside the closed set are refused", () => {
  const result = compileModule(link("ignored"), {})
  expect(result.ok).toBe(false)
}, 30_000)
