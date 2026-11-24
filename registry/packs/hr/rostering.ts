/**
 * HR Domain Pack — Slice 3: Labor Capacity, Shift Rostering, Business-Terms
 * Preference Profiles, Immutable Attendance Events, Attendance Reversals,
 * Attendance Regularization, Laws, and Deterministic Stock Solver Handle
 * (`norn.solver.rostering@1`) (#1593).
 *
 * Guided by `registry/packs/frappe-hr/ORACLE-CHECKLIST.md` (`HR-SH-01..06`, `HR-AT-01..05`)
 * and `registry/packs/PROJECTION.md` (`norn.link.frappe-hr.canon` canon gaps).
 * Strictly jurisdiction-agnostic: statutory daily/weekly hour caps, weekly rest
 * floors, and consecutive-day limits belong in `loc-*` conjunctive overlay packs.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Commitment, EconomicEvent, Resource } from "../../core/src/rea.ts"
import { TimeInterval } from "../../core/src/time.ts"
import { ISO_4217 } from "../canon/records.ts"
import { OrgUnit } from "../org/org.ts"
import { Calendar } from "../time/calendars.ts"
import { Person } from "./people_leave.ts"

export const LaborCapacity = Norn.type({
  name: "hr.LaborCapacity",
  role: "thing",
  title: "Labor capacity",
  version: "0.1.0",
  description: "Conserved employee labor capacity in hours (implements core.Resource) backing shift commitments and attendance events.",
  implements: [Resource],
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    role_code: Norn.ref("core.code"),
    period_code: Norn.ref("core.code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    unit: "h",
  },
  links: [
    { id: "owner", description: "Employee whose labor capacity is represented.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", description: "Organizational unit scheduling or administering this capacity.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const LaborCapacityQuantitySubject = Schema.Struct({
  quantity: Norn.decimal(6),
})

export const LaborCapacityNonNegative = Norn.law({
  id: "hr.LaborCapacity.non_negative.law",
  title: "Labor capacity quantity is non-negative",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LaborCapacity.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.LaborCapacity.non_negative",
    owner: "hr",
    severity: "error",
    rationale: "Conserved labor capacity in hours cannot drop below zero.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/rostering.ts" },
    subject: "hr.LaborCapacity",
    scope: { root: { kind: "entity", id: "hr.LaborCapacity" }, objectTypes: ["hr.LaborCapacity"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(LaborCapacityQuantitySubject, (self) => self.quantity >= 0),
    },
  },
})

export const LaborCapacityValidHolds = Norn.example("forty hours of labor capacity holds", {
  of: LaborCapacityNonNegative,
  given: { id: "lc_1", resource_code: "labor_hours", quantity: 40, unit: "h", currency: null, valuation_method: null, role_code: "cashier", period_code: "2026-W48" },
  expect: "holds",
})

export const LaborCapacityNegativeViolated = Norn.example("negative labor capacity is refused", {
  of: LaborCapacityNonNegative,
  given: { id: "lc_bad", resource_code: "labor_hours", quantity: -4, unit: "h", currency: null, valuation_method: null, role_code: "cashier", period_code: "2026-W48" },
  expect: "violated",
})

const PreferenceProfileProps = Schema.Struct({
  id: Schema.String,
  jurisdiction_code: Schema.NullOr(Norn.ref("core.code")),
  label: Norn.ref("core.display_name"),
  currency: Schema.Literals(ISO_4217),
  weekly_overtime_budget: Norn.ref("core.money_amount"),
  min_rest_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
  max_daily_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
  max_weekly_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
  min_weekly_rest_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
  max_consecutive_days: Schema.Int,
  weekend_fairness_priority: Schema.Literals(["low", "medium", "high"]),
  staff_preference_priority: Schema.Literals(["low", "medium", "high"]),
  avoid_split_shifts: Schema.Boolean,
})

export const PreferenceProfile = Norn.type({
  name: "hr.PreferenceProfile",
  role: "thing",
  title: "Rostering preference profile",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Governed business-terms trade-offs for deterministic rostering (cost cap, daily/weekly hours, rest preferences, weekend fairness, split-shift avoidance).",
  properties: PreferenceProfileProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    weekend_fairness_priority: "high",
    staff_preference_priority: "medium",
    avoid_split_shifts: true,
  },
  links: [
    { id: "unit", description: "Branch or unit this preference profile governs.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "calendar", description: "Operating calendar for timezone and holidays.", target: Calendar, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const ShiftRequirementProps = Schema.Struct({
  id: Schema.String,
  role_code: Norn.ref("core.code"),
  starts_at: Norn.ref("core.time.instant"),
  ends_at: Norn.ref("core.time.instant"),
  timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
  required_count: Schema.Int,
})

export const ShiftRequirement = Norn.type({
  name: "hr.ShiftRequirement",
  role: "happening",
  title: "Shift coverage requirement",
  version: "0.1.0",
  description: "Required staffing count for a role and proper half-open UTC [starts_at, ends_at) shift slot at one organizational unit (implements core.TimeInterval).",
  implements: [TimeInterval],
  properties: ShiftRequirementProps,
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "unit", description: "Branch or department requiring coverage.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const RosterStatus = Schema.Literals(["draft", "proposed", "published", "superseded"])

const RosterProps = Schema.Struct({
  id: Schema.String,
  period_start: Norn.date(),
  period_end: Norn.date(),
  solver_receipt_digest: Schema.NullOr(Schema.String),
  status: RosterStatus,
})

export const Roster = Norn.type({
  name: "hr.Roster",
  role: "happening",
  title: "Shift roster",
  version: "0.1.0",
  description: "Weekly or period shift roster proposed by the deterministic stock solver and published via governed transition.",
  properties: RosterProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "unit", description: "Branch covered by this roster.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "preference_profile", description: "Business-terms preference profile pinned for this roster.", target: PreferenceProfile, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["superseded"],
      immutable: ["superseded"],
      transitions: [
        { id: "propose", from: ["draft"], to: "proposed" },
        { id: "reopen_draft", from: ["proposed"], to: "draft" },
        {
          id: "publish",
          from: ["proposed"],
          to: "published",
          guards: [Norn.expression(RosterProps, (self) => self.solver_receipt_digest !== null)],
        },
        { id: "supersede", from: ["published"], to: "superseded" },
      ],
    },
  ],
})

export const ShiftCommitment = Norn.type({
  name: "hr.ShiftCommitment",
  role: "happening",
  title: "Shift commitment",
  version: "0.1.0",
  description: "Scheduled shift assignment (implements core.Commitment and core.TimeInterval with proper half-open UTC [starts_at, ends_at) interval).",
  implements: [Commitment, TimeInterval],
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    starts_at: Norn.ref("core.time.instant"),
    ends_at: Norn.ref("core.time.instant"),
    timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
    role_code: Norn.ref("core.code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "promised",
  },
  links: [
    { id: "provider", description: "Employee scheduled to work the shift.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Branch or unit receiving the labor.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Employee labor capacity scheduled by this shift.", target: LaborCapacity, cardinality: { to: "one", from: "many" } },
    { id: "roster", description: "Parent roster governing this shift.", target: Roster, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const AttendanceEventProps = Schema.Struct({
  id: Schema.String,
  action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
  occurred_at: Norn.ref("iso8601.timestamp"),
  quantity: Norn.ref("core.quantity"),
  unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
  currency: Schema.NullOr(Norn.ref("core.code")),
  starts_at: Norn.ref("core.time.instant"),
  ends_at: Norn.ref("core.time.instant"),
  timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
})

export const AttendanceEvent = Norn.type({
  name: "hr.AttendanceEvent",
  role: "happening",
  title: "Attendance event",
  version: "0.1.0",
  description: "Immutable posted work event fulfilling a ShiftCommitment (implements core.EconomicEvent and core.TimeInterval; clock-in/out map directly to starts_at/ends_at; corrections create a linked reversing event per HR-AT-01).",
  implements: [EconomicEvent, TimeInterval],
  properties: AttendanceEventProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    action: "produce",
  },
  links: [
    { id: "provider", description: "Employee who worked the shift.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Branch or unit where work occurred.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Employee labor capacity delivered in this event.", target: LaborCapacity, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", description: "ShiftCommitment fulfilled by this attendance record.", target: ShiftCommitment, cardinality: { to: "one", from: "many" } },
    { id: "reverses", description: "Prior attendance event reversed by this correction.", target: "hr.AttendanceEvent", cardinality: { to: "one", from: "one" } },
  ],
})

const AttendanceRequestStatus = Schema.Literals(["draft", "pending", "approved", "rejected"])

export const AttendanceRequest = Norn.type({
  name: "hr.AttendanceRequest",
  role: "happening",
  title: "Attendance regularization request",
  version: "0.1.0",
  description: "Self-service employee request to regularize missed or remote attendance with independent manager approval.",
  properties: Schema.Struct({
    id: Schema.String,
    requester_user_id: Schema.String,
    work_date: Norn.date(),
    reason: Schema.String,
    status: AttendanceRequestStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "person", description: "Employee requesting attendance regularization.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "unit", description: "Branch or unit confirming attendance.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["approved", "rejected"],
      immutable: ["approved"],
      transitions: [
        { id: "submit", from: ["draft"], to: "pending" },
        { id: "approve", from: ["pending"], to: "approved" },
        { id: "reject", from: ["pending"], to: "rejected" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Governed Laws & LAW-EXAMPLE-1 Cases

const PreferenceProfileBoundsSubject = Schema.Struct({
  min_rest_hours: Norn.decimal(2),
  max_daily_hours: Norn.decimal(2),
  max_weekly_hours: Norn.decimal(2),
  min_weekly_rest_hours: Norn.decimal(2),
  max_consecutive_days: Schema.Int,
  weekly_overtime_budget: Norn.decimal(6),
})

export const PreferenceProfileValidBounds = Norn.law({
  id: "hr.PreferenceProfile.valid_bounds.law",
  title: "Rostering preference profile bounds are valid and non-negative",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.PreferenceProfile.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.PreferenceProfile.valid_bounds",
    owner: "hr",
    severity: "error",
    rationale: "Operational rest preferences, daily/weekly hour bounds, weekly rest floors, overtime budgets, and consecutive-day caps must stay inside universal physical bounds (statutory caps compose via loc-* overlays).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/rostering.ts" },
    subject: "hr.PreferenceProfile",
    scope: { root: { kind: "entity", id: "hr.PreferenceProfile" }, objectTypes: ["hr.PreferenceProfile"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        PreferenceProfileBoundsSubject,
        (self) =>
          self.min_rest_hours >= 0 &&
          self.max_daily_hours > 0 &&
          self.max_daily_hours <= 24 &&
          self.max_weekly_hours > 0 &&
          self.min_weekly_rest_hours >= 0 &&
          self.min_weekly_rest_hours <= 168 &&
          self.max_consecutive_days >= 1 &&
          self.max_consecutive_days <= 7 &&
          self.weekly_overtime_budget >= 0,
      ),
    },
  },
})

export const PreferenceProfileValidExample = Norn.example("balanced generic preference profile holds", {
  of: PreferenceProfileValidBounds,
  given: {
    id: "prof_standard",
    jurisdiction_code: null,
    label: "Standard Retail Balanced",
    currency: "USD",
    weekly_overtime_budget: 1500,
    min_rest_hours: 10,
    max_daily_hours: 8,
    max_weekly_hours: 40,
    min_weekly_rest_hours: 24,
    max_consecutive_days: 5,
    weekend_fairness_priority: "high",
    staff_preference_priority: "medium",
    avoid_split_shifts: true,
  },
  expect: "holds",
})

export const PreferenceProfileEightConsecutiveViolated = Norn.example("eight consecutive working days exceeds calendar week", {
  of: PreferenceProfileValidBounds,
  given: {
    id: "prof_bad",
    jurisdiction_code: null,
    label: "Malformed 8-day streak",
    currency: "USD",
    weekly_overtime_budget: 1500,
    min_rest_hours: 10,
    max_daily_hours: 8,
    max_weekly_hours: 40,
    min_weekly_rest_hours: 24,
    max_consecutive_days: 8,
    weekend_fairness_priority: "low",
    staff_preference_priority: "low",
    avoid_split_shifts: false,
  },
  expect: "violated",
})

const ShiftRequirementCoverageSubject = Schema.Struct({
  required_count: Schema.Int,
})

export const ShiftRequirementPositiveCoverage = Norn.law({
  id: "hr.ShiftRequirement.positive_coverage.law",
  title: "Shift coverage requirements ask for at least one staff member",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.ShiftRequirement.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.ShiftRequirement.positive_coverage",
    owner: "hr",
    severity: "error",
    rationale: "Zero or negative staffing requirements are malformed solver inputs.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/rostering.ts" },
    subject: "hr.ShiftRequirement",
    scope: { root: { kind: "entity", id: "hr.ShiftRequirement" }, objectTypes: ["hr.ShiftRequirement"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(ShiftRequirementCoverageSubject, (self) => self.required_count >= 1),
    },
  },
})

export const ShiftRequirementValidExample = Norn.example("two cashiers required holds", {
  of: ShiftRequirementPositiveCoverage,
  given: { id: "sr_1", role_code: "cashier", starts_at: "2026-11-28T14:00:00Z", ends_at: "2026-11-28T22:00:00Z", timezone: "UTC", required_count: 2 },
  expect: "holds",
})

export const ShiftRequirementZeroViolated = Norn.example("zero headcount requirement is refused", {
  of: ShiftRequirementPositiveCoverage,
  given: { id: "sr_0", role_code: "cashier", starts_at: "2026-11-28T14:00:00Z", ends_at: "2026-11-28T22:00:00Z", timezone: "UTC", required_count: 0 },
  expect: "violated",
})

export const PublishedRosterRequiresReceipt = Norn.law({
  id: "hr.Roster.published_requires_receipt.law",
  title: "Published rosters must carry an accepted solver receipt digest",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.Roster.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.Roster.published_requires_receipt",
    owner: "hr",
    severity: "error",
    rationale: "No roster may reach published status without an authenticated deterministic solver receipt (HR-SH-01..04).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/rostering.ts" },
    subject: "hr.Roster",
    scope: { root: { kind: "entity", id: "hr.Roster" }, objectTypes: ["hr.Roster"] },
    condition: Norn.expression(RosterProps, (self) => self.status === "published"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(RosterProps, (self) => self.solver_receipt_digest !== null),
    },
  },
})

export const DraftRosterNoReceiptNotApplicable = Norn.example("draft roster without receipt is not yet governed", {
  of: PublishedRosterRequiresReceipt,
  given: { id: "ros_draft", period_start: "2026-11-23", period_end: "2026-11-29", solver_receipt_digest: null, status: "draft" },
  expect: "not_applicable",
})

export const PublishedRosterWithReceiptHolds = Norn.example("published roster with solver receipt holds", {
  of: PublishedRosterRequiresReceipt,
  given: { id: "ros_pub", period_start: "2026-11-23", period_end: "2026-11-29", solver_receipt_digest: "sha256:abc123", status: "published" },
  expect: "holds",
})

export const PublishedRosterMissingReceiptViolated = Norn.example("published roster without solver receipt is refused", {
  of: PublishedRosterRequiresReceipt,
  given: { id: "ros_bad", period_start: "2026-11-23", period_end: "2026-11-29", solver_receipt_digest: null, status: "published" },
  expect: "violated",
})

export const RosterValidPeriod = Norn.law("valid_period", {
  on: Roster,
  must: (self) => self.period_start <= self.period_end,
  because: "Roster period_start must be on or before period_end.",
})

export const RosterValidPeriodHolds = Norn.example("roster with period_start before period_end holds", {
  of: RosterValidPeriod,
  given: { id: "ros_ok", period_start: "2026-11-23", period_end: "2026-11-29", solver_receipt_digest: null, status: "draft" },
  expect: "holds",
})

export const RosterReversedPeriodViolated = Norn.example("roster with period_start after period_end is refused", {
  of: RosterValidPeriod,
  given: { id: "ros_rev", period_start: "2026-11-29", period_end: "2026-11-23", solver_receipt_digest: null, status: "draft" },
  expect: "violated",
})

// ---------------------------------------------------------------- Deterministic Stock Solver Handle & Governed Actions

export const SolveRosterFn = Norn.fn<
  { readonly rosterId: string; readonly unitId: string; readonly preferenceProfileId: string },
  { readonly status: string; readonly assignmentCount: number; readonly solverDigest: string }
>({
  id: "norn.solver.rostering",
  version: "0.1.0",
})

export const ProposeRoster = Norn.action({
  id: "hr.roster.propose",
  title: "Propose shift roster",
  version: "0.1.0",
  description: "Move a draft roster into proposed status once solver assignments are attached.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: Schema.Struct({ rosterId: Schema.String }),
  output: Schema.Struct({ rosterId: Schema.String, status: Schema.String }),
  writes: [{ type: Roster, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Roster,
    objectIdField: "rosterId",
    lifecycle: "status",
    transition: "propose",
  },
})

const PublishRosterSlice = Schema.Struct({
  slice: Schema.Struct({
    roster: Schema.Struct({
      solver_receipt_digest: Schema.NullOr(Schema.String),
    }),
  }),
})

/**
 * Single-write engine boundary note: this action transitions `hr.Roster` once a
 * non-null `solver_receipt_digest` is present; cryptographic receipt acceptance
 * (`acceptReceipt`) and multi-object `hr.ShiftCommitment` materialization in one
 * atomic commit wait on engine `#1586` (`synchronousRule` / receipt-effect wiring).
 */
