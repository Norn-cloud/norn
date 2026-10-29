// Intent: top-level JSON data consts (literals, negative finite numbers, arrays,
// objects, `as const`), imported directly or through a namespace, are flow data
// -> inlined constants; a block-bodied Effect.map callback that returns on every
// path -> ZEN projection with a first-hit table; an early return carries an
// explicit result -> terminal transition. A const that references the same data
// const from several properties (`twice`) is still data.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "../support/billing.ts"
import * as Defs from "../support/definitions.ts"
import { limits, twice } from "../support/definitions.ts"

const fallback = { reason: "none", attempts: limits.maxAttempts, floor: -1 }

export default Norn.flow(
  { id: "data-and-results", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    if (invoice.status === "draft") {
      return { label: "draft", fallback, limits: Defs.limits }
    }
    const label = yield* Effect.map(Effect.succeed(invoice), (i) => {
      if (i.overdue) {
        return "overdue"
      } else {
        return "current"
      }
    })
    return { label, fallback, limits, twice }
  }),
)
