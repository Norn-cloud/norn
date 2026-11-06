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

**MAPPING-PLAN-1:** a store may reuse the decoded mapping, source schema, compiled mapping, artifact pins, definitions digest and retained-model proof for one retained definition version. The reuse key is the version, the mapping id and a digest of every column of the installed definition row read on this call, so any changed column misses and repeats the full retained proof. Every evaluation, hit or miss, re-reads and verifies the export-journal record for the row's `commit_seq`, requires its installed row to equal the row read, requires the stored receipt at `commit_seq` to equal the journal's, and requires the journal model snapshot to equal the one the plan was proven on; the model-hash and governing-law checks are pure functions of that snapshot and the keyed row. A hit therefore refuses exactly what a cold store refuses, and bundles are identical to a cold derivation. Named scenario: **MAPPING-PLAN-REUSE** (warm and cold rechecks agree; changed row columns and tampered receipts or journal records refuse identically warm and cold).

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

- **THESIS-BUILTIN-EXCEPTION:** The thesis retains typed WASM components or external workers as the authorable boundary and names only engine-distributed built-ins as the host exception, without sandbox or isolation claims.
- **FUNCTION-HOST-1-BUILTINS-ONLY:** Core 07 reserves host for engine built-ins, denies authored/pack installation even with a copied built-in namespace, and retains exact pins, scoped capabilities, recorded receipts and proposal-only outputs. Runtime conformance is the separate #731 admission cut.
- **AUTHORED-HOST-REFUSED:** A fully pinned authored host manifest refuses `function.manifest.authored_host` at `implementation.format` through both shared manifest admission and its public ABI; supported WASM and external-worker manifests retain their admitted identities.
- **BUILTIN-NAMESPACE-NO-BYPASS:** Copying an engine-shaped function ID, crate name, version or digest into authored data cannot authorize host code.
- **ENGINE-BUILTIN-AVAILABLE (unsupported):** No engine-owned Host registry currently ships. Public manifest admission refuses Host under every execution profile; genuine built-in availability and exact-pin dispatch qualification are deferred to #761.
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

FUNCTION-HOST-1: engine built-ins only; never authorable. `implementation.format: host` is reserved for Norn-owned, versioned built-in Functions distributed in the engine and identified by the engine's own built-in namespace. Authored and pack definitions cannot install host Functions, including by imitating that namespace; admission refuses fail-closed with a stable diagnostic. Built-ins are reviewed and qualified like kernel code. `runtime.id` and `runtime.version` pin the implementing crate name and version; `implementation.digest` pins its source tree, with the dependency closure and runtime build/configuration digest retained separately. The locator is exactly `<crate>/<version>@<implementation digest>` and is informational, never permission to load arbitrary code. The engine must resolve its shipped built-in implementation by these exact pins or refuse.

HOST-TREE-1: `implementation.digest` of a host Function is the source-tree digest
of its implementing crate directory. The tree is every regular file below that
directory, except entries whose name starts with `.` and directories named
`target`; symlinks, non-UTF-8 names and other file kinds refuse. Each file is
named by its relative `/`-separated path, which is non-empty, relative, and has
no empty, `.` or `..` component, backslash or control character. Each file
contributes `{path, size, sha256}`, where `sha256` is the lower-case hex SHA-256 of
its exact bytes, with no line-ending or encoding normalization. The entries are
sorted by raw path bytes (UTF-8 bytes, which is code-point order), and the digest
is `sha256:` followed by the hex SHA-256 of `norn.function/source-tree/v1\0` and the
exact canonical JSON of that array: sorted keys, compact, strings kept byte for byte
with no NFC normalization, so an NFC-equivalent rename is a different tree. An empty tree
or a duplicate path refuses. `norn_ir::source_tree::source_tree_digest` is the
pure implementation, and `norn source-digest <dir>` walks a clean checkout. The
golden vector in `crates/norn-ir/tests/fixtures/source-tree.digest` was computed
by an independent reimplementation (Python `hashlib` and `json.dumps` with sorted
keys and compact separators). Any change to the algorithm needs a new domain
version. Named scenarios (`crates/norn-ir/tests/source_tree.rs`):
**HOST-TREE-PINNED**, **HOST-TREE-UNICODE** (a precomposed and a decomposed
spelling differ; a second golden vector pins raw-byte order),
**HOST-TREE-SENSITIVE** (an edit, rename, removal,
addition, CRLF or swap of bytes changes the digest), **HOST-TREE-ORDER**
(listing order is irrelevant) and **HOST-TREE-REFUSE**.

