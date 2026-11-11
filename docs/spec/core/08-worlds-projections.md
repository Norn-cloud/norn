# Core 08: Execution worlds and projections

Purpose [Owner]: use the real evaluator for live/historical/fork scenarios and data engineering; expose bitemporal questions, freshness, lineage and guarded analytical outputs. [Common conventions](README.md) apply.

## IR contract

Reuse `World::{Main,Fork,Historical}`, `WorldId`, `CommitId`, `LogicalTime`, `Entropy`, existing reactive query/read contracts, `ViewIrPayload`, `WidgetRoot`, metric and definition catalog roots. Extend their projections rather than add a separate simulation engine.

| Target record | Required fields |
|---|---|
| World context | world ID/kind, scope, immutable base revision, definition/parameter pins, effective clock + recorded clock, recorded entropy, external-result fixtures; historical is read-only. |
| Question | 03 query pin, `effective_as_of`, `known_as_of`, scope/authority, engine, freshness policy, source frontier and result schema; “as of” always identifies both time axes. |
| Backfill/restatement | ID, source interval/frontier, old/new mapping/definition pins, affected questions/periods, reason/approval, dry-run diff, replacement projection generation; original observations/posted events remain immutable. |
| Lineage | result cell/measure ID → contribution IDs + signed exact amounts → object/event IDs and allocation edges → source document refs/digests; definition/transform pins, grain, source cursor and uncertainty at each edge. |
| Subscription | query/scope/world/authority generation, base snapshot revision, resume token, delta sequence, cursor vector, completeness/freshness; permission changes invalidate prior projection. |
| Output | `kind: iceberg/live_subscription/export/view`, query/result pins, snapshot/generation, schema, provenance/lineage manifest, authorization/redaction policy, freshness and retention. |
| Analysis publication | input question/result pin, effective/knowledge cutoffs, chronological split, Function/model receipt, estimator/seed/options, output metric/error/interval fields, validation evidence, approval/policy, published revision. |

## Semantics

Historical queries select records valid at `effective_as_of` and known by `known_as_of`, under the interpretation pins retained then or an explicitly named restated projection. A later correction cannot leak into an earlier knowledge cutoff. Backfill captures missing history; restatement appends interpretation/correction evidence and publishes a new generation. Neither silently rewrites old answers or posted books.

One declared question generates cell/IVM/OLAP pipelines and output projections. Iceberg publication atomically switches the visible snapshot pointer only after complete generation validation; failed builds leave the prior generation visible and labelled stale. Exports retain query/time/frontier/lineage identity. A number is not fully explained until contributions reconcile exactly to its total and resolve to authorized source evidence; redacted lineage reports restricted detail, not a fictitious complete path.

