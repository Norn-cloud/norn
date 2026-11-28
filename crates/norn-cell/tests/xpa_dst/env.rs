//! Shared real-cell test harness environment over `registry/packs/supply-lab/`
//! for Whole-Business xP&A DST, fault injection, OLAP parity, and forks.
//!
//! Every write goes through `norn_kernel::commit_action` with real admitted
//! definitions (omitting server-derived fields per `ORIGIN-DATAFLOW-1`), and
//! every feed row flows through real 5-argument `capture_source_page` with
//! B6 `(mapping_id, dataset)` feed keys (`QUERY-FRESH-1`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::compute::{Frontier, QueryContext, QueryResult, Watermark};
use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::source_cursor::CursorAdvanceSpec;
use norn_cell::{
    AdmittedDefinitions, CellStore, InstalledDefinitionInput, LedgerHead, MappingOutcome, Model,
    SqliteCell, StoreOptions,
};
use norn_checker::EntityArtifactSetContext;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_checker::transform::TransformPlan;
use norn_ir::cell_id::CellId;
use norn_ir::data_quality::DataQualityFindingStatus;
use norn_ir::provenance::UtcTimestamp;
use norn_ir::{ArtifactEnvelope, LawRoot, QuestionIR, RuntimeValue};
use norn_kernel::authz::CommitBinding;
use norn_kernel::cell_host::{FactKey, JsonObject};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, DecisionTurn, FactEvent, LogicalTime,
    ObservationEvent, ObservationId, Observed, Provenance, commit_action,
};
use serde_json::{Value, json};
use tempfile::TempDir;

pub type RetailCell = SqliteCell<FactEvent, JsonObject, JsonObject>;
pub type MapCell = SqliteCell<ObservationEvent, norn_ir::RuntimeObject, norn_ir::RuntimeObject>;
pub type Watermarks = BTreeMap<String, BTreeMap<String, Watermark>>;

pub const FEED_PARTITION: &str = "MALL-CFC";
pub const POS_FEED_ID: &str = "supply.pos_sales.mapping";
pub const COUNTS_FEED_ID: &str = "supply.store_counts.mapping";
pub const RECEIPTS_FEED_ID: &str = "supply.retail_receipts.mapping";
pub const COSTS_FEED_ID: &str = "supply.unit_costs.mapping";

pub const REQUIRED_FEEDS: [&str; 4] =
    [POS_FEED_ID, COUNTS_FEED_ID, RECEIPTS_FEED_ID, COSTS_FEED_ID];

pub const EVIDENCE_TYPES: [&str; 4] = [
    "supply.PosSaleEvidence",
    "supply.StoreCountEvidence",
    "supply.RetailReceiptEvidence",
    "supply.UnitCostEvidence",
];

pub const FEED_MAPPINGS: [(&str, &str); 4] = [
    (POS_FEED_ID, "supply.PosSaleEvidence"),
    (COUNTS_FEED_ID, "supply.StoreCountEvidence"),
    (RECEIPTS_FEED_ID, "supply.RetailReceiptEvidence"),
    (COSTS_FEED_ID, "supply.UnitCostEvidence"),
];

pub const RETAIL_QUESTIONS: [&str; 7] = [
    "supply.retail.store_revenue_weekly",
    "supply.retail.store_gross_margin_weekly",
    "supply.retail.store_gross_margin_rate_weekly",
    "supply.retail.store_sell_through_weekly",
    "supply.retail.store_weeks_of_supply_weekly",
    "supply.retail.store_gmroi_weekly",
    "supply.retail.pos_feed_revenue_weekly",
];

pub const RETAIL_TRANSFORMS: [&str; 3] = [
    "supply.retail.store_sales_performance",
    "supply.retail.store_lot_aging",
    "supply.retail.entity_management_summary",
];

/// Fixed 32-byte keyed handle key for `DATA-VALUES-1` combined-view cuts.
pub const TEST_HANDLE_KEY: [u8; 32] = [0x5A; 32];
/// Live feed heartbeat freshness policy (`60s`), matching `retail.coverage.json`.
pub const FEED_MAX_AGE_MS: u64 = 60_000;
/// Anchor clock (`2026-09-07T12:00:00Z`) after W1..W9 retail events.
pub const DEFAULT_CLOCK_MS: u64 = 1_788_782_400_000;

