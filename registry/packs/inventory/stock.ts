/**
 * Governed Inventory & Warehouse Domain Pack — Conserved Stock Balances,
 * Stock Reservations (Holds), Immutable Stock Movements, and Physical Cycle Counts.
 *
 * 100% jurisdiction-agnostic. Implements core REA interfaces (`core.Resource`,
 * `core.Hold`, `core.EconomicEvent`) and `core.TimeInterval` with exact decimals.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { EconomicEvent, Hold, Resource } from "../../core/src/rea.ts"
import { TimeInterval } from "../../core/src/time.ts"
import { BinLocation, Item, LotBatch, Warehouse } from "./warehouse.ts"

// ---------------------------------------------------------------- Conserved Stock Balance

export const StockBalance = Norn.type({
  name: "inventory.StockBalance",
  role: "thing",
  title: "Stock balance",
  version: "0.1.0",
  description: "Conserved stock on hand and reserved quantity at one warehouse, optional bin, and optional lot (implements core.Resource).",
  implements: [Resource],
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity"),
    reserved_quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    {
      id: "owner",
      description: "Warehouse that owns this stock balance.",
      target: Warehouse,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "custodian",
      description: "Optional warehouse holding custody of this stock balance.",
      target: Warehouse,
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "item",
      description: "Catalog item represented by this stock balance.",
      target: Item,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "bin",
      description: "Optional bin location storing this stock balance.",
      target: BinLocation,
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "lot",
      description: "Optional lot or batch for traceable stock.",
      target: LotBatch,
      cardinality: { to: "one", from: "many" },
    },
  ],
})

// ---------------------------------------------------------------- Stock Reservation (Hold)

export const StockReservation = Norn.type({
  name: "inventory.StockReservation",
  role: "happening",
  title: "Stock reservation",
  version: "0.1.0",
  description: "Conserved stock hold against a StockBalance with explicit consume, release, and 3-day expiry transitions (implements core.Hold).",
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
    {
      id: "resource",
      description: "Stock balance reserved by this hold.",
      target: StockBalance,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "holder",
      description: "Warehouse holding this reservation.",
      target: Warehouse,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "secures",
      description: "Optional commitment secured by this stock reservation.",
      target: "core.Commitment",
      cardinality: { to: "one", from: "many" },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["released", "consumed", "expired"],
      transitions: [
        { id: "consume", from: ["open"], to: "consumed" },
        { id: "release", from: ["open"], to: "released" },
        { id: "expire", from: ["open"], to: "expired", after: { days: 3 } },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Immutable Stock Movement

export const StockMovement = Norn.type({
  name: "inventory.StockMovement",
  role: "happening",
  title: "Stock movement",
  version: "0.1.0",
  description: "Create-only immutable stock ledger event recording receipt, issue, transfer, adjustment, or scrap (implements core.EconomicEvent).",
  implements: [EconomicEvent],
  properties: Schema.Struct({
    id: Schema.String,
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    movement_kind: Schema.Literals(["receipt", "issue", "transfer", "adjustment", "scrap"]),
    occurred_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    unit_cost: Norn.ref("core.money_amount"),
    currency: Schema.NullOr(Norn.ref("core.code")),
    reference_code: Norn.ref("core.code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    action: "transfer_custody",
  },
  links: [
    {
      id: "provider",
      description: "Warehouse supplying or initiating the stock movement.",
      target: Warehouse,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "receiver",
      description: "Warehouse receiving or recording the stock movement.",
      target: Warehouse,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "resource",
      description: "Stock balance affected by this movement.",
      target: StockBalance,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "fulfils",
      description: "Optional commitment fulfilled by this stock movement.",
      target: "core.Commitment",
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "reverses",
      description: "Prior stock movement reversed by this compensating movement.",
      target: "inventory.StockMovement",
      cardinality: { to: "one", from: "one" },
    },
  ],
})

// ---------------------------------------------------------------- Physical Cycle Count

const CycleCountStatus = Schema.Literals(["draft", "submitted", "approved", "posted"])

export const CycleCount = Norn.type({
  name: "inventory.CycleCount",
  role: "happening",
  title: "Cycle count",
  version: "0.1.0",
  description: "Physical inventory count conducted over a half-open UTC TimeInterval with conserved variance and immutable approval/posting.",
  implements: [TimeInterval],
  properties: Schema.Struct({
    id: Schema.String,
    starts_at: Norn.ref("core.time.instant"),
    ends_at: Norn.ref("core.time.instant"),
    timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
    expected_quantity: Norn.ref("core.quantity"),
    counted_quantity: Norn.ref("core.quantity"),
    variance_quantity: Norn.ref("core.quantity"),
    status: CycleCountStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    {
      id: "warehouse",
      description: "Warehouse in which the cycle count is conducted.",
      target: Warehouse,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "stock_balance",
      description: "Stock balance verified by this cycle count.",
      target: StockBalance,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["posted"],
      immutable: ["approved", "posted"],
      transitions: [
        { id: "submit", from: ["draft"], to: "submitted" },
        { id: "approve", from: ["submitted"], to: "approved" },
        { id: "post", from: ["approved"], to: "posted" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Governed Laws & LAW-EXAMPLE-1 Cases

export const StockBalanceNonNegativeAndReservedBounded = Norn.law({
  id: "inventory.StockBalance.non_negative_and_reserved_bounded.law",
  title: "Stock balance carries a unit, is non-negative, and reserved quantity never exceeds on-hand quantity",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.StockBalance.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.StockBalance.non_negative_and_reserved_bounded",
    owner: "inventory",
    severity: "error",
    rationale: "Conserved stock balances must carry an explicit UCUM unit, stay non-negative, and never reserve more than on-hand stock.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/stock.ts" },
    subject: "inventory.StockBalance",
    scope: { root: { kind: "entity", id: "inventory.StockBalance" }, objectTypes: ["inventory.StockBalance"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.unit != null and self.quantity >= 0 and self.reserved_quantity >= 0 and self.reserved_quantity <= self.quantity",
    },
  },
})

export const StockBalanceValidHolds = Norn.example("stock balance with unit and reserved within on-hand holds", {
  of: StockBalanceNonNegativeAndReservedBounded,
  given: {
    id: "sb_1",
    resource_code: "SKU-BEARING-01@WH-CENTRAL-01",
    quantity: 100,
    reserved_quantity: 25,
    unit: "{each}",
    currency: null,
    valuation_method: "fifo",
  },
  expect: "holds",
})

export const StockBalanceNegativeQuantityViolated = Norn.example("negative stock balance quantity is refused", {
  of: StockBalanceNonNegativeAndReservedBounded,
  given: {
    id: "sb_neg",
    resource_code: "SKU-BEARING-01@WH-CENTRAL-01",
    quantity: -1,
    reserved_quantity: 0,
    unit: "{each}",
    currency: null,
    valuation_method: "fifo",
  },
  expect: "violated",
})

export const StockBalanceOverReservedViolated = Norn.example("reserved quantity exceeding stock balance is refused", {
  of: StockBalanceNonNegativeAndReservedBounded,
  given: {
    id: "sb_over",
    resource_code: "SKU-BEARING-01@WH-CENTRAL-01",
    quantity: 10,
    reserved_quantity: 15,
    unit: "{each}",
    currency: null,
    valuation_method: "fifo",
  },
  expect: "violated",
})

export const StockBalanceUnitlessViolated = Norn.example("unitless stock balance is refused", {
  of: StockBalanceNonNegativeAndReservedBounded,
  given: {
    id: "sb_no_unit",
    resource_code: "SKU-BEARING-01@WH-CENTRAL-01",
    quantity: 10,
    reserved_quantity: 0,
    unit: null,
    currency: null,
    valuation_method: "fifo",
  },
  expect: "violated",
})

export const StockReservationPositiveQuantity = Norn.law({
  id: "inventory.StockReservation.positive_quantity.law",
  title: "Stock reservation carries a unit and strictly positive quantity",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.StockReservation.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.StockReservation.positive_quantity",
    owner: "inventory",
    severity: "error",
    rationale: "A stock hold must carry an explicit UCUM unit and reserve a strictly positive quantity.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/stock.ts" },
    subject: "inventory.StockReservation",
    scope: { root: { kind: "entity", id: "inventory.StockReservation" }, objectTypes: ["inventory.StockReservation"] },
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.unit != null and self.quantity > 0" },
  },
})

export const StockReservationPositiveHolds = Norn.example("positive stock reservation with unit holds", {
  of: StockReservationPositiveQuantity,
  given: {
    id: "res_1",
    status: "open",
    quantity: 5,
    unit: "{each}",
    currency: null,
    expires_at: "2026-12-01T00:00:00Z",
  },
  expect: "holds",
})

export const StockReservationZeroViolated = Norn.example("zero stock reservation is refused", {
  of: StockReservationPositiveQuantity,
  given: {
    id: "res_0",
    status: "open",
    quantity: 0,
    unit: "{each}",
    currency: null,
    expires_at: "2026-12-01T00:00:00Z",
  },
  expect: "violated",
})

export const StockReservationUnitlessViolated = Norn.example("unitless stock reservation is refused", {
  of: StockReservationPositiveQuantity,
  given: {
    id: "res_no_unit",
    status: "open",
    quantity: 5,
    unit: null,
    currency: null,
    expires_at: "2026-12-01T00:00:00Z",
  },
  expect: "violated",
})

export const StockMovementPositiveQuantity = Norn.law({
  id: "inventory.StockMovement.positive_quantity.law",
  title: "Stock movement carries a unit, strictly positive quantity, and non-negative unit cost",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.StockMovement.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.StockMovement.positive_quantity",
    owner: "inventory",
    severity: "error",
    rationale: "Every posted stock movement carries an explicit UCUM unit and transfers a strictly positive physical quantity at a non-negative unit cost.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/stock.ts" },
    subject: "inventory.StockMovement",
    scope: { root: { kind: "entity", id: "inventory.StockMovement" }, objectTypes: ["inventory.StockMovement"] },
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.unit != null and self.quantity > 0 and self.unit_cost >= 0" },
  },
})

export const StockMovementValidHolds = Norn.example("positive receipt movement with unit and non-negative unit cost holds", {
  of: StockMovementPositiveQuantity,
  given: {
    id: "mov_1",
    action: "transfer_custody",
    movement_kind: "receipt",
    occurred_at: "2026-11-23T10:00:00Z",
    quantity: 25,
    unit: "{each}",
    unit_cost: 14.5,
    currency: "USD",
    reference_code: "GRN-2026-0001",
  },
  expect: "holds",
})

export const StockMovementZeroQtyViolated = Norn.example("zero-quantity stock movement is refused", {
  of: StockMovementPositiveQuantity,
  given: {
    id: "mov_zero",
    action: "transfer_custody",
    movement_kind: "receipt",
    occurred_at: "2026-11-23T10:00:00Z",
    quantity: 0,
    unit: "{each}",
    unit_cost: 14.5,
    currency: "USD",
    reference_code: "GRN-2026-0002",
  },
  expect: "violated",
})

export const StockMovementNegativeCostViolated = Norn.example("negative-cost stock movement is refused", {
  of: StockMovementPositiveQuantity,
  given: {
    id: "mov_neg_cost",
    action: "transfer_custody",
    movement_kind: "receipt",
    occurred_at: "2026-11-23T10:00:00Z",
    quantity: 10,
    unit: "{each}",
    unit_cost: -1,
    currency: "USD",
    reference_code: "GRN-2026-0003",
  },
  expect: "violated",
})

export const StockMovementUnitlessViolated = Norn.example("unitless stock movement is refused", {
  of: StockMovementPositiveQuantity,
  given: {
    id: "mov_no_unit",
    action: "transfer_custody",
    movement_kind: "receipt",
    occurred_at: "2026-11-23T10:00:00Z",
    quantity: 10,
    unit: null,
    unit_cost: 14.5,
    currency: "USD",
    reference_code: "GRN-2026-0004",
  },
  expect: "violated",
})

export const CycleCountVarianceConservation = Norn.law({
  id: "inventory.CycleCount.variance_conservation.law",
  title: "Cycle count variance equals counted minus expected quantity",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.CycleCount.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.CycleCount.variance_conservation",
    owner: "inventory",
    severity: "error",
    rationale: "Expected and counted physical quantities are non-negative and variance must equal counted_quantity minus expected_quantity.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/stock.ts" },
    subject: "inventory.CycleCount",
    scope: { root: { kind: "entity", id: "inventory.CycleCount" }, objectTypes: ["inventory.CycleCount"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.expected_quantity >= 0 and self.counted_quantity >= 0 and self.variance_quantity == self.counted_quantity - self.expected_quantity",
    },
  },
})

export const CycleCountConservedHolds = Norn.example("cycle count with conserved shrinkage variance holds", {
  of: CycleCountVarianceConservation,
  given: {
    id: "cc_1",
    starts_at: "2026-11-23T08:00:00Z",
    ends_at: "2026-11-23T10:00:00Z",
    timezone: "UTC",
    expected_quantity: 50,
    counted_quantity: 47,
    variance_quantity: -3,
    status: "draft",
  },
  expect: "holds",
})

export const CycleCountMismatchedVarianceViolated = Norn.example("cycle count with inconsistent variance is refused", {
  of: CycleCountVarianceConservation,
  given: {
    id: "cc_bad_var",
    starts_at: "2026-11-23T08:00:00Z",
    ends_at: "2026-11-23T10:00:00Z",
    timezone: "UTC",
    expected_quantity: 50,
    counted_quantity: 47,
    variance_quantity: 2,
    status: "draft",
  },
  expect: "violated",
})

export const CycleCountNegativeCountedViolated = Norn.example("cycle count with negative counted quantity is refused", {
  of: CycleCountVarianceConservation,
  given: {
    id: "cc_neg",
    starts_at: "2026-11-23T08:00:00Z",
    ends_at: "2026-11-23T10:00:00Z",
    timezone: "UTC",
    expected_quantity: 50,
    counted_quantity: -1,
    variance_quantity: -51,
    status: "draft",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Actions

const MovementPreconditionContext = Schema.Struct({
  input: Schema.Struct({
    movement_kind: Schema.String,
    quantity: Norn.decimal(6),
    unit: Schema.String,
    unit_cost: Norn.decimal(6),
  }),
  slice: Schema.Struct({
    provider: Schema.Struct({ status: Schema.String }),
    receiver: Schema.Struct({ status: Schema.String }),
    resource: Schema.Struct({
      quantity: Norn.decimal(6),
      reserved_quantity: Norn.decimal(6),
      unit: Schema.NullOr(Schema.String),
    }),
    bins: Schema.Array(Schema.Struct({ status: Schema.String })),
    lots: Schema.Array(Schema.Struct({ status: Schema.String })),
  }),
})

export const RecordMovement = Norn.action({
  id: "inventory.movement.record",
  title: "Record immutable stock movement",
  version: "0.1.0",
  description: "Post a create-only StockMovement verifying active warehouses, unit match, available unreserved stock on outflows, and active bin/lot state.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    id: Schema.String,
    action: Schema.String,
    movement_kind: Schema.String,
    occurred_at: Schema.String,
    quantity: Norn.decimal(6),
    unit: Schema.String,
    unit_cost: Norn.decimal(6),
    currency: Schema.NullOr(Schema.String),
    reference_code: Schema.String,
    providerId: Schema.String,
    receiverId: Schema.String,
    resourceId: Schema.String,
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "provider", type: Warehouse, key: { property: { from: "input", property: "providerId" } } },
    { name: "receiver", type: Warehouse, key: { property: { from: "input", property: "receiverId" } } },
    { name: "resource", type: StockBalance, key: { property: { from: "input", property: "resourceId" } } },
    { name: "bins", type: BinLocation, key: { link: { from: "resource", link: "bin" } } },
    { name: "lots", type: LotBatch, key: { link: { from: "resource", link: "lot" } } },
  ],
  preconditions: [
    Norn.expression(
      MovementPreconditionContext,
      (ctx) => ctx.slice.provider.status === "active" && ctx.slice.receiver.status === "active",
    ),
    Norn.expression(
      MovementPreconditionContext,
      (ctx) => ctx.input.quantity > 0 && ctx.input.unit_cost >= 0,
    ),
    Norn.expression(
      MovementPreconditionContext,
      (ctx) => ctx.slice.resource.unit !== null && ctx.input.unit === (ctx.slice.resource.unit ?? ""),
    ),
    Norn.expression(
      MovementPreconditionContext,
      (ctx) =>
        (ctx.input.movement_kind !== "issue" &&
          ctx.input.movement_kind !== "transfer" &&
          ctx.input.movement_kind !== "scrap") ||
        ctx.slice.resource.quantity - ctx.slice.resource.reserved_quantity >= ctx.input.quantity,
    ),
    Norn.expression(
      MovementPreconditionContext,
      (ctx) => ctx.slice.bins.every((bin) => bin.status === "active"),
    ),
    Norn.expression(
      MovementPreconditionContext,
      (ctx) => ctx.slice.lots.every((lot) => lot.status === "active"),
    ),
  ],
  createLinks: [
    { link: "provider", targetType: "inventory.Warehouse", targetIdField: "providerId" },
    { link: "receiver", targetType: "inventory.Warehouse", targetIdField: "receiverId" },
    { link: "resource", targetType: "inventory.StockBalance", targetIdField: "resourceId" },
  ],
  writes: [{ type: StockMovement, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: StockMovement,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

const ReservationCreatePreconditionContext = Schema.Struct({
  input: Schema.Struct({
    quantity: Norn.decimal(6),
    unit: Schema.String,
  }),
  slice: Schema.Struct({
    warehouse: Schema.Struct({ status: Schema.String }),
    resource: Schema.Struct({
      quantity: Norn.decimal(6),
      reserved_quantity: Norn.decimal(6),
      unit: Schema.NullOr(Schema.String),
    }),
    bins: Schema.Array(Schema.Struct({ status: Schema.String })),
    lots: Schema.Array(Schema.Struct({ status: Schema.String })),
  }),
})

export const CreateReservation = Norn.action({
  id: "inventory.reservation.create",
  title: "Create stock reservation",
  version: "0.1.0",
  description: "Create a conserved stock hold against an active warehouse, balance, bin, and lot with exact unit match and available unreserved stock.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    id: Schema.String,
    quantity: Norn.decimal(6),
    unit: Schema.String,
    currency: Schema.NullOr(Schema.String),
    expires_at: Schema.NullOr(Schema.String),
    resourceId: Schema.String,
    holderId: Schema.String,
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "holderId" } } },
    { name: "resource", type: StockBalance, key: { property: { from: "input", property: "resourceId" } } },
    { name: "bins", type: BinLocation, key: { link: { from: "resource", link: "bin" } } },
    { name: "lots", type: LotBatch, key: { link: { from: "resource", link: "lot" } } },
  ],
  preconditions: [
    Norn.expression(
      ReservationCreatePreconditionContext,
      (ctx) => ctx.slice.warehouse.status === "active",
    ),
    Norn.expression(
      ReservationCreatePreconditionContext,
      (ctx) => ctx.slice.resource.unit !== null && ctx.input.unit === (ctx.slice.resource.unit ?? ""),
    ),
    Norn.expression(
      ReservationCreatePreconditionContext,
      (ctx) =>
        ctx.input.quantity > 0 &&
        ctx.slice.resource.quantity - ctx.slice.resource.reserved_quantity >= ctx.input.quantity,
    ),
    Norn.expression(
      ReservationCreatePreconditionContext,
      (ctx) => ctx.slice.bins.every((bin) => bin.status === "active"),
    ),
    Norn.expression(
      ReservationCreatePreconditionContext,
      (ctx) => ctx.slice.lots.every((lot) => lot.status === "active"),
    ),
  ],
  createLinks: [
    { link: "resource", targetType: "inventory.StockBalance", targetIdField: "resourceId" },
    { link: "holder", targetType: "inventory.Warehouse", targetIdField: "holderId" },
  ],
  writes: [{ type: StockReservation, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: StockReservation,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

const ReservationTransitionInput = Schema.Struct({
  reservationId: Schema.String,
})

const ReservationTransitionOutput = Schema.Struct({
  reservationId: Schema.String,
  status: Schema.String,
})

export const ConsumeReservation = Norn.action({
  id: "inventory.reservation.consume",
  title: "Consume stock reservation",
  version: "0.1.0",
  description: "Consume an open stock reservation when the reserved quantity is fulfilled.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: ReservationTransitionInput,
  output: ReservationTransitionOutput,
  writes: [{ type: StockReservation, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: StockReservation,
    objectIdField: "reservationId",
    lifecycle: "status",
    transition: "consume",
  },
})

export const ReleaseReservation = Norn.action({
  id: "inventory.reservation.release",
  title: "Release stock reservation",
  version: "0.1.0",
  description: "Release an open stock reservation back to available inventory.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: ReservationTransitionInput,
  output: ReservationTransitionOutput,
  writes: [{ type: StockReservation, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: StockReservation,
    objectIdField: "reservationId",
    lifecycle: "status",
    transition: "release",
  },
})

const CycleCountTransitionInput = Schema.Struct({
  cycleCountId: Schema.String,
})

const CycleCountTransitionOutput = Schema.Struct({
  cycleCountId: Schema.String,
  status: Schema.String,
})

export const SubmitCycleCount = Norn.action({
  id: "inventory.cycle_count.submit",
  title: "Submit cycle count",
  version: "0.1.0",
  description: "Submit a draft physical cycle count for inventory manager approval.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: CycleCountTransitionInput,
  output: CycleCountTransitionOutput,
  writes: [{ type: CycleCount, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: CycleCount,
    objectIdField: "cycleCountId",
    lifecycle: "status",
    transition: "submit",
  },
})

export const ApproveCycleCount = Norn.action({
  id: "inventory.cycle_count.approve",
  title: "Approve cycle count",
  version: "0.1.0",
  description: "Approve a submitted physical cycle count under inventory manager authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: CycleCountTransitionInput,
  output: CycleCountTransitionOutput,
  writes: [{ type: CycleCount, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: CycleCount,
    objectIdField: "cycleCountId",
    lifecycle: "status",
    transition: "approve",
  },
})

export const PostCycleCount = Norn.action({
  id: "inventory.cycle_count.post",
  title: "Post cycle count",
  version: "0.1.0",
  description: "Post an approved cycle count into terminal immutable posted state.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: CycleCountTransitionInput,
  output: CycleCountTransitionOutput,
  writes: [{ type: CycleCount, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: CycleCount,
    objectIdField: "cycleCountId",
    lifecycle: "status",
    transition: "post",
  },
})
