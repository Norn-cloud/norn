// Arbitrary JS predicates would make the compiler a second expression engine (review F6).
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "predicate", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" }).pipe(
      Effect.filterOrFail((i) => i.amountMinor > 100, () => "too small"),
    )
    return { id: invoice.id }
  }),
)
