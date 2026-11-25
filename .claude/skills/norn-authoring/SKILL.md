---
name: norn-authoring
description: Write or review Norn definitions (types, machines, laws, norms, settings, flows, packs, connectors, widgets) under the authoring contract, using CURRENT syntax where v2 is not built, and run the checks that admit them.
---

# Norn authoring

## Read first

- [Authoring DX v2 spec](../../../docs/authoring/dx-spec.md): target syntax, lowering to IR, and what is CURRENT, PROPOSED, or GAP. Section 5 maps each construct to its builder file and IR kind.
- [Authoring reference](../../../docs/authoring/README.md), the [2026-11-01 decision record](../../../docs/architecture/decisions/2026-11-01.md) (which governs meaning), and the relevant [core spec](../../../docs/spec/core/README.md).
- [`tools/norn-ts/profile/norn-authoring.d.ts`](../../../tools/norn-ts/profile/norn-authoring.d.ts): the CURRENT `@norn/authoring` profile. Fixtures and the accept/reject corpus under `tools/norn-ts/corpus` show what compiles.

## The rule

**Explicit about meaning, inferred about plumbing.** Write states, transitions, who may act, what must hold, named failures, units, and external effects. Do not invent ids, versions, dependency pins, `lawVersion`, provenance, or scope boilerplate where the compiler infers them. Where CURRENT syntax still requires explicit ids, versions, or pins, copy the exact shape from a neighboring definition and keep pins aligned with the target version.

## Which syntax to write

