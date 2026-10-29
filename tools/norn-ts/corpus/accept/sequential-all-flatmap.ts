// Intent: Effect.all over a finite literal tuple -> sequential steps with tuple
// result slots (first failure short-circuits); pipe + flatMap with an inline
// callback -> compile-time composition; strict boolean guards -> first-hit table;
// Effect.fail with a registered tagged error -> typed failure event.
import { Effect, pipe } from "effect"
import { Norn } from "@norn/authoring"
import { PaymentDeclined, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "billing.pair", version: "1" },
  Effect.gen(function* () {
    const pair = yield* Effect.all([
      Norn.perform("first", loadInvoice, { invoiceId: "inv-1" }),
      Norn.perform("second", loadInvoice, { invoiceId: "inv-2" }),
    ])
    switch (pair[0].overdue) {
      case true:
        break
      case false:
        break
    }
    if (!pair[0].overdue || pair[1].status !== "open") {
      return yield* Effect.fail(new PaymentDeclined({ reason: "nothing to collect" }))
    }
    const ids = yield* pipe(
      Effect.succeed(pair),
      Effect.flatMap((both) => Effect.succeed({ first: both[0].id, second: both[1].id })),
    )
    return ids
  }),
)
