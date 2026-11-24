/**
 * Governed Accounting Domain Pack — Receivables/Payables Claims & Payment Evidence Events.
 *
 * Jurisdiction-agnostic invoicing (`core.Claim`) and create-only immutable payment
 * evidence capture (`core.EconomicEvent`). `accounting.payment.record` records
 * immutable payment evidence linked to an open invoice without mutating
 * `Invoice.amount_paid`/`status` or `MonetaryAccount.quantity`; atomic multi-object
 * settlement balance updates belong to the multi-object settlement layer (#1586).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Claim, EconomicEvent } from "../../core/src/rea.ts"
import { Book, Counterparty, MonetaryAccount } from "./ledger.ts"

const InvoiceStatus = Schema.Literals(["draft", "issued", "partially_paid", "paid", "voided"])

export const Invoice = Norn.type({
  name: "accounting.Invoice",
  role: "thing",
  title: "Invoice",
  version: "0.1.0",
  description: "An accounts receivable or accounts payable economic claim (implements core.Claim).",
  implements: [Claim],
  properties: Schema.Struct({
    id: Schema.String,
    invoice_number: Norn.ref("core.document_number"),
    direction: Schema.Literals(["receivable", "payable"]),
    subtotal: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    tax_amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    amount_paid: Norn.ref("core.money_amount", { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
    currency: Norn.ref("core.code"),
    due_date: Norn.ref("iso8601.date"),
    status: InvoiceStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "creditor", description: "Counterparty entitled to receive payment on this invoice.", target: Counterparty, cardinality: { to: "one", from: "many", required: true } },
    { id: "debtor", description: "Counterparty obliged to settle this invoice.", target: Counterparty, cardinality: { to: "one", from: "many", required: true } },
    { id: "arises_from", description: "Optional commitment that gave rise to this invoice claim.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
    { id: "book", description: "Ledger book recording this invoice.", target: Book, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["paid", "voided"],
      immutable: ["paid", "voided"],
      transitions: [
        { id: "issue", from: ["draft"], to: "issued" },
        { id: "record_partial_payment", from: ["issued"], to: "partially_paid" },
        { id: "mark_paid", from: ["issued", "partially_paid"], to: "paid" },
        { id: "void", from: ["draft", "issued"], to: "voided" },
      ],
    },
  ],
})

const PaymentAction = Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"])
const PaymentMethod = Schema.Literals(["bank_transfer", "cash", "card", "check", "wire", "other"])

export const Payment = Norn.type({
  name: "accounting.Payment",
  role: "happening",
  title: "Payment",
  version: "0.1.0",
  description: "An immutable, create-only payment or settlement economic event (implements core.EconomicEvent).",
  implements: [EconomicEvent],
  properties: Schema.Struct({
    id: Schema.String,
    action: PaymentAction,
    occurred_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    payment_method: PaymentMethod,
    reference_no: Norn.ref("core.document_number"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "provider", description: "Counterparty remitting the payment.", target: Counterparty, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Counterparty receiving the payment.", target: Counterparty, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Monetary account settled by this payment.", target: MonetaryAccount, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", description: "Optional commitment fulfilled by this payment.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
    { id: "settles", description: "Optional invoice claim settled in whole or in part by this payment.", target: Invoice, cardinality: { to: "one", from: "many" } },
    { id: "reverses", description: "Optional prior payment reversed by this payment.", target: "accounting.Payment", cardinality: { to: "one", from: "one" } },
  ],
})

// ---------------------------------------------------------------- Governed Laws & Examples (LAW-EXAMPLE-1)

export const InvoiceAmountConservation = Norn.law({
  id: "accounting.Invoice.amount_conservation.law",
  title: "Invoice amounts conserve subtotal + tax and bound settled amount",
  version: "0.1.0",
  dependencies: [{ artifactId: "accounting.Invoice.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "accounting.Invoice.amount_conservation",
    owner: "accounting",
    severity: "error",
    rationale: "Subtotal and tax must be non-negative, invoice amount must equal subtotal + tax_amount, amount_paid must stay between 0 and amount, and non-draft invoices must have a strictly positive amount.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/accounting/settlement.ts" },
    subject: "accounting.Invoice",
    scope: { root: { kind: "entity", id: "accounting.Invoice" }, objectTypes: ["accounting.Invoice"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.subtotal >= 0 and self.tax_amount >= 0 and self.amount == self.subtotal + self.tax_amount and self.amount_paid >= 0 and self.amount_paid <= self.amount and (self.status == \"draft\" or self.amount > 0)",
    },
  },
})

export const InvoiceAmountConservationIssuedHolds = Norn.example("issued invoice with exact subtotal + tax holds", {
  of: InvoiceAmountConservation,
  given: { subtotal: 1000, tax_amount: 150, amount: 1150, amount_paid: 0, status: "issued" },
  expect: "holds",
})

export const InvoiceAmountConservationDraftZeroHolds = Norn.example("zero-amount draft invoice holds", {
  of: InvoiceAmountConservation,
  given: { subtotal: 0, tax_amount: 0, amount: 0, amount_paid: 0, status: "draft" },
  expect: "holds",
})

export const InvoiceAmountConservationMismatchViolated = Norn.example("invoice whose amount differs from subtotal + tax is violated", {
  of: InvoiceAmountConservation,
  given: { subtotal: 1000, tax_amount: 150, amount: 1100, amount_paid: 0, status: "issued" },
  expect: "violated",
})

export const InvoiceAmountConservationOverpaidViolated = Norn.example("overpaid invoice is violated", {
  of: InvoiceAmountConservation,
  given: { subtotal: 1000, tax_amount: 0, amount: 1000, amount_paid: 1200, status: "paid" },
  expect: "violated",
})

export const PaymentPositiveAmount = Norn.law({
  id: "accounting.Payment.positive_amount.law",
  title: "Payment quantity is strictly positive",
  version: "0.1.0",
  dependencies: [{ artifactId: "accounting.Payment.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "accounting.Payment.positive_amount",
    owner: "accounting",
    severity: "error",
    rationale: "Every recorded payment economic event must transfer a strictly positive monetary quantity.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/accounting/settlement.ts" },
    subject: "accounting.Payment",
    scope: { root: { kind: "entity", id: "accounting.Payment" }, objectTypes: ["accounting.Payment"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.quantity > 0",
    },
  },
})

export const PaymentPositiveHolds = Norn.example("positive payment quantity holds", {
  of: PaymentPositiveAmount,
  given: { quantity: 250 },
  expect: "holds",
})

export const PaymentZeroViolated = Norn.example("zero payment quantity is violated", {
  of: PaymentPositiveAmount,
  given: { quantity: 0 },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Actions

const IssueInvoiceSlice = Schema.Struct({
  slice: Schema.Struct({
    invoice: Schema.Struct({
      amount: Norn.decimal(6),
      status: InvoiceStatus,
    }),
  }),
})

export const IssueInvoice = Norn.action({
  id: "accounting.invoice.issue",
  title: "Issue invoice",
  version: "0.1.0",
  description: "Issue a draft invoice once its total amount is strictly positive.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    invoiceId: Schema.String,
  }),
  output: Schema.Struct({
    invoiceId: Schema.String,
    status: Schema.String,
  }),
  slice: [
    { name: "invoice", type: Invoice, key: { property: { from: "input", property: "invoiceId" } } },
  ],
  preconditions: [
    Norn.expression(
      IssueInvoiceSlice,
      (ctx) => ctx.slice.invoice.status === "draft" && ctx.slice.invoice.amount > 0,
      { root: "context" },
    ),
  ],
  writes: [{ type: Invoice, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Invoice,
    objectIdField: "invoiceId",
    lifecycle: "status",
    transition: "issue",
  },
})

const VoidInvoiceSlice = Schema.Struct({
  slice: Schema.Struct({
    invoice: Schema.Struct({
      amount_paid: Norn.decimal(6),
    }),
  }),
})

export const VoidInvoice = Norn.action({
  id: "accounting.invoice.void",
  title: "Void invoice",
  version: "0.1.0",
  description: "Void an unpaid draft or issued invoice with zero recorded payments.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    invoiceId: Schema.String,
  }),
  output: Schema.Struct({
    invoiceId: Schema.String,
    status: Schema.String,
  }),
  slice: [
    { name: "invoice", type: Invoice, key: { property: { from: "input", property: "invoiceId" } } },
  ],
  preconditions: [
    Norn.expression(
      VoidInvoiceSlice,
      (ctx) => ctx.slice.invoice.amount_paid === 0,
      { root: "context" },
    ),
  ],
  writes: [{ type: Invoice, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Invoice,
    objectIdField: "invoiceId",
    lifecycle: "status",
    transition: "void",
  },
})

const RecordPaymentInput = Schema.Struct({
  id: Schema.String,
  action: PaymentAction,
  occurred_at: Schema.String,
  quantity: Norn.decimal(6),
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
  payment_method: PaymentMethod,
  reference_no: Schema.String,
  providerId: Schema.String,
  receiverId: Schema.String,
  resourceId: Schema.String,
  invoiceId: Schema.String,
})

const RecordPaymentContext = Schema.Struct({
  input: Schema.Struct({
    quantity: Norn.decimal(6),
    unit: Schema.NullOr(Schema.String),
    currency: Schema.NullOr(Schema.String),
  }),
  slice: Schema.Struct({
    invoice: Schema.Struct({
      status: InvoiceStatus,
      currency: Schema.String,
      amount: Norn.decimal(6),
      amount_paid: Norn.decimal(6),
    }),
  }),
})

export const RecordPayment = Norn.action({
  id: "accounting.payment.record",
  title: "Record immutable payment evidence",
  version: "0.1.0",
  description: "Record create-only immutable payment-event evidence linked to an open invoice claim. Captures payment evidence only and does not mutate Invoice.amount_paid/status or MonetaryAccount.quantity (atomic multi-object settlement requires engine #1586).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: RecordPaymentInput,
  output: Schema.Struct({
    id: Schema.String,
  }),
  slice: [
    { name: "provider", type: Counterparty, key: { property: { from: "input", property: "providerId" } } },
    { name: "receiver", type: Counterparty, key: { property: { from: "input", property: "receiverId" } } },
    { name: "resource", type: MonetaryAccount, key: { property: { from: "input", property: "resourceId" } } },
    { name: "invoice", type: Invoice, key: { property: { from: "input", property: "invoiceId" } } },
  ],
  preconditions: [
    Norn.expression(
      RecordPaymentContext,
      (ctx) => ctx.input.quantity > 0 && ctx.input.unit === null && ctx.input.currency !== null,
      { root: "context" },
    ),
    Norn.expression(
      RecordPaymentContext,
      (ctx) => ctx.slice.invoice.status === "issued" || ctx.slice.invoice.status === "partially_paid",
      { root: "context" },
    ),
    Norn.expression(
      RecordPaymentContext,
      (ctx) => (ctx.input.currency ?? "") === ctx.slice.invoice.currency,
      { root: "context" },
    ),
    Norn.expression(
      RecordPaymentContext,
      (ctx) => ctx.input.quantity <= ctx.slice.invoice.amount - ctx.slice.invoice.amount_paid,
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "provider", targetType: Counterparty, targetIdField: "providerId" },
    { link: "receiver", targetType: Counterparty, targetIdField: "receiverId" },
    { link: "resource", targetType: MonetaryAccount, targetIdField: "resourceId" },
    { link: "settles", targetType: Invoice, targetIdField: "invoiceId" },
  ],
  writes: [{ type: Payment, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Payment,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})
