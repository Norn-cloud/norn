import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "./support/billing.source.ts"

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
