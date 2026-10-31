/**
 * `norn.core` physics: the generative primitives every other pack maps onto
 * (REA; norn-platform#2986, norn#56). Domain and system types implement these
 * interfaces; the laws they carry are registered, with their evidence, in
 * `verification/core-physics-v1/obligations.json` (the norn.core laws PRs,
 * norn#77 and norn#90).
 *
 * Currencies are ISO 4217 codes held as `core.code` until the public registry
 * has an `iso4217.currency_code` value type. The per-record laws (positive
 * quantity, exactly one of unit and currency) are declared, not yet
 * enforced as invariants: invariant expressions cannot read registry-typed
 * properties yet.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Agent = Norn.interfaceType({
  name: "core.Agent",
  title: "Agent",
  version: "0.1.0",
  description: "An actor that holds resources, takes part in economic events and makes commitments: a person, an organisation or a system (REA agent).",
  properties: Schema.Struct({
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
  }),
})

export const AgentRole = Norn.interfaceType({
  name: "core.AgentRole",
  title: "Agent role",
  version: "0.1.0",
  description: "A role an agent plays over a period (customer, supplier, employee, carrier). A role is a relationship, not a subtype of the agent.",
  properties: Schema.Struct({
    role: Norn.ref("core.code"),
    valid_from: Schema.NullOr(Norn.ref("iso8601.date")),
    valid_to: Schema.NullOr(Norn.ref("iso8601.date")),
  }),
  links: [
    { id: "agent", description: "The agent playing the role.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "counterparty", description: "The agent the role is held towards, when it is not the business itself.", target: Agent, cardinality: { to: "one", from: "many" } },
  ],
})

export const Resource = Norn.interfaceType({
  name: "core.Resource",
  title: "Resource",
  version: "0.1.0",
  description: "Something of value that is conserved: goods, money, time, capacity or a right, held as a quantity in exactly one unit (a UCUM unit, or a currency for money).",
  properties: Schema.Struct({
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "owner", description: "The agent that owns the resource.", target: Agent, cardinality: { to: "one", from: "many" } },
    { id: "custodian", description: "The agent that holds the resource, when it is not the owner.", target: Agent, cardinality: { to: "one", from: "many" } },
  ],
})

export const Commitment = Norn.interfaceType({
  name: "core.Commitment",
  title: "Commitment",
  version: "0.1.0",
  description: "A promise of a future economic event: an order line, a contract term, a subscription period. It ends fulfilled, cancelled or breached and is never dropped (DEMO request, promise, execute).",
  properties: Schema.Struct({
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "provider", description: "The agent that promised to give.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "The agent the promise is made to.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "The resource promised.", target: Resource, cardinality: { to: "one", from: "many" } },
  ],
})

export const EconomicEvent = Norn.interfaceType({
  name: "core.EconomicEvent",
  title: "Economic event",
  version: "0.1.0",
  description: "A posted change of resources. A transfer moves ownership or custody from provider to receiver; a transformation consumes inputs and produces outputs. Posted events are immutable; a correction is a new event that reverses its original.",
  properties: Schema.Struct({
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "provider", description: "The agent giving up the resource, or performing the transformation.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "The agent receiving the resource; for a transformation, the agent the output belongs to.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "The resource moved, consumed or produced.", target: Resource, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", description: "The commitment this event fulfils, in whole or in part.", target: Commitment, cardinality: { to: "one", from: "many" } },
    { id: "reverses", description: "The posted event this event exactly reverses.", target: "core.EconomicEvent", cardinality: { to: "one", from: "one" } },
  ],
})

export const Claim = Norn.interfaceType({
  name: "core.Claim",
  title: "Claim",
  version: "0.1.0",
  description: "An economic claim: a right to receive resources that arises from a fulfilled commitment or an event, such as an invoice or a receivable, and is settled by economic events. Not a catalog claim about definitions.",
  properties: Schema.Struct({
    amount: Norn.ref("core.money_amount"),
    currency: Norn.ref("core.code"),
    due_date: Norn.ref("iso8601.date"),
  }),
  links: [
    { id: "creditor", description: "The agent entitled to receive.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "debtor", description: "The agent obliged to give.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "arises_from", description: "The commitment whose fulfilment created the claim.", target: Commitment, cardinality: { to: "one", from: "many" } },
  ],
})
