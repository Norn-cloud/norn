import { Effect } from "effect"
import { Norn } from "@norn/authoring"
export default Norn.flow({ id: "billing.precision", version: "1.0.0" }, Effect.gen(function* () {
  return { value: 9007199254740993 }
}))
