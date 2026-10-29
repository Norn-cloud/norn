import { Effect, pipe } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../flows/support/billing.source.ts"

export default Norn.flow({ id: "billing.composition", version: "1.0.0" }, Effect.gen(function* () {
  const pair = yield* Effect.all([
    Norn.perform("first", loadInvoice, { invoiceId: "inv-1" }),
    Norn.perform("second", loadInvoice, { invoiceId: "inv-2" }),
  ])
  const label = yield* Effect.map(Effect.succeed(pair[0]), (invoice) => {
    if (invoice.overdue) return "overdue"
    return "current"
  })
  const ids = yield* pipe(Effect.succeed(pair), Effect.flatMap((both) => Effect.succeed({
    first: both[0].id,
    second: both[1].id,
  })))
  return { label, first: ids.first, second: ids.second }
}))
