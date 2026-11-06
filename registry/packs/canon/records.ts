import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent, AgentRole, Resource, EconomicEvent, Commitment, LedgerEffect } from "../../core/src/physics.ts"

export const Catalog = Norn.interfaceType({
  name: "canon.Catalog", title: "Catalog metadata, never stock", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, code: Schema.String, label: Schema.String }),
})
export const Item = Norn.type({
  name: "canon.Item", title: "Item", version: "0.1.0", implements: [Catalog],
  properties: Schema.Struct({ id: Schema.String, code: Schema.String, label: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Party = Norn.type({
  name: "canon.Party", title: "Party", version: "0.1.0", implements: [Agent],
  properties: Schema.Struct({ display_name: Norn.ref("core.display_name"), kind: Schema.Literals(["person", "organization", "system"]) }),
})
export const PartyRole = Norn.type({
  name: "canon.PartyRole", title: "Source party role", version: "0.1.0", implements: [AgentRole],
  properties: Schema.Struct({ role: Norn.ref("core.code"), valid_from: Schema.NullOr(Norn.ref("iso8601.date")), valid_to: Schema.NullOr(Norn.ref("iso8601.date")) }),
  links: [
    { id: "agent", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "counterparty", target: Party, cardinality: { to: "one", from: "many" } },
  ],
})
export const Holding = Norn.type({
  name: "canon.Holding", title: "Owner and location partitioned holding", version: "0.1.0", implements: [Resource],
  properties: Schema.Struct({
    resource_code: Norn.ref("core.code"), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    location: Schema.String, quantity_basis: Schema.Literals(["on_hand", "available"]), stock_cost: Schema.NullOr(Norn.decimal(6, { currency: "cost_currency" })),
    cost_currency: Schema.NullOr(Schema.String),
  }),
  links: [
    { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner", target: Party, cardinality: { to: "one", from: "many" } },
    { id: "custodian", target: Party, cardinality: { to: "one", from: "many" } },
  ],
})
export const OrderLine = Norn.type({
  name: "canon.OrderLine", title: "Order-line commitment", version: "0.1.0", implements: [Commitment],
  properties: Schema.Struct({
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "provider", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", target: Holding, cardinality: { to: "one", from: "many" } },
  ],
})
export const Delivery = Norn.type({
  name: "canon.Delivery", title: "Delivery observation", version: "0.1.0", implements: [EconomicEvent],
  properties: Schema.Struct({
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Norn.ref("iso8601.timestamp"), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "provider", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", target: Holding, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", target: OrderLine, cardinality: { to: "one", from: "many" } },
    { id: "reverses", target: "core.EconomicEvent", cardinality: { to: "one", from: "one" } },
  ],
})
export const Posting = Norn.type({
  name: "canon.Posting", title: "Signed ledger observation, not a posting command", version: "0.1.0", implements: [LedgerEffect],
  properties: Schema.Struct({ account_code: Norn.ref("core.code"), amount: Norn.ref("core.money_amount"), currency: Norn.ref("core.code"), book: Schema.String, account_role: Schema.String }),
  links: [{ id: "event", target: Delivery, cardinality: { to: "one", from: "many", required: true } }],
})
export const Settlement = Norn.interfaceType({
  name: "canon.Settlement", title: "Reported settlement evidence", version: "0.1.0",
  properties: Schema.Struct({ amount: Norn.decimal(6, { currency: "currency" }), currency: Schema.String, kind: Schema.Literals(["payment", "refund"]) }),
})
export const SettlementObservation = Norn.type({
  name: "canon.SettlementObservation", title: "Settlement observation", version: "0.1.0", implements: [Settlement],
  properties: Schema.Struct({ amount: Norn.decimal(6, { currency: "currency" }), currency: Schema.String, kind: Schema.Literals(["payment", "refund"]) }),
  links: [
    { id: "payer", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "payee", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "event", target: "core.EconomicEvent", cardinality: { to: "one", from: "many" } },
  ],
})

/**
 * A keyed stock valuation policy (D66 demo): an upsert sets its enum
 * `valuation_method`, so a generated form shows labels, groups and a choice
 * the checker proves fits the property it writes.
 */
export const ValuationPolicy = Norn.type({
  name: "canon.ValuationPolicy", title: "Stock valuation policy", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, label: Schema.String, valuation_method: Schema.Literals(["fifo", "moving_average"]) }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const SetValuationPolicy = Norn.action({
  id: "canon.valuation_policy.set", title: "Set stock valuation policy", version: "0.1.0",
  description: "Create or update a stock valuation policy.",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String, label: Schema.String, valuation_method: Schema.Literals(["fifo", "moving_average"]) }),
  output: Schema.Struct({ id: Schema.String }),
  inputLabels: {
    id: { title: "Policy code" },
    label: { title: "Name", description: "How the policy appears in reports." },
    valuation_method: { title: "Valuation method", description: "How stock on hand is valued: first in, first out, or a moving average." },
  },
  inputGroups: [
    { id: "policy", title: "Policy", fields: ["/id", "/label"] },
    { id: "valuation", title: "Valuation", fields: ["/valuation_method"] },
  ],
  writes: [{ type: ValuationPolicy, change: "modified" }],
  effect: { kind: "upsert_input_object", type: ValuationPolicy, objectIdField: "id" },
})