Forks pin a base and run the same evaluator, schemas and laws with virtual clock and fixture outcomes. They cannot perform, use live credentials or write to main outputs. Promotion submits proposed commands again against current main authority/versions; never copy fork rows into main. Main/historical/fork caches and subscriptions cannot share identity accidentally. [Fork lifecycle and isolation](#fork-lifecycle-and-isolation) states the storage, lifecycle and merge contract.

Subscriptions send a complete authorized snapshot followed by ordered deltas; resumed tokens bind query, world and authority generation. A missing retained delta causes an explicit reset + fresh snapshot, not silent gaps. Revocation invalidates result caches and subscription access before subsequent delivery. Exact queries cannot substitute asynchronous data; analytics displays frontier/lag/coverage.

Statistical analysis is a recorded Function result over point-in-time inputs. Publication validates feature availability at prediction time, chronological holdout boundaries, completeness/quality/freshness, declared estimator and required evaluation evidence. Failed guards retain the result as unpublished evidence. A regression fit is sampled/model evidence, never a business-law proof or permission to act.

| Invariant / layer | Stable target diagnostic |
|---|---|
| WORLD-ISOLATE-1: forks never perform; history never mutates; dispatch/commit | `core.world.perform_forbidden` |
| FORK-PIN-1: a fork is created only at its parent's verified head and keeps that pin and the cell identity; create/open | `core.world.fork_stale_base`, `core.world.fork_invalid_world` |
| FORK-SIMULATE-1: only a fork records simulated results; main records performed results only; dispatch | `core.world.simulation_forbidden` |
| FORK-LIFE-1: a discarded, expired or promoted fork accepts no commit or open; host | `core.world.fork_closed` |
| FORK-MERGE-1: promotion replays commands under main's current laws; commit | `core.world.merge_refused` |
| ASOF-KNOWLEDGE-1: no future knowledge leaks into old cutoff; evaluation | `core.world.temporal_leakage` |
| RESTATE-HISTORY-1: append new interpretation, retain old answers; admission/publication | `core.world.invalid_restatement` |
| LINEAGE-SUM-1: number equals exact declared contributions; publication | `core.projection.lineage_incomplete` |
| SUB-REVISION-1: no silent gaps or revoked deliveries; host | `core.projection.reset_required` |
| ANALYSIS-PUBLISH-1: temporal/quality/evidence guards precede publication; commit | `core.projection.publication_guard` |

Evidence: runtime isolation/access enforcement; sampled replay/subscription/three-engine correspondence; bounded query tests; explicitly qualified statistical results. Locale renders labels/refusals/law slots without changing IDs, arithmetic, time or evidence. EN/AR use one semantic result and RTL metadata.

## Named acceptance scenarios

- **WORLD-FORK:** Given main payment capability, when a fork requests perform, then refuse with no outbox dispatch; fixture receipt evaluation remains possible and labelled simulated.
- **FORK-STALE-BASE:** Given main at commit 7, when a fork is requested at commit 6, then refuse `core.world.fork_stale_base` and create nothing.
- **FORK-NONINTERFERE:** Given any interleaving of main commits and performs with fork creation, fork commits, simulated results, refused performs and discards, when the fork operations are deleted from the trace, then main's ledger, state, outbox and execution count are unchanged.
- **ASOF-RESTATE:** Given a correction learned on Nov 3 about Nov 1, when querying Nov 1 known Nov 2 then known Nov 4, then retain the original answer and expose the corrected answer respectively; exact values in [beat 6](../acceptance/data-engineering.md).
- **LINEAGE-REGION:** Given inventory value 80 in North, when drilling down, then contributions 20+60 resolve to holding IDs, ERP receipt and source cursor; unauthorized source content stays concealed.
- **SUB-RESET:** Given a resumed token outside retention or from an old grant generation, when reconnecting, then reset or deny; never silently omit deltas.
- **ANALYSIS-LEAK:** Given a feature recorded after prediction cutoff, when publishing regression output, then refuse publication while retaining its receipt.
- **OUTPUT-ATOMIC:** Given a failed backfill, when reading the Iceberg/view generation, then prior complete generation remains visible with freshness label; no half-built table appears.

## Main and gaps

Present: `crates/norn-kernel/src/observations.rs`, `crates/norn-cell/src/{history,reactive,reactive_read,migration}.rs`, `crates/norn-ir/src/{view,metric,widget,definition_catalog}.rs`. Metric IR/checking is not a statistical runtime. `norn-metrics` is introduced by open #69, not present at the inspected main. #71 adds historical cursor proof; #97/#103 are WellDB prerequisites with limited evidence. Complete bitemporal queries, IVM/OLAP, publication/lineage/restatement and main-to-fork promotion are target gaps.

## Work breakdown (one coherent unit per PR; beat 6 demo priority)

1. **Demo priority:** bitemporal question/result envelope and history-at-cursor binding (#71), then ASOF-RESTATE fixtures.
2. **Demo priority:** contribution lineage projection and source-document drill-down; then authorized LINEAGE-REGION tests.
3. **Demo priority:** staged backfill/restatement generation and atomic output switch; Iceberg/export/view adapters in separate cuts, then OUTPUT-ATOMIC.
4. **Demo priority:** #69 point-in-time analytical inputs, recorded regression/forecast receipt and publication guards in separate cuts; ANALYSIS-LEAK plus exact beat-6 regression fixture.
5. Add world/base pin and scenario clock/outcome fixtures; WORLD-FORK against the real evaluator. Fork storage, lifecycle and merge follow the unit order in [Fork lifecycle and isolation](#fork-lifecycle-and-isolation).
6. Complete subscription cursor/authority reset, then IVM/full-recompute equivalence with 03, then EN/AR explanations over the same result IDs.

ASOF-KNOWLEDGE-1 incarnation selection reads authenticated immutable version evidence under an aggregate row/byte budget. It selects the highest eligible version at an inclusive knowledge cutoff and immutable source commit frontier (including equal-clock corrections), preserves tombstones and source receipts, and returns explicit exhaustion without a partial answer. Effective-time interpretation, query projection, authorization and restatement publication remain the declared question/host boundaries; this lookup alone is not DATA-ASOF acceptance.

ASOF-KNOWLEDGE-1 discovers bounded retained root identities independently of the current type hierarchy, authenticates each root’s newest eligible version before checking stored lineage, and chooses the latest matching emitting commit across roots. Removal/reparenting and unrelated roots sharing a key cannot hide retained evidence.

ASOF-KNOWLEDGE-1 historical revisions derive event sequence, ledger hash and commit count from the authenticated frontier receipt, including event-free commits. Receipt bytes/rows are admitted before decode; a frontier recorded after the requested knowledge clock refuses temporal leakage. An empty frontier is the real genesis prefix.

ASOF-KNOWLEDGE-1 native historical questions retain the existing admitted exact query algebra, explicit effective window, authenticated knowledge clock/frontier, historical World and exact member versions. Visibility uses current host authorization. The bounded profile requires unchanged retained model/definition interpretation and evaluates existing live/delayed freshness at the selected source clock; AsOf requires an authenticated cutoff whose recorded clock exactly matches its declared instant, including fractional precision. Missing cutoffs refuse unsupported execution; mismatched instants refuse stale input. AsOf preserves coverage checks but has no live heartbeat-age limit. This read-only query does not authorize a restatement or publish a projection generation.

ASOF-KNOWLEDGE-1 reader-bound historical evidence caches admit and authenticate each immutable receipt once per read; all facts still verify their snapshot commitment and exact emitting write. The cache cannot be deserialized or rebound to another reader. Shared receipt bytes are charged when decoded, rather than charging a nonexistent repeated decode for each fact in the same commit.

RESTATE-HISTORY-1 native private-cache maintenance consumes the next authenticated source receipt, retracts/replaces its changed canonical members through Core 03 arithmetic, and compares with a rebuild at the same T/K slice. Backward/skipped points and source/authorization refusals retain the prior complete generation. This is query maintenance; governed restatement activation, durable output switching and posted-book reconciliation remain separate authorities.
## Fork lifecycle and isolation

[Owner] Forking is cheap and never blocks main. Many agents fork one cell at once; creating a fork costs metadata, a fork stores only what it changes, main's commits never wait on a fork, and forks never wait on each other. Only the commit step of a merge into one cell is serialized. Proof obligations are tracked under the assurance epic (#718); WORLD-FORK is #624.

### Fork record

| Target record | Required fields |
|---|---|
| Fork pin | `world: WorldId` (never `main`, never the parent's own world), `parent: WorldId` (`main` or another fork), `base: {commits, ledger_seq, ledger_hash}` equal to the parent's verified `LedgerHead` at creation, the parent's cell identity and bindings unchanged, definition namespace root?, `expires_at?` |
| Fork state | `open`, `discarded`, `expired` or `promoted`; only `open` accepts open, read and commit |

**FORK-PIN-1.** A fork is a separate store instance, never rows in its parent's database: rows in the parent's file would queue every fork write behind the parent's single writer. A fork is created only at the parent's current verified head; a caller pin naming any other position refuses `core.world.fork_stale_base` and creates nothing. Forking from an older position needs retained roots and is added with the copy-on-write store. `main`, an empty id or the parent's own world refuses `core.world.fork_invalid_world`. The fork inherits the parent's cell identity and bindings (#502) byte for byte: the identity never forks, and a fork is served only under the scope its parent was bound to. The fork's world is the pin, not a new identity. The pin is durable in the fork instance and survives reopen; nothing can turn a fork back into main. A fork of a fork records the fork as its parent.

Inside a fork instance, the row `world` label and `ReceiptBody.world` name the instance's local timeline, which keeps the label it inherited. Relabelling inherited rows would copy them, so the pin carries the fork's world. Fork receipts are meaningful only together with that pin, and promotion replays commands, never receipts or rows.

### Lifecycle

- **Create** copies the parent's root: O(1) metadata plus, when durable, one group-committed record. The reference engine copies the whole database; it is the oracle every copy-on-write engine must match, not a production path.
- **Open** reopens an `open` fork under its pin and the inherited identity.
- **Commit** runs the same evaluator, schemas, laws and authority as main, against the fork's own state. It never reads or writes the parent after creation.
- **Discard** closes the fork. Its unique pages become unreachable and are reclaimed by garbage collection.
- **TTL.** A fork with `expires_at` becomes `expired` when the lifecycle service observes the deadline; expiry is a discard. Open, read and commit on a closed fork refuse `core.world.fork_closed`.
- **Promote** is merge by replay (below), then the fork closes as `promoted`. Promotion never copies fork rows, pages or receipts into main.

### World-scoped effects

**WORLD-ISOLATE-1.** A non-main world cannot perform real external effects. Perform in a fork refuses `core.world.perform_forbidden` before any durable claim, so nothing is dispatched and no claim is recorded. A fork gets recorded, simulated or sandbox bindings, never live credentials. A simulated result is recorded against the fork's pending command and delivered through the same consume path; the fork pin labels it simulated. Main never records a simulated result (`core.world.simulation_forbidden`). A fork never appends to its parent's export journal or object-store prefix and never writes main's outputs, subscriptions or caches.

### Addressing and dedupe

Pages and blobs dedupe within one tenant only, across all of that tenant's cells, forks and worlds. A tenant's storage address is keyed, `HMAC(tenant_key, content)`, so addresses are never comparable across tenants: storage accounting, latency and upload responses cannot reveal whether another tenant holds the same content. Per-tenant encryption at rest fits on top. Public pack and definition content keeps plain content hashes and dedupes globally. A private definition's Core 01 identity digest remains its identity in the tenant's own records; only its storage address is keyed. Definitions, ontology changesets, connectors, transforms, UI documents and policies are stored once by digest; a world pins one namespace root, the digest of a name→digest map over all of them, so forking definitions copies that root.

### Merge contract

**FORK-MERGE-1.** Facts merge by replay, never by copying state. Each command the fork admitted after its base is replayed against main's current state under main's current authority, definition versions and laws. A command whose read versions are unchanged, or which re-evaluates to the same outcome, commits as an ordinary main action. Any other command refuses `core.world.merge_refused` with a typed reason or becomes a decision card; nothing is silently overwritten. Real performs happen only under main's bindings, after the replayed command is admitted. Definition namespaces merge three-way against the fork's base root: unchanged, one-sided or identical changes merge, divergent changes become conflicts, and the merged root passes admission again. This states the contract only; replay, recheck and decision cards are a later unit.

### FORK-ISOLATE

Let H be any main history, F a fork created at main position n, and t any trace interleaving main operations with F's creation, commits, simulated results, perform attempts, reads, discard, expiry and garbage collection, with no `Merge(F)`. Let t∖F be t with F's operations deleted.

1. **Non-interference:** `π_main(run(H·t)) = π_main(run(H·(t∖F)))` for main's ledger, state, outbox and external-effect log.
2. **No escape:** no external effect is dispatched from a world other than main (`core.world.perform_forbidden`).
3. **Merge:** a trace containing `Merge(F)` changes main exactly as sequentially committing the replayed commands would, each admitted under main's authority and laws at its commit.

Storage lemmas for every fork engine:

- **L1** a write never changes a stored page under its address;
- **L2** a fork's root is a snapshot: later parent writes do not change it, and fork writes change no root they did not open;
- **L3** garbage collection never frees a page reachable from a live root;
- **L4** a fork read returns exactly what reading a full copy would (property tests against the copying oracle).

Evidence target: L1–L4 as properties against the copying reference engine and model checks of the page store; non-interference and no-escape as generated trace properties and deterministic simulation on forks; a Lean world-indexed non-interference meta-theorem and a Quint model of create, commit, merge, discard and garbage collection replayed through ITF. Until those exist, FORK-ISOLATE is a target, and each passing property is sampled evidence only.

### Unit order

1. This contract.
2. The `CellStore` fork contract with the copying reference engine, FORK-PIN-1, WORLD-ISOLATE-1 refusals, WORLD-FORK and FORK-NONINTERFERE properties.
3. A copy-on-write page store behind `SqlEngine`: keyed addresses, a Merkle page map, O(1) fork, reachability garbage collection, L1–L4 against the oracle.
4. A durable local tier with group commit and crash recovery.
5. Definition namespace roots and three-way namespace merge with re-admission.
6. Merge by replay with version recheck and decision cards.
7. Durable lineage in the object-store journal: a fork's chain starts from its parent's chain at the base.
8. The lifecycle service: TTL, promote, discard, garbage-collection scheduling, quotas, scale to zero.
9. Diff and why between worlds.
10. The FORK-ISOLATE proofs.

## Receipt-bound incarnation evidence

**SNAPSHOT-RECEIPT-1:** every new `WrittenRow` commits its complete canonical incarnation snapshot digest before the producing receipt is hashed, including tombstones and intermediate versions in one atomic batch. Historical reads verify that digest against the authenticated emitting receipt; changing both the snapshot and its co-located hash cannot replace it. Legacy/unbound entries refuse as unproven; no migration derives a trusted commitment from their current mutable contents. Only a private writer can inspect its own not-yet-receipted incarnation inside the atomic commit; it cannot export it as evidence. Named scenarios cover joint snapshot/hash tampering, unbound commitments, receipt substitution, restart and repeated writes/tombstones.

The cell is the trusted committer: receipt rewriting by a party with cell-store write access is out of scope until external receipt anchoring.

Store format `norn.cell.store/8` requires these receipt commitments. Pre-change `/7` stores, like formats `/2` through `/6`, must be recreated; opening them refuses before history verification. Never derive commitments from mutable legacy rows.

## Native restatement activation

**RESTATE-HISTORY-1 / OUTPUT-ATOMIC:** `core.analytics.Restatement` retains the
previous complete bundle while its host rebuilds every declared question slice.
The host stages a content-addressed candidate with reason, old/new question and
mapping pins, effective windows, knowledge frontiers and exact retraction and
replacement contributions. Missing, duplicate, incomplete or unauthorized slices
refuse with `core.world.invalid_restatement`; same-interpretation deltas must
match a full exact recompute before staging. Changed interpretations are explicit
complete rebuilds over retained authenticated inputs; unsupported historical
model changes still refuse.

The declared `approve` and `activate` actions use the existing kernel authorizer,
lifecycle checks and commit read guards. Approval is not a host boolean. Activation
changes a single receipt-bound slot incarnation selecting the entire bundle.
Readers authenticate that incarnation and return either all old outputs or all
new outputs. A building/ready/approved slot exposes the old complete generation
labelled stale, including after interruption and restart. Prior active versions
remain reproducible under current population authorization. The view label is
`restated`; no posted events or books are rewritten.

Named runtime scenarios cover the scrap-yield K1/K2 correction, exact loss and
ratio deltas, multi-slice selection, denied/premature approval and activation,
interrupted staging, stale tokens, current authorization and reproducible old
answers. Generated change sequences include source redelivery and restart.

RESTATE-HISTORY-1 replay retains at least the prior selected knowledge clock
while advancing each authenticated receipt frontier. A query clock ahead of
commit clocks cannot move backward during maintenance; retaining that already
selected ceiling never admits a version beyond the pinned frontier. Named and
generated clock-lead scenarios include a later excluded version below the clock
ceiling, an injected preparation refusal, reopen/resume and exact correspondence.
Preparation itself writes no slot or candidate. Its refusal leaves the ledger
and old complete answer unchanged; the separately committed building slot stays
stale and resumable across restart, as required for interrupted rebuilds.
Private slot writes choose a time no earlier than the observed cell commit-clock
floor, preserving the candidate's independently pinned knowledge coordinates.
Earlier source commits do not invalidate the captured staging time; transactional
version and clock guards still refuse actual concurrent conflicts.

RESTATE-HISTORY-1 opaque rebuild/candidate capabilities are bound to their originating cell-store instance. Foreign-host preparation/staging refuses before input reads or writes, even when world, scope and slot version coincide. Restart obtains a new capability through authenticated resume and recomputation.

## Authorized numeric source lineage

LINEAGE-SUM-1 / OBS-EVIDENCE-1: an exact numeric trace authenticates each fact
incarnation through receipt-bound historical evidence, then resolves its source
edge only through the cell's recomputed input-bundle API. Numeric amounts and
membership do not grant canonical-value or source disclosure. Independent host
grants control these two views within the same transaction and decode budget.
A source edge is explicitly `resolved`, `restricted`, or `unavailable` (no retained
bundle). Resolved evidence carries the semantic source row and original document
references, document and bundle digests, exact mapping pin and field provenance.
Source identity is never inferred from a canonical/CSV key. Catalog metadata
cannot create another numeric contribution, and document-content access is a
separate grant. Corrupt or exhausted evidence refuses the entire trace; a
restricted source never becomes an invented complete explanation. The numeric
result retains its original definition, time and cursor frontier.

DATA-LINEAGE scenarios use the unchanged scrap-yield CSV through a typed source
mapping, all four combinations of value/source grants, non-bundle facts,
redelivery and restart, source-budget exhaustion and corrupt source journal
refusal. Generated disclosure cases preserve exact sums and member identities.

## Admitted question locale views

**QUESTION-LOCALE-1:** render an already admitted plan through #496's shared complete reviewed locale map, with explicit direction, no fallback and isolated semantic expressions. Every locale retains the original QuestionIR, its canonical digest, stable semantic slot IDs, numerator/denominator quantities and units, population, filters, dimensions, effective-time window and declared knowledge/freshness policy. A separate display binding digest pins metric definitions and numeric contracts. This view neither executes nor authenticates a cutoff; historical results retain their authenticated frontier/receipt-clock provenance through the host seam. AsOf stays AsOf. Named scenarios: generated `question_locale_identity` and `question_locale_identity_refuses_unknown_or_incomplete_wording_and_retains_denominator`. Ambiguity still refuses before plan admission; only Diag's shared typed hook may remove those refusals.
