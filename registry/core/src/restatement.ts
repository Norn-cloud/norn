import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Slot = Schema.Struct({
  id: Schema.String,
  status: Schema.Literals(["building", "ready", "approved", "active"]),
  reason: Schema.String,
  slice_ids: Schema.String,
  active: Schema.String, active_digest: Schema.String,
  candidate: Schema.String, candidate_digest: Schema.String,
})

export const RestatementEvidence = Norn.interfaceType({
  name: "core.analytics.RestatementEvidence", title: "Complete analytical restatement evidence", version: "0.1.0",
  properties: Slot,
  invariants: [{ id: "complete_candidate", message: "Approval requires a complete staged analytical generation.",
    expression: Norn.expression(Slot, self => self.status === "building" || (self.reason !== "" && self.slice_ids !== "" && self.candidate !== "" && self.candidate_digest !== "")),
  }],
})

/** RESTATE-HISTORY-1 / OUTPUT-ATOMIC: the host stages one complete bundle;
 * declared actions approve and atomically select it. */
export const Restatement = Norn.type({
  name: "core.analytics.Restatement", title: "Analytical restatement", version: "0.1.0",
  properties: Slot, implements: [RestatementEvidence], identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "building", active: "", active_digest: "", candidate: "", candidate_digest: "" },
  lifecycles: [{ property: "status", initial: ["building"], terminal: [], immutable: ["ready", "approved"], transitions: [
    { id: "stage", from: ["building"], to: "ready", guards: [Norn.expression(Slot, self => self.candidate !== "" && self.candidate_digest !== "")] },
    { id: "rebuild", from: ["active"], to: "building" },
    { id: "approve", from: ["ready"], to: "approved",
      guards: [Norn.expression(Slot, self => self.reason !== "" && self.slice_ids !== "" && self.candidate !== "" && self.candidate_digest !== "")] },
    { id: "activate", from: ["approved"], to: "active",
      guards: [Norn.expression(Slot, self => self.candidate !== "" && self.candidate_digest !== "")] },
  ] }],
})

export const ApproveRestatement = Norn.action({
  id: "core.analytics.restatement.approve", title: "Approve analytical restatement", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Restatement, change: "modified" }],
  effect: { kind: "fire_transition", type: Restatement, objectIdField: "id", lifecycle: "status", transition: "approve" },
})

export const ActivateRestatement = Norn.action({
  id: "core.analytics.restatement.activate", title: "Activate complete analytical restatement", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Restatement, change: "modified" }],
  effect: { kind: "fire_transition", type: Restatement, objectIdField: "id", lifecycle: "status", transition: "activate" },
})

export const RestatementPolicy = Norn.law({
  id: "core.analytics.restatement_policy.law", title: "Approve complete restatement generations", version: "0.1.0",
  law: { lawVersion: "0.1", id: "core.analytics.restatement_policy", owner: "core.analytics",
    severity: "error", rationale: "RESTATE-HISTORY-1 / OUTPUT-ATOMIC: the host stages every authenticated slice before reviewer approval; activation selects the complete bundle without rewriting posted history.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/restatement.ts" },
    scope: { root: { kind: "entity", id: "core.analytics.Restatement" }, objectTypes: ["core.analytics.Restatement"] },
    subject: "core.analytics.Restatement", modality: { kind: "always" },
    response: { kind: "invariant", objectType: "core.analytics.Restatement", invariantId: "complete_candidate" },
  },
  dependencies: [{ artifactId: "core.analytics.Restatement.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const RestatementAccess = Norn.access({
  id: "core.analytics.restatement.access", title: "Analytical restatement reviewer", version: "0.1.0",
  rules: [{ ruleId: "reviewer", actions: [ApproveRestatement, ActivateRestatement], resourceType: Restatement,
    scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["AnalyticsReviewer"] } }],
  }],
})
