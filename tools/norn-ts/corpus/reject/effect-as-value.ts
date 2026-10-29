// Effects are run with yield*, never used as data: a returned, mapped, or stored
// effect is a nested non-JSON value whose command never executes. Tagged errors
// are constructed only for Effect.fail; declared-only values and non-finite
// literals have nothing faithful to extract.
import { Effect, pipe } from "effect"
import { Norn } from "@norn/authoring"
import { PaymentDeclined, chargeCard } from "../support/billing.ts"

declare const injected: string

export const first = Norn.flow(
  { id: "returned-effect", version: "1" },
  Effect.gen(function* () {
    return Norn.perform("charge", chargeCard, { invoiceId: "inv-1" })
  }),
)

export const second = Norn.flow(
  { id: "mapped-effect", version: "1" },
  Effect.gen(function* () {
    const nested = yield* Effect.map(Effect.succeed(1), () => Norn.perform("charge", chargeCard, { invoiceId: "inv-1" }))
    const stored = pipe(Norn.perform("again", chargeCard, { invoiceId: "inv-2" }), Effect.map((r) => r.receiptId))
    return { nested, stored }
  }),
)

export const third = Norn.flow(
  { id: "error-as-data", version: "1" },
  Effect.gen(function* () {
    const huge = 1e400
    return { error: new PaymentDeclined({ reason: injected }), huge }
  }),
)
