import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "fork-race", version: "1" },
  Effect.gen(function* () {
    const fiber = yield* Effect.forkChild(Norn.perform("load", loadInvoice, { invoiceId: "inv-1" }))
    const winner = yield* Effect.race(
      Norn.perform("a", loadInvoice, { invoiceId: "inv-a" }),
      Norn.perform("b", loadInvoice, { invoiceId: "inv-b" }),
    )
    const bounded = yield* Norn.perform("c", chargeCard, { invoiceId: winner.id }).pipe(Effect.timeout("1 minute"))
    return { fiber, bounded }
  }),
)
