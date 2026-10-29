import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

let attempts = 0
var label = "collect"

export default Norn.flow(
  { id: "mutable-capture", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    attempts = attempts + 1
    return { id: invoice.id, label, attempts }
  }),
)
