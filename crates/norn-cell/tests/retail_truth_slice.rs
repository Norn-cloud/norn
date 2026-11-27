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
//!    all refuse), and cross-scope refusal.
//! 2. **Messy retail feeds -> KPI -> `LINEAGE-SUM-1` source provenance:** POS
//!    sales, store physical counts, retail supplier receipts, and supplier unit
//!    costs ingested through real admitted mapping bundles on one `MapCell`
//!    with quarantine, redelivery idempotency, recovery, end-to-end KPI
//!    evaluation over `PosSaleEvidence`, `ContributionSource::Resolved` source
//!    drill-through, and feed-watermark withholding.
//! 3. **Holding lot aging, fail-closed count coverage, mixed-bucket missing-cost
//!    withholding, and cell-derived commercial KPIs:** exact answers over all
//!    declared retail questions and both chasm-trap-safe retail transforms;
//!    missing cost, missing counts, stale counts, future counts, and count
//!    drift all fail closed (withheld totals); depleted lots drop out of active
//!    aging cohorts.
//! 4. **Single-cut pinned read-only combined management view (`READ-LOCAL-1`):**
//!    cross-cell projection over two cells with real installed definitions and
//!    real mapped feed bundles, verifying every read matches the cell's pinned
//!    `read_envelope` frontier, open feed quarantine findings withhold totals
//!    until corrected, missing watermarks fail closed inside `evaluate_question`,
//!    explicitly labelled combined management view (never consolidated financial
//!    statements), never feeding a governed write, with exact `DuckDB` parity
//!    across revenue, gross margin, gross margin rate, and POS feed revenue when
//!    built with `--features duckdb,native`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use norn_cell::compute::lineage::{ContributionSource, LineageDisclosure};
use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::compute::{Freshness, Frontier, QueryContext, QueryResult, Watermark};
use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
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
use norn_kernel::read_envelope::{Completeness, ReadRequirement, ReadSemantics};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellHost as _, DecisionTurn, FactEvent,
    LogicalTime, ObservationEvent, ObservationId, Observed, Provenance, commit_action,
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
            &LawRoot::Entity(question.object_set.clone()),
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
    scope: String,
    org_id: String,
    env_id: String,
    cell: Option<RetailCell>,
    tick: u64,
    calls: u64,
    source_cursors: Frontier,
    watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
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

struct FeedRow<'a> {
    feed_id: &'a str,
    mapping: &'a str,
    dataset: &'a str,
    source: &'a str,
    event: &'a str,
    record: &'a str,
    payload: &'a str,
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

    fn ingest_feed(&mut self, row: &FeedRow<'_>) -> Result<String, String> {
        self.tick = self.tick.saturating_add(1_000);
        let observed_at = self.now().as_millis();
        let obs = raw_obs(row.source, row.event, row.record, row.payload)?;
        let input = map_input(row.mapping, row.dataset);
        let bundle = self.with_map_cell(|map_cell| {
            let (bundle, _) = map_cell
                .inject_mapping_bundle(&obs, &input)
                .map_err(|e| format!("inject {}/{}: {e}", row.feed_id, row.record))?;
            map_cell
                .process_mapping_bundle(&bundle)
                .map_err(|e| format!("process {}/{}: {e}", row.feed_id, row.record))?;
            Ok(bundle)
        })?;
        self.source_cursors
            .entry(row.feed_id.to_owned())
            .or_default()
            .insert(FEED_PARTITION.to_owned(), bundle.clone());
        self.watermarks
            .entry(row.feed_id.to_owned())
            .or_default()
            .insert(
                FEED_PARTITION.to_owned(),
                Watermark {
                    observed: bundle.clone(),
                    heartbeat_ms: Some(observed_at),
                    complete: true,
                },
            );
        Ok(bundle)
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
            other => Err(format!("{action} expected Committed, got {other:?}")),
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
            other => Err(format!("{action} expected Refused, got {other:?}")),
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
        other => format!("{other:?}"),
    }
}

fn bucket_decimal(result: &QueryResult, parts: &[&str]) -> Option<Decimal> {
    result.rows.iter().find_map(|(key, row)| {
        parts
            .iter()
            .all(|part| key.contains(part))
            .then_some(row.value)
            .flatten()
    })
}

fn bucket_total(result: &QueryResult, parts: &[&str]) -> Option<String> {
    bucket_decimal(result, parts).map(|d| d.canonical())
}

