// Concurrency options on Effect.all and a non-literal collection are rejected;
// v1 Effect.all is a finite literal tuple, sequential.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

const ids = ["inv-1", "inv-2"] as const

export default Norn.flow(
  { id: "concurrent-all", version: "1" },
  Effect.gen(function* () {
    const pair = yield* Effect.all(
      [Norn.perform("a", loadInvoice, { invoiceId: "inv-1" }), Norn.perform("b", loadInvoice, { invoiceId: "inv-2" })],
      { concurrency: "unbounded" },
    )
    const many = yield* Effect.all(ids.map((id) => Norn.perform("each", loadInvoice, { invoiceId: id })))
    return { pair, many }
  }),
)
