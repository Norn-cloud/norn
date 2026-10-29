import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../flows/support/billing.source.ts"

export default Norn.flow({ id: "billing.caught_continuation", version: "1.0.0" }, Effect.gen(function* () {
  const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
  const label = yield* Effect.map(Effect.succeed(invoice), (input) => {
    if (input.overdue) return "overdue"
    return "current"
  })
  const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id }).pipe(
    Effect.catchTag("PaymentDeclined", (error) => Effect.succeed({ receiptId: label })),
  )
  return { label, receipt: receipt.receiptId }
}))
