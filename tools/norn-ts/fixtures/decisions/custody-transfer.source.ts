import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// INV-TRANSFER: location changes preserve the company's quantity and carrying value.
export const Holding = Norn.type({
  name: "fixture.CustodyHolding", title: "Custody transfer holding", version: "1.0.0",
  properties: Schema.Struct({id: Schema.String, company: Schema.String, location: Schema.String, quantity: Schema.Int, value: Schema.Int, status: Schema.Literals(["ready", "moved"])}),
  lifecycles: [{property: "status", initial: ["ready"], terminal: ["moved"], transitions: [{id: "transfer", from: ["ready"], to: "moved"}]}],
})
export const Location = Norn.type({
  name: "fixture.CustodyLocation", title: "Company location", version: "1.0.0",
  properties: Schema.Struct({id: Schema.String, company: Schema.String}),
})
export const Movement = Norn.type({
  name: "fixture.CustodyMovement", title: "Internal transfer record", version: "1.0.0",
  properties: Schema.Struct({id: Schema.String, company: Schema.String, from: Schema.String, to: Schema.String, quantity: Schema.Int, value: Schema.Int}),
  links: [{id: "holding", target: Holding, cardinality: {to: "one", from: "many"}}, {id: "destination", target: Location, cardinality: {to: "one", from: "many"}}],
})
export const Account = Norn.type({
  name: "fixture.CustodyAccount", title: "Unchanged company books", version: "1.0.0",
  properties: Schema.Struct({id: Schema.String, company: Schema.String, currency: Schema.Literal("USD"), amount: Schema.Int}),
})
const Input = Schema.Struct({holding: Schema.String, destination: Schema.String, movement: Schema.String})
const Values = Schema.Struct({id: Schema.optionalKey(Schema.String), company: Schema.optionalKey(Schema.String), from: Schema.optionalKey(Schema.String), to: Schema.optionalKey(Schema.String), location: Schema.optionalKey(Schema.String), quantity: Schema.optionalKey(Schema.Int), value: Schema.optionalKey(Schema.Int)})
export const Writer = Norn.decision({
  id: "example.custody.writer", title: "Move custody without selling", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({slice: Schema.Struct({holding: Schema.Struct({id: Schema.String, company: Schema.String, location: Schema.String, quantity: Schema.Int, value: Schema.Int, status: Schema.String}), destination: Schema.Struct({id: Schema.String, company: Schema.String})}), event: Schema.Struct({input: Input, actor: Schema.String, operationId: Schema.String})}),
  // The current closed-schema profile requires optional values/patch members.
  output: Schema.Struct({facts: Schema.Array(Schema.Struct({op: Schema.Literals(["create", "patch"]), objectType: Schema.Literals(["fixture.CustodyHolding", "fixture.CustodyMovement"]), key: Schema.String, values: Schema.optionalKey(Values), patch: Schema.optionalKey(Values)})), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({}))}),
}, input => ({facts: [
  {op: "patch", objectType: "fixture.CustodyHolding", key: input.event.input.holding, patch: {location: input.event.input.destination}},
  {op: "create", objectType: "fixture.CustodyMovement", key: input.event.input.movement, values: {id: input.event.input.movement, company: input.slice.holding.company, from: input.slice.holding.location, to: input.event.input.destination, quantity: input.slice.holding.quantity, value: input.slice.holding.value}},
], commands: [], timers: []}))
export const Transfer = Norn.action({
  id: "example.custody.transfer", title: "Same-company custody transfer", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Input, output: Schema.Struct({holding: Schema.String}), synchronousRule: Writer,
  slice: [{name: "holding", type: Holding, key: {property: {from: "input", property: "holding"}}}, {name: "destination", type: Location, key: {property: {from: "input", property: "destination"}}}],
  preconditions: [Norn.unsafeZen<boolean>("slice.holding.company == slice.destination.company and slice.holding.location != input.destination")],
  writes: [{type: Holding, change: "modified"}, {type: Movement, change: "created"}],
  createLinks: [{source: {objectType: Movement, idField: "movement"}, link: "holding", targetType: Holding, targetIdField: "holding"}, {source: {objectType: Movement, idField: "movement"}, link: "destination", targetType: Location, targetIdField: "destination"}],
  effect: {kind: "fire_transition", type: Holding, objectIdField: "holding", lifecycle: "status", transition: "transfer"},
})
