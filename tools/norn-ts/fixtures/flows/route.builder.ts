// Route: a first-hit route over the accepted result (`switch` on a literal
// union plus an `if`), each branch ending in a terminal fact or continuing.
import { X, flow, perform, route, succeed } from "../../src/builder/index.ts"
import { loadInvoice, notify } from "./support/billing.builder.ts"

export default flow({ id: "billing.route", version: "1.0.0" }, [
  perform("load", loadInvoice, X.record({ invoiceId: X.lit("inv-1") }), { bind: "invoice" }),
  route([
    { when: X.eq(X.ref("invoice", "status"), X.lit("paid")), then: [succeed(X.record({ outcome: X.lit("already-paid") }))] },
    { when: X.eq(X.ref("invoice", "status"), X.lit("draft")), then: [succeed(X.record({ outcome: X.lit("not-issued") }))] },
  ]),
  route(
    [
      {
        when: X.and(X.eq(X.ref("invoice", "overdue"), X.lit(true)), X.gt(X.ref("invoice", "amountMinor"), X.lit(0))),
        then: [perform("remind", notify, X.record({ invoiceId: X.ref("invoice", "id"), outcome: X.lit("overdue") }))],
      },
    ],
  ),
  succeed(X.record({ outcome: X.lit("open") })),
])
