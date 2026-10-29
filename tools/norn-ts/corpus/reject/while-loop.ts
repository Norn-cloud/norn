import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "while-loop", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    while (invoice.overdue === true) {
      yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id })
    }
    for (const id of ["a", "b"]) {
      yield* Norn.perform("each", loadInvoice, { invoiceId: id })
    }
    return { id: invoice.id }
  }),
)
