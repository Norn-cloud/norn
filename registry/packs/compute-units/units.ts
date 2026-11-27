/**
 * `compute-units` pack: Extended UCUM / ISO 80000 / IEC 80000-13 unit catalogs
 * and ISO 15944-4 REA compute / AI metering concepts.
 *
 * Lives outside `registry/core/artifacts/core.units.json` so embedded cell base
 * fixtures remain untouched while domain packs can explicitly pin exact
 * compute, storage, sub-second time, AI, and physical UCUM vocabularies
 * (`VAL-UNIT-1`) and price compute usage with exact `Norn.rate` dimensional
 * algebra (`VAL-RATE`: `Rate<USD, {token}> × Quantity<{token}> → Money<USD>`).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent, Resource, EconomicEvent, Hold, Commitment } from "../../core/src/rea.ts"

// ---------------------------------------------------------------- Unit catalogs

export const ComputeUnitDefinition = Norn.type({
  name: "units.compute.UnitDefinition",
  role: "thing",
  title: "Compute, storage, sub-second time, and AI UCUM unit catalog",
  version: "0.1.0",
  description:
    "Exact UCUM 2.2 / IEC 80000-13 unit catalog for sub-second time (ms, us, ns), IEC binary and SI decimal storage (bit, By, KiBy..TiBy, kBy..TBy), and AI/compute countable/compound units ({token}, {input_token}, {output_token}, {request}, {vCPU}.s, {vCPU}.h, {GPU}.s, {GPU}.h, {seat}).",
  properties: Schema.Struct({
    code: Schema.Literals([
      "ms",
      "us",
      "ns",
      "bit",
      "By",
      "KiBy",
      "MiBy",
      "GiBy",
      "TiBy",
      "kBy",
      "MBy",
      "GBy",
      "TBy",
      "{token}",
      "{input_token}",
      "{output_token}",
      "{request}",
      "{vCPU}.s",
      "{vCPU}.h",
      "{GPU}.s",
      "{GPU}.h",
      "{seat}",
    ]),
    dimension: Schema.Literals(["time", "information", "compute_count", "compute_time"]),
    base_code: Schema.Literals([
      "s",
      "By",
      "{token}",
      "{input_token}",
      "{output_token}",
      "{request}",
      "{vCPU}.s",
      "{GPU}.s",
      "{seat}",
    ]),
    numerator: Schema.Int,
    denominator: Schema.Int,
    standard: Schema.Literals(["ucum-2.2", "iec-80000-13"]),
  }),
  identity: [{ name: "primary", properties: ["code"] }],
})

export const computeUnitExactTimeFactor = Norn.law("exact_time_factor", {
  on: ComputeUnitDefinition,
  when: (self) => self.dimension === "time",
  must: (self) =>
    self.base_code === "s" &&
    self.numerator === 1 &&
    ((self.code === "ms" && self.denominator === 1000) ||
      (self.code === "us" && self.denominator === 1000000) ||
      (self.code === "ns" && self.denominator === 1000000000)),
  because: "UNITS-SCALE-1: sub-second time units (ms, us, ns) carry exact UCUM rational factors to s.",
})

export const computeUnitExactInformationFactor = Norn.law("exact_information_factor", {
  on: ComputeUnitDefinition,
  when: (self) => self.dimension === "information",
  must: (self) =>
    self.base_code === "By" &&
    ((self.code === "bit" && self.numerator === 1 && self.denominator === 8) ||
      (self.denominator === 1 &&
        ((self.code === "By" && self.numerator === 1) ||
          (self.code === "KiBy" && self.numerator === 1024) ||
          (self.code === "MiBy" && self.numerator === 1048576) ||
          (self.code === "GiBy" && self.numerator === 1073741824) ||
          (self.code === "TiBy" && self.numerator === 1099511627776) ||
          (self.code === "kBy" && self.numerator === 1000) ||
          (self.code === "MBy" && self.numerator === 1000000) ||
          (self.code === "GBy" && self.numerator === 1000000000) ||
          (self.code === "TBy" && self.numerator === 1000000000000)))),
  because:
    "UNITS-SCALE-1: IEC 80000-13 binary (KiBy..TiBy) and SI decimal (kBy..TBy) storage units carry distinct exact rational factors to By.",
})

export const computeUnitExactComputeCountFactor = Norn.law("exact_compute_count_factor", {
  on: ComputeUnitDefinition,
  when: (self) => self.dimension === "compute_count",
  must: (self) =>
    self.numerator === 1 &&
    self.denominator === 1 &&
    ((self.code === "{token}" && self.base_code === "{token}") ||
      (self.code === "{input_token}" && self.base_code === "{input_token}") ||
      (self.code === "{output_token}" && self.base_code === "{output_token}") ||
      (self.code === "{request}" && self.base_code === "{request}") ||
      (self.code === "{seat}" && self.base_code === "{seat}")),
  because: "UNITS-SCALE-1: countable AI/compute units carry exact 1/1 identity factors to their own canonical bases.",
})

export const computeUnitExactComputeTimeFactor = Norn.law("exact_compute_time_factor", {
  on: ComputeUnitDefinition,
  when: (self) => self.dimension === "compute_time",
  must: (self) =>
    self.denominator === 1 &&
    ((self.code === "{vCPU}.s" && self.base_code === "{vCPU}.s" && self.numerator === 1) ||
      (self.code === "{vCPU}.h" && self.base_code === "{vCPU}.s" && self.numerator === 3600) ||
      (self.code === "{GPU}.s" && self.base_code === "{GPU}.s" && self.numerator === 1) ||
      (self.code === "{GPU}.h" && self.base_code === "{GPU}.s" && self.numerator === 3600)),
  because: "UNITS-SCALE-1: compound compute-time units carry exact rational factors (3600/1 from hours to seconds).",
})

export const computeUnitGiByHolds = Norn.example("IEC 80000-13 GiBy factor 1073741824/1 holds", {
  of: computeUnitExactInformationFactor,
  given: { code: "GiBy", dimension: "information", base_code: "By", numerator: 1073741824, denominator: 1, standard: "iec-80000-13" },
  expect: "holds",
})

export const computeUnitMsHolds = Norn.example("sub-second ms factor 1/1000 holds", {
  of: computeUnitExactTimeFactor,
  given: { code: "ms", dimension: "time", base_code: "s", numerator: 1, denominator: 1000, standard: "ucum-2.2" },
  expect: "holds",
})

export const computeUnitTokenHolds = Norn.example("countable {token} identity factor 1/1 holds", {
  of: computeUnitExactComputeCountFactor,
  given: { code: "{token}", dimension: "compute_count", base_code: "{token}", numerator: 1, denominator: 1, standard: "ucum-2.2" },
  expect: "holds",
})

export const computeUnitGpuHourHolds = Norn.example("compute {GPU}.h factor 3600/1 of {GPU}.s holds", {
  of: computeUnitExactComputeTimeFactor,
  given: { code: "{GPU}.h", dimension: "compute_time", base_code: "{GPU}.s", numerator: 3600, denominator: 1, standard: "ucum-2.2" },
  expect: "holds",
})

export const computeUnitKiByDecimalConflationViolated = Norn.example("conflating IEC KiBy with decimal 1000/1 is refused", {
  of: computeUnitExactInformationFactor,
  given: { code: "KiBy", dimension: "information", base_code: "By", numerator: 1000, denominator: 1, standard: "iec-80000-13" },
  expect: "violated",
})

export const PhysicalUnitDefinition = Norn.type({
  name: "units.physical.UnitDefinition",
  role: "thing",
  title: "Physical, logistics, and energy ISO 80000 / UCUM unit catalog",
  version: "0.1.0",
  description:
    "Exact ISO 80000 / UCUM 2.2 unit catalog for length (m, cm, mm, km), area (m2), volume (L, mL, m3), and energy (J, kW.h).",
  properties: Schema.Struct({
    code: Schema.Literals(["m", "cm", "mm", "km", "m2", "L", "mL", "m3", "J", "kW.h"]),
    dimension: Schema.Literals(["length", "area", "volume", "energy"]),
    base_code: Schema.Literals(["m", "m2", "m3", "J"]),
    numerator: Schema.Int,
    denominator: Schema.Int,
    standard: Schema.Literals(["ucum-2.2", "iso-80000"]),
  }),
  identity: [{ name: "primary", properties: ["code"] }],
})

export const physicalUnitExactRationalFactor = Norn.law("exact_rational_factor", {
  on: PhysicalUnitDefinition,
  must: (self) =>
    self.numerator > 0 &&
    self.denominator > 0 &&
    ((self.code === "m" && self.dimension === "length" && self.base_code === "m" && self.numerator === 1 && self.denominator === 1) ||
      (self.code === "cm" && self.dimension === "length" && self.base_code === "m" && self.numerator === 1 && self.denominator === 100) ||
      (self.code === "mm" && self.dimension === "length" && self.base_code === "m" && self.numerator === 1 && self.denominator === 1000) ||
      (self.code === "km" && self.dimension === "length" && self.base_code === "m" && self.numerator === 1000 && self.denominator === 1) ||
      (self.code === "m2" && self.dimension === "area" && self.base_code === "m2" && self.numerator === 1 && self.denominator === 1) ||
      (self.code === "L" && self.dimension === "volume" && self.base_code === "m3" && self.numerator === 1 && self.denominator === 1000) ||
      (self.code === "mL" && self.dimension === "volume" && self.base_code === "m3" && self.numerator === 1 && self.denominator === 1000000) ||
      (self.code === "m3" && self.dimension === "volume" && self.base_code === "m3" && self.numerator === 1 && self.denominator === 1) ||
      (self.code === "J" && self.dimension === "energy" && self.base_code === "J" && self.numerator === 1 && self.denominator === 1) ||
      (self.code === "kW.h" && self.dimension === "energy" && self.base_code === "J" && self.numerator === 3600000 && self.denominator === 1)),
  because: "UNITS-SCALE-1: every physical, logistics, and energy unit must carry its exact ISO 80000 / UCUM rational conversion factor to its base unit.",
})

export const physicalUnitKwhHolds = Norn.example("ISO 80000 kW.h factor 3600000/1 of J holds", {
  of: physicalUnitExactRationalFactor,
  given: { code: "kW.h", dimension: "energy", base_code: "J", numerator: 3600000, denominator: 1, standard: "iso-80000" },
  expect: "holds",
})

export const physicalUnitKwhWrongFactorViolated = Norn.example("kW.h with wrong Joule factor 3600/1 is refused", {
  of: physicalUnitExactRationalFactor,
  given: { code: "kW.h", dimension: "energy", base_code: "J", numerator: 3600, denominator: 1, standard: "iso-80000" },
  expect: "violated",
})

// -------------------------------------------------- REA Compute & AI Metering

export const TokenPool = Norn.type({
  name: "compute.TokenPool",
  role: "thing",
  title: "Token pool",
  version: "0.1.0",
  dependencies: [{ artifactId: "units.compute.UnitDefinition.entity", version: "0.1.0", kind: "entity" }],
  description:
    "Conserved stock of metered AI tokens (implements core.Resource, ISO 15944-4 REA). Semi-additive across pools at one instant, never summed across time.",
  implements: [Resource],
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref<Schema.String>("core.code"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "semi_additive" }, unitSource: ["unit", "currency"] }),
    unit: Norn.ref<Schema.String>("ucum.unit_code"),
    currency: Schema.NullOr(Norn.ref<Schema.String>("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    available: Norn.decimal(0, { unit: { system: "ucum", code: "{token}" }, additivity: { kind: "semi_additive" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    unit: "{token}",
  },
  links: [
    { id: "owner", description: "Agent owning this token pool.", target: "core.Agent", cardinality: { to: "one", from: "many" } },
    { id: "custodian", description: "Provider or platform holding custody of this token pool.", target: "core.Agent", cardinality: { to: "one", from: "many" } },
  ],
})

// `self.available - self.available` yields a `{token}`-typed zero so this law
// stays diagnostic-free under the repo-wide pack law gate (`registry-packs.test.ts`),
// whereas `self.available >= 0` emits `Info core.values.literal_unit`.
export const tokenPoolAvailableNonNegative = Norn.law("available_non_negative_in_tokens", {
  on: TokenPool,
  must: (self) => self.unit === "{token}" && self.currency === null && self.available >= self.available - self.available,
  because: "A compute.TokenPool is denominated strictly in {token} with non-negative available token balance.",
})

export const tokenPoolValidHolds = Norn.example("token pool with unit {token} and non-negative available balance holds", {
  of: tokenPoolAvailableNonNegative,
  given: {
    id: "pool-acme-gpt",
    resource_code: "LLM-TOKENS",
    quantity: 1000000,
    unit: "{token}",
    currency: null,
    valuation_method: null,
    available: 750000,
  },
  expect: "holds",
})

export const tokenPoolNegativeAvailableViolated = Norn.example("token pool with negative available balance is refused", {
  of: tokenPoolAvailableNonNegative,
  given: {
    id: "pool-negative",
    resource_code: "LLM-TOKENS",
    quantity: 1000000,
    unit: "{token}",
    currency: null,
    valuation_method: null,
    available: -1,
  },
  expect: "violated",
})

export const ComputeAgent = Norn.type({
  name: "compute.Agent",
  role: "thing",
  title: "Compute agent",
  version: "0.1.0",
  description:
    "Concrete accountable compute provider, customer, or workload actor implementing core.Agent.",
  implements: [Agent],
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

const UsagePriceInput = Schema.Struct({
  id: Schema.String,
  provider_id: Schema.String,
  receiver_id: Schema.String,
  resource_id: Schema.String,
  action: Schema.Literals(["consume"]),
  occurred_at: Schema.String,
  quantity: Norn.decimal(6),
  unit: Schema.String,
  currency: Schema.NullOr(Schema.String),
  billing_currency: Schema.Literal("USD"),
  token_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "{token}" }, additivity: { kind: "additive" } }),
  rate_per_token: Norn.rate(6, { currency: "billing_currency", perUnit: { system: "ucum", code: "{token}" } }),
  billed_amount: Norn.decimal(6, { currency: "billing_currency", additivity: { kind: "additive" } }),
})

export const UsageEvent = Norn.type({
  name: "compute.UsageEvent",
  role: "happening",
  title: "Compute usage event",
  version: "0.1.0",
  dependencies: [{ artifactId: "units.compute.UnitDefinition.entity", version: "0.1.0", kind: "entity" }],
  description:
    "Posted consumption of compute, storage, or AI tokens ({token}, {GPU}.s, GiBy, etc.), implementing core.EconomicEvent with exact VAL-RATE pricing (Rate<USD, {token}> × Quantity<{token}> → Money<USD>).",
  implements: [EconomicEvent],
  properties: Schema.Struct({
    id: Schema.String,
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Norn.ref<Schema.String>("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
    unit: Norn.ref<Schema.String>("ucum.unit_code"),
    currency: Schema.NullOr(Norn.ref<Schema.String>("core.code")),
    billing_currency: Schema.Literal("USD"),
    token_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "{token}" }, additivity: { kind: "additive" } }),
    rate_per_token: Norn.rate(6, { currency: "billing_currency", perUnit: { system: "ucum", code: "{token}" } }),
    billed_amount: Norn.decimal(6, { currency: "billing_currency", additivity: { kind: "additive" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "provider", description: "Compute provider delivering capacity.", target: ComputeAgent, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Customer or workload consuming compute.", target: ComputeAgent, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Compute resource or token pool consumed.", target: TokenPool, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", description: "Optional capacity commitment fulfilled by this usage.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
    { id: "reverses", description: "Prior posted usage event reversed by this correction.", target: "core.EconomicEvent", cardinality: { to: "one", from: "one" } },
  ],
})

export const usageEventComputeUnit = Norn.law("metered_in_compute_unit", {
  on: UsageEvent,
  must: (self) =>
    self.currency === null &&
    ["{token}", "{input_token}", "{output_token}", "{request}", "{vCPU}.s", "{vCPU}.h", "{GPU}.s", "{GPU}.h", "GiBy"].includes(self.unit),
  because: "Compute usage events meter an admitted compute or storage UCUM unit rather than a currency.",
})

// Scoped to `{token}` events so GPU/storage events are not forced to bill per `{token}`.
// Note: `core.EconomicEvent.quantity` carries polymorphic `unitSource: ["unit", "currency"]`
// while `token_quantity` is statically typed as `Quantity<{token}>`, so `VAL-UNIT-1` keeps
// their spaces distinct; host pricing actions (`compute.usage.price`) supply `token_quantity`.
export const usageEventTokenBilledExact = Norn.law("token_usage_billed_exact", {
  on: UsageEvent,
  when: (self) => self.unit === "{token}",
  must: (self) => self.billed_amount === self.rate_per_token * self.token_quantity,
  because: "VAL-RATE: `{token}` usage events satisfy billed_amount == rate_per_token * token_quantity exactly.",
})

export const usageEventComputeUnitHolds = Norn.example("GPU-second usage event satisfies metered_in_compute_unit", {
  of: usageEventComputeUnit,
  given: {
    id: "usage-gpu-001",
    action: "consume",
    occurred_at: "2026-10-26T12:00:00Z",
    quantity: 3600,
    unit: "{GPU}.s",
    currency: null,
    billing_currency: "USD",
    token_quantity: 0,
    rate_per_token: 0,
    billed_amount: 0,
  },
  expect: "holds",
})

export const usageEventMassUnitViolated = Norn.example("mass-unit usage event violates metered_in_compute_unit", {
  of: usageEventComputeUnit,
  given: {
    id: "usage-mass-001",
    action: "consume",
    occurred_at: "2026-10-26T12:00:00Z",
    quantity: 100,
    unit: "g",
    currency: null,
    billing_currency: "USD",
    token_quantity: 0,
    rate_per_token: 0,
    billed_amount: 0,
  },
  expect: "violated",
})

export const usageEventTokenPricingHolds = Norn.example("token usage event with exact Rate<USD,{token}> * Quantity<{token}> holds", {
  of: usageEventTokenBilledExact,
  given: {
    id: "usage-001",
    action: "consume",
    occurred_at: "2026-10-26T12:00:00Z",
    quantity: 1000000,
    unit: "{token}",
    currency: null,
    billing_currency: "USD",
    token_quantity: 1000000,
    rate_per_token: 0.000015,
    billed_amount: 15,
  },
  expect: "holds",
})

export const usageEventMismatchedTotalViolated = Norn.example("token usage event with wrong billed_amount is refused", {
  of: usageEventTokenBilledExact,
  given: {
    id: "usage-002",
    action: "consume",
    occurred_at: "2026-10-26T12:00:00Z",
    quantity: 1000000,
    unit: "{token}",
    currency: null,
    billing_currency: "USD",
    token_quantity: 1000000,
    rate_per_token: 0.000015,
    billed_amount: 14,
  },
  expect: "violated",
})

export const usageEventGpuTokenBillingNotApplicable = Norn.example("GPU-second usage event is unaffected by token_usage_billed_exact", {
  of: usageEventTokenBilledExact,
  given: {
    id: "usage-gpu-002",
    action: "consume",
    occurred_at: "2026-10-26T12:00:00Z",
    quantity: 3600,
    unit: "{GPU}.s",
    currency: null,
    billing_currency: "USD",
    token_quantity: 0,
    rate_per_token: 0,
    billed_amount: 12,
  },
  expect: "not_applicable",
})

export const PriceComputeUsage = Norn.action({
  id: "compute.usage.price",
  title: "Price a compute token usage event",
  version: "0.1.0",
  dependencies: [{ artifactId: "units.compute.UnitDefinition.entity", version: "0.1.0", kind: "entity" }],
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: UsagePriceInput,
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "provider", type: ComputeAgent, key: { property: { from: "input", property: "provider_id" } } },
    { name: "receiver", type: ComputeAgent, key: { property: { from: "input", property: "receiver_id" } } },
    { name: "resource", type: TokenPool, key: { property: { from: "input", property: "resource_id" } } },
  ],
  derive: [
    {
      field: "billed_amount",
      inputRequired: false,
      expression: Norn.expression(UsagePriceInput, (input) => input.rate_per_token * input.token_quantity),
    },
  ],
  preconditions: [Norn.expression(UsagePriceInput, (input) => input.token_quantity > 0)],
  createLinks: [
    { link: "provider", targetType: ComputeAgent, targetIdField: "provider_id" },
    { link: "receiver", targetType: ComputeAgent, targetIdField: "receiver_id" },
    { link: "resource", targetType: TokenPool, targetIdField: "resource_id" },
  ],
  writes: [{ type: UsageEvent, change: "created" }],
  effect: { kind: "upsert_input_object", type: UsageEvent, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const QuotaHold = Norn.type({
  name: "compute.QuotaHold",
  role: "happening",
  title: "Compute quota hold",
  version: "0.1.0",
  description:
    "Rate-limit, budget lock, or GPU reservation held against a compute resource (implements core.Hold with LIFE-SOUND-1..8 lifecycle).",
  implements: [Hold],
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["open", "released", "consumed", "expired"]),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "semi_additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    expires_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
    quota_scope: Schema.Literals(["rate_limit", "budget_lock", "gpu_reservation"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "open",
  },
  links: [
    { id: "resource", description: "Compute resource or token pool locked by this hold.", target: TokenPool, cardinality: { to: "one", from: "many", required: true } },
    { id: "holder", description: "Agent or workload holding the quota reservation.", target: ComputeAgent, cardinality: { to: "one", from: "many", required: true } },
    { id: "secures", description: "Optional capacity commitment secured by this hold.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["released", "consumed", "expired"],
      immutable: ["released", "consumed", "expired"],
      endings: {
        consumed: "completed",
        released: "compensated",
        expired: "cancelled",
      },
      transitions: [
        { id: "consume", from: ["open"], to: "consumed" },
        { id: "release", from: ["open"], to: "released" },
        { id: "expire", from: ["open"], to: "expired" },
      ],
    },
  ],
})

export const CapacityCommitment = Norn.type({
  name: "compute.CapacityCommitment",
  role: "happening",
  title: "Compute capacity commitment",
  version: "0.1.0",
  description:
    "Reserved GPU hours, monthly token plan, or storage tier commitment (implements core.Commitment with LIFE-SOUND-1..8 lifecycle).",
  implements: [Commitment],
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    plan_kind: Schema.Literals(["reserved_gpu_hours", "monthly_token_plan", "storage_tier"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "requested",
  },
  links: [
    { id: "provider", description: "Cloud or AI provider promising capacity.", target: ComputeAgent, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Customer receiving reserved capacity.", target: ComputeAgent, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Compute resource promised by this commitment.", target: "core.Resource", cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["requested"],
      terminal: ["fulfilled", "cancelled", "breached"],
      immutable: ["fulfilled", "cancelled", "breached"],
      endings: {
        fulfilled: "completed",
        cancelled: "cancelled",
        breached: "breached_visible",
      },
      transitions: [
        { id: "promise", from: ["requested"], to: "promised" },
        { id: "fulfil", from: ["promised"], to: "fulfilled" },
        { id: "cancel", from: ["requested", "promised"], to: "cancelled" },
        { id: "breach", from: ["promised"], to: "breached" },
      ],
    },
  ],
})

export const RegisterComputeUnit = Norn.action({
  id: "units.compute.register",
  title: "Register a compute UCUM unit definition",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    code: Schema.Literals([
      "ms", "us", "ns", "bit", "By", "KiBy", "MiBy", "GiBy", "TiBy",
      "kBy", "MBy", "GBy", "TBy", "{token}", "{input_token}", "{output_token}",
      "{request}", "{vCPU}.s", "{vCPU}.h", "{GPU}.s", "{GPU}.h", "{seat}",
    ]),
    dimension: Schema.Literals(["time", "information", "compute_count", "compute_time"]),
    base_code: Schema.Literals([
      "s", "By", "{token}", "{input_token}", "{output_token}",
      "{request}", "{vCPU}.s", "{GPU}.s", "{seat}",
    ]),
    numerator: Schema.Int,
    denominator: Schema.Int,
    standard: Schema.Literals(["ucum-2.2", "iec-80000-13"]),
  }),
  output: Schema.Struct({ code: Schema.String }),
  writes: [{ type: ComputeUnitDefinition, change: "created" }],
  effect: { kind: "upsert_input_object", type: ComputeUnitDefinition, objectIdField: "code", writeMode: { kind: "create_only" } },
})

export const RegisterPhysicalUnit = Norn.action({
  id: "units.physical.register",
  title: "Register a physical UCUM unit definition",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    code: Schema.Literals(["m", "cm", "mm", "km", "m2", "L", "mL", "m3", "J", "kW.h"]),
    dimension: Schema.Literals(["length", "area", "volume", "energy"]),
    base_code: Schema.Literals(["m", "m2", "m3", "J"]),
    numerator: Schema.Int,
    denominator: Schema.Int,
    standard: Schema.Literals(["ucum-2.2", "iso-80000"]),
  }),
  output: Schema.Struct({ code: Schema.String }),
  writes: [{ type: PhysicalUnitDefinition, change: "created" }],
  effect: { kind: "upsert_input_object", type: PhysicalUnitDefinition, objectIdField: "code", writeMode: { kind: "create_only" } },
})

export const CreateTokenPool = Norn.action({
  id: "compute.token_pool.create",
  title: "Create a compute token pool",
  version: "0.1.0",
  dependencies: [{ artifactId: "units.compute.UnitDefinition.entity", version: "0.1.0", kind: "entity" }],
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    id: Schema.String,
    resource_code: Schema.String,
    quantity: Norn.decimal(6),
    unit: Schema.String,
    currency: Schema.NullOr(Schema.String),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    available: Norn.decimal(0, { unit: { system: "ucum", code: "{token}" }, additivity: { kind: "semi_additive" } }),
  }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: TokenPool, change: "created" }],
  effect: { kind: "upsert_input_object", type: TokenPool, objectIdField: "id", writeMode: { kind: "create_only" } },
})

const PlaceQuotaHoldInput = Schema.Struct({
  id: Schema.String,
  resource_id: Schema.String,
  holder_id: Schema.String,
  quantity: Norn.decimal(6, { unit: { system: "ucum", code: "{token}" } }),
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
  expires_at: Schema.NullOr(Norn.timestamp("utc")),
  quota_scope: Schema.Literals(["rate_limit", "budget_lock", "gpu_reservation"]),
})

export const PlaceQuotaHold = Norn.action({
  id: "compute.quota_hold.place",
  title: "Place a compute quota hold",
  version: "0.1.0",
  dependencies: [{ artifactId: "units.compute.UnitDefinition.entity", version: "0.1.0", kind: "entity" }],
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: PlaceQuotaHoldInput,
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "resource", type: TokenPool, key: { property: { from: "input", property: "resource_id" } } },
    { name: "holder", type: ComputeAgent, key: { property: { from: "input", property: "holder_id" } } },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        input: PlaceQuotaHoldInput,
        slice: Schema.Struct({
          resource: Schema.Struct({
            unit: Schema.String,
            available: Norn.decimal(0, { unit: { system: "ucum", code: "{token}" } }),
          }),
        }),
      }),
      (ctx) =>
        ctx.input.quantity > 0 &&
        ctx.input.currency === null &&
        ctx.input.unit !== null &&
        (ctx.input.unit ?? "") === ctx.slice.resource.unit &&
        ctx.input.quantity <= ctx.slice.resource.available,
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "resource", targetType: TokenPool, targetIdField: "resource_id" },
    { link: "holder", targetType: ComputeAgent, targetIdField: "holder_id" },
  ],
  writes: [{ type: QuotaHold, change: "created" }],
  effect: { kind: "upsert_input_object", type: QuotaHold, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const ConsumeQuotaHold = Norn.action({
  id: "compute.quota_hold.consume",
  title: "Consume a compute quota hold",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: QuotaHold, change: "modified" }],
  effect: { kind: "fire_transition", type: QuotaHold, objectIdField: "id", lifecycle: "status", transition: "consume" },
})

export const ReleaseQuotaHold = Norn.action({
  id: "compute.quota_hold.release",
  title: "Release a compute quota hold",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: QuotaHold, change: "modified" }],
  effect: { kind: "fire_transition", type: QuotaHold, objectIdField: "id", lifecycle: "status", transition: "release" },
})

export const ExpireQuotaHold = Norn.action({
  id: "compute.quota_hold.expire",
  title: "Expire a compute quota hold",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: QuotaHold, change: "modified" }],
  effect: { kind: "fire_transition", type: QuotaHold, objectIdField: "id", lifecycle: "status", transition: "expire" },
})

export const CreateCapacityCommitment = Norn.action({
  id: "compute.capacity_commitment.create",
  title: "Request a compute capacity commitment",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    id: Schema.String,
    provider_id: Schema.String,
    receiver_id: Schema.String,
    due_at: Norn.timestamp("utc"),
    quantity: Norn.decimal(6),
    unit: Schema.NullOr(Schema.String),
    currency: Schema.NullOr(Schema.String),
    plan_kind: Schema.Literals(["reserved_gpu_hours", "monthly_token_plan", "storage_tier"]),
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "provider", type: ComputeAgent, key: { property: { from: "input", property: "provider_id" } } },
    { name: "receiver", type: ComputeAgent, key: { property: { from: "input", property: "receiver_id" } } },
  ],
  createLinks: [
    { link: "provider", targetType: ComputeAgent, targetIdField: "provider_id" },
    { link: "receiver", targetType: ComputeAgent, targetIdField: "receiver_id" },
  ],
  writes: [{ type: CapacityCommitment, change: "created" }],
  effect: { kind: "upsert_input_object", type: CapacityCommitment, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const PromiseCapacity = Norn.action({
  id: "compute.capacity_commitment.promise",
  title: "Promise a compute capacity commitment",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CapacityCommitment, change: "modified" }],
  effect: { kind: "fire_transition", type: CapacityCommitment, objectIdField: "id", lifecycle: "status", transition: "promise" },
})

export const FulfilCapacity = Norn.action({
  id: "compute.capacity_commitment.fulfil",
  title: "Fulfil a compute capacity commitment",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CapacityCommitment, change: "modified" }],
  effect: { kind: "fire_transition", type: CapacityCommitment, objectIdField: "id", lifecycle: "status", transition: "fulfil" },
})

export const CancelCapacity = Norn.action({
  id: "compute.capacity_commitment.cancel",
  title: "Cancel a compute capacity commitment",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CapacityCommitment, change: "modified" }],
  effect: { kind: "fire_transition", type: CapacityCommitment, objectIdField: "id", lifecycle: "status", transition: "cancel" },
})

export const BreachCapacity = Norn.action({
  id: "compute.capacity_commitment.breach",
  title: "Record a breach of a compute capacity commitment",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CapacityCommitment, change: "modified" }],
  effect: { kind: "fire_transition", type: CapacityCommitment, objectIdField: "id", lifecycle: "status", transition: "breach" },
})
