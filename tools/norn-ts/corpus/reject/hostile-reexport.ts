// A module re-exporting a lookalike `Effect` object: spelling matches, symbols do not.
import { Effect } from "../support/hostile-effect.ts"
import { Norn } from "@norn/authoring"

export default Norn.flow(
  { id: "hostile", version: "1" },
  Effect.gen(function* () {
    const x = yield* Effect.succeed(1)
    return { x }
  }),
)
