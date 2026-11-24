import { Norn } from "@norn/authoring"
import { Schema } from "effect"

// CREATE-WRITER-1 / ACTOR-STAMP-1 / ACTOR-INDEPENDENT-1 (#1101, #1091):
// a governed create stamps its creator from the verified principal, a writer
// computes the new record's remaining fields, and a second, independent
// principal decides it. Two neutral domains share one contract.

// Accounting: an invoice submitter and a payment approver.
export const TaxCode = Norn.type({
  name: "ledger.TaxCode", title: "Tax code", version: "1.0.0",
  properties: Schema.Struct({ code: Schema.String, flat_fee: Schema.Int }),
})
export const Invoice = Norn.type({
  name: "ledger.Invoice", title: "Supplier invoice", version: "1.0.0",
  properties: Schema.Struct({
    invoice_id: Schema.String, tax_code: Schema.String, amount: Schema.Int, tax: Schema.Int, total: Schema.Int,
    submitted_by: Schema.String, status: Schema.Literals(["submitted", "approved", "rejected"]),
  }),
  // The submitter is the verified principal that created the invoice; no
  // caller, import or later update may write it.
  protected: ["submitted_by"],
  defaults: { status: "submitted", tax: 0, total: 0 },
  lifecycles: [{
    property: "status", initial: ["submitted"], terminal: ["approved", "rejected"],
    transitions: [{ id: "approve", from: ["submitted"], to: "approved" }, { id: "reject", from: ["submitted"], to: "rejected" }],
  }],
})
export const Posting = Norn.type({
  name: "ledger.Posting", title: "Payable posting", version: "1.0.0",
  properties: Schema.Struct({ posting_id: Schema.String, invoice_id: Schema.String, amount: Schema.Int }),
})
export const TotalCoversAmount = Norn.law("invoice_total_covers_amount", {
  on: Invoice,
  must: (self) => self.total >= self.amount,
  because: "An invoice total never falls below its amount.",
})