Host execution uses the existing unqualified `effectful` profile and cannot claim WASM qualification, container isolation or action caching. It runs only behind perform, with scoped capabilities, bounds and an authenticated recorded Function receipt. Its outputs remain proposals; action acceptance and publication retain Core 04/08 authority. This declaration does not supply a host implementation or native sandbox/resource supervisor.

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

## Observed canonical relation claims (#823)

- **OBS-LINK-CLAIM-1:** the canonical concept owns its relation. Each admitted
  MappingIR `maps[].links` entry declares a source-field expression, role and
  canonical target map or identity binding. Evaluation emits one immutable
  `ObservedLinkClaim` per non-null output or explicit nullable-role clear.
  Claim identity includes its retaining observation and complete provenance;
  delivery order and redelivery do not establish identity. Source aliases
  remain provenance; they never replace canonical endpoint keys. Only typed claim
  preparation seals claim metadata at its computed identity. Generic observation
  authority cannot create claim metadata or rewrite a retained claim; claim
  identities remain immutable across observation preparation and authorization merges.
  Seals compare the catalog-effective metadata, including extension defaults.
  Re-observation never backfills or changes retained metadata when defaults evolve;
  differing effective values refuse until the existing definition-migration path
  has made the retained row conform. The typed claim preimage and identity stay fixed.
- **OBS-LINK-CONVERGE-1:** equal canonical endpoints converge to one live link;
  all source claims remain individually addressable provenance. Observation
  authority is confined to recomputed declared claims. Claims carry no link
  property values, so a convergent addition must propose exactly the relation's
  catalog-effective defaults. The existing authored relation-ownership check
  remains in force for authored mutations.
