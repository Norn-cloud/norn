import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice, notify } from "../flows/support/billing.source.ts"

export default Norn.flow({ id: "billing.nested_projection", version: "1.0.0" }, Effect.gen(function* () {
  const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
  const projected = yield* Effect.map(Effect.succeed(invoice), (input) => {
    if (input.overdue) return { summary: { label: "overdue", id: input.id } }
    return { summary: { label: "current", id: input.id } }
  })
  yield* Norn.perform("tell", notify, { invoiceId: projected.summary.id, outcome: projected.summary.label })
  return { label: projected.summary.label, id: projected.summary.id }
}))
