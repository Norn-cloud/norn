/**
 * Egypt localization pack (`loc-eg`): Egypt Labor Law No. 14 of 2025
 * (effective 2025-09-01, repealing Law No. 12 of 2003), conjunctive `Norn.law`
 * overlays directly targeting `hr.*` entities, and governed effective-dated
 * NOSI (Law 148/2019) & ETA VAT (Law 67/2016) `Norn.setting` parameters (#1593).
 *
 * Composes conjunctively with `registry/packs/hr/` (`hr ∧ loc-eg ∧ company-overlay`)
 * so `hr` itself remains 100% jurisdiction-agnostic.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { OrgUnit } from "../org/org.ts"

// ---------------------------------------------------------------- Egypt Statutory Labor Profile

export const EgyptLaborProfile = Norn.type({
  name: "loc_eg.LaborProfile",
  role: "thing",
  title: "Egypt labor-law profile",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "Governed Egyptian labor parameters under Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-124): Africa/Cairo timezone, EGP currency, statutory weekly/daily hour caps, rest gap, and annual leave tiers (15d / 21d / 30d / 45d).",
  properties: Schema.Struct({
    id: Schema.String,
    effective_from: Norn.date(),
    iana_timezone: Schema.Literals(["Africa/Cairo"]),
    currency: Schema.Literals(["EGP"]),
    max_regular_weekly_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    max_daily_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    min_rest_between_shifts_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    first_year_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    standard_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    senior_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    disability_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    seniority_threshold_years: Schema.Int,
    weekend_pattern: Schema.Literals(["friday_saturday", "friday_only"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  links: [
    { id: "unit", description: "Egyptian branch or organizational unit governed by this statutory labor profile.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const EgyptLaborProfileSubject = Schema.Struct({
  max_regular_weekly_hours: Norn.decimal(2),
  max_daily_hours: Norn.decimal(2),
  min_rest_between_shifts_hours: Norn.decimal(2),
  first_year_annual_leave_days: Norn.decimal(2),
  standard_annual_leave_days: Norn.decimal(2),
  senior_annual_leave_days: Norn.decimal(2),
  disability_annual_leave_days: Norn.decimal(2),
})

export const EgyptStatutoryFloors = Norn.law({
  id: "loc_eg.LaborProfile.statutory_floors.law",
  title: "Egyptian statutory labor-law floors and hour caps (Law No. 14 of 2025)",
  version: "0.1.0",
  dependencies: [{ artifactId: "loc_eg.LaborProfile.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.LaborProfile.statutory_floors",
    owner: "loc_eg",
    severity: "error",
    rationale: "Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-124) caps regular weekly hours at 48h and daily workplace presence at 10h, enforces >= 11h rest between shifts, and sets annual leave floors of 15d (year 1), 21d (year 2+), 30d (10+ years or age 50+), and 45d (persons with disabilities/short stature).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "loc_eg.LaborProfile",
    scope: { root: { kind: "entity", id: "loc_eg.LaborProfile" }, objectTypes: ["loc_eg.LaborProfile"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        EgyptLaborProfileSubject,
        (self) =>
          self.max_regular_weekly_hours <= 48 &&
          self.max_daily_hours <= 10 &&
          self.min_rest_between_shifts_hours >= 11 &&
          self.first_year_annual_leave_days >= 15 &&
          self.standard_annual_leave_days >= 21 &&
          self.senior_annual_leave_days >= 30 &&
          self.disability_annual_leave_days >= 45,
      ),
    },
  },
})

export const EgyptBaselineHolds = Norn.example("Law 14/2025 48h / 10h / 11h / 15d-21d-30d-45d profile holds", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_2025",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    max_regular_weekly_hours: 48,
    max_daily_hours: 10,
    min_rest_between_shifts_hours: 11,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "holds",
})

export const EgyptShortRestViolated = Norn.example("eight-hour rest between shifts violates Law 14/2025 floor", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_bad_rest",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    max_regular_weekly_hours: 48,
    max_daily_hours: 10,
    min_rest_between_shifts_hours: 8,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

export const EgyptSubFloorFirstYearLeaveViolated = Norn.example("fourteen-day first-year annual leave violates Law 14/2025 Art. 124", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_bad_y1",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    max_regular_weekly_hours: 48,
    max_daily_hours: 10,
    min_rest_between_shifts_hours: 11,
    first_year_annual_leave_days: 14,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

export const EgyptSubFloorDisabilityLeaveViolated = Norn.example("thirty-day disability leave violates Law 14/2025 Art. 124 45d floor", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_bad_dis",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    max_regular_weekly_hours: 48,
    max_daily_hours: 10,
    min_rest_between_shifts_hours: 11,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 30,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Conjunctive Overlays on hr.* Entities

const HrPreferenceProfileOverlaySubject = Schema.Struct({
  min_rest_hours: Norn.decimal(2),
  max_weekly_hours: Norn.decimal(2),
  max_consecutive_days: Schema.Int,
})

export const EgyptPreferenceProfileLaborLaw = Norn.law({
  id: "loc_eg.hr.PreferenceProfile.labor_law_14_2025.law",
  title: "Egypt Labor Law No. 14 of 2025 rostering caps on hr.PreferenceProfile",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.PreferenceProfile.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.hr.PreferenceProfile.labor_law_14_2025",
    owner: "loc_eg",
    severity: "error",
    rationale: "Conjunctive Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-123) overlay on hr.PreferenceProfile: >= 11h rest between shifts, <= 48h regular weekly hours, and >= 24h weekly rest (max 6 consecutive working days).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "hr.PreferenceProfile",
    scope: { root: { kind: "entity", id: "hr.PreferenceProfile" }, objectTypes: ["hr.PreferenceProfile"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        HrPreferenceProfileOverlaySubject,
        (self) =>
          self.min_rest_hours >= 11 &&
          self.max_weekly_hours <= 48 &&
          self.max_consecutive_days <= 6,
      ),
    },
  },
})

export const EgyptPreferenceProfileHolds = Norn.example("Cairo Law 14/2025 compliant preference profile holds", {
  of: EgyptPreferenceProfileLaborLaw,
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

export const EgyptPreferenceProfileSevenDaysViolated = Norn.example("seven consecutive days violates Egypt Law 14/2025 weekly rest overlay", {
  of: EgyptPreferenceProfileLaborLaw,
  given: {
    id: "prof_cairo_7d",
    label: "Illegal 7-day Cairo schedule",
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

export const EgyptPreferenceProfileOver48hViolated = Norn.example("52h weekly cap violates Egypt Law 14/2025 overlay", {
  of: EgyptPreferenceProfileLaborLaw,
  given: {
    id: "prof_cairo_52h",
    label: "Illegal 52h Cairo schedule",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 11,
    max_weekly_hours: 52,
    max_consecutive_days: 6,
    weekend_fairness_priority: "low",
    staff_preference_priority: "low",
    avoid_split_shifts: false,
  },
  expect: "violated",
})

const HrEmploymentOverlaySubject = Schema.Struct({
  max_weekly_hours: Norn.decimal(2),
})

export const EgyptEmploymentMaxWeeklyHours = Norn.law({
  id: "loc_eg.hr.Employment.max_weekly_hours.law",
  title: "Egypt Labor Law No. 14 of 2025 48h weekly cap on hr.Employment",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.Employment.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.hr.Employment.max_weekly_hours",
    owner: "loc_eg",
    severity: "error",
    rationale: "Conjunctive Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-123) overlay capping contracted regular weekly hours on hr.Employment at 48h.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "hr.Employment",
    scope: { root: { kind: "entity", id: "hr.Employment" }, objectTypes: ["hr.Employment"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(HrEmploymentOverlaySubject, (self) => self.max_weekly_hours <= 48),
    },
  },
})

export const EgyptEmployment48hHolds = Norn.example("48h Egypt employment contract holds", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_eg_1", effective_from: "2026-01-01", effective_to: null, currency: "EGP", base_salary: 18000, hourly_overtime_rate: 150, max_weekly_hours: 48, status: "active" },
  expect: "holds",
})

export const EgyptEmployment54hViolated = Norn.example("54h Egypt employment contract violates Law 14/2025 48h cap", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_eg_bad", effective_from: "2026-01-01", effective_to: null, currency: "EGP", base_salary: 18000, hourly_overtime_rate: 150, max_weekly_hours: 54, status: "active" },
  expect: "violated",
})

const HrLeaveAllocationOverlaySubject = Schema.Struct({
  leave_kind: Schema.Literals(["annual", "sick", "unpaid", "emergency", "compensatory"]),
  allocated_days: Norn.decimal(2),
})

export const EgyptLeaveAllocationAnnualFloor = Norn.law({
  id: "loc_eg.hr.LeaveAllocation.annual_floor.law",
  title: "Egypt Labor Law No. 14 of 2025 Art. 124 annual leave statutory floor on hr.LeaveAllocation",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LeaveAllocation.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.hr.LeaveAllocation.annual_floor",
    owner: "loc_eg",
    severity: "error",
    rationale: "Conjunctive Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Art. 124) overlay enforcing the statutory first-year minimum of 15 annual leave days (scaling to 21d in year 2+, 30d at 10+ years/age 50+, and 45d for persons with disabilities).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "hr.LeaveAllocation",
    scope: { root: { kind: "entity", id: "hr.LeaveAllocation" }, objectTypes: ["hr.LeaveAllocation"] },
    condition: Norn.expression(HrLeaveAllocationOverlaySubject, (self) => self.leave_kind === "annual"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(HrLeaveAllocationOverlaySubject, (self) => self.allocated_days >= 15),
    },
  },
})

export const EgyptAnnualLeaveFirstYear15dHolds = Norn.example("first-year 15-day annual leave allocation holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_y1", leave_kind: "annual", starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 15, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeaveStandard21dHolds = Norn.example("year-2+ 21-day annual leave allocation holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_y2", leave_kind: "annual", starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 21, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeave14dViolated = Norn.example("14-day annual leave allocation violates Law 14/2025 Art. 124 15d floor", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_bad", leave_kind: "annual", starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 14, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

export const EgyptSickLeaveNotApplicable = Norn.example("sick leave allocation is outside the annual leave floor overlay", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_sick", leave_kind: "sick", starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 5, carry_forward_days: 0, status: "active" },
  expect: "not_applicable",
})

// ---------------------------------------------------------------- Governed Statutory Settings (NOSI & ETA)

export const EgyptNosiEmployeeRate = Norn.setting({
  id: "loc_eg.setting.nosi_employee_rate",
  title: "Egypt NOSI employee social insurance rate (Law 148/2019)",
  version: "0.1.0",
  settingKey: "loc_eg.nosi.employee_rate",
  valueType: "number",
  value: "0.11",
  effectiveFrom: "2025-09-01T00:00:00Z",
  review: {
    state: "active",
    reviewedBy: "loc_eg_maintainer",
    reviewedAt: "2025-09-01T00:00:00Z",
    reason: "Egypt Social Insurance and Pensions Law No. 148 of 2019 employee contribution rate (11%).",
  },
  scopeRequired: ["orgId", "environmentId"],
})

export const EgyptNosiEmployerRate = Norn.setting({
  id: "loc_eg.setting.nosi_employer_rate",
  title: "Egypt NOSI employer social insurance rate (Law 148/2019)",
  version: "0.1.0",
  settingKey: "loc_eg.nosi.employer_rate",
  valueType: "number",
  value: "0.1875",
  effectiveFrom: "2025-09-01T00:00:00Z",
  review: {
    state: "active",
    reviewedBy: "loc_eg_maintainer",
    reviewedAt: "2025-09-01T00:00:00Z",
    reason: "Egypt Social Insurance and Pensions Law No. 148 of 2019 employer contribution rate (18.75%).",
  },
  scopeRequired: ["orgId", "environmentId"],
})

export const EgyptEtaVatStandardRate = Norn.setting({
  id: "loc_eg.setting.eta_vat_standard_rate",
  title: "Egypt ETA standard VAT rate (Law 67/2016)",
  version: "0.1.0",
  settingKey: "loc_eg.eta.vat_standard_rate",
  valueType: "number",
  value: "0.14",
  effectiveFrom: "2025-09-01T00:00:00Z",
  review: {
    state: "active",
    reviewedBy: "loc_eg_maintainer",
    reviewedAt: "2025-09-01T00:00:00Z",
    reason: "Egyptian Tax Authority (ETA) Value Added Tax Law No. 67 of 2016 standard rate (14%).",
  },
  scopeRequired: ["orgId", "environmentId"],
})
