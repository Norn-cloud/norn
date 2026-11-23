/**
 * HR Domain Pack — Slice 3: Shift Rostering, Business-Terms Preference Profiles,
 * Immutable Attendance Events, Attendance Regularization, Laws, and Deterministic
 * Stock Solver Handle (`norn.solver.rostering@1`) (#1593).
 *
 * Guided by `registry/packs/frappe-hr/ORACLE-CHECKLIST.md` (`HR-SH-01..04`, `HR-AT-01`)
 * and `registry/packs/PROJECTION.md` (`norn.link.frappe-hr.canon` canon gaps).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Commitment, EconomicEvent } from "../../core/src/rea.ts"
import { ISO_4217 } from "../canon/records.ts"
import { OrgUnit } from "../org/org.ts"
import { Calendar } from "../time/calendars.ts"
import { LeaveBalance, Person } from "./people_leave.ts"

const PreferenceProfileProps = Schema.Struct({
  id: Schema.String,
  label: Norn.ref("core.display_name"),
  currency: Schema.Literals(ISO_4217),
  weekly_overtime_budget: Norn.decimal(2, { currency: "currency" }),
  min_rest_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
  max_weekly_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
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
  description: "Governed business-terms trade-offs for deterministic rostering (cost cap, weekend fairness, shift requests, split-shift avoidance).",
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
  starts_at: Norn.timestamp("utc"),
  ends_at: Norn.timestamp("utc"),
  required_count: Schema.Int,
})

export const ShiftRequirement = Norn.type({
  name: "hr.ShiftRequirement",
  role: "thing",
  title: "Shift coverage requirement",
  version: "0.1.0",
  description: "Required staffing count for a role and half-open UTC shift slot at one organizational unit.",
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
  description: "Scheduled shift assignment (implements core.Commitment; carries half-open UTC [starts_at, ends_at) interval).",
  implements: [Commitment],
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    starts_at: Norn.timestamp("utc"),
    ends_at: Norn.timestamp("utc"),
    role_code: Norn.ref("core.code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "promised",
  },
  links: [
    { id: "provider", description: "Employee scheduled to work the shift.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Branch or unit receiving the labor.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Optional labor resource classification.", target: LeaveBalance, cardinality: { to: "one", from: "many" } },
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
  clock_in_at: Norn.timestamp("utc"),
  clock_out_at: Norn.timestamp("utc"),
})

export const AttendanceEvent = Norn.type({
  name: "hr.AttendanceEvent",
  role: "happening",
  title: "Attendance event",
  version: "0.1.0",
  description: "Immutable posted work event fulfilling a ShiftCommitment (implements core.EconomicEvent; corrections create a linked reversing event per HR-AT-01).",
  implements: [EconomicEvent],
  properties: AttendanceEventProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    action: "produce",
  },
  links: [
    { id: "provider", description: "Employee who worked the shift.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Branch or unit where work occurred.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Associated time/leave resource.", target: LeaveBalance, cardinality: { to: "one", from: "many", required: true } },
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
  description: "Self-service employee request to regularize missed or remote attendance (covers Frappe HR canon gap).",
  properties: Schema.Struct({
    id: Schema.String,
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
    rationale: "Rest floors, weekly hours, overtime budgets, and consecutive-day caps must stay inside sane operational bounds.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/rostering.ts" },
    subject: "hr.PreferenceProfile",
    scope: { root: { kind: "entity", id: "hr.PreferenceProfile" }, objectTypes: ["hr.PreferenceProfile"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.min_rest_hours >= 0 and self.max_weekly_hours > 0 and self.max_consecutive_days >= 1 and self.max_consecutive_days <= 6 and self.weekly_overtime_budget >= 0",
    },
  },
})

export const PreferenceProfileValidExample = Norn.example("balanced Cairo preference profile holds", {
  of: PreferenceProfileValidBounds,
  given: {
    id: "prof_cairo",
    label: "Cairo Retail Balanced",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 11,
    max_weekly_hours: 48,
    max_consecutive_days: 6,
    weekend_fairness_priority: "high",
    staff_preference_priority: "medium",
    avoid_split_shifts: true,
  },
  expect: "holds",
})

export const PreferenceProfileSevenConsecutiveViolated = Norn.example("seven consecutive working days is refused", {
  of: PreferenceProfileValidBounds,
  given: {
    id: "prof_bad",
    label: "Illegal 7-day streak",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 11,
    max_weekly_hours: 48,
    max_consecutive_days: 7,
    weekend_fairness_priority: "low",
    staff_preference_priority: "low",
    avoid_split_shifts: false,
  },
  expect: "violated",
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
    response: { kind: "predicate", expression: "self.required_count >= 1" },
  },
})

export const ShiftRequirementValidExample = Norn.example("two cashiers required holds", {
  of: ShiftRequirementPositiveCoverage,
  given: { id: "sr_1", role_code: "cashier", starts_at: "2026-11-28T14:00:00Z", ends_at: "2026-11-28T22:00:00Z", required_count: 2 },
  expect: "holds",
})

export const ShiftRequirementZeroViolated = Norn.example("zero headcount requirement is refused", {
  of: ShiftRequirementPositiveCoverage,
  given: { id: "sr_0", role_code: "cashier", starts_at: "2026-11-28T14:00:00Z", ends_at: "2026-11-28T22:00:00Z", required_count: 0 },
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
    condition: 'self.status == "published"',
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.solver_receipt_digest != null" },
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
    { name: "roster", type: "hr.Roster", key: { property: { from: "input", property: "rosterId" } } },
  ],
  preconditions: [
    Norn.unsafeZen<boolean>("slice.roster.solver_receipt_digest != null"),
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

export const RecordAttendance = Norn.action({
  id: "hr.attendance.record",
  title: "Record immutable attendance event",
  version: "0.1.0",
  description: "Post a create-only AttendanceEvent fulfilling a ShiftCommitment (never mutates posted history; HR-AT-01).",
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
    clock_in_at: Schema.String,
    clock_out_at: Schema.String,
    providerId: Schema.String,
    receiverId: Schema.String,
    resourceId: Schema.String,
    shiftCommitmentId: Schema.String,
  }),
  output: Schema.Struct({ id: Schema.String }),
  createLinks: [
    { link: "provider", targetType: "hr.Person", targetIdField: "providerId" },
    { link: "receiver", targetType: "org.OrgUnit", targetIdField: "receiverId" },
    { link: "resource", targetType: "hr.LeaveBalance", targetIdField: "resourceId" },
    { link: "fulfils", targetType: "hr.ShiftCommitment", targetIdField: "shiftCommitmentId" },
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
  description: "Approve a pending attendance regularization request under manager authority.",
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
