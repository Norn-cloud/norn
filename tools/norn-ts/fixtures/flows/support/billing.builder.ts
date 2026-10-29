// Hand-written builder handles shared by the flow fixtures (T7-FLOW-1): the
// builder twins of corpus/support/billing.ts.
import { S, fn, taggedError } from "../../../src/builder/index.ts"

export const PaymentDeclined = taggedError("PaymentDeclined", S.record({ reason: S.text() }))

export const CardExpired = taggedError("CardExpired", S.record({}))

export const loadInvoice = fn({
  id: "billing.load_invoice",
  version: "1.0.0",
  input: S.record({ invoiceId: S.text() }),
  output: S.record({
    id: S.text(),
    status: S.text(["draft", "open", "paid"]),
    overdue: S.boolean,
    amountMinor: S.number,
  }),
})

export const chargeCard = fn({
  id: "billing.charge_card",
  version: "1.0.0",
  input: S.record({ invoiceId: S.text() }),
  output: S.record({ receiptId: S.text() }),
  errors: [PaymentDeclined, CardExpired],
})

export const notify = fn({
  id: "billing.notify",
  version: "1.0.0",
  input: S.record({ invoiceId: S.text(), outcome: S.text() }),
  output: S.record({ delivered: S.boolean }),
})
