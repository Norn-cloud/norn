/**
 * HR Domain Pack — Payroll, Compensation, Expenses & Employee Advances (#1593).
 *
 * Guided by `registry/packs/frappe-hr/ORACLE-CHECKLIST.md` (`HR-PR-01..08`, `HR-EX-01..05`).
 * Strictly jurisdiction-agnostic: statutory tax brackets, social insurance rates,
 * and country filing schedules live in `loc-*` overlay packs.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Claim, Commitment, Resource } from "../../core/src/rea.ts"
import { TimeInterval } from "../../core/src/time.ts"
import { ISO_4217 } from "../canon/records.ts"
import { OrgUnit } from "../org/org.ts"
import { Employment, Person } from "./people_leave.ts"

// ---------------------------------------------------------------- Monetary Payroll Resource

export const PayrollFund = Norn.type({
  name: "hr.PayrollFund",
  role: "thing",
  title: "Payroll fund",
  version: "0.1.0",
  description: "Conserved employer monetary payroll disbursement resource in a declared currency (implements core.Resource; promised by SalarySlip).",
  implements: [Resource],
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "owner", description: "Employer organizational unit owning this payroll disbursement fund.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", description: "Optional treasury or payroll custodian unit administering the fund.", target: OrgUnit, cardinality: { to: "one", from: "many" } },
  ],
})

const PayrollFundSubject = Schema.Struct({
  quantity: Norn.decimal(6),
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
})

export const PayrollFundNonNegative = Norn.law({
  id: "hr.PayrollFund.non_negative.law",
  title: "Payroll fund is a non-negative monetary resource denominated in currency without UCUM unit",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.PayrollFund.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.PayrollFund.non_negative",
    owner: "hr",
    severity: "error",
    rationale: "A payroll fund is a monetary core.Resource (unit == null, currency != null) with a non-negative available balance (HR-PR-06).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.PayrollFund",
    scope: { root: { kind: "entity", id: "hr.PayrollFund" }, objectTypes: ["hr.PayrollFund"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        PayrollFundSubject,
        (self) => self.quantity >= 0 && self.unit === null && self.currency !== null,
      ),
    },
  },
})

export const PayrollFundValidHolds = Norn.example("funded USD payroll fund holds", {
  of: PayrollFundNonNegative,
  given: { id: "pf_usd", resource_code: "PAYROLL-USD", quantity: 100000, unit: null, currency: "USD", valuation_method: null },
  expect: "holds",
})

export const PayrollFundNegativeViolated = Norn.example("negative payroll fund balance is refused", {
  of: PayrollFundNonNegative,
  given: { id: "pf_neg", resource_code: "PAYROLL-USD", quantity: -500, unit: null, currency: "USD", valuation_method: null },
  expect: "violated",
})

export const PayrollFundWithUnitViolated = Norn.example("payroll fund carrying UCUM unit instead of pure currency is refused", {
  of: PayrollFundNonNegative,
  given: { id: "pf_unit", resource_code: "PAYROLL-BAD", quantity: 1000, unit: "h", currency: "USD", valuation_method: null },
  expect: "violated",
})

// ---------------------------------------------------------------- Salary Structures & Assignments

const SalaryStructureStatus = Schema.Literals(["active", "archived"])

export const SalaryStructure = Norn.type({
  name: "hr.SalaryStructure",
  role: "thing",
  title: "Salary structure",
  version: "0.1.0",
  description: "Compensation structure template declaring pay currency and frequency.",
  properties: Schema.Struct({
    id: Schema.String,
    structure_code: Norn.ref("core.code"),
    label: Norn.ref("core.display_name"),
    currency: Schema.Literals(ISO_4217),
    pay_frequency: Schema.Literals(["monthly", "biweekly", "weekly"]),
    status: SalaryStructureStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "active",
  },
  links: [
    { id: "unit", description: "Optional organizational unit owning this compensation template.", target: OrgUnit, cardinality: { to: "one", from: "many" } },
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

export const SalaryStructureAssignment = Norn.type({
  name: "hr.SalaryStructureAssignment",
  role: "thing",
  title: "Salary structure assignment",
  version: "0.1.0",
  description: "Effective-dated assignment binding an employee and employment contract to a SalaryStructure and base pay (HR-PR-07).",
  properties: Schema.Struct({
    id: Schema.String,
    effective_from: Norn.date(),
    currency: Schema.Literals(ISO_4217),
    base_pay: Norn.ref("core.money_amount", { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
  }),
  propertyMarkings: {
    base_pay: ["pii.compensation"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "person", description: "Employee assigned to this salary structure.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "employment", description: "Employment contract governed by this assignment.", target: Employment, cardinality: { to: "one", from: "many", required: true } },
    { id: "structure", description: "Compensation structure template applied.", target: SalaryStructure, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const BasePaySubject = Schema.Struct({
  base_pay: Norn.decimal(6),
})

export const SalaryStructureAssignmentNonNegativeBasePay = Norn.law({
  id: "hr.SalaryStructureAssignment.non_negative_base_pay.law",
  title: "Salary structure assignment base pay is non-negative",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.SalaryStructureAssignment.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.SalaryStructureAssignment.non_negative_base_pay",
    owner: "hr",
    severity: "error",
    rationale: "Assigned base compensation must be non-negative in exact decimal currency units (HR-PR-01, HR-PR-07).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.SalaryStructureAssignment",
    scope: { root: { kind: "entity", id: "hr.SalaryStructureAssignment" }, objectTypes: ["hr.SalaryStructureAssignment"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(BasePaySubject, (self) => self.base_pay >= 0),
    },
  },
})

export const SalaryAssignmentValidHolds = Norn.example("positive base pay assignment holds", {
  of: SalaryStructureAssignmentNonNegativeBasePay,
  given: { id: "ssa_1", effective_from: "2026-01-01", currency: "USD", base_pay: 4500 },
  expect: "holds",
})

export const SalaryAssignmentNegativeViolated = Norn.example("negative base pay assignment is refused", {
  of: SalaryStructureAssignmentNonNegativeBasePay,
  given: { id: "ssa_bad", effective_from: "2026-01-01", currency: "USD", base_pay: -100 },
  expect: "violated",
})

// ---------------------------------------------------------------- Payroll Run & Salary Slip

const PayrollRunStatus = Schema.Literals(["draft", "calculated", "approved", "posted"])

export const PayrollRun = Norn.type({
  name: "hr.PayrollRun",
  role: "happening",
  title: "Payroll run",
  version: "0.1.0",
  description: "Period payroll batch over a proper half-open UTC [starts_at, ends_at) interval with exact decimal gross/deduction/net conservation and separation of duties (implements core.TimeInterval; HR-PR-01..06).",
  implements: [TimeInterval],
  properties: Schema.Struct({
    id: Schema.String,
    period_code: Norn.ref("core.code"),
    starts_at: Norn.ref("core.time.instant"),
    ends_at: Norn.ref("core.time.instant"),
    timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
    currency: Schema.Literals(ISO_4217),
    total_gross: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    total_deductions: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    total_net: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    prepared_by_user_id: Schema.String,
    status: PayrollRunStatus,
  }),
  propertyMarkings: {
    total_gross: ["pii.compensation"],
    total_deductions: ["pii.compensation"],
    total_net: ["pii.compensation"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "unit", description: "Organizational unit executing this payroll run.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "fund", description: "Employer monetary payroll fund disbursing this payroll run.", target: PayrollFund, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["posted"],
      immutable: ["posted"],
      transitions: [
        { id: "calculate", from: ["draft"], to: "calculated" },
        { id: "reopen", from: ["calculated"], to: "draft" },
        { id: "approve", from: ["calculated"], to: "approved" },
        { id: "post", from: ["approved"], to: "posted" },
      ],
    },
  ],
})

const PayrollRunConservationSubject = Schema.Struct({
  total_gross: Norn.decimal(6),
  total_deductions: Norn.decimal(6),
  total_net: Norn.decimal(6),
})

export const PayrollRunConservation = Norn.law({
  id: "hr.PayrollRun.conservation.law",
  title: "Payroll run totals conserve net = gross - deductions without negative net",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.PayrollRun.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.PayrollRun.conservation",
    owner: "hr",
    severity: "error",
    rationale: "Payroll run totals must satisfy exact decimal conservation (total_net == total_gross - total_deductions) with non-negative components (HR-PR-01, HR-PR-04).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.PayrollRun",
    scope: { root: { kind: "entity", id: "hr.PayrollRun" }, objectTypes: ["hr.PayrollRun"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        PayrollRunConservationSubject,
        (self) =>
          self.total_gross >= 0 &&
          self.total_deductions >= 0 &&
          self.total_net === self.total_gross - self.total_deductions &&
          self.total_net >= 0,
      ),
    },
  },
})

export const PayrollRunBalancedHolds = Norn.example("balanced payroll run totals hold", {
  of: PayrollRunConservation,
  given: {
    id: "pr_2026_11",
    period_code: "2026-11",
    starts_at: "2026-11-01T00:00:00Z",
    ends_at: "2026-12-01T00:00:00Z",
    timezone: "UTC",
    currency: "USD",
    total_gross: 50000,
    total_deductions: 8500,
    total_net: 41500,
    prepared_by_user_id: "usr_payroll_1",
    status: "calculated",
  },
  expect: "holds",
})

export const PayrollRunMismatchViolated = Norn.example("payroll run with mismatched net pay is refused", {
  of: PayrollRunConservation,
  given: {
    id: "pr_bad",
    period_code: "2026-11",
    starts_at: "2026-11-01T00:00:00Z",
    ends_at: "2026-12-01T00:00:00Z",
    timezone: "UTC",
    currency: "USD",
    total_gross: 50000,
    total_deductions: 8500,
    total_net: 42000,
    prepared_by_user_id: "usr_payroll_1",
    status: "calculated",
  },
  expect: "violated",
})

export const SalarySlip = Norn.type({
  name: "hr.SalarySlip",
  role: "happening",
  title: "Salary slip",
  version: "0.1.0",
  description: "Individual employee monetary wage commitment promising disbursement from hr.PayrollFund under a PayrollRun (implements core.Commitment; exact decimal conservation per HR-PR-01..05).",
  implements: [Commitment],
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    gross_pay: Norn.ref("core.money_amount"),
    total_deductions: Norn.ref("core.money_amount"),
    net_pay: Norn.ref("core.money_amount"),
  }),
  propertyMarkings: {
    gross_pay: ["pii.compensation"],
    total_deductions: ["pii.compensation"],
    net_pay: ["pii.compensation"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "requested",
  },
  links: [
    { id: "provider", description: "Employer unit promising monetary compensation.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Employee receiving monetary compensation.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Employer monetary payroll fund promised by this salary slip.", target: PayrollFund, cardinality: { to: "one", from: "many", required: true } },
    { id: "payroll_run", description: "Parent payroll run containing this salary slip.", target: PayrollRun, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["requested"],
      terminal: ["fulfilled", "cancelled", "breached"],
      immutable: ["fulfilled"],
      transitions: [
        { id: "promise", from: ["requested"], to: "promised" },
        { id: "fulfil", from: ["promised"], to: "fulfilled" },
        { id: "cancel", from: ["requested", "promised"], to: "cancelled" },
        { id: "breach", from: ["promised"], to: "breached" },
      ],
    },
  ],
})

const SalarySlipConservationSubject = Schema.Struct({
  quantity: Norn.decimal(6),
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
  gross_pay: Norn.decimal(6),
  total_deductions: Norn.decimal(6),
  net_pay: Norn.decimal(6),
})

export const SalarySlipConservation = Norn.law({
  id: "hr.SalarySlip.conservation.law",
  title: "Salary slip conserves quantity = net_pay = gross_pay - total_deductions in pure currency",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.SalarySlip.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.SalarySlip.conservation",
    owner: "hr",
    severity: "error",
    rationale: "Every salary slip is a monetary commitment (unit == null, currency != null) conserving exact decimal net pay (net_pay == gross_pay - total_deductions >= 0) and matching its REA Commitment quantity (quantity == net_pay; HR-PR-01, HR-PR-04).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.SalarySlip",
    scope: { root: { kind: "entity", id: "hr.SalarySlip" }, objectTypes: ["hr.SalarySlip"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        SalarySlipConservationSubject,
        (self) =>
          self.gross_pay >= 0 &&
          self.total_deductions >= 0 &&
          self.net_pay === self.gross_pay - self.total_deductions &&
          self.net_pay >= 0 &&
          self.quantity === self.net_pay &&
          self.unit === null &&
          self.currency !== null,
      ),
    },
  },
})

export const SalarySlipBalancedHolds = Norn.example("balanced salary slip holds", {
  of: SalarySlipConservation,
  given: {
    id: "slip_1",
    status: "promised",
    due_at: "2026-11-30T17:00:00Z",
    quantity: 4150,
    unit: null,
    currency: "USD",
    gross_pay: 5000,
    total_deductions: 850,
    net_pay: 4150,
  },
  expect: "holds",
})

export const SalarySlipNegativeNetViolated = Norn.example("salary slip where deductions exceed gross is refused", {
  of: SalarySlipConservation,
  given: {
    id: "slip_bad",
    status: "requested",
    due_at: "2026-11-30T17:00:00Z",
    quantity: 1000,
    unit: null,
    currency: "USD",
    gross_pay: 1000,
    total_deductions: 1200,
    net_pay: -200,
  },
  expect: "violated",
})

export const SalarySlipQuantityMismatchViolated = Norn.example("salary slip whose REA commitment quantity diverges from net_pay is refused", {
  of: SalarySlipConservation,
  given: {
    id: "slip_qty_bad",
    status: "promised",
    due_at: "2026-11-30T17:00:00Z",
    quantity: 9000,
    unit: null,
    currency: "USD",
    gross_pay: 5000,
    total_deductions: 850,
    net_pay: 4150,
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Employee Advances & Expense Claims

const EmployeeAdvanceStatus = Schema.Literals(["draft", "approved", "disbursed", "settled", "cancelled"])

export const EmployeeAdvance = Norn.type({
  name: "hr.EmployeeAdvance",
  role: "thing",
  title: "Employee advance",
  version: "0.1.0",
  description: "Cash advance issued to an employee ahead of business expenses (implements core.Claim; HR-EX-03).",
  implements: [Claim],
  properties: Schema.Struct({
    id: Schema.String,
    amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    currency: Norn.ref("core.code"),
    due_date: Norn.ref("iso8601.date"),
    requester_user_id: Schema.String,
    purpose: Schema.String,
    status: EmployeeAdvanceStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "creditor", description: "Employer unit holding the advance settlement claim.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "debtor", description: "Employee accountable for settling or returning the advance.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "arises_from", description: "Optional commitment giving rise to this advance.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["settled", "cancelled"],
      immutable: ["settled", "cancelled"],
      transitions: [
        { id: "approve", from: ["draft"], to: "approved" },
        { id: "disburse", from: ["approved"], to: "disbursed" },
        { id: "settle", from: ["disbursed"], to: "settled" },
        { id: "cancel", from: ["draft", "approved"], to: "cancelled" },
      ],
    },
  ],
})

const AdvanceAmountSubject = Schema.Struct({
  amount: Norn.decimal(6),
})

export const EmployeeAdvancePositiveAmount = Norn.law({
  id: "hr.EmployeeAdvance.positive_amount.law",
  title: "Employee advance amount is strictly positive",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.EmployeeAdvance.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.EmployeeAdvance.positive_amount",
    owner: "hr",
    severity: "error",
    rationale: "An employee cash advance must request a strictly positive amount (HR-EX-03).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.EmployeeAdvance",
    scope: { root: { kind: "entity", id: "hr.EmployeeAdvance" }, objectTypes: ["hr.EmployeeAdvance"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(AdvanceAmountSubject, (self) => self.amount > 0),
    },
  },
})

export const EmployeeAdvanceValidHolds = Norn.example("positive employee advance holds", {
  of: EmployeeAdvancePositiveAmount,
  given: { id: "adv_1", amount: 500, currency: "USD", due_date: "2026-12-15", requester_user_id: "usr_emp_1", purpose: "Client site travel", status: "approved" },
  expect: "holds",
})

export const EmployeeAdvanceZeroViolated = Norn.example("zero employee advance is refused", {
  of: EmployeeAdvancePositiveAmount,
  given: { id: "adv_0", amount: 0, currency: "USD", due_date: "2026-12-15", requester_user_id: "usr_emp_1", purpose: "Empty advance", status: "draft" },
  expect: "violated",
})

const ExpenseClaimStatus = Schema.Literals(["draft", "submitted", "approved", "paid", "rejected"])

export const ExpenseClaim = Norn.type({
  name: "hr.ExpenseClaim",
  role: "thing",
  title: "Expense claim",
  version: "0.1.0",
  description: "Employee expense reimbursement claim with strict claimed/sanctioned bounds, canonical core.Claim.amount parity, and independent approval (implements core.Claim; HR-EX-01, HR-EX-02).",
  implements: [Claim],
  properties: Schema.Struct({
    id: Schema.String,
    amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    currency: Norn.ref("core.code"),
    due_date: Norn.ref("iso8601.date"),
    requester_user_id: Schema.String,
    claimed_amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    sanctioned_amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    description: Schema.String,
    status: ExpenseClaimStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "creditor", description: "Employee entitled to reimbursement.", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "debtor", description: "Employer unit obliged to reimburse sanctioned expenses.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "arises_from", description: "Optional commitment giving rise to this expense claim.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
    { id: "advance", description: "Optional employee advance partially or fully offsetting this claim.", target: EmployeeAdvance, cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["paid", "rejected"],
      immutable: ["approved", "paid"],
      transitions: [
        { id: "submit", from: ["draft"], to: "submitted" },
        { id: "approve", from: ["submitted"], to: "approved" },
        { id: "mark_paid", from: ["approved"], to: "paid" },
        { id: "reject", from: ["submitted"], to: "rejected" },
      ],
    },
  ],
})

const ExpenseClaimConservationSubject = Schema.Struct({
  claimed_amount: Norn.decimal(6),
  sanctioned_amount: Norn.decimal(6),
})

export const ExpenseClaimConservation = Norn.law({
  id: "hr.ExpenseClaim.conservation.law",
  title: "Expense claim sanctioned amount stays within [0, claimed_amount] with positive claim",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.ExpenseClaim.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.ExpenseClaim.conservation",
    owner: "hr",
    severity: "error",
    rationale: "An approver cannot sanction more than the claimed_amount or a negative amount, and claimed_amount must be positive (HR-EX-01).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.ExpenseClaim",
    scope: { root: { kind: "entity", id: "hr.ExpenseClaim" }, objectTypes: ["hr.ExpenseClaim"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        ExpenseClaimConservationSubject,
        (self) => self.claimed_amount > 0 && self.sanctioned_amount >= 0 && self.sanctioned_amount <= self.claimed_amount,
      ),
    },
  },
})

export const ExpenseClaimValidHolds = Norn.example("expense claim with sanctioned <= claimed holds", {
  of: ExpenseClaimConservation,
  given: {
    id: "exp_1",
    amount: 220,
    currency: "USD",
    due_date: "2026-12-20",
    requester_user_id: "usr_emp_1",
    claimed_amount: 250,
    sanctioned_amount: 220,
    description: "Airport transit and lodging",
    status: "approved",
  },
  expect: "holds",
})

export const ExpenseClaimOverSanctionedViolated = Norn.example("expense claim with sanctioned_amount > claimed_amount is refused", {
  of: ExpenseClaimConservation,
  given: {
    id: "exp_over",
    amount: 300,
    currency: "USD",
    due_date: "2026-12-20",
    requester_user_id: "usr_emp_1",
    claimed_amount: 250,
    sanctioned_amount: 300,
    description: "Over-sanctioned claim",
    status: "submitted",
  },
  expect: "violated",
})

const ExpenseClaimAmountSubject = Schema.Struct({
  amount: Norn.decimal(6),
  sanctioned_amount: Norn.decimal(6),
  status: ExpenseClaimStatus,
})

export const ExpenseClaimAmountConservation = Norn.law({
  id: "hr.ExpenseClaim.amount_conservation.law",
  title: "Approved or paid expense claim core.Claim.amount equals positive sanctioned_amount",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.ExpenseClaim.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "hr.ExpenseClaim.amount_conservation",
    owner: "hr",
    severity: "error",
    rationale: "Once an ExpenseClaim is approved or paid, its canonical core.Claim.amount must exactly match its positive sanctioned_amount so settlement cannot diverge from approval (HR-EX-01, HR-EX-05).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr/payroll_expenses.ts" },
    subject: "hr.ExpenseClaim",
    scope: { root: { kind: "entity", id: "hr.ExpenseClaim" }, objectTypes: ["hr.ExpenseClaim"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        ExpenseClaimAmountSubject,
        (self) =>
          (self.status !== "approved" && self.status !== "paid") ||
          (self.amount === self.sanctioned_amount && self.sanctioned_amount > 0),
      ),
    },
  },
})

export const ExpenseClaimApprovedMatchingAmountHolds = Norn.example("approved expense claim with amount == sanctioned_amount holds", {
  of: ExpenseClaimAmountConservation,
  given: {
    id: "exp_amt_ok",
    amount: 220,
    currency: "USD",
    due_date: "2026-12-20",
    requester_user_id: "usr_emp_1",
    claimed_amount: 250,
    sanctioned_amount: 220,
    description: "Matched approved claim",
    status: "approved",
  },
  expect: "holds",
})

export const ExpenseClaimDraftZeroSanctionedHolds = Norn.example("draft expense claim before sanctioning holds", {
  of: ExpenseClaimAmountConservation,
  given: {
    id: "exp_draft",
    amount: 250,
    currency: "USD",
    due_date: "2026-12-20",
    requester_user_id: "usr_emp_1",
    claimed_amount: 250,
    sanctioned_amount: 0,
    description: "Draft claim awaiting review",
    status: "draft",
  },
  expect: "holds",
})

export const ExpenseClaimApprovedMismatchAmountViolated = Norn.example("approved expense claim where core.Claim.amount diverges from sanctioned_amount is refused", {
  of: ExpenseClaimAmountConservation,
  given: {
    id: "exp_amt_bad",
    amount: 250,
    currency: "USD",
    due_date: "2026-12-20",
    requester_user_id: "usr_emp_1",
    claimed_amount: 250,
    sanctioned_amount: 220,
    description: "Divergent claim amount",
    status: "approved",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Actions

const PayrollRunInput = Schema.Struct({
  payrollRunId: Schema.String,
})

const PayrollRunOutput = Schema.Struct({
  payrollRunId: Schema.String,
  status: Schema.String,
})

const PayrollRunSlice = Schema.Struct({
  slice: Schema.Struct({
    run: Schema.Struct({
      total_gross: Norn.decimal(6),
      total_deductions: Norn.decimal(6),
      total_net: Norn.decimal(6),
      status: PayrollRunStatus,
    }),
  }),
})

export const CalculatePayroll = Norn.action({
  id: "hr.payroll.calculate",
  title: "Calculate payroll run",
  version: "0.1.0",
  description: "Move a draft payroll run into calculated status after computing exact salary slips.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "run", type: PayrollRun, key: { property: { from: "input", property: "payrollRunId" } } },
  ],
  preconditions: [
    Norn.expression(
      PayrollRunSlice,
      (ctx) => ctx.slice.run.status === "draft",
      { root: "context" },
    ),
  ],
  input: PayrollRunInput,
  output: PayrollRunOutput,
  writes: [{ type: PayrollRun, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PayrollRun,
    objectIdField: "payrollRunId",
    lifecycle: "status",
    transition: "calculate",
  },
})

export const ApprovePayroll = Norn.action({
  id: "hr.payroll.approve",
  title: "Approve payroll run",
  version: "0.1.0",
  description: "Approve a calculated payroll run under independent authority (preparer cannot approve own run).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "run", type: PayrollRun, key: { property: { from: "input", property: "payrollRunId" } } },
  ],
  preconditions: [
    Norn.expression(
      PayrollRunSlice,
      (ctx) =>
        ctx.slice.run.status === "calculated" &&
        ctx.slice.run.total_net === ctx.slice.run.total_gross - ctx.slice.run.total_deductions &&
        ctx.slice.run.total_net >= 0,
      { root: "context" },
    ),
  ],
  input: PayrollRunInput,
  output: PayrollRunOutput,
  writes: [{ type: PayrollRun, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PayrollRun,
    objectIdField: "payrollRunId",
    lifecycle: "status",
    transition: "approve",
  },
})

const PayrollPostFundSlice = Schema.Struct({
  slice: Schema.Struct({
    run: Schema.Struct({
      currency: Schema.Literals(ISO_4217),
    }),
    run_fund: Schema.Array(Schema.Struct({
      unit: Schema.NullOr(Schema.String),
      currency: Schema.NullOr(Schema.String),
    })),
  }),
})

/**
 * Single-write engine boundary note: this action locks `hr.PayrollRun` as posted
 * after verifying positive gross, exact net conservation, and linked monetary
 * `hr.PayrollFund` existence & currency agreement via `SliceKey::Link`; atomic
 * multi-object `hr.SalarySlip` aggregation, `hr.PayrollFund` debit, and ledger
 * accrual posting in one commit wait on engine `#1586` (`synchronousRule`
 * multi-object writer wiring).
 */
