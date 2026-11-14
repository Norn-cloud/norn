import { Schema } from "effect"
import { Norn } from "@norn/authoring"

/**
 * Lending: loan applications, their offers and events, offline completion
 * scores, and an assistant that may only offer help.
 *
 * Data: a public bank loan-application log (BPIC 2017, 4TU.ResearchData,
 * doi:10.4121/uuid:5f3067df-f10b-45da-b98b-86ae4c7a310b, CC BY 4.0), read
 * into one row per application, offer and event. Scores come from a logistic
 * regression trained outside Norn; Norn records them, it does not train.
 */

const Money = Norn.decimal(2, { currency: "currency" })
const At = Norn.timestamp("utc")

/** A record read from a source: every mapped lending object has a source identity. */
export const SourceRecord = Norn.interfaceType({
  name: "lending.SourceRecord", title: "Lending source record", version: "0.1.0",
  description: "A lending object read from a source system, keyed by the source's own identifier.",
  properties: Schema.Struct({ id: Schema.String }),
})

// ---------------------------------------------------------------- applications

const ApplicationValues = Schema.Struct({
  id: Schema.String,
  loan_goal: Schema.String,
  application_type: Schema.Literals(["New credit", "Limit raise"]),
  currency: Schema.Literals(["EUR"]),
  requested_amount: Money,
  created_at: At,
  last_event_at: At,
  // Minutes from creation to the last event, completion and outcome, as the source recorded them.
  // Law expressions cannot order two timestamps yet, so the contract checks these offsets.
  minutes_to_last_event: Schema.Int,
  minutes_to_complete: Schema.NullOr(Schema.Int),
  minutes_to_outcome: Schema.NullOr(Schema.Int),
  // Furthest funnel stage reached: submitted → validated → offered → accepted | denied | cancelled.
  stage: Schema.Literals(["submitted", "validated", "offered", "accepted", "denied", "cancelled"]),
  outcome: Schema.Literals(["accepted", "denied", "cancelled", "open"]),
  offer_count: Schema.Int,
  event_count: Schema.Int,
  // Ground truth for the model's target: the application completed within 24 hours of creation.
  completed_within_24h: Schema.Boolean,
  // A credit officer's review; only officers move it (lending.access).
  review: Schema.Literals(["pending", "approved", "declined"]),
})

export const Application = Norn.type({
  name: "lending.Application", title: "Loan application", version: "0.1.0", role: "thing",
  implements: [SourceRecord],
  description: "One loan application as the source log records it: what was asked for, when it reached each funnel stage, and how it ended.",
  properties: ApplicationValues,
  identity: [{ name: "primary", properties: ["id"] }],
  lifecycles: [{ property: "review", initial: ["pending"], terminal: ["approved", "declined"], transitions: [
    { id: "approve", from: ["pending"], to: "approved" },
    { id: "decline", from: ["pending"], to: "declined" },
  ] }],
})

/** Data contract: an application states the amount it asks for. */
export const AmountPresent = Norn.law({
  id: "lending.application.amount_present.law", title: "An application states a requested amount", version: "0.1.0",
  dependencies: [{ artifactId: "lending.Application.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "lending.application.amount_present", owner: "lending", severity: "error",
    rationale: "An application with no requested amount cannot be priced or scored; it is held back until the amount is known.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/lending/lending.ts" },
    subject: "lending.Application",
    scope: { root: { kind: "entity", id: "lending.Application" }, objectTypes: ["lending.Application"] },
    condition: "true", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.requested_amount > 0" },
  },
})

/** Data contract: nothing about an application happens before it was created. */
export const TimesOrdered = Norn.law({
  id: "lending.application.times_ordered.law", title: "An application's milestones follow its creation", version: "0.1.0",
  dependencies: [{ artifactId: "lending.Application.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "lending.application.times_ordered", owner: "lending", severity: "error",
    rationale: "A milestone recorded before the application existed is an impossible timestamp: a clock or extraction fault.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/lending/lending.ts" },
    subject: "lending.Application",
    scope: { root: { kind: "entity", id: "lending.Application" }, objectTypes: ["lending.Application"] },
    condition: "true", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.minutes_to_last_event >= 0 and (self.minutes_to_complete ?? 0) >= 0 and (self.minutes_to_outcome ?? 0) >= 0" },
  },
})

// ---------------------------------------------------------------- offers

