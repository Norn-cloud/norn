---
name: norn-authoring
description: Write or review Norn definitions (types, machines, laws, flows, packs, connectors) the one right way under the Authoring DX v2 spec, using today's CURRENT syntax where v2 is not built, and run the checks that admit them.
---

# Norn authoring

## Read first

- [Authoring DX v2 spec](../../../docs/authoring/dx-spec.md): the target surface, its lowering to IR, and what is CURRENT vs PROPOSED vs GAP. Its lowering table (§5) says which builder file and IR kind each construct becomes.
- [Authoring reference](../../../docs/authoring/README.md), the [2026-11-01 decision record](../../../docs/architecture/decisions/2026-11-01.md) (it governs meaning) and the relevant [core spec](../../../docs/spec/core/README.md).
- [`tools/norn-ts/profile/norn-authoring.d.ts`](../../../tools/norn-ts/profile/norn-authoring.d.ts): the only CURRENT import surface (`@norn/authoring`). Fixtures and the accept/reject corpus under `tools/norn-ts/corpus` show what compiles.

## The rule

**Explicit about meaning, inferred about plumbing.** Write states, transitions, who may act, what must hold, named failures, units and external effects. Never invent ids, versions, dependency pins, `lawVersion`, provenance or scope boilerplate in v2 source. In CURRENT syntax you still have to write that plumbing: copy it exactly from a neighboring definition and keep pins in step with the target's version.

## Which syntax to write

