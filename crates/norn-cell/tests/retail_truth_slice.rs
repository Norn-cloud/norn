//! Resource Planning Lab (S1 — Retail Truth Slice, #1941):
//!
//! End-to-end validation of the two-entity, multi-store retail truth slice
//! layered onto `registry/packs/supply-lab/`:
//! 1. **Two-entity structural non-commingling (D02):** two independent
//!    file-backed `SqliteCell`s (sole proprietorship in EGP + partnership in USD
//!    operating stores in the same mall with shared physical custodian), proving
//!    custody != ownership, lot owner == store owner, unique store count
//!    position (`ID-UNIQUE-1`), governed `UnitCost` binding (forged unit cost,
//!    wrong `costed_ref`, missing cost record, and future-effective `UnitCost`
//!    all refuse; `PosSaleCostedRequiresCostLink` forbids unlinked costed rows),
//!    and cross-scope refusal.
//! 2. **Messy retail feeds -> KPI -> `LINEAGE-SUM-1` source provenance:** POS
//!    sales, store physical counts, retail supplier receipts, and supplier unit
//!    costs ingested through real admitted mapping bundles on one `MapCell`
//!    with quarantine, redelivery idempotency, recovery, end-to-end KPI
//!    evaluation over `PosSaleEvidence`, `ContributionSource::Resolved` source
//!    drill-through, and feed-watermark withholding.
//! 3. **Holding lot aging, fail-closed count coverage (`on_hand` & `available`),
//!    mixed-bucket missing-cost withholding, and cell-derived commercial KPIs:**
//!    exact answers over all declared retail questions and both chasm-trap-safe
//!    retail transforms with full unit conservation (`received - sold == remaining`),
//!    4-week WOS from 4 real weekly sales buckets, cohort-complete sell-through,
//!    and explicit closing/average GMROI; missing cost, missing `on_hand` or
//!    `available` counts, stale counts, future counts, and count drift all fail
//!    closed.
//! 4. **Single-cut pinned read-only combined management view (`READ-LOCAL-1`):**
//!    cross-cell projection verifying independent `ReadContext` pins (cell,
//!    world, full `LedgerHead`, `DefinitionPin`, `AuthorityPin`, `scope`,
//!    `source_cursors`, `watermarks`), single-book-per-cell discipline,
//!    structured bucket selection bound to owning entity stores, `Freshness::Stale`
//!    (60s threshold) and pending-inbox mapping-refusal withholding, real
//!    verified-evidence promotion from `PosSaleEvidence` -> `PosSale`, and exact
//!    `DuckDB` parity across revenue, gross margin, store gross-margin-rate
//!    buckets, and POS feed revenue when built with `--features duckdb,native`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use norn_cell::combined_view::{
    CellCoverageSpec, CellViewInput, CombinedViewKind, CombinedViewReport, CoverageTransformGate,
    SpecCellBinding, evaluate_cell_cut, evaluate_cell_cut_from_spec,
    evaluate_combined_management_view_from_spec,
};
use norn_cell::compute::lineage::{
    ContributionSource, LineageDisclosure, keyed_lineage_handle, keyed_revision_handle,
};
use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::compute::{Freshness, Frontier, QueryContext, QueryResult, Watermark};
use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::source_cursor::CursorAdvanceSpec;
use norn_cell::{
    AdmittedDefinitions, CellError, CellStore, LedgerHead, MappingOutcome, Model, SqliteCell,
    StoreOptions,
};
use norn_checker::EntityArtifactSetContext;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::compute::arithmetic::{FACTOR, ratio};
use norn_checker::metric::compute::contribution::instant_key;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::cell_id::CellId;
use norn_ir::data_quality::DataQualityFindingStatus;
use norn_ir::{ArtifactEnvelope, Decimal, LawRoot, QuestionIR, RuntimeValue};
use norn_kernel::authz::CommitBinding;
use norn_kernel::cell_host::{FactKey, JsonObject};
use norn_kernel::read_envelope::{
    AuthorityPin, Completeness, DefinitionPin, LocalFrontier, ReadContext, ReadRequirement,
    ReadSemantics,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, DecisionTurn, FactEvent, LogicalTime,
    ObservationEvent, ObservationId, Observed, Provenance, commit_action,
};
use serde_json::{Value, json};
use tempfile::TempDir;

type RetailCell = SqliteCell<FactEvent, JsonObject, JsonObject>;
type MapCell = SqliteCell<ObservationEvent, norn_ir::RuntimeObject, norn_ir::RuntimeObject>;

const SOLE_SCOPE: &str = "org_family_sole_prop/prod";
const PARTNER_SCOPE: &str = "org_family_partnership/prod";
const SOLE_CELL_ID: &str = "0000000000000000000000000000a101";
const PARTNER_CELL_ID: &str = "0000000000000000000000000000b202";
const FEED_PARTITION: &str = "MALL-CFC";

/// 2026-09-07T12:00:00Z (after W1..W9 retail events).
const CLOCK_MS: u64 = 1_788_782_400_000;
/// Live feed heartbeat freshness policy matching `retail_questions.ts` (`maxAgeMs: 60000`).
const FEED_MAX_AGE_MS: u64 = 60_000;
/// Maximum allowed physical count age before count-staleness fails closed (7 days).
const MAX_COUNT_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const REQUIRED_FEEDS: [&str; 4] = [
    "supply.pos_sales",
    "supply.store_counts",
    "supply.retail_receipts",
    "supply.unit_costs",
];
const EVIDENCE_TYPES: [&str; 4] = [
    "supply.PosSaleEvidence",
    "supply.StoreCountEvidence",
    "supply.RetailReceiptEvidence",
    "supply.UnitCostEvidence",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_envelopes(dir: &Path) -> Result<Vec<ArtifactEnvelope>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect()
}

fn core_base() -> Result<Vec<ArtifactEnvelope>, String> {
    let core = repo().join("registry/core/artifacts");
    [
        "core.value_types.entity.json",
        "std.value_types.entity.json",
        "core.units.json",
        "core.time.json",
    ]
    .iter()
    .map(|name| {
        let text = fs::read_to_string(core.join(name)).map_err(|e| format!("{name}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{name}: {e}"))
    })
    .collect()
}

fn runtime_envelopes(artifacts: &[ArtifactEnvelope]) -> Result<Vec<RuntimeValue>, String> {
    artifacts
        .iter()
        .map(|a| {
            let wire = serde_json::to_value(a).map_err(|e| format!("{e}"))?;
            RuntimeValue::from_json(wire).map_err(|e| format!("{e:?}"))
        })
        .collect()
}

struct RetailPack {
    candidate: Vec<ArtifactEnvelope>,
    pool: Vec<ArtifactEnvelope>,
    definitions: AdmittedDefinitions,
    canonical_set: String,
    receipt: String,
    receipt_sha256: String,
}

fn build_retail_pack() -> Result<RetailPack, String> {
    let mut candidate = Vec::new();
    for dir in ["tracking", "ml", "supply-lab"] {
        candidate.extend(read_envelopes(
            &repo().join("registry/packs").join(dir).join("artifacts"),
        )?);
    }
    let base = core_base()?;
    let mut pool = base.clone();
    pool.extend(candidate.iter().cloned());
    let definitions =
        AdmittedDefinitions::from_envelopes(&pool).map_err(|e| format!("definitions: {e}"))?;
    let (sources, base_sources) = (runtime_envelopes(&candidate)?, runtime_envelopes(&base)?);
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &base_sources)
            .map_err(|e| format!("{e:?}"))?,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| format!("receipt: {e:?}"))?;
    if !checked.receipt.admitted {
        return Err(format!("pack not admitted: {:?}", checked.diagnostics));
    }
    let wire = checked
        .receipt
        .to_canonical_value()
        .map_err(|e| format!("{e:?}"))?;
    let receipt =
        String::from_utf8(norn_ir::canonical::serialize(&wire).map_err(|e| format!("{e:?}"))?)
            .map_err(|e| format!("{e}"))?;
    let receipt_sha256 =
        norn_checker::admission_receipt_sha256(&receipt).map_err(|e| format!("{e:?}"))?;
    let set: Vec<&ArtifactEnvelope> = candidate.iter().chain(&base).collect();
    let canonical_set = serde_json::to_string(&set).map_err(|e| format!("{e}"))?;
    Ok(RetailPack {
        candidate,
        pool,
        definitions,
        canonical_set,
        receipt,
        receipt_sha256,
    })
}

fn retail_pack() -> Result<&'static RetailPack, String> {
    static PACK: OnceLock<Result<RetailPack, String>> = OnceLock::new();
    PACK.get_or_init(build_retail_pack)
        .as_ref()
        .map_err(Clone::clone)
}

impl RetailPack {
    fn model(&self) -> &Model {
        self.definitions.model()
    }

    fn install_input(&self) -> norn_cell::InstalledDefinitionInput {
        norn_cell::InstalledDefinitionInput {
            canonical_set: self.canonical_set.clone(),
            registry: Vec::new(),
            receipt_sha256: self.receipt_sha256.clone(),
            admission_receipt: Some(self.receipt.clone()),
            readmission: None,
            diff: json!({"suite": "retail-truth-slice"}),
            installed_at: 1,
            layout: None,
        }
    }

    fn retail_question(&self, name: &str) -> Result<QuestionPlan, String> {
        let text = fs::read_to_string(
            repo().join("registry/packs/supply-lab/questions/retail.questions.json"),
        )
        .map_err(|e| format!("retail.questions.json: {e}"))?;
        let questions: Vec<QuestionIR> =
            serde_json::from_str(&text).map_err(|e| format!("decode retail questions: {e}"))?;
        let question = questions
            .into_iter()
            .find(|q| q.name == name)
            .ok_or_else(|| format!("missing retail question {name}"))?;
        let registry = BTreeSet::new();
        let space = derive_metric_space(
            &ScopedView::uniform(&self.pool, &registry),
            &LawRoot::Entity(question.object_set.as_str().to_owned()),
        )
        .map_err(|e| format!("derive metric space for {name}: {e:?}"))?;
        space
            .admit(&question, 100)
            .map_err(|e| format!("admit {name}: {e:?}"))
    }

    fn transform_plan(
        &self,
        artifact_id: &str,
    ) -> Result<norn_checker::transform::TransformPlan, String> {
        let artifact = self
            .candidate
            .iter()
            .find(|a| a.artifact_id.as_str() == artifact_id)
            .ok_or_else(|| format!("missing transform {artifact_id}"))?;
        norn_checker::transform::admit(artifact, &self.pool, &BTreeSet::new())
            .map_err(|e| format!("admit transform {artifact_id}: {e:?}"))
    }
}

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Committed,
    Replayed,
    Refused(String, String),
}

struct EntityBook {
    pack: &'static RetailPack,
    _dir: TempDir,
    cell_id: CellId,
    scope: String,
    org_id: String,
    env_id: String,
    cell: Option<RetailCell>,
    tick: u64,
    calls: u64,
    source_cursors: Frontier,
    watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
}

struct FeedRow<'a> {
    feed_id: &'a str,
    mapping: &'a str,
    dataset: &'a str,
    source: &'a str,
    event: &'a str,
    record: &'a str,
    payload: &'a str,
}

fn feed_watermarks(
    cursors: &Frontier,
    heartbeat_ms: u64,
) -> BTreeMap<String, BTreeMap<String, Watermark>> {
    cursors
        .iter()
        .map(|(feed, parts)| {
            let wm_parts = parts
                .iter()
                .map(|(part, cursor)| {
                    (
                        part.clone(),
                        Watermark {
                            observed: cursor.clone(),
                            heartbeat_ms: Some(heartbeat_ms),
                            complete: true,
                        },
                    )
                })
                .collect();
            (feed.clone(), wm_parts)
        })
        .collect()
}

fn default_feed_frontier(now_ms: u64) -> (Frontier, BTreeMap<String, BTreeMap<String, Watermark>>) {
    let mut cursors: Frontier = BTreeMap::new();
    for feed in REQUIRED_FEEDS {
        let mut parts = BTreeMap::new();
        parts.insert(FEED_PARTITION.to_owned(), format!("cursor:{feed}:synced"));
        cursors.insert(feed.to_owned(), parts);
    }
    let watermarks = feed_watermarks(&cursors, now_ms);
    (cursors, watermarks)
}

impl EntityBook {
    fn open(
        cell_id_hex: &str,
        org_id: &str,
        env_id: &str,
        file_name: &str,
    ) -> Result<Self, String> {
        let pack = retail_pack()?;
        let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let path = dir.path().join(file_name);
        let cell_id = CellId::try_from(cell_id_hex.to_owned())
            .map_err(|e| format!("cell id {cell_id_hex}: {e}"))?;
        let scope = format!("{org_id}/{env_id}");
        let engine = NativeEngine::open(&path, StoreOptions::default())
            .map_err(|e| format!("open sqlite {file_name}: {e}"))?;
        let mut store = CellStore::from_engine_bound(engine, pack.model().clone(), &cell_id)
            .map_err(|e| format!("bind store {file_name}: {e}"))?;
        store
            .bind("scope", &scope)
            .map_err(|e| format!("bind scope: {e}"))?;
        let mut cell = RetailCell::new(store, scope.clone(), |c| c.payload.clone())
            .map_err(|e| format!("open cell: {e}"))?;
        cell.install_definitions(pack.model().clone(), pack.install_input(), Some(0))
            .map_err(|e| format!("install definitions {file_name}: {e}"))?;
        Ok(Self {
            pack,
            _dir: dir,
            cell_id,
            scope,
            org_id: org_id.to_owned(),
            env_id: env_id.to_owned(),
            cell: Some(cell),
            tick: 0,
            calls: 0,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        })
    }

    fn cell(&self) -> Result<&RetailCell, String> {
        self.cell.as_ref().ok_or_else(|| "missing cell".to_owned())
    }

    fn cell_mut(&mut self) -> Result<&mut RetailCell, String> {
        self.cell.as_mut().ok_or_else(|| "missing cell".to_owned())
    }

    fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(CLOCK_MS.saturating_add(self.tick))
    }

    fn with_map_cell<T>(
        &mut self,
        f: impl FnOnce(&mut MapCell) -> Result<T, String>,
    ) -> Result<T, String> {
        let cell = self.cell.take().ok_or("missing retail cell")?;
        let store = cell.into_store();
        let mut map_cell = MapCell::new(store, self.scope.clone(), |_| BTreeMap::new())
            .map_err(|e| format!("open map view: {e}"))?;
        let now = self.now();
        map_cell.advance_to(now);
        let result = f(&mut map_cell);
        let store = map_cell.into_store();
        let restored = RetailCell::new(store, self.scope.clone(), |c| c.payload.clone())
            .map_err(|e| format!("restore retail cell: {e}"))?;
        self.cell = Some(restored);
        result
    }

    /// Ingest one raw source row through the real `SOURCE-CAPTURE-1` durable
    /// cursor + mapping pipeline on this cell so `_norn_source_cursor` and
    /// `_norn_source_pending` stay durably synchronized with the cell frontier.
    fn ingest_feed(&mut self, row: &FeedRow<'_>) -> Result<(String, bool), String> {
        self.tick = self.tick.saturating_add(1_000);
        let observed_at = self.now().as_millis();
        let obs = raw_obs(row.source, row.event, row.record, row.payload)?;
        let input = map_input(row.mapping, row.dataset);
        let next_cursor = format!("cursor:{}:{}", row.feed_id, row.event);
        let clean = self.with_map_cell(|map_cell| {
            let lease = map_cell
                .acquire_source_lease(row.feed_id, "retail-lab-sync", observed_at, 3_600_000)
                .map_err(|e| format!("lease {}: {e}", row.feed_id))?;
            let prev_cursor = map_cell
                .store()
                .read()
                .source_cursors(row.feed_id)
                .map_err(|e| format!("read cursor {}: {e}", row.feed_id))?
                .get(FEED_PARTITION)
                .map(|r| r.cursor_json.clone());
            let advance = CursorAdvanceSpec {
                connection_id: row.feed_id.to_owned(),
                dataset: FEED_PARTITION.to_owned(),
                lease_id: lease.lease_id.clone(),
                generation: lease.generation,
                now_ms: observed_at,
                expected_previous_cursor: prev_cursor,
                next_cursor: Some(next_cursor.clone()),
                renew_ttl_ms: None,
            };
            let captured = map_cell
                .capture_source_page(&[obs], Some(&input), &advance, 64)
                .map_err(|e| format!("capture {}/{}: {e}", row.feed_id, row.record))?;
            let mut all_clean = true;
            if captured.captured.pages.is_empty() {
                for rec_id in &captured.captured.records {
                    let outcome = map_cell
                        .process_mapping_outcome(rec_id)
                        .map_err(|e| format!("process {}/{}: {e}", row.feed_id, row.record))?;
                    if matches!(outcome, MappingOutcome::Quarantined(_)) {
                        all_clean = false;
                    }
                }
            } else {
                for (page_id, _) in &captured.captured.pages {
                    if let Some(outcomes) = map_cell
                        .process_mapping_page(page_id)
                        .map_err(|e| format!("process page {}/{}: {e}", row.feed_id, row.record))?
                        && outcomes
                            .iter()
                            .any(|(_, o)| matches!(o, MappingOutcome::Quarantined(_)))
                    {
                        all_clean = false;
                    }
                }
            }
            map_cell
                .complete_source_pending(row.feed_id, FEED_PARTITION, &captured.page_digest)
                .map_err(|e| format!("complete pending {}: {e}", row.feed_id))?;
            Ok(all_clean)
        })?;
        self.source_cursors
            .entry(row.feed_id.to_owned())
            .or_default()
            .insert(FEED_PARTITION.to_owned(), next_cursor.clone());
        self.watermarks
            .entry(row.feed_id.to_owned())
            .or_default()
            .insert(
                FEED_PARTITION.to_owned(),
                Watermark {
                    observed: next_cursor.clone(),
                    heartbeat_ms: Some(observed_at),
                    complete: clean,
                },
            );
        Ok((next_cursor, clean))
    }

    fn act(&mut self, action: &str, actor: &str, input: Value) -> Result<Outcome, String> {
        self.calls = self.calls.saturating_add(1);
        let key = format!("{}-call-{}", self.org_id, self.calls);
        self.act_keyed(action, actor, input, &key)
    }

    fn act_keyed(
        &mut self,
        action: &str,
        actor: &str,
        input: Value,
        key: &str,
    ) -> Result<Outcome, String> {
        self.tick = self.tick.saturating_add(1_000);
        let now = self.now();
        let org_id = self.org_id.clone();
        let env_id = self.env_id.clone();
        let pack = self.pack;
        let cell = self.cell_mut()?;
        cell.advance_to(now);
        let world = cell.store().world();
        let admitted = pack
            .definitions
            .action(action)
            .ok_or_else(|| format!("no action {action}"))?;
        let input: JsonObject =
            serde_json::from_value(input).map_err(|e| format!("{action} input: {e}"))?;
        let catalog = pack.definitions.catalog();
        let call = ActionCall {
            action: &admitted.root,
            action_artifact_id: &admitted.artifact_id,
            action_version: &admitted.version,
            scope: BTreeMap::from([("orgId".into(), org_id), ("environmentId".into(), env_id)]),
            actor: actor.to_owned(),
            approved_intent: None,
            input,
            idempotency_key: key.to_owned(),
            object_type: None,
            turn: DecisionTurn {
                id: key.to_owned(),
                now,
                world,
                consumes: None,
            },
            trace: None,
        };
        Ok(match commit_action(cell, &catalog, &call, &Allow) {
            Ok(ActionOutcome::Committed { .. }) => Outcome::Committed,
            Ok(ActionOutcome::Replayed { .. }) => Outcome::Replayed,
            Err(refusal) => Outcome::Refused(refusal.code().to_owned(), refusal.to_string()),
        })
    }

    fn ok(&mut self, action: &str, actor: &str, input: Value) -> Result<(), String> {
        match self.act(action, actor, input)? {
            Outcome::Committed => Ok(()),
            Outcome::Replayed => Err(format!("{action} expected Committed, got Replayed")),
            Outcome::Refused(code, msg) => Err(format!(
                "{action} expected Committed, got Refused({code}, {msg})"
            )),
        }
    }

    fn refused(
        &mut self,
        action: &str,
        actor: &str,
        input: Value,
    ) -> Result<(String, String), String> {
        match self.act(action, actor, input)? {
            Outcome::Refused(code, msg) => Ok((code, msg)),
            Outcome::Committed => Err(format!("{action} expected Refused, got Committed")),
            Outcome::Replayed => Err(format!("{action} expected Refused, got Replayed")),
        }
    }

    fn query_context(&self) -> QueryContext {
        QueryContext {
            world: "main".to_owned(),
            scope: self.scope.clone(),
            population_pin: "all".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "supply-lab-retail/1".to_owned(),
            now_ms: self.now().as_millis(),
            source_cursors: self.source_cursors.clone(),
            watermarks: self.watermarks.clone(),
        }
    }

    fn answer(&self, name: &str) -> Result<QueryResult, String> {
        let plan = self.pack.retail_question(name)?;
        self.cell()?
            .store()
            .evaluate_question(&plan, self.query_context(), |_| Ok(true))
            .map_err(|e| format!("evaluate {name}: {e}"))
    }

    fn evaluate_transform(
        &self,
        artifact_id: &str,
        access: impl FnMut(&norn_cell::ObjectRecord) -> Result<RowAccess, CellError>,
    ) -> Result<TransformResult, String> {
        let plan = self.pack.transform_plan(artifact_id)?;
        self.cell()?
            .store()
            .evaluate_transform(&plan, "main", &self.scope, access)
            .map_err(|e| format!("transform {artifact_id}: {e}"))
    }

    fn head(&self) -> Result<LedgerHead, String> {
        self.cell()?
            .store()
            .read()
            .verify_ledger()
            .map_err(|e| format!("verify_ledger: {e}"))
    }
}

fn scalar(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::String(s) => s.clone(),
        RuntimeValue::Number(d) => d.canonical(),
        RuntimeValue::Bool(b) => b.to_string(),
        RuntimeValue::Null => "null".to_owned(),
        RuntimeValue::Array(items) => format!("{items:?}"),
        RuntimeValue::Object(obj) => format!("{obj:?}"),
    }
}

/// Extract exact structured grouping tuple from `QueryRow.bucket` (never substring-matched).
fn bucket_parts(bucket: &RuntimeValue) -> Vec<String> {
    if let RuntimeValue::Array(items) = bucket {
        items.iter().map(scalar).collect()
    } else {
        vec![scalar(bucket)]
    }
}

fn bucket_decimal_exact(result: &QueryResult, expected: &[&str]) -> Option<Decimal> {
    result.rows.values().find_map(|row| {
        let parts = bucket_parts(&row.bucket);
        let matches = parts.len() == expected.len()
            && parts
                .iter()
                .zip(expected.iter())
                .all(|(actual, want)| actual == *want);
        matches.then_some(row.value).flatten()
    })
}

fn bucket_total_exact(result: &QueryResult, expected: &[&str]) -> Option<String> {
    bucket_decimal_exact(result, expected).map(|d| d.canonical())
}

/// Fail-closed lookup on a `[currency, store_id, cost_status, week]` `QueryResult`:
/// returns `None` (withheld) if ANY `missing_cost` slice is present for `(currency, store_id, week)`.
fn resolve_costed_bucket(
    result: &QueryResult,
    currency: &str,
    store_id: &str,
    week: &str,
) -> Option<Decimal> {
    let has_missing = result.rows.values().any(|row| {
        let parts = bucket_parts(&row.bucket);
        parts.first().map(String::as_str) == Some(currency)
            && parts.get(1).map(String::as_str) == Some(store_id)
            && parts.get(2).map(String::as_str) == Some("missing_cost")
            && parts.get(3).map(String::as_str) == Some(week)
    });
    if has_missing {
        return None;
    }
    bucket_decimal_exact(result, &[currency, store_id, "costed", week])
}

