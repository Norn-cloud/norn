import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Writer = Norn.decision({
  id: "example.atomic.writer", title: "Synchronous writer", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: Schema.Struct({}), event: Schema.Struct({ input: Schema.Struct({ so_number: Schema.String }), actor: Schema.String, operationId: Schema.String }) }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literal("create"), objectType: Schema.Literal("commerce.Quote"), key: Schema.String, values: Schema.Struct({ so_number: Schema.String, status: Schema.Literal("draft") }) })), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})) }),
}, input => ({ facts: [{ op: "create", objectType: "commerce.Quote", key: input.event.actor, values: { so_number: input.event.actor, status: "draft" } }], commands: [], timers: [] }))
