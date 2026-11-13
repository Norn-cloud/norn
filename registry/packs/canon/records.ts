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
/**
 * The ISO 4217 codes a canon currency admits: SIX list one (the active codes, published 2026-09-17),
 * less XTS (testing) and XXX (no currency). Every canon currency enum is this list, and every pack
 * declares its source currency fields with {@link ISO_4217_CODE_SET}, so a real source's EGP or EUR
 * maps like USD. The three-systems test holds the two lists and every use equal.
 */
export const ISO_4217 = [
  "AED", "AFN", "ALL", "AMD", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BHD", "BIF",
  "BMD", "BND", "BOB", "BOV", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF", "CHE", "CHF",
  "CHW", "CLF", "CLP", "CNY", "COP", "COU", "CRC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD",
  "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP", "GMD", "GNF", "GTQ", "GYD",
  "HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD", "JOD", "JPY", "KES",
  "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD", "LSL", "LYD",
  "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR", "MWK", "MXN", "MXV", "MYR",
  "MZN", "NAD", "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN", "PGK", "PHP", "PKR", "PLN",
  "PYG", "QAR", "RON", "RSD", "RUB", "RWF", "SAR", "SBD", "SCR", "SDG", "SEK", "SGD", "SHP", "SLE",
  "SOS", "SRD", "SSP", "STN", "SVC", "SYP", "SZL", "THB", "TJS", "TMT", "TND", "TOP", "TRY", "TTD",
  "TWD", "TZS", "UAH", "UGX", "USD", "USN", "UYI", "UYU", "UYW", "UZS", "VED", "VES", "VND", "VUV",
  "WST", "XAD", "XAF", "XAG", "XAU", "XBA", "XBB", "XBC", "XBD", "XCD", "XCG", "XDR", "XOF", "XPD",
  "XPF", "XPT", "XSU", "XUA", "YER", "ZAR", "ZMW", "ZWG",
] as const
/** {@link ISO_4217} as a source code set's values (`codeSets: [{ ..., values: ISO_4217_CODE_SET }]`). */
export const ISO_4217_CODE_SET = [
  { value: "AED" }, { value: "AFN" }, { value: "ALL" }, { value: "AMD" }, { value: "AOA" },
  { value: "ARS" }, { value: "AUD" }, { value: "AWG" }, { value: "AZN" }, { value: "BAM" },
  { value: "BBD" }, { value: "BDT" }, { value: "BHD" }, { value: "BIF" }, { value: "BMD" },
  { value: "BND" }, { value: "BOB" }, { value: "BOV" }, { value: "BRL" }, { value: "BSD" },
  { value: "BTN" }, { value: "BWP" }, { value: "BYN" }, { value: "BZD" }, { value: "CAD" },
  { value: "CDF" }, { value: "CHE" }, { value: "CHF" }, { value: "CHW" }, { value: "CLF" },
  { value: "CLP" }, { value: "CNY" }, { value: "COP" }, { value: "COU" }, { value: "CRC" },
  { value: "CUP" }, { value: "CVE" }, { value: "CZK" }, { value: "DJF" }, { value: "DKK" },
  { value: "DOP" }, { value: "DZD" }, { value: "EGP" }, { value: "ERN" }, { value: "ETB" },
  { value: "EUR" }, { value: "FJD" }, { value: "FKP" }, { value: "GBP" }, { value: "GEL" },
  { value: "GHS" }, { value: "GIP" }, { value: "GMD" }, { value: "GNF" }, { value: "GTQ" },
  { value: "GYD" }, { value: "HKD" }, { value: "HNL" }, { value: "HTG" }, { value: "HUF" },
  { value: "IDR" }, { value: "ILS" }, { value: "INR" }, { value: "IQD" }, { value: "IRR" },
  { value: "ISK" }, { value: "JMD" }, { value: "JOD" }, { value: "JPY" }, { value: "KES" },
  { value: "KGS" }, { value: "KHR" }, { value: "KMF" }, { value: "KPW" }, { value: "KRW" },
  { value: "KWD" }, { value: "KYD" }, { value: "KZT" }, { value: "LAK" }, { value: "LBP" },
  { value: "LKR" }, { value: "LRD" }, { value: "LSL" }, { value: "LYD" }, { value: "MAD" },
  { value: "MDL" }, { value: "MGA" }, { value: "MKD" }, { value: "MMK" }, { value: "MNT" },
  { value: "MOP" }, { value: "MRU" }, { value: "MUR" }, { value: "MVR" }, { value: "MWK" },
  { value: "MXN" }, { value: "MXV" }, { value: "MYR" }, { value: "MZN" }, { value: "NAD" },
  { value: "NGN" }, { value: "NIO" }, { value: "NOK" }, { value: "NPR" }, { value: "NZD" },
  { value: "OMR" }, { value: "PAB" }, { value: "PEN" }, { value: "PGK" }, { value: "PHP" },
  { value: "PKR" }, { value: "PLN" }, { value: "PYG" }, { value: "QAR" }, { value: "RON" },
  { value: "RSD" }, { value: "RUB" }, { value: "RWF" }, { value: "SAR" }, { value: "SBD" },
  { value: "SCR" }, { value: "SDG" }, { value: "SEK" }, { value: "SGD" }, { value: "SHP" },
  { value: "SLE" }, { value: "SOS" }, { value: "SRD" }, { value: "SSP" }, { value: "STN" },
  { value: "SVC" }, { value: "SYP" }, { value: "SZL" }, { value: "THB" }, { value: "TJS" },
  { value: "TMT" }, { value: "TND" }, { value: "TOP" }, { value: "TRY" }, { value: "TTD" },
  { value: "TWD" }, { value: "TZS" }, { value: "UAH" }, { value: "UGX" }, { value: "USD" },
  { value: "USN" }, { value: "UYI" }, { value: "UYU" }, { value: "UYW" }, { value: "UZS" },
  { value: "VED" }, { value: "VES" }, { value: "VND" }, { value: "VUV" }, { value: "WST" },
  { value: "XAD" }, { value: "XAF" }, { value: "XAG" }, { value: "XAU" }, { value: "XBA" },
  { value: "XBB" }, { value: "XBC" }, { value: "XBD" }, { value: "XCD" }, { value: "XCG" },
  { value: "XDR" }, { value: "XOF" }, { value: "XPD" }, { value: "XPF" }, { value: "XPT" },
  { value: "XSU" }, { value: "XUA" }, { value: "YER" }, { value: "ZAR" }, { value: "ZMW" },
  { value: "ZWG" },
] as const

