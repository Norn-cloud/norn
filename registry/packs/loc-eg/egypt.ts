/**
 * Egypt localization pack (`loc-eg`): Egypt Labor Law No. 14 of 2025
 * (effective 2025-09-01, repealing Law No. 12 of 2003), conjunctive `Norn.law`
 * overlays scoped to Egyptian records (`jurisdiction_code == "EG"` and effective
 * on/after `2025-09-01` where dated) directly targeting `hr.*` entities, and
 * governed effective-dated NOSI (Law 148/2019) & ETA VAT (Law 67/2016)
 * `Norn.setting` parameters (#1593).
 *
 * Composes conjunctively with `registry/packs/hr/` (`hr ∧ loc-eg ∧ company-overlay`)
 * so `hr` itself remains 100% jurisdiction-agnostic and multi-jurisdiction cells
 * do not falsely constrain non-Egyptian or pre-2025-09-01 records.
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
  description: "Governed Egyptian labor parameters under Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-124): Africa/Cairo timezone, EGP currency, max 48h/wk regular, max 8h/day ordinary, max 10h workplace presence (12h for Arts. 119/121 intermittent or preparatory work), >= 24h weekly rest, annual leave tiers (prorated 6-11m, 15d / 21d / 30d / 45d), and >= 7d hazardous/remote supplement.",
  properties: Schema.Struct({
    id: Schema.String,
    effective_from: Norn.date(),
    iana_timezone: Schema.Literals(["Africa/Cairo"]),
    currency: Schema.Literals(["EGP"]),
    intermittent_or_preparatory_work: Schema.Boolean,
    max_regular_weekly_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    max_daily_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    max_workplace_presence_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    min_weekly_rest_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    first_year_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    standard_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    senior_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    disability_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    hazardous_remote_supplement_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    seniority_threshold_years: Schema.Int,
    weekend_pattern: Schema.Literals(["friday_saturday", "friday_only"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: false,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  links: [
    { id: "unit", description: "Egyptian branch or organizational unit governed by this statutory labor profile.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
})

const EgyptLaborProfileSubject = Schema.Struct({
  intermittent_or_preparatory_work: Schema.Boolean,
  max_regular_weekly_hours: Norn.decimal(2),
  max_daily_hours: Norn.decimal(2),
  max_workplace_presence_hours: Norn.decimal(2),
  min_weekly_rest_hours: Norn.decimal(2),
  first_year_annual_leave_days: Norn.decimal(2),
  standard_annual_leave_days: Norn.decimal(2),
  senior_annual_leave_days: Norn.decimal(2),
  disability_annual_leave_days: Norn.decimal(2),
  hazardous_remote_supplement_days: Norn.decimal(2),
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
    rationale: "Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-124) caps regular weekly hours at 48h, ordinary daily hours at 8h, and daily workplace presence at 10h (or 12h for Arts. 119/121 intermittent or preparatory work), mandates >= 24h consecutive weekly rest, sets annual leave floors of 15d (year 1), 21d (year 2+), 30d (10+ years or age 50+), and 45d (persons with disabilities/short stature), plus >= 7d supplement for hazardous or remote work.",
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
          self.max_daily_hours <= 8 &&
          (self.intermittent_or_preparatory_work
            ? self.max_workplace_presence_hours <= 12
            : self.max_workplace_presence_hours <= 10) &&
          self.min_weekly_rest_hours >= 24 &&
          self.first_year_annual_leave_days >= 15 &&
          self.standard_annual_leave_days >= 21 &&
          self.senior_annual_leave_days >= 30 &&
          self.disability_annual_leave_days >= 45 &&
          self.hazardous_remote_supplement_days >= 7,
      ),
    },
  },
})

export const EgyptBaselineHolds = Norn.example("Law 14/2025 48h / 8h / 10h / 24h / 15d-21d-30d-45d/+7d profile holds", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_2025",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: false,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 10,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "holds",
})

export const EgyptIntermittent12hPresenceHolds = Norn.example("12h workplace presence holds for Arts. 119/121 intermittent or preparatory work", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_intermittent_12h",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: true,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 12,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "holds",
})

export const EgyptStandard12hPresenceViolated = Norn.example("12h workplace presence without intermittent/preparatory exception violates 10h cap", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_std_12h_bad",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: false,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 12,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

export const EgyptIntermittent13hPresenceViolated = Norn.example("13h workplace presence exceeds even Arts. 119/121 12h intermittent exception", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_intermittent_13h_bad",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: true,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 13,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

export const EgyptShortWeeklyRestViolated = Norn.example("twelve-hour weekly rest violates Law 14/2025 24h weekly rest floor", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_bad_rest",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: false,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 10,
    min_weekly_rest_hours: 12,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

export const EgyptOver8hDailyViolated = Norn.example("nine-hour ordinary workday violates Law 14/2025 8h daily cap", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_bad_daily",
    effective_from: "2025-09-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    intermittent_or_preparatory_work: false,
    max_regular_weekly_hours: 48,
    max_daily_hours: 9,
    max_workplace_presence_hours: 10,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
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
    intermittent_or_preparatory_work: false,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 10,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 14,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 45,
    hazardous_remote_supplement_days: 7,
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
    intermittent_or_preparatory_work: false,
    max_regular_weekly_hours: 48,
    max_daily_hours: 8,
    max_workplace_presence_hours: 10,
    min_weekly_rest_hours: 24,
    first_year_annual_leave_days: 15,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    disability_annual_leave_days: 30,
    hazardous_remote_supplement_days: 7,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Conjunctive Overlays on hr.* Entities

const HrPreferenceProfileOverlaySubject = Schema.Struct({
  jurisdiction_code: Schema.NullOr(Schema.String),
  max_daily_hours: Norn.decimal(2),
  max_weekly_hours: Norn.decimal(2),
  min_weekly_rest_hours: Norn.decimal(2),
  max_consecutive_days: Schema.Int,
})

export const EgyptPreferenceProfileLaborLaw = Norn.law({
  id: "loc_eg.hr.PreferenceProfile.labor_law_14_2025.law",
  title: "Egypt Labor Law No. 14 of 2025 rostering caps on Egyptian hr.PreferenceProfile",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.PreferenceProfile.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.hr.PreferenceProfile.labor_law_14_2025",
    owner: "loc_eg",
    severity: "error",
    rationale: "Conjunctive Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-123) overlay on Egyptian hr.PreferenceProfile (jurisdiction_code == 'EG'): <= 48h regular weekly hours, <= 8h ordinary daily hours, >= 24h consecutive weekly rest, and max 6 consecutive working days.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "hr.PreferenceProfile",
    scope: { root: { kind: "entity", id: "hr.PreferenceProfile" }, objectTypes: ["hr.PreferenceProfile"] },
    condition: Norn.expression(HrPreferenceProfileOverlaySubject, (self) => (self.jurisdiction_code ?? "") === "EG"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        HrPreferenceProfileOverlaySubject,
        (self) =>
          self.max_weekly_hours <= 48 &&
          self.max_daily_hours <= 8 &&
          self.min_weekly_rest_hours >= 24 &&
          self.max_consecutive_days <= 6,
      ),
    },
  },
})

export const EgyptPreferenceProfileHolds = Norn.example("Cairo Law 14/2025 compliant preference profile holds", {
  of: EgyptPreferenceProfileLaborLaw,
  given: {
    id: "prof_cairo",
    jurisdiction_code: "EG",
    label: "Cairo Retail Balanced",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 10,
    max_daily_hours: 8,
    max_weekly_hours: 48,
    min_weekly_rest_hours: 24,
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
    jurisdiction_code: "EG",
    label: "Illegal 7-day Cairo schedule",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 10,
    max_daily_hours: 8,
    max_weekly_hours: 48,
    min_weekly_rest_hours: 24,
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
    jurisdiction_code: "EG",
    label: "Illegal 52h Cairo schedule",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 10,
    max_daily_hours: 8,
    max_weekly_hours: 52,
    min_weekly_rest_hours: 24,
    max_consecutive_days: 6,
    weekend_fairness_priority: "low",
    staff_preference_priority: "low",
    avoid_split_shifts: false,
  },
  expect: "violated",
})

export const EgyptPreferenceProfileOver8hDailyViolated = Norn.example("9h ordinary daily cap violates Egypt Law 14/2025 Arts. 117-123", {
  of: EgyptPreferenceProfileLaborLaw,
  given: {
    id: "prof_cairo_9h",
    jurisdiction_code: "EG",
    label: "Illegal 9h daily Cairo schedule",
    currency: "EGP",
    weekly_overtime_budget: 1500,
    min_rest_hours: 10,
    max_daily_hours: 9,
    max_weekly_hours: 45,
    min_weekly_rest_hours: 24,
    max_consecutive_days: 5,
    weekend_fairness_priority: "low",
    staff_preference_priority: "low",
    avoid_split_shifts: false,
  },
  expect: "violated",
})

export const EgyptPreferenceProfileNonEgNotApplicable = Norn.example("non-Egyptian GB preference profile is outside Egypt Law 14/2025 overlay", {
  of: EgyptPreferenceProfileLaborLaw,
  given: {
    id: "prof_gb",
    jurisdiction_code: "GB",
    label: "London 4x10 Shift Profile",
    currency: "GBP",
    weekly_overtime_budget: 2000,
    min_rest_hours: 11,
    max_daily_hours: 10,
    max_weekly_hours: 40,
    min_weekly_rest_hours: 24,
    max_consecutive_days: 4,
    weekend_fairness_priority: "medium",
    staff_preference_priority: "high",
    avoid_split_shifts: true,
  },
  expect: "not_applicable",
})

const HrEmploymentOverlaySubject = Schema.Struct({
  jurisdiction_code: Schema.NullOr(Schema.String),
  effective_from: Norn.date(),
  effective_to: Schema.NullOr(Norn.date()),
  max_weekly_hours: Norn.decimal(2),
})

export const EgyptEmploymentMaxWeeklyHours = Norn.law({
  id: "loc_eg.hr.Employment.max_weekly_hours.law",
  title: "Egypt Labor Law No. 14 of 2025 48h weekly cap on Egyptian hr.Employment",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.Employment.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.hr.Employment.max_weekly_hours",
    owner: "loc_eg",
    severity: "error",
    rationale: "Conjunctive Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Arts. 117-123) overlay capping contracted regular weekly hours at 48h on Egyptian hr.Employment active on or after 2025-09-01 (jurisdiction_code == 'EG' and (effective_to == null or effective_to >= 2025-09-01)), including open-ended contracts that began before 2025-09-01.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "hr.Employment",
    scope: { root: { kind: "entity", id: "hr.Employment" }, objectTypes: ["hr.Employment"] },
    condition: Norn.expression(
      HrEmploymentOverlaySubject,
      (self) =>
        (self.jurisdiction_code ?? "") === "EG" &&
        (self.effective_to === null || Norn.parseDate(self.effective_to ?? self.effective_from) >= Norn.parseDate("2025-09-01")),
    ),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(HrEmploymentOverlaySubject, (self) => self.max_weekly_hours <= 48),
    },
  },
})

export const EgyptEmployment48hHolds = Norn.example("48h Egypt employment contract effective under Law 14/2025 holds", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_eg_1", jurisdiction_code: "EG", effective_from: "2026-01-01", effective_to: null, currency: "EGP", base_salary: 18000, hourly_overtime_rate: 150, max_weekly_hours: 48, status: "active" },
  expect: "holds",
})

export const EgyptEmployment54hViolated = Norn.example("54h Egypt employment contract effective under Law 14/2025 violates 48h cap", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_eg_bad", jurisdiction_code: "EG", effective_from: "2026-01-01", effective_to: null, currency: "EGP", base_salary: 18000, hourly_overtime_rate: 150, max_weekly_hours: 54, status: "active" },
  expect: "violated",
})

export const EgyptEmploymentOngoingPre2025Violated = Norn.example("open-ended Egyptian contract started before 2025-09-01 at 54h/wk violates Law 14/2025", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_eg_ongoing_2024", jurisdiction_code: "EG", effective_from: "2024-01-01", effective_to: null, currency: "EGP", base_salary: 16000, hourly_overtime_rate: 130, max_weekly_hours: 54, status: "active" },
  expect: "violated",
})

export const EgyptEmploymentNonEgNotApplicable = Norn.example("GB employment contract is outside Egypt Law 14/2025 overlay", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_gb", jurisdiction_code: "GB", effective_from: "2026-01-01", effective_to: null, currency: "GBP", base_salary: 5000, hourly_overtime_rate: 40, max_weekly_hours: 54, status: "active" },
  expect: "not_applicable",
})

export const EgyptEmploymentPreEffectiveDateNotApplicable = Norn.example("Egyptian contract ended before 2025-09-01 is outside Law 14/2025 overlay", {
  of: EgyptEmploymentMaxWeeklyHours,
  given: { id: "emp_eg_hist", jurisdiction_code: "EG", effective_from: "2025-01-01", effective_to: "2025-08-31", currency: "EGP", base_salary: 15000, hourly_overtime_rate: 120, max_weekly_hours: 54, status: "terminated" },
  expect: "not_applicable",
})

const HrLeaveAllocationOverlaySubject = Schema.Struct({
  jurisdiction_code: Schema.NullOr(Schema.String),
  leave_kind: Schema.Literals(["annual", "sick", "unpaid", "emergency", "compensatory"]),
  entitlement_tier: Schema.Literals(["prorated_initial", "first_year", "standard", "senior", "protected_disability"]),
  hazardous_or_remote: Schema.Boolean,
  qualifying_service_months: Schema.NullOr(Schema.Int),
  starts_on: Norn.date(),
  allocated_days: Norn.decimal(2),
})

export const EgyptLeaveAllocationAnnualFloor = Norn.law({
  id: "loc_eg.hr.LeaveAllocation.annual_floor.law",
  title: "Egypt Labor Law No. 14 of 2025 Art. 124 annual leave tier floors on Egyptian hr.LeaveAllocation",
  version: "0.1.0",
  dependencies: [{ artifactId: "hr.LeaveAllocation.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.hr.LeaveAllocation.annual_floor",
    owner: "loc_eg",
    severity: "error",
    rationale: "Conjunctive Egypt Labor Law No. 14 of 2025 (effective 2025-09-01, Art. 124) overlay on Egyptian annual leave allocations (jurisdiction_code == 'EG', leave_kind == 'annual', starts_on >= 2025-09-01): prorated_initial requires 6 <= qualifying_service_months < 12 with exact proportional floor 15 * m / 12 (+7d hazardous/remote: 7.5/14.5 at 6m, 8.75/15.75 at 7m, 10/17 at 8m, 11.25/18.25 at 9m, 12.5/19.5 at 10m, 13.75/20.75 at 11m); first_year >= 15d (+7d = 22d); standard >= 21d (+7d = 28d); senior >= 30d (+7d = 37d); protected_disability >= 45d (+7d = 52d).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "hr.LeaveAllocation",
    scope: { root: { kind: "entity", id: "hr.LeaveAllocation" }, objectTypes: ["hr.LeaveAllocation"] },
    condition: Norn.expression(
      HrLeaveAllocationOverlaySubject,
      (self) =>
        (self.jurisdiction_code ?? "") === "EG" &&
        self.leave_kind === "annual" &&
        Norn.parseDate(self.starts_on) >= Norn.parseDate("2025-09-01"),
    ),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        HrLeaveAllocationOverlaySubject,
        (self) =>
          (self.entitlement_tier === "prorated_initial" &&
            (((self.qualifying_service_months ?? 0) === 6 && (self.hazardous_or_remote ? self.allocated_days >= 14.5 : self.allocated_days >= 7.5)) ||
              ((self.qualifying_service_months ?? 0) === 7 && (self.hazardous_or_remote ? self.allocated_days >= 15.75 : self.allocated_days >= 8.75)) ||
              ((self.qualifying_service_months ?? 0) === 8 && (self.hazardous_or_remote ? self.allocated_days >= 17 : self.allocated_days >= 10)) ||
              ((self.qualifying_service_months ?? 0) === 9 && (self.hazardous_or_remote ? self.allocated_days >= 18.25 : self.allocated_days >= 11.25)) ||
              ((self.qualifying_service_months ?? 0) === 10 && (self.hazardous_or_remote ? self.allocated_days >= 19.5 : self.allocated_days >= 12.5)) ||
              ((self.qualifying_service_months ?? 0) === 11 && (self.hazardous_or_remote ? self.allocated_days >= 20.75 : self.allocated_days >= 13.75)))) ||
          (self.entitlement_tier === "first_year" && (self.hazardous_or_remote ? self.allocated_days >= 22 : self.allocated_days >= 15)) ||
          (self.entitlement_tier === "standard" && (self.hazardous_or_remote ? self.allocated_days >= 28 : self.allocated_days >= 21)) ||
          (self.entitlement_tier === "senior" && (self.hazardous_or_remote ? self.allocated_days >= 37 : self.allocated_days >= 30)) ||
          (self.entitlement_tier === "protected_disability" && (self.hazardous_or_remote ? self.allocated_days >= 52 : self.allocated_days >= 45)),
      ),
    },
  },
})

export const EgyptAnnualLeaveProratedHolds = Norn.example("prorated 7.5-day initial annual leave at 6 months holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_pro", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "prorated_initial", hazardous_or_remote: false, qualifying_service_months: 6, starts_on: "2026-07-01", ends_on: "2026-12-31", allocated_days: 7.5, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeaveProratedHazardousHolds = Norn.example("prorated 14.5-day hazardous initial annual leave at 6 months holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_pro_haz", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "prorated_initial", hazardous_or_remote: true, qualifying_service_months: 6, starts_on: "2026-07-01", ends_on: "2026-12-31", allocated_days: 14.5, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeaveProratedTinyHazardousViolated = Norn.example("0.01-day hazardous prorated allocation is refused under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_pro_tiny", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "prorated_initial", hazardous_or_remote: true, qualifying_service_months: 6, starts_on: "2026-07-01", ends_on: "2026-12-31", allocated_days: 0.01, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

export const EgyptAnnualLeaveProratedUnder6MonthsViolated = Norn.example("prorated allocation with 4 months (< 6m statutory eligibility) is refused under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_pro_4m", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "prorated_initial", hazardous_or_remote: false, qualifying_service_months: 4, starts_on: "2026-05-01", ends_on: "2026-12-31", allocated_days: 7.5, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

export const EgyptAnnualLeaveFirstYear15dHolds = Norn.example("first-year 15-day annual leave allocation holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_y1", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "first_year", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 15, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeaveStandard15dViolated = Norn.example("year-2+ standard 15-day annual leave violates Law 14/2025 Art. 124 21d floor", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_std_bad", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 15, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

export const EgyptAnnualLeaveSenior30dHolds = Norn.example("senior 30-day annual leave allocation holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_sen", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "senior", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 30, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeaveDisability45dHolds = Norn.example("protected disability 45-day annual leave holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_dis", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "protected_disability", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 45, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptAnnualLeaveHazardousStandard21dViolated = Norn.example("hazardous standard 21-day annual leave violates Law 14/2025 Art. 124 +7d (28d) floor", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_haz_bad", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: true, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 21, carry_forward_days: 0, status: "active" },
  expect: "violated",
})

export const EgyptAnnualLeaveHazardousStandard28dHolds = Norn.example("hazardous standard 28-day annual leave holds under Law 14/2025 Art. 124", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_haz_ok", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: true, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 28, carry_forward_days: 0, status: "active" },
  expect: "holds",
})

export const EgyptSickLeaveNotApplicable = Norn.example("sick leave allocation is outside the annual leave floor overlay", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_sick", jurisdiction_code: "EG", leave_kind: "sick", entitlement_tier: "standard", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 5, carry_forward_days: 0, status: "active" },
  expect: "not_applicable",
})

export const EgyptAnnualLeaveNonEgNotApplicable = Norn.example("non-Egyptian GB annual leave allocation is outside Egypt Law 14/2025 overlay", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_gb", jurisdiction_code: "GB", leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2026-01-01", ends_on: "2026-12-31", allocated_days: 10, carry_forward_days: 0, status: "active" },
  expect: "not_applicable",
})

export const EgyptAnnualLeavePreEffectiveDateNotApplicable = Norn.example("pre-2025-09-01 Egyptian annual leave allocation is outside Law 14/2025 overlay", {
  of: EgyptLeaveAllocationAnnualFloor,
  given: { id: "la_eg_hist", jurisdiction_code: "EG", leave_kind: "annual", entitlement_tier: "standard", hazardous_or_remote: false, qualifying_service_months: null, starts_on: "2025-01-01", ends_on: "2025-12-31", allocated_days: 15, carry_forward_days: 0, status: "expired" },
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