fn decimal_to_scaled_i128(value: Decimal) -> Result<i128, String> {
    let text = value.canonical();
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.as_str()),
    };
    let (whole_str, frac_str) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let whole: i128 = whole_str
        .parse()
        .map_err(|e| format!("parse whole {text}: {e}"))?;
    if frac_str.len() > 6 {
        return Err(format!("scale > 6 not supported: {text}"));
    }
    let mut padded = frac_str.to_owned();
    while padded.len() < 6 {
        padded.push('0');
    }
    let frac: i128 = padded
        .parse()
        .map_err(|e| format!("parse frac {text}: {e}"))?;
    let magnitude = whole
        .checked_mul(FACTOR)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| format!("scaled overflow: {text}"))?;
    if negative {
        magnitude
            .checked_neg()
            .ok_or_else(|| format!("negation overflow: {text}"))
    } else {
        Ok(magnitude)
    }
}

fn check_feed_coverage(
    cursors: &Frontier,
    watermarks: &BTreeMap<String, BTreeMap<String, Watermark>>,
    min_fresh_ms: u64,
) -> BTreeSet<String> {
    REQUIRED_FEEDS
        .iter()
        .filter(|feed| {
            let Some(parts) = cursors.get(**feed) else {
                return true;
            };
            if parts.is_empty() {
                return true;
            }
            let Some(wm_parts) = watermarks.get(**feed) else {
                return true;
            };
            !parts.iter().all(|(part, consumed)| {
                wm_parts.get(part).is_some_and(|wm| {
                    wm.complete
                        && wm.observed == *consumed
                        && wm.heartbeat_ms.is_some_and(|ts| ts >= min_fresh_ms)
                })
            })
        })
        .map(|feed| (*feed).to_owned())
        .collect()
}

fn seed_sole_prop(book: &mut EntityBook) -> Result<(), String> {
    book.ok(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-SOLE",
            "legal_name": "Nile Fashion Sole Proprietorship",
            "entity_kind": "sole_proprietorship",
            "book_code": "BOOK-SOLE-EGP",
            "functional_currency": "EGP"
        }),
    )?;
    for (sku, cat) in [
        ("SKU-DRESS", "dry_goods"),
        ("SKU-RING-18K", "spares"),
        ("SKU-SCARF", "dry_goods"),
    ] {
        book.ok(
            "supply.sku.register",
            "eng:dina",
            json!({"sku_id": sku, "category": cat, "unit_of_measure": "each", "shelf_life_days": 3650}),
        )?;
    }
    book.ok(
        "supply.unit_cost.record",
        "eng:dina",
        json!({
            "sku_id": "SKU-DRESS", "cost_ref": "COST-DRESS-1", "owner_entity_id": "LE-SOLE",
            "supplier_id": "SUP-ATELIER", "unit_cost": "100", "currency": "EGP",
            "effective_from": "2026-07-01T00:00:00Z"
        }),
    )?;
    for (store_id, name, kind, mall, custodian) in [
        (
            "ST-SOLE-CFC-1",
            "Sole CFC Fashion",
            "mall_store",
            Some("MALL-CFC"),
            "CUST-MALL-CFC-OPS",
        ),
        (
            "ST-SOLE-CFC-2",
            "Sole CFC Accessories",
            "mall_store",
            Some("MALL-CFC"),
            "CUST-MALL-CFC-OPS",
        ),
        (
            "ST-SOLE-CFC-3",
            "Sole CFC Outlet",
            "mall_store",
            Some("MALL-CFC"),
            "CUST-MALL-CFC-OPS",
        ),
        (
            "ST-SOLE-ZAM",
            "Sole Zamalek",
            "standalone",
            None,
            "CUST-BRANCH-ZAM",
        ),
    ] {
        book.ok(
            "supply.store.register",
            "eng:dina",
            json!({
                "store_id": store_id, "name": name, "entity_id": "LE-SOLE",
                "custodian_id": custodian, "location_kind": kind,
                "mall_code": mall, "currency": "EGP"
            }),
        )?;
    }
    Ok(())
}

fn seed_partnership(book: &mut EntityBook) -> Result<(), String> {
    book.ok(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-PARTNER",
            "legal_name": "Cairo Fine Jewelry Partnership",
            "entity_kind": "partnership",
            "book_code": "BOOK-PARTNER-USD",
            "functional_currency": "USD"
        }),
    )?;
    book.ok(
        "supply.sku.register",
        "eng:dina",
        json!({"sku_id": "SKU-RING-18K", "category": "spares", "unit_of_measure": "each", "shelf_life_days": 3650}),
    )?;
    book.ok(
        "supply.unit_cost.record",
        "eng:dina",
        json!({
            "sku_id": "SKU-RING-18K", "cost_ref": "COST-RING-1", "owner_entity_id": "LE-PARTNER",
            "supplier_id": "SUP-GOLD", "unit_cost": "250", "currency": "USD",
            "effective_from": "2026-07-01T00:00:00Z"
        }),
    )?;
    for (store_id, name, kind, mall, custodian) in [
        (
            "ST-PART-CFC-1",
            "Partner CFC Gold",
            "mall_store",
            Some("MALL-CFC"),
            "CUST-MALL-CFC-OPS",
        ),
        (
            "ST-PART-CFC-2",
            "Partner CFC Diamonds",
            "mall_store",
            Some("MALL-CFC"),
            "CUST-MALL-CFC-OPS",
        ),
        (
            "ST-PART-MAADI",
            "Partner Maadi",
            "standalone",
            None,
            "CUST-BRANCH-MAADI",
        ),
    ] {
        book.ok(
            "supply.store.register",
            "eng:dina",
            json!({
                "store_id": store_id, "name": name, "entity_id": "LE-PARTNER",
                "custodian_id": custodian, "location_kind": kind,
                "mall_code": mall, "currency": "USD"
            }),
        )?;
    }
    Ok(())
}

/// Seed `ST-SOLE-CFC-1` lots with exact unit conservation:
/// `50 (W1) + 10 (W2 depleted) + 40 (W5) = 100 received`, `25 + 0 + 35 = 60 remaining`.
/// Captures immutable opening `InventoryValuationCut` at `2026-08-10T00:00:00Z` (10000 EGP)
/// before during-period depletions occur.
fn seed_sole_cfc1_lots(sole: &mut EntityBook) -> Result<(), String> {
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "50", "remaining_qty": "50", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-07-08T09:00:00Z", "as_of": "2026-07-08T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-DEPLETED", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-07-15T09:00:00Z", "as_of": "2026-07-15T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W5", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "40", "remaining_qty": "40", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-05T09:00:00Z", "as_of": "2026-08-05T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    // Immutable opening inventory valuation cut at period_start (`2026-08-10T00:00:00Z` = 10000 EGP).
    sole.ok(
        "supply.stock_valuation_cut.record",
        "planner:omar",
        json!({
            "cut_id": "CUT-S1-2026-08-10", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "currency": "EGP", "cost_status": "costed", "book_epoch": "1970-01-01T00:00:00Z",
            "cut_at": "2026-08-10T00:00:00Z", "stock_value": "10000"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "25", "depleted_qty": "25",
            "as_of": "2026-09-04T18:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-DEPLETED", "remaining_qty": "0", "depleted_qty": "10",
            "as_of": "2026-08-20T09:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W5", "remaining_qty": "35", "depleted_qty": "5",
            "as_of": "2026-09-04T18:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )
}

/// Seed `ST-SOLE-CFC-1` fresh counts + 4 consecutive weeks of costed sales (W6..W9 = 40 units,
/// 9000 EGP revenue, 4000 EGP COGS, 5000 EGP margin; W9 ratio-of-sums = 2000/3000 = 0.666667).
fn seed_sole_cfc1(sole: &mut EntityBook) -> Result<(), String> {
    seed_sole_cfc1_lots(sole)?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/available", "store_id": "ST-SOLE-CFC-1",
            "sku_id": "SKU-DRESS", "on_hand_qty": "56", "unit": "each",
            "quantity_basis": "available", "as_of": "2026-09-05T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-101"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1",
            "sku_id": "SKU-DRESS", "on_hand_qty": "62", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-08-25T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-100"
        }),
    )?;
    sole.ok(
        "supply.store_count.recount",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1",
            "sku_id": "SKU-DRESS", "on_hand_qty": "60", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-05T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-102"
        }),
    )?;
    for (sale_id, qty, rev, sold_at) in [
        ("SALE-S1-W6", "10", "2000", "2026-08-12T14:00:00Z"),
        ("SALE-S1-W7", "10", "2000", "2026-08-19T14:00:00Z"),
        ("SALE-S1-W8", "10", "2000", "2026-08-26T14:00:00Z"),
        ("SALE-S1-1", "6", "1200", "2026-09-01T14:00:00Z"),
        ("SALE-S1-2", "4", "1800", "2026-09-03T16:00:00Z"),
    ] {
        sole.ok(
            "supply.pos_sale.record",
            "clerk:hana",
            json!({
                "sale_id": sale_id, "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
                "sold_qty": qty, "unit": "each", "net_revenue": rev, "applied_unit_cost": "100",
                "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
                "cost_status": "costed", "costed_ref": "COST-DRESS-1",
                "currency": "EGP", "sold_at": sold_at
            }),
        )?;
    }
    Ok(())
}

fn seed_sole_edge_stores(sole: &mut EntityBook) -> Result<(), String> {
    // ST-SOLE-CFC-2: MIXED costed + uncosted bucket AND NO physical count.
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S2-COSTED", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-20T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record_uncosted",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S2-NOCOST", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "20", "remaining_qty": "15", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-20T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "missing_cost", "opening_stock_value": "0", "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S2-1", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "sold_qty": "3", "unit": "each", "net_revenue": "600", "applied_unit_cost": "100",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-02T12:00:00Z"
        }),
    )?;
    sole.ok(
        "supply.pos_sale.record_uncosted",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S2-2", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "sold_qty": "2", "unit": "each", "net_revenue": "400",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "missing_cost", "currency": "EGP", "sold_at": "2026-09-03T12:00:00Z"
        }),
    )?;
    // ST-SOLE-CFC-3: STALE COUNT (>7 days old) + on-hand drift (28 vs 20).
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S3-STALE", "store_id": "ST-SOLE-CFC-3", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "30", "remaining_qty": "20", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-3/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-3",
            "sku_id": "SKU-DRESS", "on_hand_qty": "28", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-08-10T09:00:00Z",
            "count_source": "handheld_scan", "count_ref": "CC-OLD"
        }),
    )?;
    // ST-SOLE-ZAM: FUTURE COUNT (`as_of: 2026-09-10 > CLOCK_MS 2026-09-07`).
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-ZAM-1", "store_id": "ST-SOLE-ZAM", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-BRANCH-ZAM",
            "received_qty": "15", "remaining_qty": "15", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-18T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-ZAM/SKU-DRESS/on_hand", "store_id": "ST-SOLE-ZAM",
            "sku_id": "SKU-DRESS", "on_hand_qty": "15", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-10T09:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-FUT"
        }),
    )
}

fn seed_partner_operations(partner: &mut EntityBook) -> Result<(), String> {
    partner.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-P1-W3", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "owner_entity_id": "LE-PARTNER", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "20", "remaining_qty": "20", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-07-22T09:00:00Z", "as_of": "2026-07-22T09:00:00Z",
            "cost_status": "costed", "unit_cost": "250", "opening_stock_value": "0",
            "stock_value": "0", "currency": "USD"
        }),
    )?;
    partner.ok(
        "supply.stock_valuation_cut.record",
        "planner:omar",
        json!({
            "cut_id": "CUT-P1-2026-08-31", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "currency": "USD", "cost_status": "costed", "book_epoch": "1970-01-01T00:00:00Z",
            "cut_at": "2026-08-31T00:00:00Z", "stock_value": "5000"
        }),
    )?;
    partner.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-P1-W3", "remaining_qty": "12", "depleted_qty": "8",
            "as_of": "2026-09-05T10:00:00Z", "unit_cost": "250", "stock_value": "0"
        }),
    )?;
    partner.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-PART-CFC-1/SKU-RING-18K/available", "store_id": "ST-PART-CFC-1",
            "sku_id": "SKU-RING-18K", "on_hand_qty": "12", "unit": "each",
            "quantity_basis": "available", "as_of": "2026-09-06T10:00:00Z",
            "count_source": "store_audit", "count_ref": "AUD-P1"
        }),
    )?;
    partner.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-PART-CFC-1/SKU-RING-18K/on_hand", "store_id": "ST-PART-CFC-1",
            "sku_id": "SKU-RING-18K", "on_hand_qty": "12", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-06T10:00:00Z",
            "count_source": "store_audit", "count_ref": "AUD-P2"
        }),
    )?;
    partner.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-P1-1", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "sold_qty": "8", "unit": "each", "net_revenue": "3200", "applied_unit_cost": "250",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-RING-1",
            "currency": "USD", "sold_at": "2026-09-02T15:00:00Z"
        }),
    )
}

fn seed_retail_operations(sole: &mut EntityBook, partner: &mut EntityBook) -> Result<(), String> {
    seed_sole_cfc1(sole)?;
    seed_sole_edge_stores(sole)?;
    seed_partner_operations(partner)
}

// ------------------------------------------------------------------ Test 1

fn assert_cost_binding_refusals(sole: &mut EntityBook) -> Result<(), String> {
    sole.refused(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-FORGED-COST", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "2", "unit": "each", "net_revenue": "400", "applied_unit_cost": "50",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;
    sole.refused(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-WRONG-REF", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "2", "unit": "each", "net_revenue": "400", "applied_unit_cost": "100",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-NONEXISTENT",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;
    sole.refused(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-NO-COST-SKU", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-RING-18K",
            "sold_qty": "1", "unit": "each", "net_revenue": "500", "applied_unit_cost": "100",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;
    sole.ok(
        "supply.unit_cost.record",
        "eng:dina",
        json!({
            "sku_id": "SKU-SCARF", "cost_ref": "COST-SCARF-OCT", "owner_entity_id": "LE-SOLE",
            "supplier_id": "SUP-ATELIER", "unit_cost": "45", "currency": "EGP",
            "effective_from": "2026-10-01T00:00:00Z"
        }),
    )?;
    let (code, msg) = sole.refused(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-FUTURE-COST", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-SCARF",
            "sold_qty": "2", "unit": "each", "net_revenue": "200", "applied_unit_cost": "45",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-SCARF-OCT",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    assert!(msg.contains("sale_matches_cost_record"), "{msg}");
    Ok(())
}

fn assert_store_isolation_and_lifecycle_refusals(sole: &mut EntityBook) -> Result<(), String> {
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-DUP-1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "20", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-05T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-1"
        }),
    )?;
    sole.refused(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-DUP-2", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "25", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-06T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-2"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-HOLD", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-08-01T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    let (code, msg) = sole.refused(
        "supply.store.close",
        "ops:omar",
        json!({"store_id": "ST-SOLE-CFC-1"}),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    assert!(msg.contains("store_closed_requires_no_stock"), "{msg}");
    sole.ok(
        "supply.store.close",
        "ops:omar",
        json!({"store_id": "ST-SOLE-ZAM"}),
    )?;

    let plan = sole
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let mut foreign_ctx = sole.query_context();
    foreign_ctx.scope = PARTNER_SCOPE.to_owned();
    let cross_scope = sole
        .cell()?
        .store()
        .evaluate_question(&plan, foreign_ctx, |_| Ok(true));
    assert!(
        matches!(
            cross_scope,
            Err(CellError::ComputeRefused("core.compute.stale_input"))
        ),
        "foreign scope query must refuse with core.compute.stale_input: {cross_scope:?}"
    );
    Ok(())
}

#[test]
fn s1_two_entity_non_commingling_and_custody_separation() -> Result<(), String> {
    let mut sole = EntityBook::open(SOLE_CELL_ID, "org_family_sole_prop", "prod", "sole.sqlite")?;
    let mut partner = EntityBook::open(
        PARTNER_CELL_ID,
        "org_family_partnership",
        "prod",
        "partner.sqlite",
    )?;
    seed_sole_prop(&mut sole)?;
    seed_partnership(&mut partner)?;

    let sole_store = sole
        .cell()?
        .store()
        .read()
        .object("main", "supply.Store", "ST-SOLE-CFC-1")
        .map_err(|e| format!("{e}"))?
        .ok_or("missing ST-SOLE-CFC-1")?;
    assert_eq!(
        sole_store.values.get("entity_id").map(scalar).as_deref(),
        Some("LE-SOLE")
    );
    assert_eq!(
        sole_store.values.get("custodian_id").map(scalar).as_deref(),
        Some("CUST-MALL-CFC-OPS")
    );
    let partner_store = partner
        .cell()?
        .store()
        .read()
        .object("main", "supply.Store", "ST-PART-CFC-1")
        .map_err(|e| format!("{e}"))?
        .ok_or("missing ST-PART-CFC-1")?;
    assert_eq!(
        partner_store.values.get("entity_id").map(scalar).as_deref(),
        Some("LE-PARTNER")
    );
    assert_eq!(
        partner_store
            .values
            .get("custodian_id")
            .map(scalar)
            .as_deref(),
        Some("CUST-MALL-CFC-OPS")
    );

    sole.refused(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-ROGUE-PARTNER", "name": "Cross Entity", "entity_id": "LE-PARTNER",
            "custodian_id": "CUST-MALL-CFC-OPS", "location_kind": "mall_store",
            "mall_code": "MALL-CFC", "currency": "USD"
        }),
    )?;
    sole.refused(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-SOLE-USD", "name": "Wrong Currency", "entity_id": "LE-SOLE",
            "custodian_id": "CUST-MALL-CFC-OPS", "location_kind": "mall_store",
            "mall_code": "MALL-CFC", "currency": "USD"
        }),
    )?;
    // D02 single-book-per-cell: registering a second LegalEntity in an occupied cell is
    // refused structurally at the write path (`existing_books === 0`).
    let (code, msg) = sole.refused(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-OTHER-EGP", "legal_name": "Other EGP Entity",
            "entity_kind": "corporation", "book_code": "BOOK-OTHER-EGP", "functional_currency": "EGP"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    sole.refused(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-WRONG-OWNER", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-PARTNER", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-08-01T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;

    assert_cost_binding_refusals(&mut sole)?;
    assert_store_isolation_and_lifecycle_refusals(&mut sole)?;
    sole.head()?;
    partner.head()?;
    Ok(())
}

// ------------------------------------------------------------------ Test 2

fn open_retail_mapping_cell(pack: &RetailPack, dir: &Path) -> Result<MapCell, String> {
    let model = pack.model().clone();
    let mut cell = MapCell::open(
        &dir.join("retail-map.sqlite"),
        model.clone(),
        StoreOptions::default(),
        SOLE_SCOPE,
        |_| BTreeMap::new(),
    )
    .map_err(|e| format!("open map cell: {e}"))?;
    cell.install_definitions(model, pack.install_input(), Some(0))
        .map_err(|e| format!("install: {e}"))?;
    Ok(cell)
}

fn raw_obs(
    source: &str,
    event: &str,
    record: &str,
    payload: &str,
) -> Result<Observed<RuntimeValue>, String> {
    let value = RuntimeValue::from_json_text(payload).map_err(|e| format!("{e:?}"))?;
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(payload)
        .map_err(|e| format!("{e:?}"))?;
    Ok(Observed {
        id: ObservationId(event.to_owned()),
        provenance: Provenance {
            source: source.to_owned(),
            source_id: record.to_owned(),
            mapping_version: "retail-pipeline/1".to_owned(),
            observed_at: LogicalTime::from_millis(CLOCK_MS),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => source,
            "source_id" => record,
            "event_id" => event,
            "source_version" => "1",
            "pipeline_version" => "retail-pipeline/1",
            "kind" => "upsert",
            "extracted_at" => CLOCK_MS,
            "payload" => value,
            "payload_digest" => digest,
        },
    })
}

fn map_input(mapping: &str, dataset: &str) -> MappingInput {
    MappingInput {
        mapping: mapping.to_owned(),
        dataset: dataset.to_owned(),
        connector_instance: "retail-lab".to_owned(),
        partition: FEED_PARTITION.to_owned(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-09-07T12:00:00Z".to_owned()),
    }
}

fn open_findings(cell: &MapCell, object_type: &str, key: &str) -> Result<Vec<String>, String> {
    let found = cell
        .store()
        .read()
        .observation_findings("main", object_type, key)
        .map_err(|e| format!("findings {object_type}/{key}: {e}"))?;
    found
        .into_iter()
        .filter(|f| matches!(f.status, DataQualityFindingStatus::Open))
        .map(|f| serde_json::to_string(&f.rule_ref).map_err(|e| format!("{e}")))
        .collect()
}

fn verify_pos_quarantine_and_recovery(
    cell: &mut MapCell,
    pos_in: &MappingInput,
    pos_q: &QuestionPlan,
    feed_ctx: &QueryContext,
) -> Result<(), String> {
    let bad_pos = r#"{"pos_line_id":"POS-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":-3,"uom":"PCS","net_amount":600,"ccy":"EGP","sold_at":"2026-09-01T14:00:00Z"}"#;
    let (b_bad, fresh_bad) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-bad", "POS-BAD", bad_pos)?,
            pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    assert!(fresh_bad);
    cell.process_mapping_bundle(&b_bad)
        .map_err(|e| format!("{e}"))?;
    let f = open_findings(cell, "supply.PosSaleEvidence", "POS-BAD")?;
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f.iter().any(|r| r.contains("sale_positive")), "{f:?}");

    let before_fix = cell
        .store()
        .evaluate_question(pos_q, feed_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("pos feed query before fix: {e}"))?;
    assert_eq!(
        bucket_total_exact(&before_fix, &["EGP", "ST-SOLE-CFC-1", "2026-08-31"]).as_deref(),
        Some("1000")
    );
    assert!(!before_fix.complete);

    let (b_dup, fresh_dup) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-bad", "POS-BAD", bad_pos)?,
            pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    assert!(!fresh_dup, "exact redelivery captured once");
    assert!(
        !cell
            .process_mapping_bundle(&b_dup)
            .map_err(|e| format!("{e}"))?
    );
    let (b_reemit, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-bad-2", "POS-BAD", bad_pos)?,
            pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_reemit)
        .map_err(|e| format!("{e}"))?;
    assert_eq!(
        open_findings(cell, "supply.PosSaleEvidence", "POS-BAD")?.len(),
        1
    );

    let fixed_pos = r#"{"pos_line_id":"POS-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":3,"uom":"PCS","net_amount":600,"ccy":"EGP","sold_at":"2026-09-01T14:05:00Z"}"#;
    let (b_fix, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-fix", "POS-BAD", fixed_pos)?,
            pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_fix)
        .map_err(|e| format!("{e}"))?;
    assert!(open_findings(cell, "supply.PosSaleEvidence", "POS-BAD")?.is_empty());

    let after_fix = cell
        .store()
        .evaluate_question(pos_q, feed_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("pos feed query after fix: {e}"))?;
    assert!(after_fix.complete);
    assert_eq!(
        bucket_total_exact(&after_fix, &["EGP", "ST-SOLE-CFC-1", "2026-08-31"]).as_deref(),
        Some("1600")
    );
    verify_pos_lineage(cell, pos_q, feed_ctx, &after_fix)
}

fn verify_pos_feed_and_lineage(
    cell: &mut MapCell,
    pos_q: &QuestionPlan,
    feed_ctx: &QueryContext,
) -> Result<(), String> {
    let pos_in = map_input("supply.pos_sales.mapping", "pos_sale_line");
    let clean_pos_1 = r#"{"pos_line_id":"POS-1","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":3,"uom":"PCS","net_amount":600,"ccy":"LE","sold_at":"2026-09-01T14:00:00Z"}"#;
    let clean_pos_2 = r#"{"pos_line_id":"POS-2","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":2,"uom":"PCS","net_amount":400,"ccy":"EGP","sold_at":"2026-09-02T14:00:00Z"}"#;
    for (ev, id, payload) in [
        ("ev-pos-1", "POS-1", clean_pos_1),
        ("ev-pos-2", "POS-2", clean_pos_2),
    ] {
        let (bundle, fresh) = cell
            .inject_mapping_bundle(&raw_obs("synthetic-pos", ev, id, payload)?, &pos_in)
            .map_err(|e| format!("{e}"))?;
        assert!(fresh);
        assert!(
            cell.process_mapping_bundle(&bundle)
                .map_err(|e| format!("{e}"))?
        );
    }
    let pos_obj = cell
        .store()
        .read()
        .object("main", "supply.PosSaleEvidence", "POS-1")
        .map_err(|e| format!("{e}"))?
        .ok_or("POS-1 landed")?;
    assert_eq!(
        pos_obj.values.get("unit").map(scalar).as_deref(),
        Some("each")
    );
    assert_eq!(
        pos_obj.values.get("currency").map(scalar).as_deref(),
        Some("EGP")
    );
    verify_pos_quarantine_and_recovery(cell, &pos_in, pos_q, feed_ctx)?;

    let bad_uom = r#"{"pos_line_id":"POS-UOM","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":2,"uom":"BOX","net_amount":200,"ccy":"EGP","sold_at":"2026-09-01T14:00:00Z"}"#;
    let (b_uom, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-uom", "POS-UOM", bad_uom)?,
            &pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    assert!(matches!(
        cell.process_mapping_outcome(&b_uom)
            .map_err(|e| format!("{e}"))?,
        MappingOutcome::Quarantined(_)
    ));
    Ok(())
}

fn verify_pos_lineage(
    cell: &MapCell,
    pos_q: &QuestionPlan,
    feed_ctx: &QueryContext,
    after_fix: &QueryResult,
) -> Result<(), String> {
    let pos_bucket = after_fix
        .rows
        .iter()
        .find_map(|(k, row)| {
            (bucket_parts(&row.bucket) == ["EGP", "ST-SOLE-CFC-1", "2026-08-31"]).then(|| k.clone())
        })
        .ok_or("missing pos feed bucket")?;
    let mut allow_values = |_: &norn_cell::ObjectRecord| Ok(true);
    let mut allow_source = |_: &norn_cell::ObjectRecord| Ok(true);
    let traced = cell
        .store()
        .trace_question(
            pos_q,
            feed_ctx.clone(),
            &after_fix.lineage_ref,
            &pos_bucket,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut allow_values,
                source: &mut allow_source,
            },
        )
        .map_err(|e| format!("trace pos feed authorized: {e}"))?;
    assert_eq!(traced.contributions.len(), 3, "POS-1, POS-2, POS-BAD");
    let resolved_records: BTreeSet<String> = traced
        .contributions
        .iter()
        .filter_map(|c| match &c.source {
            ContributionSource::Resolved { provenance } => {
                Some(provenance.source_record_ref.record.clone())
            }
            ContributionSource::Restricted | ContributionSource::Unavailable => None,
        })
        .collect();
    assert!(
        resolved_records.contains("ST-SOLE-CFC-1/POS-1")
            && resolved_records.contains("ST-SOLE-CFC-1/POS-2")
            && resolved_records.contains("ST-SOLE-CFC-1/POS-BAD"),
        "expected resolved POS source records, got {resolved_records:?}"
    );

    let mut deny_values = |_: &norn_cell::ObjectRecord| Ok(false);
    let mut deny_source = |_: &norn_cell::ObjectRecord| Ok(false);
    let restricted = cell
        .store()
        .trace_question(
            pos_q,
            feed_ctx.clone(),
            &after_fix.lineage_ref,
            &pos_bucket,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut deny_values,
                source: &mut deny_source,
            },
        )
        .map_err(|e| format!("trace pos feed restricted: {e}"))?;
    assert_eq!(restricted.contributions.len(), 3);
    assert!(restricted.contributions.iter().all(|c| {
        c.restricted
            && c.values.is_none()
            && c.amounts.is_none()
            && matches!(c.source, ContributionSource::Restricted)
    }));
    Ok(())
}

