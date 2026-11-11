// Array closures are admitted only in expression callbacks (laws, guards,
// derivations, decisions); in flow data they would be arbitrary predicates.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "closure-in-flow", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const tags = [invoice.id, "x"]
    return { any: tags.some((t) => t === "x") }
  }),
)
