import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "switch", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    switch (invoice.status) {
      case "draft":
      case "open":
        yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id })
    }
    switch (invoice.id) {
      default:
        break
    }
    return { id: invoice.id }
  }),
)
