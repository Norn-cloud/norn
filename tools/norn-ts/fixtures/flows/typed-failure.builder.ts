// Typed failure: catchTag routes the declared PaymentDeclined failure of a
// perform to a recovery perform; the undeclared-by-handler CardExpired tag is
// uncaught and ends the Run in failed_card_expired with the failure record.
import { X, catchTag, flow, perform, succeed } from "../../src/builder/index.ts"
import { chargeCard, notify } from "./support/billing.builder.ts"

export default flow({ id: "billing.typed_failure", version: "1.0.0" }, [
  catchTag(
    [
      perform("charge", chargeCard, X.record({ invoiceId: X.lit("inv-1") }), { bind: "receipt" }),
      succeed(X.record({ outcome: X.lit("charged"), detail: X.ref("receipt", "receiptId") })),
    ],
    "PaymentDeclined",
    [
      perform("tell", notify, X.record({ invoiceId: X.lit("inv-1"), outcome: X.ref("declined", "reason") })),
      succeed(X.record({ outcome: X.lit("declined"), detail: X.ref("declined", "reason") })),
    ],
    { as: "declined" },
  ),
])
