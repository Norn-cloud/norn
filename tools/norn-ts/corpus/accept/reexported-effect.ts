// Intent: a benign local re-export (`export { Effect } from "effect"`) still
// resolves to the pinned declarations, so it is admitted; recurs-only retry is
// the minimal finite policy (2 retries, 3 attempts).
import { Effect, Schedule } from "../support/reexport-effect.ts"
import { Norn } from "@norn/authoring"
import { chargeCard } from "../support/billing.ts"

export default Norn.flow(
  { id: "reexport", version: "1" },
  Effect.gen(function* () {
    const receipt = yield* Effect.retry(Norn.perform("charge", chargeCard, { invoiceId: "inv-1" }), Schedule.recurs(2))
    return { receiptId: receipt.receiptId }
  }),
)
