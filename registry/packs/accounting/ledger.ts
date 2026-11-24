/**
 * Governed Accounting Domain Pack — General Ledger, Chart of Accounts,
 * Fiscal Periods, Double-Entry Journals, and Monetary Resources.
 *
 * Jurisdiction-agnostic core accounting model built directly on `core` REA
 * primitives (`core.Agent`, `core.Resource`, `core.EconomicEvent`,
 * `core.LedgerEffect`) and `core.TimeInterval`. Country tax rules, statutory
 * filing formats, and local chart templates belong in `loc-*` overlay packs.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent, EconomicEvent, LedgerEffect, Resource } from "../../core/src/rea.ts"
import { TimeInterval } from "../../core/src/time.ts"
import { ISO_4217 } from "../canon/records.ts"

const BookStatus = Schema.Literals(["active", "archived"])

export const Book = Norn.type({
  name: "accounting.Book",
  role: "thing",
  title: "Ledger book",
  version: "0.1.0",
  description: "An accounting book declaring a functional currency and reporting standard.",
  properties: Schema.Struct({
    id: Schema.String,
    code: Norn.ref("core.code"),
    display_name: Norn.ref("core.display_name"),
    functional_currency: Schema.Literals(ISO_4217),
    accounting_standard: Schema.Literals(["ifrs", "gaap", "local"]),
    status: BookStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "active",
  },
  lifecycles: [
    {
      property: "status",
      initial: ["active"],
      terminal: ["archived"],
      immutable: ["archived"],
      transitions: [
        { id: "archive", from: ["active"], to: "archived" },
      ],
    },
  ],
})

export const Counterparty = Norn.type({
  name: "accounting.Counterparty",
  role: "thing",
  title: "Counterparty",
  version: "0.1.0",
  description: "A legal entity, customer, supplier, or bank participating in financial claims and payments (implements core.Agent).",
  implements: [Agent],
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
    counterparty_code: Norn.ref("core.code"),
    counterparty_type: Schema.Literals(["customer", "supplier", "bank", "entity", "other"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    kind: "organization",
  },
})

const AccountStatus = Schema.Literals(["active", "frozen", "archived"])

export const Account = Norn.type({
  name: "accounting.Account",
  role: "thing",
  title: "Ledger account",
  version: "0.1.0",
  description: "A chart-of-accounts node within a ledger book; grouping accounts aggregate children and forbid direct posting.",
  properties: Schema.Struct({
    id: Schema.String,
    account_code: Norn.ref("core.code"),
    display_name: Norn.ref("core.display_name"),
    account_class: Schema.Literals(["asset", "liability", "equity", "revenue", "expense"]),
    normal_balance: Schema.Literals(["debit", "credit"]),
    currency: Schema.Literals(ISO_4217),
    is_group: Schema.Boolean,
    allow_direct_posting: Schema.Boolean,
    status: AccountStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    is_group: false,
    allow_direct_posting: true,
    status: "active",
  },
  links: [
    { id: "book", description: "Ledger book owning this account.", target: Book, cardinality: { to: "one", from: "many", required: true } },
    { id: "parent", description: "Optional parent grouping account in the chart hierarchy.", target: "accounting.Account", cardinality: { to: "one", from: "many" }, acyclic: true },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["active"],
      terminal: ["archived"],
      immutable: ["archived"],
      transitions: [
        { id: "freeze", from: ["active"], to: "frozen" },
        { id: "unfreeze", from: ["frozen"], to: "active" },
        { id: "archive", from: ["active", "frozen"], to: "archived" },
      ],
    },
  ],
})

const FiscalPeriodStatus = Schema.Literals(["open", "closing", "closed", "locked"])

export const FiscalPeriod = Norn.type({
  name: "accounting.FiscalPeriod",
  role: "happening",
  title: "Fiscal period",
  version: "0.1.0",
  description: "A proper half-open [starts_at, ends_at) UTC accounting period governing posting cutoffs (implements core.TimeInterval).",
  implements: [TimeInterval],
  properties: Schema.Struct({
    id: Schema.String,
    period_code: Norn.ref("core.code"),
    starts_at: Norn.ref("core.time.instant"),
    ends_at: Norn.ref("core.time.instant"),
    timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
    status: FiscalPeriodStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "open",
  },
  links: [
    { id: "book", description: "Ledger book governed by this fiscal period.", target: Book, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["locked"],
      immutable: ["locked"],
      transitions: [
        { id: "begin_close", from: ["open"], to: "closing" },
        { id: "close", from: ["open", "closing"], to: "closed" },
        { id: "reopen", from: ["closing", "closed"], to: "open" },
        { id: "lock", from: ["closed"], to: "locked" },
      ],
    },
  ],
})

export const MonetaryAccount = Norn.type({
  name: "accounting.MonetaryAccount",
  role: "thing",
  title: "Monetary account",
  version: "0.1.0",
  description: "A conserved cash, bank, clearing, or control monetary resource (implements core.Resource).",
  implements: [Resource],
  properties: Schema.Struct({
    id: Schema.String,
    resource_code: Norn.ref("core.code"),
    display_name: Norn.ref("core.display_name"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "semi_additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
    account_kind: Schema.Literals(["cash", "bank", "clearing", "control"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "owner", description: "Counterparty or legal entity owning this monetary account.", target: Counterparty, cardinality: { to: "one", from: "many" } },
    { id: "custodian", description: "Bank or custodian holding this monetary account.", target: Counterparty, cardinality: { to: "one", from: "many" } },
    { id: "book", description: "Ledger book tracking this monetary account.", target: Book, cardinality: { to: "one", from: "many", required: true } },
    { id: "gl_account", description: "Chart-of-accounts control account mapped to this monetary resource.", target: Account, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const FinancialEvent = Norn.type({
  name: "accounting.FinancialEvent",
  role: "happening",
  title: "Financial event",
  version: "0.1.0",
  description: "An immutable economic event backing posted journal lines or value movements (implements core.EconomicEvent).",
  implements: [EconomicEvent],
  properties: Schema.Struct({
    id: Schema.String,
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Norn.ref("iso8601.timestamp"),
    quantity: Norn.ref("core.quantity", { additivity: { kind: "additive" }, unitSource: ["unit", "currency"] }),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")),
    currency: Schema.NullOr(Norn.ref("core.code")),
    reference_no: Schema.NullOr(Norn.ref("core.document_number")),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "provider", description: "Counterparty providing value in this financial event.", target: Counterparty, cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", description: "Counterparty receiving value in this financial event.", target: Counterparty, cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", description: "Monetary account moved or adjusted by this financial event.", target: MonetaryAccount, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", description: "Optional commitment fulfilled by this financial event.", target: "core.Commitment", cardinality: { to: "one", from: "many" } },
    { id: "reverses", description: "Optional prior financial event reversed by this event.", target: "accounting.FinancialEvent", cardinality: { to: "one", from: "one" } },
  ],
})

const JournalEntryStatus = Schema.Literals(["draft", "posted", "reversed"])

export const JournalEntry = Norn.type({
  name: "accounting.JournalEntry",
  role: "happening",
  title: "Journal entry",
  version: "0.1.0",
  description: "A double-entry accounting header posted atomically into an open fiscal period; posted and reversed entries are immutable.",
  properties: Schema.Struct({
    id: Schema.String,
    entry_number: Norn.ref("core.document_number"),
    posting_date: Norn.date(),
    currency: Schema.Literals(ISO_4217),
    total_debit: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    total_credit: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    memo: Schema.String,
    created_by_user_id: Schema.String,
    status: JournalEntryStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    { id: "book", description: "Ledger book this journal entry posts into.", target: Book, cardinality: { to: "one", from: "many", required: true } },
    { id: "period", description: "Fiscal period governing posting eligibility for this entry.", target: FiscalPeriod, cardinality: { to: "one", from: "many", required: true } },
    { id: "reverses", description: "Optional posted journal entry reversed by this entry.", target: "accounting.JournalEntry", cardinality: { to: "one", from: "one" } },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["reversed"],
      immutable: ["posted", "reversed"],
      transitions: [
        { id: "post", from: ["draft"], to: "posted" },
        { id: "reverse", from: ["posted"], to: "reversed" },
      ],
    },
  ],
})

export const JournalLine = Norn.type({
  name: "accounting.JournalLine",
  role: "happening",
  title: "Journal line",
  version: "0.1.0",
  description: "One strictly one-sided debit or credit posting line valuing a financial event (implements core.LedgerEffect; positive amount is debit, negative amount is credit).",
  implements: [LedgerEffect],
  properties: Schema.Struct({
    id: Schema.String,
    account_code: Norn.ref("core.code"),
    side: Schema.Literals(["debit", "credit"]),
    amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    currency: Norn.ref("core.code"),
    debit_amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    credit_amount: Norn.ref("core.money_amount", { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    description: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "event", description: "Underlying financial event valued by this ledger effect.", target: FinancialEvent, cardinality: { to: "one", from: "many", required: true } },
    { id: "entry", description: "Parent journal entry containing this posting line.", target: JournalEntry, cardinality: { to: "one", from: "many", required: true } },
    { id: "account", description: "Chart-of-accounts node posted by this line.", target: Account, cardinality: { to: "one", from: "many", required: true } },
  ],
})

// ---------------------------------------------------------------- Governed Laws & Examples (LAW-EXAMPLE-1)

export const JournalEntryBalanced = Norn.law({
  id: "accounting.JournalEntry.balanced.law",
  title: "Every non-draft journal entry must have equal, strictly positive total debits and credits",
  version: "0.1.0",
  dependencies: [{ artifactId: "accounting.JournalEntry.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "accounting.JournalEntry.balanced",
    owner: "accounting",
    severity: "error",
    rationale: "Every non-draft journal entry must have equal, strictly positive total debits and credits.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/accounting/ledger.ts" },
    subject: "accounting.JournalEntry",
    scope: { root: { kind: "entity", id: "accounting.JournalEntry" }, objectTypes: ["accounting.JournalEntry"] },
    condition: "self.status != \"draft\"",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.total_debit == self.total_credit and self.total_debit > 0",
    },
  },
})

export const JournalEntryBalancedHolds = Norn.example("posted balanced journal entry holds", {
  of: JournalEntryBalanced,
  given: { status: "posted", total_debit: 1000, total_credit: 1000 },
  expect: "holds",
})

export const JournalEntryBalancedViolated = Norn.example("posted unbalanced journal entry is violated", {
  of: JournalEntryBalanced,
  given: { status: "posted", total_debit: 1000, total_credit: 900 },
  expect: "violated",
})

export const JournalEntryBalancedDraftNotApplicable = Norn.example("draft journal entry is not applicable", {
  of: JournalEntryBalanced,
  given: { status: "draft", total_debit: 1000, total_credit: 0 },
  expect: "not_applicable",
})

export const JournalLineOneSided = Norn.law({
  id: "accounting.JournalLine.one_sided.law",
  title: "Every journal line is strictly one-sided (debit or credit) with a matching signed core.LedgerEffect amount",
  version: "0.1.0",
  dependencies: [{ artifactId: "accounting.JournalLine.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "accounting.JournalLine.one_sided",
    owner: "accounting",
    severity: "error",
    rationale: "A journal line must carry a strictly positive amount on exactly one side (debit or credit), zero on the opposite side, and match core.LedgerEffect sign convention (+debit, -credit).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/accounting/ledger.ts" },
    subject: "accounting.JournalLine",
    scope: { root: { kind: "entity", id: "accounting.JournalLine" }, objectTypes: ["accounting.JournalLine"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "(self.side == \"debit\" and self.debit_amount > 0 and self.credit_amount == 0 and self.amount == self.debit_amount) or (self.side == \"credit\" and self.credit_amount > 0 and self.debit_amount == 0 and self.amount == -self.credit_amount)",
    },
  },
})

export const JournalLineDebitHolds = Norn.example("debit journal line holds", {
  of: JournalLineOneSided,
  given: { side: "debit", amount: 500, debit_amount: 500, credit_amount: 0 },
  expect: "holds",
})

export const JournalLineCreditHolds = Norn.example("credit journal line holds with signed negative amount", {
  of: JournalLineOneSided,
  given: { side: "credit", amount: -100, debit_amount: 0, credit_amount: 100 },
  expect: "holds",
})

export const JournalLineTwoSidedViolated = Norn.example("two-sided journal line is violated", {
  of: JournalLineOneSided,
  given: { side: "debit", amount: 500, debit_amount: 500, credit_amount: 100 },
  expect: "violated",
})

export const JournalLinePositiveCreditViolated = Norn.example("credit journal line with positive amount is violated", {
  of: JournalLineOneSided,
  given: { side: "credit", amount: 100, debit_amount: 0, credit_amount: 100 },
  expect: "violated",
})

export const AccountGroupNoDirectPosting = Norn.law("group_no_direct_posting", {
  on: Account,
  must: (self) => !self.is_group || !self.allow_direct_posting,
  because: "Grouping accounts in the chart of accounts must never allow direct journal posting.",
})

export const AccountGroupDisallowsPostingHolds = Norn.example("group account with direct posting disabled holds", {
  of: AccountGroupNoDirectPosting,
  given: { is_group: true, allow_direct_posting: false },
  expect: "holds",
})

export const AccountLeafAllowsPostingHolds = Norn.example("leaf account with direct posting enabled holds", {
  of: AccountGroupNoDirectPosting,
  given: { is_group: false, allow_direct_posting: true },
  expect: "holds",
})

export const AccountGroupDirectPostingViolated = Norn.example("group account allowing direct posting is violated", {
  of: AccountGroupNoDirectPosting,
  given: { is_group: true, allow_direct_posting: true },
  expect: "violated",
})

export const MonetaryAccountNonNegative = Norn.law({
  id: "accounting.MonetaryAccount.non_negative.law",
  title: "Monetary account balance is non-negative",
  version: "0.1.0",
  dependencies: [{ artifactId: "accounting.MonetaryAccount.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1",
    id: "accounting.MonetaryAccount.non_negative",
    owner: "accounting",
    severity: "error",
    rationale: "A conserved monetary resource balance must remain non-negative unless modeled as an explicit credit liability.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/accounting/ledger.ts" },
    subject: "accounting.MonetaryAccount",
    scope: { root: { kind: "entity", id: "accounting.MonetaryAccount" }, objectTypes: ["accounting.MonetaryAccount"] },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.quantity >= 0",
    },
  },
})

export const MonetaryAccountZeroHolds = Norn.example("zero monetary account balance holds", {
  of: MonetaryAccountNonNegative,
  given: { quantity: 0 },
  expect: "holds",
})

export const MonetaryAccountPositiveHolds = Norn.example("positive monetary account balance holds", {
  of: MonetaryAccountNonNegative,
  given: { quantity: 5000 },
  expect: "holds",
})

export const MonetaryAccountNegativeViolated = Norn.example("negative monetary account balance is violated", {
  of: MonetaryAccountNonNegative,
  given: { quantity: -10 },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Actions

const PostJournalSlice = Schema.Struct({
  slice: Schema.Struct({
    entry: Schema.Struct({
      status: JournalEntryStatus,
      total_debit: Norn.decimal(6),
      total_credit: Norn.decimal(6),
    }),
    period: Schema.Struct({
      status: FiscalPeriodStatus,
    }),
  }),
})

export const PostJournalEntry = Norn.action({
  id: "accounting.journal.post",
  title: "Post journal entry",
  version: "0.1.0",
  description: "Post a balanced draft journal entry into an open fiscal period.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    entryId: Schema.String,
    periodId: Schema.String,
  }),
  output: Schema.Struct({
    entryId: Schema.String,
    status: Schema.String,
  }),
  slice: [
    { name: "entry", type: JournalEntry, key: { property: { from: "input", property: "entryId" } } },
    { name: "period", type: FiscalPeriod, key: { property: { from: "input", property: "periodId" } } },
  ],
  preconditions: [
    Norn.expression(
      PostJournalSlice,
      (ctx) => ctx.slice.period.status === "open"
        && ctx.slice.entry.status === "draft"
        && ctx.slice.entry.total_debit === ctx.slice.entry.total_credit
        && ctx.slice.entry.total_debit > 0,
      { root: "context" },
    ),
  ],
  writes: [{ type: JournalEntry, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: JournalEntry,
    objectIdField: "entryId",
    lifecycle: "status",
    transition: "post",
  },
})

export const ReverseJournalEntry = Norn.action({
  id: "accounting.journal.reverse",
  title: "Reverse posted journal entry",
  version: "0.1.0",
  description: "Mark a posted journal entry as reversed under controller authority.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    entryId: Schema.String,
  }),
  output: Schema.Struct({
    entryId: Schema.String,
    status: Schema.String,
  }),
  writes: [{ type: JournalEntry, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: JournalEntry,
    objectIdField: "entryId",
    lifecycle: "status",
    transition: "reverse",
  },
})

export const CloseFiscalPeriod = Norn.action({
  id: "accounting.period.close",
  title: "Close fiscal period",
  version: "0.1.0",
  description: "Close an open or closing fiscal period so no further regular journal entries post into it.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    periodId: Schema.String,
  }),
  output: Schema.Struct({
    periodId: Schema.String,
    status: Schema.String,
  }),
  writes: [{ type: FiscalPeriod, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: FiscalPeriod,
    objectIdField: "periodId",
    lifecycle: "status",
    transition: "close",
  },
})

export const LockFiscalPeriod = Norn.action({
  id: "accounting.period.lock",
  title: "Lock fiscal period",
  version: "0.1.0",
  description: "Permanently lock a closed fiscal period so it becomes immutable.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "accounting.access.policy", version: "0.1.0" },
  input: Schema.Struct({
    periodId: Schema.String,
  }),
  output: Schema.Struct({
    periodId: Schema.String,
    status: Schema.String,
  }),
  writes: [{ type: FiscalPeriod, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: FiscalPeriod,
    objectIdField: "periodId",
    lifecycle: "status",
    transition: "lock",
  },
})
