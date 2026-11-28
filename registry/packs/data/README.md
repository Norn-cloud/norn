# `data` Constitutional Ontology Pack

Opt-in typed ontology pack (`registry/packs/data/`) defining constitutional
data-catalog, provenance, lineage, observation, and data-quality contracts for
Norn, grounded in W3C and ISO standards.

All canonical IR artifacts under `registry/packs/data/artifacts/` are compiled
from [`data.ts`](./data.ts) via `tools/norn-ts` (`NORN_PACKS_UPDATE=1 bun test test/registry-packs.test.ts`)
and admitted by `norn-checker` over `registry/core/artifacts/`.

## W3C & ISO Standards Mapping

| Norn Concept | Kind | Role (`GRAPH-ROLE-1` / BFO) | W3C Standard Mapping | ISO / Open Standards Mapping | Key Invariants & Laws |
| --- | --- | --- | --- | --- | --- |
| `data.Dataset` | Interface | `thing` (Continuant) | W3C DCAT v3 `dcat:Dataset`, `dcterms:publisher` | ISO/IEC 11179 (metadata registry), ISO/IEC 21838-2 (BFO Continuant) | `positive_freshness_sla` (`freshness_sla_ms == null \|\| > 0`), `non_empty_schema_uri` |
| `data.DatasetSnapshot` | Interface | `thing` (Continuant) | W3C DCAT v3 `dcat:DatasetSeries` member (`dcat:inSeries`), W3C PROV-O `prov:Entity`, `prov:wasRevisionOf`, `prov:wasGeneratedBy` | ISO 8601 (UTC bitemporal `effective_at`, `known_at`), ISO/IEC 21838-2 | `non_negative_counts` (`record_count >= 0`, `byte_size >= 0`), `non_empty_digests`, `supersedes` (`acyclic: true` on implementations) |
| `data.Activity` | Interface (`extends core.TimeInterval`) | `happening` (Occurrent) | W3C PROV-O `prov:Activity`, `prov:wasAssociatedWith`; W3C OWL-Time `time:ProperInterval` | ISO 8601 (`[starts_at, ends_at)` UTC), ISO/IEC 21838-2 (BFO Occurrent) | Inherited `core.TimeInterval.proper_interval` (`starts_at < ends_at`), `non_empty_code_digest` |
| `data.Derivation` | Interface | `happening` (Occurrent) | W3C PROV-O `prov:Derivation` (`input_snapshot` = `prov:entity` / `prov:used`, `output_snapshot` = subject of `prov:qualifiedDerivation` / `prov:wasDerivedFrom`, `activity` = `prov:hadActivity`) | OpenLineage run/dataset lineage, ISO/IEC 21838-2 | `DATA-CAUSAL-1` (`LAW-LINK-1`): `input_known_before_activity_end`, `output_known_after_activity_start`, `monotonic_knowledge_time`, `distinct_snapshots` |
| `data.Observation` | Interface | `happening` (Occurrent) | W3C SOSA/SSN `sosa:Observation`, `sosa:phenomenonTime` (`occurred_at`), `sosa:resultTime` (`observed_at`), `sosa:madeBySensor` | CloudEvents, ISO 8601, ISO/IEC 21838-2 (`BFO-DISJOINT-1`, `TODO(PR #1781)` `disjointWith: [EconomicEvent, Commitment, Claim]`) | `non_empty_digest`, `observed_at_or_after_occurred` (`occurred_at <= observed_at`, allowing late arrivals) |
| `data.QualityFinding` | Interface | `happening` (recorded assessment act) | W3C DQV `dqv:QualityMeasurement`, `dqv:computedOn` (modeled in Norn as a recorded assessment/resolution act rather than a pure BFO information continuant) | ISO/IEC 25012 (5 inherent dimensions: `accuracy`, `completeness`, `consistency`, `credibility`, `currentness`), ISO 8000 | `non_empty_offending_path` (interface law) |

## Concrete Reference Object Types

Every interface in `registry/packs/data/data.ts` has a concrete reference object
type in the same pack so `norn-checker` exercises every interface contract,
`LAW-LINK-1` causality law, `acyclic: true` link constraint, and `LIFE-SOUND-1..8`
lifecycle end-to-end:

- `data.Steward` (`implements core.Agent`, `role: "thing"`)
- `data.CatalogDataset` (`implements data.Dataset`, `role: "thing"`)
- `data.SnapshotRecord` (`implements data.DatasetSnapshot`, `role: "thing"`, `supersedes` link declares `acyclic: true`)
- `data.PipelineRun` (`implements data.Activity`, `role: "happening"`, inherits `core.TimeInterval.proper_interval`)
- `data.DerivationLink` (`implements data.Derivation`, `role: "happening"`, inherits all `DATA-CAUSAL-1` laws via `LAW-INTERFACE-1`)
- `data.TelemetryEvent` (`implements data.Observation`, `role: "happening"`)
- `data.QualityCase` (`implements data.QualityFinding`, `role: "happening"`, `LIFE-SOUND-1..8` resolution lifecycle: `open -> acknowledged | quarantined | resolved`, `acknowledged -> quarantined | resolved`, `quarantined -> resolved`, terminal immutable `resolved` with outcome `completed`)

## Evidence Summary

| Claim | Evidence Class | Source |
| --- | --- | --- |
| Canonical IR compilation & byte-for-byte artifact match | `structural` | `tools/norn-ts/test/data-pack.test.ts`, `tools/norn-ts/test/registry-packs.test.ts` |
| Full `norn-checker` admission over `registry/core/artifacts/` (`0` errors) | `structural` | `crates/norn-checker/tests/data_ontology.rs`, `tools/norn-ts/test/data-pack.test.ts` |
| `data.QualityCase` option-to-complete (`LIFE-SOUND-1..8` + `PROOF-LIFECYCLE` bounded graph) | `structural` + `exhaustive-finite` (pinned bounds) | `crates/norn-checker/tests/data_ontology.rs` |
| `data.SnapshotRecord.supersedes` `acyclic: true` declaration & subtype-override preservation (runtime cycle rejection lives in `norn-kernel::commit::edges::check_acyclic_links`) | `structural` | `crates/norn-checker/tests/data_ontology.rs` |
| `DATA-CAUSAL-1` law lowering, `LAW-LINK-1` read-path validation, and `LAW-EXAMPLE-1` valuations | `structural` + `sampled` | `registry/packs/data/data.ts`, `crates/norn-checker/tests/data_ontology.rs`, `tools/norn-ts/test/data-pack.test.ts` |
| `DATA-CAUSAL-1` (monotonic knowledge time), conditional `DATA-ACYCLIC-1` (requires stricter edge progression than this pack enforces), `TIME-ALLEN-1` (mirroring `owl-time-v1`), `BFO-DISJOINT-1` (`GRAPH-ROLE-1`) & target `DECLARED-DISJOINT` (pending PR #1781) | `checked proof` (reference specification, not Rust extraction correspondence) | `verification/data-lineage-v1/DataLineage.lean` |