/// Fail-closed lookup on a `cost_status`-partitioned `QueryResult`:
/// returns `None` (withheld) if ANY `missing_cost` slice is present for `parts`,
/// preventing partial totals from a mixed costed + uncosted bucket.
fn resolve_costed_bucket(result: &QueryResult, parts: &[&str]) -> Option<Decimal> {
    let has_missing = result
        .rows
        .keys()
        .any(|key| key.contains("missing_cost") && parts.iter().all(|part| key.contains(part)));
    if has_missing {
        return None;
    }
    result.rows.iter().find_map(|(key, row)| {
        (key.contains("costed") && parts.iter().all(|part| key.contains(part)))
            .then_some(row.value)
            .flatten()
    })
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
    Ok(if negative { -magnitude } else { magnitude })
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

fn seed_sole_cfc1_lots(sole: &mut EntityBook) -> Result<(), String> {
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "60", "remaining_qty": "60", "unit": "each",
            "received_at": "2026-07-08T09:00:00Z", "as_of": "2026-07-08T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "25",
            "as_of": "2026-09-04T18:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W5", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "40", "remaining_qty": "35", "unit": "each",
            "received_at": "2026-08-05T09:00:00Z", "as_of": "2026-09-04T18:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-DEPLETED", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-RING-18K",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "unit": "each",
            "received_at": "2026-07-15T09:00:00Z", "as_of": "2026-07-15T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-DEPLETED", "remaining_qty": "0",
            "as_of": "2026-08-20T09:00:00Z", "unit_cost": "100", "stock_value": "0"
        }),
    )
}

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
    // Kernel derives cogs_amount = sold_qty * applied_unit_cost (3000 & 1000) and gross_margin (3000 & 2000).
    sole.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S1-1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "30", "unit": "each", "net_revenue": "6000", "applied_unit_cost": "100",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;
    sole.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S1-2", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "10", "unit": "each", "net_revenue": "3000", "applied_unit_cost": "100",
            "cogs_amount": "0", "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-03T16:00:00Z"
        }),
    )
}

fn seed_sole_edge_stores(sole: &mut EntityBook) -> Result<(), String> {
    // ST-SOLE-CFC-2: MIXED costed + uncosted bucket AND NO physical count.
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S2-COSTED", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "unit": "each",
            "received_at": "2026-08-20T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.stock_lot.record_uncosted",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S2-NOCOST", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "20", "remaining_qty": "15", "unit": "each",
            "received_at": "2026-08-20T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "missing_cost", "stock_value": "0", "currency": "EGP"
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
            "cost_status": "missing_cost",
            "currency": "EGP", "sold_at": "2026-09-03T12:00:00Z"
        }),
    )?;
    // ST-SOLE-CFC-3: STALE COUNT (>7 days old) + on-hand drift (28 vs 20).
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S3-STALE", "store_id": "ST-SOLE-CFC-3", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "30", "remaining_qty": "20", "unit": "each",
            "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
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
            "received_qty": "15", "remaining_qty": "15", "unit": "each",
            "received_at": "2026-08-18T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
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
    )?;
    Ok(())
}

fn seed_partner_operations(partner: &mut EntityBook) -> Result<(), String> {
    partner.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-P1-W3", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "owner_entity_id": "LE-PARTNER", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "20", "remaining_qty": "12", "unit": "each",
            "received_at": "2026-07-22T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "250", "stock_value": "0", "currency": "USD"
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
    )?;
    Ok(())
}

fn seed_retail_operations(sole: &mut EntityBook, partner: &mut EntityBook) -> Result<(), String> {
    seed_sole_cfc1(sole)?;
    seed_sole_edge_stores(sole)?;
    seed_partner_operations(partner)
}

// ------------------------------------------------------------------ Test 1

