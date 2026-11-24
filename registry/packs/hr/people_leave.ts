/**
 * HR Domain Pack — People, Employment, Markings, Compliance Expiries, and
 * Leave Conservation Ledger (Policies, Allocations, Balances, Holds, Requests,
 * Compensatory Leave, and Leave Encashment) (#1593).
 *
 * Guided by `registry/packs/frappe-hr/ORACLE-CHECKLIST.md` (`HR-LV-01..10`, `HR-PI-01`).
 * Strictly jurisdiction-agnostic: country holidays, statutory labor caps, and
 * localized labels live in separate localization packs (e.g. `registry/packs/loc-eg/`).
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
    kind: Schema.Literals(["person"]),
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

export const PersonKindIsPerson = Norn.law("kind_is_person", {
  on: Person,
  must: (self) => self.kind === "person",
  because: "An HR person record always represents a natural person, never an organization or system.",
})

export const PersonKindPersonHolds = Norn.example("person with kind person holds", {
  of: PersonKindIsPerson,
  given: { id: "p_1", display_name: "Ada Lovelace", kind: "person", gender: "female", national_id: null, phone: null, primary_skill: "engineer" },
  expect: "holds",
})

export const PersonKindOrganizationViolated = Norn.example("person with kind organization is refused", {
  of: PersonKindIsPerson,
  given: { id: "p_org", display_name: "Operations Dept", kind: "organization", gender: null, national_id: null, phone: null, primary_skill: "ops" },
  expect: "violated",
})

export const PersonKindSystemViolated = Norn.example("person with kind system is refused", {
  of: PersonKindIsPerson,
  given: { id: "p_sys", display_name: "Scheduler Bot", kind: "system", gender: null, national_id: null, phone: null, primary_skill: "bot" },
  expect: "violated",
})

const EmploymentStatus = Schema.Literals(["active", "on_leave", "terminated"])

export const Employment = Norn.type({
  name: "hr.Employment",
  role: "thing",
  title: "Employment terms",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Effective-dated employment contract and compensation terms attached to an org.Membership.",
  properties: Schema.Struct({
    id: Schema.String,
    jurisdiction_code: Schema.NullOr(Norn.ref("core.code")),
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

export const EmploymentValidDateRange = Norn.law("valid_date_range", {
  on: Employment,
  must: (self) => self.effective_to === null || Norn.dateBefore(self.effective_from, self.effective_to ?? self.effective_from),
  because: "Employment effective_from must be strictly before effective_to whenever effective_to is present.",
})

export const EmploymentOpenEndedHolds = Norn.example("open-ended employment contract holds", {
  of: EmploymentValidDateRange,
  given: { id: "emp_1", jurisdiction_code: null, effective_from: "2026-01-01", effective_to: null, currency: "USD", base_salary: 5000, hourly_overtime_rate: 30, max_weekly_hours: 40, status: "active" },
  expect: "holds",
})

export const EmploymentBoundedHolds = Norn.example("fixed-term employment with effective_from before effective_to holds", {
  of: EmploymentValidDateRange,
  given: { id: "emp_2", jurisdiction_code: null, effective_from: "2026-01-01", effective_to: "2026-12-31", currency: "USD", base_salary: 5000, hourly_overtime_rate: 30, max_weekly_hours: 40, status: "active" },
  expect: "holds",
})

export const EmploymentEqualDatesViolated = Norn.example("employment with equal effective_from and effective_to is refused", {
  of: EmploymentValidDateRange,
  given: { id: "emp_3", jurisdiction_code: null, effective_from: "2026-06-01", effective_to: "2026-06-01", currency: "USD", base_salary: 5000, hourly_overtime_rate: 30, max_weekly_hours: 40, status: "active" },
  expect: "violated",
})

export const EmploymentReversedDatesViolated = Norn.example("employment with effective_from after effective_to is refused", {
  of: EmploymentValidDateRange,
  given: { id: "emp_4", jurisdiction_code: null, effective_from: "2026-12-31", effective_to: "2026-01-01", currency: "USD", base_salary: 5000, hourly_overtime_rate: 30, max_weekly_hours: 40, status: "active" },
  expect: "violated",
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

// ---------------------------------------------------------------- Slice 2: Leave Policy, Allocations, Ledger, Holds & Requests

const LeaveKind = Schema.Literals(["annual", "sick", "unpaid", "emergency", "compensatory"])
const LeavePolicyStatus = Schema.Literals(["active", "archived"])

export const LeavePolicy = Norn.type({
  name: "hr.LeavePolicy",
  role: "thing",
  title: "Leave policy",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Jurisdiction-agnostic entitlement policy declaring annual, sick, and emergency base allowances and carry-forward cap.",
  properties: Schema.Struct({
    id: Schema.String,
    policy_code: Norn.ref("core.code"),
    label: Norn.ref("core.display_name"),
    annual_base_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    sick_base_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    emergency_base_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    max_carry_forward_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    status: LeavePolicyStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "active",
  },
  links: [
    { id: "unit", description: "Optional organizational unit governed by this leave policy.", target: OrgUnit, cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["active"],
      terminal: ["archived"],
      immutable: ["archived"],
      transitions: [
        { id: "archive", from: ["active"], to: "archived" },
      ],
    },
  ],
})

export const LeavePolicyValidEntitlements = Norn.law("valid_entitlements", {
  on: LeavePolicy,
  must: (self) =>
    self.annual_base_days >= 0 &&
    self.sick_base_days >= 0 &&
    self.emergency_base_days >= 0 &&
    self.max_carry_forward_days >= 0,
  because: "Leave policy base entitlements and carry-forward cap must be non-negative.",
})

export const LeavePolicyValidHolds = Norn.example("non-negative leave policy entitlements hold", {
  of: LeavePolicyValidEntitlements,
  given: { id: "lp_std", policy_code: "STD-2026", label: "Standard Leave Policy", annual_base_days: 20, sick_base_days: 10, emergency_base_days: 3, max_carry_forward_days: 5, status: "active" },
  expect: "holds",
})

export const LeavePolicyNegativeCarryForwardViolated = Norn.example("negative carry-forward cap is refused", {
  of: LeavePolicyValidEntitlements,
  given: { id: "lp_bad", policy_code: "BAD-2026", label: "Negative Carry Forward", annual_base_days: 20, sick_base_days: 10, emergency_base_days: 3, max_carry_forward_days: -2, status: "active" },
  expect: "violated",
})

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
    leave_kind: LeaveKind,
    period_year: Schema.Int,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "owner", description: "Employee who owns this leave balance.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", description: "Employer unit administering the balance.", target: OrgUnit, cardinality: { to: "one", from: "many" } },
  ],
})

const LeaveAllocationStatus = Schema.Literals(["draft", "active", "expired", "cancelled"])
const EntitlementTier = Schema.Literals(["prorated_initial", "first_year", "standard", "senior", "protected_disability"])

export const LeaveAllocation = Norn.type({
  name: "hr.LeaveAllocation",
  role: "happening",
  title: "Leave allocation",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Period leave entitlement grant crediting an employee LeaveBalance under a LeavePolicy (HR-LV-01, HR-LV-06).",
  properties: Schema.Struct({
    id: Schema.String,
    jurisdiction_code: Schema.NullOr(Norn.ref("core.code")),
    leave_kind: LeaveKind,
    entitlement_tier: EntitlementTier,
    hazardous_or_remote: Schema.Boolean,
    starts_on: Norn.date(),
    ends_on: Norn.date(),
    allocated_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    carry_forward_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    status: LeaveAllocationStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    entitlement_tier: "standard",
    hazardous_or_remote: false,
    status: "draft",
  },
  links: [
    { id: "person", description: "Employee receiving this leave allocation.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "policy", description: "Leave policy authorizing this allocation.", target: LeavePolicy, cardinality: { to: "one", from: "many", required: true } },
    { id: "balance", description: "Conserved leave balance credited by this allocation.", target: LeaveBalance, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["expired", "cancelled"],
      immutable: ["expired", "cancelled"],
      transitions: [
        { id: "activate", from: ["draft"], to: "active" },
        { id: "expire", from: ["active"], to: "expired" },
        { id: "cancel", from: ["draft", "active"], to: "cancelled" },
      ],
    },
  ],
})

export const LeaveAllocationValid = Norn.law("valid_allocation", {
  on: LeaveAllocation,
  must: (self) => self.starts_on <= self.ends_on && self.allocated_days > 0 && self.carry_forward_days >= 0,
  because: "A leave allocation must cover a valid date range (starts_on <= ends_on) with strictly positive allocated_days and non-negative carry_forward_days.",
})

export const LeaveAllocationValidHolds = Norn.example("valid annual leave allocation holds", {
  of: LeaveAllocationValid,
  given: { id: "la_1", jurisdiction_code: null, leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: false, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 20, carry_forward_days: 2, status: "active" },
  expect: "holds",
})

export const LeaveAllocationReversedDatesViolated = Norn.example("leave allocation with starts_on after ends_on is refused", {
  of: LeaveAllocationValid,
  given: { id: "la_rev", jurisdiction_code: null, leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: false, starts_on: "2026-12-31", ends_on: "2026-01-01", allocated_days: 20, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

export const LeaveAllocationZeroDaysViolated = Norn.example("leave allocation with zero allocated_days is refused", {
  of: LeaveAllocationValid,
  given: { id: "la_zero", jurisdiction_code: null, leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: false, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 0, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

const LeaveHoldStatus = Schema.Literals(["open", "released", "consumed", "expired"])

export const LeaveHold = Norn.type({
  name: "hr.LeaveHold",
  role: "happening",
  title: "Leave hold",
  version: "0.1.0",
  description: "Days reserved against a LeaveBalance while a LeaveRequest awaits approval (implements core.Hold; auto-expires after 7 days).",
  implements: [Hold],
  properties: Schema.Struct({
    id: Schema.String,
    status: LeaveHoldStatus,
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

const QuantitySubject = Schema.Struct({
  quantity: Norn.decimal(6),
})

export const LeaveHoldPositiveQuantity = Norn.law({
  id: "hr.LeaveHold.positive_quantity.law",
  title: "Leave holds reserve strictly positive quantity",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LeaveHold.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.LeaveHold.positive_quantity",
    owner: "hr",
    severity: "error",
    rationale: "A leave hold must reserve strictly positive days against its backing leave balance (HR-LV-04).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/people_leave.ts" },
    subject: "hr.LeaveHold",
    scope: { root: { kind: "entity", id: "hr.LeaveHold" }, objectTypes: ["hr.LeaveHold"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(QuantitySubject, (self) => self.quantity > 0),
    },
  },
})

export const LeaveHoldPositiveHolds = Norn.example("positive leave hold quantity holds", {
  of: LeaveHoldPositiveQuantity,
  given: { id: "lh_1", status: "open", quantity: 2, unit: "d", currency: null, expires_at: null },
  expect: "holds",
})

export const LeaveHoldZeroViolated = Norn.example("zero leave hold quantity is refused", {
  of: LeaveHoldPositiveQuantity,
  given: { id: "lh_0", status: "open", quantity: 0, unit: "d", currency: null, expires_at: null },
  expect: "violated",
})

const LeaveStatus = Schema.Literals(["draft", "pending", "approved", "rejected", "cancelled"])

const LeaveRequestProps = Schema.Struct({
  id: Schema.String,
  requester_user_id: Schema.String,
  leave_kind: LeaveKind,
  starts_on: Norn.date(),
  ends_on: Norn.date(),
  days: Norn.decimal(6),
  medical_note: Schema.NullOr(Schema.String),
  status: LeaveStatus,
})

export const LeaveRequest = Norn.type({
  name: "hr.LeaveRequest",
  role: "happening",
  title: "Leave request",
  version: "0.1.0",
  description: "Governed employee leave request with explicit approval workflow, separation of duties, and positive-day guard.",
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

const CompLeaveStatus = Schema.Literals(["draft", "pending", "approved", "rejected"])

const CompLeaveProps = Schema.Struct({
  id: Schema.String,
  requester_user_id: Schema.String,
  work_date: Norn.date(),
  reason: Schema.String,
  credited_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
  status: CompLeaveStatus,
})

export const CompensatoryLeaveRequest = Norn.type({
  name: "hr.CompensatoryLeaveRequest",
  role: "happening",
  title: "Compensatory leave request",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Employee request to earn compensatory leave days backed by verified holiday or rest-day attendance (HR-LV-08).",
  properties: CompLeaveProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "person", description: "Employee claiming compensatory leave.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "balance", description: "Compensatory leave balance credited upon approval.", target: LeaveBalance, cardinality: { to: "one", from: "many", required: true } },
    { id: "attendance_event", description: "Posted holiday/rest-day attendance event backing this claim.", target: "hr.AttendanceEvent", cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["approved", "rejected"],
      immutable: ["approved"],
      transitions: [
        {
          id: "submit",
          from: ["draft"],
          to: "pending",
          guards: [Norn.expression(CompLeaveProps, (self) => self.credited_days > 0)],
        },
        { id: "approve", from: ["pending"], to: "approved" },
        { id: "reject", from: ["pending"], to: "rejected" },
      ],
    },
  ],
})

export const CompensatoryLeavePositiveDays = Norn.law("positive_days", {
  on: CompensatoryLeaveRequest,
  must: (self) => self.credited_days > 0,
  because: "A compensatory leave request must claim strictly positive credited days (HR-LV-08).",
})

export const CompensatoryLeaveValidHolds = Norn.example("one-day compensatory leave request holds", {
  of: CompensatoryLeavePositiveDays,
  given: { id: "clr_1", requester_user_id: "usr_1", work_date: "2026-07-04", reason: "Worked public holiday shift", credited_days: 1, status: "pending" },
  expect: "holds",
})

export const CompensatoryLeaveZeroViolated = Norn.example("zero-day compensatory leave request is refused", {
  of: CompensatoryLeavePositiveDays,
  given: { id: "clr_0", requester_user_id: "usr_1", work_date: "2026-07-04", reason: "Zero credit", credited_days: 0, status: "pending" },
  expect: "violated",
})

const LeaveEncashmentStatus = Schema.Literals(["draft", "approved", "paid", "cancelled"])

export const LeaveEncashment = Norn.type({
  name: "hr.LeaveEncashment",
  role: "happening",
  title: "Leave encashment",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Governed conversion of eligible unused leave days into a monetary payroll payout (HR-LV-09).",
  properties: Schema.Struct({
    id: Schema.String,
    requester_user_id: Schema.String,
    encashment_date: Norn.date(),
    encashable_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    currency: Schema.Literals(ISO_4217),
    encashment_amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    status: LeaveEncashmentStatus,
  }),
  propertyMarkings: {
    encashment_amount: ["pii.compensation"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "person", description: "Employee encashing leave days.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "balance", description: "Leave balance debited on encashment.", target: LeaveBalance, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["paid", "cancelled"],
      immutable: ["approved", "paid", "cancelled"],
      transitions: [
        { id: "approve", from: ["draft"], to: "approved" },
        { id: "mark_paid", from: ["approved"], to: "paid" },
        { id: "cancel", from: ["draft", "approved"], to: "cancelled" },
      ],
    },
  ],
})

const LeaveEncashmentSubject = Schema.Struct({
  encashable_days: Norn.decimal(2),
  encashment_amount: Norn.decimal(6),
})

export const LeaveEncashmentValid = Norn.law({
  id: "hr.LeaveEncashment.valid_encashment.law",
  title: "Leave encashment converts strictly positive days for a non-negative amount",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LeaveEncashment.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.LeaveEncashment.valid_encashment",
    owner: "hr",
    severity: "error",
    rationale: "Leave encashment must debit strictly positive encashable_days with a non-negative encashment_amount (HR-LV-09).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/people_leave.ts" },
    subject: "hr.LeaveEncashment",
    scope: { root: { kind: "entity", id: "hr.LeaveEncashment" }, objectTypes: ["hr.LeaveEncashment"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(LeaveEncashmentSubject, (self) => self.encashable_days > 0 && self.encashment_amount >= 0),
    },
  },
})

export const LeaveEncashmentValidHolds = Norn.example("positive leave encashment holds", {
  of: LeaveEncashmentValid,
  given: { id: "le_1", requester_user_id: "usr_1", encashment_date: "2026-12-20", encashable_days: 5, currency: "USD", encashment_amount: 1250, status: "approved" },
  expect: "holds",
})

export const LeaveEncashmentZeroDaysViolated = Norn.example("leave encashment with zero days is refused", {
  of: LeaveEncashmentValid,
  given: { id: "le_0", requester_user_id: "usr_1", encashment_date: "2026-12-20", encashable_days: 0, currency: "USD", encashment_amount: 0, status: "draft" },
  expect: "violated",
})

export const LeaveEncashmentNegativeAmountViolated = Norn.example("leave encashment with negative amount is refused", {
  of: LeaveEncashmentValid,
  given: { id: "le_neg", requester_user_id: "usr_1", encashment_date: "2026-12-20", encashable_days: 3, currency: "USD", encashment_amount: -100, status: "draft" },
  expect: "violated",
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
    response: {
      kind: "predicate",
      expression: Norn.expression(QuantitySubject, (self) => self.quantity >= 0),
    },
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
    condition: Norn.expression(LeaveRequestProps, (self) => self.status !== "draft"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(LeaveRequestProps, (self) => self.days > 0),
    },
  },
})

export const LeaveRequestDraftNotApplicable = Norn.example("draft leave request is not yet governed", {
  of: LeaveRequestPositiveDays,
  given: { id: "lr_0", requester_user_id: "usr_1", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-01", days: 0, medical_note: null, status: "draft" },
  expect: "not_applicable",
})

export const LeaveRequestPendingValidHolds = Norn.example("pending two-day leave request holds", {
  of: LeaveRequestPositiveDays,
  given: { id: "lr_1", requester_user_id: "usr_1", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-02", days: 2, medical_note: null, status: "pending" },
  expect: "holds",
})

export const LeaveRequestZeroDaysViolated = Norn.example("pending zero-day leave request is refused", {
  of: LeaveRequestPositiveDays,
  given: { id: "lr_2", requester_user_id: "usr_1", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-01", days: 0, medical_note: null, status: "pending" },
  expect: "violated",
})

export const LeaveRequestValidDateRange = Norn.law("valid_date_range", {
  on: LeaveRequest,
  must: (self) => self.starts_on <= self.ends_on,
  because: "Leave request starts_on must be on or before ends_on.",
})

export const LeaveRequestSingleDayHolds = Norn.example("single-day leave request holds", {
  of: LeaveRequestValidDateRange,
  given: { id: "lr_day", requester_user_id: "usr_1", leave_kind: "annual", starts_on: "2026-12-01", ends_on: "2026-12-01", days: 1, medical_note: null, status: "pending" },
  expect: "holds",
})

export const LeaveRequestReversedDatesViolated = Norn.example("leave request with starts_on after ends_on is refused", {
  of: LeaveRequestValidDateRange,
  given: { id: "lr_rev", requester_user_id: "usr_1", leave_kind: "annual", starts_on: "2026-12-05", ends_on: "2026-12-01", days: 1, medical_note: null, status: "pending" },
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
  status: Schema.String,
})

const LeaveBalanceLinkSlice = Schema.Struct({
  slice: Schema.Struct({
    balance: Schema.Struct({
      id: Schema.String,
    }),
    request_balance: Schema.Array(Schema.Struct({
      id: Schema.String,
    })),
  }),
})

const LeaveBalanceAmountSlice = Schema.Struct({
  slice: Schema.Struct({
    request: Schema.Struct({
      days: Norn.decimal(6),
      status: LeaveStatus,
    }),
    balance: Schema.Struct({
      quantity: Norn.decimal(6),
    }),
  }),
})

export const SubmitLeave = Norn.action({
  id: "hr.leave.submit",
  title: "Submit leave request",
  version: "0.1.0",
  description: "Move a draft leave request into pending manager approval once its linked leave balance holds sufficient days.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "request", type: LeaveRequest, key: { property: { from: "input", property: "leaveRequestId" } } },
    { name: "balance", type: LeaveBalance, key: { property: { from: "input", property: "balanceId" } } },
    { name: "request_balance", type: LeaveBalance, key: { link: { from: "request", link: "balance" } } },
  ],
  preconditions: [
    Norn.expression(
      LeaveBalanceLinkSlice,
      (ctx) => ctx.slice.request_balance.map((b) => b.id).includes(ctx.slice.balance.id),
      { root: "context" },
    ),
    Norn.expression(
      LeaveBalanceAmountSlice,
      (ctx) => ctx.slice.request.days > 0 && ctx.slice.balance.quantity >= ctx.slice.request.days,
      { root: "context" },
    ),
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

/**
 * Single-write engine boundary note: this action transitions `hr.LeaveRequest`
 * and checks the linked `hr.LeaveBalance` via `SliceKey::Link`, while atomic
 * multi-object balance debit + `hr.LeaveHold` consumption in one commit waits on
 * engine `#1586` (`synchronousRule` multi-object delta writer wiring).
 */
