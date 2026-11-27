//! Resource Planning Lab (S1 — Retail Truth Slice, #1941):
//!
//! End-to-end validation of the two-entity, multi-store retail truth slice
//! layered onto `registry/packs/supply-lab/`:
//! 1. **Two-entity structural non-commingling (D02):** two independent
//!    file-backed `SqliteCell`s (sole proprietorship in EGP + partnership in USD
//!    operating stores in the same mall with shared physical custodian), proving
//!    custody != ownership, lot owner == store owner, unique store count
//!    position (`ID-UNIQUE-1`), and cross-scope refusal.
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
//!    cross-cell projection verifying every read matches the cell's pinned
//!    `read_envelope` frontier, explicitly labelled combined management view
//!    (never consolidated financial statements), never feeding a governed
//!    write, with exact `DuckDB` combined-projection parity when built with
//!    `--features duckdb,native`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_cell::compute::lineage::{ContributionSource, LineageDisclosure};
use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::compute::{QueryContext, QueryResult};
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

struct RetailPack {
    candidate: Vec<ArtifactEnvelope>,
    pool: Vec<ArtifactEnvelope>,
    definitions: AdmittedDefinitions,
}

fn retail_pack() -> Result<RetailPack, String> {
    let mut candidate = Vec::new();
    for dir in ["tracking", "ml", "supply-lab"] {
        candidate.extend(read_envelopes(
            &repo().join("registry/packs").join(dir).join("artifacts"),
        )?);
    }
    let mut pool = core_base()?;
    pool.extend(candidate.iter().cloned());
    let definitions =
        AdmittedDefinitions::from_envelopes(&pool).map_err(|e| format!("definitions: {e}"))?;
    Ok(RetailPack {
        candidate,
        pool,
        definitions,
    })
}

impl RetailPack {
    fn model(&self) -> &Model {
        self.definitions.model()
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
    pack: RetailPack,
    _dir: TempDir,
    scope: String,
    org_id: String,
    env_id: String,
    cell: RetailCell,
    tick: u64,
    calls: u64,
    source_cursors: BTreeMap<String, String>,
    watermarks: BTreeMap<String, u64>,
}

fn complete_feed_watermarks(now_ms: u64) -> (BTreeMap<String, String>, BTreeMap<String, u64>) {
    let mut cursors = BTreeMap::new();
    let mut watermarks = BTreeMap::new();
    for feed in REQUIRED_FEEDS {
        cursors.insert(feed.to_owned(), format!("cursor:{feed}:synced"));
        watermarks.insert(feed.to_owned(), now_ms);
    }
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
        let cell = RetailCell::new(store, scope.clone(), |c| c.payload.clone())
            .map_err(|e| format!("open cell: {e}"))?;
        let (source_cursors, watermarks) = complete_feed_watermarks(CLOCK_MS);
        Ok(Self {
            pack,
            _dir: dir,
            scope,
            org_id: org_id.to_owned(),
            env_id: env_id.to_owned(),
            cell,
            tick: 0,
            calls: 0,
            source_cursors,
            watermarks,
        })
    }

    fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(CLOCK_MS.saturating_add(self.tick))
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
        self.cell.advance_to(now);
        let world = self.cell.store().world();
        let admitted = self
            .pack
            .definitions
            .action(action)
            .ok_or_else(|| format!("no action {action}"))?;
        let input: JsonObject =
            serde_json::from_value(input).map_err(|e| format!("{action} input: {e}"))?;
        let catalog = self.pack.definitions.catalog();
        let call = ActionCall {
            action: &admitted.root,
            action_artifact_id: &admitted.artifact_id,
            action_version: &admitted.version,
            scope: BTreeMap::from([
                ("orgId".into(), self.org_id.clone()),
                ("environmentId".into(), self.env_id.clone()),
            ]),
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
        Ok(
            match commit_action(&mut self.cell, &catalog, &call, &Allow) {
                Ok(ActionOutcome::Committed { .. }) => Outcome::Committed,
                Ok(ActionOutcome::Replayed { .. }) => Outcome::Replayed,
                Err(refusal) => Outcome::Refused(refusal.code().to_owned(), refusal.to_string()),
            },
        )
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
        self.cell
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
        self.cell
            .store()
            .evaluate_transform(&plan, "main", &self.scope, access)
            .map_err(|e| format!("transform {artifact_id}: {e}"))
    }

    fn head(&self) -> Result<LedgerHead, String> {
        self.cell
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
    result.rows.iter().find_map(|(key, values)| {
        parts
            .iter()
            .all(|part| key.contains(part))
            .then(|| values.first().and_then(|v| *v))
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
    result.rows.iter().find_map(|(key, values)| {
        (key.contains("costed") && parts.iter().all(|part| key.contains(part)))
            .then(|| values.first().and_then(|v| *v))
            .flatten()
    })
}

/// Convert a `Decimal` (canonical text with at most 6 fractional digits) to
/// exact scale-6 integer units matching `norn_checker::metric::compute::arithmetic`.
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

fn check_feed_coverage(watermarks: &BTreeMap<String, u64>, min_fresh_ms: u64) -> BTreeSet<String> {
    REQUIRED_FEEDS
        .iter()
        .filter(|feed| watermarks.get(**feed).is_none_or(|wm| *wm < min_fresh_ms))
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
    for (sku, cat) in [("SKU-DRESS", "dry_goods"), ("SKU-RING-18K", "spares")] {
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
            "cost_id": "COST-DRESS-1", "sku_id": "SKU-DRESS", "owner_entity_id": "LE-SOLE",
            "supplier_id": "SUP-ATELIER", "unit_cost": "100", "currency": "EGP",
            "effective_from": "2026-07-01T00:00:00Z"
        }),
    )?;
    // Three stores in the shared mall (`MALL-CFC`, custodian `CUST-MALL-CFC-OPS` != owner `LE-SOLE`)
    // plus one standalone store (`ST-SOLE-ZAM`).
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
            "cost_id": "COST-RING-1", "sku_id": "SKU-RING-18K", "owner_entity_id": "LE-PARTNER",
            "supplier_id": "SUP-GOLD", "unit_cost": "250", "currency": "USD",
            "effective_from": "2026-07-01T00:00:00Z"
        }),
    )?;
    // Two stores in the SAME mall (`MALL-CFC`, same physical custodian `CUST-MALL-CFC-OPS`,
    // DIFFERENT owning legal entity `LE-PARTNER`, book, and USD currency!) plus one standalone.
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