export const PublishRoster = Norn.action({
  id: "hr.roster.publish",
  title: "Publish shift roster",
  version: "0.1.0",
  description: "Publish a proposed shift roster after verifying its accepted deterministic solver receipt.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "roster", type: Roster, key: { property: { from: "input", property: "rosterId" } } },
  ],
  preconditions: [
    Norn.expression(
      PublishRosterSlice,
      (ctx) => ctx.slice.roster.solver_receipt_digest !== null,
      { root: "context" },
    ),
  ],
  input: Schema.Struct({ rosterId: Schema.String }),
  output: Schema.Struct({ rosterId: Schema.String, status: Schema.String }),
  writes: [{ type: Roster, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Roster,
    objectIdField: "rosterId",
    lifecycle: "status",
    transition: "publish",
  },
})

const AttendanceQuantityContext = Schema.Struct({
  input: Schema.Struct({
    quantity: Norn.decimal(6),
  }),
})

const RecordAttendanceLinksContext = Schema.Struct({
  input: Schema.Struct({
    providerId: Schema.String,
    receiverId: Schema.String,
    resourceId: Schema.String,
  }),
  slice: Schema.Struct({
    commitment_provider: Schema.Array(Schema.Struct({ id: Schema.String })),
    commitment_receiver: Schema.Array(Schema.Struct({ id: Schema.String })),
    commitment_resource: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

export const RecordAttendance = Norn.action({
  id: "hr.attendance.record",
  title: "Record immutable attendance event",
  version: "0.1.0",
  description: "Post a create-only AttendanceEvent delivering hr.LaborCapacity and fulfilling a ShiftCommitment whose linked provider, receiver, and resource match the input (never mutates posted history; HR-AT-01).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    id: Schema.String,
    action: Schema.String,
    occurred_at: Schema.String,
    quantity: Norn.decimal(6),
    unit: Schema.NullOr(Schema.String),
    currency: Schema.NullOr(Schema.String),
    starts_at: Schema.String,
    ends_at: Schema.String,
    timezone: Schema.NullOr(Schema.String),
    providerId: Schema.String,
    receiverId: Schema.String,
    resourceId: Schema.String,
    shiftCommitmentId: Schema.String,
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "provider", type: Person, key: { property: { from: "input", property: "providerId" } } },
    { name: "receiver", type: OrgUnit, key: { property: { from: "input", property: "receiverId" } } },
    { name: "resource", type: LaborCapacity, key: { property: { from: "input", property: "resourceId" } } },
    { name: "commitment", type: ShiftCommitment, key: { property: { from: "input", property: "shiftCommitmentId" } } },
    { name: "commitment_provider", type: Person, key: { link: { from: "commitment", link: "provider" } } },
    { name: "commitment_receiver", type: OrgUnit, key: { link: { from: "commitment", link: "receiver" } } },
    { name: "commitment_resource", type: LaborCapacity, key: { link: { from: "commitment", link: "resource" } } },
  ],
  preconditions: [
    Norn.expression(
      AttendanceQuantityContext,
      (ctx) => ctx.input.quantity > 0,
      { root: "context" },
    ),
    Norn.expression(
      RecordAttendanceLinksContext,
      (ctx) =>
        ctx.slice.commitment_provider.map((p) => p.id).includes(ctx.input.providerId) &&
        ctx.slice.commitment_receiver.map((r) => r.id).includes(ctx.input.receiverId) &&
        ctx.slice.commitment_resource.map((c) => c.id).includes(ctx.input.resourceId),
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "provider", targetType: Person, targetIdField: "providerId" },
    { link: "receiver", targetType: OrgUnit, targetIdField: "receiverId" },
    { link: "resource", targetType: LaborCapacity, targetIdField: "resourceId" },
    { link: "fulfils", targetType: ShiftCommitment, targetIdField: "shiftCommitmentId" },
  ],
  writes: [{ type: AttendanceEvent, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: AttendanceEvent,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

const ReverseAttendanceQuantityContext = Schema.Struct({
  input: Schema.Struct({
    quantity: Norn.decimal(6),
  }),
  slice: Schema.Struct({
    original: Schema.Struct({
      quantity: Norn.decimal(6),
    }),
  }),
})

const ReverseAttendanceDenominationContext = Schema.Struct({
  input: Schema.Struct({
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    unit: Schema.NullOr(Schema.String),
    currency: Schema.NullOr(Schema.String),
  }),
  slice: Schema.Struct({
    original: Schema.Struct({
      action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
      unit: Schema.NullOr(Schema.String),
      currency: Schema.NullOr(Schema.String),
    }),
  }),
})

const ReverseAttendanceLinksContext = Schema.Struct({
  input: Schema.Struct({
    id: Schema.String,
    providerId: Schema.String,
    receiverId: Schema.String,
    resourceId: Schema.String,
    reversesEventId: Schema.String,
  }),
  slice: Schema.Struct({
    original: Schema.Struct({ id: Schema.String }),
    original_provider: Schema.Array(Schema.Struct({ id: Schema.String })),
    original_receiver: Schema.Array(Schema.Struct({ id: Schema.String })),
    original_resource: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

export const ReverseAttendance = Norn.action({
  id: "hr.attendance.reverse",
  title: "Reverse posted attendance event",
  version: "0.1.0",
  description: "Post a create-only reversing AttendanceEvent linked via `reverses` to a prior AttendanceEvent with inverse REA action (produce <-> consume), exact quantity/unit/currency match, and matching provider, receiver, and labor capacity (HR-AT-01, HR-AT-04).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    id: Schema.String,
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Schema.String,
    quantity: Norn.decimal(6),
    unit: Schema.NullOr(Schema.String),
    currency: Schema.NullOr(Schema.String),
    starts_at: Schema.String,
    ends_at: Schema.String,
    timezone: Schema.NullOr(Schema.String),
    providerId: Schema.String,
    receiverId: Schema.String,
    resourceId: Schema.String,
    reversesEventId: Schema.String,
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "provider", type: Person, key: { property: { from: "input", property: "providerId" } } },
    { name: "receiver", type: OrgUnit, key: { property: { from: "input", property: "receiverId" } } },
    { name: "resource", type: LaborCapacity, key: { property: { from: "input", property: "resourceId" } } },
    { name: "original", type: AttendanceEvent, key: { property: { from: "input", property: "reversesEventId" } } },
    { name: "original_provider", type: Person, key: { link: { from: "original", link: "provider" } } },
    { name: "original_receiver", type: OrgUnit, key: { link: { from: "original", link: "receiver" } } },
    { name: "original_resource", type: LaborCapacity, key: { link: { from: "original", link: "resource" } } },
  ],
  preconditions: [
    Norn.expression(
      ReverseAttendanceQuantityContext,
      (ctx) => ctx.input.quantity > 0 && ctx.input.quantity === ctx.slice.original.quantity,
      { root: "context" },
    ),
    Norn.expression(
      ReverseAttendanceDenominationContext,
      (ctx) =>
        ((ctx.slice.original.action === "produce" && ctx.input.action === "consume") ||
          (ctx.slice.original.action === "consume" && ctx.input.action === "produce")) &&
        (ctx.input.unit === null) === (ctx.slice.original.unit === null) &&
        (ctx.input.unit ?? "") === (ctx.slice.original.unit ?? "") &&
        (ctx.input.currency === null) === (ctx.slice.original.currency === null) &&
        (ctx.input.currency ?? "") === (ctx.slice.original.currency ?? ""),
      { root: "context" },
    ),
    Norn.expression(
      ReverseAttendanceLinksContext,
      (ctx) =>
        ctx.input.id !== ctx.input.reversesEventId &&
        ctx.slice.original.id === ctx.input.reversesEventId &&
        ctx.slice.original_provider.map((p) => p.id).includes(ctx.input.providerId) &&
        ctx.slice.original_receiver.map((r) => r.id).includes(ctx.input.receiverId) &&
        ctx.slice.original_resource.map((c) => c.id).includes(ctx.input.resourceId),
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "provider", targetType: Person, targetIdField: "providerId" },
    { link: "receiver", targetType: OrgUnit, targetIdField: "receiverId" },
    { link: "resource", targetType: LaborCapacity, targetIdField: "resourceId" },
    { link: "reverses", targetType: AttendanceEvent, targetIdField: "reversesEventId" },
  ],
  writes: [{ type: AttendanceEvent, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: AttendanceEvent,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

export const SubmitAttendanceRequest = Norn.action({
  id: "hr.attendance_request.submit",
  title: "Submit attendance regularization request",
  version: "0.1.0",
  description: "Submit a draft attendance regularization request for manager review.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: Schema.Struct({ requestId: Schema.String }),
  output: Schema.Struct({ requestId: Schema.String, status: Schema.String }),
  writes: [{ type: AttendanceRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: AttendanceRequest,
    objectIdField: "requestId",
    lifecycle: "status",
    transition: "submit",
  },
})

export const ApproveAttendanceRequest = Norn.action({
  id: "hr.attendance_request.approve",
  title: "Approve attendance regularization request",
  version: "0.1.0",
  description: "Approve a pending attendance regularization request under independent manager authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: Schema.Struct({ requestId: Schema.String }),
  output: Schema.Struct({ requestId: Schema.String, status: Schema.String }),
  writes: [{ type: AttendanceRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: AttendanceRequest,
    objectIdField: "requestId",
    lifecycle: "status",
    transition: "approve",
  },
})

export const RejectAttendanceRequest = Norn.action({
  id: "hr.attendance_request.reject",
  title: "Reject attendance regularization request",
  version: "0.1.0",
  description: "Reject a pending attendance regularization request under independent manager authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: Schema.Struct({ requestId: Schema.String }),
  output: Schema.Struct({ requestId: Schema.String, status: Schema.String }),
  writes: [{ type: AttendanceRequest, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: AttendanceRequest,
    objectIdField: "requestId",
    lifecycle: "status",
    transition: "reject",
  },
})
