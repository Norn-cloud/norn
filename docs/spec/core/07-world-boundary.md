# Core 07: Controlled world boundary

Purpose [Owner]: integrations are packs of connectors, source ontology, mappings and governed write-back; change feeds and Functions make data engineering first-class. [Common conventions](README.md) apply.

## IR contract

Reuse `ConnectorRoot`, `SourceSchemaIr`, `MappingIr`, mapping evidence/gaps, `FunctionManifest`, invocation/result contracts, `BlobRef`, `RelevantStateToken`, `Command`, `Generation` and `IdempotencyKey`.

| Target record | Required fields |
|---|---|
| Feed declaration | connector instance/scope, exact source schema + mapping pins, partitions/cursor ordering, snapshot-to-CDC handoff rule, dedup key, event/recorded-time fields, heartbeat/lag policy, tombstone semantics, declared coverage gaps. |
| Observation | source event ID, partition/cursor, operation insert/update/delete, source document ref/digest, source effective time, local recorded revision, raw content ref, mapping pins, typed multi-object role links, quality/coverage evidence. |
| Finding | stable ID from observation + law/version + evaluation context, law ID, reason, offending fields, evidence refs, status open/resolved/superseded, quarantine disposition; retain original finding on correction. |
| Outbound intent | operation ID, Function/connector pin, destination, canonical input digest, capability/secret handles, actor/delegation, retry/timeout/provider dedup contract, relevant-state pins, compensation action reference. |
| Outcome | operation/generation/attempt, state, provider receipt/reference, result/error schema + digest, observed/recorded time, reconciliation evidence, causal original operation; outputs are proposals, never direct writes. |

Functions use canonical `FunctionManifest {abi, function_id, version, input_schema, output_schema, error_schema, implementation, dependency_closure, runtime, platform, capabilities, determinism, bounds, cache, retention, steps}`. Admission rejects a missing `dependency_closure` and requires its declared digest to equal the digest computed from its entries. WIT/language bindings must agree; opaque scoped secret handles resolve only inside authorized performers, never the pure evaluator.

The checker requires `dependency_closure.entries` in strictly increasing order
by `name`. Names must be non-blank; empty or whitespace-only names refuse.
Each name must be unique, regardless of entry kind or digest;
duplicates and out-of-order names refuse rather than being silently sorted or
deduplicated. An `OciImage` implementation with an empty dependency closure
refuses admission.

OBS-EVIDENCE-1 findings may reference a law by its artifact ID, stable LawIR ID
and canonical declaration digest. A law pin records identity; it does not prove
evaluation, authorize promotion or inherit an invariant's verified classification.
The observation evaluator must bind that pin to the retained governing law.
Definition snapshots retain exact `lawArtifacts` envelopes, canonicalized by
artifact ID and included in the model hash; installation checks them against
the canonical admitted set. Historical lookup uses the pinned definition version.
CP2 resolves only exact entity-root laws directly attached to the observed type.
Applicable inherited or effective-instance scope refuses with
`core.laws.unsupported_semantics` and an `unsupported until B2` detail, preserving
LAW-COMPLETE-1 / LAW-SUPPORTED-1 rather than omitting governing constraints.

LAW-COMMIT-1 uses the metered expression evaluator for CP2 pointwise laws:
`always`/`never` predicates with an optional Boolean activation condition.
Unsupported timing, response, lifecycle or commitment scope fails closed with
`core.laws.unsupported_semantics`; native violations use `core.laws.violated`.

OBS-EVIDENCE-1 evaluates observations against those same retained direct laws,
retaining the actual law ID/declaration digest in each finding. The emitting
receipt pins the definition version for historical re-evaluation; current laws
cannot substitute for that snapshot. A law pin is classified as `Integrity` or `Quality` only
when it resolves to the supported direct-law contract.

Pointwise recovery checks the violated rule under an authenticated retained
catalog without requiring old outcome values to remain identical. Lifecycle
out-of-band findings require state-aware recovery and refuse pointwise revalidation.

