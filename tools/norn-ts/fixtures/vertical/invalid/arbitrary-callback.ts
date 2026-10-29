// expect: NORN-FLOW-CAPTURE at line containing: const pick =
// An arbitrary closure: the compiler cannot see what it does.
// T7-EFX-3 (#2786) vertical fixture, Effect.gen source. Sequence -> route ->
// typed failure -> finite retry timer -> accepted proposal -> terminal fact:
// classify a WhatsApp message with the T1-FN-1 classifier (one retry after a
// 2 s timer), route on the accepted intent, and hand an unsupported language
// to a human. Builder twin: route-message.builder.ts; golden: route-message.ir.json;
// event trace: trace.json (crates/norn-decisions/tests/flow_vertical.rs).
import { Effect, Schedule } from "effect"
import { Norn } from "@norn/authoring"
import { classifyIntent } from "../support/classifier.source.ts"

export default Norn.flow(
  { id: "crm.route_message", version: "1.0.0" },
  Effect.gen(function* () {
    const classified = yield* Norn.perform("classify", classifyIntent, {
      channel: "whatsapp",
      locale: "en",
      messageText: "Hi, can I change my order to 3 boxes instead of 2?",
    }).pipe(Effect.retry(Schedule.max([Schedule.spaced("2 seconds"), Schedule.recurs(1)])))
    const pick = (c: { readonly intent: string }) => c.intent === "order.modify"
    if (classified.intent === "order.modify") {
      return { route: "orders", intent: classified.intent }
    }
    return { route: "inbox", intent: classified.intent }
  }).pipe(Effect.catchTag("UnsupportedLanguage", (unsupported) => Effect.gen(function* () {
    return { route: "human", intent: unsupported.locale }
  }))),
)
