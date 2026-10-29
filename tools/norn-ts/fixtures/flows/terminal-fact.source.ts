import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { chargeCard, loadInvoice, notify, InvoiceVoid } from "./support/billing.source.ts"

export default Norn.flow(
  { id: "billing.terminal_fact", version: "1.0.0" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    if (invoice.status === "draft") {
      yield* Effect.fail(new InvoiceVoid({ invoiceId: invoice.id }))
    }
    return { outcome: "issued", invoice: invoice.id }
  }).pipe(Effect.catchTag("InvoiceVoid", (voided) => Effect.succeed({ outcome: "void", invoice: voided.invoiceId }))),
)