fn verify_receipt_and_cost_feeds(cell: &mut MapCell) -> Result<(), String> {
    let rcpt_in = map_input("supply.retail_receipts.mapping", "retail_receipt");
    let nocost_rcpt = r#"{"receipt_line_id":"RR-NOCOST","store_code":"ST-SOLE-CFC-2","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","received_qty":20,"damaged_qty":0,"uom":"PCS","unit_cost":null,"ccy":"EGP","received_at":"2026-08-20T09:00:00Z"}"#;
    let (b_r1, _) = cell
        .inject_mapping_bundle(
            &raw_obs(
                "synthetic-retail-receipt",
                "ev-rr-1",
                "RR-NOCOST",
                nocost_rcpt,
            )?,
            &rcpt_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_r1)
        .map_err(|e| format!("{e}"))?;
    let r_obj = cell
        .store()
        .read()
        .object("main", "supply.RetailReceiptEvidence", "RR-NOCOST")
        .map_err(|e| format!("{e}"))?
        .ok_or("RR-NOCOST landed")?;
    assert_eq!(
        r_obj.values.get("supplier_id").map(scalar).as_deref(),
        Some("SUP-ATELIER")
    );
    assert!(open_findings(cell, "supply.RetailReceiptEvidence", "RR-NOCOST")?.is_empty());

    let bad_rcpt = r#"{"receipt_line_id":"RR-BAD","store_code":"ST-SOLE-CFC-2","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","received_qty":10,"damaged_qty":12,"uom":"PCS","unit_cost":-5,"ccy":"EGP","received_at":"2026-08-20T09:00:00Z"}"#;
    let (b_r2, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-retail-receipt", "ev-rr-2", "RR-BAD", bad_rcpt)?,
            &rcpt_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_r2)
        .map_err(|e| format!("{e}"))?;
    let r_f = open_findings(cell, "supply.RetailReceiptEvidence", "RR-BAD")?;
    assert!(
        r_f.iter().any(|r| r.contains("damaged_within_received")),
        "{r_f:?}"
    );
    assert!(
        r_f.iter().any(|r| r.contains("retail_receipt_cost_valid")),
        "{r_f:?}"
    );

    let cost_in = map_input("supply.unit_costs.mapping", "unit_cost");
    let bad_cost = r#"{"cost_id":"COST-BAD","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","unit_cost":0,"ccy":"EGP","effective_from":"2026-08-01T00:00:00Z"}"#;
    let (b_c1, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-cost", "ev-c-1", "COST-BAD", bad_cost)?,
            &cost_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_c1)
        .map_err(|e| format!("{e}"))?;
    let c_f = open_findings(cell, "supply.UnitCostEvidence", "COST-BAD")?;
    assert!(
        c_f.iter().any(|r| r.contains("unit_cost_positive")),
        "{c_f:?}"
    );

    let good_cost = r#"{"cost_id":"COST-BAD","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","unit_cost":100,"ccy":"EGP","effective_from":"2026-08-01T00:00:00Z"}"#;
    let (b_c2, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-cost", "ev-c-2", "COST-BAD", good_cost)?,
            &cost_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_c2)
        .map_err(|e| format!("{e}"))?;
    assert!(open_findings(cell, "supply.UnitCostEvidence", "COST-BAD")?.is_empty());
    Ok(())
}

fn verify_secondary_feeds(cell: &mut MapCell) -> Result<(), String> {
    let cnt_in = map_input("supply.store_counts.mapping", "store_count");
    let neg_cnt = r#"{"count_id":"SC-BAD","store_code":"ST-SOLE-CFC-1","sku_code":"SKU-DRESS","counted_qty":-10,"uom":"EA","basis":"on_hand","counted_at":"2026-09-05T10:00:00Z"}"#;
    let (b_cnt, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-store-count", "ev-cnt-1", "SC-BAD", neg_cnt)?,
            &cnt_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_cnt)
        .map_err(|e| format!("{e}"))?;
    let cnt_f = open_findings(cell, "supply.StoreCountEvidence", "SC-BAD")?;
    assert!(
        cnt_f.iter().any(|r| r.contains("on_hand_non_negative")),
        "{cnt_f:?}"
    );

    let bad_basis = r#"{"count_id":"SC-BASIS","store_code":"ST-SOLE-CFC-1","sku_code":"SKU-DRESS","counted_qty":10,"uom":"EA","basis":"shrinkage","counted_at":"2026-09-05T10:00:00Z"}"#;
    let (b_basis, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-store-count", "ev-cnt-2", "SC-BASIS", bad_basis)?,
            &cnt_in,
        )
        .map_err(|e| format!("{e}"))?;
    assert!(matches!(
        cell.process_mapping_outcome(&b_basis)
            .map_err(|e| format!("{e}"))?,
        MappingOutcome::Quarantined(_)
    ));
    verify_receipt_and_cost_feeds(cell)
}

#[test]
fn s1_messy_retail_feeds_quarantine_idempotency_and_recovery() -> Result<(), String> {
    let pack = retail_pack()?;
    let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
    let mut cell = open_retail_mapping_cell(pack, dir.path())?;
    let (source_cursors, watermarks) = default_feed_frontier(CLOCK_MS);
    let feed_ctx = QueryContext {
        world: "main".to_owned(),
        scope: SOLE_SCOPE.to_owned(),
        population_pin: "all".to_owned(),
        authorization_epoch: 1,
        mapping_pin: "supply.pos_sales.mapping/0.1.0".to_owned(),
        now_ms: CLOCK_MS,
        source_cursors: source_cursors.clone(),
        watermarks: watermarks.clone(),
    };
    let pos_q = pack.retail_question("supply.retail.pos_feed_revenue_weekly")?;
    verify_pos_feed_and_lineage(&mut cell, &pos_q, &feed_ctx)?;

    let mut partial_wm = watermarks;
    partial_wm.remove("supply.store_counts");
    let mut degraded_ctx = feed_ctx;
    degraded_ctx.watermarks = partial_wm.clone();
    let degraded_res = cell
        .store()
        .evaluate_question(&pos_q, degraded_ctx, |_| Ok(true))
        .map_err(|e| format!("degraded pos feed query: {e}"))?;
    assert!(!degraded_res.complete);
    assert_eq!(degraded_res.freshness, Freshness::Incomplete);
    let missing_feeds = check_feed_coverage(
        &source_cursors,
        &partial_wm,
        CLOCK_MS.saturating_sub(FEED_MAX_AGE_MS),
    );
    assert!(missing_feeds.contains("supply.store_counts"));

    verify_secondary_feeds(&mut cell)?;
    cell.store()
        .read()
        .verify_ledger()
        .map_err(|e| format!("verify_ledger: {e}"))?;
    Ok(())
}

// ------------------------------------------------------------------ Test 3

