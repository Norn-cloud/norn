// Review F3: a typed Effect.fail never throws into the generator, so this catch
// can never observe PaymentDeclined. Lowering it to catchTag would change the language.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "try-catch", version: "1" },
  Effect.gen(function* () {
    try {
      yield* Norn.perform("charge", chargeCard, { invoiceId: "inv-1" })
    } catch {
      yield* Norn.perform("fallback", chargeCard, { invoiceId: "inv-2" })
    }
    return { charged: true }
  }),
)
