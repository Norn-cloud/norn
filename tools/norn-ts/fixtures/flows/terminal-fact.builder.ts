// Terminal fact: a flow-raised typed failure (Effect.fail) caught in the same
// step, and the terminal result written to the Run's result slot.
import { S, X, catchTag, fail, flow, perform, route, succeed, taggedError } from "../../src/builder/index.ts"
import { loadInvoice } from "./support/billing.builder.ts"

const InvoiceVoid = taggedError("InvoiceVoid", S.record({ invoiceId: S.text() }))

export default flow({ id: "billing.terminal_fact", version: "1.0.0" }, [
  catchTag(
    [
      perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") }), { bind: "invoice" }),
      route([{ when: X.eq(X.ref("invoice", "status"), X.lit("draft")), then: [fail(InvoiceVoid, X.record({ invoiceId: X.ref("invoice", "id") }))] }]),
      succeed(X.record({ outcome: X.lit("issued"), invoice: X.ref("invoice", "id") })),
    ],
    "InvoiceVoid",
    [succeed(X.record({ outcome: X.lit("void"), invoice: X.ref("voided", "invoiceId") }))],
    { as: "voided" },
  ),
])