#[derive(Debug, Clone, PartialEq, Eq)]
enum CountCoverageKind {
    MissingOnHandCount,
    MissingAvailableCount,
    FutureCount {
        basis: String,
        count_as_of: String,
    },
    StaleCount {
        basis: String,
        last_count_as_of: String,
        age_ms: u64,
    },
    OnHandDrift {
        counted_qty: String,
        derived_lot_qty: String,
    },
    AvailableExceedsOnHand {
        available_qty: String,
        on_hand_qty: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CountCoverageFinding {
    store_id: String,
    sku_id: String,
    kind: CountCoverageKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CountCoverageReport {
    findings: Vec<CountCoverageFinding>,
    on_hand_issue_stores: BTreeSet<String>,
    availability_issue_stores: BTreeSet<String>,
}

fn parse_rfc3339_ms(ts: &str) -> Result<u64, String> {
    let (secs, nanos) =
        instant_key(ts, false).ok_or_else(|| format!("invalid RFC3339 timestamp: {ts}"))?;
    let secs_u64 = u64::try_from(secs).map_err(|_| format!("negative timestamp {ts}"))?;
    Ok(secs_u64
        .saturating_mul(1_000)
        .saturating_add(u64::from(nanos / 1_000_000)))
}

fn list_objects(
    reader: &norn_cell::StoreReader<'_, NativeEngine>,
    object_type: &str,
) -> Result<Vec<norn_cell::ObjectRecord>, String> {
    let ids = reader
        .ids_after("main", object_type, None, 1_000)
        .map_err(|e| format!("ids_after {object_type}: {e}"))?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(obj) = reader
            .object("main", object_type, &id)
            .map_err(|e| format!("object {object_type}/{id}: {e}"))?
        {
            out.push(obj);
        }
    }
    Ok(out)
}

type BasisCount = (u64, String, Decimal);

fn collect_latest_counts(
    counts: &[norn_cell::ObjectRecord],
    now_ms: u64,
    keys: &mut BTreeSet<(String, String)>,
    findings: &mut Vec<CountCoverageFinding>,
) -> Result<BTreeMap<(String, String, String), BasisCount>, String> {
    let mut latest: BTreeMap<(String, String, String), BasisCount> = BTreeMap::new();
    for count in counts {
        let basis = count
            .values
            .get("quantity_basis")
            .map(scalar)
            .unwrap_or_default();
        if basis != "on_hand" && basis != "available" {
            continue;
        }
        let store_id = count.values.get("store_id").map(scalar).unwrap_or_default();
        let sku_id = count.values.get("sku_id").map(scalar).unwrap_or_default();
        let as_of = count.values.get("as_of").map(scalar).unwrap_or_default();
        let as_of_ms = parse_rfc3339_ms(&as_of)?;
        keys.insert((store_id.clone(), sku_id.clone()));

        if as_of_ms > now_ms {
            findings.push(CountCoverageFinding {
                store_id,
                sku_id,
                kind: CountCoverageKind::FutureCount {
                    basis,
                    count_as_of: as_of,
                },
            });
            continue;
        }
        let counted = if let Some(RuntimeValue::Number(d)) = count.values.get("on_hand_qty") {
            *d
        } else {
            Decimal::from(0_i64)
        };
        let bkey = (store_id, sku_id, basis);
        if latest
            .get(&bkey)
            .is_none_or(|(prev_ms, _, _)| as_of_ms >= *prev_ms)
        {
            latest.insert(bkey, (as_of_ms, as_of, counted));
        }
    }
    Ok(latest)
}

fn check_position_counts(
    (store_id, sku_id): (&str, &str),
    derived: Decimal,
    (on_hand, avail): (Option<&BasisCount>, Option<&BasisCount>),
    (now_ms, max_age_ms): (u64, u64),
    findings: &mut Vec<CountCoverageFinding>,
) {
    let has_future_on_hand = findings.iter().any(|f| {
        f.store_id == store_id
            && f.sku_id == sku_id
            && matches!(&f.kind, CountCoverageKind::FutureCount { basis, .. } if basis == "on_hand")
    });
    let has_future_avail = findings.iter().any(|f| {
        f.store_id == store_id
            && f.sku_id == sku_id
            && matches!(&f.kind, CountCoverageKind::FutureCount { basis, .. } if basis == "available")
    });
    let on_hand_qty = match on_hand {
        None => {
            if !has_future_on_hand && derived > Decimal::from(0_i64) {
                findings.push(CountCoverageFinding {
                    store_id: store_id.to_owned(),
                    sku_id: sku_id.to_owned(),
                    kind: CountCoverageKind::MissingOnHandCount,
                });
            }
            None
        }
        Some((as_of_ms, as_of, counted)) => {
            let age_ms = now_ms.saturating_sub(*as_of_ms);
            if age_ms > max_age_ms {
                findings.push(CountCoverageFinding {
                    store_id: store_id.to_owned(),
                    sku_id: sku_id.to_owned(),
                    kind: CountCoverageKind::StaleCount {
                        basis: "on_hand".to_owned(),
                        last_count_as_of: as_of.clone(),
                        age_ms,
                    },
                });
            }
            if *counted != derived {
                findings.push(CountCoverageFinding {
                    store_id: store_id.to_owned(),
                    sku_id: sku_id.to_owned(),
                    kind: CountCoverageKind::OnHandDrift {
                        counted_qty: counted.canonical(),
                        derived_lot_qty: derived.canonical(),
                    },
                });
            }
            Some(*counted)
        }
    };
    match avail {
        None => {
            if !has_future_avail && derived > Decimal::from(0_i64) {
                findings.push(CountCoverageFinding {
                    store_id: store_id.to_owned(),
                    sku_id: sku_id.to_owned(),
                    kind: CountCoverageKind::MissingAvailableCount,
                });
            }
        }
        Some((a_ms, a_as_of, a_qty)) => {
            let a_age = now_ms.saturating_sub(*a_ms);
            if a_age > max_age_ms {
                findings.push(CountCoverageFinding {
                    store_id: store_id.to_owned(),
                    sku_id: sku_id.to_owned(),
                    kind: CountCoverageKind::StaleCount {
                        basis: "available".to_owned(),
                        last_count_as_of: a_as_of.clone(),
                        age_ms: a_age,
                    },
                });
            }
            let ceiling = on_hand_qty.unwrap_or(derived);
            if *a_qty > ceiling {
                findings.push(CountCoverageFinding {
                    store_id: store_id.to_owned(),
                    sku_id: sku_id.to_owned(),
                    kind: CountCoverageKind::AvailableExceedsOnHand {
                        available_qty: a_qty.canonical(),
                        on_hand_qty: ceiling.canonical(),
                    },
                });
            }
        }
    }
}

/// Fail-closed physical count coverage & staleness check across all live holdings
/// and both `on_hand` and `available` `StoreCount`s.
fn evaluate_count_coverage(
    book: &EntityBook,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<CountCoverageReport, String> {
    evaluate_store_count_coverage(book.cell()?.store(), now_ms, max_age_ms)
}

fn evaluate_store_count_coverage(
    store: &CellStore<NativeEngine>,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<CountCoverageReport, String> {
    let reader = store.read();
    let mut derived_on_hand: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    for lot in &list_objects(&reader, "supply.StockLot")? {
        let store_id = lot.values.get("store_id").map(scalar).unwrap_or_default();
        let sku_id = lot.values.get("sku_id").map(scalar).unwrap_or_default();
        if let Some(RuntimeValue::Number(qty)) = lot.values.get("remaining_qty") {
            let entry = derived_on_hand
                .entry((store_id, sku_id))
                .or_insert_with(|| Decimal::from(0_i64));
            *entry = entry.checked_add(*qty).ok_or("lot sum overflow")?;
        }
    }

    let counts = list_objects(&reader, "supply.StoreCount")?;
    let mut findings = Vec::new();
    let mut keys: BTreeSet<(String, String)> = derived_on_hand
        .iter()
        .filter(|(_, qty)| **qty > Decimal::from(0_i64))
        .map(|(k, _)| k.clone())
        .collect();
    let latest = collect_latest_counts(&counts, now_ms, &mut keys, &mut findings)?;

    for (store_id, sku_id) in keys {
        let derived = derived_on_hand
            .get(&(store_id.clone(), sku_id.clone()))
            .copied()
            .unwrap_or_else(|| Decimal::from(0_i64));
        let on_hand = latest.get(&(store_id.clone(), sku_id.clone(), "on_hand".to_owned()));
        let avail = latest.get(&(store_id.clone(), sku_id.clone(), "available".to_owned()));
        check_position_counts(
            (&store_id, &sku_id),
            derived,
            (on_hand, avail),
            (now_ms, max_age_ms),
            &mut findings,
        );
    }

    let mut on_hand_issue_stores = BTreeSet::new();
    let mut availability_issue_stores = BTreeSet::new();
    for f in &findings {
        availability_issue_stores.insert(f.store_id.clone());
        let is_on_hand_issue = match &f.kind {
            CountCoverageKind::MissingOnHandCount | CountCoverageKind::OnHandDrift { .. } => true,
            CountCoverageKind::FutureCount { basis, .. }
            | CountCoverageKind::StaleCount { basis, .. } => basis == "on_hand",
            CountCoverageKind::MissingAvailableCount
            | CountCoverageKind::AvailableExceedsOnHand { .. } => false,
        };
        if is_on_hand_issue {
            on_hand_issue_stores.insert(f.store_id.clone());
        }
    }
    Ok(CountCoverageReport {
        findings,
        on_hand_issue_stores,
        availability_issue_stores,
    })
}

fn verify_retail_sales_and_margin_questions(
    sole: &EntityBook,
    s1_on_hand: Decimal,
    s1_total_received: Decimal,
) -> Result<(Decimal, usize), String> {
    let sales_q = sole.answer("supply.retail.store_sales_weekly")?;
    let mut s1_sold = Decimal::from(0_i64);
    let mut s1_sales_weeks = 0_usize;
    for row in sales_q.rows.values() {
        let parts = bucket_parts(&row.bucket);
        if parts.first().map(String::as_str) == Some("each")
            && parts.get(1).map(String::as_str) == Some("ST-SOLE-CFC-1")
            && let Some(qty) = row.value
        {
            s1_sold = s1_sold.checked_add(qty).ok_or("sold sum")?;
            s1_sales_weeks = s1_sales_weeks.saturating_add(1);
        }
    }
    assert_eq!(s1_sales_weeks, 4, "4 real weekly sales buckets W6..W9");
    assert_eq!(s1_sold.canonical(), "40");
    assert_eq!(
        s1_total_received
            .checked_sub(s1_sold)
            .ok_or("conservation")?,
        s1_on_hand
    );

    let rev_q = sole.answer("supply.retail.store_revenue_weekly")?;
    assert_eq!(
        bucket_total_exact(&rev_q, &["EGP", "ST-SOLE-CFC-1", "2026-08-31"]).as_deref(),
        Some("3000")
    );
    let gm_sum = sole.answer("supply.retail.store_gross_margin_weekly")?;
    let s1_w9_gm = resolve_costed_bucket(&gm_sum, "EGP", "ST-SOLE-CFC-1", "2026-08-31")
        .ok_or("missing ST-SOLE-CFC-1 W9 gross margin")?;
    assert_eq!(s1_w9_gm.canonical(), "2000");
    assert_eq!(
        resolve_costed_bucket(&gm_sum, "EGP", "ST-SOLE-CFC-2", "2026-08-31"),
        None
    );

    let gm_rate = sole.answer("supply.retail.store_gross_margin_rate_weekly")?;
    let s1_rate = resolve_costed_bucket(&gm_rate, "EGP", "ST-SOLE-CFC-1", "2026-08-31")
        .ok_or("missing ST-SOLE-CFC-1 gross margin rate")?;
    assert_eq!(
        s1_rate.canonical(),
        "0.666667",
        "W9 ratio-of-sums 2000/3000"
    );
    assert_eq!(
        resolve_costed_bucket(&gm_rate, "EGP", "ST-SOLE-CFC-2", "2026-08-31"),
        None
    );
    Ok((s1_sold, s1_sales_weeks))
}

fn resolve_costed_window_bucket(
    result: &QueryResult,
    currency: &str,
    store_id: &str,
) -> Option<Decimal> {
    if bucket_decimal_exact(result, &[currency, store_id, "missing_cost"]).is_some() {
        return None;
    }
    bucket_decimal_exact(result, &[currency, store_id, "costed"])
}

fn verify_opening_cut_and_lot_date_refusals(sole: &mut EntityBook) -> Result<(), String> {
    // 0a. Future receipt with backdated `as_of < received_at` refuses via `StockLotAsOfNotBeforeReceipt`.
    let (code, msg) = sole.refused(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-BACKDATED-ASOF", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "5", "remaining_qty": "5", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-09-20T09:00:00Z", "as_of": "2026-09-01T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    // 0b. Backdating `UpdateStockLotBalance.as_of` or unwinding `depleted_qty` refuses monotonically.
    let (code, msg) = sole.refused(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "25", "depleted_qty": "25",
            "as_of": "2026-08-09T18:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    let (code, msg) = sole.refused(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "30", "depleted_qty": "20",
            "as_of": "2026-09-05T18:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    // 0c. Opening cut after depletion refuses original receipt value (10000 != remaining 6000).
    let (code, msg) = sole.refused(
        "supply.stock_valuation_cut.record",
        "planner:omar",
        json!({
            "cut_id": "CUT-S1-FORGED-OPEN", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "currency": "EGP", "cost_status": "costed", "book_epoch": "1970-01-01T00:00:00Z",
            "cut_at": "2026-09-05T00:00:00Z", "stock_value": "10000"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    // 0c-bis. Isolated post-cutoff-sale test: all lots have `as_of <= cut_at` and matching
    // `stock_value = 5000`, so ONLY `sales_by_cut_count < all_sales_count` triggers refusal.
    verify_opening_cut_post_sales_refusal()?;
    // 0d. Rollup citing a nonexistent opening cut refuses fail-closed (forcing GMROI withheld).
    sole.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-NO-CUT", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-07", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-MISSING",
            "count_ref": "CC-102", "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-12T09:00:00Z",
            "sales_weeks": 5, "on_hand_qty": "60", "avg_weekly_sales_qty": "8",
            "weeks_of_supply": "0", "period_gross_margin": "5000",
            "opening_stock_value": "10000", "closing_stock_value": "6000",
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    Ok(())
}

fn verify_opening_cut_post_sales_refusal() -> Result<(), String> {
    let mut iso = EntityBook::open(
        SOLE_CELL_ID,
        "org_family_sole_prop",
        "prod",
        "sole-post-sale-cut.sqlite",
    )?;
    seed_sole_prop(&mut iso)?;
    iso.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-ISO-1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "50", "remaining_qty": "50", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-05T09:00:00Z", "as_of": "2026-08-05T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    iso.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-ISO-AFTER-CUT", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "2", "unit": "each", "net_revenue": "400", "applied_unit_cost": "100",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-08-12T14:00:00Z"
        }),
    )?;
    // Lot `as_of` (2026-08-05) <= `cut_at` (2026-08-10) and `stock_value` (5000) == exact sum,
    // so ONLY `sales_by_cut_count < all_sales_count` can trigger refusal.
    let (code, msg) = iso.refused(
        "supply.stock_valuation_cut.record",
        "planner:omar",
        json!({
            "cut_id": "CUT-ISO-POST-SALE", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "currency": "EGP", "cost_status": "costed", "book_epoch": "1970-01-01T00:00:00Z",
            "cut_at": "2026-08-10T00:00:00Z", "stock_value": "5000"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    Ok(())
}

fn verify_store_kpi_rollup_temporal_refusals(
    sole: &mut EntityBook,
    (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val): (Decimal, Decimal, Decimal, Decimal),
) -> Result<(), String> {
    verify_opening_cut_and_lot_date_refusals(sole)?;
    // 1. Non-Monday `period_week` passes all preconditions and refuses via `StoreCommercialRollupPeriodWeekMatchesEnd`.
    let (code, msg) = sole.refused(
        "supply.store_kpi.record_withheld",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-BAD-MON", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-01", "unit": "each", "currency": "EGP",
            "cost_status": "missing_cost", "count_status": "unverified",
            "period_start": "2026-08-11T00:00:00Z", "period_end": "2026-09-05T18:00:00Z",
            "sales_weeks": 4,
            "on_hand_qty": "0", "avg_weekly_sales_qty": "0", "weeks_of_supply": "0",
            "period_gross_margin": "0", "opening_stock_value": "0", "closing_stock_value": "0",
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    assert!(
        msg.contains("store_commercial_rollup_period_week_matches_end"),
        "{msg}"
    );
    // 2. Wrong-week Monday string (`2026-08-17`) matching `CUT-S1-2026-08-10` and `2 * 20 = 40`
    // passes all preconditions and refuses via `StoreCommercialRollupPeriodWeekMatchesEnd`.
    let (code, msg) = sole.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-WRONG-WK", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-08-17", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
            "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-05T18:00:00Z",
            "sales_weeks": 2,
            "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": "20",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    assert!(
        msg.contains("store_commercial_rollup_period_week_matches_end"),
        "{msg}"
    );
    verify_store_kpi_rollup_stale_and_backdate_refusals(
        sole,
        (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val),
    )
}

fn verify_store_kpi_rollup_stale_and_backdate_refusals(
    sole: &mut EntityBook,
    (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val): (Decimal, Decimal, Decimal, Decimal),
) -> Result<(), String> {
    // 3. Stale physical count (>7d before `period_end`) refuses via `StoreCommercialRollupCountNotFuture`.
    let (code, msg) = sole.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-STALE-CNT", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-28", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
            "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-29T18:00:00Z",
            "sales_weeks": 8,
            "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": "5",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    assert!(
        msg.contains("store_commercial_rollup_count_not_future"),
        "{msg}"
    );
    // 4. Backdated `count_as_of` disagreeing with linked `StoreCount.as_of` refuses at write boundary.
    let (code, msg) = sole.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-BACKDATE", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-07", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
            "count_as_of": "2026-09-04T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-12T09:00:00Z",
            "sales_weeks": 5,
            "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": "8",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    verify_monotonic_count_and_cutoff_refusals((
        s1_on_hand,
        s1_margin,
        s1_opening_val,
        s1_closing_val,
    ))
}

fn verify_monotonic_count_and_cutoff_refusals(
    (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val): (Decimal, Decimal, Decimal, Decimal),
) -> Result<(), String> {
    // Isolated scratch book avoids rolling back monotonic `as_of` timestamps on the main book.
    let mut scratch = EntityBook::open(
        SOLE_CELL_ID,
        "org_family_sole_prop",
        "prod",
        "sole-monotonic-refusals.sqlite",
    )?;
    seed_sole_prop(&mut scratch)?;
    seed_sole_cfc1(&mut scratch)?;
    // 1. Forward recount after a historical rollup succeeds (recounts stay unlocked),
    //    while drifted count (`61 != derived 60`) refuses `supply.store_kpi.record`.
    scratch.ok(
        "supply.store_count.recount",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1",
            "sku_id": "SKU-DRESS", "on_hand_qty": "61", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-05T10:30:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-102B"
        }),
    )?;
    let (code, msg) = scratch.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-DRIFTED-CNT", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-08-31", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102B",
            "count_as_of": "2026-09-05T10:30:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-05T18:00:00Z",
            "sales_weeks": 4, "on_hand_qty": "61", "avg_weekly_sales_qty": "10",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    // 2. Backdating `RecountStore.as_of` below current `count.as_of` refuses monotonically.
    let (code, msg) = scratch.refused(
        "supply.store_count.recount",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1",
            "sku_id": "SKU-DRESS", "on_hand_qty": "60", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-05T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-102-BACK"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    verify_monotonic_future_and_cutoff_refusals(
        &mut scratch,
        (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val),
    )
}

fn verify_monotonic_future_and_cutoff_refusals(
    scratch: &mut EntityBook,
    (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val): (Decimal, Decimal, Decimal, Decimal),
) -> Result<(), String> {
    // 3. Future `count_as_of > period_end` refuses via `StoreCommercialRollupCountNotFuture`.
    scratch.ok(
        "supply.store_count.recount",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1",
            "sku_id": "SKU-DRESS", "on_hand_qty": "60", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-13T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-102-FUT"
        }),
    )?;
    let (code, msg) = scratch.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-FUTURE-CNT", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-07", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102-FUT",
            "count_as_of": "2026-09-13T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-12T09:00:00Z",
            "sales_weeks": 5, "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": "8",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.laws.violated", "{msg}");
    // 4. Post-cutoff lot balance update (`as_of > period_end`) refuses GMROI closing valuation.
    scratch.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "24", "depleted_qty": "26",
            "as_of": "2026-09-13T09:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )?;
    let (code, msg) = scratch.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-POST-CUTOFF-LOT", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-07", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102-FUT",
            "count_as_of": "2026-09-13T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-12T09:00:00Z",
            "sales_weeks": 5, "on_hand_qty": "59", "avg_weekly_sales_qty": "8",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(), "closing_stock_value": "5900",
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    Ok(())
}

fn verify_store_kpi_rollup_operand_refusals(
    sole: &mut EntityBook,
    (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val): (Decimal, Decimal, Decimal, Decimal),
) -> Result<(), String> {
    let cases = [
        (
            "KPI-ST-SOLE-CFC-1-FORGED-AVG",
            5_i64,
            "9",
            s1_margin.canonical(),
            s1_opening_val.canonical(),
            s1_closing_val.canonical(),
        ),
        (
            "KPI-ST-SOLE-CFC-1-WRONG-WKS",
            4_i64,
            "10",
            s1_margin.canonical(),
            s1_opening_val.canonical(),
            s1_closing_val.canonical(),
        ),
        (
            "KPI-ST-SOLE-CFC-1-FORGED-GM",
            5_i64,
            "8",
            "9999".to_owned(),
            s1_opening_val.canonical(),
            s1_closing_val.canonical(),
        ),
        (
            "KPI-ST-SOLE-CFC-1-OMIT-CARRIED-IN",
            5_i64,
            "8",
            s1_margin.canonical(),
            "0".to_owned(),
            s1_closing_val.canonical(),
        ),
        (
            "KPI-ST-SOLE-CFC-1-FORGED-CLOSE",
            5_i64,
            "8",
            s1_margin.canonical(),
            s1_opening_val.canonical(),
            "8000".to_owned(),
        ),
    ];
    for (rollup_id, sales_weeks, avg_sales, margin, open_val, close_val) in cases {
        let (code, msg) = sole.refused(
            "supply.store_kpi.record",
            "planner:omar",
            json!({
                "rollup_id": rollup_id, "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
                "period_week": "2026-09-07", "unit": "each", "currency": "EGP",
                "cost_status": "costed", "count_status": "verified",
                "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
                "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
                "book_epoch": "1970-01-01T00:00:00Z",
                "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-12T18:00:00Z",
                "sales_weeks": sales_weeks,
                "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": avg_sales,
                "weeks_of_supply": "0", "period_gross_margin": margin,
                "opening_stock_value": open_val, "closing_stock_value": close_val,
                "average_inventory_value": "0", "gmroi": "0"
            }),
        )?;
        assert_eq!(code, "core.action.precondition", "{rollup_id}: {msg}");
    }
    Ok(())
}

fn verify_store_kpi_rollup_refusals(
    sole: &mut EntityBook,
    (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val): (Decimal, Decimal, Decimal, Decimal),
) -> Result<(), String> {
    // 1. Forged on_hand_qty disagreeing with governed StoreCount refuses at write boundary.
    let (code, msg) = sole.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-FORGED", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-09-07", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
            "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-12T18:00:00Z",
            "sales_weeks": 5,
            "on_hand_qty": "99", "avg_weekly_sales_qty": "8",
            "weeks_of_supply": "0", "period_gross_margin": "5000",
            "opening_stock_value": "10000", "closing_stock_value": "6000",
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    verify_store_kpi_rollup_operand_refusals(
        sole,
        (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val),
    )?;
    // 2. Duplicate `(store_id, period_week)` under a different `period_end` timestamp refuses.
    sole.refused(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-W9-DUP", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-08-31", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
            "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-06T18:00:00Z",
            "sales_weeks": 4,
            "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": "10",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    verify_store_kpi_rollup_temporal_refusals(
        sole,
        (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val),
    )
}

fn seed_partner_commercial_rollup(partner: &mut EntityBook) -> Result<(), String> {
    let p1_opening_val = resolve_costed_window_bucket(
        &partner.answer("supply.retail.store_opening_stock_value_weekly")?,
        "USD",
        "ST-PART-CFC-1",
    )
    .ok_or("missing ST-PART-CFC-1 opening_stock_value")?;
    let p1_closing_val = resolve_costed_bucket(
        &partner.answer("supply.retail.store_stock_value_weekly")?,
        "USD",
        "ST-PART-CFC-1",
        "2026-08-31",
    )
    .ok_or("missing ST-PART-CFC-1 closing stock_value")?;
    partner.ok(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-PART-CFC-1-W9", "store_id": "ST-PART-CFC-1",
            "sku_id": "SKU-RING-18K", "period_week": "2026-08-31",
            "unit": "each", "currency": "USD",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-PART-CFC-1/SKU-RING-18K/on_hand", "opening_cut_id": "CUT-P1-2026-08-31", "count_ref": "AUD-P2",
            "count_as_of": "2026-09-06T10:00:00Z", "costed_ref": "COST-RING-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-31T00:00:00Z", "period_end": "2026-09-06T23:59:59.999999999Z",
            "sales_weeks": 1,
            "on_hand_qty": "12", "avg_weekly_sales_qty": "8", "weeks_of_supply": "0",
            "period_gross_margin": "1200",
            "opening_stock_value": p1_opening_val.canonical(),
            "closing_stock_value": p1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )
}

/// Seed governed store-week `StoreCommercialRollup` rows derived strictly from
/// each cell's own evaluated questions (`on_hand`, `sold/weeks`, `margin`,
/// immutable lot-level `opening_stock_value`, closing `stock_value`).
fn seed_store_commercial_rollups(
    sole: &mut EntityBook,
    partner: &mut EntityBook,
) -> Result<(), String> {
    let s1_on_hand = bucket_decimal_exact(
        &sole.answer("supply.retail.store_on_hand_weekly")?,
        &["each", "ST-SOLE-CFC-1", "2026-08-31"],
    )
    .ok_or("missing ST-SOLE-CFC-1 on_hand")?;
    let s1_opening_val = resolve_costed_window_bucket(
        &sole.answer("supply.retail.store_opening_stock_value_weekly")?,
        "EGP",
        "ST-SOLE-CFC-1",
    )
    .ok_or("missing ST-SOLE-CFC-1 opening_stock_value")?;
    let s1_closing_val = resolve_costed_bucket(
        &sole.answer("supply.retail.store_stock_value_weekly")?,
        "EGP",
        "ST-SOLE-CFC-1",
        "2026-08-31",
    )
    .ok_or("missing ST-SOLE-CFC-1 closing stock_value")?;
    let sales_tf = sole.evaluate_transform("supply.retail.store_sales_performance", |_| {
        Ok(RowAccess::Read)
    })?;
    let s1_margin = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1"))
        .and_then(|r| {
            if let Some(RuntimeValue::Number(d)) = r.values.get("effective_gross_margin") {
                Some(*d)
            } else {
                None
            }
        })
        .ok_or("missing ST-SOLE-CFC-1 effective_gross_margin")?;
    sole.ok(
        "supply.store_kpi.record",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-1-W9", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "period_week": "2026-08-31", "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "opening_cut_id": "CUT-S1-2026-08-10", "count_ref": "CC-102",
            "count_as_of": "2026-09-05T10:00:00Z", "costed_ref": "COST-DRESS-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-10T00:00:00Z", "period_end": "2026-09-05T18:00:00Z",
            "sales_weeks": 4,
            "on_hand_qty": s1_on_hand.canonical(), "avg_weekly_sales_qty": "10",
            "weeks_of_supply": "0", "period_gross_margin": s1_margin.canonical(),
            "opening_stock_value": s1_opening_val.canonical(),
            "closing_stock_value": s1_closing_val.canonical(),
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    verify_store_kpi_rollup_refusals(
        sole,
        (s1_on_hand, s1_margin, s1_opening_val, s1_closing_val),
    )?;
    // Mixed-coverage store `ST-SOLE-CFC-2` (costed + missing_cost lots, unverified count):
    // unique `(store_id, period_week)` grain records ONE withheld rollup so the declared
    // dashboard tile returns ONLY `None` for the entire store-week (zero partial numeric bucket).
    sole.ok(
        "supply.store_kpi.record_withheld",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-SOLE-CFC-2-W9", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "period_week": "2026-08-31", "unit": "each", "currency": "EGP",
            "cost_status": "missing_cost", "count_status": "unverified",
            "period_start": "2026-08-31T00:00:00Z", "period_end": "2026-09-05T18:00:00Z",
            "sales_weeks": 1,
            "on_hand_qty": "0", "avg_weekly_sales_qty": "0", "weeks_of_supply": "0",
            "period_gross_margin": "0", "opening_stock_value": "0",
            "closing_stock_value": "0", "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    seed_partner_commercial_rollup(partner)
}

fn verify_declared_kpi_ratio_questions(sole: &EntityBook) -> Result<(), String> {
    let open_val_q = sole.answer("supply.retail.store_opening_stock_value_weekly")?;
    let s1_open_val = resolve_costed_window_bucket(&open_val_q, "EGP", "ST-SOLE-CFC-1")
        .ok_or("missing ST-SOLE-CFC-1 opening_stock_value")?;
    assert_eq!(s1_open_val.canonical(), "10000");
    assert_eq!(
        resolve_costed_window_bucket(&open_val_q, "EGP", "ST-SOLE-CFC-2"),
        None
    );

    // Declared sell-through question: ratio of sums `sum(depleted_qty) / sum(received_qty)` = 40/100 = 0.4.
    let sell_through = sole.answer("supply.retail.store_sell_through_weekly")?;
    assert_eq!(
        bucket_total_exact(&sell_through, &["each", "ST-SOLE-CFC-1"]).as_deref(),
        Some("0.4")
    );

    // Declared WOS & GMROI questions at unique store-SKU-week grain: `ST-SOLE-CFC-2` has ONLY ONE
    // bucket for W9 and its value is `None` directly on the declared read path (zero partial split).
    let wos_q = sole.answer("supply.retail.store_weeks_of_supply_weekly")?;
    assert_eq!(
        bucket_decimal_exact(
            &wos_q,
            &["each", "ST-SOLE-CFC-1", "SKU-DRESS", "2026-08-31"]
        )
        .map(|d| d.canonical())
        .as_deref(),
        Some("6")
    );
    let s2_wos_rows: Vec<_> = wos_q
        .rows
        .values()
        .filter(|r| bucket_parts(&r.bucket).get(1).map(String::as_str) == Some("ST-SOLE-CFC-2"))
        .collect();
    assert_eq!(s2_wos_rows.len(), 1);
    assert_eq!(
        s2_wos_rows.first().and_then(|r| r.value),
        None,
        "mixed/unverified store-SKU-week WOS must withhold (None) directly on the declared read path"
    );

    let gmroi_q = sole.answer("supply.retail.store_gmroi_weekly")?;
    assert_eq!(
        bucket_decimal_exact(
            &gmroi_q,
            &["EGP", "ST-SOLE-CFC-1", "SKU-DRESS", "2026-08-31"]
        )
        .map(|d| d.canonical())
        .as_deref(),
        Some("0.625")
    );
    let s2_gmroi_rows: Vec<_> = gmroi_q
        .rows
        .values()
        .filter(|r| bucket_parts(&r.bucket).get(1).map(String::as_str) == Some("ST-SOLE-CFC-2"))
        .collect();
    assert_eq!(s2_gmroi_rows.len(), 1);
    assert_eq!(
        s2_gmroi_rows.first().and_then(|r| r.value),
        None,
        "mixed/missing_cost store-week GMROI must withhold (None) directly on the declared read path"
    );
    Ok(())
}

fn verify_retail_questions(
    sole: &EntityBook,
) -> Result<(Decimal, Decimal, Decimal, Decimal, usize), String> {
    let availability = sole.answer("supply.retail.store_availability_weekly")?;
    assert_eq!(
        bucket_total_exact(&availability, &["each", "ST-SOLE-CFC-1", "2026-08-31"]).as_deref(),
        Some("56")
    );
    let on_hand = sole.answer("supply.retail.store_on_hand_weekly")?;
    let s1_on_hand = bucket_decimal_exact(&on_hand, &["each", "ST-SOLE-CFC-1", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 on_hand")?;
    assert_eq!(s1_on_hand.canonical(), "60");

    let stock_val = sole.answer("supply.retail.store_stock_value_weekly")?;
    let s1_stock_val = resolve_costed_bucket(&stock_val, "EGP", "ST-SOLE-CFC-1", "2026-08-31")
        .ok_or("missing ST-SOLE-CFC-1 stock_value")?;
    assert_eq!(s1_stock_val.canonical(), "6000");
    assert_eq!(
        resolve_costed_bucket(&stock_val, "EGP", "ST-SOLE-CFC-2", "2026-08-31"),
        None
    );
    verify_declared_kpi_ratio_questions(sole)?;

    let aging_q = sole.answer("supply.retail.lot_received_aging_weekly")?;
    assert_eq!(
        bucket_total_exact(&aging_q, &["each", "ST-SOLE-CFC-1", "2026-07-06"]).as_deref(),
        Some("50")
    );
    assert_eq!(
        bucket_total_exact(&aging_q, &["each", "ST-SOLE-CFC-1", "2026-07-13"]).as_deref(),
        Some("10")
    );
    assert_eq!(
        bucket_total_exact(&aging_q, &["each", "ST-SOLE-CFC-1", "2026-08-03"]).as_deref(),
        Some("40")
    );
    let mut s1_total_received = Decimal::from(0_i64);
    for row in aging_q.rows.values() {
        let parts = bucket_parts(&row.bucket);
        if parts.first().map(String::as_str) == Some("each")
            && parts.get(1).map(String::as_str) == Some("ST-SOLE-CFC-1")
            && let Some(qty) = row.value
        {
            s1_total_received = s1_total_received.checked_add(qty).ok_or("received sum")?;
        }
    }
    assert_eq!(s1_total_received.canonical(), "100");

    let (s1_sold, s1_sales_weeks) =
        verify_retail_sales_and_margin_questions(sole, s1_on_hand, s1_total_received)?;
    Ok((
        s1_on_hand,
        s1_stock_val,
        s1_total_received,
        s1_sold,
        s1_sales_weeks,
    ))
}

fn verify_transform_withholding(
    sole: &EntityBook,
    sales_tf: &TransformResult,
    aging_tf: &TransformResult,
) -> Result<(), String> {
    let s2_sales = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-2"))
        .ok_or("missing ST-SOLE-CFC-2 sales row")?;
    assert_eq!(
        s2_sales.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        s2_sales.values.get("effective_cogs"),
        Some(&RuntimeValue::Null)
    );
    assert_eq!(
        s2_sales.values.get("effective_gross_margin"),
        Some(&RuntimeValue::Null)
    );
    let s2_lot = aging_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-2"))
        .ok_or("missing ST-SOLE-CFC-2 lot row")?;
    assert_eq!(
        s2_lot.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(false))
    );

    let withheld_tf = sole.evaluate_transform("supply.retail.store_sales_performance", |obj| {
        if obj.object_type == "supply.PosSale" && obj.id == "SALE-S1-2" {
            Ok(RowAccess::Count)
        } else {
            Ok(RowAccess::Read)
        }
    })?;
    assert!(!withheld_tf.complete);
    assert_eq!(withheld_tf.withheld.get("sale").copied(), Some(1));
    Ok(())
}

fn verify_retail_kpi_ratios(
    sole: &EntityBook,
    (s1_on_hand, s1_stock_val, s1_margin): (Decimal, Decimal, Decimal),
    (s1_total_received, s1_sold, s1_sales_weeks): (Decimal, Decimal, usize),
) -> Result<(), String> {
    // Aligned KPIs from complete cohorts & real 4-week sales cadence:
    // - Sell-through over complete received cohort (including depleted W2): 40 / 100 = 0.400000
    assert_eq!(
        ratio(
            decimal_to_scaled_i128(s1_sold)?,
            decimal_to_scaled_i128(s1_total_received)?
        ),
        Some(400_000)
    );
    // - WOS from 4 real weekly sales buckets: 40 / 4 = 10/wk -> 60 / 10 = 6.000000 weeks
    let weeks_i128 = i128::try_from(s1_sales_weeks).map_err(|_| "weeks overflow")?;
    let weekly_sales = ratio(
        decimal_to_scaled_i128(s1_sold)?,
        weeks_i128.saturating_mul(FACTOR),
    )
    .ok_or("weekly")?;
    assert_eq!(
        ratio(decimal_to_scaled_i128(s1_on_hand)?, weekly_sales),
        Some(6_000_000)
    );
    // - Average-inventory GMROI using immutable lot-level receipt valuation from
    //   `store_opening_stock_value_weekly` (opening 10000, closing 6000 -> avg 8000): 5000 / 8000 = 0.625000
    let opening_val = resolve_costed_window_bucket(
        &sole.answer("supply.retail.store_opening_stock_value_weekly")?,
        "EGP",
        "ST-SOLE-CFC-1",
    )
    .ok_or("missing ST-SOLE-CFC-1 opening_stock_value")?;
    let avg_inventory = decimal_to_scaled_i128(opening_val)?
        .checked_add(decimal_to_scaled_i128(s1_stock_val)?)
        .and_then(|s| s.checked_div(2))
        .ok_or("avg inventory overflow")?;
    assert_eq!(
        ratio(decimal_to_scaled_i128(s1_margin)?, avg_inventory),
        Some(625_000)
    );
    // - Closing-valuation GMROI: 5000 / 6000 = 0.833333
    assert_eq!(
        ratio(
            decimal_to_scaled_i128(s1_margin)?,
            decimal_to_scaled_i128(s1_stock_val)?
        ),
        Some(833_333)
    );
    Ok(())
}

fn verify_retail_transforms_and_kpis(
    sole: &EntityBook,
    (s1_on_hand, s1_stock_val): (Decimal, Decimal),
    (s1_total_received, s1_sold, s1_sales_weeks): (Decimal, Decimal, usize),
) -> Result<(), String> {
    let sales_tf = sole.evaluate_transform("supply.retail.store_sales_performance", |_| {
        Ok(RowAccess::Read)
    })?;
    assert!(sales_tf.complete);
    let s1_sales = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1"))
        .ok_or("missing ST-SOLE-CFC-1 sales row")?;
    assert_eq!(
        s1_sales.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(true))
    );
    let s1_margin =
        if let Some(RuntimeValue::Number(d)) = s1_sales.values.get("effective_gross_margin") {
            *d
        } else {
            return Err(format!(
                "expected numeric margin, got {:?}",
                s1_sales.values.get("effective_gross_margin")
            ));
        };
    assert_eq!(s1_margin.canonical(), "5000");

    // Active lot aging (`remaining_qty > 0`): depleted W2 lot (`2026-07-15`, ISO week `2026-07-13`)
    // drops out, leaving only active W1 (`2026-07-06`, 50) + W5 (`2026-08-03`, 40) = 90 received.
    let aging_tf =
        sole.evaluate_transform("supply.retail.store_lot_aging", |_| Ok(RowAccess::Read))?;
    assert!(
        !aging_tf.rows.iter().any(|r| {
            r.values
                .get("received_at")
                .map(scalar)
                .is_some_and(|wk| wk.starts_with("2026-07-13"))
                || r.values
                    .get("oldest_received_at")
                    .map(scalar)
                    .is_some_and(|ts| ts.starts_with("2026-07-15"))
        }),
        "fully depleted W2 lot must not appear in active lot aging cohorts"
    );
    let mut s1_active_received = Decimal::from(0_i64);
    let mut s1_active_weeks = BTreeSet::new();
    for row in aging_tf
        .rows
        .iter()
        .filter(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1"))
    {
        if let Some(wk) = row.values.get("received_at").map(scalar) {
            s1_active_weeks.insert(wk);
        }
        if let Some(RuntimeValue::Number(qty)) = row.values.get("received_qty") {
            s1_active_received = s1_active_received
                .checked_add(*qty)
                .ok_or("active received sum")?;
        }
    }
    assert_eq!(s1_active_received.canonical(), "90");
    assert!(s1_active_weeks.iter().any(|w| w.starts_with("2026-07-06")));
    assert!(s1_active_weeks.iter().any(|w| w.starts_with("2026-08-03")));

    verify_retail_kpi_ratios(
        sole,
        (s1_on_hand, s1_stock_val, s1_margin),
        (s1_total_received, s1_sold, s1_sales_weeks),
    )?;
    verify_transform_withholding(sole, &sales_tf, &aging_tf)
}

fn verify_phantom_availability_refused(sole: &mut EntityBook) -> Result<(), String> {
    sole.ok(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-SOLE-PHANTOM", "name": "Phantom Avail Branch", "entity_id": "LE-SOLE",
            "custodian_id": "CUST-BRANCH-ZAM", "location_kind": "standalone",
            "mall_code": null, "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-PHANTOM/SKU-DRESS/available",
            "store_id": "ST-SOLE-PHANTOM", "sku_id": "SKU-DRESS", "on_hand_qty": "5",
            "unit": "each", "quantity_basis": "available", "as_of": "2026-09-06T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-PHANTOM"
        }),
    )?;
    let report = evaluate_count_coverage(sole, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    assert!(report.availability_issue_stores.contains("ST-SOLE-PHANTOM"));
    assert!(report.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-PHANTOM"
            && matches!(
                &f.kind,
                CountCoverageKind::AvailableExceedsOnHand {
                    available_qty,
                    on_hand_qty
                } if available_qty == "5" && on_hand_qty == "0"
            )
    }));
    Ok(())
}

fn verify_count_coverage_cases(sole: &mut EntityBook) -> Result<(), String> {
    let report = evaluate_count_coverage(sole, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.store_id == "ST-SOLE-CFC-1"),
        "{report:?}"
    );
    assert!(report.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-CFC-2" && matches!(f.kind, CountCoverageKind::MissingOnHandCount)
    }));
    assert!(report.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-CFC-2" && matches!(f.kind, CountCoverageKind::MissingAvailableCount)
    }));
    assert!(report.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-CFC-3" && matches!(f.kind, CountCoverageKind::StaleCount { .. })
    }));
    assert!(report.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-CFC-3" && matches!(f.kind, CountCoverageKind::OnHandDrift { .. })
    }));
    assert!(report.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-ZAM" && matches!(f.kind, CountCoverageKind::FutureCount { .. })
    }));

    sole.ok(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-SOLE-NO-AVAIL", "name": "OnHand Only Branch", "entity_id": "LE-SOLE",
            "custodian_id": "CUST-BRANCH-ZAM", "location_kind": "standalone",
            "mall_code": null, "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-NO-AVAIL", "store_id": "ST-SOLE-NO-AVAIL", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-BRANCH-ZAM",
            "received_qty": "10", "remaining_qty": "10", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-25T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-NO-AVAIL/SKU-DRESS/on_hand", "store_id": "ST-SOLE-NO-AVAIL",
            "sku_id": "SKU-DRESS", "on_hand_qty": "10", "unit": "each",
            "quantity_basis": "on_hand", "as_of": "2026-09-06T10:00:00Z",
            "count_source": "pos_cycle_count", "count_ref": "CC-NOAVAIL"
        }),
    )?;
    let no_avail = evaluate_count_coverage(sole, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    assert!(!no_avail.on_hand_issue_stores.contains("ST-SOLE-NO-AVAIL"));
    assert!(
        no_avail
            .availability_issue_stores
            .contains("ST-SOLE-NO-AVAIL")
    );
    assert!(no_avail.findings.iter().any(|f| {
        f.store_id == "ST-SOLE-NO-AVAIL"
            && matches!(f.kind, CountCoverageKind::MissingAvailableCount)
    }));
    verify_phantom_availability_refused(sole)
}

