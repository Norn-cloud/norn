/**
 * `org` foundation pack (#1591, Astra ruling 2026-11-23).
 *
 * Minimal W3C Organization Ontology (`org:OrganizationalUnit`, `org:Post`,
 * `org:Membership`) anchored to Norn's REA core (`core.Agent`, `core.AgentRole`)
 * and `identity.BusinessUnit`.
 *
 * Guardrail: `org.OrgUnit` NEVER replaces or forks `identity.BusinessUnit` as a
 * Cedar/AccessIR security scope dimension. Employment/membership is always a
 * relationship (`org.Membership`), never a subtype of `core.Agent`.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent, AgentRole } from "../../core/src/rea.ts"

export const OrgUnit = Norn.type({
  name: "org.OrgUnit",
  role: "thing",
  title: "Organizational unit",
  version: "0.1.0",
  description: "A branch, department, workshop, or team within an organization (W3C ORG OrganizationalUnit).",
  implements: [Agent],
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["organization"]),
    unit_code: Norn.ref("core.code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    kind: "organization",
  },
  links: [
    { id: "parent", description: "Parent organizational unit (strictly acyclic).", target: "org.OrgUnit", cardinality: { to: "one", from: "many" }, acyclic: true },
    { id: "business_unit", description: "Optional identity security boundary.", target: "identity.BusinessUnit", cardinality: { to: "one", from: "many" } },
  ],
})

export const orgUnitKindIsOrganization = Norn.law("kind_is_organization", {
  on: OrgUnit,
  must: (self) => self.kind === "organization",
  because: "An organizational unit is always an organization agent, never a person or system.",
})

export const orgUnitKindOrganizationHolds = Norn.example("org unit with organization kind holds", {
  of: orgUnitKindIsOrganization,
  given: { id: "unit-cairo", display_name: "Cairo Workshop", kind: "organization", unit_code: "CAIRO-WS" },
  expect: "holds",
})

export const orgUnitKindPersonViolated = Norn.example("org unit with person kind is refused", {
  of: orgUnitKindIsOrganization,
  given: { id: "unit-person", display_name: "Alice", kind: "person", unit_code: "PERSON-1" },
  expect: "violated",
})

export const orgUnitKindSystemViolated = Norn.example("org unit with system kind is refused", {
  of: orgUnitKindIsOrganization,
  given: { id: "unit-system", display_name: "Scheduler Bot", kind: "system", unit_code: "SYS-1" },
  expect: "violated",
})

export const Post = Norn.type({
  name: "org.Post",
  role: "thing",
  title: "Post",
  version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  description: "A budgeted role or position within an organizational unit that can be vacant or filled over time (W3C ORG Post).",
  properties: Schema.Struct({
    id: Schema.String,
    post_code: Norn.ref("core.code"),
    title: Norn.ref("core.display_name"),
    role_code: Norn.ref("core.code"),
    fte_capacity: Norn.decimal(2, { unit: { system: "ucum", code: "{each}" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "unit", description: "Organizational unit owning this post.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const Membership = Norn.type({
  name: "org.Membership",
  role: "thing",
  title: "Membership",
  version: "0.1.0",
  description: "Time-bounded relationship linking an Agent to an OrgUnit and optional Post (W3C ORG Membership, implements core.AgentRole).",
  implements: [AgentRole],
  properties: Schema.Struct({
    id: Schema.String,
    role: Norn.ref<Schema.String>("core.code"),
    valid_from: Norn.ref<Schema.String>("iso8601.date"),
    valid_to: Schema.NullOr(Norn.ref<Schema.String>("iso8601.date")),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "agent", description: "The agent holding this membership.", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "counterparty", description: "Employer or counterparty organization.", target: "core.Agent", cardinality: { to: "one", from: "many" } },
    { id: "unit", description: "Organizational unit of the membership.", target: OrgUnit, cardinality: { to: "one", from: "many", required: true } },
    { id: "post", description: "Specific post fulfilled by this membership, if any.", target: Post, cardinality: { to: "one", from: "many" } },
  ],
})

export const membershipValidDateRange = Norn.law("valid_date_range", {
  on: Membership,
  must: (self) => self.valid_to === null || Norn.dateBefore(self.valid_from, self.valid_to ?? self.valid_from),
  because: "A membership valid_from date must be strictly before valid_to whenever valid_to is present.",
})

export const membershipOpenEndedHolds = Norn.example("open-ended membership with valid_from holds", {
  of: membershipValidDateRange,
  given: { id: "mem-1", role: "goldsmith", valid_from: "2026-01-01", valid_to: null },
  expect: "holds",
})

export const membershipBoundedHolds = Norn.example("bounded membership with valid_from before valid_to holds", {
  of: membershipValidDateRange,
  given: { id: "mem-2", role: "goldsmith", valid_from: "2026-01-01", valid_to: "2026-12-31" },
  expect: "holds",
})

export const membershipEqualBoundsViolated = Norn.example("membership with equal valid_from and valid_to is refused", {
  of: membershipValidDateRange,
  given: { id: "mem-3", role: "goldsmith", valid_from: "2026-06-01", valid_to: "2026-06-01" },
  expect: "violated",
})

export const membershipReversedBoundsViolated = Norn.example("membership with valid_from after valid_to is refused", {
  of: membershipValidDateRange,
  given: { id: "mem-4", role: "goldsmith", valid_from: "2026-12-31", valid_to: "2026-01-01" },
  expect: "violated",
})
