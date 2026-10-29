// Running an Effect is never part of a definition; no Effect code runs server-side.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

const flow = Norn.flow(
  { id: "run-promise", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    return { id: invoice.id }
  }),
)

export const result = Effect.runPromise(Effect.succeed(flow))
