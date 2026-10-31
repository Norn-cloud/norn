// expect: NORN-FLOW-SCHEMA-PROJECTION
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Invalid = Norn.action({
  id: "billing.invalid", title: "Invalid decimal output", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ amount: Norn.decimal(2) }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
})
