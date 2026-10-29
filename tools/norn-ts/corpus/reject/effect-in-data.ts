// Effects run only through yield*: an effect passed as a perform input, wrapped
// by Effect.succeed, or nested in a data object is stored, never executed.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard } from "../support/billing.ts"
import { sink } from "../support/definitions.ts"

export default Norn.flow(
  { id: "effect-in-data", version: "1" },
  Effect.gen(function* () {
    const a = yield* Norn.perform("outer", sink, Norn.perform("inner", chargeCard, { invoiceId: "inv-1" }))
    const b = yield* Effect.succeed(Norn.perform("wrapped", chargeCard, { invoiceId: "inv-2" }))
    const c = { pending: Norn.perform("nested", chargeCard, { invoiceId: "inv-3" }).pipe(Effect.map((x) => x.receiptId)) }
    return { a, b, c }
  }),
)
