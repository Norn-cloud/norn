import { Effect } from "effect"
import { Norn } from "@norn/authoring"

const read = Norn.fn<{}, { status?: "voided" }>({ id: "probe.read", version: "1" })
const write = Norn.fn<{ status: "ready" }, {}>({ id: "probe.write", version: "1" })

export default Norn.flow({ id: "probe.transfer", version: "1" }, Effect.gen(function* () {
  const row = yield* Norn.perform("load", read, {})
  return yield* Norn.perform("store", write, row)
}))
