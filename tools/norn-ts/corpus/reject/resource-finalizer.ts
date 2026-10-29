// Upstream resource finalizers are not business compensation (review §2).
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../support/billing.ts"

export default Norn.flow(
  { id: "resource", version: "1" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" }).pipe(
      Effect.ensuring(Effect.succeed("cleanup")),
    )
    const held = yield* Effect.acquireUseRelease(
      Effect.succeed(invoice.id),
      (id) => Effect.succeed(id),
      () => Effect.succeed("release"),
    )
    return { held }
  }),
)
