import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "sync-promise", version: "1" },
  Effect.gen(function* () {
    const a = yield* Effect.sync(() => "computed")
    const b = yield* Effect.promise(async () => "fetched")
    const c = yield* Effect.tryPromise(() => Promise.resolve("tried"))
    const d = yield* Effect.suspend(() => Norn.perform("load", loadInvoice, { invoiceId: "inv-1" }))
    const e = yield* Effect.map(Effect.succeed(1), async (v) => await Promise.resolve(v))
    return { a, b, c, d, e }
  }),
)