#[test]
fn s1_holding_lot_aging_count_staleness_and_commercial_kpis_withholding() -> Result<(), String> {
    let mut sole = EntityBook::open(SOLE_CELL_ID, "org_family_sole_prop", "prod", "sole.sqlite")?;
    let mut partner = EntityBook::open(
        PARTNER_CELL_ID,
        "org_family_partnership",
        "prod",
        "partner.sqlite",
    )?;
    seed_sole_prop(&mut sole)?;
    seed_partnership(&mut partner)?;
    seed_retail_operations(&mut sole, &mut partner)?;
    seed_store_commercial_rollups(&mut sole, &mut partner)?;

    sole.refused(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "20", "depleted_qty": "30",
            "as_of": "2026-09-06T09:00:00Z", "unit_cost": "100", "stock_value": "0",
            "received_at": "2026-09-01T09:00:00Z"
        }),
    )?;

    let (s1_on_hand, s1_stock_val, s1_total_received, s1_sold, s1_sales_weeks) =
        verify_retail_questions(&sole)?;
    verify_retail_transforms_and_kpis(
        &sole,
        (s1_on_hand, s1_stock_val),
        (s1_total_received, s1_sold, s1_sales_weeks),
    )?;
    verify_count_coverage_cases(&mut sole)
}

// ------------------------------------------------------------------ Test 4

/// Per-cell frontier & coverage metadata pinned to ONE verified cut (`READ-LOCAL-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct EntityFrontierSnapshot {
    cell_id: String,
    legal_entity_id: String,
    book_code: String,
    currency: String,
    scope: String,
    world: String,
    revision: u64,
    ledger_seq: u64,
    ledger_hash: String,
    definition_version: u64,
    definition_pin: String,
    source_cursors: Frontier,
    watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
    complete: bool,
    open_feed_findings: usize,
    pending_inbox_quarantined: usize,
    missing_feed_coverage: BTreeSet<String>,
    missing_cost_stores: BTreeSet<String>,
    on_hand_issue_stores: BTreeSet<String>,
    availability_issue_stores: BTreeSet<String>,
    entity_net_revenue: Option<String>,
    entity_gross_margin: Option<String>,
    /// Direct per-store gross-margin-rate outputs from `store_gross_margin_rate_weekly`
    /// (exercising DuckDB's actual ratio-of-sums evaluation, withheld on missing cost).
    store_gross_margin_rates: BTreeMap<String, Option<String>>,
    entity_pos_feed_revenue: Option<String>,
    entity_on_hand_verified: Option<bool>,
    entity_availability_verified: Option<bool>,
}

/// Read-only combined management view across multiple legal-entity cells.
/// Carries no cell handles or write authority; explicitly NOT consolidated
/// financial statements.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CombinedManagementView {
    view_kind: &'static str,
    is_consolidated_financial_statement: bool,
    read_only: bool,
    entities: Vec<EntityFrontierSnapshot>,
}

/// Count BOTH open evidence findings AND unconsumed pending inbox rows (mapping refusals
/// that produced no object row).
fn count_open_feed_issues(book: &EntityBook) -> Result<(usize, usize), String> {
    let reader = book.cell()?.store().read();
    let mut open = 0_usize;
    for ty in EVIDENCE_TYPES {
        let ids = reader
            .ids_after("main", ty, None, 1_000)
            .map_err(|e| format!("ids_after {ty}: {e}"))?;
        for id in ids {
            let found = reader
                .observation_findings("main", ty, &id)
                .map_err(|e| format!("findings {ty}/{id}: {e}"))?;
            open = open.saturating_add(
                found
                    .into_iter()
                    .filter(|f| matches!(f.status, DataQualityFindingStatus::Open))
                    .count(),
            );
        }
    }
    let pending_inbox = reader
        .pending_observations()
        .map_err(|e| format!("pending_observations: {e}"))?
        .len();
    Ok((open, pending_inbox))
}

fn ingest_baseline_entity_feeds(
    sole: &mut EntityBook,
    partner: &mut EntityBook,
) -> Result<(), String> {
    let sole_rows = [
        FeedRow {
            feed_id: "supply.pos_sales",
            mapping: "supply.pos_sales.mapping",
            dataset: "pos_sale_line",
            source: "synthetic-pos",
            event: "ev-sole-pos-1",
            record: "POS-SOLE-1",
            payload: r#"{"pos_line_id":"POS-SOLE-1","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":30,"uom":"PCS","net_amount":6000,"ccy":"EGP","sold_at":"2026-09-01T14:00:00Z"}"#,
        },
        FeedRow {
            feed_id: "supply.store_counts",
            mapping: "supply.store_counts.mapping",
            dataset: "store_count",
            source: "synthetic-store-count",
            event: "ev-sole-cnt-1",
            record: "SC-SOLE-1",
            payload: r#"{"count_id":"SC-SOLE-1","store_code":"ST-SOLE-CFC-1","sku_code":"SKU-DRESS","counted_qty":60,"uom":"EA","basis":"on_hand","counted_at":"2026-09-05T10:00:00Z"}"#,
        },
        FeedRow {
            feed_id: "supply.retail_receipts",
            mapping: "supply.retail_receipts.mapping",
            dataset: "retail_receipt",
            source: "synthetic-retail-receipt",
            event: "ev-sole-rr-1",
            record: "RR-SOLE-1",
            payload: r#"{"receipt_line_id":"RR-SOLE-1","store_code":"ST-SOLE-CFC-1","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","received_qty":50,"damaged_qty":0,"uom":"PCS","unit_cost":100,"ccy":"EGP","received_at":"2026-07-08T09:00:00Z"}"#,
        },
        FeedRow {
            feed_id: "supply.unit_costs",
            mapping: "supply.unit_costs.mapping",
            dataset: "unit_cost",
            source: "synthetic-cost",
            event: "ev-sole-cost-1",
            record: "COST-SOLE-1",
            payload: r#"{"cost_id":"COST-SOLE-1","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","unit_cost":100,"ccy":"EGP","effective_from":"2026-07-01T00:00:00Z"}"#,
        },
    ];
    for row in &sole_rows {
        let (_, clean) = sole.ingest_feed(row)?;
        assert!(clean, "baseline feed {} must ingest cleanly", row.record);
    }

    let partner_rows = [
        FeedRow {
            feed_id: "supply.pos_sales",
            mapping: "supply.pos_sales.mapping",
            dataset: "pos_sale_line",
            source: "synthetic-pos",
            event: "ev-part-pos-1",
            record: "POS-PART-1",
            payload: r#"{"pos_line_id":"POS-PART-1","branch_code":"ST-PART-CFC-1","barcode_sku":"SKU-RING-18K","qty":8,"uom":"PCS","net_amount":3200,"ccy":"USD","sold_at":"2026-09-02T15:00:00Z"}"#,
        },
        FeedRow {
            feed_id: "supply.store_counts",
            mapping: "supply.store_counts.mapping",
            dataset: "store_count",
            source: "synthetic-store-count",
            event: "ev-part-cnt-1",
            record: "SC-PART-1",
            payload: r#"{"count_id":"SC-PART-1","store_code":"ST-PART-CFC-1","sku_code":"SKU-RING-18K","counted_qty":12,"uom":"EA","basis":"on_hand","counted_at":"2026-09-06T10:00:00Z"}"#,
        },
        FeedRow {
            feed_id: "supply.retail_receipts",
            mapping: "supply.retail_receipts.mapping",
            dataset: "retail_receipt",
            source: "synthetic-retail-receipt",
            event: "ev-part-rr-1",
            record: "RR-PART-1",
            payload: r#"{"receipt_line_id":"RR-PART-1","store_code":"ST-PART-CFC-1","sku_code":"SKU-RING-18K","vendor_code":"GOLD-09","received_qty":20,"damaged_qty":0,"uom":"PCS","unit_cost":250,"ccy":"USD","received_at":"2026-07-22T09:00:00Z"}"#,
        },
        FeedRow {
            feed_id: "supply.unit_costs",
            mapping: "supply.unit_costs.mapping",
            dataset: "unit_cost",
            source: "synthetic-cost",
            event: "ev-part-cost-1",
            record: "COST-PART-1",
            payload: r#"{"cost_id":"COST-PART-1","sku_code":"SKU-RING-18K","vendor_code":"GOLD-09","unit_cost":250,"ccy":"USD","effective_from":"2026-07-01T00:00:00Z"}"#,
        },
    ];
    for row in &partner_rows {
        let (_, clean) = partner.ingest_feed(row)?;
        assert!(clean, "baseline feed {} must ingest cleanly", row.record);
    }
    Ok(())
}

/// Promote one verified `PosSaleEvidence` row into a governed `PosSale` action commit,
/// refusing if any open data-quality finding exists on that evidence row.
fn promote_verified_pos_evidence(
    book: &mut EntityBook,
    pos_line_id: &str,
    sale_id: &str,
) -> Result<(), String> {
    let (store_id, sku_id, sold_qty, unit, net_revenue, currency, sold_at, cost_ref, unit_cost) = {
        let reader = book.cell()?.store().read();
        let findings = reader
            .observation_findings("main", "supply.PosSaleEvidence", pos_line_id)
            .map_err(|e| format!("findings {pos_line_id}: {e}"))?;
        if findings
            .iter()
            .any(|f| matches!(f.status, DataQualityFindingStatus::Open))
        {
            return Err(format!("refusing promotion of quarantined {pos_line_id}"));
        }
        let ev = reader
            .object("main", "supply.PosSaleEvidence", pos_line_id)
            .map_err(|e| format!("{e}"))?
            .ok_or_else(|| format!("missing PosSaleEvidence {pos_line_id}"))?;
        let sku_id = ev.values.get("sku_code").map(scalar).ok_or("sku_code")?;
        let cost = reader
            .object("main", "supply.UnitCost", &sku_id)
            .map_err(|e| format!("{e}"))?
            .ok_or_else(|| format!("missing UnitCost {sku_id}"))?;
        (
            ev.values
                .get("store_code")
                .map(scalar)
                .ok_or("store_code")?,
            sku_id,
            ev.values.get("sold_qty").map(scalar).ok_or("sold_qty")?,
            ev.values.get("unit").map(scalar).ok_or("unit")?,
            ev.values
                .get("net_revenue")
                .map(scalar)
                .ok_or("net_revenue")?,
            ev.values.get("currency").map(scalar).ok_or("currency")?,
            ev.values.get("sold_at").map(scalar).ok_or("sold_at")?,
            cost.values.get("cost_ref").map(scalar).ok_or("cost_ref")?,
            cost.values
                .get("unit_cost")
                .map(scalar)
                .ok_or("unit_cost")?,
        )
    };
    book.ok(
        "supply.pos_sale.promote_from_evidence",
        "clerk:hana",
        json!({
            "sale_id": sale_id, "source_evidence_id": pos_line_id,
            "store_id": store_id, "sku_id": sku_id,
            "sold_qty": sold_qty, "unit": unit, "net_revenue": net_revenue,
            "applied_unit_cost": unit_cost, "cogs_amount": "0",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": cost_ref,
            "currency": currency, "sold_at": sold_at
        }),
    )
}

struct EntityPin {
    cell_id: String,
    world: String,
    book_code: String,
    currency: String,
    head: LedgerHead,
    definition_version: u64,
    entity_stores: BTreeSet<String>,
}

/// Independently verify single-book-per-cell discipline and full `ReadContext` pins.
fn read_entity_pin(book: &EntityBook, legal_entity_id: &str) -> Result<EntityPin, String> {
    let reader = book.cell()?.store().read();
    let entities = list_objects(&reader, "supply.LegalEntity")?;
    if entities.len() != 1 {
        return Err(format!(
            "cell {} violates D02 single-book invariant: found {} LegalEntities",
            book.cell_id.as_str(),
            entities.len()
        ));
    }
    let entity_obj = entities
        .into_iter()
        .find(|e| e.id == legal_entity_id)
        .ok_or_else(|| format!("cell does not own {legal_entity_id}"))?;
    let book_code = entity_obj
        .values
        .get("book_code")
        .map(scalar)
        .ok_or("missing book_code")?;
    let currency = entity_obj
        .values
        .get("functional_currency")
        .map(scalar)
        .ok_or("missing functional_currency")?;
    let entity_stores: BTreeSet<String> = list_objects(&reader, "supply.Store")?
        .into_iter()
        .filter(|s| s.values.get("entity_id").map(scalar).as_deref() == Some(legal_entity_id))
        .map(|s| s.id)
        .collect();
    let head = book.head()?;

    let key = FactKey::new("supply.LegalEntity", legal_entity_id);
    let authority = CommitBinding {
        action_id: "read".into(),
        resource_kind: "supply.LegalEntity".into(),
        resource_id: legal_entity_id.to_owned(),
        artifact_version: Some("0.1.0".into()),
        policy_id: "supply.retail.access".into(),
        policy_version: "0.1.0".into(),
        resource_version: "1".into(),
        authorization_epoch: "1".into(),
        evaluated_at_logical_time: book.now().as_millis().to_string(),
    };
    let expected_ctx = ReadContext {
        frontier: LocalFrontier {
            cell: book.cell_id.clone(),
            world: "main".to_owned(),
            revision: head.commits,
        },
        definition_pin: DefinitionPin {
            version: 1,
            model_hash: book.pack.model().model_hash().to_owned(),
            admission_receipt: Some(book.pack.receipt_sha256.clone()),
        },
        authority_pin: AuthorityPin::from(&authority),
    };
    let envelope = book
        .cell()?
        .read_envelope(&key, &authority)
        .map_err(|e| format!("read_envelope {legal_entity_id}: {e}"))?;
    assert_eq!(envelope.token.completeness, Completeness::Complete);
    assert_eq!(envelope.token.semantics, ReadSemantics::Exact);
    envelope
        .token
        .validate(&expected_ctx, ReadRequirement::Exact)
        .map_err(|e| format!("validate token {legal_entity_id}: {e}"))?;
    Ok(EntityPin {
        cell_id: expected_ctx.frontier.cell.as_str().to_owned(),
        world: expected_ctx.frontier.world,
        book_code,
        currency,
        head,
        definition_version: expected_ctx.definition_pin.version,
        entity_stores,
    })
}

fn sum_entity_currency_buckets(
    result: &QueryResult,
    currency: &str,
    entity_stores: &BTreeSet<String>,
) -> Result<Decimal, String> {
    let mut sum = Decimal::from(0_i64);
    for row in result.rows.values() {
        let parts = bucket_parts(&row.bucket);
        if parts.first().map(String::as_str) == Some(currency)
            && parts.get(1).is_some_and(|s| entity_stores.contains(s))
            && let Some(amount) = row.value
        {
            sum = sum.checked_add(amount).ok_or("currency bucket overflow")?;
        }
    }
    Ok(sum)
}

fn detect_missing_cost_stores(
    gm_q: &QueryResult,
    gm_rate_q: &QueryResult,
    lots: &TransformResult,
    entity_stores: &BTreeSet<String>,
) -> (BTreeSet<String>, bool) {
    let mut stores = BTreeSet::new();
    let mut has_slice = false;
    for row in gm_q.rows.values().chain(gm_rate_q.rows.values()) {
        let parts = bucket_parts(&row.bucket);
        if parts.get(2).map(String::as_str) == Some("missing_cost")
            && let Some(store) = parts.get(1)
            && entity_stores.contains(store)
        {
            has_slice = true;
            stores.insert(store.clone());
        }
    }
    for row in &lots.rows {
        let store = row.values.get("store_id").map(scalar).unwrap_or_default();
        if entity_stores.contains(&store)
            && row.values.get("cost_complete") != Some(&RuntimeValue::Bool(true))
        {
            stores.insert(store);
        }
    }
    (stores, has_slice)
}

