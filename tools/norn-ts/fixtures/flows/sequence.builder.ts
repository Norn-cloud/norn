// Sequence: two performs in order; the second reads the first's result, and
// the terminal fact reads both (the first across a durable boundary, from the
// result slot).
import { X, flow, perform, succeed } from "../../src/builder/index.ts"
import { chargeCard, loadInvoice } from "./support/billing.builder.ts"

export default flow({ id: "billing.sequence", version: "1.0.0" }, [
  perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") }), { bind: "invoice" }),
  perform("charge", chargeCard, X.record({ invoiceId: X.ref("invoice", "id") }), { bind: "receipt" }),
  succeed(X.record({ invoice: X.ref("invoice", "id"), receipt: X.ref("receipt", "receiptId") })),
])
