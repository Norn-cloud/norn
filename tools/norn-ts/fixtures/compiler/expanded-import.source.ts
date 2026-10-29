import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { expanded } from "./support/expanded-constant.source.ts"
export default Norn.flow({ id: "billing.expanded_import", version: "1.0.0" }, Effect.gen(function* () {
return expanded
}))