`@norn/authoring/v2` does not exist yet. Until [#1176](https://github.com/Norn-cloud/norn/issues/1176) checks a unit off, write the CURRENT form and keep the shape the v2 form will take, so the codemod can print it back.

| Meaning | v2 (PROPOSED) | Write today (CURRENT) |
|---|---|---|
| Thing / happening | `thing("Party", { implements, props, links, key })` / `happening(...)` | `Norn.type({ name: "<ns>.Party", role, implements, properties: Schema.Struct(...), identity, links })` |
| Interface | `thing.interface(...)` | `Norn.interfaceType({...})` |
| Reference to a type | the imported value | the imported value; a string only for installed types with no source module |
| Link | `link.one(T).required()`, `link.many(() => T).owned()` | `links: [{ id, target: T, cardinality: { to, from, required } }]` |
| Lifecycle | `machine: { initial, states: { s: { on: { e: { to, by, when } } }, done: final({ immutable: true }) } }` | `lifecycles: [{ property: "status", initial, terminal, immutable, transitions: [{ id, from, to, guards }] }]` + one `Norn.action` (`fire_transition`) and one `Norn.access` rule per event |
| Law | `law("name", { on: T, when, must, because })` | `Norn.law({ id, title, version, law: LawIR, dependencies })`; prefer an interface invariant with `Norn.expression(Schema, self => ...)` and a law `response: { kind: "invariant" }` (see `registry/core/src/restatement.ts`) over a ZEN string |
| Guard / derivation | typed lambda | `Norn.expression(Schema, x => ...)` |
| Flow | `flow("name", { input, retry, run: async (input) => { await step(...); await wait.for(...) } })` | `Norn.flow(meta, Effect.gen(function* () { yield* Norn.perform(...) }))` (stays accepted in v2) |
| Function / failure | `fn(...)`, `failure("Tag", {...})` | `Norn.fn(...)`, `Data.TaggedError` |
| Connector | `connector(...).map(source, Target, s => ({...}), { ignore, authority: "evidence" })` | `Norn.connector` + `Norn.sourceSchema` + `Norn.mapping` + `Norn.ontologyPack` / `Norn.packLink` |
| Pack | `pack("accounting", { version, requires, exports })` | a pack directory compiled by `compilePack`; no composition-pack constructor yet |
| Defeasible law / override | `unless: [...]`, `severity: { override }` | **not available** (engine gaps, spec §8, #1126). Do not emulate them with `warning` or by weakening a law; report the gap |

Hard rules in both syntaxes:

- No string expressions you could write as a typed lambda. `Norn.unsafeZen` and raw JDM are the open [#58 gap](https://github.com/Norn-cloud/norn/issues/58), never an endorsed pattern.
- Lambdas are compiled, never run: only the subset in spec §4.5 (comparisons, boolean ops, arithmetic, `?:`, `??`, path reads, string builtins, `date`/`timestamp` parsing). In CURRENT syntax the typed builder (`tools/norn-ts/src/builder/expr.ts`) has no closures and no `sum`, so `some`/`every`/`filter`/`map`/`sum` are PROPOSED (v2 only); do not write them in CURRENT sources. `%` is not admitted by `norn-zen/6` in either syntax. No local helper calls, clocks, randomness, mutation or loops.
- Object references are links, never scalar `*_id` strings or `Norn.ref` (which names a **value type**).
- Money and quantities are exact: `Norn.decimal(scale, { unit })`, `{ currency: "<prop>" }`, `Norn.rate(...)`. Never `Schema.Number` in exact arithmetic.
- No domain code or opcodes in the kernel. Domain meaning lives in packs.

## Semantics to get right

1. State the invariant, its subject type, units and atomic cell boundary before choosing syntax. Reuse `registry/core` interfaces (`core.Agent`, `core.Resource`, `core.EconomicEvent`, `core.LedgerEffect`, `core.Claim`, `core.Commitment`, `core.Hold`); `physics.ts` is renamed `rea.ts` in a separate unit.
2. Things vs happenings: catalog Item vs Holding/lot; a Claim is never a Commitment. Posted happenings are immutable; a correction is a new linked reversal.
3. Every policy that needs approval or evidence needs an executable path (an event with `by`). Every machine needs terminal paths and named refusals.
4. Enumerate all writers: actions, imports, timers, flow results, triggered rules. All synchronous balancing effects commit together; a later trigger never finishes today's balance.
5. Laws attach where they govern; implementations inherit interface laws and never weaken them (`law.model.weakens_inherited`).
6. External effects go through `perform` / `step` on a declared Function; results are accepted by a checked action. Imported rows are evidence until a checked action promotes them.
7. Cross-link derivations are questions, not stored props. Declare grain, units, freshness and zero-denominator behavior.
8. Jewelry (sell, assay, melt) and data-engineering work have their own contracts: [jewelry acceptance](../../../docs/spec/acceptance/jewelry-demo.md), [data acceptance](../../../docs/spec/acceptance/data-engineering.md) and the [reference's data section](../../../docs/authoring/README.md#data-engineering-target-unless-marked-current).
9. Proof claims: only the constitution Melt law has a generated model-checked result today (`verification/profiles/constitution-one-create.json`); core laws have hand-written reference models. Never report a law as proved because it compiles; unknown is never proved.

## Checks to run

From `tools/norn-ts` (heavy runs go to Boat, per repo rules):

1. `bun src/cli.ts profile <file.ts>`: profile recognition (not admission).
2. `bun src/cli.ts check <pack-dir|file.ts> --org ID --env ID` with `NORN_CHECKER_WASM_PKG` built as the SDK README says: the authoritative Rust checker.
3. `bun src/cli.ts compile ... --out FILE`, then diff against the checked-in `registry/*/artifacts/*.json` when you changed a registry source. Generated artifacts change only when meaning changed.
4. `bun run typecheck` and `bun test` for any SDK change; `bash scripts/ci/core.sh cheap` from the repo root.
5. For changed semantics add named positive and refusal scenarios (no partial writes, replay/idempotency, the relevant races). For a new law on a type with live data, record what `norn plan` would report (spec §9); until `plan` evaluates laws, say so.

Report: changed definitions, the invariants and scenarios, exact-head gate results, and remaining gaps marked CURRENT / PROPOSED / GAP. A review verdict is **approve** or **needs-attention**, with severity and confidence per finding.

## Appendix: review prompts kept from the previous skill

### Jewelry review prompts

- Sell: delivery plus payment/receivable, original-cost allocations and balanced per-currency effects. Scrap buyback is a new exchange unless it is a true sale return.
- Assay: append versioned evidence. A posted holding's changed fine-metal measure requires an explicit reconciling movement and variance counterpart; never overwrite purity/history.
- Melt: reserve inputs, fund WIP/`melting`, record output and loss provenance, derive declared totals/ratio, provide APPROVE/REJECT paths, invalidate stale approvals, and post output + loss + value effects atomically.
- A balanced constructor covers its own pairs; it does not certify account selection, valuation or arbitrary writers. Cardinality alone does not prevent double counting.

### Data engineering checklist (TARGET unless verified CURRENT)

Read [the authoring section](../../../docs/authoring/README.md#data-engineering-target-unless-marked-current), [acceptance](../../../docs/spec/acceptance/data-engineering.md) and the decision record's data-engineering section.

- Declare one question for exact cell, IVM and DuckDB/Iceberg; require equivalent canonical totals and contribution IDs at the same version, temporal slice and frontier. Generated pipelines replace authored DAGs.
- Specify grain, stable keys, source ownership/gaps, units, rounding and fan-out; distinguish enrichment from numeric contributions. Use synthetic orders/payments only for a generic contrast.
- Return per-source cursors, lag, freshness, completeness, quarantine count and generation identity; fresh does not imply complete. Stale analytical reads need labels; exact governed-law use refuses.
- Preserve column/source-document and definition-level lineage, including transforms, mapping versions/digests and visible redaction.
- Capture invalid imports as evidence plus law-linked DataQualityFinding/quarantine, not refusals; native invalid actions refuse. Replay/redelivery creates no duplicate active finding or contribution.
- Pin effective and knowledge times. Append late evidence; restate explicitly, preserve old answers/books and expose only complete generations. Reject publication using features/labels recorded after their declared cutoff.
- Check incremental = recompute, exact cell and OLAP through the nine DATA-* scenarios, including restart, quarantine and restatement. Guard statistical publication and retain refused unpublished results.
- Verify MappingIR, DataQualityFinding, lineage and QuestionIR/metric behavior on main before marking anything CURRENT; distinguish IR, checking, runtime, authoring and proof evidence.

### Reject these shortcuts

- No domain code/opcodes in the kernel, including stock helpers renamed as generic operations. Domain compositions must expose their reads and effects.
- No hand-written per-feature theorem statements/models as application evidence. Proof bodies may be Lean only against generated statements and count after checking.
- No raw IR/`unsafeZen`/JDM in the normal authoring profile. Current `Norn.law`/`unsafeZen` leakage is the open [#58 gap](https://github.com/Norn-cloud/norn/issues/58), not an endorsed pattern; use supported typed expressions or report missing SDK semantics.
- No special observation construct, `observe()`, mining authority, `workflow()` or separate business runtime. Current `Norn.flow` is compiled lifecycle syntax.
- No hidden future trigger to finish balancing today's transaction, silent history rewrite, weakened inherited law or “unknown = proved.”

### Validate and return

- Use the reference's CURRENT command sequence: profile → compile/admission. Add named positive/refusal/correspondence scenarios for changed semantics; verify no partial writes, replay/idempotency and relevant races.
- Run the repo-required gates in its prescribed environment. SDK changes need its typecheck/tests; engine changes need affected tests, enrollment/provenance, cheap and required full gates. Never claim TARGET examples ran against today's SDK.
- Report changed definitions, invariants/scenarios, exact-head gate results and remaining semantic gaps. Review verdict: **approve** or **needs-attention**, with severity/confidence for findings. Leave merge/deploy decisions to the applicable repo rules and task authorization.