export const PostPayroll = Norn.action({
  id: "hr.payroll.post",
  title: "Post payroll run",
  version: "0.1.0",
  description: "Permanently post a non-empty approved payroll run after verifying its linked PayrollFund exists in matching currency with null UCUM unit (HR-PR-05, HR-PR-06).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "run", type: PayrollRun, key: { property: { from: "input", property: "payrollRunId" } } },
    { name: "run_fund", type: PayrollFund, key: { link: { from: "run", link: "fund" } } },
  ],
  preconditions: [
    Norn.expression(
      PayrollRunSlice,
      (ctx) =>
        ctx.slice.run.status === "approved" &&
        ctx.slice.run.total_gross > 0 &&
        ctx.slice.run.total_net === ctx.slice.run.total_gross - ctx.slice.run.total_deductions,
      { root: "context" },
    ),
    Norn.expression(
      PayrollPostFundSlice,
      (ctx) =>
        ctx.slice.run_fund.length === 1 &&
        ctx.slice.run_fund.every((f) => f.unit === null) &&
        ctx.slice.run_fund.map((f) => f.currency ?? "").includes(ctx.slice.run.currency),
      { root: "context" },
    ),
  ],
  input: PayrollRunInput,
  output: PayrollRunOutput,
  writes: [{ type: PayrollRun, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PayrollRun,
    objectIdField: "payrollRunId",
    lifecycle: "status",
    transition: "post",
  },
})

