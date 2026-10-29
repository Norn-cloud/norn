// Shared definitions used by several fixtures: a Function handle and a tagged error.
import { Data } from "effect"
import { Norn } from "@norn/authoring"

export interface Invoice {
  readonly id: string
  readonly status: "draft" | "open" | "paid"
  readonly overdue: boolean
  readonly amountMinor: number
}

export class PaymentDeclined extends Data.TaggedError("PaymentDeclined")<{ readonly reason: string }> {}

export const chargeCard = Norn.fn<{ readonly invoiceId: string }, { readonly receiptId: string }, PaymentDeclined>({
  id: "billing.charge-card",
  version: "1",
})

export const loadInvoice = Norn.fn<{ readonly invoiceId: string }, Invoice>({
  id: "billing.load-invoice",
  version: "1",
})