- **OBS-LINK-CONTRIBUTION-1:** the latest consumed claim for a source row,
  declared canonical source/role and mapping artifact contributes current membership.
  Its capture namespace comes from the existing connector/partition/source-row
  identity, so identical row aliases from independent connectors never supersede
  each other.
  Supersession is recorded and one-way: a claim names the earlier claims of its
  contribution that it replaces in `supersedes`, part of its immutable preimage,
  so the order persists across turns and content addressing keeps it acyclic.
  Mapping evaluation leaves the record empty and the claim planner fills it from
  retained claims, ordered by source occurrence rather than arrival; redelivery
  reuses the retained claim. Retained evidence never supersedes a fresh claim. Competing claims with no recorded order are contested
  and authorize neither an addition nor a withdrawal; they still hold their edges
  until a fresh claim records their order.
  Standing and edge support are decided once over the turn's final claim set, so
  sealing a claim is only an intent and a claim merged later can still refuse it.
  Replacing its target withdraws an earlier observed edge only if no active
  claim still supports that edge. Prepared old and successor evidence is
  required for a withdrawal, and the kernel refuses it while any other prepared
  claim for the same canonical edge is not superseded. Authored edges without
  claim backing stay intact. Every superseded claim remains immutable evidence.
  The kernel enforces these rules over prepared claims only: it cannot see a
  retained claim the turn did not prepare, so an omitted successor or supporter
  leaves a stale claim current. It also accepts whatever `supersedes` a fresh
  claim carries, so it cannot tell a newer occurrence from a late one. The claim
  planner (#877) must prepare every retained claim of the edge and contribution,
  and must order supersession by source occurrence, never by arrival: an older
  source occurrence must never supersede a newer stored one, including a
  late-arriving older occurrence that is not a redelivery. Such an occurrence
  records no order over the newer claim, so the two stay contested until a later
  occurrence records both. These gaps are ship blockers for #823: no production
  caller of `seal_observed_link_claim` may land before #877.
- **OBS-LINK-PENDING-1:** a missing target leaves a retained claim pending by
  canonical identity, including targets in the same capture. Target arrival
  resolves it without source redelivery or an arrival-order assumption. Capture,
  claims, consumption and cursor advancement share the existing atomic boundary.
  A declared capture horizon closes pending claims into evidence-bearing findings.
- **OBS-LINK-CONFLICT-1:** incompatible to-one claims, cardinality or endpoint-type
  violations become source-conflict findings and quarantine their subjects under
  OBS-EVIDENCE-1. No source occurrence or contradictory claim is silently discarded.
  Native proposals violating those laws still refuse without writes.

Unit 1 declares the claim concept, pins the MappingIR output correspondence and
enforces at the kernel observation boundary: OBS-LINK-CLAIM-1 claim immutability
and typed-only claim writes; OBS-LINK-CONVERGE-1 corroboration without namespace
ownership, endpoint/type law and default-only link values; and
OBS-LINK-CONTRIBUTION-1 recorded one-way supersession with prepared-support
checks over the final claim set.
Pending resolution, complete support enumeration and conflict quarantine
require the subsequent runtime units.
Unit 2 admits claims and resolves equal endpoints through the existing observed
commit path. The consumed mapping-bundle journal retains pending claims and
recomputable multi-source provenance; a transaction guard rechecks the planned
links against that journal before admission. The planner records `supersedes`
as every earlier-consumed claim of the contribution with a lower raw
`source_version` (lexicographic, as the dataplane orders it), so a retained
claim's identity recomputes exactly from the journal and a late older occurrence
records no order over a newer one. It prepares every claim of each changed
canonical source/role group and plans only the edge changes the kernel's
final-set standing authorizes. A plan or link provenance read selects bundles
naming its commit's objects or claim endpoints, then the bundles naming those
claims' sources, so unrelated bundles spend no budget while every earlier claim
of an affected contribution is read. The selected set refuses above 1,000
bundles, 4 MiB of bundle evidence or 20,000 claims; budget exhaustion cannot
silently omit a claim. Contradiction and horizon findings require the subsequent
runtime unit.
The claim's registry `evidence` scalar holds the exact canonical JSON of its
typed preimage (`canonical_json_exact_of`: sorted object keys, verbatim strings
and number tokens) as UTF-8 text; endpoint columns permit lookup without
treating serialized claims as authority.

Named acceptance scenarios:

- **OBS-LINK-DECLARED:** ERPNext, Shopify and Odoo retain every declared role,
  target type/key and relation-rule provenance from their admitted mappings.
- **OBS-LINK-IDENTITY:** changing a source, mapping pin, observation or endpoint
  changes claim identity; repeated evaluation of the same evidence preserves it.
  A clear and an unresolved explicit target are distinct claims.
- **OBS-LINK-EXTENSION-DEFAULT:** an admitted extension default is included in
  both creation and retained-read seals. A later changed default cannot rewrite
  the existing claim through re-observation.
- **OBS-LINK-RETARGET:** a superseding source contribution replaces its old
  endpoint, retaining old evidence and preserving active corroborating claims
  and unbacked authored edges. Redelivery of an already consumed older event
  cannot roll the relation back.
- **OBS-LINK-CLEAR-UNION:** a to-many clear withdraws only its own contribution;
  another source's asserted endpoint stays present through repeated captures.
- **OBS-LINK-CONCRETE-DEFAULT:** a pending claim from an admitted mapped subtype
  resolves with its inherited effective link defaults, exactly as commit
  normalization does. Redeclared subtype defaults and native type promotion
  remain catalog/admission refusals.
- **OBS-LINK-LATE-OCCURRENCE:** an older source version captured after a newer
  one supersedes only older claims; the two stay contested, the newer edge stays,
  and a later version records the order.
- **OBS-LINK-CLOSED-SCOPE:** a target arrival reads the superseding claim of a
  contribution even when that claim names a different target.
- **OBS-LINK-CONVERGENCE:** three source claims about one delivery's `fulfils`
  relation produce one edge with three provenance entries.
- **OBS-LINK-STALE-REVERSAL:** after a replacement, the retained stale claim and
  its retained successor cannot restore the stale edge or withdraw the current one,
  whether the successor is prepared before or after the stale claim is sealed.
  A fresh claim recording the successor wins, even when it returns to the old target.
- **OBS-LINK-SUPPORTED-WITHDRAWAL:** a replacement cannot withdraw an edge that
  another connector's prepared claim still supports; once that connector's own
  replacement is recorded, a later turn can.
- **OBS-LINK-LINK-VALUES:** a convergent addition proposing non-default link
  property values refuses.
- **OBS-LINK-PENDING-ORDER:** generated valid input orders and redeliveries yield
  the same final canonical links and claim set; reopen/replay retains that state.
- **OBS-LINK-QUARANTINE:** missing/foreign targets at the declared horizon and
  contradictory to-one claims retain source facts plus findings; native illegal
  link writes refuse, and no partial capture or cursor advance is visible.
