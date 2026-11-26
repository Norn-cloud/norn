# Writing Norn apps

Read the [thesis](../architecture/norn-thesis.md), [2026-11-01 decisions](../architecture/decisions/2026-11-01.md), and [core specification index](../spec/core/README.md) before designing definitions or engine features. The decision record governs conflicts with earlier designs. Acceptance contracts: [completeness](../spec/acceptance/completeness-gate.md), [jewelry](../spec/acceptance/jewelry-demo.md), [master ontology MVP](../spec/acceptance/master-ontology-mvp.md).

**CURRENT** means supported authoring syntax in [`tools/norn-ts`](../../tools/norn-ts/README.md), inspected at `591b091` (2026-11-25). It does not imply every runtime or proof guarantee is qualified. **TARGET** means the intended authoring model; target examples below are illustrative contracts, not compilable SDK examples. `@norn/dsl` and its helpers are target notation, not today's package API. Never translate a missing construct into domain code in the kernel.

## The model (TARGET)

Everything is a concept. **Things** include resources and agents; **happenings** include events, processes, conversions, exchanges and commitments. A process is a concept with states, actions and links. There is no separate `workflow()`. State is a fold over immutable events; behavior is declared data, not methods or hidden mutable state. `is` specializes contracts, not implementation.

The core business ontology is itself authored in Norn: Agent, Resource, EconomicEvent, Commitment, Claim, Hold, LedgerEffect, Process, Conversion and Exchange, with constitutional laws. Inventory, accounting and sales build on it; jewelry composes them and adds metal concepts and laws; an app binds those packs into an operating context. None of the business physics is trusted kernel code.

One concept bundles `is`, stored and derived properties, links, states, actions, laws, policies, access and sources. Packs and apps contain definitions. Admitted definitions also have a queryable, read-only meta-ontology projection: states, transitions, laws, policies and parameters are inspectable objects. Editing that projection does not activate executable authority.

| Construct | CURRENT authoring equivalent (`origin/main` `591b091`) | TARGET contract |
| --- | --- | --- |
| Things and happenings | `Norn.type`, `Norn.interfaceType`, `extends`, `implements`, `protected`, `ownedCollections`, `pathEquations`, `Norn.boundDecimal`, and inline `lifecycles` (flat `transitions` or XState-shaped `MachineSpec` with `states`, `after`, `final`, `resting`, `events`) | `concept({ is, props, derived, links, states, actions, laws, policies, access, sources })` |
| Links | `Norn.type({ links: [...] })`; action `createLinks` | Named typed links and owned drafts with explicit identity and creation |
| Actions, decisions and approvals | `Norn.action` (`fire_transition`, `upsert_input_object` with `writeMode: { kind: "upsert" \| "create_only" \| "update_only" } \| { kind: "draft_only", lifecycle, state }`, `mutate_parent_with_rows`, `fromCaller`, `independentOf`, `aggregates`, `synchronousRule`, `inputLabels`, `inputGroups`), `Norn.callerId()`, `Norn.approval`, `Norn.decision` (callbacks, `hit: "unique"` tables, `principalInputs`), `Norn.orderedFold`, `Norn.expression` (`norn-zen/7` with closures and `Norn.sum`) | Composable actions with a complete checked synchronous effect closure |
| Durable execution | `Norn.flow` + bounded Effect syntax lowers to lifecycle data | Happenings, commitments and receipts; no second runtime as business authority |
| Laws and policies | Typed `Norn.law(name, { on, when?, must \| never \| eventually, within?, severity?, unless?, because })` (plus legacy raw `LawIR` overload for `#58`); `Norn.obligation`, `Norn.prohibition`, `Norn.permission` (a defeater that grants nothing); `Norn.example`; `Norn.access`; `Norn.markings` | Typed structured requirements and deontic policies; access remains separate |
| Packs, apps and overlays | File and pack-directory compilation; semantic-only `Norn.ontologyPack` and `Norn.packLink`; no app or overlay constructors | Pinned pack instances, explicit exports, app bindings, conjunctive overlays |
| Parameters | `Norn.setting` (effective-dated `SettingRoot` with exact `DecimalLiteral` strings for `"number"`, `PARAM-EXACT-1`) and `parameter` slice entries | Typed, scoped, effective-dated governed values |
| Defined concepts | No general `.where` classifier constructor | First-class pure classification over existing identities |
| Integrations | `Norn.sourceSchema`, `Norn.mapping` (reference-bound source pin, `MAP-ENUM-TEXT-1`), `Norn.connector` (`poll` / `pull`); action `commands` | Integration packs with external ontology, canonical links and governed write-back |
| Functions and receipts | `Norn.fn`, `Norn.perform` inside `Norn.flow`; production receipt intake and atomic consumption via `SqliteCell::{set_result_authorizer, retain_function_receipt, commit_result_refusal}` (`ResultAuthority`, `#1688`, fail-closed until host wires authorizer) | Canonical FunctionManifest; typed, capability-scoped performers and recorded receipts |
| Metrics, views, widgets and SDK | `Norn.question` (`link`/`links` paths, `asOf: "effective" \| "knowledge"`), `Norn.metricId`, `Norn.dashboard`, `Norn.transform`, `Norn.model`, `Norn.labels`; `W.widget` / `W.uiSdk` / `W.fromUiSdk` / `W.outcome` (`tools/norn-ts/src/builder/widget.ts`), Ring-1 `norn-ui-checker`, `<NornAppRenderer>` (`@norn/react`, `#1763`); `@norn/sdk` (`createNornClient`, `LiveClient`, `parseExactJson`) + `norn codegen --ts` (`#1684`); `norn explain` CLI. No `metric`, `view` or `expects` constructors in this profile | Reactive questions and views, typed measures, evidence and refusals |

