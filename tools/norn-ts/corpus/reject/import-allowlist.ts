// Imports are allowlisted: effect and its subpaths, @norn/authoring, and local
// TypeScript source modules. Other packages, declaration-only local modules, and
// side-effect-only imports (which run code on import) are rejected.
import "../support/billing.ts"
import { version } from "typescript"
import { answer } from "../support/declared-only"
export { transpile } from "typescript"
import { Effect } from "effect"
import { Norn } from "@norn/authoring"

export default Norn.flow(
  { id: "import-allowlist", version: "1" },
  Effect.gen(function* () {
    return { ok: true }
  }),
)
