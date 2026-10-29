// Intent: structural Schema subset -> EntityIR v2 (T7-EFX-4); Data.TaggedError ->
// canonical error schema {_tag, ...fields}; Tool/Toolkit resolved from
// effect/unstable/ai -> ActionIR projection -> MCP (T7-EFX-5).
import { Data, Schema } from "effect"
import { Tool, Toolkit } from "effect/unstable/ai"

export class InvoiceNotFound extends Data.TaggedError("InvoiceNotFound")<{ readonly invoiceId: string }> {}

export const InvoiceId = Schema.String.pipe(Schema.brand("InvoiceId"))
export const InvoiceStatus = Schema.Literals(["draft", "open", "paid"])
export const Line = Schema.Struct({
  sku: Schema.String,
  quantity: Schema.Int,
  note: Schema.optionalKey(Schema.String),
})

export class Invoice extends Schema.Class<Invoice>("Invoice")({
  id: InvoiceId,
  status: InvoiceStatus,
  lines: Schema.Array(Line),
  paidAt: Schema.NullOr(Schema.String),
  rate: Schema.Finite,
  archived: Schema.Boolean,
}) {}

export const InvoiceEvent = Schema.TaggedUnion({
  Opened: { id: InvoiceId },
  Paid: { id: InvoiceId, receiptId: Schema.String },
})

export const Kind = Schema.Union([Schema.Literal("credit"), Schema.Literal("debit"), Schema.Null])

export const GetInvoice = Tool.make("get_invoice", {
  parameters: Schema.Struct({ id: InvoiceId }),
  success: Invoice,
})

export const Billing = Toolkit.make(GetInvoice)
