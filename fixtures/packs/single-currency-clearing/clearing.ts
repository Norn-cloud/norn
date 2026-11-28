import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent, EconomicEvent, Hold, LedgerEffect, Resource } from "../../../registry/core/src/rea.ts"

/**
 * Single-currency clearing benchmark pack (#1878).
 *
 * One USD clearing book, one cell, one fixed quantity template
 * (`usd_clearing_debit`), and one pinned ISO 20022 `pacs.008.001.10` payment
 * subset profile. Combines `core.Hold`, `core.LedgerEffect`, funded quantity
 * reservations (`quantityEffect`), maker-checker separation of duties
 * (`independentOf`), Cedar `AccessIr` role/scope enforcement, and an
 * authenticated AML/sanctions screening `acceptReceipt` gate.
 *
 * Honest evidence boundary: exercises maker-checker control, Cedar policy
 * enforcement, payment-to-reservation/settlement binding, REA `LiquidityHold` /
 * `ClearingEntry` recording, and receipt-backed reservation capture integrity
 * over a pinned `pacs.008.001.10` subset; makes no claim of full ISO 20022 XSD
 * conformance, external message mapping, crash-before-fsync recovery, or
 * organizational SOX 404 audit coverage.
 */

const Scope = ["orgId", "environmentId"] as const
const Access = { artifactId: "clearing.usd.access.policy", version: "0.1.0" } as const
const CoreDeps = [
  { artifactId: "core.value_types.entity", version: "0.2.0", kind: "entity" as const },
  { artifactId: "std.value_types.entity", version: "0.2.0", kind: "entity" as const },
]
const ScreeningManifestDigest =
  "sha256:4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a"

export const ClearingMarkings = Norn.markings({
  id: "clearing.usd.markings.entity",
  title: "Retained core classifications for clearing pack",
  version: "0.1.0",
  markings: [
    { id: "pii.contact", label: "Personal contact", severity: "high", category: "pii.contact", mask: { kind: "redacted" } },
    { id: "pii.identity", label: "Personal identity", severity: "high", category: "pii.identity", mask: { kind: "redacted" } },
    { id: "pii.financial", label: "Personal financial", severity: "high", category: "pii.financial", mask: { kind: "redacted" } },
    { id: "confidential.finance", label: "Confidential financial", severity: "high", category: "confidential.finance", mask: { kind: "redacted" } },
    { id: "pii.health", label: "Personal health", severity: "high", category: "pii.health", mask: { kind: "redacted" } },
  ],
})

