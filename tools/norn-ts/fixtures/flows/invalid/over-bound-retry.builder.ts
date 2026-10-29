// NORN-FLOW-UNBOUNDED: recurs(50) is over the static bound; it is rejected,
// never clamped.
import { X, flow, perform, retry, succeed } from "../../../src/builder/index.ts"
import { chargeCard } from "../support/billing.builder.ts"

export default flow({ id: "billing.over_bound_retry", version: "1.0.0" }, [
  retry(perform("charge", chargeCard, X.record({ invoiceId: X.lit("inv-1") })), { recurs: 50 }),
  succeed(X.lit(null)),
])
