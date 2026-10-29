import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice, notify } from "../flows/support/billing.source.ts"

export default Norn.flow({ id: "billing.branch_continuation", version: "1.0.0" }, Effect.gen(function* () {
  const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
  const label = yield* Effect.map(Effect.succeed(invoice), (input) => {
    if (input.overdue) return "overdue"
    return "current"
  })
  yield* Norn.perform("tell", notify, { invoiceId: invoice.id, outcome: label })
  return { label }
}))
