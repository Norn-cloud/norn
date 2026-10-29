// NORN-FLOW-UNBOUNDED: a retry policy with a delay but no finite recurs(n).
import { X, flow, perform, retry, succeed } from "../../../src/builder/index.ts"
import { chargeCard } from "../support/billing.builder.ts"

export default flow({ id: "billing.unbounded_retry", version: "1.0.0" }, [
  retry(perform("charge", chargeCard, X.record({ invoiceId: X.lit("inv-1") })), { delay: { kind: "spaced", millis: 1000 } }),
  succeed(X.lit(null)),
])
