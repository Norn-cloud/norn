import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "equality-arithmetic", version: "1" },
  Effect.gen(function* () {
    const a = yield* Norn.perform("a", loadInvoice, { invoiceId: "inv-1" })
    const b = yield* Norn.perform("b", loadInvoice, { invoiceId: "inv-2" })
    const key = a.id
    if (a === b) {
      return { same: true }
    }
    const total = a.amountMinor + b.amountMinor
    const pick = a[key as "id"]
    const loose = a.status == "open"
    const fallback = a.id ?? "none"
    return { total, pick, loose, fallback }
  }),
)
