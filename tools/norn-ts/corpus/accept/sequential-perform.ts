// Intent: flow row -> Run lifecycle; two perform steps (load, charge) as commands
// + waiting facts; switch on a literal union -> first-hit route with exhaustive
// coverage; retry around one perform -> attempt counter (3 retries, 4 attempts)
// + exponential retry timer; sleep -> timer request; catchTag -> failure route to
// a terminal "declined" result fact.
import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "billing.collect", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    switch (invoice.status) {
      case "paid":
        return { outcome: "already-paid", detail: invoice.id }
      case "draft":
        return { outcome: "not-issued", detail: invoice.id }
      case "open":
        break
    }
    if (invoice.overdue === true && invoice.amountMinor > 0) {
      yield* Effect.sleep("5 minutes")
    }
    const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id }).pipe(
      Effect.retry(Schedule.max([Schedule.exponential("1 second", 2), Schedule.recurs(3)])),
    )
    return { outcome: "charged", detail: receipt.receiptId }
  }).pipe(
    Effect.catchTag("PaymentDeclined", (declined) => Effect.succeed({ outcome: "declined", detail: declined.reason })),
  ),
)
