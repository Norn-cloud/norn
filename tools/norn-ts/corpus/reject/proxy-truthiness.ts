// Review F2: truthiness is the proxy-danger case (a symbolic proxy is always
// truthy and never === a literal). Conditions must be strict booleans.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "truthiness", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    if (invoice) {
      yield* Effect.sleep("1 second")
    }
    if (invoice.amountMinor) {
      yield* Effect.sleep("2 seconds")
    }
    if (invoice.overdue && invoice.id) {
      yield* Effect.sleep("3 seconds")
    }
    if (!invoice.id) {
      yield* Effect.sleep("4 seconds")
    }
    return { branch: invoice.overdue ? "ternary" : "other" }
  }),
)