fn collect_store_gross_margin_rates(
    gm_rate_q: &QueryResult,
    currency: &str,
    entity_stores: &BTreeSet<String>,
    feeds_fresh_and_complete: bool,
) -> BTreeMap<String, Option<String>> {
    let mut latest_week: BTreeMap<String, String> = BTreeMap::new();
    let mut missing_cost: BTreeSet<String> = BTreeSet::new();
    let mut rates: BTreeMap<(String, String), String> = BTreeMap::new();
    for row in gm_rate_q.rows.values() {
        let parts = bucket_parts(&row.bucket);
        let (Some(ccy), Some(store), Some(status), Some(week)) =
            (parts.first(), parts.get(1), parts.get(2), parts.get(3))
        else {
            continue;
        };
        if ccy != currency || !entity_stores.contains(store) {
            continue;
        }
        if latest_week.get(store).is_none_or(|prev| week > prev) {
            latest_week.insert(store.clone(), week.clone());
        }
        if status == "missing_cost" {
            missing_cost.insert(store.clone());
        } else if status == "costed"
            && let Some(val) = row.value
        {
            rates.insert((store.clone(), week.clone()), val.canonical());
        }
    }
    latest_week
        .into_iter()
        .map(|(store, week)| {
            let val = if feeds_fresh_and_complete && !missing_cost.contains(&store) {
                rates.get(&(store.clone(), week)).cloned()
            } else {
                None
            };
            (store, val)
        })
        .collect()
}

fn verify_query_cut(
    book: &EntityBook,
    expected_head: &LedgerHead,
    expected_def_pin: &str,
    q: &QueryResult,
) -> Result<(), String> {
    if q.revision != *expected_head
        || q.known_as_of != expected_head.commits
        || q.definition_pin != expected_def_pin
        || q.context.scope != book.scope
        || q.context.source_cursors != book.source_cursors
        || q.context.watermarks != book.watermarks
    {
        return Err(format!(
            "query cut mismatch on {}: rev={:?} vs {:?}, def_pin={} vs {}",
            book.scope, q.revision, expected_head, q.definition_pin, expected_def_pin
        ));
    }
    Ok(())
}

fn pending_observation_ids(book: &EntityBook) -> Result<Vec<(u64, String)>, String> {
    Ok(book
        .cell()?
        .store()
        .read()
        .pending_observations()
        .map_err(|e| format!("pending_observations: {e}"))?
        .into_iter()
        .map(|p| (p.position, p.observation_id))
        .collect())
}

fn verify_entity_cut_stability(
    book: &EntityBook,
    legal_entity_id: &str,
    expected_head: &LedgerHead,
    pending_before: &[(u64, String)],
    transforms: [&TransformResult; 2],
    questions: [&QueryResult; 4],
) -> Result<(), String> {
    let expected_def_pin = book.pack.receipt_sha256.as_str();
    let head_after = book.head()?;
    let pending_after = pending_observation_ids(book)?;
    let tf_ok = transforms
        .iter()
        .all(|t| t.revision == *expected_head && t.definition_pin == expected_def_pin);
    if head_after != *expected_head || !tf_ok || pending_after != pending_before {
        return Err(format!(
            "frontier, definition cut, or pending inbox moved during projection of {legal_entity_id}"
        ));
    }
    for q in questions {
        verify_query_cut(book, expected_head, expected_def_pin, q)?;
    }
    Ok(())
}

fn project_entity_frontier(
    book: &EntityBook,
    legal_entity_id: &str,
    eval_question: &mut dyn FnMut(&str) -> Result<QueryResult, String>,
) -> Result<EntityFrontierSnapshot, String> {
    let pin = read_entity_pin(book, legal_entity_id)?;
    let pending_before = pending_observation_ids(book)?;
    let lots = book.evaluate_transform("supply.retail.store_lot_aging", |_| Ok(RowAccess::Read))?;
    let entity_tf = book.evaluate_transform("supply.retail.entity_management_summary", |_| {
        Ok(RowAccess::Read)
    })?;
    let rev_q = eval_question("supply.retail.store_revenue_weekly")?;
    let gm_q = eval_question("supply.retail.store_gross_margin_weekly")?;
    let gm_rate_q = eval_question("supply.retail.store_gross_margin_rate_weekly")?;
    let pos_feed_q = eval_question("supply.retail.pos_feed_revenue_weekly")?;
    let (open_feed_findings, pending_inbox_quarantined) = count_open_feed_issues(book)?;
    let count_report = evaluate_count_coverage(book, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    verify_entity_cut_stability(
        book,
        legal_entity_id,
        &pin.head,
        &pending_before,
        [&lots, &entity_tf],
        [&rev_q, &gm_q, &gm_rate_q, &pos_feed_q],
    )?;
    let expected_def_pin = book.pack.receipt_sha256.as_str();

    let missing_feed_coverage = check_feed_coverage(
        &book.source_cursors,
        &book.watermarks,
        book.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
    );
    let rev_sum = sum_entity_currency_buckets(&rev_q, &pin.currency, &pin.entity_stores)?;
    let gm_sum = sum_entity_currency_buckets(&gm_q, &pin.currency, &pin.entity_stores)?;
    let pos_feed_sum = sum_entity_currency_buckets(&pos_feed_q, &pin.currency, &pin.entity_stores)?;
    let (missing_cost_stores, has_missing_cost_slice) =
        detect_missing_cost_stores(&gm_q, &gm_rate_q, &lots, &pin.entity_stores);

    let all_fresh = [&rev_q, &gm_q, &gm_rate_q, &pos_feed_q]
        .iter()
        .all(|q| q.complete && matches!(q.freshness, Freshness::Exact | Freshness::Fresh));
    let feeds_and_rows_complete = lots.complete
        && entity_tf.complete
        && all_fresh
        && open_feed_findings == 0
        && pending_inbox_quarantined == 0
        && missing_feed_coverage.is_empty();
    let entity_tf_cost_complete = entity_tf
        .rows
        .iter()
        .find(|r| r.values.get("st_entity_id").map(scalar).as_deref() == Some(legal_entity_id))
        .is_some_and(|r| r.values.get("cost_complete") == Some(&RuntimeValue::Bool(true)));
    let cost_complete =
        entity_tf_cost_complete && missing_cost_stores.is_empty() && !has_missing_cost_slice;
    let on_hand_ok = feeds_and_rows_complete && count_report.on_hand_issue_stores.is_empty();
    let avail_ok = feeds_and_rows_complete && count_report.availability_issue_stores.is_empty();
    let store_gross_margin_rates = collect_store_gross_margin_rates(
        &gm_rate_q,
        &pin.currency,
        &pin.entity_stores,
        feeds_and_rows_complete,
    );

    Ok(EntityFrontierSnapshot {
        cell_id: pin.cell_id,
        legal_entity_id: legal_entity_id.to_owned(),
        book_code: pin.book_code,
        currency: pin.currency,
        scope: book.scope.clone(),
        world: pin.world,
        revision: pin.head.commits,
        ledger_seq: pin.head.seq,
        ledger_hash: pin.head.hash,
        definition_version: pin.definition_version,
        definition_pin: expected_def_pin.to_owned(),
        source_cursors: book.source_cursors.clone(),
        watermarks: book.watermarks.clone(),
        complete: on_hand_ok && avail_ok && cost_complete,
        open_feed_findings,
        pending_inbox_quarantined,
        missing_feed_coverage,
        missing_cost_stores,
        on_hand_issue_stores: count_report.on_hand_issue_stores,
        availability_issue_stores: count_report.availability_issue_stores,
        entity_net_revenue: feeds_and_rows_complete.then(|| rev_sum.canonical()),
        entity_gross_margin: (feeds_and_rows_complete && cost_complete).then(|| gm_sum.canonical()),
        store_gross_margin_rates,
        entity_pos_feed_revenue: feeds_and_rows_complete.then(|| pos_feed_sum.canonical()),
        entity_on_hand_verified: on_hand_ok.then_some(true),
        entity_availability_verified: avail_ok.then_some(true),
    })
}

const COMBINED_VIEW_QUESTIONS: [&str; 7] = [
    "supply.retail.store_revenue_weekly",
    "supply.retail.store_gross_margin_weekly",
    "supply.retail.store_gross_margin_rate_weekly",
    "supply.retail.store_sell_through_weekly",
    "supply.retail.store_weeks_of_supply_weekly",
    "supply.retail.store_gmroi_weekly",
    "supply.retail.pos_feed_revenue_weekly",
];

fn legal_entity_read_authority(
    book: &EntityBook,
    legal_entity_id: &str,
) -> (FactKey, CommitBinding) {
    let key = FactKey::new("supply.LegalEntity", legal_entity_id);
    let authority = CommitBinding {
        action_id: "read".into(),
        resource_kind: "supply.LegalEntity".into(),
        resource_id: legal_entity_id.to_owned(),
        artifact_version: Some("0.1.0".into()),
        policy_id: "supply.retail.access".into(),
        policy_version: "0.1.0".into(),
        resource_version: "1".into(),
        authorization_epoch: "1".into(),
        evaluated_at_logical_time: book.now().as_millis().to_string(),
    };
    (key, authority)
}

const TEST_HANDLE_KEY: [u8; 32] = [0x42; 32];

fn load_retail_coverage_spec() -> Result<CellCoverageSpec, String> {
    let text =
        fs::read_to_string(repo().join("registry/packs/supply-lab/questions/retail.coverage.json"))
            .map_err(|e| format!("retail.coverage.json: {e}"))?;
    CellCoverageSpec::from_json_str(&text).map_err(|e| format!("decode retail.coverage.json: {e}"))
}

fn load_spec_plans(
    pack: &RetailPack,
    spec: &CellCoverageSpec,
) -> Result<
    (
        BTreeMap<String, norn_checker::transform::TransformPlan>,
        BTreeMap<String, QuestionPlan>,
    ),
    String,
> {
    let mut tf_ids = BTreeSet::new();
    for g in &spec.coverage_transforms {
        tf_ids.insert(g.transform_id.as_str());
    }
    for g in &spec.reconciliation_gates {
        tf_ids.insert(g.expected_transform_id.as_str());
        tf_ids.insert(g.observed_transform_id.as_str());
    }
    for g in &spec.expected_grain_gates {
        tf_ids.insert(g.expected_transform_id.as_str());
    }
    let mut transforms = BTreeMap::new();
    for id in tf_ids {
        transforms.insert(id.to_owned(), pack.transform_plan(id)?);
    }
    let mut questions = BTreeMap::new();
    for q in &spec.questions {
        questions.insert(q.clone(), pack.retail_question(q)?);
    }
    Ok((transforms, questions))
}

fn evaluate_single_spec_cut(
    book: &EntityBook,
    legal_entity_id: &str,
    spec: &CellCoverageSpec,
    whole_world_values: bool,
) -> Result<norn_cell::combined_view::CellCutSnapshot, String> {
    let (transforms, questions) = load_spec_plans(book.pack, spec)?;
    let allow_tf = |_: &norn_checker::transform::TransformPlan| Ok(());
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let frontier = || Ok((book.source_cursors.clone(), book.watermarks.clone()));
    let (anchor_key, authority) = legal_entity_read_authority(book, legal_entity_id);
    let binding = SpecCellBinding {
        store: book.cell()?.store(),
        anchor_key: &anchor_key,
        authority: &authority,
        context: book.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values,
        min_disclosed_bucket: 2,
        read_frontier: &frontier,
        authorize_transform: &allow_tf,
        row_access: &allow_all,
        transforms: &transforms,
        questions: &questions,
    };
    evaluate_cell_cut_from_spec(spec, &binding, &mut |_, plan, ctx| {
        book.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })
    .map_err(|e| format!("evaluate_cell_cut_from_spec: {e}"))
}

fn evaluate_two_cell_report(
    sole: &EntityBook,
    sole_entity_id: &str,
    partner: &EntityBook,
    partner_entity_id: &str,
    eval_fn: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, String> {
    let spec = load_retail_coverage_spec()?;
    let (transforms, questions) = load_spec_plans(sole.pack, &spec)?;
    let allow_tf = |_: &norn_checker::transform::TransformPlan| Ok(());
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let sole_frontier = || Ok((sole.source_cursors.clone(), sole.watermarks.clone()));
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (sole_key, sole_auth) = legal_entity_read_authority(sole, sole_entity_id);
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, partner_entity_id);
    let bindings = [
        SpecCellBinding {
            store: sole.cell()?.store(),
            anchor_key: &sole_key,
            authority: &sole_auth,
            context: sole.query_context(),
            handle_key: &TEST_HANDLE_KEY,
            whole_world_values: false,
            min_disclosed_bucket: 2,
            read_frontier: &sole_frontier,
            authorize_transform: &allow_tf,
            row_access: &allow_all,
            transforms: &transforms,
            questions: &questions,
        },
        SpecCellBinding {
            store: partner.cell()?.store(),
            anchor_key: &partner_key,
            authority: &partner_auth,
            context: partner.query_context(),
            handle_key: &TEST_HANDLE_KEY,
            whole_world_values: false,
            min_disclosed_bucket: 2,
            read_frontier: &partner_frontier,
            authorize_transform: &allow_tf,
            row_access: &allow_all,
            transforms: &transforms,
            questions: &questions,
        },
    ];
    evaluate_combined_management_view_from_spec(&spec, &bindings, eval_fn)
        .map_err(|e| format!("combined_management_view: {e}"))
}

fn verify_library_combined_view_report(
    sole: &EntityBook,
    partner: &EntityBook,
) -> Result<CombinedViewReport, String> {
    let report = evaluate_two_cell_report(
        sole,
        "LE-SOLE",
        partner,
        "LE-PARTNER",
        |idx, _, plan, ctx| {
            let store = if idx == 0 {
                sole.cell().map_err(CellError::Engine)?.store()
            } else {
                partner.cell().map_err(CellError::Engine)?.store()
            };
            store.evaluate_question(plan, ctx, |_| Ok(true))
        },
    )?;
    assert_eq!(report.view_kind, CombinedViewKind::CombinedManagementView);
    assert!(!report.is_consolidated_financial_statement);
    assert!(report.read_only);
    assert_eq!(report.cells.len(), 2);
    let c0 = report.cells.first().ok_or("missing cell 0")?;
    let c1 = report.cells.get(1).ok_or("missing cell 1")?;
    let (_, sole_auth) = legal_entity_read_authority(sole, "LE-SOLE");
    let (_, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");
    assert_eq!(c0.cell_id.as_str(), SOLE_CELL_ID);
    assert_eq!(c1.cell_id.as_str(), PARTNER_CELL_ID);
    assert_eq!(c0.definition_pin, sole.pack.receipt_sha256);
    assert_eq!(c1.definition_pin, partner.pack.receipt_sha256);
    assert_eq!(c0.authority_pin, AuthorityPin::from(&sole_auth));
    assert_eq!(c1.authority_pin, AuthorityPin::from(&partner_auth));
    // Sole proprietorship has uncosted lots/sales, stale/drifted/future counts, and missing SKU rollups:
    // shipped library read path MUST mark it incomplete, emit a Partial read token, and withhold answers.
    assert!(!c0.complete);
    assert_eq!(c0.read_token.completeness, Completeness::Partial);
    assert!(c0.answers.is_none());
    for expected_gate in [
        "supply.retail.entity_management_summary",
        "supply.retail.store_lot_aging",
        "supply.retail.store_active_positions",
        "supply.retail.on_hand_count_reconciliation",
        "supply.retail.available_count_reconciliation",
        "supply.retail.rollup_sales_reconciliation",
        "supply.retail.rollup_margin_reconciliation",
        "supply.retail.rollup_on_hand_reconciliation",
        "supply.retail.rollup_closing_value_reconciliation",
        "supply.retail.store_weeks_of_supply_coverage",
        "supply.retail.store_gmroi_coverage",
    ] {
        assert!(
            c0.failed_coverage_gates.contains(expected_gate),
            "sole must fail shipped gate {expected_gate}: {:?}",
            c0.failed_coverage_gates
        );
    }
    // Partnership has complete costs, fresh reconciled counts, complete SKU rollups, and fresh feeds: complete!
    // DATA-VALUES-1: with `whole_world_values: false`, disclosed answers carry keyed revision & lineage handles.
    assert!(c1.complete);
    assert_eq!(c1.read_token.completeness, Completeness::Complete);
    assert!(c1.failed_coverage_gates.is_empty());
    let p_answers = c1
        .answers
        .as_ref()
        .ok_or("missing complete partner answers")?;
    assert_eq!(p_answers.len(), COMBINED_VIEW_QUESTIONS.len());
    let raw_rev = partner.answer("supply.retail.store_revenue_weekly")?;
    let disclosed_rev = p_answers
        .get("supply.retail.store_revenue_weekly")
        .ok_or("missing disclosed revenue answer")?;
    assert_eq!(
        disclosed_rev.revision.hash,
        keyed_revision_handle(&TEST_HANDLE_KEY, &raw_rev.revision.hash)
    );
    assert_ne!(disclosed_rev.revision.hash, raw_rev.revision.hash);
    assert_eq!(
        disclosed_rev.lineage_ref,
        keyed_lineage_handle(&TEST_HANDLE_KEY, &raw_rev.lineage_ref)
    );
    assert_ne!(disclosed_rev.lineage_ref, raw_rev.lineage_ref);

    verify_duplicate_cell_refusal(sole)?;
    verify_library_cut_row_access_gate(partner)?;
    verify_expected_grain_missing_sku_gate()?;
    verify_shipped_phantom_week_and_late_sale_gates()?;
    Ok(report)
}

fn verify_duplicate_cell_refusal(sole: &EntityBook) -> Result<(), String> {
    let dup = evaluate_two_cell_report(sole, "LE-SOLE", sole, "LE-SOLE", |_, _, plan, ctx| {
        sole.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(
        dup.is_err(),
        "duplicate cell_id in combined view must refuse"
    );
    Ok(())
}

fn verify_shipped_phantom_week_and_late_sale_gates() -> Result<(), String> {
    let spec = load_retail_coverage_spec()?;
    // 1. Out-of-window POS sale (week `2026-09-07`) must NOT invalidate the `2026-08-31` rollup;
    //    a late POS sale inside `2026-08-31` MUST invalidate it until `supply.store_kpi.refresh`.
    let mut partner = EntityBook::open(
        PARTNER_CELL_ID,
        "org_family_partnership",
        "prod",
        "partner-late-sale.sqlite",
    )?;
    let mut sole_dummy = EntityBook::open(
        SOLE_CELL_ID,
        "org_family_sole_prop",
        "prod",
        "sole-late-sale.sqlite",
    )?;
    seed_sole_prop(&mut sole_dummy)?;
    seed_partnership(&mut partner)?;
    seed_partner_operations(&mut partner)?;
    seed_partner_commercial_rollup(&mut partner)?;
    ingest_baseline_entity_feeds(&mut sole_dummy, &mut partner)?;
    partner.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-P1-NEXT-WK", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "sold_qty": "1", "unit": "each", "net_revenue": "400", "applied_unit_cost": "250",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-RING-1",
            "currency": "USD", "sold_at": "2026-09-07T00:00:00Z"
        }),
    )?;
    let out_of_window = evaluate_single_spec_cut(&partner, "LE-PARTNER", &spec, false)?;
    assert!(
        out_of_window.complete,
        "exact next-week boundary sale must not invalidate 2026-08-31 rollup: {:?}",
        out_of_window.failed_coverage_gates
    );
    partner.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-P1-LATE", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "sold_qty": "2", "unit": "each", "net_revenue": "800", "applied_unit_cost": "250",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-RING-1",
            "currency": "USD", "sold_at": "2026-09-06T21:30:00Z"
        }),
    )?;
    let stale_snap = evaluate_single_spec_cut(&partner, "LE-PARTNER", &spec, false)?;
    assert!(!stale_snap.complete);
    assert!(stale_snap.answers.is_none());
    assert!(
        stale_snap
            .failed_coverage_gates
            .contains("supply.retail.rollup_sales_reconciliation")
    );
    assert!(
        stale_snap
            .failed_coverage_gates
            .contains("supply.retail.rollup_margin_reconciliation")
    );
    partner.ok(
        "supply.store_kpi.refresh",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-PART-CFC-1-W9", "store_id": "ST-PART-CFC-1",
            "sku_id": "SKU-RING-18K", "period_week": "2026-08-31",
            "unit": "each", "currency": "USD", "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-PART-CFC-1/SKU-RING-18K/on_hand",
            "opening_cut_id": "CUT-P1-2026-08-31", "count_ref": "AUD-P2",
            "count_as_of": "2026-09-06T10:00:00Z", "costed_ref": "COST-RING-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-31T00:00:00Z", "period_end": "2026-09-06T23:59:59.999999999Z",
            "sales_weeks": 1, "on_hand_qty": "12", "avg_weekly_sales_qty": "10",
            "weeks_of_supply": "0", "period_gross_margin": "1500",
            "opening_stock_value": "5000", "closing_stock_value": "3000",
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    let refreshed = evaluate_single_spec_cut(&partner, "LE-PARTNER", &spec, false)?;
    assert!(refreshed.complete, "{:?}", refreshed.failed_coverage_gates);
    verify_shipped_rollup_stock_staleness(&mut partner, &spec)?;
    verify_shipped_phantom_and_stale_week_gates(&mut partner, &spec)
}

fn verify_shipped_rollup_stock_staleness(
    partner: &mut EntityBook,
    spec: &CellCoverageSpec,
) -> Result<(), String> {
    // 1b. A lot depletion + matching recount effective before `period_end` must invalidate
    //     the rollup's closing quantity and valuation until `supply.store_kpi.refresh`.
    partner.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-P1-W3", "remaining_qty": "10", "depleted_qty": "10",
            "as_of": "2026-09-06T09:00:00Z", "unit_cost": "250", "stock_value": "0"
        }),
    )?;
    for basis in ["on_hand", "available"] {
        partner.ok(
            "supply.store_count.recount",
            "clerk:hana",
            json!({
                "store_count_id": format!("ST-PART-CFC-1/SKU-RING-18K/{basis}"),
                "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
                "on_hand_qty": "10", "unit": "each", "quantity_basis": basis,
                "as_of": "2026-09-06T12:00:00Z", "count_source": "store_audit", "count_ref": "AUD-P2-REC"
            }),
        )?;
    }
    let stale_stock = evaluate_single_spec_cut(partner, "LE-PARTNER", spec, false)?;
    assert!(!stale_stock.complete);
    assert!(stale_stock.answers.is_none());
    assert!(
        !stale_stock
            .failed_coverage_gates
            .contains("supply.retail.on_hand_count_reconciliation")
    );
    assert!(
        stale_stock
            .failed_coverage_gates
            .contains("supply.retail.rollup_on_hand_reconciliation")
    );
    assert!(
        stale_stock
            .failed_coverage_gates
            .contains("supply.retail.rollup_closing_value_reconciliation")
    );
    partner.ok(
        "supply.store_kpi.refresh",
        "planner:omar",
        json!({
            "rollup_id": "KPI-ST-PART-CFC-1-W9", "store_id": "ST-PART-CFC-1",
            "sku_id": "SKU-RING-18K", "period_week": "2026-08-31",
            "unit": "each", "currency": "USD", "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-PART-CFC-1/SKU-RING-18K/on_hand",
            "opening_cut_id": "CUT-P1-2026-08-31", "count_ref": "AUD-P2-REC",
            "count_as_of": "2026-09-06T12:00:00Z", "costed_ref": "COST-RING-1",
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-31T00:00:00Z", "period_end": "2026-09-06T23:59:59.999999999Z",
            "sales_weeks": 1, "on_hand_qty": "10", "avg_weekly_sales_qty": "10",
            "weeks_of_supply": "0", "period_gross_margin": "1500",
            "opening_stock_value": "5000", "closing_stock_value": "2500",
            "average_inventory_value": "0", "gmroi": "0"
        }),
    )?;
    let restored = evaluate_single_spec_cut(partner, "LE-PARTNER", spec, false)?;
    assert!(restored.complete, "{:?}", restored.failed_coverage_gates);
    Ok(())
}

