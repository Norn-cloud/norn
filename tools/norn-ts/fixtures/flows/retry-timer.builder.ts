// Finite retry timer: Schedule.max([Schedule.exponential("1 second", 2),
// Schedule.recurs(3)]) around one perform: 3 retries (4 attempts), delays
// 1 s, 2 s, 4 s, then the failure routes on; a sleep before it is a timer
// request and timer event.
import { X, flow, perform, retry, seconds, sleep, succeed } from "../../src/builder/index.ts"
import { chargeCard } from "./support/billing.builder.ts"

export default flow({ id: "billing.retry_timer", version: "1.0.0" }, [
  sleep("settle", seconds(30)),
  retry(perform("charge", chargeCard, X.record({ invoiceId: X.lit("inv-1") }), { bind: "receipt" }), {
    recurs: 3,
    delay: { kind: "exponential", baseMillis: 1000, factor: 2 },
  }),
  succeed(X.record({ receipt: X.ref("receipt", "receiptId") })),
])
