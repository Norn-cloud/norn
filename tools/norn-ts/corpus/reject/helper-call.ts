// Helper functions (including recursive ones) are calls the compiler cannot see.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

function normalise(id: string): string {
  return normalise(id)
}

export default Norn.flow(
  { id: "helper-call", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const upper = invoice.id.toUpperCase()
    const again = normalise(upper)
    return { again }
  }),
)
