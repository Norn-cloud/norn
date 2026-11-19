// distributor-1: the deployment-local ontology. Everything here is what the
// canon (registry/packs/canon) could not express for a mid-size distributor,
// so it is the per-deployment cost the reuse number counts. Each type says
// which canon gap it fills; a canon that grows these makes the next
// deployment cheaper.
import { Norn } from "@norn/authoring"
import { Schema } from "effect"
import { Item, Party } from "../../../../../registry/packs/canon/records.ts"

const Currency = Schema.Literals(["EGP", "USD"])

/**
 * A mapping targets a type through an interface. The distributor's own types
 * have no canon interface, so they share this marker (a deployment cost: a
 * canon interface per concept would carry meaning, this one carries none).
 */
export const Observed = Norn.interfaceType({
  name: "distributor.Observed", title: "Observed from a source", version: "0.1.0",
  properties: Schema.Struct({}),
})
const Region = Schema.Literals(["Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt", "unknown"])

/**
 * GAP canon.Party: no region, tax id, credit limit or second-language name.
 * One profile per party and source; the party itself stays canon.
 */
export const CustomerProfile = Norn.type({
  name: "distributor.CustomerProfile", implements: [Observed], title: "Customer profile", version: "0.1.0",
  properties: Schema.Struct({
    profile_id: Schema.String, region: Region, tax_id: Schema.NullOr(Schema.String),
    credit_limit: Schema.NullOr(Norn.decimal(2)), name_local: Schema.NullOr(Schema.String), source_system: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["profile_id"] }],
  links: [{ id: "party", target: Party, cardinality: { to: "one", from: "many", required: true } }],
})

/**
 * GAP canon: no invoice. canon.ItemizedDocument is the concept and its law
 * (total = sum of lines) is inherited here, not copied. Region is copied onto
 * the invoice because a question groups across a link only by linked time.
 */
export const SalesInvoice = Norn.type({
  name: "distributor.SalesInvoice", title: "Sales invoice", version: "0.1.0", implements: ["canon.ItemizedDocument"],
  properties: Schema.Struct({
    invoice_id: Schema.String, posted_at: Norn.ref("iso8601.timestamp"), region: Region,
    currency: Currency, total: Norn.decimal(2, { currency: "currency", additivity: { kind: "additive" } }), source_system: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["invoice_id"] }],
  links: [
    { id: "customer", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "lines", target: "distributor.DocumentLine", cardinality: { to: "many", from: "one" } },
  ],
})

/** A customer's purchase order as received (EDI 850): also an itemized document. */
export const CustomerOrder = Norn.type({
  name: "distributor.CustomerOrder", title: "Customer purchase order", version: "0.1.0", implements: ["canon.ItemizedDocument"],
  properties: Schema.Struct({
    order_id: Schema.String, ordered_at: Norn.ref("iso8601.timestamp"),
    currency: Currency, total: Norn.decimal(2, { currency: "currency", additivity: { kind: "additive" } }), source_system: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["order_id"] }],
  links: [
    { id: "customer", target: Party, cardinality: { to: "one", from: "many" } },
    { id: "lines", target: "distributor.DocumentLine", cardinality: { to: "many", from: "one" } },
  ],
})

/** One line of an invoice or a customer order (canon.LineItem). */
export const DocumentLine = Norn.type({
  name: "distributor.DocumentLine", title: "Document line", version: "0.1.0", implements: ["canon.LineItem"],
  properties: Schema.Struct({
    line_id: Schema.String, quantity: Norn.decimal(3), rate: Norn.decimal(2),
    currency: Currency, amount: Norn.decimal(2, { currency: "currency", additivity: { kind: "additive" } }),
  }),
  identity: [{ name: "primary", properties: ["line_id"] }],
  links: [
    { id: "document", target: "canon.ItemizedDocument", cardinality: { to: "one", from: "many", required: true } },
    { id: "item", target: Item, cardinality: { to: "one", from: "many" } },
  ],
})

/**
 * GAP canon.SettlementObservation: payer and payee are required, but a bank
 * line names a counterparty only as free text. The receipt keeps the bank's
 * fact; the invoice link is the reconciliation, made or not.
 */
export const BankReceipt = Norn.type({
  name: "distributor.BankReceipt", implements: [Observed], title: "Bank receipt", version: "0.1.0",
  properties: Schema.Struct({
    receipt_id: Schema.String, bank: Schema.Literals(["BANK-A", "BANK-B"]), booked_at: Norn.ref("iso8601.timestamp"),
    currency: Currency, amount: Norn.decimal(2, { currency: "currency", additivity: { kind: "additive" } }),
    remittance: Schema.String, matched: Schema.Boolean,
  }),
  identity: [{ name: "primary", properties: ["receipt_id"] }],
  links: [{ id: "invoice", target: SalesInvoice, cardinality: { to: "one", from: "many" } }],
})

