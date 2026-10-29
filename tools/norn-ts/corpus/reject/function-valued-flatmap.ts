// Review F1: a function-valued flatMap continuation is opaque (JSON drops it).
// Only inline callbacks passed directly to an admitted combinator are syntax.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"
import type { Invoice } from "../support/billing.ts"

const next = (invoice: Invoice) => Norn.perform("charge", chargeCard, { invoiceId: invoice.id })

export default Norn.flow(
  { id: "function-valued", version: "1" },
  Effect.gen(function* () {
    const receipt = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" }).pipe(Effect.flatMap(next))
    return { receiptId: receipt.receiptId }
  }),
)
