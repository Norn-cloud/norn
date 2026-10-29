// NORN-FLOW-TAG: catchTag names a tag that no Function performed in its body
// declares (loadInvoice declares no errors) and nothing there raises.
import { X, catchTag, flow, perform, succeed } from "../../../src/builder/index.ts"
import { loadInvoice } from "../support/billing.builder.ts"

export default flow({ id: "billing.undeclared_tag", version: "1.0.0" }, [
  catchTag(
    [perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") })), succeed(X.lit("loaded"))],
    "PaymentDeclined",
    [succeed(X.lit("declined"))],
  ),
])