/**
 * Settlement matching: a receipt reconciled to an invoice pays exactly its
 * total. A nullable link cannot be narrowed in a law lambda (NORN-FLOW-TYPE),
 * so this is the IR form, guarded by the extractor's `matched` flag.
 */
export const ReceiptMatchesInvoice = Norn.law({
  id: "distributor.BankReceipt.pays_its_invoice.law", title: "A reconciled receipt pays its invoice in full", version: "0.1.0",
  dependencies: [{ artifactId: "distributor.BankReceipt.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "distributor.BankReceipt.pays_its_invoice", owner: "distributor",
    severity: "warning", rationale: "A receipt reconciled to an invoice pays exactly its total in its currency; anything else is a short payment or a wrong match.",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/deployments/distributor-1/project/src/distributor.ts" },
    subject: "distributor.BankReceipt",
    scope: { root: { kind: "entity", id: "distributor.BankReceipt" }, objectTypes: ["distributor.BankReceipt"] },
    condition: "self.matched",
    modality: { kind: "always" }, response: { kind: "predicate", expression: "self.amount == self.invoice.total && self.currency == self.invoice.currency" },
  },
})

/**
 * GAP canon.Holding: a balance at one instant, with no opening, no movements
 * and no conservation law. A stock ledger per item and location carries them.
 */
export const StockLedger = Norn.type({
  name: "distributor.StockLedger", implements: [Observed], title: "Stock ledger", version: "0.1.0",
  properties: Schema.Struct({
    ledger_id: Schema.String, location: Schema.String, opening_qty: Norn.decimal(3), closing_qty: Norn.decimal(3),
    observed_at: Norn.ref("iso8601.timestamp"), source_system: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["ledger_id"] }],
  links: [
    { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } },
    { id: "moves", target: "distributor.StockMove", cardinality: { to: "many", from: "one" } },
  ],
})

export const StockMove = Norn.type({
  name: "distributor.StockMove", implements: [Observed], title: "Stock movement", version: "0.1.0",
  properties: Schema.Struct({
    move_id: Schema.String, quantity: Norn.decimal(3, { additivity: { kind: "additive" } }), occurred_at: Norn.ref("iso8601.timestamp"), reference: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["move_id"] }],
  links: [{ id: "ledger", target: StockLedger, cardinality: { to: "one", from: "many", required: true } }],
})

export const StockConserves = Norn.law({
  id: "distributor.StockLedger.conserves.law", title: "Closing stock is opening stock plus its movements", version: "0.1.0",
  dependencies: [{ artifactId: "distributor.StockLedger.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "distributor.StockLedger.conserves", owner: "distributor",
    severity: "error", rationale: "Stock is neither created nor lost: the closing balance equals the opening balance plus every receipt and less every issue.",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/deployments/distributor-1/project/src/distributor.ts" },
    subject: "distributor.StockLedger",
    scope: { root: { kind: "entity", id: "distributor.StockLedger" }, objectTypes: ["distributor.StockLedger"] },
    condition: "len(self.moves) > 0",
    modality: { kind: "always" }, response: { kind: "predicate", expression: "self.closing_qty == self.opening_qty + sum(map(self.moves, #.quantity))" },
  },
})

/**
 * Settlement matching as a governed action: a mapping links only within one
 * source, so a bank receipt (files) reaches an invoice (an ERP) only here.
 * The caller restates the receipt; the effect upserts it with the link, and
 * the pays_its_invoice law then judges the match.
 */
export const ReconcileReceipt = Norn.action({
  id: "distributor.receipt.reconcile", title: "Reconcile a receipt to an invoice", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({
    // GAP: action inputs cannot be timestamps (Norn.ref has no closed-schema type), so booked_at is text.
    receipt_id: Schema.String, bank: Schema.Literals(["BANK-A", "BANK-B"]), booked_at: Schema.String, currency: Currency,
    amount: Norn.decimal(2), remittance: Schema.String, matched: Schema.Boolean, invoice_id: Schema.String,
  }),
  output: Schema.Struct({ receipt_id: Schema.String }),
  slice: [{ name: "invoice", type: SalesInvoice, key: { property: { from: "input", property: "invoice_id" } } }],
  createLinks: [{ link: "invoice", targetType: SalesInvoice, targetIdField: "invoice_id" }],
  writes: [{ type: BankReceipt, change: "modified" }, { type: BankReceipt, change: "created" }],
  effect: { kind: "upsert_input_object", type: BankReceipt, objectIdField: "receipt_id" },
})

/** The web and list prices of an item (xlsx price list). */
export const PriceListEntry = Norn.type({
  name: "distributor.PriceListEntry", implements: [Observed], title: "Price list entry", version: "0.1.0",
  properties: Schema.Struct({ entry_id: Schema.String, list_price: Norn.decimal(2), web_price: Schema.NullOr(Norn.decimal(2)), currency: Currency }),
  identity: [{ name: "primary", properties: ["entry_id"] }],
  links: [{ id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } }],
})

