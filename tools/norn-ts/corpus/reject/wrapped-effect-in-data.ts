// Syntactic wrappers do not hide an effect: a parenthesized, `satisfies`-wrapped,
// or non-null-asserted pipeline root in a data position is still an effect.
import { Effect, pipe } from "effect"
import { Norn } from "@norn/authoring"

export default Norn.flow(
  { id: "wrapped-effect-in-data", version: "1" },
  Effect.gen(function* () {
    const a = yield* Effect.succeed((Effect.succeed(1)).pipe(Effect.map((x) => x)))
    const b = yield* Effect.succeed(pipe((Effect.succeed(1)), Effect.map((x) => x)))
    const c = yield* Effect.succeed((Effect.succeed(1) satisfies Effect.Effect<number>).pipe(Effect.map((x) => x)))
    return { a, b, c }
  }),
)
