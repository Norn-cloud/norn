// Decision followed by a read-only question step (QUESTION-STEP-1): an admin
// decider's approval cannot escalate the run starter's authority on a
// subsequent norn.question@1 step, and STEP-PIN-1 detects question definition
// drift while parked on the decision.
import { DECISION_FN, S, X, actionFn, catchTag, flow, perform, questionFn, route, succeed } from "../../src/builder/index.ts"

const CHECK_STOCK = questionFn("freshness.order_line_qty")
const CONVERT = actionFn(
  "constitution.melt.convert",
  S.record({ id: S.text(), input_fine_g: S.text(), output_fine_g: S.text() }),
)

const option = (id: string, label: string) =>
  X.record({ id: X.lit(id), label: X.lit(label), scope: X.lit("Melt 7 only"), effect: X.record({ action: X.lit("constitution.melt.convert") }) })

const MELT = X.record({ id: X.lit("melt-7"), input_fine_g: X.lit("12"), output_fine_g: X.lit("10") })

export default flow({ id: "ops.decide_then_question", version: "1.0.0", title: "Decide then read a question" }, [
  perform("ask_review", DECISION_FN, X.record({
    question: X.lit("Approve checking stock and converting melt 7?"),
    risk: X.lit("medium"),
    options: X.list(option("approve", "Approve"), option("reject", "Reject")),
    decider: X.record({ roles: X.list(X.lit("admin")) }),
  }), { bind: "review" }),
  route(
    [{ when: X.eq(X.ref("review", "choice"), X.lit("approve")), then: [
      catchTag(
        [
          perform("check_question", CHECK_STOCK, X.record({ question: X.lit("freshness.order_line_qty") }), { bind: "ans" }),
          perform("convert", CONVERT, X.record({ action: X.lit("constitution.melt.convert"), input: MELT })),
          succeed(X.record({ outcome: X.lit("converted") })),
        ],
        "StaleSource",
        [succeed(X.record({ outcome: X.lit("withheld_stale") }))],
      ),
    ] }],
    [succeed(X.record({ outcome: X.lit("rejected") }))],
  ),
])