/** A customer's open balance in the pre-ERP aging file. */
export const ReceivableBalance = Norn.type({
  name: "distributor.ReceivableBalance", implements: [Observed], title: "Receivable balance", version: "0.1.0",
  properties: Schema.Struct({ balance_id: Schema.String, balance: Norn.decimal(2, { additivity: { kind: "semi_additive" } }), last_txn_on: Schema.String, blocked: Schema.Boolean }),
  identity: [{ name: "primary", properties: ["balance_id"] }],
  links: [{ id: "party", target: Party, cardinality: { to: "one", from: "many", required: true } }],
})

/** Raising a customer's credit limit: requested, then decided. */
export const CreditRequest = Norn.type({
  name: "distributor.CreditRequest", title: "Credit limit request", version: "0.1.0",
  properties: Schema.Struct({
    request_id: Schema.String, profile_id: Schema.String, new_limit: Norn.decimal(2), requester: Schema.String,
    status: Schema.Literals(["draft", "submitted", "approved", "rejected"]),
  }),
  identity: [{ name: "primary", properties: ["request_id"] }],
  links: [{ id: "profile", target: CustomerProfile, cardinality: { to: "one", from: "many", required: true } }],
  defaults: { status: "draft" },
  lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["approved", "rejected"],
    transitions: [
      { id: "submit", from: ["draft"], to: "submitted" },
      { id: "approve", from: ["submitted"], to: "approved" },
      { id: "reject", from: ["submitted"], to: "rejected" },
    ],
  }],
})

const RequestRef = Schema.Struct({ request_id: Schema.String })

export const RequestCredit = Norn.action({
  id: "distributor.credit.request", title: "Request a credit limit", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ request_id: Schema.String, profile_id: Schema.String, new_limit: Norn.decimal(2), requester: Schema.String }),
  output: RequestRef,
  // GAP: `fromCaller` (server-set requester) is not on main (#1469); the caller states it.
  slice: [{ name: "profile", type: CustomerProfile, key: { property: { from: "input", property: "profile_id" } } }],
  createLinks: [{ link: "profile", targetType: CustomerProfile, targetIdField: "profile_id" }],
  writes: [{ type: CreditRequest, change: "created" }],
  effect: { kind: "upsert_input_object", type: CreditRequest, objectIdField: "request_id", writeMode: { kind: "create_only" } },
  access: { artifactId: "distributor.credit.policy", version: "0.1.0" },
})

export const SubmitCredit = Norn.action({
  id: "distributor.credit.submit", title: "Submit for approval", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: CreditRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: CreditRequest, objectIdField: "request_id", lifecycle: "status", transition: "submit" },
  access: { artifactId: "distributor.credit.policy", version: "0.1.0" },
})

export const ApproveCredit = Norn.action({
  id: "distributor.credit.approve", title: "Approve", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: CreditRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: CreditRequest, objectIdField: "request_id", lifecycle: "status", transition: "approve" },
  access: { artifactId: "distributor.credit.policy", version: "0.1.0" },
})

export const RejectCredit = Norn.action({
  id: "distributor.credit.reject", title: "Reject", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: CreditRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: CreditRequest, objectIdField: "request_id", lifecycle: "status", transition: "reject" },
  access: { artifactId: "distributor.credit.policy", version: "0.1.0" },
})

export const CreditPolicy = Norn.access({
  id: "distributor.credit", title: "Credit request access", version: "0.1.0",
  rules: [
    {
      ruleId: "sales_request",
      actions: [RequestCredit, SubmitCredit],
      resourceType: CreditRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }, { resource: { name: "requester", op: "eq", value: { principalAttribute: "id" } } }],
    },
    {
      ruleId: "finance_decides_others",
      actions: [ApproveCredit, RejectCredit],
      resourceType: CreditRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["admin", "owner"] } }, { resource: { name: "requester", op: "ne", value: { principalAttribute: "id" } } }],
    },
  ],
})
