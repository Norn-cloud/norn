import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "./support/billing.source.ts"

export default Norn.flow(
  { id: "billing.typed_failure", version: "1.0.0" },
  Effect.gen(function* () {
    const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: "inv-1" })
    return { outcome: "charged", detail: receipt.receiptId }
  }).pipe(Effect.catchTag("PaymentDeclined", (declined) => Effect.gen(function* () {
    yield* Norn.perform("tell", notify, { invoiceId: "inv-1", outcome: declined.reason })
    return { outcome: "declined", detail: declined.reason }
  }))),
)