fn seed_retail_operations(sole: &mut EntityBook, partner: &mut EntityBook) -> Result<(), String> {
    // --- Cell A (LE-SOLE, EGP) ---
    // ST-SOLE-CFC-1: fully costed, fresh physical count, 2 live holding lots across W1 & W5
    // plus 1 fully depleted lot on SKU-RING-18K (proving depleted lots drop from active aging).
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
    // Partial depletion preserves original `received_at` (2026-07-08) and derives stock_value = 2500.
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1",
            "remaining_qty": "25",
            "as_of": "2026-09-04T18:00:00Z",
            "unit_cost": "100",
            "stock_value": "0"
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
    // Fully depleted lot on SKU-RING-18K: must drop out of `StoreLotAging` (`remaining_qty > 0`).
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
            "lot_id": "LOT-S1-DEPLETED",
            "remaining_qty": "0",
            "as_of": "2026-08-20T09:00:00Z",
            "unit_cost": "100",
            "stock_value": "0"
        }),
    )?;
    // Fresh physical counts at ST-SOLE-CFC-1 (initial count recounted to fresh `2026-09-05`).
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/available", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "56", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-05T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-101"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "62", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-08-25T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-100"
        }),
    )?;
    sole.ok(
        "supply.store_count.recount",
        "clerk:hana",
        json!({
            "store_count_id": "ST-SOLE-CFC-1/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "60", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-09-05T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-102"
        }),
    )?;
    // Two costed POS sales linked to verified `COST-DRESS-1` (ratio-of-sums = 5000 / 9000 = 0.555556).
    sole.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S1-1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "30", "unit": "each", "net_revenue": "6000", "cogs_amount": "3000",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;
    sole.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S1-2", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "10", "unit": "each", "net_revenue": "3000", "cogs_amount": "1000",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-03T16:00:00Z"
        }),
    )?;

    // ST-SOLE-CFC-2: MIXED costed + uncosted bucket (BOTH costed & uncosted lots, BOTH costed &
    // uncosted sales in W9!) AND NO physical count (tests MissingCount fail-closed!).
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
            "cost_status": "missing_cost", "unit_cost": null, "stock_value": "0", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S2-1", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "sold_qty": "3", "unit": "each", "net_revenue": "600", "cogs_amount": "300",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-DRESS-1",
            "currency": "EGP", "sold_at": "2026-09-02T12:00:00Z"
        }),
    )?;
    sole.ok(
        "supply.pos_sale.record_uncosted",
        "clerk:hana",
        json!({
            "sale_id": "SALE-S2-2", "store_id": "ST-SOLE-CFC-2", "sku_id": "SKU-DRESS",
            "sold_qty": "2", "unit": "each", "net_revenue": "400", "cogs_amount": "0",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "missing_cost", "costed_ref": null,
            "currency": "EGP", "sold_at": "2026-09-03T12:00:00Z"
        }),
    )?;

    // ST-SOLE-CFC-3: STALE COUNT (>7 days old: 2026-08-10 vs CLOCK_MS 2026-09-07) + on-hand drift.
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
            "store_count_id": "ST-SOLE-CFC-3/SKU-DRESS/on_hand", "store_id": "ST-SOLE-CFC-3", "sku_id": "SKU-DRESS",
            "on_hand_qty": "28", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-08-10T09:00:00Z", "count_source": "handheld_scan", "count_ref": "CC-OLD"
        }),
    )?;

    // ST-SOLE-ZAM: FUTURE COUNT (`as_of: 2026-09-10 > CLOCK_MS 2026-09-07`) -> must fail closed!
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
            "store_count_id": "ST-SOLE-ZAM/SKU-DRESS/on_hand", "store_id": "ST-SOLE-ZAM", "sku_id": "SKU-DRESS",
            "on_hand_qty": "15", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-09-10T09:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-FUT"
        }),
    )?;

    // --- Cell B (LE-PARTNER, USD) ---
    // ST-PART-CFC-1: fully costed USD jewelry store in the same mall `MALL-CFC`, fresh counts.
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
            "store_count_id": "ST-PART-CFC-1/SKU-RING-18K/available", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "on_hand_qty": "12", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-06T10:00:00Z", "count_source": "store_audit", "count_ref": "AUD-P1"
        }),
    )?;
    partner.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "ST-PART-CFC-1/SKU-RING-18K/on_hand", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "on_hand_qty": "12", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-09-06T10:00:00Z", "count_source": "store_audit", "count_ref": "AUD-P2"
        }),
    )?;
    partner.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-P1-1", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "sold_qty": "8", "unit": "each", "net_revenue": "3200", "cogs_amount": "2000",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-RING-1",
            "currency": "USD", "sold_at": "2026-09-02T15:00:00Z"
        }),
    )?;
    Ok(())
}

