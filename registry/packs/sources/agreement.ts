import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// SOURCE-AGREE-1 (07-world-boundary): two sources may report different values of
// a measure's unit, currency or basis for one record. The cell derives that
// finding from verified history and never excludes the record for it. A data
// steward may accept a disagreement with a reason, which marks the finding
// acknowledged, and reopen it with a reason. The review is keyed by the
// finding's id, so a later disagreement (another finding) is not covered by an
// earlier acceptance. The reviewer is the verified actor, never caller input.

const Review = Schema.Struct({
  id: Schema.String,
  status: Schema.Literals(["accepted", "reopened"]),
  reason: Schema.String,
  reviewed_by: Schema.String,
})

export const AgreementReview = Norn.type({
  name: "source.AgreementReview", title: "Source disagreement review", version: "0.1.0",
  properties: Review,
  identity: [{ name: "primary", properties: ["id"] }],
})

const AcceptInput = Schema.Struct({
  id: Schema.String, status: Schema.Literals(["accepted"]), reason: Schema.String,
  reviewed_by: Schema.optionalKey(Schema.String),
})

const ReopenInput = Schema.Struct({
  id: Schema.String, status: Schema.Literals(["reopened"]), reason: Schema.String,
  reviewed_by: Schema.optionalKey(Schema.String),
})

/** Accept a source disagreement: both reports stand, and the finding is acknowledged. */
export const AcceptDisagreement = Norn.action({
  id: "source.agreement.accept", title: "Accept a source disagreement", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: AcceptInput, output: Schema.Struct({ id: Schema.String }),
  inputLabels: {
    id: { title: "Finding", description: "The disagreement finding to accept, as the record's findings name it." },
    status: { title: "Decision", description: "Accepted: both sources' reports stand." },
    reason: { title: "Reason", description: "Why the disagreement is expected, kept with the review." },
  },
  derive: [{ field: "reviewed_by", inputRequired: false,
    expression: Norn.expression(Schema.Struct({ actor: Schema.Struct({ subjectId: Schema.String }) }), context => context.actor.subjectId, { root: "context" }) }],
  preconditions: [Norn.expression(Schema.Struct({ input: AcceptInput }), x => x.input.reason !== "")],
  writes: [{ type: AgreementReview, change: "modified" }, { type: AgreementReview, change: "created" }],
  effect: { kind: "upsert_input_object", type: AgreementReview, objectIdField: "id", writeMode: { kind: "upsert" } },
  access: { artifactId: "source.agreement.access.policy", version: "0.1.0" },
})

/** Reopen an accepted disagreement: the finding is open again while the sources disagree. */
export const ReopenDisagreement = Norn.action({
  id: "source.agreement.reopen", title: "Reopen a source disagreement", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: ReopenInput, output: Schema.Struct({ id: Schema.String }),
  inputLabels: {
    id: { title: "Finding", description: "The accepted disagreement finding to reopen." },
    status: { title: "Decision", description: "Reopened: the disagreement needs attention again." },
    reason: { title: "Reason", description: "Why the acceptance no longer holds, kept with the review." },
  },
  derive: [{ field: "reviewed_by", inputRequired: false,
    expression: Norn.expression(Schema.Struct({ actor: Schema.Struct({ subjectId: Schema.String }) }), context => context.actor.subjectId, { root: "context" }) }],
  preconditions: [Norn.expression(Schema.Struct({ input: ReopenInput }), x => x.input.reason !== "")],
  // An upsert may create; a reopened review of a never-accepted finding changes nothing.
  writes: [{ type: AgreementReview, change: "modified" }, { type: AgreementReview, change: "created" }],
  effect: { kind: "upsert_input_object", type: AgreementReview, objectIdField: "id", writeMode: { kind: "upsert" } },
  access: { artifactId: "source.agreement.access.policy", version: "0.1.0" },
})

/** Who may review a source disagreement: a data steward, an admin or an owner. */
export const AgreementAccess = Norn.access({
  id: "source.agreement.access", title: "Source disagreement steward", version: "0.1.0",
  rules: [{ ruleId: "steward", actions: [AcceptDisagreement, ReopenDisagreement], resourceType: AgreementReview,
    scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["DataSteward", "admin", "owner"] } }],
  }],
})
