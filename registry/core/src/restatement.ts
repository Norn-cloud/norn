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

/** RESTATE-HISTORY-1 / OUTPUT-ATOMIC: the host stages one complete bundle;
 * declared actions approve and atomically select it. */
export const Restatement = Norn.type({
  name: "core.analytics.Restatement", title: "Analytical restatement", version: "0.1.0",
  properties: Slot, identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "building", active: "", active_digest: "", candidate: "", candidate_digest: "" },
  lifecycles: [{ property: "status", initial: ["building"], terminal: ["active"], transitions: [
    { id: "approve", from: ["ready"], to: "approved", guardRefusalCode: "core.world.invalid_restatement",
      guards: [Norn.expression(Slot, self => self.reason !== "" && self.slice_ids !== "" && self.candidate !== "" && self.candidate_digest !== "")] },
    { id: "activate", from: ["approved"], to: "active", guardRefusalCode: "core.world.invalid_restatement",
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
