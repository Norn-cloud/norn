// NORN-FLOW-UNSUPPORTED-SYNTAX: one step id names two sites (step ids are
// the stable, version-scoped identity of a durable step).
import { X, flow, perform, succeed } from "../../../src/builder/index.ts"
import { loadInvoice } from "../support/billing.builder.ts"

export default flow({ id: "billing.duplicate_step", version: "1.0.0" }, [
  perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") })),
  perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-2") })),
  succeed(X.lit(null)),
])