// A stock's cost is summed per its ISO 4217 currency (METRIC-UNIT-PARTITION-1); a cost without its
// currency is refused at the write (the invariant), and a holding with no cost may have neither.
const StockValuationProperties = Schema.Struct({
  stock_cost: Schema.NullOr(Norn.decimal(6, { currency: "cost_currency", additivity: { kind: "semi_additive" } })),
  cost_currency: Schema.NullOr(Schema.Literals(ISO_4217)),
})
export const StockValuation = Norn.interfaceType({
  name: "canon.StockValuation", title: "Valued stock", version: "0.1.0",
  properties: StockValuationProperties,
  invariants: [{ id: "cost_has_currency", message: "A stock cost names its currency.",
    expression: Norn.expression(StockValuationProperties, self => self.stock_cost === null || self.cost_currency !== null),
  }],
})
export const Holding = Norn.type({
  name: "canon.Holding", title: "Owner and location partitioned holding", version: "0.1.0", implements: [Resource, StockValuation],
  properties: Schema.Struct({
    // A holding is a stock: summable across holdings at one instant, never over time. Every canon
    // quantity is held in `unit` or, for money, `currency` (METRIC-UNIT-PARTITION-1); a holding's is
    // also on its `quantity_basis`, since on-hand and available quantities never add (THREE-SYSTEMS).
    resource_code: Norn.ref("core.code"), quantity: Norn.ref("core.quantity", { additivity: { kind: "semi_additive" }, unitSource: ["unit", "currency", "quantity_basis"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    // When the source observed this balance (valid time; METRIC-SNAPSHOT-ASOF-1 orders a holding's versions by it).
    observed_at: Norn.ref("iso8601.timestamp"),
    location: Schema.String, quantity_basis: Schema.Literals(["on_hand", "available"]), stock_cost: Schema.NullOr(Norn.decimal(6, { currency: "cost_currency", additivity: { kind: "semi_additive" } })),
    cost_currency: Schema.NullOr(Schema.Literals(ISO_4217)),
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
    // An ordered quantity is a flow: additive across lines and over due_at.
    due_at: Norn.ref("iso8601.timestamp"), quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
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
    // A delivered quantity is a flow: additive across events and over occurred_at.
    occurred_at: Norn.ref("iso8601.timestamp"), quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
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
// A posting's amount is a flow: additive, its time is its event's (METRIC-GRAIN-LINK-1), and its
// sums split by `currency`, since core.money_amount binds none (METRIC-UNIT-PARTITION-1).
export const Posting = Norn.type({
  name: "canon.Posting", title: "Signed ledger observation, not a posting command", version: "0.1.0", implements: [LedgerEffect],
  properties: Schema.Struct({ account_code: Norn.ref("core.code"), amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }), currency: Norn.ref("core.code"), book: Schema.String, account_role: Schema.String }),
  links: [{ id: "event", target: Delivery, cardinality: { to: "one", from: "many", required: true } }],
})
// A settlement's amount is a flow summed per currency (METRIC-UNIT-PARTITION-1); its currency is
// an ISO 4217 code, so "usd" is refused at the write instead of splitting a total from "USD".
export const Settlement = Norn.interfaceType({
  name: "canon.Settlement", title: "Reported settlement evidence", version: "0.1.0",
  properties: Schema.Struct({ amount: Norn.decimal(6, { currency: "currency", additivity: { kind: "additive" } }), currency: Schema.Literals(ISO_4217), kind: Schema.Literals(["payment", "refund"]) }),
})
export const SettlementObservation = Norn.type({
  name: "canon.SettlementObservation", title: "Settlement observation", version: "0.1.0", implements: [Settlement],
  properties: Schema.Struct({ amount: Norn.decimal(6, { currency: "currency", additivity: { kind: "additive" } }), currency: Schema.Literals(ISO_4217), kind: Schema.Literals(["payment", "refund"]) }),
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
  writes: [{ type: ValuationPolicy, change: "created" }, { type: ValuationPolicy, change: "modified" }],
  effect: { kind: "upsert_input_object", type: ValuationPolicy, objectIdField: "id" },
})
