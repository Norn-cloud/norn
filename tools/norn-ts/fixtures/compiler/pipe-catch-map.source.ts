import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, notify } from "../flows/support/billing.source.ts"

export default Norn.flow({ id: "billing.pipe_catch_map", version: "1.0.0" }, Effect.gen(function* () {
  const receiptId = yield* Norn.perform("charge", chargeCard, { invoiceId: "inv-1" }).pipe(
    Effect.catchTag("PaymentDeclined", () => {
      return Effect.succeed({ receiptId: "declined" })
    }),
    Effect.catchTag("CardExpired", () => Effect.succeed({ receiptId: "expired" })),
    Effect.map((receipt) => receipt.receiptId),
  )
  yield* Norn.perform("notify", notify, { invoiceId: "inv-1", outcome: receiptId })
  return receiptId
}))