export const Offer = Norn.type({
  name: "lending.Offer", title: "Loan offer", version: "0.1.0", role: "thing",
  implements: [SourceRecord],
  description: "A loan offer made on an application: amount, monthly cost, term and the applicant's response.",
  properties: Schema.Struct({
    id: Schema.String, application_id: Schema.String, currency: Schema.Literals(["EUR"]),
    offered_amount: Money, monthly_cost: Money, first_withdrawal: Money,
    terms: Schema.Int, credit_score: Schema.Int, selected: Schema.Boolean,
    status: Schema.Literals(["created", "sent", "returned", "accepted", "refused", "cancelled"]),
    created_at: At,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "application", target: Application, cardinality: { to: "one", from: "many", required: true }, inverse: "offers" }],
})

/** Data contract: the repayments of an offer cover what it lends. */
export const RepaymentCovers = Norn.law({
  id: "lending.offer.repayment_covers.law", title: "An offer's repayments cover its amount", version: "0.1.0",
  dependencies: [{ artifactId: "lending.Offer.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "lending.offer.repayment_covers", owner: "lending", severity: "error",
    rationale: "Monthly cost times the number of terms below the offered amount means a term or a cost was misread.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/lending/lending.ts" },
    subject: "lending.Offer",
    scope: { root: { kind: "entity", id: "lending.Offer" }, objectTypes: ["lending.Offer"] },
    condition: "true", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.monthly_cost * self.terms >= self.offered_amount and self.first_withdrawal <= self.offered_amount" },
  },
})

// ---------------------------------------------------------------- events

/** An event read from a source log, keyed by the source's own event identifier. */
export const SourceEvent = Norn.interfaceType({
  name: "lending.SourceEvent", title: "Lending source event", version: "0.1.0", role: "happening",
  description: "A lending event read from a source log, keyed by the source's own event identifier.",
  properties: Schema.Struct({ id: Schema.String }),
})

export const ApplicationEvent = Norn.type({
  name: "lending.ApplicationEvent", title: "Application event", version: "0.1.0", role: "happening",
  implements: [SourceEvent],
  description: "One event of the source log on an application: its activity, lifecycle step, who performed it and when it happened (its valid time).",
  properties: Schema.Struct({
    id: Schema.String, application_id: Schema.String, activity: Schema.String,
    origin: Schema.Literals(["Application", "Offer", "Workflow"]), lifecycle: Schema.String,
    resource: Schema.String, offer_id: Schema.NullOr(Schema.String), occurred_at: At,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "application", target: Application, cardinality: { to: "one", from: "many", required: true }, inverse: "events" }],
})

// ---------------------------------------------------------------- scores (offline model)

