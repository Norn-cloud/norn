import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard } from "../support/billing.ts"

export default Norn.flow(
  { id: "unbounded", version: "1" },
  Effect.gen(function* () {
    const a = yield* Norn.perform("a", chargeCard, { invoiceId: "inv-1" }).pipe(Effect.retry(Schedule.spaced("1 second")))
    const b = yield* Norn.perform("b", chargeCard, { invoiceId: "inv-1" }).pipe(Effect.retry(Schedule.forever))
    const c = yield* Norn.perform("c", chargeCard, { invoiceId: "inv-1" }).pipe(
      Effect.retry(Schedule.max([Schedule.exponential("1 second"), Schedule.recurs(3)]).pipe(Schedule.jittered)),
    )
    return { a, b, c }
  }),
)
