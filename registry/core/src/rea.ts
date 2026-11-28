/**
 * `norn.core` physics: the generative primitives every other pack maps onto
 * (REA; norn-platform#2986, norn#56). Domain and system types implement these
 * interfaces; the laws they carry are registered, with their evidence, in
 * `verification/core-physics-v1/obligations.json` (the norn.core laws PRs,
 * norn#77 and norn#90).
 *
 * Currencies on these interfaces stay `core.code` for pack compatibility while
 * `iso4217.currency_code` is materialized in `std.value_types.entity`. The
 * per-record laws (non-negative/positive quantity, non-zero/positive amount,
 * and unit/currency exclusivity) are enforced directly as interface invariants
 * over the materialized value-type catalogs (#1583).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const QuantityAndMeasure = Schema.Struct({
  quantity: Norn.decimal(6),
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
})

const MoneyAmount = Schema.Struct({
  amount: Norn.decimal(6),
})

export const Agent = Norn.interfaceType({
  name: "core.Agent",
  role: "thing",
  title: "Agent",
  version: "0.2.0",
  description: "An actor that holds resources, takes part in economic events and makes commitments: a person, an organisation or a system (REA agent).",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
  }),
})

export const AgentRole = Norn.interfaceType({
  name: "core.AgentRole",
  title: "Agent role",
  version: "0.2.0",
  description: "A role an agent plays over a period (customer, supplier, employee, carrier). A role is a relationship, not a subtype of the agent.",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
    { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
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
  role: "thing",
  title: "Resource",
  version: "0.2.0",
  description: "Something of value that is conserved: goods, money, time, capacity or a right, held as a quantity in exactly one unit (a UCUM unit, or a currency for money). `valuation_method` is the governed parameter of core.law.stock_valuation (FIFO or moving average); it is null for a resource that is not valued.",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
    { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
  }),
  links: [
    { id: "owner", description: "The agent that owns the resource.", target: Agent, cardinality: { to: "one", from: "many" } },
    { id: "custodian", description: "The agent that holds the resource, when it is not the owner.", target: Agent, cardinality: { to: "one", from: "many" } },
  ],
  invariants: [
    {
      id: "non_negative_quantity",
      message: "A resource quantity must be non-negative.",
      expression: Norn.expression(QuantityAndMeasure, (self) => self.quantity >= 0),
    },
    {
      id: "single_measure",
      message: "A resource must carry either a unit or a currency, never both or neither.",
      expression: Norn.expression(
        QuantityAndMeasure,
        (self) => (self.unit !== null && self.currency === null) || (self.unit === null && self.currency !== null),
      ),
    },
  ],
})

export const Commitment = Norn.interfaceType({
  name: "core.Commitment",
  role: "happening",
  title: "Commitment",
  version: "0.2.0",
  description: "A promise of a future economic event: an order line, a contract term, a subscription period. It ends fulfilled, cancelled or breached and is never dropped (DEMO request, promise, execute).",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
    { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
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
  invariants: [
    {
      id: "positive_quantity",
      message: "A commitment quantity must be strictly positive.",
      expression: Norn.expression(QuantityAndMeasure, (self) => self.quantity > 0),
    },
    {
      id: "single_measure",
      message: "A commitment must carry either a unit or a currency, never both or neither.",
      expression: Norn.expression(
        QuantityAndMeasure,
        (self) => (self.unit !== null && self.currency === null) || (self.unit === null && self.currency !== null),
      ),
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["requested", "promised"],
      terminal: ["fulfilled", "cancelled", "breached"],
      immutable: ["fulfilled", "cancelled", "breached"],
      endings: {
        fulfilled: "completed",
        cancelled: "cancelled",
        breached: "breached_visible",
      },
      transitions: [
        { id: "promise", from: ["requested"], to: "promised" },
        { id: "fulfil", from: ["promised"], to: "fulfilled" },
        { id: "cancel", from: ["requested", "promised"], to: "cancelled" },
        { id: "breach", from: ["requested", "promised"], to: "breached" },
      ],
    },
  ],
})

export const Hold = Norn.interfaceType({
  name: "core.Hold",
  role: "happening",
  title: "Hold",
  version: "0.2.0",
  description: "A quantity set aside against a balance: an escrow, a stock reservation or a use of a quota. A hold is released, consumed or expires, and never exceeds what is available (core.law.escrow_non_negative).",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
    { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    status: Schema.Literals(["open", "released", "consumed", "expired"]),
    quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    expires_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
  }),
  links: [
    { id: "resource", description: "The resource held.", target: Resource, cardinality: { to: "one", from: "many", required: true } },
    { id: "holder", description: "The agent the quantity is held for.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "secures", description: "The commitment the hold secures, if any.", target: Commitment, cardinality: { to: "one", from: "many" } },
  ],
  invariants: [
    {
      id: "positive_quantity",
      message: "A hold quantity must be strictly positive.",
      expression: Norn.expression(QuantityAndMeasure, (self) => self.quantity > 0),
    },
    {
      id: "single_measure",
      message: "A hold must carry either a unit or a currency, never both or neither.",
      expression: Norn.expression(
        QuantityAndMeasure,
        (self) => (self.unit !== null && self.currency === null) || (self.unit === null && self.currency !== null),
      ),
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["released", "consumed", "expired"],
      immutable: ["released", "consumed", "expired"],
      endings: {
        consumed: "completed",
        released: "compensated",
        expired: "cancelled",
      },
      transitions: [
        { id: "consume", from: ["open"], to: "consumed" },
        { id: "release", from: ["open"], to: "released" },
        { id: "expire", from: ["open"], to: "expired" },
      ],
    },
  ],
})

export const EconomicEvent = Norn.interfaceType({
  name: "core.EconomicEvent",
  role: "happening",
  title: "Economic event",
  version: "0.2.0",
  description: "A posted change of resources. A transfer moves ownership or custody from provider to receiver; a transformation consumes inputs and produces outputs. Posted events are immutable; a correction is a new event that reverses its original.",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
    { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
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
  invariants: [
    {
      id: "positive_quantity",
      message: "An economic event quantity must be strictly positive.",
      expression: Norn.expression(QuantityAndMeasure, (self) => self.quantity > 0),
    },
    {
      id: "single_measure",
      message: "An economic event must carry either a unit or a currency, never both or neither.",
      expression: Norn.expression(
        QuantityAndMeasure,
        (self) => (self.unit !== null && self.currency === null) || (self.unit === null && self.currency !== null),
      ),
    },
  ],
})

export const LedgerEffect = Norn.interfaceType({
  name: "core.LedgerEffect",
  role: "happening",
  title: "Ledger effect",
  version: "0.2.0",
  description: "The value view of an economic event: one signed amount on one account. A positive amount is a debit and a negative one a credit; the side is presentation only and is never stored. The effects posted together balance (core.law.value_conservation).",
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    account_code: Norn.ref("core.code"),
    amount: Norn.ref("core.money_amount"),
    currency: Norn.ref("core.code"),
  }),
  links: [
    { id: "event", description: "The economic event this effect values.", target: EconomicEvent, cardinality: { to: "one", from: "many", required: true } },
  ],
  invariants: [
    {
      id: "non_zero_amount",
      message: "A ledger effect amount must be non-zero.",
      expression: Norn.expression(MoneyAmount, (self) => self.amount !== 0),
    },
  ],
})

export const Claim = Norn.interfaceType({
  name: "core.Claim",
  role: "thing",
  title: "Claim",
  version: "0.2.0",
  description: "An economic claim: a right to receive resources, such as an invoice or a receivable, settled by economic events. Not a catalog claim about definitions. Disjoint from core.Commitment (Commitment ⊓ Claim = ∅): a promise is not yet a right, and no type is both. A fulfilled or accepted commitment gives rise to claims (givesRiseTo); the relation is recorded on the claim as `arises_from`, because a commitment is not edited when a claim arises.",
  disjointWith: [Commitment],
  dependencies: [
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
    { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    amount: Norn.ref("core.money_amount"),
    currency: Norn.ref("core.code"),
    due_date: Norn.ref("iso8601.date"),
  }),
  links: [
    { id: "creditor", description: "The agent entitled to receive.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "debtor", description: "The agent obliged to give.", target: Agent, cardinality: { to: "one", from: "many", required: true } },
    { id: "arises_from", description: "The commitment that gave rise to the claim (givesRiseTo, seen from the claim).", target: Commitment, cardinality: { to: "one", from: "many" } },
  ],
  invariants: [
    {
      id: "positive_amount",
      message: "A claim amount must be strictly positive.",
      expression: Norn.expression(MoneyAmount, (self) => self.amount > 0),
    },
  ],
})

export const Process = Norn.interfaceType({
  name: "core.Process", title: "Process", version: "0.1.0", role: "happening",
  properties: Schema.Struct({}),
})
export const Conversion = Norn.interfaceType({
  name: "core.Conversion", title: "Conversion", version: "0.1.0", role: "happening",
  extends: [Process], properties: Schema.Struct({}),
})
export const Exchange = Norn.interfaceType({
  name: "core.Exchange", title: "Exchange", version: "0.1.0", role: "happening",
  extends: [Process], properties: Schema.Struct({}),
})

export const HoldableResource = Norn.interfaceType({
  name: "core.HoldableResource",
  role: "thing",
  title: "Holdable resource",
  version: "0.2.0",
  description:
    "A conserved resource that supports open holds/escrows/reservations through inverse link `holds` and enforces core.law.escrow_non_negative at commit.",
  extends: [Resource],
  dependencies: [
    { artifactId: "core.Resource.interface", version: "0.2.0", kind: "entity" },
    { artifactId: "core.Hold.interface", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({}),
  links: [
    {
      id: "holds",
      description: "Holds placed against this resource (inverse of core.Hold.resource).",
      target: Hold,
      cardinality: { to: "many", from: "one" },
    },
  ],
})

export const ValuedEvent = Norn.interfaceType({
  name: "core.ValuedEvent",
  role: "happening",
  title: "Valued economic event",
  version: "0.2.0",
  description:
    "A posted economic event whose accounting legs are linked through inverse role `effects` and must net to zero in `valuation_currency` (core.law.value_conservation).",
  extends: [EconomicEvent],
  dependencies: [
    { artifactId: "core.EconomicEvent.interface", version: "0.2.0", kind: "entity" },
    { artifactId: "core.LedgerEffect.interface", version: "0.2.0", kind: "entity" },
    { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    valuation_currency: Norn.ref("core.code"),
  }),
  links: [
    {
      id: "effects",
      description: "Signed ledger effects valuing this economic event (inverse of core.ValuedLedgerEffect.event).",
      target: "core.ValuedLedgerEffect",
      cardinality: { to: "many", from: "one" },
    },
  ],
})

export const ValuedLedgerEffect = Norn.interfaceType({
  name: "core.ValuedLedgerEffect",
  role: "happening",
  title: "Valued ledger effect",
  version: "0.2.0",
  description:
    "A signed ledger effect bound to a `core.ValuedEvent` whose currency must match the event's `valuation_currency` so `core.ValuedEvent.effects_balance` never nets unlike currencies.",
  extends: [LedgerEffect],
  dependencies: [
    { artifactId: "core.LedgerEffect.interface", version: "0.2.0", kind: "entity" },
    { artifactId: "core.ValuedEvent.interface", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({}),
  links: [
    {
      id: "event",
      description: "The valued economic event this ledger effect belongs to.",
      target: ValuedEvent,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

export const ReversibleEvent = Norn.interfaceType({
  name: "core.ReversibleEvent",
  role: "happening",
  title: "Reversible economic event",
  version: "0.2.0",
  description:
    "An economic event that tracks `is_reversal` explicitly and narrows `reverses` to `core.ReversibleEvent` so one-hop LAW-LINK-1 enforcement prevents reversing a reversal (core.law.correction_is_linked_reversal).",
  extends: [EconomicEvent],
  dependencies: [
    { artifactId: "core.EconomicEvent.interface", version: "0.2.0", kind: "entity" },
  ],
  properties: Schema.Struct({
    is_reversal: Schema.Boolean,
  }),
  links: [
    {
      id: "reverses",
      description: "The posted reversible event this event exactly reverses.",
      target: "core.ReversibleEvent",
      cardinality: { to: "one", from: "one" },
    },
  ],
})

export const HoldMeasureMatchesResource = Norn.law({
  id: "core.Hold.measure_matches_resource.law",
  title: "A hold's unit and currency match its target resource",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.Hold.interface", kind: "entity", version: "0.2.0", required: true },
    { artifactId: "core.Resource.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.Hold.measure_matches_resource",
    owner: "core",
    severity: "error",
    rationale: "A hold or reservation must carry the exact unit or currency of the resource balance it locks.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.Hold",
    scope: { root: { kind: "entity", id: "core.Hold" }, objectTypes: ["core.Hold"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression:
        "((self.unit == null) == (self.resource.unit == null)) and ((self.unit ?? '') == (self.resource.unit ?? '')) and ((self.currency == null) == (self.resource.currency == null)) and ((self.currency ?? '') == (self.resource.currency ?? ''))",
    },
  },
})

export const EconomicEventReversalMatchesOriginal = Norn.law({
  id: "core.EconomicEvent.reversal_matches_original.law",
  title: "A reversing economic event matches its original's quantity, measure, and action",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.EconomicEvent.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.EconomicEvent.reversal_matches_original",
    owner: "core",
    severity: "error",
    rationale:
      "1-hop LAW-LINK-1 reversal parity: a linked reversal matches the quantity, unit, currency, and action of the posted event it reverses (same-resource / per-account cancellation remains enforced by the kernel reversal primitive).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.EconomicEvent",
    scope: { root: { kind: "entity", id: "core.EconomicEvent" }, objectTypes: ["core.EconomicEvent"] },
    condition: "self.reverses != null",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression:
        "self.quantity == self.reverses.quantity and ((self.unit == null) == (self.reverses.unit == null)) and ((self.unit ?? '') == (self.reverses.unit ?? '')) and ((self.currency == null) == (self.reverses.currency == null)) and ((self.currency ?? '') == (self.reverses.currency ?? '')) and self.action == self.reverses.action",
    },
  },
})

export const ReversibleEventNoDoubleReversal = Norn.law({
  id: "core.ReversibleEvent.no_reversal_of_reversal.law",
  title: "A linked reversal marks itself as a reversal and never reverses another reversal",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.ReversibleEvent.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.ReversibleEvent.no_reversal_of_reversal",
    owner: "core",
    severity: "error",
    rationale: "A linked reversal must carry `is_reversal == true` and may only reverse an original event (`self.reverses.is_reversal == false`).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.ReversibleEvent",
    scope: { root: { kind: "entity", id: "core.ReversibleEvent" }, objectTypes: ["core.ReversibleEvent"] },
    condition: "self.reverses != null",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.is_reversal == true and self.reverses.is_reversal == false",
    },
  },
})

export const ReversibleEventFlagRequiresLink = Norn.law({
  id: "core.ReversibleEvent.reversal_flag_requires_link.law",
  title: "An event marked as a reversal must link to its original",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.ReversibleEvent.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.ReversibleEvent.reversal_flag_requires_link",
    owner: "core",
    severity: "error",
    rationale: "Setting `is_reversal == true` requires an explicit `reverses` link to the posted original event.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.ReversibleEvent",
    scope: { root: { kind: "entity", id: "core.ReversibleEvent" }, objectTypes: ["core.ReversibleEvent"] },
    condition: "self.is_reversal == true",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.reverses != null",
    },
  },
})

export const HoldableResourceOpenHoldsWithinQuantity = Norn.law({
  id: "core.HoldableResource.open_holds_within_quantity.law",
  title: "Open holds against a resource never exceed its available quantity",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.HoldableResource.interface", kind: "entity", version: "0.2.0", required: true },
    { artifactId: "core.Hold.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.HoldableResource.open_holds_within_quantity",
    owner: "core",
    severity: "error",
    rationale: "The sum of open hold quantities against a holdable resource may never exceed the resource's quantity (core.law.escrow_non_negative).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.HoldableResource",
    scope: { root: { kind: "entity", id: "core.HoldableResource" }, objectTypes: ["core.HoldableResource"] },
    condition: "len(self.holds) > 0",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "sum(map(self.holds, #.status == 'open' ? #.quantity : 0)) <= self.quantity",
    },
  },
})

export const ValuedEventEffectsBalance = Norn.law({
  id: "core.ValuedEvent.effects_balance.law",
  title: "Signed ledger effects valuing an economic event net to zero",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.ValuedEvent.interface", kind: "entity", version: "0.2.0", required: true },
    { artifactId: "core.ValuedLedgerEffect.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.ValuedEvent.effects_balance",
    owner: "core",
    severity: "error",
    rationale: "Every valued economic event whose ledger effects are posted together must balance to zero (core.law.value_conservation); per-leg currency uniformity is enforced by core.ValuedLedgerEffect.currency_matches_event.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.ValuedEvent",
    scope: { root: { kind: "entity", id: "core.ValuedEvent" }, objectTypes: ["core.ValuedEvent"] },
    condition: "len(self.effects) > 0",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "sum(map(self.effects, #.amount)) == 0",
    },
  },
})

export const ValuedLedgerEffectCurrencyMatchesEvent = Norn.law({
  id: "core.ValuedLedgerEffect.currency_matches_event.law",
  title: "A valued ledger effect carries the valuation currency of its economic event",
  version: "0.2.0",
  dependencies: [
    { artifactId: "core.ValuedLedgerEffect.interface", kind: "entity", version: "0.2.0", required: true },
    { artifactId: "core.ValuedEvent.interface", kind: "entity", version: "0.2.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "core.ValuedLedgerEffect.currency_matches_event",
    owner: "core",
    severity: "error",
    rationale: "Every ledger leg of a valued economic event must use the event's declared valuation_currency so mixed currencies are never netted (core.law.value_conservation).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/rea.ts" },
    subject: "core.ValuedLedgerEffect",
    scope: { root: { kind: "entity", id: "core.ValuedLedgerEffect" }, objectTypes: ["core.ValuedLedgerEffect"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.currency == self.event.valuation_currency",
    },
  },
})
