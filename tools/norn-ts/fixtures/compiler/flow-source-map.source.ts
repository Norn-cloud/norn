import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "../flows/support/billing.source.ts"

export default Norn.flow(
  { id: "billing.source_map", version: "1.0.0" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const status = yield* Effect.map(Effect.succeed(invoice.status), (value) => {
      return value
    })
    const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id }).pipe(
      Effect.retry(Schedule.recurs(1)),
    )
    const notices = yield* Effect.all([
      Norn.perform("parallel_notice", notify, { invoiceId: invoice.id, outcome: "started" }),
    ])
    switch (status) {
      case "paid":
        yield* Norn.perform("paid_notice", notify, { invoiceId: invoice.id, outcome: "paid" })
        break
      case "draft":
        yield* Effect.sleep("1 second")
        break
      default:
        yield* Norn.perform("open_notice", notify, { invoiceId: invoice.id, outcome: "open" })
        break
    }
    if (invoice.overdue) {
      yield* Norn.perform("overdue_notice", notify, { invoiceId: invoice.id, outcome: "overdue" })
    } else {
      yield* Effect.fail(new InvoiceVoid({ invoiceId: invoice.id }))
    }
    return { receipt: receipt.receiptId, delivered: notices[0].delivered }
  }).pipe(
    Effect.catchTag("PaymentDeclined", (declined) => Effect.gen(function* () {
      yield* Norn.perform("declined_notice", notify, { invoiceId: "inv-1", outcome: declined.reason })
      return { receipt: "declined", delivered: true }
    })),
  ),
)