fn assert_cost_binding_refusals(sole: &mut EntityBook) -> Result<(), String> {
    // 1. Forged `applied_unit_cost` (50 != 100 on COST-DRESS-1) refuses.
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
    // 2. Wrong `costed_ref` refuses.
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
    // 3. SKU without any registered `UnitCost` (`SKU-RING-18K` on Cell A) refuses.
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
    // 4. Future-effective `UnitCost` (`effective_from: 2026-10-01 > sold_at: 2026-09-01`) refuses!
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
    // ID-UNIQUE-1: duplicate StoreCount for `(store_id, sku_id, quantity_basis)` refuses.
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

    // LAW-LINK-MANY-1: closing a store with live stock refuses; closing an empty store succeeds.
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-HOLD", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "unit": "each",
            "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-08-01T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
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

    // Cross-scope query on Cell A using Cell B's scope refuses.
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
    sole.ok(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-OTHER-EGP", "legal_name": "Other EGP Entity",
            "entity_kind": "corporation", "book_code": "BOOK-OTHER-EGP", "functional_currency": "EGP"
        }),
    )?;
    sole.refused(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-WRONG-OWNER", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-OTHER-EGP", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "unit": "each",
            "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-08-01T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "0", "currency": "EGP"
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
        bucket_total(&before_fix, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
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
        bucket_total(&after_fix, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
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
        .keys()
        .find(|k| k.contains("ST-SOLE-CFC-1") && k.contains("EGP") && k.contains("2026-08-31"))
        .cloned()
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
            _ => None,
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

    // Missing feed watermark fails closed inside BOTH `evaluate_question` and `check_feed_coverage`.
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
        CLOCK_MS.saturating_sub(MAX_COUNT_AGE_MS),
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
    MissingCount,
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
        let counted = match count.values.get("on_hand_qty") {
            Some(RuntimeValue::Number(d)) => *d,
            _ => Decimal::from(0_i64),
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
    let has_future = findings.iter().any(|f| {
        f.store_id == store_id
            && f.sku_id == sku_id
            && matches!(f.kind, CountCoverageKind::FutureCount { .. })
    });
    let Some((as_of_ms, as_of, counted)) = on_hand else {
        if !has_future && derived > Decimal::from(0_i64) {
            findings.push(CountCoverageFinding {
                store_id: store_id.to_owned(),
                sku_id: sku_id.to_owned(),
                kind: CountCoverageKind::MissingCount,
            });
        }
        return;
    };
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
    if let Some((a_ms, a_as_of, a_qty)) = avail {
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
        if *a_qty > *counted {
            findings.push(CountCoverageFinding {
                store_id: store_id.to_owned(),
                sku_id: sku_id.to_owned(),
                kind: CountCoverageKind::AvailableExceedsOnHand {
                    available_qty: a_qty.canonical(),
                    on_hand_qty: counted.canonical(),
                },
            });
        }
    }
}

/// Fail-closed physical count coverage & staleness check across the union of all
/// live holdings (`remaining_qty > 0`) and all `on_hand` / `available` `StoreCount`s.
/// (Wires into `#1905` `RequiredSources` freshness once B6 lands.)
fn evaluate_count_coverage(
    book: &EntityBook,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<Vec<CountCoverageFinding>, String> {
    let reader = book.cell()?.store().read();
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
    Ok(findings)
}

fn verify_retail_questions(sole: &EntityBook) -> Result<(Decimal, Decimal, Decimal), String> {
    let availability = sole.answer("supply.retail.store_availability_weekly")?;
    assert_eq!(
        bucket_total(&availability, &["ST-SOLE-CFC-1", "each", "2026-08-31"]).as_deref(),
        Some("56")
    );
    let on_hand = sole.answer("supply.retail.store_on_hand_weekly")?;
    let s1_on_hand = bucket_decimal(&on_hand, &["ST-SOLE-CFC-1", "each", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 on_hand")?;
    assert_eq!(
        s1_on_hand.canonical(),
        "60",
        "derived lot on-hand = 25 + 35"
    );

    let stock_val = sole.answer("supply.retail.store_stock_value_weekly")?;
    let s1_stock_val = resolve_costed_bucket(&stock_val, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 stock_value")?;
    assert_eq!(s1_stock_val.canonical(), "6000");
    assert_eq!(
        resolve_costed_bucket(&stock_val, &["ST-SOLE-CFC-2", "EGP", "2026-08-31"]),
        None,
        "mixed costed + uncosted stock value bucket must withhold"
    );

    let aging_q = sole.answer("supply.retail.lot_received_aging_weekly")?;
    assert_eq!(
        bucket_total(&aging_q, &["ST-SOLE-CFC-1", "each", "2026-07-06"]).as_deref(),
        Some("60")
    );
    assert_eq!(
        bucket_total(&aging_q, &["ST-SOLE-CFC-1", "each", "2026-08-03"]).as_deref(),
        Some("40")
    );

    let sales_q = sole.answer("supply.retail.store_sales_weekly")?;
    let s1_sold = bucket_decimal(&sales_q, &["ST-SOLE-CFC-1", "each", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 sales")?;
    assert_eq!(s1_sold.canonical(), "40");

    let rev_q = sole.answer("supply.retail.store_revenue_weekly")?;
    assert_eq!(
        bucket_total(&rev_q, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
        Some("9000")
    );

    let gm_sum = sole.answer("supply.retail.store_gross_margin_weekly")?;
    let s1_gm = resolve_costed_bucket(&gm_sum, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 gross margin")?;
    assert_eq!(s1_gm.canonical(), "5000");
    assert_eq!(
        resolve_costed_bucket(&gm_sum, &["ST-SOLE-CFC-2", "EGP", "2026-08-31"]),
        None
    );

    let gm_rate = sole.answer("supply.retail.store_gross_margin_rate_weekly")?;
    let s1_rate = resolve_costed_bucket(&gm_rate, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 gross margin rate")?;
    assert_eq!(s1_rate.canonical(), "0.555556", "ratio-of-sums 5000/9000");
    assert_eq!(
        resolve_costed_bucket(&gm_rate, &["ST-SOLE-CFC-2", "EGP", "2026-08-31"]),
        None
    );
    Ok((s1_on_hand, s1_stock_val, s1_sold))
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

fn verify_retail_transforms_and_kpis(
    sole: &EntityBook,
    s1_on_hand: Decimal,
    s1_stock_val: Decimal,
    s1_sold: Decimal,
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
    let s1_margin = match s1_sales.values.get("effective_gross_margin") {
        Some(RuntimeValue::Number(d)) => *d,
        other => return Err(format!("expected numeric margin, got {other:?}")),
    };
    assert_eq!(s1_margin.canonical(), "5000");

    let aging_tf =
        sole.evaluate_transform("supply.retail.store_lot_aging", |_| Ok(RowAccess::Read))?;
    assert!(
        !aging_tf
            .rows
            .iter()
            .any(|r| r.values.get("sku_id").map(scalar).as_deref() == Some("SKU-RING-18K")),
        "fully depleted lot must not appear in active lot aging cohorts"
    );
    let mut s1_live_received = Decimal::from(0_i64);
    for row in aging_tf.rows.iter().filter(|r| {
        r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1")
            && r.values.get("sku_id").map(scalar).as_deref() == Some("SKU-DRESS")
    }) {
        if let Some(RuntimeValue::Number(qty)) = row.values.get("received_qty") {
            s1_live_received = s1_live_received.checked_add(*qty).ok_or("received sum")?;
        }
    }
    assert_eq!(s1_live_received.canonical(), "100");

    assert_eq!(
        ratio(
            decimal_to_scaled_i128(s1_sold)?,
            decimal_to_scaled_i128(s1_live_received)?
        ),
        Some(400_000)
    );
    let weekly_sales = ratio(
        decimal_to_scaled_i128(s1_sold)?,
        4_i128.saturating_mul(FACTOR),
    )
    .ok_or("weekly")?;
    assert_eq!(
        ratio(decimal_to_scaled_i128(s1_on_hand)?, weekly_sales),
        Some(6_000_000)
    );
    assert_eq!(
        ratio(
            decimal_to_scaled_i128(s1_margin)?,
            decimal_to_scaled_i128(s1_stock_val)?
        ),
        Some(833_333)
    );
    verify_transform_withholding(sole, &sales_tf, &aging_tf)
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

    // Immutable `received_at` cannot be rewritten via `supply.stock_lot.update_balance`.
    sole.refused(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "remaining_qty": "20", "as_of": "2026-09-06T09:00:00Z",
            "unit_cost": "100", "stock_value": "0", "received_at": "2026-09-01T09:00:00Z"
        }),
    )?;

    let (s1_on_hand, s1_stock_val, s1_sold) = verify_retail_questions(&sole)?;
    verify_retail_transforms_and_kpis(&sole, s1_on_hand, s1_stock_val, s1_sold)?;

    let findings = evaluate_count_coverage(&sole, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    assert!(
        !findings.iter().any(|f| f.store_id == "ST-SOLE-CFC-1"),
        "{findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.store_id == "ST-SOLE-CFC-2"
                && matches!(f.kind, CountCoverageKind::MissingCount)),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.store_id == "ST-SOLE-CFC-3"
            && matches!(f.kind, CountCoverageKind::StaleCount { .. })),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.store_id == "ST-SOLE-CFC-3"
            && matches!(f.kind, CountCoverageKind::OnHandDrift { .. })),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.store_id == "ST-SOLE-ZAM"
            && matches!(f.kind, CountCoverageKind::FutureCount { .. })),
        "{findings:?}"
    );
    Ok(())
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
    definition_version: u64,
    complete: bool,
    open_feed_findings: usize,
    missing_feed_coverage: BTreeSet<String>,
    missing_cost_stores: BTreeSet<String>,
    count_issue_stores: BTreeSet<String>,
    /// Withheld (`None`) whenever row/feed coverage or quarantine findings are incomplete.
    entity_net_revenue: Option<String>,
    /// Withheld (`None`) whenever any store has missing cost or incomplete coverage/quarantine.
    entity_gross_margin: Option<String>,
    /// Ratio-of-sums gross margin rate (`scale-6` canonical) withheld whenever margin is withheld.
    entity_gross_margin_rate: Option<String>,
    /// POS feed revenue from mapped `PosSaleEvidence`, withheld on incomplete feed/quarantine.
    entity_pos_feed_revenue: Option<String>,
    /// Withheld (`None`) whenever any store has missing, stale, future, drifted, or over-available counts.
    entity_on_hand_verified: Option<bool>,
    /// Gated on the same count coverage so `available` never publishes without valid physical counts.
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

fn count_open_evidence_findings(book: &EntityBook) -> Result<usize, String> {
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
    Ok(open)
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
            payload: r#"{"receipt_line_id":"RR-SOLE-1","store_code":"ST-SOLE-CFC-1","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","received_qty":60,"damaged_qty":0,"uom":"PCS","unit_cost":100,"ccy":"EGP","received_at":"2026-07-08T09:00:00Z"}"#,
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
        sole.ingest_feed(row)?;
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
        partner.ingest_feed(row)?;
    }
    Ok(())
}

fn sum_currency_buckets(result: &QueryResult, currency: &str) -> Result<Decimal, String> {
    let mut sum = Decimal::from(0_i64);
    for (bucket, row) in &result.rows {
        if bucket.contains(currency)
            && let Some(amount) = row.value
        {
            sum = sum.checked_add(amount).ok_or("currency bucket overflow")?;
        }
    }
    Ok(sum)
}

fn format_scaled_ratio(scaled: i128) -> Result<String, String> {
    Decimal::parse(&format!("{scaled}e-6"))
        .map(|d| d.canonical())
        .map_err(|e| format!("format ratio {scaled}: {e}"))
}

fn read_entity_pin(
    book: &EntityBook,
    legal_entity_id: &str,
) -> Result<(String, String, String, String, u64, u64), String> {
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
    let envelope = book
        .cell()?
        .read_envelope(&key, &authority)
        .map_err(|e| format!("read_envelope {legal_entity_id}: {e}"))?;
    assert_eq!(envelope.token.completeness, Completeness::Complete);
    assert_eq!(envelope.token.semantics, ReadSemantics::Exact);
    envelope
        .token
        .validate(&envelope.token.context, ReadRequirement::Exact)
        .map_err(|e| format!("validate token {legal_entity_id}: {e}"))?;
    let entity_obj = book
        .cell()?
        .store()
        .read()
        .object("main", "supply.LegalEntity", legal_entity_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| format!("missing {legal_entity_id}"))?;
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
    Ok((
        envelope.token.context.frontier.cell.as_str().to_owned(),
        envelope.token.context.frontier.world,
        book_code,
        currency,
        envelope.token.context.frontier.revision,
        envelope.token.context.definition_pin.version,
    ))
}

fn detect_missing_cost_stores(
    gm_q: &QueryResult,
    gm_rate_q: &QueryResult,
    lots: &TransformResult,
) -> (BTreeSet<String>, bool) {
    let mut stores = BTreeSet::new();
    for key_str in gm_q.rows.keys().chain(gm_rate_q.rows.keys()) {
        if key_str.contains("missing_cost") {
            for store in [
                "ST-SOLE-CFC-1",
                "ST-SOLE-CFC-2",
                "ST-SOLE-CFC-3",
                "ST-SOLE-ZAM",
                "ST-PART-CFC-1",
            ] {
                if key_str.contains(store) {
                    stores.insert(store.to_owned());
                }
            }
        }
    }
    for row in &lots.rows {
        if row.values.get("cost_complete") != Some(&RuntimeValue::Bool(true)) {
            stores.insert(row.values.get("store_id").map(scalar).unwrap_or_default());
        }
    }
    let has_slice = gm_q.rows.keys().any(|k| k.contains("missing_cost"))
        || gm_rate_q.rows.keys().any(|k| k.contains("missing_cost"));
    (stores, has_slice)
}

fn project_entity_frontier(
    book: &EntityBook,
    legal_entity_id: &str,
    eval_question: &mut dyn FnMut(&str) -> Result<QueryResult, String>,
) -> Result<EntityFrontierSnapshot, String> {
    let (cell_id, world, book_code, currency, pinned_rev, definition_version) =
        read_entity_pin(book, legal_entity_id)?;
    let lots = book.evaluate_transform("supply.retail.store_lot_aging", |_| Ok(RowAccess::Read))?;
    let rev_q = eval_question("supply.retail.store_revenue_weekly")?;
    let gm_q = eval_question("supply.retail.store_gross_margin_weekly")?;
    let gm_rate_q = eval_question("supply.retail.store_gross_margin_rate_weekly")?;
    let pos_feed_q = eval_question("supply.retail.pos_feed_revenue_weekly")?;
    let open_feed_findings = count_open_evidence_findings(book)?;
    let count_issues = evaluate_count_coverage(book, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    if lots.revision.commits != pinned_rev
        || rev_q.known_as_of != pinned_rev
        || gm_q.known_as_of != pinned_rev
        || gm_rate_q.known_as_of != pinned_rev
        || pos_feed_q.known_as_of != pinned_rev
        || book.head()?.commits != pinned_rev
    {
        return Err(format!(
            "frontier moved during projection of {legal_entity_id}"
        ));
    }

    let missing_feed_coverage = check_feed_coverage(
        &book.source_cursors,
        &book.watermarks,
        CLOCK_MS.saturating_sub(MAX_COUNT_AGE_MS),
    );
    let rev_sum = sum_currency_buckets(&rev_q, &currency)?;
    let gm_sum = sum_currency_buckets(&gm_q, &currency)?;
    let pos_feed_sum = sum_currency_buckets(&pos_feed_q, &currency)?;
    let (missing_cost_stores, has_missing_cost_slice) =
        detect_missing_cost_stores(&gm_q, &gm_rate_q, &lots);
    let count_issue_stores: BTreeSet<String> =
        count_issues.into_iter().map(|f| f.store_id).collect();
    let feeds_and_rows_complete = lots.complete
        && rev_q.complete
        && gm_q.complete
        && gm_rate_q.complete
        && pos_feed_q.complete
        && open_feed_findings == 0
        && missing_feed_coverage.is_empty();
    let cost_complete = missing_cost_stores.is_empty() && !has_missing_cost_slice;
    let counts_verified = feeds_and_rows_complete && count_issue_stores.is_empty();
    let gm_rate_str = if feeds_and_rows_complete && cost_complete {
        ratio(
            decimal_to_scaled_i128(gm_sum)?,
            decimal_to_scaled_i128(rev_sum)?,
        )
        .map(format_scaled_ratio)
        .transpose()?
    } else {
        None
    };

    Ok(EntityFrontierSnapshot {
        cell_id,
        legal_entity_id: legal_entity_id.to_owned(),
        book_code,
        currency,
        scope: book.scope.clone(),
        world,
        revision: pinned_rev,
        definition_version,
        complete: counts_verified && cost_complete,
        open_feed_findings,
        missing_feed_coverage,
        missing_cost_stores,
        count_issue_stores,
        entity_net_revenue: feeds_and_rows_complete.then(|| rev_sum.canonical()),
        entity_gross_margin: (feeds_and_rows_complete && cost_complete).then(|| gm_sum.canonical()),
        entity_gross_margin_rate: gm_rate_str,
        entity_pos_feed_revenue: feeds_and_rows_complete.then(|| pos_feed_sum.canonical()),
        entity_on_hand_verified: counts_verified.then_some(true),
        entity_availability_verified: counts_verified.then_some(true),
    })
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

    let sync_tenant = |book: &EntityBook| -> Result<TenantId, String> {
        let store = book.cell()?.store();
        let tenant = TenantId::of_scope(&book.scope);
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
    };

    let sole_tenant = sync_tenant(sole)?;
    let partner_tenant = sync_tenant(partner)?;
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

fn verify_degraded_and_quarantined_view(sole: &mut EntityBook) -> Result<(), String> {
    // 1. Removing a feed watermark causes `evaluate_question` + view to withhold revenue.
    let saved_wm = sole.watermarks.remove("supply.pos_sales");
    let degraded = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert!(degraded.missing_feed_coverage.contains("supply.pos_sales"));
    assert_eq!(degraded.entity_net_revenue, None);
    assert_eq!(degraded.entity_pos_feed_revenue, None);
    if let Some(wm) = saved_wm {
        sole.watermarks.insert("supply.pos_sales".to_owned(), wm);
    }

    // 2. Ingesting a real quarantined POS row on Cell A withholds revenue until corrected!
    sole.ingest_feed(&FeedRow {
        feed_id: "supply.pos_sales", mapping: "supply.pos_sales.mapping", dataset: "pos_sale_line",
        source: "synthetic-pos", event: "ev-sole-pos-bad", record: "POS-SOLE-BAD",
        payload: r#"{"pos_line_id":"POS-SOLE-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":-4,"uom":"PCS","net_amount":800,"ccy":"EGP","sold_at":"2026-09-02T14:00:00Z"}"#,
    })?;
    let quarantined = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert_eq!(quarantined.open_feed_findings, 1);
    assert_eq!(quarantined.entity_net_revenue, None);
    assert_eq!(quarantined.entity_pos_feed_revenue, None);

    // Valid correction clears the finding and lifts POS feed revenue to 6800 EGP!
    sole.ingest_feed(&FeedRow {
        feed_id: "supply.pos_sales", mapping: "supply.pos_sales.mapping", dataset: "pos_sale_line",
        source: "synthetic-pos", event: "ev-sole-pos-fix", record: "POS-SOLE-BAD",
        payload: r#"{"pos_line_id":"POS-SOLE-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":4,"uom":"PCS","net_amount":800,"ccy":"EGP","sold_at":"2026-09-02T14:05:00Z"}"#,
    })?;
    let recovered = project_entity_frontier(sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert_eq!(recovered.open_feed_findings, 0);
    assert_eq!(recovered.entity_net_revenue.as_deref(), Some("10000"));
    assert_eq!(recovered.entity_pos_feed_revenue.as_deref(), Some("6800"));
    Ok(())
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
    assert_eq!(cell_view.view_kind, "combined_management_view");
    assert!(!cell_view.is_consolidated_financial_statement);
    assert!(cell_view.read_only);

    let sole_snap = cell_view.entities.first().ok_or("missing sole snap")?;
    assert_eq!(sole_snap.cell_id, SOLE_CELL_ID);
    assert_eq!(sole_snap.book_code, "BOOK-SOLE-EGP");
    assert_eq!(sole_snap.currency, "EGP");
    assert_eq!(sole_snap.definition_version, 1);
    assert_eq!(sole_snap.open_feed_findings, 0);
    assert!(!sole_snap.complete);
    assert_eq!(sole_snap.entity_net_revenue.as_deref(), Some("10000"));
    assert_eq!(sole_snap.entity_pos_feed_revenue.as_deref(), Some("6000"));
    assert_eq!(sole_snap.entity_gross_margin, None);
    assert_eq!(sole_snap.entity_gross_margin_rate, None);
    assert_eq!(sole_snap.entity_on_hand_verified, None);
    assert_eq!(sole_snap.entity_availability_verified, None);
    assert!(sole_snap.missing_cost_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.count_issue_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.count_issue_stores.contains("ST-SOLE-CFC-3"));
    assert!(sole_snap.count_issue_stores.contains("ST-SOLE-ZAM"));

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
        partner_snap.entity_gross_margin_rate.as_deref(),
        Some("0.375")
    );
    assert_eq!(
        partner_snap.entity_pos_feed_revenue.as_deref(),
        Some("3200")
    );
    assert_eq!(partner_snap.entity_on_hand_verified, Some(true));
    assert_eq!(partner_snap.entity_availability_verified, Some(true));

    #[cfg(all(feature = "duckdb", feature = "native"))]
    verify_duckdb_combined_view_parity(&sole, &partner, &cell_view)?;
    verify_degraded_and_quarantined_view(&mut sole)
}
