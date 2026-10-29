// Definitions are checked as a closure: an imported local module that fails the
// profile fails the importer at the import specifier.
import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { startedAt } from "../support/ambient-const.ts"

export default Norn.flow(
  { id: "imported-ambient", version: "1" },
  Effect.gen(function* () {
    return { startedAt }
  }),
)