fn verify_shipped_phantom_and_stale_week_gates(
    partner: &mut EntityBook,
    spec: &CellCoverageSpec,
) -> Result<(), String> {
    // 2. Older-week (`2026-08-31`) rollup must NOT satisfy target week `2026-09-07` in `ExpectedGrainGate`.
    let mut next_wk_spec = spec.clone();
    for g in &mut next_wk_spec.expected_grain_gates {
        g.required_bucket_filters = vec![(3, "2026-09-07".to_owned())];
    }
    let wrong_wk = evaluate_single_spec_cut(partner, "LE-PARTNER", &next_wk_spec, false)?;
    assert!(!wrong_wk.complete);
    assert!(
        wrong_wk
            .failed_coverage_gates
            .contains("supply.retail.store_weeks_of_supply_coverage")
    );
    assert!(
        wrong_wk
            .failed_coverage_gates
            .contains("supply.retail.store_gmroi_coverage")
    );
    // 3. Phantom positive availability count with zero lots must fail `available_count_reconciliation`.
    partner.ok(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-PART-PHANTOM", "name": "Phantom Partner Branch",
            "entity_id": "LE-PARTNER", "custodian_id": "CUST-MALL-CFC-OPS",
            "location_kind": "standalone", "mall_code": null, "currency": "USD"
        }),
    )?;
    partner.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-PART-PHANTOM/SKU-RING-18K/available",
            "store_id": "ST-PART-PHANTOM", "sku_id": "SKU-RING-18K",
            "on_hand_qty": "5", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-06T10:00:00Z", "count_source": "store_audit", "count_ref": "AUD-PHANTOM"
        }),
    )?;
    let phantom = evaluate_single_spec_cut(partner, "LE-PARTNER", spec, false)?;
    assert!(!phantom.complete);
    assert!(phantom.answers.is_none());
    assert!(
        phantom
            .failed_coverage_gates
            .contains("supply.retail.available_count_reconciliation")
    );
    Ok(())
}

fn verify_expected_grain_missing_sku_gate() -> Result<(), String> {
    // Adding a second live SKU with no `StoreCommercialRollup` must fail closed via
    // `ExpectedGrainGate` so store-SKU KPIs never silently omit a SKU.
    let mut partner = EntityBook::open(
        PARTNER_CELL_ID,
        "org_family_partnership",
        "prod",
        "partner-missing-sku.sqlite",
    )?;
    seed_partnership(&mut partner)?;
    seed_partner_operations(&mut partner)?;
    seed_partner_commercial_rollup(&mut partner)?;
    let mut sole_dummy = EntityBook::open(
        SOLE_CELL_ID,
        "org_family_sole_prop",
        "prod",
        "sole-missing-sku.sqlite",
    )?;
    seed_sole_prop(&mut sole_dummy)?;
    ingest_baseline_entity_feeds(&mut sole_dummy, &mut partner)?;
    partner.ok(
        "supply.sku.register",
        "eng:dina",
        json!({"sku_id": "SKU-NECKLACE", "category": "spares", "unit_of_measure": "each", "shelf_life_days": 3650}),
    )?;
    partner.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-P1-NECK", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-NECKLACE",
            "owner_entity_id": "LE-PARTNER", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "5", "remaining_qty": "5", "depleted_qty": "0", "sell_through_rate": "0",
            "unit": "each", "received_at": "2026-08-20T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "opening_stock_value": "0",
            "stock_value": "0", "currency": "USD"
        }),
    )?;
    for basis in ["on_hand", "available"] {
        partner.ok(
            "supply.store_count.record",
            "clerk:hana",
            json!({
                "store_count_id": format!("ST-PART-CFC-1/SKU-NECKLACE/{basis}"),
                "store_id": "ST-PART-CFC-1", "sku_id": "SKU-NECKLACE",
                "on_hand_qty": "5", "unit": "each", "quantity_basis": basis,
                "as_of": "2026-09-06T10:00:00Z", "count_source": "store_audit", "count_ref": "AUD-P3"
            }),
        )?;
    }
    let report = evaluate_two_cell_report(
        &sole_dummy,
        "LE-SOLE",
        &partner,
        "LE-PARTNER",
        |idx, _, plan, ctx| {
            let store = if idx == 0 {
                sole_dummy.cell().map_err(CellError::Engine)?.store()
            } else {
                partner.cell().map_err(CellError::Engine)?.store()
            };
            store.evaluate_question(plan, ctx, |_| Ok(true))
        },
    )?;
    let p_snap = report.cells.get(1).ok_or("missing partner snap")?;
    assert!(!p_snap.complete);
    assert!(p_snap.answers.is_none());
    assert!(
        p_snap
            .failed_coverage_gates
            .contains("supply.retail.store_weeks_of_supply_coverage")
    );
    assert!(
        p_snap
            .failed_coverage_gates
            .contains("supply.retail.store_gmroi_coverage")
    );
    Ok(())
}

fn verify_library_cut_row_access_gate(partner: &EntityBook) -> Result<(), String> {
    // HOST-READ-2 / VIEW-READ-1:
    // - `RowAccess::Count` withholds rows -> `tf.complete == false` -> gate fails closed.
    // - `RowAccess::None` rows are invisible -> never flip `failed_coverage_gates` (no existence oracle).
    // - Pre-evaluation `authorize_transform` denial refuses up front before scanning any rows.
    let rev_plan = partner
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let entity_tf_plan = partner
        .pack
        .transform_plan("supply.retail.entity_management_summary")?;
    let allow_tf = |_: &norn_checker::transform::TransformPlan| Ok(());
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let count_only = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Count);
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");

    let count_input = CellViewInput {
        store: partner.cell()?.store(),
        anchor_key: &partner_key,
        authority: &partner_auth,
        context: partner.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &partner_frontier,
        min_fresh_ms: partner.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[CoverageTransformGate {
            name: "supply.retail.entity_management_summary",
            plan: &entity_tf_plan,
            required_true_column: "cost_complete",
            authorize_transform: &allow_tf,
            row_access: &count_only,
        }],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", &rev_plan)],
    };
    let count_snap = evaluate_cell_cut(&count_input, &mut |_, plan, ctx| {
        partner
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })
    .map_err(|e| format!("count-only evaluate_cell_cut: {e}"))?;
    assert!(!count_snap.complete);
    assert!(
        count_snap
            .failed_coverage_gates
            .contains("supply.retail.entity_management_summary")
    );
    assert!(count_snap.answers.is_none());
    verify_library_cut_population_only_disclosure(partner, &rev_plan)?;
    verify_library_cut_hidden_and_denied_gate(partner, &rev_plan, &entity_tf_plan)
}

fn verify_library_cut_population_only_disclosure(
    partner: &EntityBook,
    rev_plan: &QuestionPlan,
) -> Result<(), String> {
    // DATA-VALUES-1 (D52/D53): a Population-only (`RowAccess::Count`) reader must never
    // receive individual `QueryRow.members` amounts, small buckets (< min_disclosed_bucket)
    // must be suppressed (`value: None`, empty `totals`), and `revision.hash` stays keyed
    // even when `whole_world_values` was requested.
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let count_only = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Count);
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");
    let raw_rev = partner.answer("supply.retail.store_revenue_weekly")?;
    let pop_input = CellViewInput {
        store: partner.cell()?.store(),
        anchor_key: &partner_key,
        authority: &partner_auth,
        context: partner.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: true,
        member_access: &count_only,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &partner_frontier,
        min_fresh_ms: partner.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let pop_snap = evaluate_cell_cut(&pop_input, &mut |_, plan, ctx| {
        partner
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })
    .map_err(|e| format!("population-only evaluate_cell_cut: {e}"))?;
    assert!(pop_snap.complete);
    let answers = pop_snap.answers.as_ref().ok_or("missing pop answers")?;
    let rev = answers
        .get("supply.retail.store_revenue_weekly")
        .ok_or("missing pop revenue")?;
    assert_eq!(
        rev.revision.hash,
        keyed_revision_handle(&TEST_HANDLE_KEY, &raw_rev.revision.hash)
    );
    assert!(!rev.rows.is_empty());
    for row in rev.rows.values() {
        assert!(row.suppressed, "single-member bucket must suppress");
        assert!(row.value.is_none());
        assert!(row.totals.is_empty());
        assert!(row.membership_digest.is_none());
        for member in row.members.values() {
            assert!(member.restricted);
            assert!(member.amounts.is_none());
        }
    }
    verify_non_inlined_and_short_key_disclosure(partner, rev_plan)
}

fn verify_non_inlined_and_short_key_disclosure(
    partner: &EntityBook,
    rev_plan: &QuestionPlan,
) -> Result<(), String> {
    // Non-inlined analytics buckets (`member_count.is_some()`) MUST strip unkeyed
    // `membership_digest` when `!whole_world_values`, both when suppressed and disclosed.
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");
    for (count, expect_suppressed) in [(1_u64, true), (5_u64, false)] {
        let input = CellViewInput {
            store: partner.cell()?.store(),
            anchor_key: &partner_key,
            authority: &partner_auth,
            context: partner.query_context(),
            handle_key: &TEST_HANDLE_KEY,
            whole_world_values: false,
            member_access: &allow_all,
            min_disclosed_bucket: 2,
            required_feeds: &REQUIRED_FEEDS,
            read_frontier: &partner_frontier,
            min_fresh_ms: partner.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
            evidence_types: &EVIDENCE_TYPES,
            evidence_access: &allow_all,
            coverage_transforms: &[],
            reconciliation_gates: &[],
            expected_grain_gates: &[],
            questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
        };
        let snap = evaluate_cell_cut(&input, &mut |_, plan, ctx| {
            let mut res = partner
                .cell()
                .map_err(CellError::Engine)?
                .store()
                .evaluate_question(plan, ctx, |_| Ok(true))?;
            for row in res.rows.values_mut() {
                row.members.clear();
                row.member_count = Some(count);
                row.membership_digest = Some("sha256:raw-unkeyed-digest".to_owned());
            }
            Ok(res)
        })
        .map_err(|e| format!("non-inlined evaluate_cell_cut: {e}"))?;
        let rev = snap
            .answers
            .as_ref()
            .and_then(|a| a.get("supply.retail.store_revenue_weekly"))
            .ok_or("missing non-inlined revenue")?;
        for row in rev.rows.values() {
            assert_eq!(row.suppressed, expect_suppressed);
            assert!(
                row.membership_digest.is_none(),
                "unkeyed membership_digest must be withheld for restricted non-inlined buckets"
            );
            if expect_suppressed {
                assert!(row.value.is_none());
                assert!(row.totals.is_empty());
            } else {
                assert!(row.value.is_some());
            }
        }
    }
    verify_short_handle_key_refusal(partner, rev_plan)
}

