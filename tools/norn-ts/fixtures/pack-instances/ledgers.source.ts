import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Account = Norn.interfaceType({ name: "demo.GenericAccount", title: "Ledger account", version: "1.0.0", properties: Schema.Struct({ id: Schema.String }) })
const Amounts = Schema.Struct({ debit: Schema.Finite, credit: Schema.Finite })
export const Ledger = Norn.interfaceType({
  name: "demo.GenericLedger", title: "Generic ledger", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, debit: Norn.boundDecimal("amount"), credit: Norn.boundDecimal("amount") }),
  links: [{ id: "from", target: Account, cardinality: { to: "one", from: "many", required: true } }, { id: "to", target: Account, cardinality: { to: "one", from: "many", required: true } }],
  invariants: [{ id: "balanced", expression: Norn.expression(Amounts, x => x.debit >= 0 && x.credit >= 0 && x.debit === x.credit), message: "Both legs conserve the ledger's denomination." }],
})
const Currency = Schema.Struct({ currency: Schema.String })
export const UsdOnly = Norn.interfaceType({
  name: "demo.UsdLedger", title: "USD ledger contract", version: "1.0.0",
  properties: Schema.Struct({ currency: Schema.Literals(["USD"]) }),
  invariants: [{ id: "usd_currency", expression: Norn.expression(Currency, x => x.currency === "USD"), message: "The monetary ledger is bound to USD." }],
})
export const UsdAccount = Norn.type({ name: "demo.UsdAccount", title: "USD account", version: "1.0.0", implements: [Account, UsdOnly], properties: Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-usd"]), currency: Schema.Literals(["USD"]) }), identity: [{ name: "primary", properties: ["id"] }] })
export const FineAccount = Norn.type({ name: "demo.FineAccount", title: "Fine-gram account", version: "1.0.0", implements: [Account], properties: Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-fine"]) }), identity: [{ name: "primary", properties: ["id"] }] })
export const UsdTransfer = Norn.type({
  name: "demo.UsdTransfer", title: "USD transfer", version: "1.0.0", implements: [Ledger, UsdOnly],
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "from", target: UsdAccount, cardinality: { to: "one", from: "many", required: true } }, { id: "to", target: UsdAccount, cardinality: { to: "one", from: "many", required: true } }],
  properties: Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-usd"]), from_account: Schema.String, to_account: Schema.String, currency: Schema.Literals(["USD"]), debit: Norn.decimal(2, { currency: "currency" }), credit: Norn.decimal(2, { currency: "currency" }) }),
})
export const FineTransfer = Norn.type({
  name: "demo.FineTransfer", title: "Fine-gram transfer", version: "1.0.0", implements: [Ledger],
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "from", target: FineAccount, cardinality: { to: "one", from: "many", required: true } }, { id: "to", target: FineAccount, cardinality: { to: "one", from: "many", required: true } }],
  properties: Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-fine"]), from_account: Schema.String, to_account: Schema.String, debit: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), credit: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }) }),
})
export const BalanceLaw = Norn.law({
  id: "demo.ledger.balance.law", title: "Ledger conservation", version: "1.0.0",
  law: { lawVersion: "0.1", id: "demo.ledger.balance", owner: "demo", severity: "error", rationale: "A ledger retains both equal legs in one denomination.", provenance: { source: "authoring_sdk", sourcePath: "tools/norn-ts/fixtures/pack-instances/ledgers.source.ts" }, scope: { root: { kind: "entity", id: "demo.GenericLedger" }, objectTypes: ["demo.GenericLedger"] }, subject: "demo.GenericLedger", modality: { kind: "always" }, response: { kind: "invariant", objectType: "demo.GenericLedger", invariantId: "balanced" } },
  dependencies: [{ artifactId: "demo.GenericLedger.interface", kind: "entity", version: "1.0.0", required: true }],
})
export const CurrencyLaw = Norn.law({
  id: "demo.ledger.usd.law", title: "USD binding", version: "1.0.0",
  law: { lawVersion: "0.1", id: "demo.ledger.usd", owner: "demo", severity: "error", rationale: "Currency constraints apply to the monetary instance only.", provenance: { source: "authoring_sdk", sourcePath: "tools/norn-ts/fixtures/pack-instances/ledgers.source.ts" }, scope: { root: { kind: "entity", id: "demo.UsdLedger" }, objectTypes: ["demo.UsdLedger"] }, subject: "demo.UsdLedger", modality: { kind: "always" }, response: { kind: "invariant", objectType: "demo.UsdLedger", invariantId: "usd_currency" } },
  dependencies: [{ artifactId: "demo.UsdLedger.interface", kind: "entity", version: "1.0.0", required: true }],
})
const UsdInput = Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-usd"]), from_account: Schema.String, to_account: Schema.String, currency: Schema.Literals(["USD"]), debit: Schema.Finite, credit: Schema.Finite })
const FineInput = Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-fine"]), from_account: Schema.String, to_account: Schema.String, debit: Schema.Finite, credit: Schema.Finite })
export const PostUsd = Norn.action({
  id: "demo.ledger.post_usd", title: "Post USD transfer", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  createLinks: [{ link: "from", targetType: UsdAccount, targetIdField: "from_account" }, { link: "to", targetType: UsdAccount, targetIdField: "to_account" }],
  input: UsdInput, output: Schema.Struct({ id: Schema.String }), writes: [{ type: UsdTransfer, change: "created" }],
  effect: { kind: "upsert_input_object", type: UsdTransfer, objectIdField: "id", writeMode: { kind: "create_only" } },
})
export const PostFine = Norn.action({
  id: "demo.ledger.post_fine", title: "Post fine-gram transfer", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  createLinks: [{ link: "from", targetType: FineAccount, targetIdField: "from_account" }, { link: "to", targetType: FineAccount, targetIdField: "to_account" }],
  input: FineInput, output: Schema.Struct({ id: Schema.String }), writes: [{ type: FineTransfer, change: "created" }],
  effect: { kind: "upsert_input_object", type: FineTransfer, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const OpenUsd = Norn.action({ id: "demo.ledger.open_usd", title: "Open USD account", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, input: Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-usd"]), currency: Schema.Literals(["USD"]) }), output: Schema.Struct({ id: Schema.String }), writes: [{ type: UsdAccount, change: "created" }], effect: { kind: "upsert_input_object", type: UsdAccount, objectIdField: "id", writeMode: { kind: "create_only" } } })
export const OpenFine = Norn.action({ id: "demo.ledger.open_fine", title: "Open fine-gram account", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, input: Schema.Struct({ id: Schema.String, ledger_id: Schema.Literals(["ledger-fine"]) }), output: Schema.Struct({ id: Schema.String }), writes: [{ type: FineAccount, change: "created" }], effect: { kind: "upsert_input_object", type: FineAccount, objectIdField: "id", writeMode: { kind: "create_only" } } })
