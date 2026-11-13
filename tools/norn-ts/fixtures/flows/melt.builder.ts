// Approve a melt: a person decides, and on approval the cell calls the
// governed action `constitution.melt.convert` itself, under the authority of
// whoever started the run (ACTION-STEP-1), never the decider's.
import { DECISION_FN, S, X, actionFn, flow, perform, route, succeed } from "../../src/builder/index.ts"

const CONVERT = actionFn("constitution.melt.convert", S.record({ id: S.text(), input_fine_g: S.text(), output_fine_g: S.text() }))

const option = (id: string, label: string) =>
  X.record({ id: X.lit(id), label: X.lit(label), scope: X.lit("Melt 7 only"), effect: X.record({ action: X.lit("constitution.melt.convert") }) })

const MELT = X.record({ id: X.lit("melt-7"), input_fine_g: X.lit("12"), output_fine_g: X.lit("10") })

export default flow({ id: "ops.approve_melt", version: "1.0.0", title: "Approve a melt" }, [
  perform("ask_review", DECISION_FN, X.record({
    question: X.lit("Convert melt 7: 12 g in, 10 g out?"),
    risk: X.lit("medium"),
    options: X.list(option("approve", "Convert it"), option("reject", "Leave it")),
    decider: X.record({ roles: X.list(X.lit("admin")) }),
  }), { bind: "review" }),
  route([{ when: X.eq(X.ref("review", "choice"), X.lit("approve")), then: [
    perform("convert", CONVERT, X.record({ action: X.lit("constitution.melt.convert"), input: MELT })),
    succeed(X.record({ outcome: X.lit("converted") })),
  ] }], [succeed(X.record({ outcome: X.lit("left") }))]),
])