export const ClearingParticipant = Norn.type({
  name: "clearing.Participant",
  role: "thing",
  title: "Clearing participant bank",
  version: "0.1.0",
  implements: [Agent],
  dependencies: CoreDeps,
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
    bic: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const LiquidityBook = Norn.type({
  name: "clearing.LiquidityBook",
  role: "thing",
  title: "Single-currency USD clearing liquidity book",
  version: "0.1.0",
  implements: [Resource],
  dependencies: CoreDeps,
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref("core.code"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "semi_additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "owner", target: ClearingParticipant, cardinality: { to: "one", from: "many" } },
    { id: "custodian", target: ClearingParticipant, cardinality: { to: "one", from: "many" } },
  ],
})

const PaymentStatus = Schema.Literals(["draft", "approved", "reserved", "settled", "rejected", "expired"])

export const PaymentInstruction = Norn.type({
  name: "clearing.PaymentInstruction",
  role: "happening",
  title: "Pinned pacs.008.001.10 single-currency clearing instruction",
  version: "0.1.0",
  implements: [EconomicEvent],
  dependencies: CoreDeps,
  properties: Schema.Struct({
    id: Schema.String,
    message_profile: Schema.Literals(["pacs.008.001.10"]),
    msg_id: Schema.String,
    end_to_end_id: Schema.String,
    debtor_bic: Schema.String,
    creditor_bic: Schema.String,
    provider_id: Schema.String,
    receiver_id: Schema.String,
    resource_id: Schema.String,
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
    amount_minor: Schema.String,
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    status: PaymentStatus,
    maker_id: Schema.String,
    screening_digest: Schema.NullOr(Schema.String),
    screening_decision: Schema.NullOr(Schema.Literals(["clear", "hit", "error"])),
    reservation_id: Schema.NullOr(Schema.String),
    posting_id: Schema.NullOr(Schema.String),
  }),
  protected: ["maker_id"],
  origins: {
    maker_id: "actor",
    screening_digest: "receipt",
    screening_decision: "receipt",
  },
  immutableAfterCreate: [
    "message_profile",
    "msg_id",
    "end_to_end_id",
    "debtor_bic",
    "creditor_bic",
    "quantity",
    "amount_minor",
    "currency",
  ],
  identity: [
    { name: "primary", properties: ["id"] },
    { name: "by_pacs008_msg", properties: ["msg_id"] },
  ],
  defaults: {
    status: "draft",
    action: "transfer_ownership",
  },
  links: [
    { id: "provider", target: ClearingParticipant, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", target: ClearingParticipant, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", target: LiquidityBook, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
    { id: "reverses", target: "core.EconomicEvent", cardinality: { to: "one", from: "one" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["settled", "rejected", "expired"],
      immutable: ["settled", "rejected", "expired"],
      endings: {
        settled: "completed",
        rejected: "compensated",
        expired: "cancelled",
      },
      transitions: [
        { id: "approve", from: ["draft"], to: "approved" },
        { id: "reserve", from: ["approved"], to: "reserved" },
        { id: "settle", from: ["reserved"], to: "settled" },
        { id: "reject", from: ["draft", "approved", "reserved"], to: "rejected" },
        { id: "expire", from: ["reserved"], to: "expired" },
      ],
    },
  ],
})

export const LiquidityHold = Norn.type({
  name: "clearing.LiquidityHold",
  role: "happening",
  title: "Clearing liquidity hold",
  version: "0.1.0",
  implements: [Hold],
  dependencies: CoreDeps,
  properties: Schema.Struct({
    id: Schema.String,
    payment_id: Schema.String,
    resource_id: Schema.String,
    holder_id: Schema.String,
    status: Schema.Literals(["open", "released", "consumed", "expired"]),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "semi_additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    expires_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "open" },
  links: [
    { id: "resource", target: LiquidityBook, cardinality: { to: "one", from: "many", required: true } },
    { id: "holder", target: ClearingParticipant, cardinality: { to: "one", from: "many", required: true } },
    { id: "secures", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
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

export const ClearingEntry = Norn.type({
  name: "clearing.ClearingEntry",
  role: "happening",
  title: "Single-currency USD clearing ledger effect",
  version: "0.1.0",
  implements: [LedgerEffect],
  dependencies: CoreDeps,
  properties: Schema.Struct({
    id: Schema.String,
    event_id: Schema.String,
    account_code: Norn.ref("core.code"),
    amount: Norn.ref("core.money_amount"),
    currency: Norn.ref("core.code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "event", target: PaymentInstruction, cardinality: { to: "one", from: "many", required: true } },
  ],
})

// ------------------------------------------------------------------ Laws

export const SingleCurrencyUsdPayment = Norn.law({
  id: "clearing.payment.single_currency_usd.law",
  title: "Every payment instruction clears strictly in USD with positive amount",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "clearing.payment.single_currency_usd",
    owner: "clearing",
    severity: "error",
    rationale: "Single-currency USD clearing book admits strictly positive USD instructions only.",
    provenance: {
      source: "authoring_sdk",
      sourcePath: "fixtures/packs/single-currency-clearing/clearing.ts",
    },
    scope: { root: { kind: "entity", id: "clearing.PaymentInstruction" }, objectTypes: ["clearing.PaymentInstruction"] },
    subject: "clearing.PaymentInstruction",
    condition: "true",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression:
        "(self.currency ?? '') == 'USD' and self.unit == null and self.quantity > 0 and self.message_profile == 'pacs.008.001.10' and self.amount_minor != ''",
    },
  },
  dependencies: [{ artifactId: "clearing.PaymentInstruction.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const SettledRequiresClearScreening = Norn.law({
  id: "clearing.payment.settled_requires_clear_screening.law",
  title: "Settled payments require an authenticated clear AML/sanctions screening receipt",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "clearing.payment.settled_requires_clear_screening",
    owner: "clearing",
    severity: "error",
    rationale: "No payment instruction may settle without a clear screening decision and receipt digest.",
    provenance: {
      source: "authoring_sdk",
      sourcePath: "fixtures/packs/single-currency-clearing/clearing.ts",
    },
    scope: { root: { kind: "entity", id: "clearing.PaymentInstruction" }, objectTypes: ["clearing.PaymentInstruction"] },
    subject: "clearing.PaymentInstruction",
    condition: "true",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression:
        "self.status != 'settled' or ((self.screening_decision ?? '') == 'clear' and self.screening_digest != null)",
    },
  },
  dependencies: [{ artifactId: "clearing.PaymentInstruction.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const SingleCurrencyUsdEntry = Norn.law({
  id: "clearing.entry.single_currency_usd.law",
  title: "Every clearing ledger effect is denominated in USD",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "clearing.entry.single_currency_usd",
    owner: "clearing",
    severity: "error",
    rationale: "Single-currency clearing entries must carry currency USD.",
    provenance: {
      source: "authoring_sdk",
      sourcePath: "fixtures/packs/single-currency-clearing/clearing.ts",
    },
    scope: { root: { kind: "entity", id: "clearing.ClearingEntry" }, objectTypes: ["clearing.ClearingEntry"] },
    subject: "clearing.ClearingEntry",
    condition: "true",
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.currency == 'USD'" },
  },
  dependencies: [{ artifactId: "clearing.ClearingEntry.entity", kind: "entity", version: "0.1.0", required: true }],
})

// ------------------------------------------------------------------ Actions

const InitiatePaymentInput = Schema.Struct({
  id: Schema.String,
  message_profile: Schema.Literals(["pacs.008.001.10"]),
  msg_id: Schema.String,
  end_to_end_id: Schema.String,
  debtor_bic: Schema.String,
  creditor_bic: Schema.String,
  provider_id: Schema.String,
  receiver_id: Schema.String,
  resource_id: Schema.String,
  action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
  occurred_at: Schema.String,
  quantity: Schema.String,
  amount_minor: Schema.String,
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
  maker_id: Schema.String,
})

const PaymentRef = Schema.Struct({ id: Schema.String })

export const InitiatePayment = Norn.action({
  id: "clearing.payment.initiate",
  title: "Initiate pacs.008 payment instruction (maker)",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  dependencies: CoreDeps,
  fromCaller: ["maker_id"],
  input: InitiatePaymentInput,
  output: PaymentRef,
  slice: [
    { name: "provider", type: ClearingParticipant, key: { property: { from: "input", property: "provider_id" } } },
    { name: "receiver", type: ClearingParticipant, key: { property: { from: "input", property: "receiver_id" } } },
    { name: "resource", type: LiquidityBook, key: { property: { from: "input", property: "resource_id" } } },
  ],
  createLinks: [
    { link: "provider", targetType: ClearingParticipant, targetIdField: "provider_id" },
    { link: "receiver", targetType: ClearingParticipant, targetIdField: "receiver_id" },
    { link: "resource", targetType: LiquidityBook, targetIdField: "resource_id" },
  ],
  writes: [{ type: PaymentInstruction, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: PaymentInstruction,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

export const ApprovePayment = Norn.action({
  id: "clearing.payment.approve",
  title: "Approve payment instruction (checker, independent of maker)",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  independentOf: ["maker_id"],
  input: PaymentRef,
  output: PaymentRef,
  writes: [{ type: PaymentInstruction, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PaymentInstruction,
    objectIdField: "id",
    lifecycle: "status",
    transition: "approve",
  },
})

const BalanceVersionEntry = Schema.Struct({
  account: Schema.String,
  unit: Schema.String,
  layer: Schema.String,
  version: Schema.Int,
})

const ReserveFundsInput = Schema.Struct({
  id: Schema.String,
  reservation_id: Schema.String,
  amount_minor: Schema.String,
  balance_versions: Schema.Array(BalanceVersionEntry),
})

const PaymentSlice = Schema.Struct({
  payment: Schema.Struct({
    id: Schema.String,
    status: PaymentStatus,
    amount_minor: Schema.String,
    reservation_id: Schema.NullOr(Schema.String),
  }),
})

export const ReserveFunds = Norn.action({
  id: "clearing.payment.reserve",
  title: "Reserve USD clearing liquidity for approved payment",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: ReserveFundsInput,
  output: PaymentRef,
  slice: [{ name: "payment", type: PaymentInstruction, key: { property: { from: "input", property: "id" } } }],
  preconditions: [
    Norn.expression(
      Schema.Struct({ input: ReserveFundsInput, slice: PaymentSlice }),
      x => x.input.amount_minor === x.slice.payment.amount_minor,
    ),
  ],
  writes: [{ type: PaymentInstruction, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PaymentInstruction,
    objectIdField: "id",
    lifecycle: "status",
    transition: "reserve",
    setFields: { reservation_id: "reservation_id" },
  },
  quantityEffect: {
    ledgerId: "clearing_usd",
    balanceVersionsField: "balance_versions",
    operation: {
      kind: "reserve",
      reservationIdField: "reservation_id",
      templateId: "usd_clearing_debit",
      parameters: { amount: { field: "amount_minor", scale: 2 } },
      ttlMs: 300000,
    },
  },
})

const SettlePaymentInput = Schema.Struct({
  id: Schema.String,
  screening_receipt: Schema.String,
  reservation_id: Schema.String,
  posting_id: Schema.String,
  balance_versions: Schema.Array(BalanceVersionEntry),
})

/**
 * Single-turn atomic settlement: consumes the authenticated AML/sanctions
 * `FunctionReceipt` (`acceptReceipt`), checks that `reservation_id` matches
 * the payment's stored reservation, writes `screening_digest` +
 * `screening_decision`, captures the funded USD quantity reservation
 * (`post_reservation`), and transitions `reserved -> settled` in one SQLite commit.
 */
export const SettlePayment = Norn.action({
  id: "clearing.payment.settle",
  title: "Settle payment with screening receipt and reservation capture",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: SettlePaymentInput,
  output: PaymentRef,
  slice: [{ name: "payment", type: PaymentInstruction, key: { property: { from: "input", property: "id" } } }],
  preconditions: [
    Norn.expression(
      Schema.Struct({ input: SettlePaymentInput, slice: PaymentSlice }),
      x => (x.slice.payment.reservation_id ?? "") !== "" && x.input.reservation_id === (x.slice.payment.reservation_id ?? ""),
    ),
  ],
  writes: [{ type: PaymentInstruction, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PaymentInstruction,
    objectIdField: "id",
    lifecycle: "status",
    transition: "settle",
    setFields: { posting_id: "posting_id" },
    acceptReceipt: {
      referenceField: "screening_receipt",
      manifestDigest: ScreeningManifestDigest,
      digestProperty: "screening_digest",
      resultBindings: { screening_decision: "decision" },
    },
  },
  quantityEffect: {
    ledgerId: "clearing_usd",
    balanceVersionsField: "balance_versions",
    operation: {
      kind: "post_reservation",
      reservationIdField: "reservation_id",
      postingIdField: "posting_id",
    },
  },
})

const RecordHoldInput = Schema.Struct({
  id: Schema.String,
  payment_id: Schema.String,
  resource_id: Schema.String,
  holder_id: Schema.String,
  quantity: Schema.String,
  unit: Schema.NullOr(Schema.String),
  currency: Schema.NullOr(Schema.String),
  expires_at: Schema.NullOr(Schema.String),
})

export const RecordLiquidityHold = Norn.action({
  id: "clearing.hold.record",
  title: "Record REA LiquidityHold bound to reserved payment",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  dependencies: CoreDeps,
  input: RecordHoldInput,
  output: PaymentRef,
  slice: [
    { name: "payment", type: PaymentInstruction, key: { property: { from: "input", property: "payment_id" } } },
    { name: "resource", type: LiquidityBook, key: { property: { from: "input", property: "resource_id" } } },
    { name: "holder", type: ClearingParticipant, key: { property: { from: "input", property: "holder_id" } } },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({ input: RecordHoldInput, slice: PaymentSlice }),
      x => x.slice.payment.status === "reserved" && (x.slice.payment.reservation_id ?? "") === x.input.id,
    ),
  ],
  createLinks: [
    { link: "resource", targetType: LiquidityBook, targetIdField: "resource_id" },
    { link: "holder", targetType: ClearingParticipant, targetIdField: "holder_id" },
  ],
  writes: [{ type: LiquidityHold, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: LiquidityHold,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

const RecordClearingEntryInput = Schema.Struct({
  id: Schema.String,
  event_id: Schema.String,
  account_code: Schema.String,
  amount: Schema.String,
  currency: Schema.String,
})

export const RecordClearingEntry = Norn.action({
  id: "clearing.entry.record",
  title: "Record REA ClearingEntry bound to settled payment",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  dependencies: CoreDeps,
  input: RecordClearingEntryInput,
  output: PaymentRef,
  slice: [{ name: "payment", type: PaymentInstruction, key: { property: { from: "input", property: "event_id" } } }],
  preconditions: [
    Norn.expression(
      Schema.Struct({ input: RecordClearingEntryInput, slice: PaymentSlice }),
      x => x.slice.payment.status === "settled" && x.input.account_code !== "",
    ),
  ],
  createLinks: [{ link: "event", targetType: PaymentInstruction, targetIdField: "event_id" }],
  writes: [{ type: ClearingEntry, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: ClearingEntry,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

export const RejectPayment = Norn.action({
  id: "clearing.payment.reject",
  title: "Reject payment instruction",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: PaymentRef,
  output: PaymentRef,
  writes: [{ type: PaymentInstruction, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PaymentInstruction,
    objectIdField: "id",
    lifecycle: "status",
    transition: "reject",
  },
})

export const ExpirePayment = Norn.action({
  id: "clearing.payment.expire",
  title: "Expire uncaptured payment hold",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: PaymentRef,
  output: PaymentRef,
  writes: [{ type: PaymentInstruction, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: PaymentInstruction,
    objectIdField: "id",
    lifecycle: "status",
    transition: "expire",
  },
})

// ------------------------------------------------------------------ Access

export const ClearingAccess = Norn.access({
  id: "clearing.usd.access",
  title: "Single-currency USD clearing access policy",
  version: "0.1.0",
  rules: [
    {
      ruleId: "makers_initiate",
      actions: [InitiatePayment],
      resourceType: PaymentInstruction,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["maker"] } }],
    },
    {
      ruleId: "checkers_approve_or_reject",
      actions: [ApprovePayment, RejectPayment],
      resourceType: PaymentInstruction,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["checker"] } }],
    },
    {
      ruleId: "operators_reserve_settle_expire",
      actions: [ReserveFunds, SettlePayment, ExpirePayment],
      resourceType: PaymentInstruction,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["clearing_operator"] } }],
    },
    {
      ruleId: "operators_record_hold",
      actions: [RecordLiquidityHold],
      resourceType: LiquidityHold,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["clearing_operator"] } }],
    },
    {
      ruleId: "operators_record_entry",
      actions: [RecordClearingEntry],
      resourceType: ClearingEntry,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["clearing_operator"] } }],
    },
    {
      ruleId: "clearing_staff_read_payments",
      actions: ["norn.read"],
      resourceType: PaymentInstruction,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["maker", "checker", "clearing_operator"] } }],
    },
    {
      ruleId: "clearing_staff_read_participants",
      actions: ["norn.read"],
      resourceType: ClearingParticipant,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["maker", "checker", "clearing_operator"] } }],
    },
    {
      ruleId: "clearing_staff_read_books",
      actions: ["norn.read"],
      resourceType: LiquidityBook,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["maker", "checker", "clearing_operator"] } }],
    },
    {
      ruleId: "clearing_staff_read_holds",
      actions: ["norn.read"],
      resourceType: LiquidityHold,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["maker", "checker", "clearing_operator"] } }],
    },
    {
      ruleId: "clearing_staff_read_entries",
      actions: ["norn.read"],
      resourceType: ClearingEntry,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["maker", "checker", "clearing_operator"] } }],
    },
  ],
})