`@norn/authoring/v2` does not exist yet. Until [#1176](https://github.com/Norn-cloud/norn/issues/1176) checks a unit off, write the CURRENT form and keep the v2 structure so the codemod can round-trip it.

| Meaning | v2 (PROPOSED) | Write today (CURRENT, `origin/main` `591b091`) |
|---|---|---|
| Thing or happening | `thing("Party", { implements, props, links, key })` / `happening(...)` | `Norn.type({ name: "<ns>.Party", role, implements, properties: Schema.Struct(...), identity, links, defaults, protected, ownedCollections, pathEquations })` |
| Interface | `thing.interface(...)` | `Norn.interfaceType({ name, title, version, properties, links, invariants })` |
| Reference to a type | imported value | imported value; use a string only for installed types with no source module |
| Link | `link.one(T).required()`, `link.many(() => T).owned()` | `links: [{ id, target: T, cardinality: { to, from, required }, acyclic, inverse }]` |
| Lifecycle | `machine: { initial, states: { s: { on: { e: { to, by, when } } }, done: final({ immutable: true }) } }` | `lifecycles: [{ property: "status", initial, states: { s: { on, after, final, resting, immutable } }, events }]` (machine shape, `LIFE-SOUND-1..9`) or flat `transitions: [...]`; separate `Norn.action` (`fire_transition`) and `Norn.access` per event |
| Action and approval | `action(...)` | `Norn.action` with `effect` (`fire_transition`, `upsert_input_object` with `writeMode: { kind: "upsert" \| "create_only" \| "update_only" } \| { kind: "draft_only", lifecycle, state }`, or `mutate_parent_with_rows`), `fromCaller`, `independentOf`, `aggregates`, `createLinks`, `synchronousRule`, `inputLabels`, `inputGroups`; `Norn.approval(...)` for human approval gates; `Norn.callerId()` in `derive` |
| Law | `law("name", { on: T, when, must, because })` | `Norn.law("name", { on: T, when?, must \| never \| eventually, within?, severity?, unless?, because })` (inferred id, scope, dependencies); legacy raw `LawIR` overload remains only for `#58` |
| Norms and examples | same | `Norn.obligation`, `Norn.prohibition`, `Norn.permission` (`cite`, `yieldsTo`, `unless`), and `Norn.example(name, { of, given, expect })` |
| Defeasible law / override | `unless: { label: { when, because } }`, `severity: { override }` | `unless` record syntax is CURRENT in authoring and checker admission; commit-path evaluation (`core.laws.unsupported_semantics`) and `severity: { override }` remain engine gaps (`#1126`). Do not emulate them with `warning` or by weakening a law |
| Guard, decision, derivation | typed lambda | `Norn.expression(Schema, x => ...)`, `Norn.decision(meta, callback \| rows)` (including `hit: "unique"` tables and `principalInputs`), `Norn.orderedFold(...)` |
| Parameter / setting | `param(...)` | `Norn.setting({ id, title, version, settingKey, effectiveFrom, effectiveTo?, supersedes?, review, scopeRequired, valueType, value })` (`"number"` requires exact `DecimalLiteral` string, `PARAM-EXACT-1`) |
| Flow, function, failure | `flow("name", { input, retry, run: async ... })`, `fn(...)`, `failure(...)` | `Norn.flow(meta, Effect.gen(function* () { yield* Norn.perform(...) }))`, `Norn.fn(...)`, `Data.TaggedError` |
| Connector and mapping | `connector(...).map(source, Target, s => ({...}), { ignore, authority: "evidence" })` | `Norn.connector` (`poll` / `pull`), `Norn.sourceSchema`, `Norn.mapping` (reference-bound source pin, `MAP-ENUM-TEXT-1`), `Norn.ontologyPack`, `Norn.packLink` |
| Questions, dashboards, transforms, models | `question(...)` | `Norn.question` (`link`/`links` paths, `asOf: "effective" \| "knowledge"`), `Norn.metricId(...)`, `Norn.dashboard`, `Norn.transform`, `Norn.model`, `Norn.labels`, `Norn.markings` |
| Pack | `pack("accounting", { version, requires, exports })` | pack directory compiled by `compilePack`; no composition-pack TS constructor yet |
| Widgets and UI (`#1763`) | declarative UI in pack | `W.widget`, `W.uiSdk`, `W.fromUiSdk`, `W.outcome`, `W.lowerWidget` from `tools/norn-ts/src/builder/widget.ts` (`builder/index.ts`, not yet in `@norn/authoring`); admitted by Ring-1 `norn-ui-checker` (`check_artifact_set`, `check_widget_ux_set`); rendered by `<NornAppRenderer>` in `@norn/react` |

### Runtime and SDK contracts

- **Typed SDK and codegen (`#1684`):** `bun tools/norn-ts/src/cli.ts codegen --ts <path> [--base <path>]... [--out file.d.ts] [--check]` emits types-only `ActionMap`, `ObjectMap`, `DefinitionsDigest`, `NornDefinitions` (and `WidgetIslands` / `RequiredCatalogRegistry` when catalog islands exist). Pass `ActionMap` to `createNornClient<ActionMap>` from `@norn/sdk` (3-arm `CallResult`: `receipt | pending | refused`, `callEncoded`, `intent`, `pinned`, `listActions`, `LiveClient`, `parseExactJson`).
- **Overnight `time.WorkingWindow` (`#1683`):** `time.WorkingWindow` v0.2.0 and `time.window.materialize_interval` v0.2.0 (`registry/packs/time/calendars.ts`) add `end_day_offset` in `{0, 1}` (default `0`). `weekday` anchors the start civil date; `end_day_offset = 1` ends on the next civil date, supporting overnight shifts and equal-endpoint civil days (23 h spring-forward, 25 h fall-back).
- **Production `FunctionReceipt` (`#1688`):** `SqliteCell::{set_result_authorizer, retain_function_receipt, commit_result_refusal}` (`norn_cell::ResultAuthority`) resolves and atomically consumes retained `norn.function/v1` receipts on `SqliteCell`. Hosts fail closed (`action.receipt_effect_unsupported`) until they install an authorizer.
- **Runtime catalog and MBT seeding (`#1769`):** `norn_cell::AdmittedDefinitions::from_envelopes` builds one model, action list, and guard set (`catalog()` binds `TransitionGuard`s by construction). `norn-testkit`'s `AdmittedPack`, `pack_mbt::dst`, and the pack MBT driver bind guards and seed `protected` (`ACTOR-STAMP-1`) types through their `create_only` stamping actions.

### Hard rules

- Write typed lambdas instead of string expressions. `Norn.unsafeZen` and raw JDM are the open [#58 gap](https://github.com/Norn-cloud/norn/issues/58), never an endorsed pattern.
- Lambdas are compiled statically under `norn-zen/7` (`crates/norn-expr/README.md`) and never executed. Supported operations include comparisons, boolean ops, checked arithmetic (`%` is not admitted), `?:`, `??`, path reads, string methods, array closures (`.some`, `.every`, `.filter`, `.map`, `.filter(...).length`), `Norn.sum`, `Norn.floor`, `Norn.byteLength`, `Norn.take`, date/timestamp helpers (`parseDate`, `parseTimestamp`, `dateBefore`, `timestampBefore`, `timestampAtOrBefore`, `daysBetween`, `addDays`, `secondsBetween`, `addSeconds`), interval helpers (`intervalsOverlap`, `intervalGapSeconds`, `intervalIntersectionSeconds`), and `resolveLocalTimestamp`. Local helper calls, clocks, randomness, mutation, and loops are refused.
- Object references belong in `links`, never scalar `*_id` strings or `Norn.ref` (which names a value type).
- Money and quantities are exact: `Norn.decimal(scale, { unit })`, `{ currency: "<prop>" }`, `{ unitSource: [...] }`, `Norn.rate(...)`, `Norn.boundDecimal(group)`. Never use `Schema.Number` in exact arithmetic.
- A balanced constructor covers its own pairs; it does not certify account selection, valuation, or arbitrary writers, and cardinality alone does not prevent double counting.
- No hand-written per-feature theorem statements or models as application evidence. Proof bodies may use Lean only against generated statements and count only after checking.
- Keep domain logic and opcodes out of the kernel. Domain meaning belongs in packs.
- Never add `observe()`, `workflow()`, or a second business runtime. `Norn.flow` compiles to lifecycle and rule data.
- Never hide balancing work in a later trigger, rewrite posted history, weaken an inherited law, or treat `unknown` evidence as proved.

## Semantics to get right

1. State the invariant, subject type, units, and atomic cell boundary before choosing syntax. Reuse `registry/core` interfaces (`core.Agent`, `core.Resource`, `core.EconomicEvent`, `core.LedgerEffect`, `core.Claim`, `core.Commitment`, `core.Hold` in `registry/core/src/rea.ts`).
2. Separate things from happenings: catalog `Item` is not `Holding`; a `Claim` is not a `Commitment`. Posted happenings are immutable; correct them with a linked reversal.
3. Give every policy that requires approval or evidence an executable path (an event or action with an authorized role), and give every machine terminal or resting paths and named refusals.
4. Enumerate every writer: actions, imports, timers, flow results, and synchronous rules. Commit all synchronous balancing effects in one cell commit.
5. Attach laws where their quantifiers live. Implementations inherit interface laws and cannot weaken them (`law.model.weakens_inherited`).
6. Route external computation and effects through `perform` on a declared Function, and accept results through a checked action. Imported rows stay evidence until a checked action promotes them.
7. Express cross-link derivations as questions or transforms, not stored properties. Declare grain, units, freshness, and zero-denominator behavior.
8. Follow the domain acceptance contracts when touching jewelry or data engineering:
   - **Jewelry** ([`docs/spec/acceptance/jewelry-demo.md`](../../../docs/spec/acceptance/jewelry-demo.md)): sales post delivery, cost allocation, and payment or receivable with balanced per-currency effects (scrap buyback is a new exchange unless it is a sale return); assays append versioned evidence and reconcile posted holdings via explicit adjustments rather than overwriting purity; melts reserve inputs, fund WIP, record output and loss provenance, invalidate stale approvals on revision, and post output, loss, and value effects atomically.
   - **Data engineering** ([`docs/spec/acceptance/data-engineering.md`](../../../docs/spec/acceptance/data-engineering.md), [reference data section](../../../docs/authoring/README.md#data-engineering-target-unless-marked-current)): declare one question for exact cell, IVM, and OLAP (`DuckDB`/`DataFusion`) with matching canonical totals and contribution IDs at the same frontier; specify grain, fan-out, freshness, completeness, lineage, and bitemporal cutoffs; quarantine invalid imports as replay-stable `DataQualityFinding` evidence while refusing invalid native actions.
9. Label proof evidence honestly. Only the constitution Melt law has a generated model-checked result (`verification/profiles/constitution-one-create.json`); core laws use hand-written reference models. Never claim a law is proved because it compiles or passes examples.

## Checks to run

From `tools/norn-ts` (run heavy Rust builds and full suites on CI or disposable Boat sandboxes, never on shared dev hosts):

1. Cache prebuilt checker WASM, dev host, and Rust `norn` once: `bun src/cli.ts setup` (or `scripts/dev/fetch-local-runtime.sh` from repo root), or set `NORN_CHECKER_WASM_PKG` and `NORN_DEV_HOST_BIN` to local builds.
2. Profile recognition (not admission): `bun src/cli.ts profile <file.ts>`.
3. Authoritative Rust checker admission: `bun src/cli.ts check <pack-dir|project-dir|file.ts> --org ID --env ID`.
4. Types-only SDK codegen check (when changing actions, objects, or catalog islands): `bun src/cli.ts codegen --ts <path> [--base <path>]... --out <file.d.ts> --check`.
5. Registry regeneration: when changing registry sources, regenerate via the registry test path rather than `compile` (`compile` emits one scoped draft array, whereas checked-in `registry/*/artifacts/*.json` are separate files with system envelopes): `NORN_PACKS_UPDATE=1 bun test test/registry-packs.test.ts` (and `test/core-rea.test.ts` for core).
6. SDK and repo gates: `bun run typecheck` and `bun test` in `tools/norn-ts`; `python3 scripts/record_new_files.py --check` and `bash scripts/ci/core.sh cheap` from repo root.
7. Add named positive, refusal, and correspondence scenarios for changed semantics (no partial writes, replay idempotency, relevant races).

Report changed definitions, invariants and scenarios, exact-head gate results, and remaining gaps marked CURRENT, PROPOSED, or GAP. A review verdict is **approve** or **needs-attention**, with severity and confidence per finding.
