import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Season = Norn.type({
  name: "fashion.Season",
  title: "Season",
  version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String, brand: Schema.String }),
  identity: [{ name: "primary", properties: ["code"] }],
})

export const CreateSeason = Norn.action({
  id: "fashion.season.create",
  title: "Create season",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ code: Schema.String, brand: Schema.String }),
  output: Schema.Struct({ code: Schema.String }),
  derive: [
    { field: "code", expression: Norn.unsafeZen("trim(input.code)") },
    { field: "code", expression: Norn.unsafeZen("upper(input.code)"), inputRequired: true },
    { field: "brand", expression: Norn.unsafeZen('input.brand ?? "NORN"'), inputRequired: false },
  ],
  slice: [{ name: "existing", type: Season, key: { property: { from: "input", property: "code" } } }],
  preconditions: [Norn.unsafeZen<boolean>("slice.existing.brand == input.brand")],
  writes: [{ type: Season, change: "created" }],
  effect: { kind: "upsert_input_object", type: Season, objectIdField: "code", writeMode: { kind: "create_only" } },
})
