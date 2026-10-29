import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

const stepName = "charge"
const wait = "5 minutes"

export default Norn.flow(
  { id: "non-literal", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const receipt = yield* Norn.perform(stepName, chargeCard, { invoiceId: invoice.id })
    yield* Effect.sleep(wait)
    return { receiptId: receipt.receiptId }
  }),
)
