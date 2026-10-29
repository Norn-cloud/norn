// Every flow path returns an explicit result: a bare `return` yields undefined,
// which is not a JSON value.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "bare-return", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    if (invoice.overdue) {
      return
    }
    return { id: invoice.id }
  }),
)