const ExpenseClaimInput = Schema.Struct({
  expenseClaimId: Schema.String,
})

const ExpenseClaimOutput = Schema.Struct({
  expenseClaimId: Schema.String,
  status: Schema.String,
})

const ExpenseClaimSlice = Schema.Struct({
  slice: Schema.Struct({
    claim: Schema.Struct({
      amount: Norn.decimal(6),
      claimed_amount: Norn.decimal(6),
      sanctioned_amount: Norn.decimal(6),
      status: ExpenseClaimStatus,
    }),
  }),
})

export const SubmitExpenseClaim = Norn.action({
  id: "hr.expense_claim.submit",
  title: "Submit expense claim",
  version: "0.1.0",
  description: "Submit a draft employee expense claim for independent manager approval.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "claim", type: ExpenseClaim, key: { property: { from: "input", property: "expenseClaimId" } } },
  ],
  preconditions: [
    Norn.expression(
      ExpenseClaimSlice,
      (ctx) => ctx.slice.claim.claimed_amount > 0,
      { root: "context" },
    ),
  ],
  input: ExpenseClaimInput,
  output: ExpenseClaimOutput,
  writes: [{ type: ExpenseClaim, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExpenseClaim,
    objectIdField: "expenseClaimId",
    lifecycle: "status",
    transition: "submit",
  },
})

