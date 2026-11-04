import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Process } from "../../registry/core/src/physics"

const Order = Schema.Struct({
  id: Schema.String,
  status: Schema.Literals(["draft", "approved", "received", "cancelled"]),
  ordered: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
  received: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
})

export const Purchase = Norn.type({
  name: "stress.Purchase", title: "Synthetic purchase to receipt", version: "1.0.0",
  role: "happening", implements: [Process], properties: Order,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "draft", received: 0 },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["received", "cancelled"],
    immutable: ["received", "cancelled"], transitions: [
      { id: "approve", from: ["draft"], to: "approved" },
      { id: "receive", from: ["approved"], to: "received",
        guards: [Norn.expression(Order, x => x.received === x.ordered)] },
      { id: "cancel", from: ["draft", "approved"], to: "cancelled" },
    ] }],
})

export const PositiveOrder = Norn.law({
  id: "stress.purchase_positive.law", title: "Order quantity must be positive", version: "1.0.0",
  dependencies: [{ artifactId: "stress.Purchase.entity", kind: "entity", version: "1.0.0", required: true }],
  law: { lawVersion: "0.1", id: "stress.purchase_positive", owner: "stress", severity: "error",
    rationale: "A purchase promises a positive quantity.",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/stress/purchase.ts" },
    scope: { root: { kind: "entity", id: "stress.Purchase" }, objectTypes: ["stress.Purchase"] },
    subject: "stress.Purchase", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.ordered > 0" } },
})
export const ReceiptLimit = Norn.law({
  id: "stress.purchase_receipt_limit.law", title: "Receipts stay within the order", version: "1.0.0",
  dependencies: [{ artifactId: "stress.Purchase.entity", kind: "entity", version: "1.0.0", required: true }],
  law: { lawVersion: "0.1", id: "stress.purchase_receipt_limit", owner: "stress", severity: "error",
    rationale: "Received quantity cannot be negative or exceed the approved order.",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/stress/purchase.ts" },
    scope: { root: { kind: "entity", id: "stress.Purchase" }, objectTypes: ["stress.Purchase"] },
    subject: "stress.Purchase", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.received >= 0 and self.received <= self.ordered" } },
})

export const CreatePurchase = Norn.action({
  id: "stress.purchase.create", title: "Create purchase", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  input: Schema.Struct({ id: Schema.String,
    ordered: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
    received: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
  }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Purchase, change: "created" }],
  effect: { kind: "upsert_input_object", type: Purchase, objectIdField: "id", writeMode: { kind: "create_only" } },
})
export const ApprovePurchase = Norn.action({
  id: "stress.purchase.approve", title: "Approve purchase", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Purchase, change: "modified" }],
  effect: { kind: "fire_transition", type: Purchase, objectIdField: "id", lifecycle: "status", transition: "approve" },
})
export const RecordReceivedQuantity = Norn.action({
  id: "stress.purchase.record_received", title: "Record counted receipt quantity", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  slice: [{ name: "purchase", type: Purchase, key: { property: { from: "input", property: "id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ purchase: Order }) }), x => x.slice.purchase.status === "approved")],
  input: Schema.Struct({ id: Schema.String, received: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }) }),
  output: Schema.Struct({ id: Schema.String }), writes: [{ type: Purchase, change: "modified" }, { type: Purchase, change: "created" }],
  effect: { kind: "upsert_input_object", type: Purchase, objectIdField: "id",
    writeMode: { kind: "upsert" } },
})
export const ReceivePurchase = Norn.action({
  id: "stress.purchase.receive", title: "Complete receipt", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Purchase, change: "modified" }],
  effect: { kind: "fire_transition", type: Purchase, objectIdField: "id", lifecycle: "status", transition: "receive" },
})

const Line = Schema.Struct({
  id: Schema.String,
  accepted: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
  rejected: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
  delivered: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
})
const LineInput = Schema.Struct({
  id: Schema.String,
  accepted: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
  rejected: Schema.optionalKey(Norn.decimal(0, { unit: { system: "ucum", code: "1" } })),
  delivered: Norn.decimal(0, { unit: { system: "ucum", code: "1" } }),
})
export const ReceiptLine = Norn.type({
  name: "stress.ReceiptLine", title: "Immutable receipt inspection", version: "1.0.0",
  role: "happening", implements: [Process], properties: Line,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  identity: [{ name: "primary", properties: ["id"] }], defaults: { rejected: 0 },
})
export const ReceiptBalance = Norn.law({
  id: "stress.receipt_balance.law", title: "Every delivered unit is accepted or rejected", version: "1.0.0",
  dependencies: [{ artifactId: "stress.ReceiptLine.entity", kind: "entity", version: "1.0.0", required: true }],
  law: { lawVersion: "0.1", id: "stress.receipt_balance", owner: "stress", severity: "error",
    rationale: "Inspection accounts for the entire delivery, including rejected goods.",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/stress/purchase.ts" },
    scope: { root: { kind: "entity", id: "stress.ReceiptLine" }, objectTypes: ["stress.ReceiptLine"] },
    subject: "stress.ReceiptLine", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.delivered == self.accepted + self.rejected" } },
})
export const InspectReceipt = Norn.action({
  id: "stress.receipt.inspect", title: "Record receipt inspection", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  input: LineInput, output: Schema.Struct({ id: Schema.String }),
  derive: [{ field: "rejected", inputRequired: false,
    expression: Norn.expression(LineInput, x => x.delivered - x.accepted) }],
  preconditions: [Norn.expression(Schema.Struct({ input: LineInput }), x => x.input.delivered > 0 && x.input.accepted >= 0 && x.input.accepted <= x.input.delivered)],
  writes: [{ type: ReceiptLine, change: "created" }],
  effect: { kind: "upsert_input_object", type: ReceiptLine, objectIdField: "id", writeMode: { kind: "create_only" } },
})
