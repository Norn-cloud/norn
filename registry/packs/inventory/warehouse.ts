/**
 * Governed Inventory & Warehouse Domain Pack — Facilities, Catalog & Traceable Lots.
 *
 * 100% jurisdiction-agnostic: taxes, customs tariffs, and localized labels live
 * in separate localization packs. Built directly on Norn Core REA (`core.Agent`).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent } from "../../core/src/rea.ts"

// ---------------------------------------------------------------- Warehouses & Bins

export const Warehouse = Norn.type({
  name: "inventory.Warehouse",
  role: "thing",
  title: "Warehouse",
  version: "0.1.0",
  description: "A warehouse, depot, or storage facility acting as an organizational REA agent (implements core.Agent).",
  implements: [Agent],
  properties: Schema.Struct({
    id: Schema.String,
    warehouse_code: Norn.ref("core.code"),
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
    status: Schema.Literals(["active", "frozen", "closed"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    kind: "organization",
    status: "active",
  },
  links: [
    {
      id: "parent",
      description: "Optional parent warehouse or facility in the storage hierarchy (strictly acyclic).",
      target: "inventory.Warehouse",
      cardinality: { to: "one", from: "many" },
      acyclic: true,
    },
  ],
})

export const BinLocation = Norn.type({
  name: "inventory.BinLocation",
  role: "thing",
  title: "Bin location",
  version: "0.1.0",
  description: "A physical storage bin or rack position inside a warehouse with exact capacity in UCUM units.",
  properties: Schema.Struct({
    id: Schema.String,
    bin_code: Norn.ref("core.code"),
    zone: Norn.ref("core.code"),
    max_capacity: Norn.ref("core.quantity"),
    unit: Norn.ref("ucum.unit_code"),
    status: Schema.Literals(["active", "quarantined", "inactive"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "active",
  },
  links: [
    {
      id: "warehouse",
      description: "Warehouse containing this storage bin.",
      target: Warehouse,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

// ---------------------------------------------------------------- Catalog Items & Lots

export const Item = Norn.type({
  name: "inventory.Item",
  role: "thing",
  title: "Inventory item",
  version: "0.1.0",
  description: "A catalog SKU with valuation method, UCUM base unit, lot-tracking policy, and reorder threshold. Catalog metadata is never stock.",
  properties: Schema.Struct({
    id: Schema.String,
    sku: Norn.ref("core.code"),
    gtin: Schema.NullOr(Norn.ref("gs1.gtin")),
    display_name: Norn.ref("core.display_name"),
    base_unit: Norn.ref("ucum.unit_code"),
    valuation_method: Schema.Literals(["fifo", "moving_average"]),
    track_lot: Schema.Boolean,
    reorder_point: Norn.decimal(6),
    status: Schema.Literals(["active", "discontinued"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    valuation_method: "fifo",
    track_lot: false,
    status: "active",
  },
})

const LotBatchStatus = Schema.Literals(["active", "quarantined", "expired", "recalled"])

export const LotBatch = Norn.type({
  name: "inventory.LotBatch",
  role: "thing",
  title: "Lot or batch",
  version: "0.1.0",
  description: "A traceable production lot or batch for an inventory item with quarantine, expiry, and immutable recall lifecycle.",
  properties: Schema.Struct({
    id: Schema.String,
    lot_code: Norn.ref("core.code"),
    manufactured_on: Norn.date(),
    expires_on: Schema.NullOr(Norn.date()),
    status: LotBatchStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "active",
  },
  links: [
    {
      id: "item",
      description: "Catalog item tracked by this lot or batch.",
      target: Item,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["active"],
      terminal: ["recalled"],
      immutable: ["recalled"],
      transitions: [
        { id: "quarantine", from: ["active"], to: "quarantined" },
        { id: "release_quarantine", from: ["quarantined"], to: "active" },
        { id: "expire", from: ["active", "quarantined"], to: "expired" },
        { id: "recall", from: ["active", "quarantined", "expired"], to: "recalled" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Governed Laws & LAW-EXAMPLE-1 Cases

export const WarehouseKindIsOrganization = Norn.law({
  id: "inventory.Warehouse.kind_is_organization.law",
  title: "Warehouse agent kind is always organization",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.Warehouse.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.Warehouse.kind_is_organization",
    owner: "inventory",
    severity: "error",
    rationale: "A warehouse or storage facility is an organizational agent, never a natural person or software system.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/warehouse.ts" },
    subject: "inventory.Warehouse",
    scope: { root: { kind: "entity", id: "inventory.Warehouse" }, objectTypes: ["inventory.Warehouse"] },
    modality: { kind: "always" },
    response: { kind: "predicate", expression: 'self.kind == "organization"' },
  },
})

export const WarehouseOrganizationHolds = Norn.example("organization warehouse holds", {
  of: WarehouseKindIsOrganization,
  given: {
    id: "wh_central",
    warehouse_code: "WH-CENTRAL-01",
    display_name: "Central Distribution Center",
    kind: "organization",
    status: "active",
  },
  expect: "holds",
})

export const WarehousePersonViolated = Norn.example("person-kind warehouse is refused", {
  of: WarehouseKindIsOrganization,
  given: {
    id: "wh_bad",
    warehouse_code: "WH-BAD-01",
    display_name: "Invalid Warehouse Agent",
    kind: "person",
    status: "active",
  },
  expect: "violated",
})

export const ItemNonNegativeReorder = Norn.law({
  id: "inventory.Item.non_negative_reorder.law",
  title: "Item reorder point is never negative",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.Item.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.Item.non_negative_reorder",
    owner: "inventory",
    severity: "error",
    rationale: "A replenishment reorder threshold is a non-negative physical quantity.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/warehouse.ts" },
    subject: "inventory.Item",
    scope: { root: { kind: "entity", id: "inventory.Item" }, objectTypes: ["inventory.Item"] },
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.reorder_point >= 0" },
  },
})

export const ItemZeroReorderHolds = Norn.example("zero reorder point holds", {
  of: ItemNonNegativeReorder,
  given: {
    id: "item_1",
    sku: "SKU-BEARING-01",
    gtin: "01234567890128",
    display_name: "Precision Steel Bearing",
    base_unit: "{each}",
    valuation_method: "fifo",
    track_lot: true,
    reorder_point: 0,
    status: "active",
  },
  expect: "holds",
})

export const ItemNegativeReorderViolated = Norn.example("negative reorder point is refused", {
  of: ItemNonNegativeReorder,
  given: {
    id: "item_2",
    sku: "SKU-BEARING-02",
    gtin: null,
    display_name: "Invalid Reorder Item",
    base_unit: "{each}",
    valuation_method: "moving_average",
    track_lot: false,
    reorder_point: -5,
    status: "active",
  },
  expect: "violated",
})

export const LotBatchValidDates = Norn.law({
  id: "inventory.LotBatch.valid_dates.law",
  title: "Lot manufacture date does not exceed expiry date",
  version: "0.1.0",
  dependencies: [{ artifactId: "inventory.LotBatch.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "inventory.LotBatch.valid_dates",
    owner: "inventory",
    severity: "error",
    rationale: "When an expiry date is recorded on a lot, manufactured_on must be on or before expires_on.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/inventory/warehouse.ts" },
    subject: "inventory.LotBatch",
    scope: { root: { kind: "entity", id: "inventory.LotBatch" }, objectTypes: ["inventory.LotBatch"] },
    condition: "self.expires_on != null",
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "date(self.manufactured_on) <= date(self.expires_on ?? self.manufactured_on)" },
  },
})

export const LotBatchOpenDatesNotApplicable = Norn.example("non-perishable lot without expiry date is not applicable", {
  of: LotBatchValidDates,
  given: {
    id: "lot_open",
    lot_code: "LOT-2026-OPEN",
    manufactured_on: "2026-11-01",
    expires_on: null,
    status: "active",
  },
  expect: "not_applicable",
})

export const LotBatchValidDatesHolds = Norn.example("lot with manufactured_on before expires_on holds", {
  of: LotBatchValidDates,
  given: {
    id: "lot_valid",
    lot_code: "LOT-2026-VALID",
    manufactured_on: "2026-11-01",
    expires_on: "2028-11-01",
    status: "active",
  },
  expect: "holds",
})

export const LotBatchReversedDatesViolated = Norn.example("lot with manufactured_on after expires_on is refused", {
  of: LotBatchValidDates,
  given: {
    id: "lot_bad",
    lot_code: "LOT-2026-REVERSED",
    manufactured_on: "2028-11-01",
    expires_on: "2026-11-01",
    status: "active",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Lot Lifecycle Actions

const LotTransitionInput = Schema.Struct({
  lotId: Schema.String,
})

const LotTransitionOutput = Schema.Struct({
  lotId: Schema.String,
  status: Schema.String,
})

export const QuarantineLot = Norn.action({
  id: "inventory.lot.quarantine",
  title: "Quarantine lot or batch",
  version: "0.1.0",
  description: "Move an active lot into quarantine pending quality inspection.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: LotTransitionInput,
  output: LotTransitionOutput,
  writes: [{ type: LotBatch, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LotBatch,
    objectIdField: "lotId",
    lifecycle: "status",
    transition: "quarantine",
  },
})

export const RecallLot = Norn.action({
  id: "inventory.lot.recall",
  title: "Recall lot or batch",
  version: "0.1.0",
  description: "Permanently recall a lot or batch into terminal immutable recalled state.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "inventory.access.policy", version: "0.1.0" },
  input: LotTransitionInput,
  output: LotTransitionOutput,
  writes: [{ type: LotBatch, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: LotBatch,
    objectIdField: "lotId",
    lifecycle: "status",
    transition: "recall",
  },
})
