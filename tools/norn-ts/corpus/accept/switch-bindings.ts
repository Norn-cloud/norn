import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"

export default Norn.flow({ id: "billing.switch_bindings", version: "1" }, Effect.gen(function* () {
  const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "id" })
  switch (invoice.status) {
    case "open": break
    case "paid":
    case "draft": return { continue: false }
    default: return { continue: false }
  }
  return { continue: true }
}))
