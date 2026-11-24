/**
 * HR Domain Pack — Recruitment, Onboarding Lifecycle & Performance Appraisals (#1593).
 *
 * Guided by `registry/packs/frappe-hr/ORACLE-CHECKLIST.md` (`HR-LC-01..06`).
 * Strictly jurisdiction-agnostic.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { ISO_4217 } from "../canon/records.ts"
import { OrgUnit, Post } from "../org/org.ts"
import { Employment, Person } from "./people_leave.ts"

// ---------------------------------------------------------------- Recruitment: Openings, Applicants & Offers

const JobOpeningStatus = Schema.Literals(["open", "on_hold", "closed"])

export const JobOpening = Norn.type({
  name: "hr.JobOpening",
  role: "thing",
  title: "Job opening",
  version: "0.1.0",
  description: "Authorized headcount requisition linked to an organizational unit and budgeted org.Post.",
  properties: Schema.Struct({
    id: Schema.String,
    title: Norn.ref("core.display_name"),
    openings_count: Schema.Int,
    status: JobOpeningStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "open",
  },
  links: [
    { id: "unit", description: "Organizational unit hiring for this opening.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "post", description: "Budgeted organizational post fulfilled by this opening.", target: Post, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["closed"],
      immutable: ["closed"],
      transitions: [
        { id: "hold", from: ["open"], to: "on_hold" },
        { id: "reopen", from: ["on_hold"], to: "open" },
        { id: "close", from: ["open", "on_hold"], to: "closed" },
      ],
    },
  ],
})

export const JobOpeningPositiveCount = Norn.law("positive_openings", {
  on: JobOpening,
  must: (self) => self.openings_count >= 1,
  because: "A job opening must authorize at least one position (openings_count >= 1).",
})

export const JobOpeningValidHolds = Norn.example("job opening with two seats holds", {
  of: JobOpeningPositiveCount,
  given: { id: "jo_1", title: "Store Associate", openings_count: 2, status: "open" },
  expect: "holds",
})

export const JobOpeningZeroViolated = Norn.example("job opening with zero seats is refused", {
  of: JobOpeningPositiveCount,
  given: { id: "jo_0", title: "Invalid Zero Seat Opening", openings_count: 0, status: "open" },
  expect: "violated",
})

const JobApplicantStatus = Schema.Literals(["applied", "screening", "interviewed", "offered", "hired", "rejected", "withdrawn"])

export const JobApplicant = Norn.type({
  name: "hr.JobApplicant",
  role: "thing",
  title: "Job applicant",
  version: "0.1.0",
  description: "Candidate progressing through the recruitment pipeline for a JobOpening (contact PII marked).",
  properties: Schema.Struct({
    id: Schema.String,
    full_name: Norn.ref("core.display_name"),
    email: Schema.String,
    phone: Schema.NullOr(Schema.String),
    status: JobApplicantStatus,
  }),
  propertyMarkings: {
    email: ["pii.demographic"],
    phone: ["pii.demographic"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "applied",
  },
  links: [
    { id: "opening", description: "Job opening the applicant applied to.", target: JobOpening, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["applied"],
      terminal: ["hired", "rejected", "withdrawn"],
      immutable: ["hired"],
      transitions: [
        { id: "screen", from: ["applied"], to: "screening" },
        { id: "interview", from: ["screening"], to: "interviewed" },
        { id: "extend_offer", from: ["interviewed"], to: "offered" },
        { id: "hire", from: ["offered"], to: "hired" },
        { id: "reject", from: ["applied", "screening", "interviewed", "offered"], to: "rejected" },
        { id: "withdraw", from: ["applied", "screening", "interviewed", "offered"], to: "withdrawn" },
      ],
    },
  ],
})

const JobOfferStatus = Schema.Literals(["draft", "sent", "accepted", "declined", "expired"])

export const JobOffer = Norn.type({
  name: "hr.JobOffer",
  role: "happening",
  title: "Job offer",
  version: "0.1.0",
  description: "Formal compensation offer extended to a JobApplicant with exact CTC and expiry date (HR-LC-01).",
  properties: Schema.Struct({
    id: Schema.String,
    currency: Schema.Literals(ISO_4217),
    offered_ctc: Norn.ref("core.money_amount", { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
    offer_date: Norn.date(),
    expiry_date: Norn.date(),
    status: JobOfferStatus,
  }),
  propertyMarkings: {
    offered_ctc: ["pii.compensation"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "applicant", description: "Candidate receiving this job offer.", target: JobApplicant, cardinality: { to: "one", from: "many", required: true } },
    { id: "unit", description: "Hiring organizational unit.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["accepted", "declined", "expired"],
      immutable: ["accepted"],
      transitions: [
        { id: "send", from: ["draft"], to: "sent" },
        { id: "accept", from: ["sent"], to: "accepted" },
        { id: "decline", from: ["sent"], to: "declined" },
        { id: "expire", from: ["sent"], to: "expired" },
      ],
    },
  ],
})

const JobOfferSubject = Schema.Struct({
  offered_ctc: Norn.decimal(6),
  offer_date: Norn.date(),
  expiry_date: Norn.date(),
})

export const JobOfferValid = Norn.law({
  id: "hr.JobOffer.valid_offer.law",
  title: "Job offer carries strictly positive CTC and non-reversed offer/expiry dates",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.JobOffer.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.JobOffer.valid_offer",
    owner: "hr",
    severity: "error",
    rationale: "A job offer must quote a strictly positive offered_ctc and an expiry_date on or after offer_date (HR-LC-01).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/talent.ts" },
    subject: "hr.JobOffer",
    scope: { root: { kind: "entity", id: "hr.JobOffer" }, objectTypes: ["hr.JobOffer"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        JobOfferSubject,
        (self) => self.offered_ctc > 0 && Norn.parseDate(self.offer_date) <= Norn.parseDate(self.expiry_date),
      ),
    },
  },
})

export const JobOfferValidHolds = Norn.example("valid job offer holds", {
  of: JobOfferValid,
  given: { id: "off_1", currency: "USD", offered_ctc: 72000, offer_date: "2026-11-01", expiry_date: "2026-11-15", status: "sent" },
  expect: "holds",
})

export const JobOfferZeroCtcViolated = Norn.example("job offer with zero offered_ctc is refused", {
  of: JobOfferValid,
  given: { id: "off_0", currency: "USD", offered_ctc: 0, offer_date: "2026-11-01", expiry_date: "2026-11-15", status: "draft" },
  expect: "violated",
})

export const JobOfferReversedDatesViolated = Norn.example("job offer expiring before offer_date is refused", {
  of: JobOfferValid,
  given: { id: "off_rev", currency: "USD", offered_ctc: 72000, offer_date: "2026-11-15", expiry_date: "2026-11-01", status: "draft" },
  expect: "violated",
})

// ---------------------------------------------------------------- Onboarding & Performance Appraisal

const OnboardingStatus = Schema.Literals(["in_progress", "completed", "cancelled"])

const OnboardingProps = Schema.Struct({
  id: Schema.String,
  required_items_count: Schema.Int,
  completed_items_count: Schema.Int,
  status: OnboardingStatus,
})

export const OnboardingChecklist = Norn.type({
  name: "hr.OnboardingChecklist",
  role: "happening",
  title: "Onboarding checklist",
  version: "0.1.0",
  description: "Structured employee onboarding checklist that cannot complete until all required items are finished (HR-LC-01).",
  properties: OnboardingProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "in_progress",
  },
  links: [
    { id: "person", description: "Employee being onboarded.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "employment", description: "Employment contract activated through onboarding.", target: Employment, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["in_progress"],
      terminal: ["completed", "cancelled"],
      immutable: ["completed"],
      transitions: [
        {
          id: "complete",
          from: ["in_progress"],
          to: "completed",
          guards: [Norn.expression(OnboardingProps, (self) => self.completed_items_count === self.required_items_count)],
        },
        { id: "cancel", from: ["in_progress"], to: "cancelled" },
      ],
    },
  ],
})

export const OnboardingValidProgress = Norn.law("valid_progress", {
  on: OnboardingChecklist,
  must: (self) =>
    self.required_items_count >= 1 &&
    self.completed_items_count >= 0 &&
    self.completed_items_count <= self.required_items_count &&
    (self.status !== "completed" || self.completed_items_count === self.required_items_count),
  because: "Onboarding completed_items_count must stay within [0, required_items_count] and equal required_items_count when status is completed.",
})

export const OnboardingInProgressHolds = Norn.example("in-progress onboarding checklist with partial completion holds", {
  of: OnboardingValidProgress,
  given: { id: "onb_1", required_items_count: 5, completed_items_count: 3, status: "in_progress" },
  expect: "holds",
})

export const OnboardingCompletedFullHolds = Norn.example("completed onboarding checklist with all items done holds", {
  of: OnboardingValidProgress,
  given: { id: "onb_2", required_items_count: 5, completed_items_count: 5, status: "completed" },
  expect: "holds",
})

export const OnboardingCompletedIncompleteViolated = Norn.example("completed onboarding checklist with remaining items is refused", {
  of: OnboardingValidProgress,
  given: { id: "onb_bad", required_items_count: 5, completed_items_count: 4, status: "completed" },
  expect: "violated",
})

export const OnboardingExceededCountViolated = Norn.example("onboarding checklist with completed > required is refused", {
  of: OnboardingValidProgress,
  given: { id: "onb_over", required_items_count: 5, completed_items_count: 6, status: "in_progress" },
  expect: "violated",
})

const AppraisalStatus = Schema.Literals(["draft", "submitted", "finalized"])

export const Appraisal = Norn.type({
  name: "hr.Appraisal",
  role: "happening",
  title: "Performance appraisal",
  version: "0.1.0",
  description: "Cycle-bound employee performance appraisal with exact 0..100 decimal score and immutable finalized state (HR-LC-04, HR-LC-05).",
  properties: Schema.Struct({
    id: Schema.String,
    cycle_code: Norn.ref("core.code"),
    score: Norn.decimal(2),
    status: AppraisalStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "person", description: "Employee being evaluated.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "reviewer", description: "Manager or reviewer conducting the evaluation.", target: Person, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["finalized"],
      immutable: ["finalized"],
      transitions: [
        { id: "submit", from: ["draft"], to: "submitted" },
        { id: "finalize", from: ["submitted"], to: "finalized" },
      ],
    },
  ],
})

export const AppraisalValidScore = Norn.law("valid_score", {
  on: Appraisal,
  must: (self) => self.score >= 0 && self.score <= 100,
  because: "Performance appraisal score must stay within the exact 0..100 percentage range (HR-LC-05).",
})

export const AppraisalValidScoreHolds = Norn.example("appraisal score within 0..100 holds", {
  of: AppraisalValidScore,
  given: { id: "app_1", cycle_code: "2026-H2", score: 88.5, status: "finalized" },
  expect: "holds",
})

export const AppraisalScoreAbove100Violated = Norn.example("appraisal score above 100 is refused", {
  of: AppraisalValidScore,
  given: { id: "app_high", cycle_code: "2026-H2", score: 105, status: "submitted" },
  expect: "violated",
})

export const AppraisalScoreNegativeViolated = Norn.example("negative appraisal score is refused", {
  of: AppraisalValidScore,
  given: { id: "app_neg", cycle_code: "2026-H2", score: -1, status: "draft" },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Actions

const JobOfferInput = Schema.Struct({
  offerId: Schema.String,
})

const JobOfferOutput = Schema.Struct({
  offerId: Schema.String,
  status: Schema.String,
})

const JobOfferSlice = Schema.Struct({
  slice: Schema.Struct({
    offer: Schema.Struct({
      offered_ctc: Norn.decimal(6),
      status: JobOfferStatus,
    }),
  }),
})

export const SendJobOffer = Norn.action({
  id: "hr.job_offer.send",
  title: "Send job offer",
  version: "0.1.0",
  description: "Send a draft job offer with positive CTC to the candidate.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "offer", type: JobOffer, key: { property: { from: "input", property: "offerId" } } },
  ],
  preconditions: [
    Norn.expression(
      JobOfferSlice,
      (ctx) => ctx.slice.offer.status === "draft" && ctx.slice.offer.offered_ctc > 0,
      { root: "context" },
    ),
  ],
  input: JobOfferInput,
  output: JobOfferOutput,
  writes: [{ type: JobOffer, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: JobOffer,
    objectIdField: "offerId",
    lifecycle: "status",
    transition: "send",
  },
})

export const AcceptJobOffer = Norn.action({
  id: "hr.job_offer.accept",
  title: "Accept job offer",
  version: "0.1.0",
  description: "Record candidate acceptance of a sent job offer, locking its terms as immutable (HR-LC-01).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "offer", type: JobOffer, key: { property: { from: "input", property: "offerId" } } },
  ],
  preconditions: [
    Norn.expression(
      JobOfferSlice,
      (ctx) => ctx.slice.offer.status === "sent",
      { root: "context" },
    ),
  ],
  input: JobOfferInput,
  output: JobOfferOutput,
  writes: [{ type: JobOffer, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: JobOffer,
    objectIdField: "offerId",
    lifecycle: "status",
    transition: "accept",
  },
})

const OnboardingSlice = Schema.Struct({
  slice: Schema.Struct({
    checklist: Schema.Struct({
      required_items_count: Schema.Int,
      completed_items_count: Schema.Int,
      status: OnboardingStatus,
    }),
  }),
})

export const CompleteOnboarding = Norn.action({
  id: "hr.onboarding.complete",
  title: "Complete employee onboarding",
  version: "0.1.0",
  description: "Mark an employee onboarding checklist completed once every required item is finished.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "checklist", type: OnboardingChecklist, key: { property: { from: "input", property: "checklistId" } } },
  ],
  preconditions: [
    Norn.expression(
      OnboardingSlice,
      (ctx) =>
        ctx.slice.checklist.status === "in_progress" &&
        ctx.slice.checklist.completed_items_count === ctx.slice.checklist.required_items_count,
      { root: "context" },
    ),
  ],
  input: Schema.Struct({ checklistId: Schema.String }),
  output: Schema.Struct({ checklistId: Schema.String, status: Schema.String }),
  writes: [{ type: OnboardingChecklist, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: OnboardingChecklist,
    objectIdField: "checklistId",
    lifecycle: "status",
    transition: "complete",
  },
})

const AppraisalSlice = Schema.Struct({
  slice: Schema.Struct({
    appraisal: Schema.Struct({
      score: Norn.decimal(2),
      status: AppraisalStatus,
    }),
  }),
})

export const FinalizeAppraisal = Norn.action({
  id: "hr.appraisal.finalize",
  title: "Finalize performance appraisal",
  version: "0.1.0",
  description: "Lock a submitted performance appraisal as finalized and immutable (HR-LC-04, HR-LC-05).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "appraisal", type: Appraisal, key: { property: { from: "input", property: "appraisalId" } } },
  ],
  preconditions: [
    Norn.expression(
      AppraisalSlice,
      (ctx) =>
        ctx.slice.appraisal.status === "submitted" &&
        ctx.slice.appraisal.score >= 0 &&
        ctx.slice.appraisal.score <= 100,
      { root: "context" },
    ),
  ],
  input: Schema.Struct({ appraisalId: Schema.String }),
  output: Schema.Struct({ appraisalId: Schema.String, status: Schema.String }),
  writes: [{ type: Appraisal, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Appraisal,
    objectIdField: "appraisalId",
    lifecycle: "status",
    transition: "finalize",
  },
})
