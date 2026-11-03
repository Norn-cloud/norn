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

Forks pin a base and run the same evaluator, schemas and laws with virtual clock and fixture outcomes. They cannot perform, use live credentials or write to main outputs. Promotion submits proposed commands again against current main authority/versions; never copy fork rows into main. Main/historical/fork caches and subscriptions cannot share identity accidentally.

Subscriptions send a complete authorized snapshot followed by ordered deltas; resumed tokens bind query, world and authority generation. A missing retained delta causes an explicit reset + fresh snapshot, not silent gaps. Revocation invalidates result caches and subscription access before subsequent delivery. Exact queries cannot substitute asynchronous data; analytics displays frontier/lag/coverage.

Statistical analysis is a recorded Function result over point-in-time inputs. Publication validates feature availability at prediction time, chronological holdout boundaries, completeness/quality/freshness, declared estimator and required evaluation evidence. Failed guards retain the result as unpublished evidence. A regression fit is sampled/model evidence, never a business-law proof or permission to act.

| Invariant / layer | Stable target diagnostic |
|---|---|
| WORLD-ISOLATE-1: forks never perform; history never mutates; dispatch/commit | `core.world.perform_forbidden` |
| ASOF-KNOWLEDGE-1: no future knowledge leaks into old cutoff; evaluation | `core.world.temporal_leakage` |
| RESTATE-HISTORY-1: append new interpretation, retain old answers; admission/publication | `core.world.invalid_restatement` |
| LINEAGE-SUM-1: number equals exact declared contributions; publication | `core.projection.lineage_incomplete` |
| SUB-REVISION-1: no silent gaps or revoked deliveries; host | `core.projection.reset_required` |
| ANALYSIS-PUBLISH-1: temporal/quality/evidence guards precede publication; commit | `core.projection.publication_guard` |

Evidence: runtime isolation/access enforcement; sampled replay/subscription/three-engine correspondence; bounded query tests; explicitly qualified statistical results. Locale renders labels/refusals/law slots without changing IDs, arithmetic, time or evidence. EN/AR use one semantic result and RTL metadata.

## Named acceptance scenarios

- **WORLD-FORK:** Given main payment capability, when a fork requests perform, then refuse with no outbox dispatch; fixture receipt evaluation remains possible and labelled simulated.
- **ASOF-RESTATE:** Given a correction learned on Nov 3 about Nov 1, when querying Nov 1 known Nov 2 then known Nov 4, then retain the original answer and expose the corrected answer respectively; exact values in [beat 6](../acceptance/data-engineering.md).
- **LINEAGE-REGION:** Given inventory value 80 in North, when drilling down, then contributions 20+60 resolve to holding IDs, ERP receipt and source cursor; unauthorized source content stays concealed.
- **SUB-RESET:** Given a resumed token outside retention or from an old grant generation, when reconnecting, then reset or deny; never silently omit deltas.
- **ANALYSIS-LEAK:** Given a feature recorded after prediction cutoff, when publishing regression output, then refuse publication while retaining its receipt.
- **OUTPUT-ATOMIC:** Given a failed backfill, when reading the Iceberg/view generation, then prior complete generation remains visible with freshness label; no half-built table appears.

## Main and gaps

Present: `crates/norn-kernel/src/observations.rs`, `crates/norn-cell/src/{history,reactive,reactive_read,migration}.rs`, `crates/norn-ir/src/{view,metric,widget,definition_catalog}.rs`. Metric IR/checking is not a statistical runtime. `norn-metrics` is introduced by open #69, not present at the inspected main. #71 adds historical cursor proof; #97/#103 are WellDB prerequisites with limited evidence. Complete bitemporal queries, IVM/OLAP, publication/lineage/restatement and main-to-fork promotion are target gaps.

## Work breakdown (each PR ≤400 lines; beat 6 demo priority)

