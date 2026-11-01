# Core specification contracts

Normative target, 2026-11-01. Read the [decisions](../../architecture/decisions/2026-11-01.md) and [thesis](../../architecture/norn-thesis.md) first. Each primitive is an implementation contract, not authoring syntax. Unlabelled detailed target choices below and in primitive specs are **[Default]** refinements of those decisions; explicit [Owner]/[Astra] requirements retain their authority.

## Contract conventions

- Existing Rust identifiers are named exactly; **target** records specify semantic fields to add/lower using those types, not a second runtime or an assertion they exist. Record fields are required unless `?`; lists are ordered only where specified, maps have canonical key ordering; unknown variants/fields refuse. No untyped extension bag may carry authority.
- `Pin = {artifact_id: String, version: String, digest: Digest}`; `Scope =` the existing scoped authority identity, including organisation/tenant/environment; `Revision` is a committed position plus definition generation. Implementations reuse existing equivalent IDs instead of creating parallel wrappers. Member identity is `(Pin, member kind, member ID)`, independent of display labels.
- `Exact` means `RuntimeValue` exact-number representation checked against `PropertyType`, `DecimalLiteral`, unit/scale constraints; never JSON binary64 for quantities/money. `Expr` means the existing admitted `ZenExpression`/RuleIR profile, extended only through shared typed semantics; no host callbacks.
- Every diagnostic reserves a stable `core.<primitive>.<reason>` target code. Register it with the existing diagnostic registry; do not silently rename existing public codes. An adapter may retain an existing transport code and add the canonical spec code. Payload: `{code, invariant_id, phase, subject_ids, definition_pins, retryable, safe_details, message_key, typed_args}`. Redact unauthorized details before returning them. Refusal leaves all business state unchanged.
- Construction creates only well-typed candidates; admission checks definitions/closure; commit checks full proposed post-state under current authority. Their evidence is independent. A missing required evaluator or exhausted bound refuses; it never yields a successful empty result.
- A generated preservation goal is `Admitted(D) ∧ Law(S) ∧ Enabled(writer,S,input) ∧ Assumptions ⇒ Law(Eval(D,writer,S,input))`. Also generate initialization, non-vacuity and explicit boundary goals. Enumerate **all** writers, including migrations/imports/timers/results; keep unsupported goals open. Reuse the runtime evaluator's operation semantics and version them in evidence.
- PR units include contract, negative scenarios, correspondence tests, source enrollment and provenance. Every executable unit runs affected crate/authoring tests, `python3 scripts/proof_enrollment.py`, `cargo xtask ci provenance`, `bash scripts/ci/core.sh cheap` (fast), workspace Clippy and required CI gates on CI/Boat. Full qualification uses `bash scripts/ci/core.sh all`, including workspace tests, SDK/WASM parity and promoted verification. No gate was executed by writing this specification.

## Status against main

Inspected public `origin/main` at **1227c72b2637a489ea2c51e51a62354376c0a322** after fetch. `P` = inspected partial substrate, `M` = target capability not established. No row is end-to-end complete. Proof `P` means narrow generated or reference evidence exists, never full implementation proof. Each linked spec identifies source paths and missing behavior.

| Primitive | IR | Admission | Evaluator | Commit enforcement | Authoring | Proof |
|---|---|---|---|---|---|---|
| [01 Values/identity](01-values-identity.md) | P: values, keys | P: codecs | P: exact runtime values | P: exact storage | P: typed properties | P: narrow arithmetic |
| [02 Concept graph](02-concept-graph.md) | P: entities/catalog | P: interface/lifecycle checks | P: effective graph | P: lifecycle/links | P: entity builders | P: reference models |
| [03 Query/compute](03-query-compute.md) | P: RuleIR/slices | P: bounded profiles | P: expressions/reactive | P: point reads | P: rules; folds pending | P: narrow induction |
| [04 Transactions](04-transactions-effects.md) | P: DecisionOutput/CellCommit | P: writer checks | P: decision preparation | P: atomic CAS/outbox | P: actions/rules | P: reference CAS/atomicity |
| [05 Laws/authority](05-laws-authority.md) | P: LawIR/structured | P: deontic/obligations | P: narrow safety | P: authz/invariants | P: structured Rust | P: limited induction; no complete dispatch |
| [06 Composition/config](06-composition-config.md) | P: packs/settings | P: dependencies/migration | P: effective definitions | P: install/preflight | P: packs/settings | M: retained law transport |
| [07 World boundary](07-world-boundary.md) | P: Functions/connectors | P: contracts/mappings | P: outbox/result handling | P: freshness/generations | P: connector/mapping | M: full uncertainty contract |
| [08 Worlds/projections](08-worlds-projections.md) | P: worlds/views/metrics | P: view/metric checks | P: replay/subscriptions | P: world/scope checks | P: view/widget | M: full projection correspondence |

Lane state at inspection: #316 merged (structured safety authoring); #322 open (EN/AR). #323/#324 open (fold declarations/fencing; declaration does not imply fold execution). B1–B3 on #213 are design, not implemented coverage. #227 open (core-aligned inventory). #270, #308–#312 and #317 open (host and surface qualification). Neither a PR body nor a passing branch fixture changes this main matrix.

Data engineering is first-class [Owner]: 03 owns declared pipelines and exact/IVM/OLAP equivalence; 07 owns CDC/provenance/findings/quarantine and ML receipts; 08 owns bitemporal questions, lineage, restatement and guarded analytical publication. At this baseline, metric IR/checking exists; `norn-metrics` is absent (open #69). #71 history-at-cursor and #97/#103 WellDB prerequisites are open. IVM/OLAP runtime, complete freshness/frontier semantics, CDC quarantine and statistical publication remain missing target coverage, not shipped capabilities.

## Dependency and ownership map

1. 01 canonical values/identity → 02 graph and 03 pure algebra.
2. 02 + 03 → 04 transaction closure and 05 runtime law semantics.
3. 05 effective-law model + 02 graph → 06 retained composition; 06 migration lowers through 04.
4. 01 + 04 + 05 + 06 → 07 performer/integration boundary.
5. 03 + 04 + 05 + 06 + 07 → 08 worlds/projections and acceptance.

These are delivery dependencies, not permission to bypass a later authority check: intermediate releases refuse operations whose necessary layer is missing. Lane A owns generic evaluation/effects; B owns laws/composition; C authors core/domain/demo packs. Host/surfaces consume the shared authority paths. Cross-lane changes coordinate on #213.

## Acceptance and evidence

The [data-engineering beat 6](../acceptance/data-engineering.md), [completeness gate](../acceptance/completeness-gate.md), [jewelry demo](../acceptance/jewelry-demo.md) and [ontology MVP](../acceptance/master-ontology-mvp.md) are target suites. Named scenarios must bind to executable tests and exact-head results before a status advances. Count each scenario once per declared profile; enumerate skips and unresolved divergence buckets.

Evidence keys include semantics/compiler digest, law and writer closure, exact dependency graph, unit/rounding profile, parameters, world/base revision, assumptions/bounds and backend/tool versions. Distinguish structural, runtime-enforced, sampled, bounded, checked reference proof and checked implementation proof. A generated goal without a checked result is unknown. An implementation proof must include translation/evaluator correspondence, not just a theorem over a second model.
