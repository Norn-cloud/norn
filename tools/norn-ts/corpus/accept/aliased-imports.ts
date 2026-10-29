// Intent: recognition is by resolved symbol. Renamed imports, a namespace import,
// a subpath import, and an aliased Norn namespace all resolve to the admitted
// declarations and lower exactly like the canonical spelling.
import { Effect as Fx } from "effect"
import * as E from "effect"
import { gen } from "effect/Effect"
import { Norn as N } from "@norn/authoring"
import { chargeCard } from "../support/billing.ts"

export default N.flow(
  { id: "aliases", version: "1" },
  gen(function* () {
    const receipt = yield* N.perform("charge", chargeCard, { invoiceId: "inv-1" })
    const same = yield* Fx.succeed(receipt.receiptId)
    const out = yield* E.Effect.map(Fx.succeed(same), (id) => ({ receiptId: id }))
    return out
  }),
)
