// A local binding named `Effect` is not the admitted symbol, however it is spelled.
import { Effect as RealEffect } from "effect"
import { Norn } from "@norn/authoring"

const Effect = { gen: RealEffect.gen }

export default Norn.flow(
  { id: "shadowed", version: "1" },
  Effect.gen(function* () {
    return { ok: true }
  }),
)
