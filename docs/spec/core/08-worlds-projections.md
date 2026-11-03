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

## Receipt-bound incarnation evidence

**SNAPSHOT-RECEIPT-1:** every new `WrittenRow` commits its complete canonical incarnation snapshot digest before the producing receipt is hashed, including tombstones and intermediate versions in one atomic batch. Historical reads verify that digest against the authenticated emitting receipt; changing both the snapshot and its co-located hash cannot replace it. Legacy/unbound entries refuse as unproven; no migration derives a trusted commitment from their current mutable contents. Only a private writer can inspect its own not-yet-receipted incarnation inside the atomic commit; it cannot export it as evidence. Named scenarios cover joint snapshot/hash tampering, unbound commitments, receipt substitution, restart and repeated writes/tombstones.

The cell is the trusted committer: receipt rewriting by a party with cell-store write access is out of scope until external receipt anchoring.

Store format `norn.cell.store/8` requires these receipt commitments. Pre-change `/7` stores, like formats `/2` through `/6`, must be recreated; opening them refuses before history verification. Never derive commitments from mutable legacy rows.

### READ-LOCAL-1 — local read evidence

A `norn.cell.read/v1` envelope carries one `(cell, world, revision)` frontier,
completeness, exact-versus-stale semantics, definition generation/model/admission
pins, and the existing CommitBinding policy/resource/authorization epoch pins.
It never claims a global snapshot. Stale evidence retains its own local frontier.
Token reuse compares the entire trusted context; exact requests refuse stale or
partial evidence. Tokens describe authorized results and grant no authority.

The native point-read envelope captures its value, durable cell identity,
revision and definition pins in one store transaction. The server supplies its
current authorized CommitBinding; resource mismatch refuses. This native profile
supports main only. Internal `load` stays local, and legacy unqualified stores
cannot issue portable tokens. No multi-cell vector, migration, or new authority
model is introduced.

Named scenarios: **READ-CONTEXT** refuses reuse across cell, world, revision,
definition and authority contexts; **READ-EXACT** refuses stale/partial evidence;
**READ-SNAPSHOT** binds the existing native point-read result to its local cut.
