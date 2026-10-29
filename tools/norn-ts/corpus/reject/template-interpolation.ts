// Runtime template interpolation coerces values (review §6 probe); v1 rejects it.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "template", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const memo = `invoice ${invoice.id} owes ${invoice.amountMinor}`
    const fixed = `definition-time constants are fine`
    return { memo, fixed }
  }),
)
