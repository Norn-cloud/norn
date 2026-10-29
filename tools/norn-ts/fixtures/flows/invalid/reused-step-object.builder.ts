// NORN-FLOW-UNSUPPORTED-SYNTAX: a durable step object cannot occur twice,
// even when both occurrences share the same object identity.
import { X, flow, perform, route, sleep, succeed, seconds } from "../../../src/builder/index.ts"
import { chargeCard } from "../support/billing.builder.ts"

const charge = perform("charge", chargeCard, X.record({ invoiceId: X.lit("inv-1") }))

export default flow({ id: "billing.reused_step_object", version: "1.0.0" }, [
  route([{ when: X.lit(true), then: [charge, sleep("wait", seconds(1)), charge] }]),
  succeed(X.lit(null)),
])