/// Allow-all test authorizer for governed commits inside DST cells.
pub struct AllowAll;

impl ActionAuthorizer for AllowAll {
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

/// Outcome of a governed action call.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed,
    Replayed,
    Refused(String, String),
}

/// Admitted `tracking + ml + supply-lab` pack cached once per test binary.
pub struct RetailPack {
    pub candidate: Vec<ArtifactEnvelope>,
    pub pool: Vec<ArtifactEnvelope>,
    pub definitions: AdmittedDefinitions,
    pub canonical_set: String,
    pub receipt: String,
    pub receipt_sha256: String,
}

#[must_use]
pub fn repo_root() -> PathBuf {
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
    let core = repo_root().join("registry/core/artifacts");
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
        .map(|artifact| {
            let wire = serde_json::to_value(artifact).map_err(|e| format!("{e}"))?;
            RuntimeValue::from_json(wire).map_err(|e| format!("{e:?}"))
        })
        .collect()
}

fn build_retail_pack() -> Result<RetailPack, String> {
    let mut candidate = Vec::new();
    for dir in ["tracking", "ml", "supply-lab"] {
        candidate.extend(read_envelopes(
            &repo_root()
                .join("registry/packs")
                .join(dir)
                .join("artifacts"),
        )?);
    }
    let base = core_base()?;
    let mut pool = base.clone();
    pool.extend_from_slice(&candidate);
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

/// Cached admitted `supply-lab` retail pack.
///
/// # Errors
/// Returns an error if the pack artifacts cannot be loaded or admitted.
pub fn retail_pack() -> Result<&'static RetailPack, String> {
    static PACK: OnceLock<Result<RetailPack, String>> = OnceLock::new();
    PACK.get_or_init(build_retail_pack)
        .as_ref()
        .map_err(Clone::clone)
}

impl RetailPack {
    #[must_use]
    pub fn model(&self) -> &Model {
        self.definitions.model()
    }

    #[must_use]
    pub fn install_input(&self) -> InstalledDefinitionInput {
        InstalledDefinitionInput {
            canonical_set: self.canonical_set.clone(),
            registry: Vec::new(),
            receipt_sha256: self.receipt_sha256.clone(),
            admission_receipt: Some(self.receipt.clone()),
            readmission: None,
            diff: json!({"suite": "xpa-dst-harness"}),
            installed_at: 1,
            layout: None,
        }
    }

    /// Admit a declared retail question from `retail.questions.json` with a
    /// configurable member bound.
    ///
    /// # Errors
    /// Returns an error if the question cannot be found or admitted.
    pub fn retail_question_with_bound(
        &self,
        name: &str,
        bound: usize,
    ) -> Result<QuestionPlan, String> {
        let question = self.retail_question_ir(name)?;
        self.admit_question_ir(&question, bound)
    }

    /// Admit a declared retail question with default member bound `2000`.
    ///
    /// # Errors
    /// Returns an error if the question cannot be found or admitted.
    pub fn retail_question(&self, name: &str) -> Result<QuestionPlan, String> {
        self.retail_question_with_bound(name, 2_000)
    }

    /// Load a raw `QuestionIR` from `retail.questions.json`.
    ///
    /// # Errors
    /// Returns an error if the file or question is missing.
    pub fn retail_question_ir(&self, name: &str) -> Result<QuestionIR, String> {
        let text = fs::read_to_string(
            repo_root().join("registry/packs/supply-lab/questions/retail.questions.json"),
        )
        .map_err(|e| format!("retail.questions.json: {e}"))?;
        let questions: Vec<QuestionIR> =
            serde_json::from_str(&text).map_err(|e| format!("decode retail questions: {e}"))?;
        questions
            .into_iter()
            .find(|q| q.name == name)
            .ok_or_else(|| format!("missing retail question {name}"))
    }

    /// Admit an arbitrary `QuestionIR` against the derived metric space.
    ///
    /// # Errors
    /// Returns an error with diagnostic codes if admission refuses.
    pub fn admit_question_ir(
        &self,
        question: &QuestionIR,
        bound: usize,
    ) -> Result<QuestionPlan, String> {
        let registry = BTreeSet::new();
        let space = derive_metric_space(
            &ScopedView::uniform(&self.pool, &registry),
            &LawRoot::Entity(question.object_set.as_str().to_owned()),
        )
        .map_err(|e| format!("derive metric space for {}: {e:?}", question.name))?;
        space
            .admit(question, bound)
            .map_err(|e| format!("admit {}: {e:?}", question.name))
    }

