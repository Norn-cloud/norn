// Read-only question guard (QUESTION-STEP-1): evaluates an admitted question
// with RequiredSources freshness enforcement. Stale or incomplete required
// sources settle as typed StaleSource and withhold the downstream action;
// fresh Answered results route into the governed action.
import { S, X, actionFn, catchTag, flow, perform, questionFn, route, succeed } from "../../src/builder/index.ts"

const CHECK_STOCK = questionFn("freshness.order_line_qty")
const CONVERT = actionFn(
  "constitution.melt.convert",
  S.record({ id: S.text(), input_fine_g: S.text(), output_fine_g: S.text() }),
)

const MELT = X.record({ id: X.lit("melt-7"), input_fine_g: X.lit("12"), output_fine_g: X.lit("10") })

export default flow({ id: "ops.question_guard", version: "1.0.0", title: "Guard a melt with a question" }, [
  catchTag(
    [
      perform("check_question", CHECK_STOCK, X.record({ question: X.lit("freshness.order_line_qty") }), { bind: "ans" }),
      route(
        [{ when: X.and(X.eq(X.ref("ans", "status"), X.lit("Answered")), X.eq(X.ref("ans", "complete"), X.lit(true))), then: [
          perform("convert", CONVERT, X.record({ action: X.lit("constitution.melt.convert"), input: MELT })),
          succeed(X.record({ outcome: X.lit("converted"), queryPin: X.ref("ans", "queryPin") })),
        ] }],
        [succeed(X.record({ outcome: X.lit("withheld"), queryPin: X.ref("ans", "queryPin") }))],
      ),
    ],
    "StaleSource",
    [succeed(X.record({ outcome: X.lit("withheld_stale"), queryPin: X.lit("") }))],
  ),
])
