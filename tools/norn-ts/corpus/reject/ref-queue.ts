import { Effect, Queue, Ref } from "effect"
import { Norn } from "@norn/authoring"

export default Norn.flow(
  { id: "ref-queue", version: "1" },
  Effect.gen(function* () {
    const counter = yield* Ref.make(0)
    const queue = yield* Queue.unbounded<string>()
    return { counter, queue }
  }),
)
