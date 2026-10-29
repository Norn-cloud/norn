import { Effect, Schedule, Schema } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "../../flows/support/billing.source.ts"

export default Norn.flow(
  { id: "billing.retry_timer", version: "1.0.0" },
  Effect.gen(function* () {
    const settle = yield* Effect.sleep("30 seconds")
    const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: "inv-1" }).pipe(
      Effect.retry(Schedule.max([Schedule.exponential("1 second", 2), Schedule.recurs(3)])),
    )
    return { receipt: receipt.receiptId }
  }),
)

// A module may declare ontology definitions next to its flow; the pack lowers both.
export const Invoice = Norn.type({
  name: "billing.Invoice",
  title: "Invoice",
  version: "1.0.0",
  properties: Schema.Struct({ invoice_id: Schema.String }),
  identity: [{ name: "primary", properties: ["invoice_id"] }],
})
