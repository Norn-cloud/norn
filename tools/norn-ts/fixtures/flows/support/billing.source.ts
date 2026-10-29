import { Data } from "effect"
import { Norn } from "@norn/authoring"

export class PaymentDeclined extends Data.TaggedError("PaymentDeclined")<{ readonly reason: string }> {}
export class CardExpired extends Data.TaggedError("CardExpired")<{}> {}
export class InvoiceVoid extends Data.TaggedError("InvoiceVoid")<{ readonly invoiceId: string }> {}

export const loadInvoice = Norn.fn<{
  readonly invoiceId: string
}, {
  readonly id: string
  readonly status: "draft" | "open" | "paid"
  readonly overdue: boolean
  readonly amountMinor: number
}>({ id: "billing.load_invoice", version: "1.0.0" })

export const chargeCard = Norn.fn<{
  readonly invoiceId: string
}, { readonly receiptId: string }, PaymentDeclined | CardExpired>({
  id: "billing.charge_card", version: "1.0.0",
})

export const notify = Norn.fn<{
  readonly invoiceId: string
  readonly outcome: string
}, { readonly delivered: boolean }>({ id: "billing.notify", version: "1.0.0" })