export const CompletionScore = Norn.type({
  name: "lending.CompletionScore", title: "Completion score", version: "0.1.0", role: "thing",
  implements: [SourceRecord],
  description: "The offline model's probability that an application completes within 24 hours, from what was known at minute 10. Recorded with the model version that produced it; Norn does not train or run the model.",
  properties: Schema.Struct({
    id: Schema.String, application_id: Schema.String, model_version: Schema.String,
    score: Norn.decimal(4), scored_at: At, features_digest: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

/** Data contract: a score is a probability. */
export const ScoreIsProbability = Norn.law({
  id: "lending.score.is_probability.law", title: "A completion score is a probability", version: "0.1.0",
  dependencies: [{ artifactId: "lending.CompletionScore.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "lending.score.is_probability", owner: "lending", severity: "error",
    rationale: "A score outside [0, 1] did not come from the declared model; it is held back.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/lending/lending.ts" },
    subject: "lending.CompletionScore",
    scope: { root: { kind: "entity", id: "lending.CompletionScore" }, objectTypes: ["lending.CompletionScore"] },
    condition: "true", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.score >= 0 and self.score <= 1 and self.model_version != ''" },
  },
})

// ---------------------------------------------------------------- the assistance policy

const PolicyValues = Schema.Struct({
  id: Schema.String, // "<policy>@<version>"
  policy: Schema.String, version: Schema.Int,
  threshold: Norn.decimal(2),
  model_version: Schema.String,
  rationale: Schema.String,
  status: Schema.Literals(["draft", "active", "retired"]),
})

export const AssistancePolicy = Norn.type({
  name: "lending.AssistancePolicy", title: "Assistance policy", version: "0.1.0", role: "thing",
  description: "A versioned rule for when the assistant offers help: applications whose completion score is below the threshold. A draft becomes active only through an officer's promotion.",
  properties: PolicyValues,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["retired"], transitions: [
    { id: "promote", from: ["draft"], to: "active" },
    { id: "retire", from: ["draft", "active"], to: "retired" },
  ] }],
})

export const ProposePolicy = Norn.action({
  id: "lending.policy.propose", title: "Propose an assistance policy version", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String, policy: Schema.String, version: Schema.Int, threshold: Norn.decimal(2), model_version: Schema.String, rationale: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  preconditions: [Norn.unsafeZen<boolean>("input.threshold > 0 and input.threshold < 1 and input.rationale != ''")],
  writes: [{ type: AssistancePolicy, change: "created" }],
  effect: { kind: "upsert_input_object", type: AssistancePolicy, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const PromotePolicy = Norn.action({
  id: "lending.policy.promote", title: "Promote an assistance policy version", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  writes: [{ type: AssistancePolicy, change: "modified" }],
  effect: { kind: "fire_transition", type: AssistancePolicy, objectIdField: "id", lifecycle: "status", transition: "promote" },
})

export const RetirePolicy = Norn.action({
  id: "lending.policy.retire", title: "Retire an assistance policy version", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  writes: [{ type: AssistancePolicy, change: "modified" }],
  effect: { kind: "fire_transition", type: AssistancePolicy, objectIdField: "id", lifecycle: "status", transition: "retire" },
})

// ---------------------------------------------------------------- the assistant

const ProposalValues = Schema.Struct({
  id: Schema.String, application_id: Schema.String,
  kind: Schema.Literals(["offer_help", "approve", "decline", "price"]),
  rationale: Schema.String,
  proposed_by: Schema.String,
  by_agent: Schema.Boolean,
  outcome: Schema.Literals(["allowed", "refused"]),
  outcome_reason: Schema.String,
})

export const AgentProposal = Norn.type({
  name: "lending.AgentProposal", title: "Assistant proposal", version: "0.1.0", role: "happening",
  description: "What the assistant proposed for an application, and what the assistance-only rule made of it. Every proposal is recorded with its receipt, whether it was allowed or refused.",
  properties: ProposalValues,
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "application", target: Application, cardinality: { to: "one", from: "many", required: true }, inverse: "proposals" }],
})

/** Assistance only: an assistant's proposal to approve, decline or price is never carried out. */
export const AssistanceOnly = Norn.law({
  id: "lending.assistance_only.law", title: "The assistant may only offer help", version: "0.1.0",
  dependencies: [{ artifactId: "lending.AgentProposal.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "lending.assistance_only", owner: "lending", severity: "error",
    rationale: "Approving, declining and pricing credit are a credit officer's decisions. An assistant may offer an applicant help; any other proposal it makes is recorded as refused.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/lending/lending.ts" },
    subject: "lending.AgentProposal",
    scope: { root: { kind: "entity", id: "lending.AgentProposal" }, objectTypes: ["lending.AgentProposal"] },
    condition: "self.by_agent", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.kind == 'offer_help' or self.outcome == 'refused'" },
  },
})

const ProposalInput = Schema.Struct({
  id: Schema.String, application_id: Schema.String,
  kind: Schema.Literals(["offer_help", "approve", "decline", "price"]),
  rationale: Schema.String,
  proposed_by: Schema.optionalKey(Schema.String),
  by_agent: Schema.optionalKey(Schema.Boolean),
  outcome: Schema.optionalKey(Schema.Literals(["allowed", "refused"])),
  outcome_reason: Schema.optionalKey(Schema.String),
})

/**
 * Record a proposal. Who proposed is the verified caller, never input; an API
 * key (a `service:` principal) is the assistant. Its approve, decline and
 * price proposals are refused by the assistance-only rule, and recorded so.
 */
export const SubmitProposal = Norn.action({
  id: "lending.proposal.submit", title: "Record an assistant proposal", version: "0.1.0",
  input: ProposalInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  slice: [{ name: "application", type: Application, key: { property: { from: "input", property: "application_id" } } }],
  derive: [
    { field: "proposed_by", inputRequired: false, expression: Norn.unsafeZen<string>("actor.subjectId") },
    { field: "by_agent", inputRequired: false, expression: Norn.unsafeZen<boolean>("startsWith(actor.subjectId, 'service:')") },
    { field: "outcome", inputRequired: false, expression: Norn.unsafeZen<string>("input.kind == 'offer_help' ? 'allowed' : (startsWith(actor.subjectId, 'service:') ? 'refused' : 'allowed')") },
    { field: "outcome_reason", inputRequired: false, expression: Norn.unsafeZen<string>("input.kind == 'offer_help' ? 'Offering help is within the assistant remit.' : (startsWith(actor.subjectId, 'service:') ? 'Refused by lending.assistance_only: approving, declining and pricing are decisions for a credit officer.' : 'Proposed by a person.')") },
  ],
  createLinks: [{ link: "application", targetType: "lending.Application", targetIdField: "application_id" }],
  writes: [{ type: AgentProposal, change: "created" }],
  effect: { kind: "upsert_input_object", type: AgentProposal, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const AssistanceOffer = Norn.type({
  name: "lending.AssistanceOffer", title: "Assistance offer", version: "0.1.0", role: "happening",
  description: "Help offered to an applicant (a call, an email, a chat), under the active assistance policy, because the application's completion score was below its threshold.",
  properties: Schema.Struct({
    id: Schema.String, application_id: Schema.String, score_id: Schema.String, policy_id: Schema.String,
    channel: Schema.Literals(["call", "email", "chat"]), message: Schema.String, offered_by: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

// The score as an action reads it (a closed record; its timestamp is read as text).
const ScoreSlice = Schema.Struct({
  id: Schema.String, application_id: Schema.String, model_version: Schema.String,
  score: Norn.decimal(4), scored_at: Schema.String, features_digest: Schema.String,
})

const OfferHelpInput = Schema.Struct({
  id: Schema.String, application_id: Schema.String, score_id: Schema.String, policy_id: Schema.String,
  channel: Schema.Literals(["call", "email", "chat"]), message: Schema.String,
  offered_by: Schema.optionalKey(Schema.String),
})

/** Offer help, only under the active policy and only below its threshold. */
export const OfferAssistance = Norn.action({
  id: "lending.assistance.offer", title: "Offer an applicant help", version: "0.1.0",
  input: OfferHelpInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  slice: [
    { name: "score", type: CompletionScore, key: { property: { from: "input", property: "score_id" } } },
    { name: "policy", type: AssistancePolicy, key: { property: { from: "input", property: "policy_id" } } },
  ],
  derive: [{ field: "offered_by", inputRequired: false, expression: Norn.unsafeZen<string>("actor.subjectId") }],
  preconditions: [Norn.expression(Schema.Struct({ input: OfferHelpInput, slice: Schema.Struct({ score: ScoreSlice, policy: PolicyValues }) }),
    (x) => x.slice.policy.status === "active" && x.slice.score.application_id === x.input.application_id && x.slice.score.model_version === x.slice.policy.model_version && x.slice.score.score < x.slice.policy.threshold)],
  writes: [{ type: AssistanceOffer, change: "created" }],
  effect: { kind: "upsert_input_object", type: AssistanceOffer, objectIdField: "id", writeMode: { kind: "create_only" } },
})

// ---------------------------------------------------------------- officer decisions

export const ApproveApplication = Norn.action({
  id: "lending.application.approve", title: "Approve an application", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  writes: [{ type: Application, change: "modified" }],
  effect: { kind: "fire_transition", type: Application, objectIdField: "id", lifecycle: "review", transition: "approve" },
})

export const DeclineApplication = Norn.action({
  id: "lending.application.decline", title: "Decline an application", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  writes: [{ type: Application, change: "modified" }],
  effect: { kind: "fire_transition", type: Application, objectIdField: "id", lifecycle: "review", transition: "decline" },
})

const PriceInput = Schema.Struct({
  id: Schema.String, application_id: Schema.String, currency: Schema.Literals(["EUR"]),
  offered_amount: Money, monthly_cost: Money, first_withdrawal: Money, terms: Schema.Int, credit_score: Schema.Int,
  selected: Schema.Boolean, status: Schema.Literals(["created"]), created_at: Schema.String,
})

export const PriceOffer = Norn.action({
  id: "lending.offer.price", title: "Price an offer", version: "0.1.0",
  input: PriceInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "lending.access.policy", version: "0.1.0" },
  slice: [{ name: "application", type: Application, key: { property: { from: "input", property: "application_id" } } }],
  createLinks: [{ link: "application", targetType: "lending.Application", targetIdField: "application_id" }],
  writes: [{ type: Offer, change: "created" }],
  effect: { kind: "upsert_input_object", type: Offer, objectIdField: "id", writeMode: { kind: "create_only" } },
})

/**
 * Who may do what. Members (and the assistant's API key, which is a member)
 * record proposals and offer help; only admins and owners, the credit
 * officers, approve, decline, price and promote a policy.
 */
export const LendingAccess = Norn.access({
  id: "lending.access", title: "Lending", version: "0.1.0",
  rules: [
    { ruleId: "propose", actions: [SubmitProposal], resourceType: AgentProposal,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "assist", actions: [OfferAssistance], resourceType: AssistanceOffer,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "policy_draft", actions: [ProposePolicy], resourceType: AssistancePolicy,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "policy_promote", actions: [PromotePolicy, RetirePolicy], resourceType: AssistancePolicy,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["admin", "owner"] } }] },
    { ruleId: "decide", actions: [ApproveApplication, DeclineApplication], resourceType: Application,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["admin", "owner"] } }] },
    { ruleId: "price", actions: [PriceOffer], resourceType: Offer,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["admin", "owner"] } }] },
  ],
})
