// NORN-FLOW-RESULT: a path completes without an explicit result.
import { X, flow, perform } from "../../../src/builder/index.ts"
import { loadInvoice } from "../support/billing.builder.ts"

export default flow({ id: "billing.no_result", version: "1.0.0" }, [
  perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") })),
])