    /// Admit a declared transform artifact by id.
    ///
    /// # Errors
    /// Returns an error if the transform is missing or fails admission.
    pub fn transform_plan(&self, artifact_id: &str) -> Result<TransformPlan, String> {
        let artifact = self
            .candidate
            .iter()
            .find(|a| a.artifact_id.as_str() == artifact_id)
            .ok_or_else(|| format!("missing transform {artifact_id}"))?;
        norn_checker::transform::admit(artifact, &self.pool, &BTreeSet::new())
            .map_err(|e| format!("admit transform {artifact_id}: {e:?}"))
    }
}

/// Input payload for one raw retail feed row ingested through `capture_source_page`.
pub struct FeedRow<'a> {
    pub feed_id: &'a str,
    pub mapping: &'a str,
    pub dataset: &'a str,
    pub source: &'a str,
    pub event: &'a str,
    pub record: &'a str,
    pub payload: &'a str,
}

/// One isolated, file-backed legal-entity cell with durable cursors and watermarks.
pub struct EntityCell {
    pub pack: &'static RetailPack,
    _dir: TempDir,
    pub cell_id: CellId,
    pub entity_id: String,
    pub currency: String,
    pub scope: String,
    pub org_id: String,
    pub env_id: String,
    cell: Option<RetailCell>,
    pub clock_base_ms: u64,
    pub tick: u64,
    pub calls: u64,
    pub source_cursors: Frontier,
    pub watermarks: Watermarks,
}

impl EntityCell {
    /// Open a fresh file-backed entity cell bound to `cell_id_hex` and `org_id/env_id`.
    ///
    /// # Errors
    /// Returns an error if `SQLite` open or definition installation fails.
    pub fn open(
        cell_id_hex: &str,
        entity_id: &str,
        currency: &str,
        org_id: &str,
        env_id: &str,
        clock_base_ms: u64,
    ) -> Result<Self, String> {
        let pack = retail_pack()?;
        let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let path = dir.path().join(format!("{entity_id}.sqlite"));
        let cell_id = CellId::try_from(cell_id_hex.to_owned())
            .map_err(|e| format!("cell id {cell_id_hex}: {e}"))?;
        let scope = format!("{org_id}/{env_id}");
        let engine = NativeEngine::open(&path, StoreOptions::default())
            .map_err(|e| format!("open sqlite {entity_id}: {e}"))?;
        let mut store = CellStore::from_engine_bound(engine, pack.model().clone(), &cell_id)
            .map_err(|e| format!("bind store {entity_id}: {e}"))?;
        store
            .bind("scope", &scope)
            .map_err(|e| format!("bind scope: {e}"))?;
        let mut cell = RetailCell::new(store, scope.clone(), |c| c.payload.clone())
            .map_err(|e| format!("open cell: {e}"))?;
        cell.install_definitions(pack.model().clone(), pack.install_input(), Some(0))
            .map_err(|e| format!("install definitions {entity_id}: {e}"))?;
        cell.advance_to(LogicalTime::from_millis(clock_base_ms));
        Ok(Self {
            pack,
            _dir: dir,
            cell_id,
            entity_id: entity_id.to_owned(),
            currency: currency.to_owned(),
            scope,
            org_id: org_id.to_owned(),
            env_id: env_id.to_owned(),
            cell: Some(cell),
            clock_base_ms,
            tick: 0,
            calls: 0,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        })
    }

    /// Borrow the underlying [`RetailCell`].
    ///
    /// # Errors
    /// Returns an error if the cell is temporarily checked out.
    pub fn cell(&self) -> Result<&RetailCell, String> {
        self.cell.as_ref().ok_or_else(|| "missing cell".to_owned())
    }

    /// Mutably borrow the underlying [`RetailCell`].
    ///
    /// # Errors
    /// Returns an error if the cell is temporarily checked out.
    pub fn cell_mut(&mut self) -> Result<&mut RetailCell, String> {
        self.cell.as_mut().ok_or_else(|| "missing cell".to_owned())
    }