Route changes to the relevant core contract:

| Concern | Core spec |
| --- | --- |
| Exact values, identity, units | [01 Values/identity](../spec/core/01-values-identity.md) |
| Concepts, links, classification, catalog | [02 Concept graph](../spec/core/02-concept-graph.md) |
| Folds, queries, deterministic arithmetic | [03 Query/compute](../spec/core/03-query-compute.md) |
| Writers, atomicity, read sets, outbox | [04 Transactions/effects](../spec/core/04-transactions-effects.md) |
| Laws, policies, permissions, evidence | [05 Laws/authority](../spec/core/05-laws-authority.md) |
| Pack instances, settings, upgrades | [06 Composition/config](../spec/core/06-composition-config.md) |
| Functions, integrations, reconciliation | [07 World boundary](../spec/core/07-world-boundary.md) |
| Forks, scenarios, metrics, reactive views | [08 Worlds/projections](../spec/core/08-worlds-projections.md) |

**CURRENT** Norn definitions are written in Effect's vocabulary (Effect Schema for shapes, `Effect.gen` for flows) and type-checked against the pinned `effect` package, but Norn never runs them: the compiler reads the syntax and lowers it to Norn IR, and the Rust checker decides what is admitted. Use the ruled syntax subset, not arbitrary TypeScript. See the [profile declarations](../../tools/norn-ts/profile/norn-authoring.d.ts), [compiler fixtures](../../tools/norn-ts/fixtures), and [rejection corpus](../../tools/norn-ts/corpus/reject).

### A small CURRENT definition

Catalog Item describes a product; Holding identifies a lot/resource at an owner/location. A product's metadata is not stock. This example declares identities and a link only; it does not implement receiving, valuation or conservation.

```ts
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Item = Norn.type({
  name: "jewelry.Item", title: "Item", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, label: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Holding = Norn.type({
  name: "jewelry.Holding", title: "Holding", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "item", target: Item,
    cardinality: { to: "one", from: "many", required: true } }],
})
```

Use `Norn.decimal(scale, { unit })` for exact quantities and `{ currency: "currencyProperty" }` for money bound to a sibling currency property. Specify scale, rounding, overflow and zero-division behavior for computations. `Norn.ref` names a shared **value type**, not an object reference. Object references belong in `links`; do not store `Customer.ref` or `Scrap.many` as scalar props. When a value type is shared by a flow and a stock, declare additivity where it is used: `Norn.ref("core.quantity", { additivity: { kind: "additive" } })` on an event's quantity and `{ kind: "semi_additive" }` on a holding's (METRIC-ADDITIVITY-REF-1). A use site may only add additivity to an integer or decimal type that declares none. When a quantity's unit is stored beside it rather than fixed, declare where: `{ unitSource: ["unit", "currency"] }` (METRIC-UNIT-PARTITION-1). Questions that aggregate it must then group by or `eq`-filter each of those properties.

### Labels in any language (CURRENT)

Ids stay language-neutral; people read labels. `Norn.type`, `Norn.interfaceType`, `Norn.action`, `Norn.decision` and `Norn.law` take `label` and `localizedDescription` as maps from BCP 47 tag to text, and types also take `propertyLabels` and `valueLabels` (enum property → stored value → label). `needsReview` lists the locales whose text was proposed, for example machine-translated, and still needs a person's review.

```ts
export const Invoice = Norn.type({
  name: "sales.Invoice", title: "Invoice", version: "1.0.0",
  label: { en: "Invoice", ar: "فاتورة" },
  properties: Schema.Struct({ id: Schema.String, status: Schema.Literals(["draft", "posted"]) }),
  propertyLabels: { id: { en: "Invoice number", ar: "رقم الفاتورة" } },
  valueLabels: { status: { draft: { en: "Draft", ar: "مسودة" }, posted: { en: "Posted", ar: "مُرحّلة" } } },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["id"] }],
})
```

