import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "./support/billing.source.ts"

export default Norn.flow(
  { id: "billing.route", version: "1.0.0" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    switch (invoice.status) {
      case "paid": return { outcome: "already-paid" }
      case "draft": return { outcome: "not-issued" }
      case "open": break
    }
    if (invoice.overdue === true && invoice.amountMinor > 0) {
      yield* Norn.perform("remind", notify, { invoiceId: invoice.id, outcome: "overdue" })
    }
    return { outcome: "open" }
  }),
)
