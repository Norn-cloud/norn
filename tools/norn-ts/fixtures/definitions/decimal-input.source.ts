import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Receipt = Norn.type({
  name: "billing.Receipt", title: "Receipt", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, amount: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Record = Norn.action({
  id: "billing.receipt.record", title: "Record receipt", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String, amount: Norn.decimal(2) }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Receipt, change: "created" }],
  effect: { kind: "upsert_input_object", type: Receipt, objectIdField: "id", writeMode: { kind: "create_only" } },
})
