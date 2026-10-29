import { Effect, pipe } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice, notify } from "../flows/support/billing.source.ts"

export default Norn.flow({ id: "billing.pipe_flat_map", version: "1.0.0" }, Effect.gen(function* () {
  const invoiceId = yield* pipe(
    Norn.perform("load", loadInvoice, { invoiceId: "inv-1" }),
    Effect.flatMap((invoice) => {
      return Effect.succeed({ invoiceId: invoice.id })
    }),
    Effect.map((record) => record.invoiceId),
  )
  yield* Norn.perform("notify", notify, { invoiceId, outcome: "delivered" })
  return invoiceId
}))