// ------------------------------------------------------------------ Test 1

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

    // Both entities operate inside `MALL-CFC` under shared physical custodian `CUST-MALL-CFC-OPS`,
    // while legal ownership stays strictly bound to each cell's own LegalEntity.
    let sole_store = sole
        .cell
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
        .cell
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

    // Cross-entity write refuses: Cell A (`LE-SOLE`) cannot register a store owned by `LE-PARTNER`.
    sole.refused(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-ROGUE-PARTNER", "name": "Cross Entity", "entity_id": "LE-PARTNER",
            "custodian_id": "CUST-MALL-CFC-OPS", "location_kind": "mall_store",
            "mall_code": "MALL-CFC", "currency": "USD"
        }),
    )?;

    // Cross-currency store under `LE-SOLE` (EGP) refuses.
    sole.refused(
        "supply.store.register",
        "eng:dina",
        json!({
            "store_id": "ST-SOLE-USD", "name": "Wrong Currency", "entity_id": "LE-SOLE",
            "custodian_id": "CUST-MALL-CFC-OPS", "location_kind": "mall_store",
            "mall_code": "MALL-CFC", "currency": "USD"
        }),
    )?;

    // Same-currency cross-owner StockLot refuses (`owner_entity_id != store.entity_id`).
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

    // Costed POS sale without a real registered `UnitCost` refuses (no fabricated `costed_ref`).
    sole.refused(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-FAKE-COST", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "sold_qty": "2", "unit": "each", "net_revenue": "400", "cogs_amount": "200",
            "gross_margin": "0", "gross_margin_rate": "0",
            "cost_status": "costed", "costed_ref": "COST-NONEXISTENT",
            "currency": "EGP", "sold_at": "2026-09-01T14:00:00Z"
        }),
    )?;

    // ID-UNIQUE-1: two StoreCount rows for the same `(store_id, sku_id, quantity_basis)`
    // under different `store_count_id`s refuse so snapshot sums cannot double-count a position.
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
        .cell
        .store()
        .evaluate_question(&plan, foreign_ctx, |_| Ok(true));
    assert!(
        matches!(
            cross_scope,
            Err(CellError::ComputeRefused("core.compute.stale_input"))
        ),
        "foreign scope query must refuse with core.compute.stale_input: {cross_scope:?}"
    );

    sole.head()?;
    partner.head()?;
    Ok(())
}

// ------------------------------------------------------------------ Test 2

fn runtime_envelopes(artifacts: &[ArtifactEnvelope]) -> Result<Vec<RuntimeValue>, String> {
    artifacts
        .iter()
        .map(|a| {
            let wire = serde_json::to_value(a).map_err(|e| format!("{e}"))?;
            RuntimeValue::from_json(wire).map_err(|e| format!("{e:?}"))
        })
        .collect()
}

fn open_retail_mapping_cell(pack: &RetailPack, dir: &Path) -> Result<MapCell, String> {
    let base = core_base()?;
    let (sources, base_sources) = (
        runtime_envelopes(&pack.candidate)?,
        runtime_envelopes(&base)?,
    );
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
        return Err(format!("not admitted: {:?}", checked.diagnostics));
    }
    let wire = checked
        .receipt
        .to_canonical_value()
        .map_err(|e| format!("{e:?}"))?;
    let receipt =
        String::from_utf8(norn_ir::canonical::serialize(&wire).map_err(|e| format!("{e:?}"))?)
            .map_err(|e| format!("{e}"))?;
    let model = pack.model().clone();
    let mut cell = MapCell::open(
        &dir.join("retail-map.sqlite"),
        model.clone(),
        StoreOptions::default(),
        SOLE_SCOPE,
        |_| BTreeMap::new(),
    )
    .map_err(|e| format!("open map cell: {e}"))?;
    let set: Vec<&ArtifactEnvelope> = pack.candidate.iter().chain(&base).collect();
    cell.install_definitions(
        model,
        norn_cell::InstalledDefinitionInput {
            canonical_set: serde_json::to_string(&set).map_err(|e| format!("{e}"))?,
            registry: Vec::new(),
            receipt_sha256: norn_checker::admission_receipt_sha256(&receipt)
                .map_err(|e| format!("{e:?}"))?,
            admission_receipt: Some(receipt),
            readmission: None,
            diff: json!({"suite": "retail-truth-slice"}),
            installed_at: 1,
            layout: None,
        },
        Some(0),
    )
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
        partition: "MALL-CFC".to_owned(),
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

