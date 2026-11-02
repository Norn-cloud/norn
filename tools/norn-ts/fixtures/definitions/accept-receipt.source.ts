import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Analysis = Norn.type({
  name: "test.Analysis", title: "Analysis", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, digest: Schema.String, score: Schema.Int, status: Schema.Literals(["proposed", "published"]) }),
  lifecycles: [{ property: "status", initial: ["proposed"], terminal: ["published"], transitions: [{ id: "publish", from: ["proposed"], to: "published" }] }],
})
export const Publish = Norn.action({
  id: "test.analysis.publish", title: "Publish", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String, receipt: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Analysis, change: "modified" }],
  effect: { kind: "fire_transition", type: Analysis, objectIdField: "id", lifecycle: "status", transition: "publish",
    acceptReceipt: { referenceField: "receipt", manifestDigest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", digestProperty: "digest", resultBindings: { score: "score" } } },
})