const SubmitInput = Schema.Struct({ invoice_id: Schema.String, tax_code: Schema.String, amount: Schema.Int, submitted_by: Schema.String })
const InvoiceRef = Schema.Struct({ invoice_id: Schema.String })
const WriterSlice = Schema.Struct({ code: Schema.Struct({ code: Schema.String, flat_fee: Schema.Int }) })
const WriterValues = Schema.Struct({
  tax: Schema.optionalKey(Schema.Int), total: Schema.optionalKey(Schema.Int),
  posting_id: Schema.optionalKey(Schema.String), invoice_id: Schema.optionalKey(Schema.String), amount: Schema.optionalKey(Schema.Int),
})
export const InvoiceWriter = Norn.decision({
  id: "ledger.invoice.writer", title: "Price and post a submitted invoice", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: WriterSlice, event: Schema.Struct({ input: SubmitInput, actor: Schema.String, operationId: Schema.String }) }),
  // The current closed-schema profile requires optional values/patch members.
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literals(["create", "patch"]), objectType: Schema.Literals(["ledger.Invoice", "ledger.Posting"]), key: Schema.String, values: Schema.optionalKey(WriterValues), patch: Schema.optionalKey(WriterValues) })), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})) }),
}, input => ({
  facts: [
    { op: "patch", objectType: "ledger.Invoice", key: input.event.input.invoice_id, patch: { tax: input.slice.code.flat_fee, total: input.event.input.amount + input.slice.code.flat_fee } },
    { op: "create", objectType: "ledger.Posting", key: input.event.input.invoice_id, values: { posting_id: input.event.input.invoice_id, invoice_id: input.event.input.invoice_id, amount: input.event.input.amount + input.slice.code.flat_fee } },
  ], commands: [], timers: [],
}))
export const SubmitInvoice = Norn.action({
  id: "ledger.invoice.submit", title: "Submit an invoice", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: SubmitInput, output: InvoiceRef,
  // The server stamps the submitter; a caller never supplies it.
  fromCaller: ["submitted_by"],
  slice: [{ name: "code", type: TaxCode, key: { property: { from: "input", property: "tax_code" } } }],
  synchronousRule: InvoiceWriter,
  writes: [{ type: Invoice, change: "created" }, { type: Invoice, change: "modified" }, { type: Posting, change: "created" }, { type: Posting, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Invoice, objectIdField: "invoice_id", writeMode: { kind: "create_only" } },
})
export const ApprovePayment = Norn.action({
  id: "ledger.invoice.approve", title: "Approve payment", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: InvoiceRef, output: InvoiceRef,
  writes: [{ type: Invoice, change: "modified" }],
  effect: { kind: "fire_transition", type: Invoice, objectIdField: "invoice_id", lifecycle: "status", transition: "approve" },
  // The approver is never the principal recorded as the submitter.
  independentOf: ["submitted_by"],
})

// Accounting, #1101 create plus secondary transition: raising a payable
// matches its goods receipt in the same commit. The receipt's `match` is not
// guarded, so a writer may fire it; a guarded transition it may not.
export const GoodsReceipt = Norn.type({
  name: "ledger.GoodsReceipt", title: "Goods receipt", version: "1.0.0",
  properties: Schema.Struct({ receipt_id: Schema.String, status: Schema.Literals(["received", "matched"]) }),
  defaults: { status: "received" },
  lifecycles: [{ property: "status", initial: ["received"], terminal: ["matched"], transitions: [{ id: "match", from: ["received"], to: "matched" }] }],
})
export const Payable = Norn.type({
  name: "ledger.Payable", title: "Payable", version: "1.0.0",
  properties: Schema.Struct({ payable_id: Schema.String, receipt_id: Schema.String, amount: Schema.Int, submitted_by: Schema.String }),
  protected: ["submitted_by"],
})
const RaiseInput = Schema.Struct({ payable_id: Schema.String, receipt_id: Schema.String, amount: Schema.Int, submitted_by: Schema.String })
export const MatchWriter = Norn.decision({
  id: "ledger.payable.match_writer", title: "Match the goods receipt of a payable", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: Schema.Struct({ receipt: Schema.Struct({ receipt_id: Schema.String, status: Schema.String }) }), event: Schema.Struct({ input: RaiseInput, actor: Schema.String, operationId: Schema.String }) }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literal("transition"), objectType: Schema.Literal("ledger.GoodsReceipt"), key: Schema.String, lifecycle: Schema.Literal("status"), transition: Schema.Literal("match") })), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})) }),
}, input => ({
  facts: [{ op: "transition", objectType: "ledger.GoodsReceipt", key: input.event.input.receipt_id, lifecycle: "status", transition: "match" }],
  commands: [], timers: [],
}))
export const RaisePayable = Norn.action({
  id: "ledger.payable.raise", title: "Raise a payable", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RaiseInput, output: Schema.Struct({ payable_id: Schema.String }),
  fromCaller: ["submitted_by"],
  slice: [{ name: "receipt", type: GoodsReceipt, key: { property: { from: "input", property: "receipt_id" } } }],
  synchronousRule: MatchWriter,
  writes: [{ type: Payable, change: "created" }, { type: GoodsReceipt, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Payable, objectIdField: "payable_id", writeMode: { kind: "create_only" } },
})

// Supply chain: a transfer requester and the releasing custodian.
export const Transfer = Norn.type({
  name: "supply.Transfer", title: "Stock transfer", version: "1.0.0",
  properties: Schema.Struct({ transfer_id: Schema.String, sku: Schema.String, quantity: Schema.Int, requested_by: Schema.String, status: Schema.Literals(["requested", "released"]) }),
  protected: ["requested_by"],
  defaults: { status: "requested" },
  lifecycles: [{ property: "status", initial: ["requested"], terminal: ["released"], transitions: [{ id: "release", from: ["requested"], to: "released" }] }],
})
const TransferInput = Schema.Struct({ transfer_id: Schema.String, sku: Schema.String, quantity: Schema.Int, requested_by: Schema.String })
const TransferRef = Schema.Struct({ transfer_id: Schema.String })
export const RequestTransfer = Norn.action({
  id: "supply.transfer.request", title: "Request a transfer", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: TransferInput, output: TransferRef,
  fromCaller: ["requested_by"],
  writes: [{ type: Transfer, change: "created" }],
  effect: { kind: "upsert_input_object", type: Transfer, objectIdField: "transfer_id", writeMode: { kind: "create_only" } },
})
export const ReleaseTransfer = Norn.action({
  id: "supply.transfer.release", title: "Release a transfer", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: TransferRef, output: TransferRef,
  writes: [{ type: Transfer, change: "modified" }],
  effect: { kind: "fire_transition", type: Transfer, objectIdField: "transfer_id", lifecycle: "status", transition: "release" },
  independentOf: ["requested_by"],
})