export const ApproveLeave = Norn.action({
  id: "hr.leave.approve",
  title: "Approve leave request",
  version: "0.1.0",
  description: "Approve a pending leave request after verifying its linked leave balance under independent manager authority (HR-LV-04, HR-LV-05).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "request", type: LeaveRequest, key: { property: { from: "input", property: "leaveRequestId" } } },
    { name: "balance", type: LeaveBalance, key: { property: { from: "input", property: "balanceId" } } },
    { name: "request_balance", type: LeaveBalance, key: { link: { from: "request", link: "balance" } } },
  ],
  preconditions: [
    Norn.expression(
      LeaveBalanceLinkSlice,
      (ctx) => ctx.slice.request_balance.map((b) => b.id).includes(ctx.slice.balance.id),
      { root: "context" },
    ),
    Norn.expression(
      LeaveBalanceAmountSlice,
      (ctx) => ctx.slice.request.status === "pending" && ctx.slice.balance.quantity >= ctx.slice.request.days,
      { root: "context" },
    ),
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
  description: "Reject a pending leave request under independent manager authority.",
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

const LeaveHoldInput = Schema.Struct({
  holdId: Schema.String,
})

const LeaveHoldOutput = Schema.Struct({
  holdId: Schema.String,
  status: Schema.String,
})

const LeaveHoldSlice = Schema.Struct({
  slice: Schema.Struct({
    hold: Schema.Struct({
      status: LeaveHoldStatus,
      quantity: Norn.decimal(6),
    }),
  }),
})

export const ConsumeLeaveHold = Norn.action({
  id: "hr.leave_hold.consume",
  title: "Consume leave hold",
  version: "0.1.0",
  description: "Consume an open leave hold when the associated leave request is approved.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "hold", type: LeaveHold, key: { property: { from: "input", property: "holdId" } } },
  ],
  preconditions: [
    Norn.expression(
      LeaveHoldSlice,
      (ctx) => ctx.slice.hold.status === "open" && ctx.slice.hold.quantity > 0,
      { root: "context" },
    ),
  ],
  input: LeaveHoldInput,
  output: LeaveHoldOutput,
  writes: [{ type: LeaveHold, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveHold,
    objectIdField: "holdId",
    lifecycle: "status",
    transition: "consume",
  },
})

export const ReleaseLeaveHold = Norn.action({
  id: "hr.leave_hold.release",
  title: "Release leave hold",
  version: "0.1.0",
  description: "Release an open leave hold back to available balance when a request is rejected or cancelled.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "hold", type: LeaveHold, key: { property: { from: "input", property: "holdId" } } },
  ],
  preconditions: [
    Norn.expression(
      LeaveHoldSlice,
      (ctx) => ctx.slice.hold.status === "open",
      { root: "context" },
    ),
  ],
  input: LeaveHoldInput,
  output: LeaveHoldOutput,
  writes: [{ type: LeaveHold, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveHold,
    objectIdField: "holdId",
    lifecycle: "status",
    transition: "release",
  },
})

const CompLeaveInput = Schema.Struct({
  requestId: Schema.String,
})

const CompLeaveOutput = Schema.Struct({
  requestId: Schema.String,
  status: Schema.String,
})

const CompLeaveSlice = Schema.Struct({
  slice: Schema.Struct({
    request: Schema.Struct({
      credited_days: Norn.decimal(2),
      status: CompLeaveStatus,
    }),
  }),
})

export const SubmitCompensatoryLeave = Norn.action({
  id: "hr.comp_leave.submit",
  title: "Submit compensatory leave request",
  version: "0.1.0",
  description: "Submit a draft compensatory leave request backed by holiday or rest-day attendance.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "request", type: CompensatoryLeaveRequest, key: { property: { from: "input", property: "requestId" } } },
  ],
  preconditions: [
    Norn.expression(
      CompLeaveSlice,
      (ctx) => ctx.slice.request.credited_days > 0,
      { root: "context" },
    ),
  ],
  input: CompLeaveInput,
  output: CompLeaveOutput,
  writes: [{ type: CompensatoryLeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: CompensatoryLeaveRequest,
    objectIdField: "requestId",
    lifecycle: "status",
    transition: "submit",
  },
})

export const ApproveCompensatoryLeave = Norn.action({
  id: "hr.comp_leave.approve",
  title: "Approve compensatory leave request",
  version: "0.1.0",
  description: "Approve a pending compensatory leave request under independent manager authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "request", type: CompensatoryLeaveRequest, key: { property: { from: "input", property: "requestId" } } },
  ],
  preconditions: [
    Norn.expression(
      CompLeaveSlice,
      (ctx) => ctx.slice.request.status === "pending" && ctx.slice.request.credited_days > 0,
      { root: "context" },
    ),
  ],
  input: CompLeaveInput,
  output: CompLeaveOutput,
  writes: [{ type: CompensatoryLeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: CompensatoryLeaveRequest,
    objectIdField: "requestId",
    lifecycle: "status",
    transition: "approve",
  },
})

export const RejectCompensatoryLeave = Norn.action({
  id: "hr.comp_leave.reject",
  title: "Reject compensatory leave request",
  version: "0.1.0",
  description: "Reject a pending compensatory leave request under independent manager authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: CompLeaveInput,
  output: CompLeaveOutput,
  writes: [{ type: CompensatoryLeaveRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: CompensatoryLeaveRequest,
    objectIdField: "requestId",
    lifecycle: "status",
    transition: "reject",
  },
})

const LeaveEncashmentSlice = Schema.Struct({
  slice: Schema.Struct({
    encashment: Schema.Struct({
      encashable_days: Norn.decimal(2),
      encashment_amount: Norn.decimal(6),
      status: LeaveEncashmentStatus,
    }),
  }),
})

export const ApproveLeaveEncashment = Norn.action({
  id: "hr.leave_encashment.approve",
  title: "Approve leave encashment",
  version: "0.1.0",
  description: "Approve a draft leave encashment under independent HR or payroll authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "encashment", type: LeaveEncashment, key: { property: { from: "input", property: "encashmentId" } } },
  ],
  preconditions: [
    Norn.expression(
      LeaveEncashmentSlice,
      (ctx) =>
        ctx.slice.encashment.status === "draft" &&
        ctx.slice.encashment.encashable_days > 0 &&
        ctx.slice.encashment.encashment_amount >= 0,
      { root: "context" },
    ),
  ],
  input: Schema.Struct({ encashmentId: Schema.String }),
  output: Schema.Struct({ encashmentId: Schema.String, status: Schema.String }),
  writes: [{ type: LeaveEncashment, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LeaveEncashment,
    objectIdField: "encashmentId",
    lifecycle: "status",
    transition: "approve",
  },
})
