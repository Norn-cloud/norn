// A block-bodied Effect.map callback must return a value on every path; an empty
// body, a missing else, or a bare return projects undefined.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "map-block-undefined", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const empty = yield* Effect.map(Effect.succeed(invoice), () => {})
    const partial = yield* Effect.map(Effect.succeed(invoice), (i) => {
      if (i.overdue) {
        return i.id
      }
    })
    const bare = yield* Effect.map(Effect.succeed(invoice), () => {
      return
    })
    return { empty, partial, bare }
  }),
)