fn verify_short_handle_key_refusal(
    partner: &EntityBook,
    rev_plan: &QuestionPlan,
) -> Result<(), String> {
    // D53: handle_key shorter than 32 bytes must refuse fail-closed.
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");
    let short_key = [0x11_u8; 16];
    let short_input = CellViewInput {
        store: partner.cell()?.store(),
        anchor_key: &partner_key,
        authority: &partner_auth,
        context: partner.query_context(),
        handle_key: &short_key,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &partner_frontier,
        min_fresh_ms: partner.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let short_res = evaluate_cell_cut(&short_input, &mut |_, plan, ctx| {
        partner
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(matches!(short_res, Err(CellError::Encoding(_))));
    Ok(())
}

fn verify_library_cut_hidden_and_denied_gate(
    partner: &EntityBook,
    rev_plan: &QuestionPlan,
    entity_tf_plan: &norn_checker::transform::TransformPlan,
) -> Result<(), String> {
    let allow_tf = |_: &norn_checker::transform::TransformPlan| Ok(());
    let hide_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::None);
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");

    // HOST-READ-2: `RowAccess::None` rows stay strictly invisible (cannot leak existence).
    let hidden_input = CellViewInput {
        store: partner.cell()?.store(),
        anchor_key: &partner_key,
        authority: &partner_auth,
        context: partner.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &hide_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &partner_frontier,
        min_fresh_ms: partner.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &hide_all,
        coverage_transforms: &[CoverageTransformGate {
            name: "supply.retail.entity_management_summary",
            plan: entity_tf_plan,
            required_true_column: "cost_complete",
            authorize_transform: &allow_tf,
            row_access: &hide_all,
        }],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let hidden_snap = evaluate_cell_cut(&hidden_input, &mut |_, plan, ctx| {
        partner
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |obj| Ok(hide_all(obj)? != RowAccess::None))
    })
    .map_err(|e| format!("hidden evaluate_cell_cut: {e}"))?;
    assert!(
        hidden_snap.failed_coverage_gates.is_empty(),
        "HOST-READ-2: RowAccess::None must not leak row presence via gate failures"
    );
    let hidden_rev = hidden_snap
        .answers
        .as_ref()
        .and_then(|a| a.get("supply.retail.store_revenue_weekly"))
        .ok_or("missing hidden revenue answer")?;
    assert!(
        hidden_rev.rows.is_empty(),
        "HOST-READ-2: hidden rows must never appear in disclosed answers"
    );
    // A rogue evaluator returning `RowAccess::None` members must be refused fail-closed.
    let rogue_hidden = evaluate_cell_cut(&hidden_input, &mut |_, plan, ctx| {
        partner
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(
        matches!(
            rogue_hidden,
            Err(CellError::ComputeRefused("core.compute.stale_input"))
        ),
        "evaluator disclosing RowAccess::None member IDs must refuse: {rogue_hidden:?}"
    );
    verify_library_cut_denied_transform_gate(partner, rev_plan, entity_tf_plan)
}

fn verify_library_cut_denied_transform_gate(
    partner: &EntityBook,
    rev_plan: &QuestionPlan,
    entity_tf_plan: &norn_checker::transform::TransformPlan,
) -> Result<(), String> {
    let deny_tf = |_: &norn_checker::transform::TransformPlan| -> Result<(), CellError> {
        Err(CellError::Encoding("transform access denied".to_owned()))
    };
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let count_only = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Count);
    let partner_frontier = || Ok((partner.source_cursors.clone(), partner.watermarks.clone()));
    let (partner_key, partner_auth) = legal_entity_read_authority(partner, "LE-PARTNER");
    let denied_input = CellViewInput {
        store: partner.cell()?.store(),
        anchor_key: &partner_key,
        authority: &partner_auth,
        context: partner.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &partner_frontier,
        min_fresh_ms: partner.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[CoverageTransformGate {
            name: "supply.retail.entity_management_summary",
            plan: entity_tf_plan,
            required_true_column: "cost_complete",
            authorize_transform: &deny_tf,
            row_access: &count_only,
        }],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let denied = evaluate_cell_cut(&denied_input, &mut |_, plan, ctx| {
        partner
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(
        matches!(denied, Err(CellError::Encoding(_))),
        "pre-evaluation VIEW-READ-1 transform denial must refuse: {denied:?}"
    );
    Ok(())
}

#[cfg(all(feature = "duckdb", feature = "native"))]
fn assert_query_result_parity(
    label: &str,
    cell_q: &QueryResult,
    duck_q: &QueryResult,
) -> Result<(), String> {
    assert_eq!(duck_q.query_pin, cell_q.query_pin, "{label}: query_pin");
    assert_eq!(
        duck_q.definition_pin, cell_q.definition_pin,
        "{label}: definition_pin"
    );
    assert_eq!(duck_q.revision, cell_q.revision, "{label}: revision");
    assert_eq!(
        duck_q.known_as_of, cell_q.known_as_of,
        "{label}: known_as_of"
    );
    assert_eq!(duck_q.grain, cell_q.grain, "{label}: grain");
    assert_eq!(
        duck_q.lineage_ref, cell_q.lineage_ref,
        "{label}: lineage_ref"
    );
    assert_eq!(duck_q.complete, cell_q.complete, "{label}: complete");
    assert_eq!(duck_q.freshness, cell_q.freshness, "{label}: freshness");
    assert_eq!(duck_q.exclusions, cell_q.exclusions, "{label}: exclusions");
    assert_eq!(duck_q.unlinked, cell_q.unlinked, "{label}: unlinked");
    assert_eq!(
        duck_q.rows.len(),
        cell_q.rows.len(),
        "{label}: row count mismatch"
    );
    for (key, cell_row) in &cell_q.rows {
        let duck_row = duck_q
            .rows
            .get(key)
            .ok_or_else(|| format!("{label}: missing DuckDB bucket {key}"))?;
        assert_eq!(duck_row.bucket, cell_row.bucket, "{label}/{key}: bucket");
        assert_eq!(duck_row.totals, cell_row.totals, "{label}/{key}: totals");
        assert_eq!(duck_row.value, cell_row.value, "{label}/{key}: value");
        assert_eq!(duck_row.members, cell_row.members, "{label}/{key}: members");
        assert_eq!(
            duck_row.suppressed, cell_row.suppressed,
            "{label}/{key}: suppressed"
        );
        assert_eq!(
            duck_row.null_counts, cell_row.null_counts,
            "{label}/{key}: null_counts"
        );
    }
    Ok(())
}

#[cfg(all(feature = "duckdb", feature = "native"))]
fn sync_duckdb_tenant(
    duck: &norn_cell::analytics::engine::Engine,
    book: &EntityBook,
) -> Result<norn_cell::analytics::TenantId, String> {
    let store = book.cell()?.store();
    let tenant = norn_cell::analytics::TenantId::of_scope(&book.scope);
    let mut frontier = duck
        .frontier(
            &store
                .analytics_frontier(&tenant, "main")
                .map_err(|e| format!("frontier: {e}"))?,
        )
        .map_err(|e| format!("{}: {}", e.code, e.detail))?
        .frontier;
    loop {
        let batch = store
            .analytics_delta(&tenant, "main", frontier, 64)
            .map_err(|e| format!("delta: {e}"))?;
        frontier = duck
            .sync(&batch.request)
            .map_err(|e| format!("{}: {}", e.code, e.detail))?
            .frontier;
        if batch.complete {
            break;
        }
    }
    Ok(tenant)
}

#[cfg(all(feature = "duckdb", feature = "native"))]
fn assert_combined_view_report_parity(
    cell_report: &CombinedViewReport,
    duck_report: &CombinedViewReport,
) -> Result<(), String> {
    assert_eq!(duck_report.view_kind, cell_report.view_kind);
    assert_eq!(
        duck_report.is_consolidated_financial_statement,
        cell_report.is_consolidated_financial_statement
    );
    assert_eq!(duck_report.read_only, cell_report.read_only);
    for (cell_c, duck_c) in cell_report.cells.iter().zip(&duck_report.cells) {
        assert_eq!(duck_c.cell_id, cell_c.cell_id);
        assert_eq!(duck_c.revision, cell_c.revision);
        assert_eq!(duck_c.ledger_seq, cell_c.ledger_seq);
        assert_eq!(duck_c.definition_pin, cell_c.definition_pin);
        assert_eq!(duck_c.authority_pin, cell_c.authority_pin);
        assert_eq!(duck_c.read_token, cell_c.read_token);
        assert_eq!(duck_c.source_cursors, cell_c.source_cursors);
        assert_eq!(duck_c.watermarks, cell_c.watermarks);
        assert_eq!(duck_c.failed_coverage_gates, cell_c.failed_coverage_gates);
        assert_eq!(duck_c.complete, cell_c.complete);
        assert_eq!(duck_c.answers.is_some(), cell_c.answers.is_some());
        if let (Some(cell_answers), Some(duck_answers)) = (&cell_c.answers, &duck_c.answers) {
            for q_name in COMBINED_VIEW_QUESTIONS {
                let cq = cell_answers.get(q_name).ok_or("missing cell answer")?;
                let dq = duck_answers.get(q_name).ok_or("missing duck answer")?;
                assert_query_result_parity(&format!("{}/{q_name}", cell_c.scope), cq, dq)?;
            }
        }
    }
    Ok(())
}

#[cfg(all(feature = "duckdb", feature = "native"))]
fn verify_duckdb_combined_view_parity(
    sole: &EntityBook,
    partner: &EntityBook,
    cell_view: &CombinedManagementView,
) -> Result<(), String> {
    use norn_cell::analytics::TenantId;
    let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
    let duck = norn_cell::analytics::engine::Engine::new(norn_cell::analytics::engine::Settings {
        root: dir.path().join("duckdb"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("duckdb: {e}"))?;

    let sole_tenant = sync_duckdb_tenant(&duck, sole)?;
    let partner_tenant = sync_duckdb_tenant(&duck, partner)?;
    let duck_eval =
        |book: &EntityBook, tenant: &TenantId, q_name: &str| -> Result<QueryResult, String> {
            let plan = book.pack.retail_question(q_name)?;
            let prepared = book
                .cell()?
                .store()
                .analytics_prepare(&plan, book.query_context(), tenant)
                .map_err(|e| format!("prepare {q_name}: {e}"))?;
            let response = duck
                .evaluate(prepared.request())
                .map_err(|e| format!("duckdb {q_name}: {} {}", e.code, e.detail))?;
            book.cell()?
                .store()
                .analytics_answer(prepared, response)
                .map_err(|e| format!("answer {q_name}: {e}"))
        };

    let cell_report = verify_library_combined_view_report(sole, partner)?;
    let duck_report = evaluate_two_cell_report(
        sole,
        "LE-SOLE",
        partner,
        "LE-PARTNER",
        |idx, q_name, _, _| {
            let (book, tenant) = if idx == 0 {
                (sole, &sole_tenant)
            } else {
                (partner, &partner_tenant)
            };
            duck_eval(book, tenant, q_name).map_err(CellError::Engine)
        },
    )?;
    assert_combined_view_report_parity(&cell_report, &duck_report)?;
    // Also verify exact-vs-DuckDB question parity on every declared question for `sole`.
    for q_name in COMBINED_VIEW_QUESTIONS {
        let cq = sole.answer(q_name)?;
        let dq = duck_eval(sole, &sole_tenant, q_name)?;
        assert_query_result_parity(&format!("{}/{q_name}", sole.scope), &cq, &dq)?;
    }

    let duck_view = CombinedManagementView {
        view_kind: "combined_management_view",
        is_consolidated_financial_statement: false,
        read_only: true,
        entities: vec![
            project_entity_frontier(sole, "LE-SOLE", &mut |q| duck_eval(sole, &sole_tenant, q))?,
            project_entity_frontier(partner, "LE-PARTNER", &mut |q| {
                duck_eval(partner, &partner_tenant, q)
            })?,
        ],
    };
    assert_eq!(
        duck_view, *cell_view,
        "DuckDB combined management view must match Cell combined management view exactly"
    );
    Ok(())
}

/// Entrypoint invoked by `dataset_laws::retail_truth_slice_combined_view_duckdb_parity`
/// so CI's DuckDB phase always executes S1's full Cell-vs-DuckDB combined-view &
/// per-question (including `lineage_ref` and `members`) parity proof.
#[cfg(all(feature = "duckdb", feature = "native"))]
pub(crate) fn run_s1_duckdb_combined_view_parity() -> Result<(), String> {
    let mut sole = EntityBook::open(SOLE_CELL_ID, "org_family_sole_prop", "prod", "sole.sqlite")?;
    let mut partner = EntityBook::open(
        PARTNER_CELL_ID,
        "org_family_partnership",
        "prod",
        "partner.sqlite",
    )?;
    seed_sole_prop(&mut sole)?;
    seed_partnership(&mut partner)?;
    seed_retail_operations(&mut sole, &mut partner)?;
    seed_store_commercial_rollups(&mut sole, &mut partner)?;
    ingest_baseline_entity_feeds(&mut sole, &mut partner)?;
    let cell_view = CombinedManagementView {
        view_kind: "combined_management_view",
        is_consolidated_financial_statement: false,
        read_only: true,
        entities: vec![
            project_entity_frontier(&sole, "LE-SOLE", &mut |q| sole.answer(q))?,
            project_entity_frontier(&partner, "LE-PARTNER", &mut |q| partner.answer(q))?,
        ],
    };
    verify_duckdb_combined_view_parity(&sole, &partner, &cell_view)
}

fn verify_mismatched_promotion_refused(sole: &mut EntityBook) -> Result<(), String> {
    for (sale_id, qty, rev, sold_at, expected_code) in [
        (
            "SALE-TAMPER-QTY",
            "5",
            "800",
            "2026-09-02T14:05:00Z",
            "core.action.precondition",
        ),
        (
            "SALE-TAMPER-REV",
            "4",
            "999",
            "2026-09-02T14:05:00Z",
            "core.action.precondition",
        ),
        (
            "SALE-TAMPER-TS",
            "4",
            "800",
            "2026-09-03T14:05:00Z",
            "core.laws.violated",
        ),
    ] {
        let (code, msg) = sole.refused(
            "supply.pos_sale.promote_from_evidence",
            "clerk:hana",
            json!({
                "sale_id": sale_id, "source_evidence_id": "POS-SOLE-BAD",
                "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
                "sold_qty": qty, "unit": "each", "net_revenue": rev,
                "applied_unit_cost": "100", "cogs_amount": "0",
                "gross_margin": "0", "gross_margin_rate": "0",
                "cost_status": "costed", "costed_ref": "COST-DRESS-1",
                "currency": "EGP", "sold_at": sold_at
            }),
        )?;
        assert_eq!(code, expected_code, "{sale_id}: {msg}");
    }
    Ok(())
}

fn verify_quarantined_evidence_access(sole: &EntityBook) -> Result<(), String> {
    let lib_quarantined = evaluate_single_library_cut(sole, "LE-SOLE")?;
    assert_eq!(lib_quarantined.open_findings, 1);
    assert!(!lib_quarantined.complete);
    assert_eq!(
        lib_quarantined.read_token.completeness,
        Completeness::Partial
    );
    assert!(lib_quarantined.answers.is_none());
    // HOST-READ-2: `RowAccess::None` hides quarantined evidence findings; `RowAccess::Count`
    // marks the cut incomplete without disclosing finding counts.
    let hide_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::None);
    let count_only = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Count);
    let lib_hidden_findings = evaluate_single_library_cut_with_access(sole, "LE-SOLE", &hide_all)?;
    assert_eq!(lib_hidden_findings.open_findings, 0);
    let lib_count_findings = evaluate_single_library_cut_with_access(sole, "LE-SOLE", &count_only)?;
    assert_eq!(lib_count_findings.open_findings, 0);
    assert!(!lib_count_findings.complete);
    assert!(lib_count_findings.answers.is_none());
    Ok(())
}

fn verify_quarantine_promotion_and_single_book_guard(sole: &mut EntityBook) -> Result<(), String> {
    sole.ingest_feed(&FeedRow {
        feed_id: "supply.pos_sales",
        mapping: "supply.pos_sales.mapping",
        dataset: "pos_sale_line",
        source: "synthetic-pos",
        event: "ev-sole-pos-bad",
        record: "POS-SOLE-BAD",
        payload: r#"{"pos_line_id":"POS-SOLE-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":-4,"uom":"PCS","net_amount":800,"ccy":"EGP","sold_at":"2026-09-02T14:00:00Z"}"#,
    })?;
    assert!(promote_verified_pos_evidence(sole, "POS-SOLE-BAD", "SALE-PROMOTED-BAD").is_err());
    verify_quarantined_evidence_access(sole)?;
    let quarantined = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert_eq!(quarantined.open_feed_findings, 1);
    assert_eq!(quarantined.entity_net_revenue, None);
    assert_eq!(quarantined.entity_pos_feed_revenue, None);

    sole.ingest_feed(&FeedRow {
        feed_id: "supply.pos_sales",
        mapping: "supply.pos_sales.mapping",
        dataset: "pos_sale_line",
        source: "synthetic-pos",
        event: "ev-sole-pos-fix",
        record: "POS-SOLE-BAD",
        payload: r#"{"pos_line_id":"POS-SOLE-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":4,"uom":"PCS","net_amount":800,"ccy":"EGP","sold_at":"2026-09-02T14:05:00Z"}"#,
    })?;
    verify_mismatched_promotion_refused(sole)?;
    promote_verified_pos_evidence(sole, "POS-SOLE-BAD", "SALE-PROMOTED-POS-SOLE-BAD")?;
    // 1-to-1 `source_evidence` link cardinality structurally blocks promoting the same
    // PosSaleEvidence line a second time under any new sale_id or action key.
    assert!(
        promote_verified_pos_evidence(sole, "POS-SOLE-BAD", "SALE-PROMOTED-DUP").is_err(),
        "second promotion of POS-SOLE-BAD under a different sale_id must refuse"
    );
    let promoted = sole
        .cell()?
        .store()
        .read()
        .object("main", "supply.PosSale", "SALE-PROMOTED-POS-SOLE-BAD")
        .map_err(|e| format!("{e}"))?
        .ok_or("missing promoted PosSale")?;
    assert_eq!(
        promoted
            .values
            .get("source_evidence_id")
            .map(scalar)
            .as_deref(),
        Some("POS-SOLE-BAD")
    );
    let recovered = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert_eq!(recovered.open_feed_findings, 0);
    assert_eq!(recovered.entity_pos_feed_revenue.as_deref(), Some("6800"));
    assert_eq!(recovered.entity_net_revenue.as_deref(), Some("10800"));

    let (_, clean_uom) = sole.ingest_feed(&FeedRow {
        feed_id: "supply.pos_sales",
        mapping: "supply.pos_sales.mapping",
        dataset: "pos_sale_line",
        source: "synthetic-pos",
        event: "ev-sole-pos-box",
        record: "POS-SOLE-BOX",
        payload: r#"{"pos_line_id":"POS-SOLE-BOX","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":2,"uom":"BOX","net_amount":200,"ccy":"EGP","sold_at":"2026-09-02T15:00:00Z"}"#,
    })?;
    assert!(!clean_uom, "unknown UOM must quarantine at mapping time");
    let lib_map_refused = evaluate_single_library_cut(sole, "LE-SOLE")?;
    assert_eq!(lib_map_refused.pending_observations, 1);
    assert!(!lib_map_refused.complete);
    assert!(lib_map_refused.answers.is_none());
    let map_refused = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert_eq!(map_refused.pending_inbox_quarantined, 1);
    assert_eq!(map_refused.entity_net_revenue, None);

    // D02 single-book-per-cell: registering a second LegalEntity in an occupied cell
    // refuses structurally at the write boundary (`existing_books === 0`).
    let (code, msg) = sole.refused(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-ROGUE-EGP", "legal_name": "Rogue Second Book",
            "entity_kind": "corporation", "book_code": "BOOK-ROGUE-EGP", "functional_currency": "EGP"
        }),
    )?;
    assert_eq!(code, "core.action.precondition", "{msg}");
    Ok(())
}

fn evaluate_single_library_cut_with_access(
    book: &EntityBook,
    legal_entity_id: &str,
    evidence_access: norn_cell::combined_view::RowAccessDecider<'_>,
) -> Result<norn_cell::combined_view::CellCutSnapshot, String> {
    let rev_plan = book
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let (anchor_key, authority) = legal_entity_read_authority(book, legal_entity_id);
    let live_frontier = || Ok((book.source_cursors.clone(), book.watermarks.clone()));
    let input = CellViewInput {
        store: book.cell()?.store(),
        anchor_key: &anchor_key,
        authority: &authority,
        context: book.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: evidence_access,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &live_frontier,
        min_fresh_ms: book.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", &rev_plan)],
    };
    evaluate_cell_cut(&input, &mut |_, plan, ctx| {
        book.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })
    .map_err(|e| format!("evaluate_cell_cut: {e}"))
}

fn evaluate_single_library_cut(
    book: &EntityBook,
    legal_entity_id: &str,
) -> Result<norn_cell::combined_view::CellCutSnapshot, String> {
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    evaluate_single_library_cut_with_access(book, legal_entity_id, &allow_all)
}

fn verify_library_cut_tampering_refusals(sole: &EntityBook) -> Result<(), String> {
    let rev_plan = sole
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let gm_plan = sole
        .pack
        .retail_question("supply.retail.store_gross_margin_weekly")?;
    let (anchor_key, authority) = legal_entity_read_authority(sole, "LE-SOLE");
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let live_frontier = || Ok((sole.source_cursors.clone(), sole.watermarks.clone()));
    // 1. Wrong-question substitution callback must refuse (`query_pin` mismatch).
    let input = CellViewInput {
        store: sole.cell()?.store(),
        anchor_key: &anchor_key,
        authority: &authority,
        context: sole.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &live_frontier,
        min_fresh_ms: sole.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", &rev_plan)],
    };
    let swapped = evaluate_cell_cut(&input, &mut |_, _, ctx| {
        sole.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(&gm_plan, ctx, |_| Ok(true))
    });
    assert!(
        matches!(
            swapped,
            Err(CellError::ComputeRefused("core.compute.stale_input"))
        ),
        "substituted question answer must refuse: {swapped:?}"
    );
    // 2. Duplicate question names must refuse up front with zero evaluator invocations.
    let dup_q_input = CellViewInput {
        store: sole.cell()?.store(),
        anchor_key: &anchor_key,
        authority: &authority,
        context: sole.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &live_frontier,
        min_fresh_ms: sole.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[
            ("supply.retail.store_revenue_weekly", &rev_plan),
            ("supply.retail.store_revenue_weekly", &rev_plan),
        ],
    };
    let mut dup_eval_calls = 0_usize;
    let dup_q = evaluate_cell_cut(&dup_q_input, &mut |_, plan, ctx| {
        dup_eval_calls = dup_eval_calls.saturating_add(1);
        sole.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(
        matches!(
            dup_q,
            Err(CellError::ComputeRefused("core.compute.duplicate_question"))
        ),
        "duplicate question name must refuse: {dup_q:?}"
    );
    assert_eq!(
        dup_eval_calls, 0,
        "duplicate question refusal must run before any evaluator call"
    );
    verify_library_cut_bad_authority_refusal(sole, &rev_plan, &anchor_key)
}

fn verify_library_cut_bad_authority_refusal(
    sole: &EntityBook,
    rev_plan: &QuestionPlan,
    anchor_key: &FactKey,
) -> Result<(), String> {
    // 3. Authority binding resource mismatch must refuse fail-closed (`READ-LOCAL-1`).
    let (_, wrong_authority) = legal_entity_read_authority(sole, "LE-PARTNER");
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let live_frontier = || Ok((sole.source_cursors.clone(), sole.watermarks.clone()));
    let bad_auth_input = CellViewInput {
        store: sole.cell()?.store(),
        anchor_key,
        authority: &wrong_authority,
        context: sole.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &live_frontier,
        min_fresh_ms: sole.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let bad_auth = evaluate_cell_cut(&bad_auth_input, &mut |_, plan, ctx| {
        sole.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(
        matches!(bad_auth, Err(CellError::Encoding(_))),
        "mismatched READ-LOCAL-1 authority must refuse: {bad_auth:?}"
    );
    // 4. Mid-read feed frontier shift must refuse fail-closed (`core.compute.stale_input`).
    let calls = std::cell::Cell::new(0_u8);
    let shifting_frontier = || {
        let n = calls.get();
        calls.set(n.saturating_add(1));
        let mut wms = sole.watermarks.clone();
        if n > 0
            && let Some(wm) = wms
                .get_mut("supply.pos_sales")
                .and_then(|m| m.get_mut(FEED_PARTITION))
        {
            wm.observed = "cursor:shifted-mid-read".to_owned();
        }
        Ok((sole.source_cursors.clone(), wms))
    };
    let (_, sole_auth) = legal_entity_read_authority(sole, "LE-SOLE");
    let shifted_input = CellViewInput {
        store: sole.cell()?.store(),
        anchor_key,
        authority: &sole_auth,
        context: sole.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &shifting_frontier,
        min_fresh_ms: sole.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let shifted = evaluate_cell_cut(&shifted_input, &mut |_, plan, ctx| {
        sole.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(
        matches!(
            shifted,
            Err(CellError::ComputeRefused("core.compute.stale_input"))
        ),
        "mid-read frontier shift must refuse: {shifted:?}"
    );
    verify_library_cut_durable_cursor_mismatch(sole, rev_plan, anchor_key, &sole_auth)
}

fn verify_library_cut_durable_cursor_mismatch(
    sole: &EntityBook,
    rev_plan: &QuestionPlan,
    anchor_key: &FactKey,
    sole_auth: &CommitBinding,
) -> Result<(), String> {
    // 5. Stable durable-cursor vs QueryContext mismatch must refuse fail-closed.
    let allow_all = |_: &norn_cell::ObjectRecord| Ok(RowAccess::Read);
    let mut forged_ctx = sole.query_context();
    if let Some(parts) = forged_ctx.source_cursors.get_mut("supply.pos_sales") {
        parts.insert(FEED_PARTITION.to_owned(), "cursor:forged-stable".to_owned());
    }
    if let Some(wm) = forged_ctx
        .watermarks
        .get_mut("supply.pos_sales")
        .and_then(|m| m.get_mut(FEED_PARTITION))
    {
        wm.observed = "cursor:forged-stable".to_owned();
    }
    let forged_frontier = || {
        Ok((
            forged_ctx.source_cursors.clone(),
            forged_ctx.watermarks.clone(),
        ))
    };
    let forged_input = CellViewInput {
        store: sole.cell()?.store(),
        anchor_key,
        authority: sole_auth,
        context: forged_ctx.clone(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &forged_frontier,
        min_fresh_ms: sole.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let forged = evaluate_cell_cut(&forged_input, &mut |_, plan, ctx| {
        sole.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(matches!(
        forged,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    // 6. Claiming a consumed cursor when durable `_norn_source_cursor` has no record must refuse.
    let mut missing = EntityBook::open(
        SOLE_CELL_ID,
        "org_family_sole_prop",
        "prod",
        "sole-no-durable-cursor.sqlite",
    )?;
    seed_sole_prop(&mut missing)?;
    let (m_key, m_auth) = legal_entity_read_authority(&missing, "LE-SOLE");
    let claimed_input = CellViewInput {
        store: missing.cell()?.store(),
        anchor_key: &m_key,
        authority: &m_auth,
        context: forged_ctx.clone(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &forged_frontier,
        min_fresh_ms: missing.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", rev_plan)],
    };
    let missing_res = evaluate_cell_cut(&claimed_input, &mut |_, plan, ctx| {
        missing
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    assert!(matches!(
        missing_res,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    Ok(())
}

fn verify_library_cut_degraded_and_tampered(sole: &mut EntityBook) -> Result<(), String> {
    verify_library_cut_tampering_refusals(sole)?;
    // 4. Observed-vs-consumed cursor mismatch must mark the feed missing and withhold answers.
    let prev_obs = sole
        .watermarks
        .get_mut("supply.pos_sales")
        .and_then(|m| m.get_mut(FEED_PARTITION))
        .map(|wm| std::mem::replace(&mut wm.observed, "cursor:drifted".to_owned()));
    let drift_snap = evaluate_single_library_cut(sole, "LE-SOLE")?;
    assert!(drift_snap.missing_feeds.contains("supply.pos_sales"));
    assert!(!drift_snap.complete);
    assert_eq!(drift_snap.read_token.completeness, Completeness::Partial);
    assert!(drift_snap.answers.is_none());
    if let (Some(obs), Some(wm)) = (
        prev_obs,
        sole.watermarks
            .get_mut("supply.pos_sales")
            .and_then(|m| m.get_mut(FEED_PARTITION)),
    ) {
        wm.observed = obs;
    }
    // 5. Observed watermark on partition B with no consumed cursor must withhold answers.
    let hb_ms = sole.now().as_millis();
    if let Some(parts) = sole.watermarks.get_mut("supply.pos_sales") {
        parts.insert(
            "partition-b".to_owned(),
            Watermark {
                observed: "cursor:p-b:1".to_owned(),
                complete: true,
                heartbeat_ms: Some(hb_ms),
            },
        );
    }
    let unconsumed_part_snap = evaluate_single_library_cut(sole, "LE-SOLE")?;
    assert!(
        unconsumed_part_snap
            .missing_feeds
            .contains("supply.pos_sales")
    );
    assert!(!unconsumed_part_snap.complete);
    assert!(unconsumed_part_snap.answers.is_none());
    if let Some(parts) = sole.watermarks.get_mut("supply.pos_sales") {
        parts.remove("partition-b");
    }
    Ok(())
}

fn verify_degraded_stale_and_quarantined_view(sole: &mut EntityBook) -> Result<(), String> {
    verify_library_cut_degraded_and_tampered(sole)?;

    let saved_wm = sole.watermarks.remove("supply.pos_sales");
    let lib_degraded = evaluate_single_library_cut(sole, "LE-SOLE")?;
    assert!(lib_degraded.missing_feeds.contains("supply.pos_sales"));
    assert!(!lib_degraded.complete);
    assert!(lib_degraded.answers.is_none());
    let degraded = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert!(degraded.missing_feed_coverage.contains("supply.pos_sales"));
    assert_eq!(degraded.entity_net_revenue, None);
    assert_eq!(degraded.entity_pos_feed_revenue, None);
    if let Some(wm) = saved_wm {
        sole.watermarks.insert("supply.pos_sales".to_owned(), wm);
    }

    let stale_hb = sole.now().as_millis().saturating_sub(120_000);
    let prev_hb = sole
        .watermarks
        .get_mut("supply.pos_sales")
        .and_then(|m| m.get_mut(FEED_PARTITION))
        .and_then(|wm| wm.heartbeat_ms.replace(stale_hb));
    let lib_stale = evaluate_single_library_cut(sole, "LE-SOLE")?;
    assert!(lib_stale.missing_feeds.contains("supply.pos_sales"));
    assert!(!lib_stale.complete);
    assert!(lib_stale.answers.is_none());
    let stale_q = sole.answer("supply.retail.store_revenue_weekly")?;
    assert_eq!(stale_q.freshness, Freshness::Stale);
    let stale_view = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert!(
        stale_view
            .missing_feed_coverage
            .contains("supply.pos_sales")
    );
    assert_eq!(stale_view.entity_net_revenue, None);
    if let (Some(hb), Some(wm)) = (
        prev_hb,
        sole.watermarks
            .get_mut("supply.pos_sales")
            .and_then(|m| m.get_mut(FEED_PARTITION)),
    ) {
        wm.heartbeat_ms = Some(hb);
    }
    verify_quarantine_promotion_and_single_book_guard(sole)
}

#[test]
fn s1_read_only_combined_management_view_across_two_cells() -> Result<(), String> {
    let mut sole = EntityBook::open(SOLE_CELL_ID, "org_family_sole_prop", "prod", "sole.sqlite")?;
    let mut partner = EntityBook::open(
        PARTNER_CELL_ID,
        "org_family_partnership",
        "prod",
        "partner.sqlite",
    )?;
    seed_sole_prop(&mut sole)?;
    seed_partnership(&mut partner)?;
    seed_retail_operations(&mut sole, &mut partner)?;
    seed_store_commercial_rollups(&mut sole, &mut partner)?;
    ingest_baseline_entity_feeds(&mut sole, &mut partner)?;

    // Exercise the shipped domain-agnostic `norn_cell::combined_view` library read path directly.
    let _lib_report = verify_library_combined_view_report(&sole, &partner)?;

    let cell_view = CombinedManagementView {
        view_kind: "combined_management_view",
        is_consolidated_financial_statement: false,
        read_only: true,
        entities: vec![
            project_entity_frontier(&sole, "LE-SOLE", &mut |q| sole.answer(q))?,
            project_entity_frontier(&partner, "LE-PARTNER", &mut |q| partner.answer(q))?,
        ],
    };
    assert_eq!(cell_view.view_kind, "combined_management_view");
    assert!(!cell_view.is_consolidated_financial_statement);
    assert!(cell_view.read_only);

    let sole_snap = cell_view.entities.first().ok_or("missing sole snap")?;
    assert_eq!(sole_snap.cell_id, SOLE_CELL_ID);
    assert_eq!(sole_snap.book_code, "BOOK-SOLE-EGP");
    assert_eq!(sole_snap.currency, "EGP");
    assert_eq!(sole_snap.definition_version, 1);
    assert_eq!(sole_snap.open_feed_findings, 0);
    assert_eq!(sole_snap.pending_inbox_quarantined, 0);
    assert!(!sole_snap.complete);
    assert_eq!(sole_snap.entity_net_revenue.as_deref(), Some("10000"));
    assert_eq!(sole_snap.entity_pos_feed_revenue.as_deref(), Some("6000"));
    assert_eq!(sole_snap.entity_gross_margin, None);
    assert_eq!(
        sole_snap.store_gross_margin_rates.get("ST-SOLE-CFC-1"),
        Some(&Some("0.666667".to_owned()))
    );
    assert_eq!(
        sole_snap.store_gross_margin_rates.get("ST-SOLE-CFC-2"),
        Some(&None)
    );
    assert_eq!(sole_snap.entity_on_hand_verified, None);
    assert_eq!(sole_snap.entity_availability_verified, None);
    assert!(sole_snap.missing_cost_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.on_hand_issue_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.on_hand_issue_stores.contains("ST-SOLE-CFC-3"));
    assert!(sole_snap.on_hand_issue_stores.contains("ST-SOLE-ZAM"));

    let partner_snap = cell_view.entities.get(1).ok_or("missing partner snap")?;
    assert_eq!(partner_snap.cell_id, PARTNER_CELL_ID);
    assert_ne!(partner_snap.cell_id, sole_snap.cell_id);
    assert_eq!(partner_snap.book_code, "BOOK-PARTNER-USD");
    assert_eq!(partner_snap.currency, "USD");
    assert_eq!(partner_snap.definition_version, 1);
    assert!(partner_snap.complete);
    assert_eq!(partner_snap.entity_net_revenue.as_deref(), Some("3200"));
    assert_eq!(partner_snap.entity_gross_margin.as_deref(), Some("1200"));
    assert_eq!(
        partner_snap.store_gross_margin_rates.get("ST-PART-CFC-1"),
        Some(&Some("0.375".to_owned()))
    );
    assert_eq!(
        partner_snap.entity_pos_feed_revenue.as_deref(),
        Some("3200")
    );
    assert_eq!(partner_snap.entity_on_hand_verified, Some(true));
    assert_eq!(partner_snap.entity_availability_verified, Some(true));

    #[cfg(all(feature = "duckdb", feature = "native"))]
    verify_duckdb_combined_view_parity(&sole, &partner, &cell_view)?;
    verify_degraded_stale_and_quarantined_view(&mut sole)
}
