// The data-builder twin of route-message.source.ts: the same flow written
// directly against the closure-free builder. Both lower to route-message.ir.json.
import { X, catchTag, flow, perform, retry, route, succeed } from "../../src/builder/index.ts"
import { classifyIntent } from "./support/classifier.builder.ts"

export default flow({ id: "crm.route_message", version: "1.0.0" }, [
  catchTag(
    [
      retry(
        perform(
          "classify",
          classifyIntent,
          X.record({ channel: X.lit("whatsapp"), locale: X.lit("en"), messageText: X.lit("Hi, can I change my order to 3 boxes instead of 2?") }),
          { bind: "classified" },
        ),
        { recurs: 1, delay: { kind: "spaced", millis: 2000 } },
      ),
      route([
        {
          when: X.eq(X.ref("classified", "intent"), X.lit("order.modify")),
          then: [succeed(X.record({ route: X.lit("orders"), intent: X.ref("classified", "intent") }))],
        },
      ]),
      succeed(X.record({ route: X.lit("inbox"), intent: X.ref("classified", "intent") })),
    ],
    "UnsupportedLanguage",
    [succeed(X.record({ route: X.lit("human"), intent: X.ref("unsupported", "locale") }))],
    { as: "unsupported" },
  ),
])