OBS-LEGACY-1: law-bearing snapshots without retained laws refuse with
`core.boundary.legacy_law_snapshot`; findings pinned to definition version 0
refuse with `core.boundary.unpinned_finding`, without effects or substitution
from the current model/catalog: legacy stores must be recreated; migration is out of scope. Authenticated migration is tracked in [#505](https://github.com/Norn-cloud/norn/issues/505) for post-Nov 5.
Kernel-owned naming counters remain outside the tenant action catalog and do
not acquire unrelated tenant laws; unknown application subjects still refuse.

## Ingestion versus governed action

[Owner] Imported events already happened. Validate their framing/provenance; durably capture representable source observations and advance the cursor atomically with capture. Mapping/law violations produce **findings + quarantine**, not a refused source fact. Quarantined rows retain raw evidence and stay out of authoritative aggregates unless an explicit observed-data question includes them with quality labels. Invalid transport/authentication is still a boundary failure; it is not a business-law refusal of an external occurrence.

CDC replays deduplicate by `(connector instance, partition, source event identity)`. Same ID/different bytes is an evidence conflict finding. Snapshot handoff records its watermark then consumes CDC without gaps/duplicates. Persist capture, dedup record and cursor together; crash/restart never advances past uncaptured data. Multi-object events preserve each role; Item and Holding mappings remain separate. Declared gaps make coverage incomplete, not silently complete.

Canonical promotion/write-back is a separate governed action under 04/05. Its laws may **refuse** the proposed commit. Observational truth and authoritative posted truth are separate projections; source violations cannot authorize native mutations. Write-back carries causal source/operation IDs to suppress echoes without discarding independent evidence.

CP2 quarantine constraints include integrity-classified invariants, error-severity
direct safety laws, and error-severity findings from supported built-in rules.
Warning-severity built-in findings do not quarantine. Recovery uses revalidation:
an appended corrected observation is eligible only when every such constraint passes. `authoritative_object` excludes quarantined
records; raw reads retain source truth. Finding status is per law: a correction that passes law A resolves finding A
without rewriting the original, even if law B still has an open finding.
Object quarantine remains while any finding under the declared quarantine
constraints is open; recovery requires every such finding to be resolved.
Recovery re-evaluates each finding's rule using the retained definition model
pinned by its emitting receipt, including after a definition install; the current
catalog never substitutes. Quarantine derives from these version-bound open
findings, and changed failing values do not resolve a still-violated rule.
Capture atomically suppresses an already proven finding ID, including after
redelivery/restart; retry normalization uses the original commit cutoff.

The CP2 bridge contract captures content-addressed evidence bundles over the
sealed raw-envelope digest and exact source/mapping/definition pins, preserving
diagnostics and nonconforming records, and issues observed-write authority only
after cell-owned recomputation matches the pinned bundle
(`core.boundary.bundle_mismatch` on disagreement).
CP2 authoritative/governed non-main reads touching a source-disputed or NFC-quarantined subject refuse `core.world.source_dispute_unqualified` until Core 08 retains the source-evidence frontier; they never omit the overlay.
OBS-EVIDENCE-1 uses NFC only for inbox lookup keys, preserves non-NFC spelling evidence as bytes in a durable “identity normalized; possible collision” finding, and refuses convergence, authoritative promotion and governed reads (rechecked at commit) of every mapped subject in that bucket pending the separate opaque/signed source-byte retention and byte-exact identity bridge.

FEED-CONFLICT implements OBS-EVIDENCE-1 for mapped captures: compare the immutable
`kind`, `source_version`, `payload_digest` and redaction record for the trusted
connector/partition plus source/record/event identity, matching the ingress oracle.
Receipt time, cursor and pipeline metadata do not change source-event content.
In one capture transaction, preserve the first bundle and retain conflicting raw
input, pins, diagnostics and a replay-stable `core.boundary.import_finding` in the
Observed inbox. The conflicting candidate cannot be applied and every affected mapped subject, including the preserved first canonical object, is quarantined from authoritative/governed reads and aggregates (with visible counts), may appear only as `disputed` in label-permitted analytical reads, and is released only by an explicit governed accept-first, accept-second-as-correction or reject review action recorded with its own receipt. Rechecking recomputes
both admitted bundles; stored finding claims confer no authority. Redelivery and
restart add no second conflict finding; ordinary corrections do not resolve it.
Full cursor convergence and conflict capture for ordinary unmapped submissions
remain follow-up cuts for this CP2 bundle bridge.

## Perform and recovery semantics

All Functions, including ML/heavy pure computation, run behind perform and return recorded schema-checked receipts. Pin training/input snapshots, model/code/dependency digests, seeds where relevant, bounds and result blobs. ML confidence is not proof or write authority. Reconcile output with input identity/freshness/current authority and publish only through a guarded action.

Commit+outbox is atomic. State graph: requested → dispatched; dispatched → succeeded/failed/outcome-unknown; unknown → reconciliation-required; reconciliation → confirmed success/failure or remains unresolved. A known successful compensable consequence may enter compensation-pending → compensated; failed compensation remains pending with failure evidence. “Failed” means known failure, not timeout. Pre-dispatch cancellation is recorded separately.

At-least-once dispatch preserves operation ID across attempts. Retry ambiguous outcomes automatically only under an explicit provider idempotency/lookup contract; otherwise require reconciliation/manual evidence. Revocation blocks fresh attempts; an already dispatched result still enters receipt history. Cancellation racing with success records the success and starts a separately authorized compensation. Replay uses recorded outcomes and never calls a provider.

| Invariant / layer | Stable target code (finding codes are not refusals) |
|---|---|
| FEED-CURSOR-1: capture/dedup/cursor atomic; ingestion commit | `core.boundary.cursor_gap` |
| OBS-EVIDENCE-1: preserve imported truth/provenance, quarantine violations; capture/evaluation | `core.boundary.import_finding` |
| EFFECT-IDENTITY-1: attempts retain exact operation identity; dispatch | `core.boundary.identity_mismatch` |
| EFFECT-UNKNOWN-1: uncertainty never means failed/succeeded; reconciliation | `core.boundary.outcome_unknown` |
| FUNCTION-CAP-1: pinned typed bounded capability boundary; admission/dispatch | `core.boundary.capability_denied` |
| RESULT-FRESH-1: output receipt is not mutation authority; governed commit | `core.boundary.stale_result` |

Evidence: structural mapping/manifest admission; runtime capability/freshness enforcement; sampled crash/retry/CDC correspondence; external assumptions remain explicit open proof goals. Quarantine is evidence-bearing handling, never a claim of correction.

## Named acceptance scenarios

- **FEED-CONFLICT-RACE:** Given mapped preparation, when another connection captures conflict or NFC evidence before the observed commit transaction begins, then recheck that exact bundle inside the transaction and refuse promotion/consumption. Ordinary corrections under a different source event retain their existing capture path.
- **FEED-RESTART:** Given captured cursor 7, when crash occurs before cursor 8 capture commits, then resume at 8 and store one observation; post-commit crash deduplicates redelivery.
- **EVENT-MULTI:** Given one ERP event for supplier, voucher and holding, when mapped, then retain all role links and source document; neither a case key nor Item replaces the holding.
- **OBS-BAD:** Given imported purity 1100‰, when evaluated, then capture a quarantined finding; identical native write refuses without business changes.
- **OBS-RECOVERY-UPGRADE:** After a definition install, an old finding stays open while its pinned rule fails; only a correction satisfying that retained rule resolves it. New definitions never supersede the old finding implicitly.
- **OBS-RECOVERY-LIFECYCLE-REFUSE:** An unchanged out-of-band lifecycle finding cannot resolve through pointwise evaluation; unsupported state-aware recovery refuses.
- **OBS-LEGACY-LAW-REFUSE:** A legacy snapshot omitting admitted laws refuses with `core.boundary.legacy_law_snapshot` and leaves receipts, definitions and facts unchanged.
- **OBS-LEGACY-V0-REFUSE:** A version-0 finding refuses quality/recovery reads with `core.boundary.unpinned_finding`; the current model cannot supply its missing authority and no receipt or fact changes.
- **EFFECT-UNCERTAIN:** Given provider success followed by lost reply, when retried, then dedup/lookup resolves once or outcome remains unknown; never issue a new payment ID.
- **EFFECT-COMPENSATE:** Given confirmed payment 25, when cancellation wins after dispatch, then a separate refund 25 records its own outcome; failed refund remains outstanding.
- **FUNCTION-ML:** Given a pinned training table and model Function, when it returns a forecast, then record output/input digests; stale or leaked-future input cannot pass publication guards.

## Main and gaps

Present: `crates/norn-ir/src/{connector,source_schema,mapping,mapping_evidence,function}.rs`, `crates/norn-kernel/src/effect.rs`, `crates/norn-cell/src/host.rs`, `registry/packs/{erpnext,shopify,odoo}/`. Source contracts and outbox/freshness machinery exist, not complete CDC/quarantine/reconciliation or public Function execution. #69's `norn-metrics` temporal tables are pending, not general ML publication. #270/#308–#312 host/surfaces must use this same authority boundary.

## Work breakdown (each PR ≤400 lines; demo priority first)

1. **Demo priority:** feed/cursor/observation contract, then atomic dedup/checkpoint capture, then FEED-RESTART tests.
2. **Demo priority:** ERPNext/Shopify synthetic mappings + role/provenance/gap declarations, then OBS-BAD quarantine/finding evaluation; [beat 6](../acceptance/data-engineering.md).
3. **Demo priority:** typed ML receipt inputs/outputs and freshness/temporal publication guards using #69/08; FUNCTION-ML negative cases.
4. Complete B1 mapping/role preservation with 06; EVENT-MULTI and source echo tests.
5. Add uncertainty/reconciliation state contract, then dispatch/lookup tests, then compensation races with 04; each supported provider declares its assumptions.
6. Qualify manifest-projected WASM/worker execution, secrets, bounds and replay; unsupported profiles stay refused.

## Host Function execution

FUNCTION-HOST-1: `implementation.format: host` declares host-provided native code. `runtime.id` and `runtime.version` pin the implementing crate name and version; `implementation.digest` pins its source tree, with the dependency closure and runtime build/configuration digest retained separately. The locator is exactly `<crate>/<version>@<implementation digest>` and is informational, never permission to load arbitrary code. The host must resolve its configured implementation by these exact pins or refuse.

Host execution uses the existing unqualified `effectful` profile and cannot claim WASM qualification, container isolation or action caching. It runs only behind perform, with scoped capabilities, bounds and an authenticated recorded Function receipt. Its outputs remain proposals; action acceptance and publication retain Core 04/08 authority. This declaration does not supply a host implementation or native sandbox/resource supervisor.

**PROPERTY-RULE-PIN-1:** Decimal/Record observation failures reference `norn.values.property_validity` plus the canonical digest of the exact retained property-domain declaration closure (including inherited, referenced-type and interface constraints), with stale/forged pins Unverified and no attribution to an unrelated conservation law.

OBS-DISPUTE-LOOKUP-1: governed subject lookups reuse a complete, recomputed
in-memory subject index only while the native engine's private SQLite change
counters remain unchanged; local writes, rollbacks and external commits invalidate
it. Hosts without a connection revision reuse the index only inside the immutable
commit-guard transaction, rebuilding for each ordinary read. This adds no admitted
SQL statement or function.
Rebuilding checks every retained conflict/NFC finding and all mapping contexts
against their retained definitions before publishing positive or negative
membership. Later mapping contexts therefore extend an existing dispute.
No caller claim or persisted membership grants authority. A cold snapshot costs
one evidence rebuild; each guarded transaction is a cold snapshot, so this profile
does not bound commit cost independently of retained inbox size. Repeated operands
use indexed membership without decoding unrelated evidence. Bounded fact/reference
reads apply their byte budget first.

Indexes built by quality, guard or candidate validation remain transaction-local
and cannot survive rollback. Native connection revisions are unavailable while
uncommitted state is visible. A moving evidence revision retries at most three
complete rebuild attempts; continued movement refuses the retryable
`core.boundary.source_evidence_changed`, while stable recomputation disagreement
continues to refuse `core.boundary.bundle_mismatch`.

Interface-typed governed link targets resolve their concrete identity roots in the
requested world before index lookup. If the identity is absent, every verified
indexed root for that key is checked conservatively; an interface name never
causes `UnknownObjectType` at the dispute fence or silently loses quarantine.