The compiler emits these labels as a separate `labels` artifact (`sales.Invoice.entity.labels`), never inside the type, so adding or fixing a translation changes no artifact hash, model hash or receipt (LABEL-LOCALE-1, [core 08](../spec/core/08-worlds-projections.md#localized-definition-labels)). A tenant or locale pack labels types another layer owns with `Norn.labels({ id, title, version, types: { "canon.Holding": { label: { ar: "رصيد مخزون" }, properties: { … }, values: { … } } } })` (see `registry/packs/locale-ar`); its labels replace the installed ones for the same target and locale. `requiredLocales` lists the locales the layer serves, and every definition with no label in one is listed as a warning. Labels may not contain bidi control characters, because renderers isolate ids, codes and numbers themselves.

## How to write a Norn app

1. Read the decision record, the relevant core file from the index, and the named acceptance scenarios. State the invariant, governing container and atomic boundary before choosing syntax.
2. Reuse pinned core/domain pack instances and their interfaces. Separate catalog objects, holdings, agents, events, commitments and claims. Declare stable identities, exact units, link roles and scope.
3. Define happenings with explicit states, typed action inputs, rejections and terminal paths. Add executable approval/evidence paths for every policy. Pin in-flight instances to their definition version.
4. Declare deterministic reads, including empty ranges and link membership; declare every writer. Keep all synchronous balancing effects in one cell commit. Use commitments and compensation across cells.
5. Attach laws to their governing concept, happening, pack or app. Preserve inherited identities and mapping paths. Bind all cross-module quantifiers to explicit instances, units and temporal context.
6. Separate compile-time type/unit bindings from governed runtime parameters. Record the effective setting version and date basis with each action; backdating requires a restatement plan.
7. Map external systems and observed behavior as ordinary integration data. Use `perform` for external computation and effects; accept its recorded result through a governed action.
8. Add defined concepts, metrics and views over the same authorized truth. Declare grain, multiplicity, time semantics, completeness and freshness; expose unknown results.
9. Inspect direct effects, synchronous writer closure and guarantees. Add `expects` assertions for required assurance, not duplicates of inherited laws. Generate obligations; put checked proof bodies/evidence beside pack definitions.
10. Run the supported profile/compiler/checker, named positive and refusal scenarios, and repo gates on Boat/CI. Verify no partial writes, replay, duplicates and concurrency. If TARGET semantics are missing, report the generic gap rather than claiming the app works.

## Worked TARGET: sell → scrap → melt

All names in this section denote **pack declarations**, including any movement/allocation helper. Such helpers must expose their definitions, reads and effects. They are not new stock/GL opcodes or runnable examples for the current SDK.

### Concepts, classifications and overlays

```ts
// TARGET notation; Fineness and FineGrams are exact pack value types.
export const Metal = concept({
  is: [inventory.Holding],
  props: { metal: MetalKind },
  links: { item: inventory.Item.one, assays: Assay.many,
    acceptedMeasurement: Assay.one, vault: Vault.one },
  derived: { fineGrams: (m) => m.acceptedMeasurement.fineGrams },
  laws: { purityInRange: law({
    shall: (m) => m.acceptedMeasurement.purity.between(0, 1000),
  }) },
})
export const Scrap = concept({ is: [Metal], links: { seller: Customer.one } })
export const HighValueScrap = Scrap.where((s) => s.fineGrams.gt("100"))
```

An Assay is an ordinary versioned evidence concept with mass, purity, fine grams, method, assessor, event time, recorded time and source identity. Its exact measure is `massGrams × purityPerMille / 1000`, under declared rounding. An unassayed draft is not eligible to become a posted Metal holding. Accepted measurements are versioned; posted historical measures remain fixed.

`HighValueScrap` classifies the same scrap identities; it creates no copies and triggers no behavior. Predicates are pure and acyclic. Missing evidence is unknown, not false assurance. An approval obligation activated by classification survives a later membership change. CURRENT: classifications and this accepted-measurement derivation have no equivalent constructor in the profile.

An inventory receipt overlay must target the **received holding/lot**, or its declared material classification, not `receipt.item.is(Metal)`. TARGET overlays are scoped conjunctive additions to a pinned definition instance. Refuse conflicting compositions, recheck migration and progress, and never replace an inherited writer. Tightening safety can still deadlock a process.

### Sell: two economic sides

For a synthetic sale of one piece at 150 USD with original inventory cost 100 USD:

| Action | Declared effects in the posting commit |
| --- | --- |
| `SELL({ holding, customer, price, currency, effectiveDate })` | Consume the identified allocation; record delivery to the customer; Dr COGS 100 / Cr inventory 100; Dr receivable 150 / Cr sales 150; link the delivery and receivable to the same Exchange |
| `RECORD_PAYMENT({ exchange, receipt })` | Accept payment evidence; Dr cash 150 / Cr receivable 150; settle the claim exactly once |

Both sides are mandatory: delivery and cash **or a payable/receivable commitment**, with an explicit duality link. A sale return references original sale allocations and cost; a later scrap buyback is a new exchange, not automatically a return. Compensation appends linked inverse effects with new authorization; history is never deleted.

### Scrap buyback: repeatable assays and typed offers

The happening links `seller`, an owned `draftLot`, `assays`, `offer` and `consideration` (payment or payable). Draft ownership/creation must be declared. Offer input includes exact amount, currency, pricing source/version and expiry.

| State / action | Next state and contract |
| --- | --- |
| `weighed` or `assaying` / `RECORD_ASSAY({ evidence })` | `assaying`; append evidence linked to this lot and assessor; allow further independent assays |
| `assaying` / `OFFER({ offer })` | `offered`; bind the typed offer, check validity and rate policy; require sufficient accepted assay evidence |
| `offered` / `ACCEPT({ offerId })` | `bought`; validate the still-current offer/evidence, receive the lot and create payment intent or payable in the same commit |
| `offered` / `DECLINE` | `returned`; release draft/holds; no posted acquisition |

For high-value scrap, OFFER and ACCEPT require two independent assays by different accountable people, not two sessions of one person. Evidence must cover the same lot/revision; deduplicating one source receipt cannot count as two assays. A draft remeasurement invalidates stale offers. Every eligible state has an evidence path; a policy saying “twice” with a single terminal ASSAY transition is insufficient.

Example: receive 200 g at 750‰ = 150 fine g, consideration 9,000 USD. Post inventory/metal inflow with its declared external source counterpart, Dr inventory 9,000 / Cr payable 9,000, and link the acquisition to the liability. Cash settlement is a subsequent evidence-backed action. Do not mark a remote payment successful when merely enqueueing it.

**Assay after posting:** RECORD_ASSAY appends a new observation; it does not `set` the old lot's purity. If accepted purity is 745‰ on that 200 g lot, the new measure is 149 fine g. A separate authorized `RECONCILE_ASSAY` links old/new evidence and appends a −1 fine g adjustment with a +1 fine g variance counterpart, plus any explicitly valued money adjustment. Advance the accepted-measurement version atomically with reconciliation; retain the prior 150 fine g history. Reject a bare purity update that changes conserved stock.

### Melt: fund, measure, approve, post

This example uses the original 750‰ lot; the post-acquisition assay reconciliation above is an alternative scenario.

Melt specializes Conversion. Its links are input allocations/holds, output draft, measurements, loss evidence and approvals. Declare `inputFineGrams`, `outputFineGrams`, `lossFineGrams`, `inputValue`, `outputValue`, `lossValue` and `lossRatio = lossFineGrams / inputFineGrams`; require positive input before division. These are derived from accepted evidence and pinned allocation costs, never undeclared metric fields.

| State / action | Next state and contract |
| --- | --- |
| `planned` / `START({ allocations })` | `melting`; validate/release input holds and consume identified allocations into WIP, fund the metal account `melting` from `vault`, and move original cost inventory → WIP atomically |
| `melting` / `POUR({ outputDraft })` | `poured`; link the owned output draft; no premature available stock |
| `poured` / `WEIGH({ outputAssay, lossEvidence })` | `measured`; record accepted output/loss evidence and derive totals/ratio |
| `measured` / `REQUEST_APPROVAL` | `awaitingApproval` when above threshold; bind request to the measurement/allocation revision |
| `awaitingApproval` / `APPROVE({ requestId })` | `approved`; authorized manager adds evidence covering that exact revision; `REJECT` enters `needsRevision` |
| `needsRevision` / `REVISE({ evidence })` | `measured`; invalidate earlier approvals and recompute totals |
| `measured` or `approved` / `POST` | `posted`; `lossRatio <= threshold` path or valid required approval; consume WIP into output + loss and post matching value effects atomically |

At START, transfer 150 fine g and cost 9,000 USD to WIP. At POST, produce 149 fine g plus 1 fine g recorded process loss; transfer `melting → vault: 149` and `melting → process_loss: 1`. At the declared proportional cost basis, Dr inventory 8,940 / Dr process loss 60 / Cr WIP 9,000. Link loss provenance and consumed allocations to the Conversion. Gross grams, fine grams and money remain distinct measures.

The governing law is `inputFineGrams = outputFineGrams + lossFineGrams`, together with nonnegative availability, provenance and per-currency value balance. Here loss ratio is `1/150`, above `0.004`, so POST needs approval; compare `lossFineGrams > threshold × inputFineGrams` exactly to avoid rounded-ratio approval errors. Omitted loss refuses; the output must not become available. Concurrently consuming an input refuses stale intent or retries bounded contention after complete read-set validation, including phantom inserts. Cancellation after START must explicitly reverse valid allocations or enter compensation/reconciliation; it cannot abandon WIP.

## Packs, laws, policies and parameters (TARGET unless marked CURRENT)

TARGET pack instances form a pinned DAG: core → inventory/accounting/sales → jewelry → app. Resolve transitive `uses` to explicit instances; two accounting books must not accidentally share a singleton. Generic `ledger({ unit })` binds a compile-time unit/interface; it does not make tax rules meaningful for fine grams.

Cross-module laws may belong to an app **or a composing pack**. This TARGET contract sketch shows the bindings required; `StockBookContext` and the query helpers denote declarations to be supplied by the composing pack, not an existing SDK API:

```ts
// TARGET: context values identify pinned pack instances and one cell domain.
const context: StockBookContext = {
  inventory: inventoryInstance, accounting: accountingInstance,
  organization: org, book: stockBook, currency: "USD",
  valuationBasis: "moving_average", accountRoles: { stock: stockAccount },
  revision: transactionRevision, openingBalances: openingBalanceSet,
}
export const JewelryApp = app({
  uses: [inventoryInstance, accountingInstance, jewelryInstance],
  laws: { stockMatchesBooks: law({
    bindings: context,
    shall: (c) => c.holdings.closingValue().eq(c.book.stockClosingBalance()),
  }) },
})
```

Both sides select the same scope, revision, currency, valuation basis and opening balances. Include/exclude WIP consistently; declare any variance/external boundary. Never use an unexplained callable app context or sum unlike currencies. Reject an atomic law spanning cells; model the cross-cell obligation explicitly.

Laws attach where their quantifiers live. Concept purity bounds, happening conservation, pack traceability and app reconciliation are different subjects. Laws inherit through containment and `is`; mappings carry checked interpretations and preservation obligations. Preserve origins and deduplicate diamonds. A layer need not restate inherited laws, and cannot weaken them.

Laws use structured scope, condition, component, timing and response, plus deontic grades. EN/AR are renderings of one IR. A policy can require manager approval before posting; access determines who can execute APPROVE. Neither overrides conservation or grants authority by UI visibility. Agents act under authenticated, attenuated, revocable delegation; different people means different accountable identities.

TARGET parameters are typed, scoped, effective-dated and governed by actions. `lossThreshold: "0.004"` is a setting; it is stricter than `"0.005"` only under the declared order “lower threshold requires approval more often.” Arbitrary parameter changes are not monotonic. Parameter-local `shall` lowers to the common law representation with identity/subject/evidence; cross-setting laws stay on the pack/app. Declare scope precedence and refuse ambiguous intervals. Record valid time, commit time and exact selected setting version; do not apply today's tax rate to historical sales.

CURRENT: `Norn.setting` compiles effective-dated [SettingIR](../../crates/norn-ir/src/setting.rs) definitions in the authoring profile, and `SettingValue::Number(DecimalLiteral)` enforces exact decimal strings (`PARAM-EXACT-1`, refusing binary floats). Existing customization and additive migration support does not qualify the full target overlay contract. Keep in-flight happenings and effects pinned until an explicit validated migration rechecks data, laws and progress.

## Integrations and observed behavior (TARGET unless marked CURRENT)

An integration pack contains the external system's ontology, connector, pinned mappings into canonical concepts and governed write-back actions. Map ERPNext Item to catalog Item, and stock ledger or lot records to Holdings and Movements separately. A holding's accepted material classification controls the jewelry overlay.

POS logs, agent traces and ERP history are ordinary mapped data. One event can link to a lot, customer, exchange and assessor with typed roles; retain source event identity, event and recorded time, source authority, coverage and pinned mapping and definition versions. Deduplicate and reconcile before promotion to authoritative posted facts. Nonconforming observations remain evidence; do not discard them or silently execute a canonical action to manufacture conformance.

Discovery and conformance are questions over that data and the admitted definition catalog, shown in ordinary views. There is no `observe()`, `observed/` convention, or special mining definition kind. CURRENT source, mapping and connector declarations cover intake and mapping admission, not complete temporal many-object analysis. Existing `on: "observed"` trigger vocabulary does not change this model.

## Functions, effects and guarantees (TARGET unless marked CURRENT)

Functions are the computation escape boundary: WASM components or out-of-process workers, typed and capability-scoped, invoked behind `perform`. The canonical FunctionManifest governs checked WIT/Effect projections. Rust/Python components are initial targets; other workers use the same contract. Outputs are recorded receipts/proposals accepted through checked actions. A Function never executes inside a pure law/decision or independently authorizes writes. Secrets resolve only at the authorized performer.

CURRENT: use `Norn.fn` handles and `yield* Norn.perform("step-id", handle, input)` inside supported `Norn.flow`/Effect syntax; see the [sequential perform fixture](../../tools/norn-ts/corpus/accept/sequential-perform.ts). This is compiled lifecycle data, not permission to adopt Effect Workflow or a saga runtime.

Every writer lowers to one closed checked effect IR: action effects, `set`, creation, imports, migrations, timers/results and triggered rules. TARGET explain must show **direct effects**, their **transitive synchronous writer closure**, and later asynchronous commands separately. A declared `writes` list alone is not that guarantee. A trigger tomorrow cannot balance a posting committed today.

Commit business state and outbox intent atomically; dispatch is at least once with stable operation identity and provider deduplication contracts. Track requested → dispatched → succeeded/failed/outcome-unknown; unknown → reconciliation-required. A confirmed compensable consequence can enter compensation-pending → compensated; failed compensation remains pending with attempt evidence. An ambiguous non-idempotent payment requires reconciliation/manual resolution, not a blind retry or exactly-once claim. Compensation is a new authorized effect and can itself fail. Forks/historical worlds cannot perform real effects.

Laws are theorem statements: conjectures until proved, then lemmas for higher layers. Definitions specify the transition system. Generate initialization, preservation for **every writer**, non-vacuity and boundary obligations from those definitions. Pack proof bodies may use Lean against generated statements under `.norn/proofs`; published immutable releases carry evidence bound to definitions, semantics, settings, assumptions, bounds and tools. Do not hand-write an application theorem/model as a substitute for the compiler.

Enforce by construction where supported, at admission, then at commit. Balanced constructors guarantee their own pairs, not arbitrary writers, account selection or correct valuation. Runtime safety does not depend on a proof badge; a safety check does not prove eventual progress. Unsupported/stale/timed-out evidence remains unknown. Lean bodies count only after Lean checks them; bounded model results retain their bounds.

TARGET guarantee UX shows an assurance vector through `explain`: construction/admission/runtime status, proof backend, assumptions/bounds and unresolved claims. `expects` asserts the evidence level the app requires, without re-declaring inherited laws. Neither symbol exists in the current profile; inspect checker diagnostics/receipts today and report unqualified claims explicitly.

CURRENT: [structured requirements](../platform/structured-requirements.md) provide a narrow Rust safety-authoring wrapper; unsupported timing/response semantics refuse. [Source-bound induction statements](../../crates/norn-checker/src/obligations/induction.rs) exist for a limited fragment; they are not a general solver or activation gate.

### Coverage is a matrix, not a badge

This inspection is not an end-to-end qualification. “Partial” means an existing mechanism with narrower semantics; “unqualified” means no execution/evidence claim here. Check the relevant spec and exact-head gates before promoting any cell to supported.

| Area | IR | Admission | Evaluator | Commit enforcement | Authoring | Proof |
| --- | --- | --- | --- | --- | --- | --- |
| Objects, actions and links | Exists | Existing checks | Action programs (`upsert_input_object` incl. `update_only`, `fire_transition`, `mutate_parent_with_rows`) | Declared program and CAS mechanisms (`AdmittedDefinitions` binds guards) | CURRENT types, machines, actions, approvals | No automatic general theorem claim |
| Laws and composition | LawIR and structured wrapper | Scope, writer, exception-tree and norm-precedence checks | Supported fragments (`unless` evaluation open) | Full target closure unqualified | CURRENT typed `Norn.law`, norms, examples, Rust safety wrapper | Limited induction; no complete compiler |
| Settings and migration | Exists (`DecimalLiteral`) | Review and preflight checks | Scoped parameter resolution | Full temporal and upgrade contract unqualified | CURRENT `Norn.setting` | Unqualified |
| Integration and effects | Exists | Mapping and connector contracts (`MAP-ENUM-TEXT-1`) | Poll, pull, outbox and `SqliteCell` `FunctionReceipt` resolution (`#1688`) | Full uncertainty contract unqualified; hosts fail closed until authorizer is wired | CURRENT constructors and `perform` | Source evidence is not proof |
| Metrics, views, widgets and catalog | Exists (`QuestionIR`, `ViewIR`, `ModelIR`, `WidgetUx`) | Metric, transform, model and Ring-1 `norn-ui-checker` checks | Exact cell, IVM, DuckDB/DataFusion transforms, as-of-knowledge and restatement | Full three-engine equivalence and permission guarantees unqualified | CURRENT questions, `metricId`, dashboards, transforms, models, `W.widget` + `<NornAppRenderer>` | Unqualified |

## Metrics and views (TARGET)

TARGET types and interfaces generate candidate measures with declared additivity; business ratios remain explicit. Define `ScrapYield = sum(posted output fine g) / sum(posted input fine g)` at Conversion grain, with zero-input behavior and a declared period. Do not average batch ratios blindly. Holding balances are snapshots, not additive over time. Grain, set and bag semantics and fan-out-aware aggregation prevent double counting; link cardinality alone does not.

A board is a view or screen, reactive by default, with authorized queries and drill-through. Declare revision tokens, freshness, completeness and reconnect or permission invalidation. A paginated UI subset cannot discharge a universal law.

## Data engineering (TARGET unless marked CURRENT)

Read the [data-engineering acceptance contract](../spec/acceptance/data-engineering.md)
and the [data-engineering decision](../architecture/decisions/2026-11-01.md#owner-addition-data-engineering-and-demo-beat-6).
This section describes required semantics, not a fully qualified end-to-end pipeline.
CURRENT runtime pieces include `Norn.question`, `Norn.metricId`, `Norn.dashboard`,
`Norn.transform`, `Norn.model`, receipt-driven IVM (`compute::ivm`, including as-of and
linked-time plans), embedded DuckDB and DataFusion transforms, as-of-knowledge questions,
restatement staging and `norn-olapd`; full three-engine equivalence across the profile,
complete freshness and frontier semantics, CDC quarantine and statistical publication
remain TARGET. IR types, admission, evaluation, runtime enforcement, authoring and proof
each require separate evidence.

### One definition, three engines

The ontology is the semantic layer. Author one question against typed concepts,
links and measures; declarations generate its pipeline rather than requiring a
handwritten DAG. The same definition targets:

- Exact in-cell execution for the declared snapshot.
- Incremental view maintenance (IVM) over admitted changes.
- Analytical OLAP over DuckDB/Iceberg (WellDB).

Equivalence is an obligation: compare canonical totals and contribution identities
at the same source frontier, time slice and definition version. An IR declaration
or an OLAP result alone does not establish three-engine execution. Preserve exact
units, scale, rounding, currency and zero-denominator behavior across engines.
An analytical or statistical result gains no stock/ledger authority by existing.

### Grain, keys and fan-out

Declare what one input and output row represents, its stable identity keys, link
multiplicities and grouping dimensions before writing an aggregate. State which
source owns each observation and which mappings supply enrichment or explicit
gaps. Missing cost is not permission to invent it from a selling price.

Count each numeric contribution at its declared grain. A one-to-many enrichment
join must not multiply the measure: two tags or two order lines do not create two
holdings. Link cardinality alone is not an aggregation contract. Declare the
contribution identities used for deduplication and correspondence checks.

### Freshness and completeness travel with the answer

Declare required sources and a freshness bound. Return per-source cursors,
observed lag, freshness, completeness and quarantine count as result metadata;
carry generation/frontier identity into Iceberg, exports, subscriptions and views.
Heartbeats establish observed freshness separately from data cursor movement.
Fresh heartbeats do not make unknown or gapped coverage complete.

A label-permitted analytical read may show a stale answer with its stale label;
an exact governed-law use refuses stale inputs. Never hide an incomplete answer
behind a fresh badge. In DATA-FRESH, a 60-second bound makes lag 30 seconds fresh
and 61 seconds stale. These are synthetic acceptance inputs, not service claims.

### Column and definition lineage

For each output measure/column, retain the contributing objects and source
columns/documents, allocation or valuation transforms, source digests, mapping
identity and cursor frontier. Pin the question/definition version that gave those
inputs meaning. This is both column lineage and definition-level lineage.
Drill-through must distinguish numeric contributions from catalog enrichment;
restricted document content is redacted with the restriction visible.

### Findings, refusals and replay-stable quarantine

Keep imported observations as evidence even when they violate a law. Capture the
raw record and cursor, link a DataQualityFinding to the violated law and source
evidence, quarantine the observation, and exclude it from eligible aggregates
while reporting its quarantine count. DATA-QUARANTINE requires durable raw data
and a `core.boundary.import_finding`; capture itself is not refused.

A native governed action proposing the same invalid state refuses and writes no
canonical business state. A finding records a nonconforming observation; it does
not approve or repair it. Reprocessing the same source event must not create a
second active finding or duplicate its contribution. Test redelivery and restart
with the same source identity, evidence and frontier: quarantine must be replay-stable.

### As-of questions, late data and restatement

Declare both effective time (when a fact applies) and knowledge time (when it was
recorded). An as-of answer pins both, plus the applicable definition and approved
restatement. Late-arriving correction evidence is appended; it does not overwrite
the earlier observation or silently rewrite old answers.

Rebuild affected temporal slices through an explicit restatement plan. Keep old
results reproducible, label the new analytical view restated, and keep original
posted books unchanged until a separate governed adjustment posts. Analytical
restatement is not evidence that the books have been reconciled.

Retraction/replacement deltas must match recompute for each bitemporal slice.
An interrupted rebuild leaves the old complete generation visible and stale;
never switch readers to a partially rebuilt generation.

Training and holdout cutoffs also use recorded-time evidence. A feature or label
recorded after its declared prediction/training/holdout cutoff cannot be treated
as if it had been known earlier. Reject publication for temporal leakage, retain
the unpublished result and report the publication-guard detail. Late data is not
a reason to silently move the split or relabel a historical prediction.

### Incremental = recompute

For each accepted change, compare the IVM delta and resulting answer with full
recompute, exact cell and OLAP at the same frontier. Include redelivery,
restart/resume, quarantine exclusion and late correction/restatement cases.
Match canonical values and contribution IDs, not just a plausible dashboard total.
Old and new generations must remain identifiable across all output sinks.

Use the nine named DATA-* acceptance scenarios for qualification. Keep runtime,
sampled differential and statistical evidence separate. A passing fixture is not
a universal IVM proof or a production forecast-quality claim. Forecast/regression
runs behind the controlled Function boundary with pinned input version, split,
model/code digest, parameters and receipt; authorized publication checks complete,
fresh, non-quarantined inputs, temporal validity and the declared holdout threshold.

### Synthetic orders and payments: before and after

**Before (pandas-style sketch).** The join grain and temporal assumptions live in
notebook code. An order with several payments repeats its order amount below.

```python
# Synthetic tables only; illustrative, not a qualified pipeline.
joined = orders.merge(payments, on="order_id")
by_region = joined.groupby("region")["order_amount"].sum()
```

**After (Norn authoring contract, TARGET, not SDK code).** Declare an Order key
and a Payment key, a typed Payment to Order link, and a question at Order grain.
Declare whether the measure is ordered amount or paid amount. Ordered amount
contributes once per Order; paid amount aggregates Payment identities explicitly
before combining at Order grain. Pin currency, rounding, region mapping, source
ownership and gaps rather than letting a join decide the meaning.

The generated pipeline must maintain that same definition across the three
engines. Every answer carries its frontier, freshness/completeness and quarantine
count. Drill-through explains source columns/documents and definition lineage.
Invalid imported observations become replay-stable findings; a governed invalid
action refuses. Late payment/correction evidence produces an explicit bitemporal
restatement, while prior knowledge-time answers remain reproducible. Prove the
claimed behavior with named correspondence scenarios before calling it CURRENT.

## Do / don't

| Do | Don't |
| --- | --- |
| Add generic semantics with spec correspondence and a second non-stock example | Put stock, GL, jewelry, tax or allocation-purpose switches in the kernel |
| Generate statements from admitted definitions; ship checked evidence in packs | Hand-write per-feature proofs or change a theorem to fit a proof |
| Use typed constructors and supported expressions | Author raw IR, ZEN/JDM or `unsafeZen` in the normal profile |
| Declare ordinary integration mappings for observed events | Introduce `observe()`, mining authority or special observation folders |
| Declare happenings, states, actions and receipts | Add `workflow()` or a second business runtime |
| Preserve costs, source allocations and immutable history | Rewrite posted purity, quantities, payment results or old settings |
| Keep synchronous laws within one cell; expose cross-cell commitments | Hide balancing work in later triggers or claim distributed atomicity |

[`norn#58`](https://github.com/Norn-cloud/norn/issues/58) is an open compatibility gap: the profile still keeps the legacy raw-LawIR overload of `Norn.law` and warns on `Norn.unsafeZen`. Prefer the typed shorthand `Norn.law(name, { on, when?, must | never | eventually, because })`, `Norn.expression`, and typed decisions; where typed law syntax is missing (such as `severity: { override }`), report the gap rather than copying raw LawIR into new apps.

## Common refusals and fixes

CURRENT codes come from the profile and corpus; semantic rows describe TARGET refusal classes, not invented stable diagnostic IDs. For named codes and scenarios, use the relevant core spec.

| Refusal | Fix |
| --- | --- |
| `NORN-FLOW-AMBIENT`, `NORN-FLOW-UNSUPPORTED-CALL` | Remove clock, random, network, or helper execution; use recorded inputs, supported expressions, or a Function behind perform |
| `NORN-FLOW-CAPTURE`, `NORN-FLOW-EXPRESSION` | Remove mutable captures, truthiness, or arbitrary predicates; use explicit typed Boolean expressions and declared inputs |
| `NORN-FLOW-SLICE` | Declare the read, path, or link in the bounded slice; do not fetch undeclared state |
| `NORN-FLOW-UNBOUNDED`, `NORN-FLOW-CONCURRENCY` | Use supported bounded retries and sequential steps; handle business concurrency through checked actions and read sets |
| `NORN-FLOW-SCHEMA-PROJECTION`, `NORN-FLOW-RESULT` | Use projectable structural schemas and explicit serializable results on every path |
| Inherited-law weakening or unmapped required semantics | Retain law origins; complete the pinned interpretation and its obligations |
| Unbalanced closure, stale allocation, or missing partition read | Include every synchronous counterpart and full read dependencies; reevaluate eligible contention or return stale-intent conflict |
| Assay changes a posted measure or approval evidence is stale | Append observation and reconciliation; bind fresh approvals to the exact revision |
| Missing payment side or uncertain external outcome | Create the linked claim or commitment; reconcile the recorded result before settlement |
| Mixed units, ambiguous book or parameter, or classification cycle | Bind exact units, instances, and effective versions; break cyclic predicates |
| Unsupported or stale proof evidence | Show unknown; implement the generic semantics or regenerate and check evidence; never lower the required assurance |

## CURRENT CLI, runtime and authoring notes (`origin/main` `591b091`)

**CLI commands (`tools/norn-ts`).** Run `bun src/cli.ts setup` (or `scripts/dev/fetch-local-runtime.sh` from the repository root) once to cache the verified prebuilt checker WASM, dev host, and Rust `norn` into `~/.cache/norn/runtime/` (or set `NORN_CHECKER_WASM_PKG` and `NORN_DEV_HOST_BIN` to local builds). Then use `bun src/cli.ts init`, `profile <file.ts>`, `check <pack-dir|project-dir|file.ts> --org ID --env ID`, `compile`, `explain`, `codegen --ts`, `plan`, `apply`, and `dev`. Profile recognition is not admission. Run `bun run typecheck` and `bun test` in `tools/norn-ts`, then `bash scripts/ci/core.sh cheap` from the repository root and the required full CI gates in the test environment.

**Typed SDK and codegen (`#1684`).** `bun src/cli.ts codegen --ts <artifacts|dir|pack|project> [--base <path>]... [--out file.d.ts] [--check]` emits a deterministic types-only `.d.ts` file containing `ActionMap`, `ObjectMap`, `DefinitionsDigest`, `NornDefinitions`, and (when catalog islands exist) `WidgetIslands` and `RequiredCatalogRegistry`. Pair `ActionMap` with `createNornClient<ActionMap>` from `@norn/sdk` (`packages/norn-sdk`), which exposes `call`, `callEncoded`, `intent`, `pinned`, `listActions`, `LiveClient`, and `parseExactJson` over the 3-arm `CallResult` (`receipt | pending | refused`).

**Declarative UI, Ring-1 checking and `<NornAppRenderer>` (`#1763`).** Author `WidgetUx` declarations with `W.widget`, `W.uiSdk`, `W.fromUiSdk`, `W.outcome`, and `W.lowerWidget` (`tools/norn-ts/src/builder/widget.ts`, re-exported from `tools/norn-ts/src/builder/index.ts`; not yet in `@norn/authoring`). Ring-1 `norn-ui-checker` (`check_artifact_set`, `check_widget_ux_set`) validates widget laws, state machines, catalog contracts, and UI SDK capabilities. `@norn/react` renders admitted `WidgetUx` documents with `<NornAppRenderer>` and `useWidgetMachine`.

**Overnight `time.WorkingWindow` (`#1683`).** `time.WorkingWindow` v0.2.0 and `time.window.materialize_interval` v0.2.0 (`registry/packs/time/calendars.ts`) add `end_day_offset` in `{0, 1}` (default `0`). `weekday` names the start date; `end_day_offset = 1` anchors `end_local` on the following civil date so overnight windows and 23 h / 25 h DST civil days resolve under the calendar's pinned tzdb and DST policy.

**Production `FunctionReceipt` (`#1688`).** `SqliteCell::retain_function_receipt`, `set_result_authorizer` (`norn_cell::ResultAuthority`), and `commit_result_refusal` reconcile, retain, and atomically consume `norn.function/v1` receipts. Hosts fail closed (`action.receipt_effect_unsupported`) until they install an authorizer.

**`AdmittedDefinitions`, `update_only` and MBT seeding (`#1769`).** `norn_cell::AdmittedDefinitions::from_envelopes` constructs one model, decoded action list, and `TransitionGuard` set whose `catalog()` binds guards by construction. `upsert_input_object` supports `writeMode: { kind: "update_only" }` (target must already exist; patches only, declares only a `modified` write). `norn-testkit`'s `AdmittedPack`, `pack_mbt::dst`, and the pack MBT driver bind transition guards and seed `protected` (`ACTOR-STAMP-1`) types through their `create_only` stamping actions.

**Exact decimals and rates.** `Norn.rate(scale, { currency: "currency", perUnit: { system: "ucum", code: "g" }, rounding: "refuse" })` emits Decimal `currencySource` plus `perUnit` at one scale; omitted rounding defaults to `refuse`. The bounded typed profile admits `Rate<C,U> * Quantity<U> -> Money<C>`, `Money<C> / Quantity<U> -> Rate<C,U>`, and same-type `+`, `-`, and comparisons; general compound units remain TARGET. Exact action and rule schemas use the same `Norn.decimal` and `Norn.rate` declarations (`{type: "number", nornType: <Decimal>}`). Declare a fixed singleton currency sibling with `Schema.Literal("USD")`; a bare `Schema.Number` keeps legacy arithmetic and cannot enter an exact computation. Decimal options accept explicit `rounding` (`refuse`, `toward_zero`, `half_even`, `half_away_from_zero`), defaulting to `refuse`. Direct decision callbacks lower each declared numeric destination to the shared rescale site; exact callbacks with sub-decisions or loop or table transforms currently refuse. Keep gram and rate working scales within the profile maximum of 18. Returned quantities and money retain their destination bindings; mixed currencies, denominator codes, and `Rate * Rate` refuse.

**Principal inputs (`PRINCIPAL-INPUT-1`).** See [Core 05](../spec/core/05-laws-authority.md#principal-input-1-declared-principal-attributes-as-typed-decision-inputs-1091). A `decide` rule that reads who is acting declares it: `Norn.access({ principalCatalog: [{ name: "approver", type: "bool", source: { kind: "role" } }, { name: "finance", type: "stringSet", source: { kind: "roleProfile", roles: ["approver", "auditor"] } }] })` declares the catalog; `Norn.decision({ principalInputs: [{ name: "isApprover", kind: { holdsRole: { role: "approver", at: "resource" } } }], input: Schema.Struct({ slice, event, principal: Schema.Struct({ isApprover: Schema.Boolean }) }) }, input => ...)` declares the input and reads `input.principal.isApprover`. The host resolves it from the verified principal and the PDP; a request that submits `principal` refuses, an unresolvable input refuses `decision.principal_input_unavailable`, and reading an undeclared `principal.*` name refuses at admission.

**Path equations (`PATH-EQ-1`).** `Norn.type` accepts `pathEquations: [{ id: "provider", left: ["fulfils", "provider"], right: ["provider"] }]`. These compile to typed EntityIR extension data and lower to chase EGDs (Core 05). Both endpoints must have the same concrete type and every link must be functional; optional paths assert equality only when both exist. Declaration validation is structural evidence; mapping-preservation admission and #712's four schema-level mutation scenarios remain TARGET until implemented and qualified.
