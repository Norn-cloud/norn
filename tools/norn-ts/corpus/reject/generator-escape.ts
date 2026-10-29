// Effect.gen / perform outside the Norn.flow body escape the compiler; flow bodies
// are inline, and effect values are never stored, only bound with yield*.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"

export const body = Effect.gen(function* () {
  const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
  return { id: invoice.id }
})

export const inline = Norn.flow(
  { id: "stored-effect", version: "1" },
  Effect.gen(function* () {
    const step = Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const first = yield* step
    return { id: first.id }
  }),
)

export default Norn.flow({ id: "escape", version: "1" }, body)
