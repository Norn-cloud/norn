import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "../../flows/support/billing.source.ts"

export default Norn.flow(
  { id: "billing.sequence", version: "1.0.0" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id })
    return { invoice: invoice.id, receipt: receipt.receiptId }
  }),
)