export const ApproveExpenseClaim = Norn.action({
  id: "hr.expense_claim.approve",
  title: "Approve expense claim",
  version: "0.1.0",
  description: "Approve a submitted expense claim under independent manager authority (requester cannot approve own claim; HR-EX-01, HR-EX-02).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "claim", type: ExpenseClaim, key: { property: { from: "input", property: "expenseClaimId" } } },
  ],
  preconditions: [
    Norn.expression(
      ExpenseClaimSlice,
      (ctx) =>
        ctx.slice.claim.status === "submitted" &&
        ctx.slice.claim.sanctioned_amount > 0 &&
        ctx.slice.claim.sanctioned_amount <= ctx.slice.claim.claimed_amount &&
        ctx.slice.claim.amount === ctx.slice.claim.sanctioned_amount,
      { root: "context" },
    ),
  ],
  input: ExpenseClaimInput,
  output: ExpenseClaimOutput,
  writes: [{ type: ExpenseClaim, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExpenseClaim,
    objectIdField: "expenseClaimId",
    lifecycle: "status",
    transition: "approve",
  },
})

export const RejectExpenseClaim = Norn.action({
  id: "hr.expense_claim.reject",
  title: "Reject expense claim",
  version: "0.1.0",
  description: "Reject a submitted employee expense claim under independent manager authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  input: ExpenseClaimInput,
  output: ExpenseClaimOutput,
  writes: [{ type: ExpenseClaim, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExpenseClaim,
    objectIdField: "expenseClaimId",
    lifecycle: "status",
    transition: "reject",
  },
})

const AdvanceInput = Schema.Struct({
  advanceId: Schema.String,
})

const AdvanceOutput = Schema.Struct({
  advanceId: Schema.String,
  status: Schema.String,
})

const AdvanceSlice = Schema.Struct({
  slice: Schema.Struct({
    advance: Schema.Struct({
      amount: Norn.decimal(6),
      status: EmployeeAdvanceStatus,
    }),
  }),
})

export const ApproveEmployeeAdvance = Norn.action({
  id: "hr.advance.approve",
  title: "Approve employee advance",
  version: "0.1.0",
  description: "Approve a draft employee advance under independent manager or HR authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "advance", type: EmployeeAdvance, key: { property: { from: "input", property: "advanceId" } } },
  ],
  preconditions: [
    Norn.expression(
      AdvanceSlice,
      (ctx) => ctx.slice.advance.status === "draft" && ctx.slice.advance.amount > 0,
      { root: "context" },
    ),
  ],
  input: AdvanceInput,
  output: AdvanceOutput,
  writes: [{ type: EmployeeAdvance, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: EmployeeAdvance,
    objectIdField: "advanceId",
    lifecycle: "status",
    transition: "approve",
  },
})

export const DisburseEmployeeAdvance = Norn.action({
  id: "hr.advance.disburse",
  title: "Disburse employee advance",
  version: "0.1.0",
  description: "Record disbursement of an approved employee cash advance (HR-EX-03).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr.access.policy", version: "0.1.0" },
  slice: [
    { name: "advance", type: EmployeeAdvance, key: { property: { from: "input", property: "advanceId" } } },
  ],
  preconditions: [
    Norn.expression(
      AdvanceSlice,
      (ctx) => ctx.slice.advance.status === "approved" && ctx.slice.advance.amount > 0,
      { root: "context" },
    ),
  ],
  input: AdvanceInput,
  output: AdvanceOutput,
  writes: [{ type: EmployeeAdvance, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: EmployeeAdvance,
    objectIdField: "advanceId",
    lifecycle: "status",
    transition: "disburse",
  },
})
