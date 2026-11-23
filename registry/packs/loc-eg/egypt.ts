/**
 * Egypt localization pack (`loc-eg`): jurisdiction-specific labor baseline,
 * statutory parameter defaults, and conjunctive labor laws (#1593).
 *
 * Composes conjunctively with `registry/packs/hr/` (`hr ∧ loc-eg ∧ company-overlay`)
 * so `hr` itself contains zero country-specific branches.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const EgyptLaborProfile = Norn.type({
  name: "loc_eg.LaborProfile",
  role: "thing",
  title: "Egypt labor-law profile",
  version: "0.1.0",
  description: "Governed Egyptian labor parameters (Africa/Cairo timezone, EGP currency, statutory weekly/daily hour caps, rest gap, and annual leave tiers).",
  properties: Schema.Struct({
    id: Schema.String,
    effective_from: Norn.date(),
    iana_timezone: Schema.Literals(["Africa/Cairo"]),
    currency: Schema.Literals(["EGP"]),
    max_regular_weekly_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    max_daily_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    min_rest_between_shifts_hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
    standard_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
    senior_annual_leave_days: Norn.decimal(2, { unit: { system: "ucum", code: "d" } }),
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
})

export const EgyptStatutoryFloors = Norn.law({
  id: "loc_eg.LaborProfile.statutory_floors.law",
  title: "Egyptian statutory labor-law floors and hour caps",
  version: "0.1.0",
  dependencies: [{ artifactId: "loc_eg.LaborProfile.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "loc_eg.LaborProfile.statutory_floors",
    owner: "loc_eg",
    severity: "error",
    rationale: "Egyptian labor baselines cap regular weekly hours at 48h, daily hours at 10h, enforce >= 11h rest between shifts, and grant >= 21/30 annual leave days.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/loc-eg/egypt.ts" },
    subject: "loc_eg.LaborProfile",
    scope: { root: { kind: "entity", id: "loc_eg.LaborProfile" }, objectTypes: ["loc_eg.LaborProfile"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.max_regular_weekly_hours <= 48 and self.max_daily_hours <= 10 and self.min_rest_between_shifts_hours >= 11 and self.standard_annual_leave_days >= 21 and self.senior_annual_leave_days >= 30",
    },
  },
})

export const EgyptBaselineHolds = Norn.example("standard Egypt 48h / 21d / 30d profile holds", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_2026",
    effective_from: "2026-01-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    max_regular_weekly_hours: 48,
    max_daily_hours: 10,
    min_rest_between_shifts_hours: 11,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "holds",
})

export const EgyptShortRestViolated = Norn.example("eight-hour rest between shifts violates Egyptian floor", {
  of: EgyptStatutoryFloors,
  given: {
    id: "eg_bad_rest",
    effective_from: "2026-01-01",
    iana_timezone: "Africa/Cairo",
    currency: "EGP",
    max_regular_weekly_hours: 48,
    max_daily_hours: 10,
    min_rest_between_shifts_hours: 8,
    standard_annual_leave_days: 21,
    senior_annual_leave_days: 30,
    seniority_threshold_years: 10,
    weekend_pattern: "friday_saturday",
  },
  expect: "violated",
})
