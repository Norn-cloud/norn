/**
 * HR Domain Pack — Slice 1 (People, Markings & Compliance Expiries) and
 * Slice 2 (Leave Conservation Ledger, Holds, Laws & Approvals) (#1593).
 *
 * Guided by `registry/packs/frappe-hr/ORACLE-CHECKLIST.md` (`HR-LV-01..06`, `HR-PI-01`).
 * Jurisdiction-agnostic: country holidays, statutory labor caps, and Arabic
 * labels live in separate localization packs (e.g. `registry/packs/loc-eg/`).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent, Hold, Resource } from "../../core/src/rea.ts"
import { ISO_4217 } from "../canon/records.ts"
import { Membership, OrgUnit } from "../org/org.ts"

// ---------------------------------------------------------------- Slice 1: People, Employment & Compliance

export const Person = Norn.type({
  name: "hr.Person",
  role: "thing",
  title: "Person",
  version: "0.1.0",
  description: "A natural person participating in HR relationships (implements core.Agent; sensitive fields marked).",
  implements: [Agent],
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
    gender: Schema.NullOr(Schema.Literals(["female", "male", "unspecified"])),
    national_id: Schema.NullOr(Schema.String),
    phone: Schema.NullOr(Schema.String),
    primary_skill: Norn.ref("core.code"),
  }),
  propertyMarkings: {
    gender: ["pii.demographic"],
    national_id: ["pii.gov_id"],
    phone: ["pii.demographic"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    kind: "person",
  },
})

const EmploymentStatus = Schema.Literals(["active", "on_leave", "terminated"])

export const Employment = Norn.type({
  name: "hr.Employment",
  role: "thing",
  title: "Employment terms",
  version: "0.1.0",
  description: "Effective-dated employment contract and compensation terms attached to an org.Membership.",
  properties: Schema.Struct({
    id: Schema.String,
    effective_from: Norn.date(),
    effective_to: Schema.NullOr(Norn.date()),
    currency: Schema.Literals(ISO_4217),
    base_salary: Norn.decimal(2, { currency: "currency", additivity: { kind: "semi_additive" } }),
    hourly_overtime_rate: Norn.decimal(2, { currency: "currency" }),
    max_weekly_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    status: EmploymentStatus,
  }),
  propertyMarkings: {
    base_salary: ["pii.compensation"],
    hourly_overtime_rate: ["pii.compensation"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "active",
  },
  links: [
    { id: "person", description: "Employed person.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "membership", description: "Underlying W3C ORG membership binding unit and post.", target: Membership, cardinality: { to: "one", from: "many", required: true } },
    { id: "home_unit", description: "Primary branch or department.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["active"],
      terminal: ["terminated"],
      transitions: [
        { id: "start_leave", from: ["active"], to: "on_leave" },
        { id: "return_to_work", from: ["on_leave"], to: "active" },
        { id: "terminate", from: ["active", "on_leave"], to: "terminated" },
      ],
    },
  ],
})

const DocumentStatus = Schema.Literals(["valid", "expiring_soon", "expired", "renewed"])

export const ComplianceDocument = Norn.type({
  name: "hr.ComplianceDocument",
  role: "thing",
  title: "Compliance document",
  version: "0.1.0",
  description: "Tracked ID, visa, contract, or license with explicit expiry date for timer-driven reminders.",
  properties: Schema.Struct({
    id: Schema.String,
    doc_type: Schema.Literals(["national_id", "passport", "visa", "work_permit", "health_cert", "license"]),
    document_number: Schema.String,
    expires_on: Norn.date(),
    status: DocumentStatus,
  }),
  propertyMarkings: {
    document_number: ["pii.gov_id"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "valid",
  },
  links: [
    { id: "person", description: "Document holder.", target: Person, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["valid"],
      terminal: ["renewed"],
      transitions: [
        { id: "mark_expiring", from: ["valid"], to: "expiring_soon" },
        { id: "mark_expired", from: ["valid", "expiring_soon"], to: "expired" },
        { id: "renew", from: ["valid", "expiring_soon", "expired"], to: "renewed" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Slice 2: Leave Ledger, Holds & Approvals

export const LeaveBalance = Norn.type({
  name: "hr.LeaveBalance",
  role: "thing",
  title: "Leave balance",
  version: "0.1.0",
  description: "Conserved leave entitlement balance (reuses core.Resource stock-flow physics in days).",
  implements: [Resource],
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    leave_kind: Schema.Literals(["annual", "sick", "unpaid", "emergency"]),
    period_year: Schema.Int,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "owner", description: "Employee who owns this leave balance.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", description: "Employer unit administering the balance.", target: OrgUnit, cardinality: { to: "one", from: "many" } },
  ],
})

export const LeaveHold = Norn.type({
  name: "hr.LeaveHold",
  role: "happening",
  title: "Leave hold",
  version: "0.1.0",
  description: "Days reserved against a LeaveBalance while a LeaveRequest awaits approval (implements core.Hold; auto-expires after 7 days).",
  implements: [Hold],
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["open", "released", "consumed", "expired"]),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    expires_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "open",
  },
  links: [
    { id: "resource", description: "Held leave balance.", target: LeaveBalance, cardinality: { to: "one", from: "many", required: true } },
    { id: "holder", description: "Employee requesting leave.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "secures", description: "Optional commitment secured by this hold.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["released", "consumed", "expired"],
      transitions: [
        { id: "consume", from: ["open"], to: "consumed" },
        { id: "release", from: ["open"], to: "released" },
        { id: "expire_unapproved", from: ["open"], to: "expired", after: { days: 7 } },
      ],
    },
  ],
})

const LeaveStatus = Schema.Literals(["draft", "pending", "approved", "rejected", "cancelled"])

const LeaveRequestProps = Schema.Struct({
  id: Schema.String,
  leave_kind: Schema.Literals(["annual", "sick", "unpaid", "emergency"]),
  starts_on: Norn.date(),
  ends_on: Norn.date(),
  days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
  medical_note: Schema.NullOr(Schema.String),
  status: LeaveStatus,
})

export const LeaveRequest = Norn.type({
  name: "hr.LeaveRequest",
  role: "happening",
  title: "Leave request",
  version: "0.1.0",
  description: "Governed employee leave request with explicit approval workflow and positive-day guard.",
  properties: LeaveRequestProps,
  propertyMarkings: {
    medical_note: ["pii.medical"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "person", description: "Requesting employee.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "balance", description: "Leave balance debited on approval.", target: LeaveBalance, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["approved", "rejected", "cancelled"],
      immutable: ["approved"],
      transitions: [
        {
          id: "submit",
          from: ["draft"],
          to: "pending",
          guards: [Norn.expression(LeaveRequestProps, (self) => self.days > 0)],
        },
        { id: "approve", from: ["pending"], to: "approved" },
        { id: "reject", from: ["pending"], to: "rejected" },
        { id: "cancel", from: ["draft", "pending"], to: "cancelled" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Governed Laws & LAW-EXAMPLE-1 Cases

export const LeaveBalanceNonNegative = Norn.law({
  id: "hr.LeaveBalance.non_negative.law",
  title: "Leave balance never drops below zero",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LeaveBalance.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.LeaveBalance.non_negative",
    owner: "hr",
    severity: "error",
    rationale: "Conserved leave balances cannot go silently negative (HR-LV-04); unpaid leave is a separate leave_kind.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/people_leave.ts" },
    subject: "hr.LeaveBalance",
    scope: { root: { kind: "entity", id: "hr.LeaveBalance" }, objectTypes: ["hr.LeaveBalance"] },
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.quantity >= 0" },
  },
})

export const LeaveBalanceZeroHolds = Norn.example("zero remaining annual leave is allowed", {
  of: LeaveBalanceNonNegative,
  given: { id: "lb_1", resource_code: "annual_leave", quantity: 0, unit: "d", currency: null, valuation_method: null, leave_kind: "annual", period_year: 2026 },
  expect: "holds",
})

export const LeaveBalanceNegativeViolated = Norn.example("negative leave balance is refused", {
  of: LeaveBalanceNonNegative,
  given: { id: "lb_2", resource_code: "annual_leave", quantity: -1, unit: "d", currency: null, valuation_method: null, leave_kind: "annual", period_year: 2026 },
  expect: "violated",
})

export const LeaveRequestPositiveDays = Norn.law({
  id: "hr.LeaveRequest.positive_days.law",
  title: "Submitted or approved leave requests consume strictly positive days",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LeaveRequest.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.LeaveRequest.positive_days",
    owner: "hr",
    severity: "error",
    rationale: "A non-draft leave request must consume positive days in exact decimal units (HR-LV-01).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/people_leave.ts" },
    subject: "hr.LeaveRequest",
    scope: { root: { kind: "entity", id: "hr.LeaveRequest" }, objectTypes: ["hr.LeaveRequest"] },
    condition: 'self.status != "draft"',
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.days > 0" },
  },
})

export const LeaveRequestDraftNotApplicable = Norn.example("draft leave request is not yet governed", {
  of: LeaveRequestPositiveDays,
  given: { id: "lr_0", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-01", days: 0, medical_note: null, status: "draft" },
  expect: "not_applicable",
})

export const LeaveRequestPendingValidHolds = Norn.example("pending two-day leave request holds", {
  of: LeaveRequestPositiveDays,
  given: { id: "lr_1", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-02", days: 2, medical_note: null, status: "pending" },
  expect: "holds",
})

export const LeaveRequestZeroDaysViolated = Norn.example("pending zero-day leave request is refused", {
  of: LeaveRequestPositiveDays,
  given: { id: "lr_2", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-01", days: 0, medical_note: null, status: "pending" },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Actions (Slice 2)

const LeaveBalanceCheckedInput = Schema.Struct({
  leaveRequestId: Schema.String,
  balanceId: Schema.String,
})

const LeaveTransitionInput = Schema.Struct({
  leaveRequestId: Schema.String,
})

const LeaveTransitionOutput = Schema.Struct({
  leaveRequestId: Schema.String,
  status: LeaveStatus,
})

export const SubmitLeave = Norn.action({
  id: "hr.leave.submit",
  title: "Submit leave request",
  version: "0.1.0",
  description: "Move a draft leave request into pending manager approval once sufficient leave balance exists.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "request", type: "hr.LeaveRequest", key: { property: { from: "input", property: "leaveRequestId" } } },
    { name: "balance", type: "hr.LeaveBalance", key: { property: { from: "input", property: "balanceId" } } },
  ],
  preconditions: [
    Norn.unsafeZen<boolean>("slice.request.days > 0 and slice.balance.quantity >= slice.request.days"),
  ],
  input: LeaveBalanceCheckedInput,
  output: LeaveTransitionOutput,
  writes: [{ type: LeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveRequest,
    objectIdField: "leaveRequestId",
    lifecycle: "status",
    transition: "submit",
  },
})

export const ApproveLeave = Norn.action({
  id: "hr.leave.approve",
  title: "Approve leave request",
  version: "0.1.0",
  description: "Approve a pending leave request after verifying remaining balance under independent manager authority (HR-LV-04, HR-LV-05).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "request", type: "hr.LeaveRequest", key: { property: { from: "input", property: "leaveRequestId" } } },
    { name: "balance", type: "hr.LeaveBalance", key: { property: { from: "input", property: "balanceId" } } },
  ],
  preconditions: [
    Norn.unsafeZen<boolean>('slice.request.status == "pending" and slice.balance.quantity >= slice.request.days'),
  ],
  input: LeaveBalanceCheckedInput,
  output: LeaveTransitionOutput,
  writes: [{ type: LeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveRequest,
    objectIdField: "leaveRequestId",
    lifecycle: "status",
    transition: "approve",
  },
})

export const RejectLeave = Norn.action({
  id: "hr.leave.reject",
  title: "Reject leave request",
  version: "0.1.0",
  description: "Reject a pending leave request and release any pending hold.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: LeaveTransitionInput,
  output: LeaveTransitionOutput,
  writes: [{ type: LeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveRequest,
    objectIdField: "leaveRequestId",
    lifecycle: "status",
    transition: "reject",
  },
})

export const CancelLeave = Norn.action({
  id: "hr.leave.cancel",
  title: "Cancel leave request",
  version: "0.1.0",
  description: "Cancel an unapproved leave request.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: LeaveTransitionInput,
  output: LeaveTransitionOutput,
  writes: [{ type: LeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveRequest,
    objectIdField: "leaveRequestId",
    lifecycle: "status",
    transition: "cancel",
  },
})
