// A negated numeric literal is still a literal: -1e400 is -Infinity, which JSON
// would serialize as null.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"

export const floor = -1e400

export default Norn.flow(
  { id: "non-finite-negative", version: "1" },
  Effect.gen(function* () {
    return { low: -1e400 }
  }),
)
