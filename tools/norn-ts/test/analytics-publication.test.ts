import { expect, test } from "bun:test"
import { join, resolve } from "node:path"
import { Schema } from "effect"
import { FORECAST_MANIFEST_DIGEST } from "../../../registry/core/src/shared/metrics-pin.ts"
import { compilePack, checkPack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const ROOT = resolve(TOOL_ROOT, "../..")
const Envelope = Schema.Struct({ artifactId: Schema.String, payload: Schema.Unknown })

function publication() {
  const result = compilePack(join(ROOT, "registry/core/src"), {
    scope: {}, registry: "registry/core", root: ROOT,
    modules: [join(ROOT, "registry/core/src/analytics.ts")],
  })
  if (!result.ok) throw new Error(JSON.stringify(result.failures))
  const artifacts = result.artifacts.filter(artifact => Schema.decodeUnknownSync(Envelope)(artifact).artifactId.startsWith("core.analytics."))
  expect(artifacts).toHaveLength(6)
  return artifacts
}

test("ANALYSIS-PUBLISH-1 declares publication states, law and reference-only governed actions", () => {
  const artifacts = publication().map(artifact => Schema.decodeUnknownSync(Envelope)(artifact))
  expect(artifacts.find(artifact => artifact.artifactId === "core.analytics.Publication.entity")?.payload).toMatchObject({
    objectType: { lifecycles: [{ property: "status", transitions: [
      { id: "publish", guardRefusalCode: "core.projection.publication_guard", guards: expect.arrayContaining([expect.stringContaining("temporal_leakage")]) },
      { id: "refuse" },
    ] }] },
  })
  expect(artifacts.find(artifact => artifact.artifactId === "core.analytics.publish.action")?.payload).toMatchObject({
    inputSchema: { additionalProperties: false, properties: { id: { type: "string" }, receipt: { type: "string" } }, required: ["id", "receipt"] },
    effectProgram: { kind: "fire_transition", transition: "publish", acceptReceipt: {
      referenceField: "receipt", manifestDigest: FORECAST_MANIFEST_DIGEST, digestProperty: "receipt_digest",
      resultBindings: { fresh: "fresh", temporal_leakage: "temporal_leakage", holdout_error_nanos: "rmse_nanos" },
    } },
  })
  expect(artifacts.find(artifact => artifact.artifactId === "core.analytics.publication_policy.law")?.payload).toMatchObject({
    modality: { kind: "always" }, response: { kind: "invariant", invariantId: "publication_policy" },
  })
  expect(artifacts.find(artifact => artifact.artifactId === "core.analytics.publication.access.policy")?.payload).toMatchObject({
    rules: [{ allOf: [{ role: { anyOf: ["AnalyticsReviewer"] } }] }],
  })
})

test.skipIf(!checker)("Rust admits the declared publication policy with the configured Function binding", () => {
  const report = checkPack(checker!, publication(), [])
  expect(report.diagnostics.diagnostics).toEqual([])
  expect(report.admitted).toBe(true)
})