#[test]
fn s1_messy_retail_feeds_quarantine_idempotency_and_recovery() -> Result<(), String> {
    let pack = retail_pack()?;
    let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
    let mut cell = open_retail_mapping_cell(&pack, dir.path())?;
    let (source_cursors, watermarks) = complete_feed_watermarks(CLOCK_MS);
    let feed_ctx = QueryContext {
        world: "main".to_owned(),
        scope: SOLE_SCOPE.to_owned(),
        population_pin: "all".to_owned(),
        authorization_epoch: 1,
        mapping_pin: "supply.pos_sales.mapping/0.1.0".to_owned(),
        now_ms: CLOCK_MS,
        source_cursors,
        watermarks: watermarks.clone(),
    };
    let pos_q = pack.retail_question("supply.retail.pos_feed_revenue_weekly")?;

    // 1. POS sales feed: clean rows land, bad row quarantines, KPI reflects quarantine & recovery,
    //    and LINEAGE-SUM-1 resolves producing POS source records!
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

    // Bad POS row (negative quantity, 600 EGP) -> quarantined via SalePositive law.
    let bad_pos = r#"{"pos_line_id":"POS-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":-3,"uom":"PCS","net_amount":600,"ccy":"EGP","sold_at":"2026-09-01T14:00:00Z"}"#;
    let (b_bad, fresh_bad) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-bad", "POS-BAD", bad_pos)?,
            &pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    assert!(fresh_bad);
    cell.process_mapping_bundle(&b_bad)
        .map_err(|e| format!("{e}"))?;
    let f = open_findings(&cell, "supply.PosSaleEvidence", "POS-BAD")?;
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f.iter().any(|r| r.contains("sale_positive")), "{f:?}");

    // End-to-end KPI check BEFORE correction: quarantined `POS-BAD` is excluded from the bucket
    // (600 + 400 = 1000, never 1600) AND marks the result incomplete (`complete == false`)!
    let before_fix = cell
        .store()
        .evaluate_question(&pos_q, feed_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("pos feed query before fix: {e}"))?;
    assert_eq!(
        bucket_total(&before_fix, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
        Some("1000"),
        "quarantined POS-BAD is excluded from revenue bucket"
    );
    assert!(
        !before_fix.complete,
        "quarantined row must mark QueryResult incomplete"
    );

    // Exact redelivery + re-emitted duplicate event id: zero duplicate findings, unchanged KPI!
    let (b_dup, fresh_dup) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-bad", "POS-BAD", bad_pos)?,
            &pos_in,
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
            &pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_reemit)
        .map_err(|e| format!("{e}"))?;
    assert_eq!(
        open_findings(&cell, "supply.PosSaleEvidence", "POS-BAD")?.len(),
        1,
        "redelivery never duplicates active findings"
    );
    let after_redelivery = cell
        .store()
        .evaluate_question(&pos_q, feed_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("pos feed query after redelivery: {e}"))?;
    assert_eq!(
        bucket_total(&after_redelivery, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
        Some("1000"),
        "redelivery never double-counts revenue"
    );

    // Valid correction clears the finding AND lifts the complete KPI bucket to 1600 EGP!
    let fixed_pos = r#"{"pos_line_id":"POS-BAD","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":3,"uom":"PCS","net_amount":600,"ccy":"EGP","sold_at":"2026-09-01T14:05:00Z"}"#;
    let (b_fix, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-fix", "POS-BAD", fixed_pos)?,
            &pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_fix)
        .map_err(|e| format!("{e}"))?;
    assert!(open_findings(&cell, "supply.PosSaleEvidence", "POS-BAD")?.is_empty());

    let after_fix = cell
        .store()
        .evaluate_question(&pos_q, feed_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("pos feed query after fix: {e}"))?;
    assert!(
        after_fix.complete,
        "cleared finding restores complete answer"
    );
    assert_eq!(
        bucket_total(&after_fix, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
        Some("1600"),
        "recovered POS-BAD joins bucket: 600 + 400 + 600 = 1600"
    );

    // LINEAGE-SUM-1: scoped number-to-source drill-through resolves the actual producing POS
    // source records (`ContributionSource::Resolved`) for an authorized reader, and redacts them
    // (`ContributionSource::Restricted`) for a restricted reader!
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
            &pos_q,
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
                Some(provenance.source_record_ref.as_str().to_owned())
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
            &pos_q,
            feed_ctx,
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

    // Missing feed watermark fails closed on feed coverage check.
    let mut partial_wm = watermarks;
    partial_wm.remove("supply.store_counts");
    let missing_feeds = check_feed_coverage(&partial_wm, CLOCK_MS.saturating_sub(MAX_COUNT_AGE_MS));
    assert!(missing_feeds.contains("supply.store_counts"));

    // Unknown UOM ("BOX") lands quarantined at mapping time.
    let bad_uom = r#"{"pos_line_id":"POS-UOM","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":2,"uom":"BOX","net_amount":200,"ccy":"EGP","sold_at":"2026-09-01T14:00:00Z"}"#;
    let (b_uom, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-uom", "POS-UOM", bad_uom)?,
            &pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    let outcome = cell
        .process_mapping_outcome(&b_uom)
        .map_err(|e| format!("{e}"))?;
    assert!(
        matches!(outcome, MappingOutcome::Quarantined(_)),
        "{outcome:?}"
    );

    // 2. Store physical count feed: inherits `on_hand_non_negative`, quarantines unknown basis.
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
    let cnt_f = open_findings(&cell, "supply.StoreCountEvidence", "SC-BAD")?;
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

    // 3. Retail supplier receipt feed: vendor normalization, missing cost allowed, negative cost & over-damage quarantined.
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
    assert!(open_findings(&cell, "supply.RetailReceiptEvidence", "RR-NOCOST")?.is_empty());

    let bad_rcpt = r#"{"receipt_line_id":"RR-BAD","store_code":"ST-SOLE-CFC-2","sku_code":"SKU-DRESS","vendor_code":"ATELIER-01","received_qty":10,"damaged_qty":12,"uom":"PCS","unit_cost":-5,"ccy":"EGP","received_at":"2026-08-20T09:00:00Z"}"#;
    let (b_r2, _) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-retail-receipt", "ev-rr-2", "RR-BAD", bad_rcpt)?,
            &rcpt_in,
        )
        .map_err(|e| format!("{e}"))?;
    cell.process_mapping_bundle(&b_r2)
        .map_err(|e| format!("{e}"))?;
    let r_f = open_findings(&cell, "supply.RetailReceiptEvidence", "RR-BAD")?;
    assert!(
        r_f.iter().any(|r| r.contains("damaged_within_received")),
        "{r_f:?}"
    );
    assert!(
        r_f.iter().any(|r| r.contains("retail_receipt_cost_valid")),
        "{r_f:?}"
    );

    // 4. Unit cost feed: zero unit cost quarantined, positive correction recovers.
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
    let c_f = open_findings(&cell, "supply.UnitCostEvidence", "COST-BAD")?;
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
    assert!(open_findings(&cell, "supply.UnitCostEvidence", "COST-BAD")?.is_empty());

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
        count_as_of: String,
    },
    StaleCount {
        last_count_as_of: String,
        age_ms: u64,
    },
    OnHandDrift {
        counted_qty: String,
        derived_lot_qty: String,
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

/// Fail-closed physical count coverage & staleness check across the union of all
/// live holdings (`remaining_qty > 0`) and all `on_hand` `StoreCount`s.
/// Selects the latest eligible (`as_of <= now_ms`) count per `(store_id, sku_id)`
/// and emits structured findings for missing, future, stale, or drifted counts.
/// (Wires into `#1905` `RequiredSources` freshness once B6 lands.)
fn evaluate_count_coverage(
    book: &EntityBook,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<Vec<CountCoverageFinding>, String> {
    let reader = book.cell.store().read();
    let lots = list_objects(&reader, "supply.StockLot")?;
    let mut derived_on_hand: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    for lot in &lots {
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
    let mut latest_eligible: BTreeMap<(String, String), (u64, String, Decimal)> = BTreeMap::new();
    let mut findings = Vec::new();
    let mut keys: BTreeSet<(String, String)> = derived_on_hand
        .iter()
        .filter(|(_, qty)| **qty > Decimal::from(0_i64))
        .map(|(k, _)| k.clone())
        .collect();

    for count in &counts {
        if count.values.get("quantity_basis").map(scalar).as_deref() != Some("on_hand") {
            continue;
        }
        let store_id = count.values.get("store_id").map(scalar).unwrap_or_default();
        let sku_id = count.values.get("sku_id").map(scalar).unwrap_or_default();
        let as_of = count.values.get("as_of").map(scalar).unwrap_or_default();
        let as_of_ms = parse_rfc3339_ms(&as_of)?;
        let key = (store_id.clone(), sku_id.clone());
        keys.insert(key.clone());

        if as_of_ms > now_ms {
            findings.push(CountCoverageFinding {
                store_id,
                sku_id,
                kind: CountCoverageKind::FutureCount { count_as_of: as_of },
            });
            continue;
        }
        let counted = match count.values.get("on_hand_qty") {
            Some(RuntimeValue::Number(d)) => *d,
            _ => Decimal::from(0_i64),
        };
        let replace = latest_eligible
            .get(&key)
            .is_none_or(|(prev_ms, _, _)| as_of_ms >= *prev_ms);
        if replace {
            latest_eligible.insert(key, (as_of_ms, as_of, counted));
        }
    }

    for (store_id, sku_id) in keys {
        let derived = derived_on_hand
            .get(&(store_id.clone(), sku_id.clone()))
            .copied()
            .unwrap_or_else(|| Decimal::from(0_i64));
        let has_future = findings.iter().any(|f| {
            f.store_id == store_id
                && f.sku_id == sku_id
                && matches!(f.kind, CountCoverageKind::FutureCount { .. })
        });
        match latest_eligible.get(&(store_id.clone(), sku_id.clone())) {
            None => {
                if !has_future && derived > Decimal::from(0_i64) {
                    findings.push(CountCoverageFinding {
                        store_id,
                        sku_id,
                        kind: CountCoverageKind::MissingCount,
                    });
                }
            }
            Some((as_of_ms, as_of, counted)) => {
                let age_ms = now_ms.saturating_sub(*as_of_ms);
                if age_ms > max_age_ms {
                    findings.push(CountCoverageFinding {
                        store_id: store_id.clone(),
                        sku_id: sku_id.clone(),
                        kind: CountCoverageKind::StaleCount {
                            last_count_as_of: as_of.clone(),
                            age_ms,
                        },
                    });
                }
                if *counted != derived {
                    findings.push(CountCoverageFinding {
                        store_id,
                        sku_id,
                        kind: CountCoverageKind::OnHandDrift {
                            counted_qty: counted.canonical(),
                            derived_lot_qty: derived.canonical(),
                        },
                    });
                }
            }
        }
    }
    Ok(findings)
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

    // Attempting to rewrite a lot's `received_at` during a balance update refuses at input validation
    // (`additionalProperties: false` structurally excludes `received_at` from the update schema).
    sole.refused(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1",
            "remaining_qty": "20",
            "as_of": "2026-09-06T09:00:00Z",
            "unit_cost": "100",
            "stock_value": "0",
            "received_at": "2026-09-01T09:00:00Z"
        }),
    )?;

    // Admit and evaluate all 7 operational retail KPI questions on Cell A.
    let availability = sole.answer("supply.retail.store_availability_weekly")?;
    assert_eq!(
        bucket_total(&availability, &["ST-SOLE-CFC-1", "each", "2026-08-31"]).as_deref(),
        Some("56"),
        "store availability as-of W9"
    );
    let on_hand = sole.answer("supply.retail.store_on_hand_weekly")?;
    let s1_on_hand = bucket_decimal(&on_hand, &["ST-SOLE-CFC-1", "each", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 on_hand")?;
    assert_eq!(
        s1_on_hand.canonical(),
        "60",
        "derived lot on-hand = 25 + 35"
    );

    // Cost-status-partitioned stock value: pure-costed ST-SOLE-CFC-1 publishes 6000 EGP;
    // MIXED costed + uncosted ST-SOLE-CFC-2 FAILS CLOSED (`None` — no partial total!).
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
        Some("60"),
        "W1 lot receipt cohort preserved after balance depletion"
    );
    assert_eq!(
        bucket_total(&aging_q, &["ST-SOLE-CFC-1", "each", "2026-08-03"]).as_deref(),
        Some("40"),
        "W5 lot receipt cohort"
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

    // Gross margin rate: pure-costed ST-SOLE-CFC-1 = 5000 / 9000 = 0.555556 (rounded midpoint away
    // from zero, ratio of sums != 0.583333 average of line rates); mixed ST-SOLE-CFC-2 WITHHOLDS!
    let gm_rate = sole.answer("supply.retail.store_gross_margin_rate_weekly")?;
    let s1_rate = resolve_costed_bucket(&gm_rate, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 gross margin rate")?;
    assert_eq!(
        s1_rate.canonical(),
        "0.555556",
        "exact ratio-of-sums 5000/9000 rounded to scale 6"
    );
    assert_eq!(
        resolve_costed_bucket(&gm_rate, &["ST-SOLE-CFC-2", "EGP", "2026-08-31"]),
        None,
        "mixed costed + uncosted gross margin bucket must withhold"
    );

    // Chasm-trap-safe retail transforms: StoreSalesPerformance & StoreLotAging.
    let sales_tf = sole.evaluate_transform("supply.retail.store_sales_performance", |_| {
        Ok(RowAccess::Read)
    })?;
    assert!(sales_tf.complete);
    let s1_sales_row = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1"))
        .ok_or("missing ST-SOLE-CFC-1 sales row")?;
    assert_eq!(
        s1_sales_row.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(true))
    );
    let s1_margin = match s1_sales_row.values.get("effective_gross_margin") {
        Some(RuntimeValue::Number(d)) => *d,
        other => return Err(format!("expected numeric margin, got {other:?}")),
    };
    assert_eq!(s1_margin.canonical(), "5000");

    // Active lot aging transform (`remaining_qty > 0` filter): depleted LOT-S1-DEPLETED (SKU-RING-18K)
    // drops out completely, while live W1 & W5 SKU-DRESS lots sum to 100 received!
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

    // Compute sell-through, WOS (4-week window), and GMROI directly from REAL cell outputs!
    let sell_through = ratio(
        decimal_to_scaled_i128(s1_sold)?,
        decimal_to_scaled_i128(s1_live_received)?,
    );
    assert_eq!(
        sell_through,
        Some(400_000),
        "cell-derived sell-through = 0.400000"
    );
    let weekly_sales = ratio(
        decimal_to_scaled_i128(s1_sold)?,
        4_i128.saturating_mul(FACTOR),
    )
    .ok_or("weekly sales")?;
    let wos = ratio(decimal_to_scaled_i128(s1_on_hand)?, weekly_sales);
    assert_eq!(wos, Some(6_000_000), "cell-derived WOS = 6.000000 weeks");
    let gmroi = ratio(
        decimal_to_scaled_i128(s1_margin)?,
        decimal_to_scaled_i128(s1_stock_val)?,
    );
    assert_eq!(
        gmroi,
        Some(833_333),
        "cell-derived GMROI = 5000/6000 = 0.833333"
    );

    // Missing-cost withholding at ST-SOLE-CFC-2 in both transforms.
    let s2_sales_row = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-2"))
        .ok_or("missing ST-SOLE-CFC-2 sales row")?;
    assert_eq!(
        s2_sales_row.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        s2_sales_row.values.get("effective_cogs"),
        Some(&RuntimeValue::Null)
    );
    assert_eq!(
        s2_sales_row.values.get("effective_gross_margin"),
        Some(&RuntimeValue::Null)
    );
    let s2_lot_row = aging_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-2"))
        .ok_or("missing ST-SOLE-CFC-2 lot row")?;
    assert_eq!(
        s2_lot_row.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(false))
    );

    // Row-level authorization withholding (`RowAccess::Count`) marks transform incomplete!
    let withheld_tf = sole.evaluate_transform("supply.retail.store_sales_performance", |obj| {
        if obj.object_type == "supply.PosSale" && obj.id == "SALE-S1-2" {
            Ok(RowAccess::Count)
        } else {
            Ok(RowAccess::Read)
        }
    })?;
    assert!(
        !withheld_tf.complete,
        "withheld sale row marks transform incomplete"
    );
    assert_eq!(withheld_tf.withheld.get("sale").copied(), Some(1));

    // Count coverage fails closed on ALL 4 cases:
    // - ST-SOLE-CFC-1: fresh + matched -> zero findings
    // - ST-SOLE-CFC-2: positive holding with zero counts -> MissingCount
    // - ST-SOLE-CFC-3: >7d old + 28 != 20 -> StaleCount + OnHandDrift
    // - ST-SOLE-ZAM: future count (2026-09-10 > CLOCK_MS) -> FutureCount
    let coverage_findings = evaluate_count_coverage(&sole, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    assert!(
        !coverage_findings
            .iter()
            .any(|f| f.store_id == "ST-SOLE-CFC-1"),
        "fresh matched store has zero findings: {coverage_findings:?}"
    );
    assert!(
        coverage_findings.iter().any(|f| {
            f.store_id == "ST-SOLE-CFC-2" && matches!(f.kind, CountCoverageKind::MissingCount)
        }),
        "missing count on positive holding must fail closed: {coverage_findings:?}"
    );
    assert!(
        coverage_findings.iter().any(|f| {
            f.store_id == "ST-SOLE-CFC-3" && matches!(f.kind, CountCoverageKind::StaleCount { .. })
        }),
        "stale count must fail closed: {coverage_findings:?}"
    );
    assert!(
        coverage_findings.iter().any(|f| {
            f.store_id == "ST-SOLE-CFC-3" && matches!(f.kind, CountCoverageKind::OnHandDrift { .. })
        }),
        "drifted count must fail closed: {coverage_findings:?}"
    );
    assert!(
        coverage_findings.iter().any(|f| {
            f.store_id == "ST-SOLE-ZAM" && matches!(f.kind, CountCoverageKind::FutureCount { .. })
        }),
        "future count must fail closed: {coverage_findings:?}"
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
    missing_feed_coverage: BTreeSet<String>,
    missing_cost_stores: BTreeSet<String>,
    count_issue_stores: BTreeSet<String>,
    /// Withheld (`None`) whenever row/feed coverage is incomplete; never cross-currency summed.
    entity_net_revenue: Option<String>,
    /// Withheld (`None`) whenever any store has missing cost or incomplete feed/row coverage.
    entity_gross_margin: Option<String>,
    /// Withheld (`None`) whenever any store has missing, stale, future, or drifted counts.
    entity_on_hand_verified: Option<bool>,
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

fn project_entity_frontier(
    book: &EntityBook,
    legal_entity_id: &str,
    eval_question: &mut dyn FnMut(&str) -> Result<QueryResult, String>,
) -> Result<EntityFrontierSnapshot, String> {
    // READ-LOCAL-1: capture authenticated local point read + frontier on the legal entity root.
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
        .cell
        .read_envelope(&key, &authority)
        .map_err(|e| format!("read_envelope {legal_entity_id}: {e}"))?;
    assert_eq!(envelope.token.completeness, Completeness::Complete);
    assert_eq!(envelope.token.semantics, ReadSemantics::Exact);
    envelope
        .token
        .validate(&envelope.token.context, ReadRequirement::Exact)
        .map_err(|e| format!("validate token {legal_entity_id}: {e}"))?;
    let pinned_rev = envelope.token.context.frontier.revision;

    let entity_obj = book
        .cell
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

    let sales = book.evaluate_transform("supply.retail.store_sales_performance", |_| {
        Ok(RowAccess::Read)
    })?;
    let lots = book.evaluate_transform("supply.retail.store_lot_aging", |_| Ok(RowAccess::Read))?;
    let rev_q = eval_question("supply.retail.store_revenue_weekly")?;
    let gm_q = eval_question("supply.retail.store_gross_margin_rate_weekly")?;
    let count_issues = evaluate_count_coverage(book, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    let head_after = book.head()?;

    // Single-cut verification: refuse if the cell advanced between `read_envelope` and any read.
    if sales.revision.commits != pinned_rev
        || lots.revision.commits != pinned_rev
        || rev_q.known_as_of != pinned_rev
        || gm_q.known_as_of != pinned_rev
        || head_after.commits != pinned_rev
    {
        return Err(format!(
            "frontier moved during projection of {legal_entity_id}: envelope={pinned_rev}, sales={}, lots={}, rev_q={}, gm_q={}, after={}",
            sales.revision.commits,
            lots.revision.commits,
            rev_q.known_as_of,
            gm_q.known_as_of,
            head_after.commits,
        ));
    }

    let missing_feed_coverage =
        check_feed_coverage(&book.watermarks, CLOCK_MS.saturating_sub(MAX_COUNT_AGE_MS));
    let mut missing_cost_stores = BTreeSet::new();
    let mut rev_sum = Decimal::from(0_i64);
    for (bucket, vals) in &rev_q.rows {
        if bucket.contains(&currency)
            && let Some(Some(amount)) = vals.first()
        {
            rev_sum = rev_sum.checked_add(*amount).ok_or("rev overflow")?;
        }
    }
    let mut margin_sum = Decimal::from(0_i64);
    for row in &sales.rows {
        let store_id = row.values.get("store_id").map(scalar).unwrap_or_default();
        if row.values.get("cost_complete") == Some(&RuntimeValue::Bool(true)) {
            if let Some(RuntimeValue::Number(gm)) = row.values.get("effective_gross_margin") {
                margin_sum = margin_sum.checked_add(*gm).ok_or("margin overflow")?;
            }
        } else {
            missing_cost_stores.insert(store_id);
        }
    }
    for row in &lots.rows {
        if row.values.get("cost_complete") != Some(&RuntimeValue::Bool(true)) {
            missing_cost_stores.insert(row.values.get("store_id").map(scalar).unwrap_or_default());
        }
    }
    // Also detect any `missing_cost` slice surfaced directly in `store_gross_margin_rate_weekly`.
    let has_missing_cost_slice = gm_q.rows.keys().any(|k| k.contains("missing_cost"));
    let count_issue_stores: BTreeSet<String> =
        count_issues.into_iter().map(|f| f.store_id).collect();
    let feeds_and_rows_complete = sales.complete
        && lots.complete
        && rev_q.complete
        && gm_q.complete
        && missing_feed_coverage.is_empty();
    let cost_complete = missing_cost_stores.is_empty() && !has_missing_cost_slice;
    let complete = feeds_and_rows_complete && cost_complete && count_issue_stores.is_empty();

    Ok(EntityFrontierSnapshot {
        cell_id: envelope.token.context.frontier.cell.as_str().to_owned(),
        legal_entity_id: legal_entity_id.to_owned(),
        book_code,
        currency,
        scope: book.scope.clone(),
        world: envelope.token.context.frontier.world,
        revision: pinned_rev,
        definition_version: envelope.token.context.definition_pin.version,
        complete,
        entity_net_revenue: feeds_and_rows_complete.then(|| rev_sum.canonical()),
        entity_gross_margin: (feeds_and_rows_complete && cost_complete)
            .then(|| margin_sum.canonical()),
        entity_on_hand_verified: (feeds_and_rows_complete && count_issue_stores.is_empty())
            .then_some(true),
        missing_feed_coverage,
        missing_cost_stores,
        count_issue_stores,
    })
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
    assert_eq!(cell_view.entities.len(), 2);

    // Cell A (`LE-SOLE`, EGP): feeds complete so revenue (10000 EGP) is published, while
    // missing cost (ST-SOLE-CFC-2) withholds gross margin and count issues (ST-SOLE-CFC-2 missing,
    // ST-SOLE-CFC-3 stale+drifted, ST-SOLE-ZAM future) withhold verified on-hand.
    let sole_snap = cell_view.entities.first().ok_or("missing sole snap")?;
    assert_eq!(sole_snap.cell_id, SOLE_CELL_ID);
    assert_eq!(sole_snap.book_code, "BOOK-SOLE-EGP");
    assert_eq!(sole_snap.currency, "EGP");
    assert!(sole_snap.revision > 0);
    assert!(!sole_snap.complete);
    assert_eq!(sole_snap.entity_net_revenue.as_deref(), Some("10000"));
    assert_eq!(
        sole_snap.entity_gross_margin, None,
        "withheld on missing cost"
    );
    assert_eq!(
        sole_snap.entity_on_hand_verified, None,
        "withheld on count issues"
    );
    assert!(sole_snap.missing_cost_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.count_issue_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.count_issue_stores.contains("ST-SOLE-CFC-3"));
    assert!(sole_snap.count_issue_stores.contains("ST-SOLE-ZAM"));

    // Cell B (`LE-PARTNER`, USD): completely independent frontier and currency, complete = true.
    let partner_snap = cell_view.entities.get(1).ok_or("missing partner snap")?;
    assert_eq!(partner_snap.cell_id, PARTNER_CELL_ID);
    assert_ne!(partner_snap.cell_id, sole_snap.cell_id);
    assert_eq!(partner_snap.book_code, "BOOK-PARTNER-USD");
    assert_eq!(partner_snap.currency, "USD");
    assert!(partner_snap.revision > 0);
    assert!(partner_snap.complete);
    assert_eq!(partner_snap.entity_net_revenue.as_deref(), Some("3200"));
    assert_eq!(partner_snap.entity_gross_margin.as_deref(), Some("1200"));
    assert_eq!(partner_snap.entity_on_hand_verified, Some(true));

    // When a required feed watermark is missing on Cell A, revenue is ALSO withheld!
    sole.watermarks.remove("supply.pos_sales");
    let degraded_sole = project_entity_frontier(&sole, "LE-SOLE", &mut |q| sole.answer(q))?;
    assert!(!degraded_sole.complete);
    assert!(
        degraded_sole
            .missing_feed_coverage
            .contains("supply.pos_sales")
    );
    assert_eq!(
        degraded_sole.entity_net_revenue, None,
        "missing POS feed watermark must withhold revenue"
    );
    // Restore watermark for DuckDB combined-view parity check below.
    sole.watermarks
        .insert("supply.pos_sales".to_owned(), CLOCK_MS);

    // DuckDB combined management view projection: mirror BOTH isolated tenant cells into DuckDB,
    // construct `CombinedManagementView` directly from DuckDB answers, and assert exact parity
    // with the Cell-built `CombinedManagementView`!
    #[cfg(all(feature = "duckdb", feature = "native"))]
    {
        use norn_cell::analytics::TenantId;
        let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
        let duck =
            norn_cell::analytics::engine::Engine::new(norn_cell::analytics::engine::Settings {
                root: dir.path().join("duckdb"),
                memory_limit: "256MB".into(),
                threads: 1,
            })
            .map_err(|e| format!("duckdb: {e}"))?;

        let sync_tenant = |book: &EntityBook| -> Result<TenantId, String> {
            let store = book.cell.store();
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

        let sole_tenant = sync_tenant(&sole)?;
        let partner_tenant = sync_tenant(&partner)?;
        let duck_eval =
            |book: &EntityBook, tenant: &TenantId, q_name: &str| -> Result<QueryResult, String> {
                let plan = book.pack.retail_question(q_name)?;
                let prepared = book
                    .cell
                    .store()
                    .analytics_prepare(&plan, book.query_context(), tenant)
                    .map_err(|e| format!("prepare {q_name}: {e}"))?;
                let response = duck
                    .evaluate(prepared.request())
                    .map_err(|e| format!("duckdb {q_name}: {} {}", e.code, e.detail))?;
                book.cell
                    .store()
                    .analytics_answer(prepared, response)
                    .map_err(|e| format!("answer {q_name}: {e}"))
            };

        let duck_view = CombinedManagementView {
            view_kind: "combined_management_view",
            is_consolidated_financial_statement: false,
            read_only: true,
            entities: vec![
                project_entity_frontier(&sole, "LE-SOLE", &mut |q| {
                    duck_eval(&sole, &sole_tenant, q)
                })?,
                project_entity_frontier(&partner, "LE-PARTNER", &mut |q| {
                    duck_eval(&partner, &partner_tenant, q)
                })?,
            ],
        };
        assert_eq!(
            duck_view, cell_view,
            "DuckDB combined management view must match Cell combined management view exactly"
        );
    }

    Ok(())
}
