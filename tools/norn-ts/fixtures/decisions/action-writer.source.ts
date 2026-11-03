import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Writer = Norn.decision({
  id: "example.atomic.writer", title: "Synchronous writer", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: Schema.Struct({}), event: Schema.Struct({ input: Schema.Struct({ so_number: Schema.String }), actor: Schema.String, operationId: Schema.String }) }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literal("create"), objectType: Schema.Literal("commerce.Quote"), key: Schema.String, values: Schema.Struct({ so_number: Schema.String, status: Schema.Literal("draft") }) })), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})) }),
}, input => ({ facts: [{ op: "create", objectType: "commerce.Quote", key: input.event.actor, values: { so_number: input.event.actor, status: "draft" } }], commands: [], timers: [] }))

export const Source = Norn.type({
  name: "commerce.SalesOrder", title: "Atomic source fixture", version: "1.0.0",
  properties: Schema.Struct({so_number: Schema.String, status: Schema.Literals(["draft", "approved"])}), defaults: {status: "draft"},
  links: [{id: "quote", target: "commerce.Quote", cardinality: {to: "one", from: "many"}}],
  lifecycles: [{property: "status", initial: ["draft"], terminal: ["approved"], transitions: [{id: "approve", from: ["draft"], to: "approved"}]}],
})
export const Target = Norn.type({
  name: "commerce.Quote", title: "Atomic target fixture", version: "1.0.0",
  properties: Schema.Struct({so_number: Schema.String, status: Schema.Literals(["draft", "approved"])}), defaults: {status: "draft"},
  links: [{id: "order", target: Source, cardinality: {to: "one", from: "many"}}],
})
export const Approve = Norn.action({
  id: "example.atomic.approve", title: "Atomic closure fixture", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({so_number: Schema.String}), output: Schema.Struct({so_number: Schema.String}),
  synchronousRule: Writer, writes: [{type: Source, change: "modified"}, {type: Target, change: "created"}],
  createLinks: [{link: "quote", targetType: Target, targetIdField: "so_number"}, {source: {objectType: Target, idField: "so_number"}, link: "order", targetType: Source, targetIdField: "so_number"}],
  commands: [{connector: "payment", operation: "create", doctype: "PaymentIntent", fields: {amount: Norn.unsafeZen("300"), currency: Norn.unsafeZen("'USD'")}}],
  effect: {kind: "fire_transition", type: Source, objectIdField: "so_number", lifecycle: "status", transition: "approve"},
})
