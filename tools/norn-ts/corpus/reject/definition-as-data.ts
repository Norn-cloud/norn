// Flow data is JSON: a reference to a top-level Schema, flow, Norn.fn handle,
// Tool value, class, or a const that wraps one is a definition marker, not data.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import * as Defs from "../support/definitions.ts"
import { PaymentDeclined } from "../support/billing.ts"
import { Shape, lookup, otherFlow, sink, wrapped } from "../support/definitions.ts"

export default Norn.flow(
  { id: "definition-as-data", version: "1" },
  Effect.gen(function* () {
    const r = yield* Norn.perform("sink", sink, { schema: Shape, tool: lookup })
    const again = yield* Norn.perform("sink-again", sink, wrapped)
    return { r, again, flow: otherFlow, handle: sink, error: PaymentDeclined, viaNamespace: Defs.Shape }
  }),
)
