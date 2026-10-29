// NORN-FLOW-VALUE: the perform input does not conform to the Function's
// closed input schema (a missing required field and an undeclared one).
import { X, flow, perform, succeed } from "../../../src/builder/index.ts"
import { chargeCard } from "../support/billing.builder.ts"

export default flow({ id: "billing.wrong_input", version: "1.0.0" }, [
  perform("charge", chargeCard, X.record({ invoice: X.lit("inv-1") })),
  succeed(X.lit(null)),
])
