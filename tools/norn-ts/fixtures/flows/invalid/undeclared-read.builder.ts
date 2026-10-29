// NORN-FLOW-SLICE: the terminal fact reads a field the Function's output
// schema does not declare.
import { X, flow, perform, succeed } from "../../../src/builder/index.ts"
import { loadInvoice } from "../support/billing.builder.ts"

export default flow({ id: "billing.undeclared_read", version: "1.0.0" }, [
  perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") }), { bind: "invoice" }),
  succeed(X.ref("invoice", "customer")),
])