1. **Demo priority:** bitemporal question/result envelope and history-at-cursor binding (#71), then ASOF-RESTATE fixtures.
2. **Demo priority:** contribution lineage projection and source-document drill-down; then authorized LINEAGE-REGION tests.
3. **Demo priority:** staged backfill/restatement generation and atomic output switch; Iceberg/export/view adapters in separate cuts, then OUTPUT-ATOMIC.
4. **Demo priority:** #69 point-in-time analytical inputs, recorded regression/forecast receipt and publication guards in separate cuts; ANALYSIS-LEAK plus exact beat-6 regression fixture.
5. Add world/base pin and scenario clock/outcome fixtures; WORLD-FORK against the real evaluator.
6. Complete subscription cursor/authority reset, then IVM/full-recompute equivalence with 03, then EN/AR explanations over the same result IDs.

ASOF-KNOWLEDGE-1 incarnation selection reads authenticated immutable version evidence under an aggregate row/byte budget. It selects the highest eligible version at an inclusive knowledge cutoff and immutable source commit frontier (including equal-clock corrections), preserves tombstones and source receipts, and returns explicit exhaustion without a partial answer. Effective-time interpretation, query projection, authorization and restatement publication remain the declared question/host boundaries; this lookup alone is not DATA-ASOF acceptance.

ASOF-KNOWLEDGE-1 discovers bounded retained root identities independently of the current type hierarchy, authenticates each root’s newest eligible version before checking stored lineage, and chooses the latest matching emitting commit across roots. Removal/reparenting and unrelated roots sharing a key cannot hide retained evidence.

ASOF-KNOWLEDGE-1 historical revisions derive event sequence, ledger hash and commit count from the authenticated frontier receipt, including event-free commits. Receipt bytes/rows are admitted before decode; a frontier recorded after the requested knowledge clock refuses temporal leakage. An empty frontier is the real genesis prefix.

ASOF-KNOWLEDGE-1 native historical questions retain the existing admitted exact query algebra, explicit effective window, authenticated knowledge clock/frontier, historical World and exact member versions. Visibility uses current host authorization. The bounded profile requires unchanged retained model/definition interpretation and evaluates existing live/delayed freshness at the selected source clock; AsOf requires an authenticated cutoff whose recorded clock exactly matches its declared instant, including fractional precision. Missing cutoffs refuse unsupported execution; mismatched instants refuse stale input. AsOf preserves coverage checks but has no live heartbeat-age limit. This read-only query does not authorize a restatement or publish a projection generation.

ASOF-KNOWLEDGE-1 reader-bound historical evidence caches admit and authenticate each immutable receipt once per read; all facts still verify their snapshot commitment and exact emitting write. The cache cannot be deserialized or rebound to another reader. Shared receipt bytes are charged when decoded, rather than charging a nonexistent repeated decode for each fact in the same commit.

RESTATE-HISTORY-1 native private-cache maintenance consumes the next authenticated source receipt, retracts/replaces its changed canonical members through Core 03 arithmetic, and compares with a rebuild at the same T/K slice. Backward/skipped points and source/authorization refusals retain the prior complete generation. This is query maintenance; governed restatement activation, durable output switching and posted-book reconciliation remain separate authorities.
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

## Admitted question locale views

**QUESTION-LOCALE-1:** render an already admitted plan through #496's shared complete reviewed locale map, with explicit direction, no fallback and isolated semantic expressions. Every locale retains the original QuestionIR, its canonical digest, stable semantic slot IDs, numerator/denominator quantities and units, population, filters, dimensions, effective-time window and declared knowledge/freshness policy. A separate display binding digest pins metric definitions and numeric contracts. This view neither executes nor authenticates a cutoff; historical results retain their authenticated frontier/receipt-clock provenance through the host seam. AsOf stays AsOf. Named scenarios: generated `question_locale_identity` and `question_locale_identity_refuses_unknown_or_incomplete_wording_and_retains_denominator`. Ambiguity still refuses before plan admission; only Diag's shared typed hook may remove those refusals.
