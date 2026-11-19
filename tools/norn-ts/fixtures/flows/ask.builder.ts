// Ask: a person decides (`norn.decision@1`), and the flow routes on their
// choice. The decision step parks in the cell until a decider resolves it.
import { DECISION_FN, X, flow, perform, route, succeed } from "../../src/builder/index.ts"

const option = (id: string, label: string, action: string) =>
  X.record({ id: X.lit(id), label: X.lit(label), scope: X.lit("This refund only"), effect: X.record({ action: X.lit(action) }) })

export default flow({ id: "ops.approve_refund", version: "1.0.0", title: "Approve a refund" }, [
  perform("ask_review", DECISION_FN, X.record({
    question: X.lit("Refund order 1042 in full?"),
    risk: X.lit("high"),
    options: X.list(option("approve", "Approve the refund", "ops.refund.approve"), option("reject", "Reject the refund", "ops.refund.reject")),
    agentPick: X.lit("approve"),
    decider: X.record({ roles: X.list(X.lit("admin"), X.lit("owner")) }),
    expiresInMs: X.lit(86_400_000),
  }), { bind: "review", label: "Review the refund" }),
  route([{ when: X.eq(X.ref("review", "choice"), X.lit("approve")), then: [succeed(X.record({ outcome: X.lit("approved") }))] }], [
    succeed(X.record({ outcome: X.lit("rejected") })),
  ]),
])