    #[must_use]
    pub const fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(self.clock_base_ms.saturating_add(self.tick))
    }

    /// Run a closure against this cell's store viewed as a [`MapCell`].
    ///
    /// # Errors
    /// Returns an error if swapping the host wrapper fails or `f` errors.
    pub fn with_map_cell<T>(
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

    /// Execute one governed action call through `norn_kernel::commit_action`.
    ///
    /// # Errors
    /// Returns an error if the action is unknown or JSON decoding fails.
    pub fn act(&mut self, action: &str, actor: &str, input: Value) -> Result<Outcome, String> {
        self.calls = self.calls.saturating_add(1);
        let key = format!("{}-call-{}", self.org_id, self.calls);
        self.act_keyed(action, actor, input, &key)
    }

    /// Execute one governed action call with an explicit idempotency key.
    ///
    /// # Errors
    /// Returns an error if the action is unknown or JSON decoding fails.
    pub fn act_keyed(
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
        Ok(match commit_action(cell, &catalog, &call, &AllowAll) {
            Ok(ActionOutcome::Committed { .. }) => Outcome::Committed,
            Ok(ActionOutcome::Replayed { .. }) => Outcome::Replayed,
            Err(refusal) => Outcome::Refused(refusal.code().to_owned(), refusal.to_string()),
        })
    }

    /// Commit an action, failing if it was replayed or refused.
    ///
    /// # Errors
    /// Returns an error if the call does not commit.
    pub fn ok(&mut self, action: &str, actor: &str, input: Value) -> Result<(), String> {
        match self.act(action, actor, input)? {
            Outcome::Committed => Ok(()),
            Outcome::Replayed => Err(format!("{action} expected Committed, got Replayed")),
            Outcome::Refused(code, msg) => Err(format!(
                "{action} expected Committed, got Refused({code}, {msg})"
            )),
        }
    }

    /// Assert that an action refuses and returns `(code, message)`.
    ///
    /// # Errors
    /// Returns an error if the action unexpectedly commits or replays.
    pub fn refused(
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

    /// Ingest one raw source row through the real 5-argument `capture_source_page`
    /// durable cursor + mapping pipeline (`QUERY-FRESH-1`, B6 `(mapping_id, dataset)` keys).
    ///
    /// # Errors
    /// Returns an error if lease acquisition, capture, or processing fails.
    pub fn ingest_feed(&mut self, row: &FeedRow<'_>) -> Result<(String, bool), String> {
        self.tick = self.tick.saturating_add(1_000);
        let observed_at = self.now().as_millis();
        let obs = raw_obs(row.source, row.event, row.record, row.payload, observed_at)?;
        let input = map_input(row.mapping, row.dataset);
        let next_cursor = format!("cursor:{}:{}", row.feed_id, row.event);
        let clean = self.with_map_cell(|map_cell| {
            let lease = map_cell
                .acquire_source_lease(row.feed_id, "xpa-dst-sync", observed_at, 3_600_000)
                .map_err(|e| format!("lease {}: {e}", row.feed_id))?;
            let prev_cursor = map_cell
                .store()
                .read()
                .source_cursors(row.feed_id)
                .map_err(|e| format!("read cursor {}: {e}", row.feed_id))?
                .get(row.dataset)
                .map(|r| r.cursor_json.clone());
            let advance = CursorAdvanceSpec {
                connection_id: row.feed_id.to_owned(),
                dataset: row.dataset.to_owned(),
                lease_id: lease.lease_id,
                generation: lease.generation,
                now_ms: observed_at,
                expected_previous_cursor: prev_cursor,
                next_cursor: Some(next_cursor.clone()),
                renew_ttl_ms: None,
            };
            let captured = map_cell
                .capture_source_page(&[obs], Some(&input), &advance, 64, None)
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
                .complete_source_pending(row.feed_id, row.dataset, &captured.page_digest)
                .map_err(|e| format!("complete pending {}: {e}", row.feed_id))?;
            Ok(all_clean)
        })?;
        self.source_cursors
            .entry(row.feed_id.to_owned())
            .or_default()
            .insert(row.dataset.to_owned(), next_cursor.clone());
        self.watermarks
            .entry(row.feed_id.to_owned())
            .or_default()
            .insert(
                row.dataset.to_owned(),
                Watermark {
                    observed: next_cursor.clone(),
                    heartbeat_ms: Some(observed_at),
                    complete: clean,
                },
            );
        Ok((next_cursor, clean))
    }

    /// Refresh all 4 feed watermark heartbeats to `self.now()`.
    pub fn refresh_watermarks(&mut self) {
        let now_ms = self.now().as_millis();
        for parts in self.watermarks.values_mut() {
            for wm in parts.values_mut() {
                wm.heartbeat_ms = Some(now_ms);
            }
        }
    }

    /// Build a live `QueryContext` pinned to this cell's scope, clock, and cursors.
    #[must_use]
    pub fn query_context(&self) -> QueryContext {
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

    /// Evaluate a declared retail question via the exact in-cell evaluator.
    ///
    /// # Errors
    /// Returns an error if admission or evaluation fails.
    pub fn answer(&self, name: &str) -> Result<QueryResult, String> {
        let plan = self.pack.retail_question(name)?;
        self.cell()?
            .store()
            .evaluate_question(&plan, self.query_context(), |_| Ok(true))
            .map_err(|e| format!("evaluate {name}: {e}"))
    }

    /// Evaluate a declared retail transform via the exact in-cell interpreter.
    ///
    /// # Errors
    /// Returns an error if admission or evaluation fails.
    pub fn evaluate_transform(&self, artifact_id: &str) -> Result<TransformResult, String> {
        let plan = self.pack.transform_plan(artifact_id)?;
        self.cell()?
            .store()
            .evaluate_transform(&plan, "main", &self.scope, |_| Ok(RowAccess::Read))
            .map_err(|e| format!("transform {artifact_id}: {e}"))
    }

    /// Verified ledger head of this cell.
    ///
    /// # Errors
    /// Returns an error if ledger verification fails.
    pub fn head(&self) -> Result<LedgerHead, String> {
        self.cell()?
            .store()
            .verified_head()
            .map_err(|e| format!("verified_head: {e}"))
    }

    /// Count open data-quality findings for a specific evidence record.
    ///
    /// # Errors
    /// Returns an error if reading findings fails.
    pub fn open_finding_count(&self, object_type: &str, key: &str) -> Result<usize, String> {
        let findings = self
            .cell()?
            .store()
            .read()
            .observation_findings("main", object_type, key)
            .map_err(|e| format!("findings {object_type}/{key}: {e}"))?;
        Ok(findings
            .into_iter()
            .filter(|f| matches!(f.status, DataQualityFindingStatus::Open))
            .count())
    }

    /// Build the `READ-LOCAL-1` anchor key and PDP authority binding for this entity book.
    #[must_use]
    pub fn read_authority(&self) -> (FactKey, CommitBinding) {
        let key = FactKey::new("supply.LegalEntity", &self.entity_id);
        let authority = CommitBinding {
            action_id: "read".into(),
            resource_kind: "supply.LegalEntity".into(),
            resource_id: self.entity_id.clone(),
            artifact_version: Some("0.1.0".into()),
            policy_id: "supply.retail.access".into(),
            policy_version: "0.1.0".into(),
            resource_version: "1".into(),
            authorization_epoch: "1".into(),
            evaluated_at_logical_time: self.now().as_millis().to_string(),
        };
        (key, authority)
    }
}

/// Build one canonical `norn.raw_observation/v1` envelope for `capture_source_page`.
///
/// # Errors
/// Returns an error if the JSON payload fails decoding or canonical hashing.
pub fn raw_obs(
    source: &str,
    event: &str,
    record: &str,
    payload: &str,
    observed_at_ms: u64,
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
            observed_at: LogicalTime::from_millis(observed_at_ms),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => source,
            "source_id" => record,
            "event_id" => event,
            "source_version" => "1",
            "pipeline_version" => "retail-pipeline/1",
            "kind" => "upsert",
            "extracted_at" => observed_at_ms,
            "payload" => value,
            "payload_digest" => digest,
        },
    })
}

#[must_use]
pub fn map_input(mapping: &str, dataset: &str) -> MappingInput {
    MappingInput {
        mapping: mapping.to_owned(),
        dataset: dataset.to_owned(),
        connector_instance: "retail-lab".to_owned(),
        partition: FEED_PARTITION.to_owned(),
        recorded_at: UtcTimestamp("2026-09-07T12:00:00Z".to_owned()),
    }
}
