//! Resource Planning Lab (S1 — Retail Truth Slice, #1941):
//!
//! End-to-end validation of the two-entity, multi-store retail truth slice
//! layered onto `registry/packs/supply-lab/`:
//! 1. **Two-entity structural non-commingling (D02):** two independent
//!    file-backed `SqliteCell`s (sole proprietorship in EGP + partnership in USD
//!    operating stores in the same mall with shared physical custodian), proving
//!    custody != ownership and neither cell can mutate or net the other entity's
//!    books or currencies.
//! 2. **Messy retail feeds with quarantine & idempotency:** POS sales, store
//!    physical counts, retail supplier receipts, and supplier unit costs
//!    ingested through real admitted mapping bundles (`inject_mapping_bundle` /
//!    `process_mapping_outcome`) with bad-row quarantine, exact redelivery
//!    idempotency, and recovery on correction.
//! 3. **Holding lot aging, count-staleness drift, commercial KPIs, withheld
//!    totals, and `LINEAGE-SUM-1`:** exact answers over all 7 declared retail
//!    questions and both chasm-trap-safe retail transforms; missing cost and
//!    stale counts fail closed (withheld totals); scoped number-to-source
//!    lineage drill-through.
//! 4. **Read-only combined management view (`READ-LOCAL-1`):** cross-cell
//!    projection carrying per-cell `read_envelope` frontiers and coverage
//!    status, explicitly labelled combined management view (never consolidated
//!    financial statements), never feeding a governed write, plus optional
//!    `DuckDB` mirror parity when built with `--features duckdb`.

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
        Ok(Self {
            pack,
            _dir: dir,
            scope,
            org_id: org_id.to_owned(),
            env_id: env_id.to_owned(),
            cell,
            tick: 0,
            calls: 0,
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
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
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

fn bucket_total(result: &QueryResult, parts: &[&str]) -> Option<String> {
    result.rows.iter().find_map(|(key, values)| {
        parts
            .iter()
            .all(|part| key.contains(part))
            .then(|| {
                values
                    .first()
                    .and_then(|v| v.as_ref().map(Decimal::canonical))
            })
            .flatten()
    })
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
    // Three stores in the shared mall (`MALL-CFC`, custodian `CUST-MALL-CFC-OPS` != owner `LE-SOLE`)
    // plus one standalone store.
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
    // ST-SOLE-CFC-1: fully costed, fresh physical count, 2 holding lots across different receipt weeks (aging!).
    // Lot 1 received W1 (2026-07-08): received 60, remaining 25 @ 100 EGP = 2500 EGP.
    // Lot 2 received W5 (2026-08-05): received 40, remaining 35 @ 100 EGP = 3500 EGP.
    // Total received = 100, remaining on_hand = 60, stock_value = 6000 EGP.
    sole.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "60", "remaining_qty": "60", "unit": "each",
            "received_at": "2026-07-08T09:00:00Z", "as_of": "2026-07-08T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "6000", "currency": "EGP"
        }),
    )?;
    // Partial depletion of Lot 1 preserves original `received_at` (2026-07-08) for aging!
    sole.ok(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "60", "remaining_qty": "25", "unit": "each",
            "received_at": "2026-07-08T09:00:00Z", "as_of": "2026-09-04T18:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "2500", "currency": "EGP"
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
            "cost_status": "costed", "unit_cost": "100", "stock_value": "3500", "currency": "EGP"
        }),
    )?;
    // Fresh physical counts at ST-SOLE-CFC-1 (within 7 days of CLOCK_MS 2026-09-07T12:00:00Z).
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-S1-AVAIL", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "56", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-05T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-101"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-S1-OH", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "on_hand_qty": "60", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-09-05T10:00:00Z", "count_source": "pos_cycle_count", "count_ref": "CC-102"
        }),
    )?;
    // Two POS sales in W9 (2026-09-01 and 2026-09-03) with different unit prices to prove ratio-of-sums:
    // Sale 1: 30 units, revenue 6000, COGS 3000 -> margin 3000 (50% line rate)
    // Sale 2: 10 units, revenue 3000, COGS 1000 -> margin 2000 (66.67% line rate)
    // Combined: 40 units sold, revenue 9000, COGS 4000, gross_margin 5000,
    // exact gross_margin_rate = 5000 / 9000 = 0.555555... (NOT average of 50% and 66.67% = 58.33%!).
    // Sell-through = 40 / 100 = 0.4; WOS over 4 weeks = 60 / (40 / 4) = 6; GMROI = 5000 / 6000 = 0.833333...
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

    // ST-SOLE-CFC-2: MISSING COST coverage (one costed + one missing_cost sale, one missing_cost lot).
    sole.ok(
        "supply.stock_lot.record",
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
        "supply.pos_sale.record",
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
            "cost_status": "costed", "unit_cost": "100", "stock_value": "2000", "currency": "EGP"
        }),
    )?;
    sole.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-S3-STALE", "store_id": "ST-SOLE-CFC-3", "sku_id": "SKU-DRESS",
            "on_hand_qty": "28", "unit": "each", "quantity_basis": "on_hand",
            "as_of": "2026-08-10T09:00:00Z", "count_source": "handheld_scan", "count_ref": "CC-OLD"
        }),
    )?;

    // --- Cell B (LE-PARTNER, USD) ---
    // ST-PART-CFC-1: fully costed USD jewelry store in the same mall `MALL-CFC`.
    partner.ok(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-P1-W3", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "owner_entity_id": "LE-PARTNER", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "20", "remaining_qty": "12", "unit": "each",
            "received_at": "2026-07-22T09:00:00Z", "as_of": "2026-09-05T10:00:00Z",
            "cost_status": "costed", "unit_cost": "250", "stock_value": "3000", "currency": "USD"
        }),
    )?;
    partner.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-P1-AVAIL", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
            "on_hand_qty": "12", "unit": "each", "quantity_basis": "available",
            "as_of": "2026-09-06T10:00:00Z", "count_source": "store_audit", "count_ref": "AUD-P1"
        }),
    )?;
    partner.ok(
        "supply.store_count.record",
        "clerk:hana",
        json!({
            "store_count_id": "CNT-P1-OH", "store_id": "ST-PART-CFC-1", "sku_id": "SKU-RING-18K",
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

    // Cross-currency StockLot at an EGP store refuses.
    sole.refused(
        "supply.stock_lot.record",
        "clerk:hana",
        json!({
            "lot_id": "LOT-BAD-CCY", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "10", "remaining_qty": "10", "unit": "each",
            "received_at": "2026-08-01T09:00:00Z", "as_of": "2026-08-01T09:00:00Z",
            "cost_status": "costed", "unit_cost": "10", "stock_value": "100", "currency": "USD"
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
            "cost_status": "costed", "unit_cost": "100", "stock_value": "1000", "currency": "EGP"
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
        matches!(cross_scope, Err(CellError::ScopeMismatch { .. })),
        "foreign scope query must refuse: {cross_scope:?}"
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

    // 1. POS sales feed: unit/currency normalization, negative-qty law quarantine,
    //    unknown-unit enum quarantine, redelivery idempotency, and recovery.
    let pos_in = map_input("supply.pos_sales.mapping", "pos_sale_line");
    let clean_pos = r#"{"pos_line_id":"POS-1","branch_code":"ST-SOLE-CFC-1","barcode_sku":"SKU-DRESS","qty":3,"uom":"PCS","net_amount":600,"ccy":"LE","sold_at":"2026-09-01T14:00:00Z"}"#;
    let (bundle, fresh) = cell
        .inject_mapping_bundle(
            &raw_obs("synthetic-pos", "ev-pos-1", "POS-1", clean_pos)?,
            &pos_in,
        )
        .map_err(|e| format!("{e}"))?;
    assert!(fresh);
    assert!(
        cell.process_mapping_bundle(&bundle)
            .map_err(|e| format!("{e}"))?
    );
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
    assert!(open_findings(&cell, "supply.PosSaleEvidence", "POS-1")?.is_empty());

    // Bad POS row (negative quantity) -> quarantined via SalePositive law.
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

    // Exact redelivery commits nothing; new event id for same bad row never duplicates active finding.
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

    // Valid correction clears the active finding.
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
struct CountStalenessFinding {
    store_id: String,
    sku_id: String,
    last_count_as_of: String,
    age_ms: u64,
    counted_qty: String,
    derived_lot_qty: String,
}

fn parse_rfc3339_ms(ts: &str) -> Result<u64, String> {
    let (secs, nanos) = norn_expr::temporal::timestamp_ordering_key(ts)
        .map_err(|e| format!("parse timestamp {ts}: {e}"))?;
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

/// Inspect physical counts vs derived lot on-hand and fail closed whenever a
/// store's latest count exceeds `max_age_ms` or drifts from derived lots.
/// (Wires into `#1905` `RequiredSources` freshness once B6 lands.)
fn detect_count_staleness_and_drift(
    book: &EntityBook,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<Vec<CountStalenessFinding>, String> {
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
    let mut findings = Vec::new();
    for count in &counts {
        if count.values.get("quantity_basis").map(scalar).as_deref() != Some("on_hand") {
            continue;
        }
        let store_id = count.values.get("store_id").map(scalar).unwrap_or_default();
        let sku_id = count.values.get("sku_id").map(scalar).unwrap_or_default();
        let as_of = count.values.get("as_of").map(scalar).unwrap_or_default();
        let as_of_ms = parse_rfc3339_ms(&as_of)?;
        let age_ms = now_ms.saturating_sub(as_of_ms);
        let counted = match count.values.get("on_hand_qty") {
            Some(RuntimeValue::Number(d)) => *d,
            _ => Decimal::from(0_i64),
        };
        let derived = derived_on_hand
            .get(&(store_id.clone(), sku_id.clone()))
            .copied()
            .unwrap_or_else(|| Decimal::from(0_i64));
        if age_ms > max_age_ms || counted != derived {
            findings.push(CountStalenessFinding {
                store_id,
                sku_id,
                last_count_as_of: as_of,
                age_ms,
                counted_qty: counted.canonical(),
                derived_lot_qty: derived.canonical(),
            });
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

    // Attempting to rewrite a lot's `received_at` during a balance update refuses.
    sole.refused(
        "supply.stock_lot.update_balance",
        "clerk:hana",
        json!({
            "lot_id": "LOT-S1-W1", "store_id": "ST-SOLE-CFC-1", "sku_id": "SKU-DRESS",
            "owner_entity_id": "LE-SOLE", "custodian_id": "CUST-MALL-CFC-OPS",
            "received_qty": "60", "remaining_qty": "20", "unit": "each",
            "received_at": "2026-09-01T09:00:00Z", "as_of": "2026-09-06T09:00:00Z",
            "cost_status": "costed", "unit_cost": "100", "stock_value": "2000", "currency": "EGP"
        }),
    )?;

    // Admit and evaluate all 7 declared retail KPI questions on Cell A.
    let availability = sole.answer("supply.retail.store_availability_weekly")?;
    assert_eq!(
        bucket_total(&availability, &["ST-SOLE-CFC-1", "each", "2026-08-31"]).as_deref(),
        Some("56"),
        "store availability as-of W9"
    );
    let on_hand = sole.answer("supply.retail.store_on_hand_weekly")?;
    assert_eq!(
        bucket_total(&on_hand, &["ST-SOLE-CFC-1", "each", "2026-08-31"]).as_deref(),
        Some("60"),
        "derived lot on-hand = 25 + 35"
    );
    let stock_val = sole.answer("supply.retail.store_stock_value_weekly")?;
    assert_eq!(
        bucket_total(&stock_val, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
        Some("6000"),
        "costed stock value = 2500 + 3500 EGP"
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
    assert_eq!(
        bucket_total(&sales_q, &["ST-SOLE-CFC-1", "each", "2026-08-31"]).as_deref(),
        Some("40")
    );
    let rev_q = sole.answer("supply.retail.store_revenue_weekly")?;
    assert_eq!(
        bucket_total(&rev_q, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"]).as_deref(),
        Some("9000")
    );
    // Gross margin rate = sum(5000) / sum(9000) = 0.555555... (ratio of sums, NOT average of 50% and 66.67%).
    let gm_rate = sole.answer("supply.retail.store_gross_margin_rate_weekly")?;
    let rate_str = bucket_total(&gm_rate, &["ST-SOLE-CFC-1", "EGP", "2026-08-31"])
        .ok_or("missing ST-SOLE-CFC-1 gross margin rate")?;
    assert!(
        rate_str.starts_with("0.555555"),
        "expected ratio-of-sums 5000/9000 = 0.555555..., got {rate_str}"
    );

    // Chasm-trap-safe retail transforms: StoreSalesPerformance & StoreLotAging.
    let sales_tf = sole.evaluate_transform("supply.retail.store_sales_performance", |_| {
        Ok(RowAccess::Read)
    })?;
    assert!(sales_tf.complete);
    let s1_row = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1"))
        .ok_or("missing ST-SOLE-CFC-1 sales row")?;
    assert_eq!(
        s1_row.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(true))
    );
    assert_eq!(
        s1_row.values.get("net_revenue").map(scalar).as_deref(),
        Some("9000")
    );
    assert_eq!(
        s1_row
            .values
            .get("effective_gross_margin")
            .map(scalar)
            .as_deref(),
        Some("5000")
    );
    // Sell-through = 40 / (60 + 40) = 0.4; WOS over 4 weeks = 60 / (40/4) = 6; GMROI = 5000 / 6000.
    use norn_checker::metric::compute::arithmetic::{FACTOR, ratio};
    let sold_scaled = 40_i128.saturating_mul(FACTOR);
    let received_scaled = 100_i128.saturating_mul(FACTOR);
    let on_hand_scaled = 60_i128.saturating_mul(FACTOR);
    let margin_scaled = 5_000_i128.saturating_mul(FACTOR);
    let stock_val_scaled = 6_000_i128.saturating_mul(FACTOR);
    assert_eq!(
        ratio(sold_scaled, received_scaled),
        Some(400_000),
        "exact sell-through ratio = 0.400000"
    );
    let weekly_rate = ratio(sold_scaled, 4_i128.saturating_mul(FACTOR)).ok_or("weekly rate")?;
    assert_eq!(weekly_rate, 10_000_000, "10 units/week over 4 weeks");
    assert_eq!(
        ratio(on_hand_scaled, weekly_rate),
        Some(6_000_000),
        "exact 6.000000 weeks of supply"
    );
    assert_eq!(
        ratio(margin_scaled, stock_val_scaled),
        Some(833_333),
        "exact GMROI 5000/6000 = 0.833333"
    );

    // Missing-cost withholding at ST-SOLE-CFC-2: `cost_complete == false`, margin & rate withheld (`Null`)!
    let s2_row = sales_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-2"))
        .ok_or("missing ST-SOLE-CFC-2 sales row")?;
    assert_eq!(
        s2_row.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        s2_row.values.get("effective_cogs"),
        Some(&RuntimeValue::Null)
    );
    assert_eq!(
        s2_row.values.get("effective_gross_margin"),
        Some(&RuntimeValue::Null)
    );
    assert_eq!(
        s2_row.values.get("gross_margin_rate"),
        Some(&RuntimeValue::Null)
    );

    // Lot aging transform flags missing cost on ST-SOLE-CFC-2 and preserves oldest receipt date on ST-SOLE-CFC-1.
    let aging_tf =
        sole.evaluate_transform("supply.retail.store_lot_aging", |_| Ok(RowAccess::Read))?;
    let s2_lot = aging_tf
        .rows
        .iter()
        .find(|r| r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-2"))
        .ok_or("missing ST-SOLE-CFC-2 lot row")?;
    assert_eq!(
        s2_lot.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(false))
    );
    let s1_w1_lot = aging_tf
        .rows
        .iter()
        .find(|r| {
            r.values.get("store_id").map(scalar).as_deref() == Some("ST-SOLE-CFC-1")
                && r.values.get("oldest_received_at").map(scalar).as_deref()
                    == Some("2026-07-08T09:00:00Z")
        })
        .ok_or("missing W1 oldest lot cohort for ST-SOLE-CFC-1")?;
    assert_eq!(
        s1_w1_lot.values.get("cost_complete"),
        Some(&RuntimeValue::Bool(true))
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

    // Count-staleness & drift check fails closed for ST-SOLE-CFC-3 (28 days old & 28 != 20),
    // while fresh ST-SOLE-CFC-1 has zero findings.
    let stale_findings = detect_count_staleness_and_drift(&sole, CLOCK_MS, MAX_COUNT_AGE_MS)?;
    assert_eq!(stale_findings.len(), 1, "{stale_findings:?}");
    let finding = stale_findings.first().ok_or("expected 1 stale finding")?;
    assert_eq!(finding.store_id, "ST-SOLE-CFC-3");
    assert!(finding.age_ms > MAX_COUNT_AGE_MS);
    assert_eq!(finding.counted_qty, "28");
    assert_eq!(finding.derived_lot_qty, "20");

    // LINEAGE-SUM-1: scoped number-to-source drill-through on `store_revenue_weekly`.
    let rev_plan = sole
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let rev_bucket = rev_q
        .rows
        .keys()
        .find(|k| k.contains("ST-SOLE-CFC-1") && k.contains("EGP") && k.contains("2026-08-31"))
        .cloned()
        .ok_or("missing revenue bucket")?;
    let mut allow_values = |_: &norn_cell::ObjectRecord| Ok(true);
    let mut allow_source = |_: &norn_cell::ObjectRecord| Ok(true);
    let traced = sole
        .cell
        .store()
        .trace_question(
            &rev_plan,
            sole.query_context(),
            &rev_q.lineage_ref,
            &rev_bucket,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut allow_values,
                source: &mut allow_source,
            },
        )
        .map_err(|e| format!("trace authorized: {e}"))?;
    assert_eq!(traced.contributions.len(), 2, "SALE-S1-1 and SALE-S1-2");
    assert!(
        traced
            .contributions
            .iter()
            .all(|c| !c.restricted && c.values.is_some())
    );

    // Restricted reader: numeric membership stays pinned, values & source stay withheld.
    let mut deny_values = |_: &norn_cell::ObjectRecord| Ok(false);
    let mut deny_source = |_: &norn_cell::ObjectRecord| Ok(false);
    let restricted = sole
        .cell
        .store()
        .trace_question(
            &rev_plan,
            sole.query_context(),
            &rev_q.lineage_ref,
            &rev_bucket,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut deny_values,
                source: &mut deny_source,
            },
        )
        .map_err(|e| format!("trace restricted: {e}"))?;
    assert_eq!(restricted.contributions.len(), 2);
    assert!(restricted.contributions.iter().all(|c| {
        c.restricted
            && c.values.is_none()
            && c.amounts.is_none()
            && matches!(c.source, ContributionSource::Restricted)
    }));

    Ok(())
}

// ------------------------------------------------------------------ Test 4

/// Per-cell frontier & coverage metadata captured under `READ-LOCAL-1`.
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
    missing_cost_stores: BTreeSet<String>,
    stale_count_stores: BTreeSet<String>,
    /// Exact store-level revenue (never cross-currency summed).
    entity_net_revenue: Option<String>,
    /// Withheld (`None`) whenever any store in the entity has missing cost.
    entity_gross_margin: Option<String>,
    /// Withheld (`None`) whenever any store in the entity has stale/drifted counts.
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
    let stale = detect_count_staleness_and_drift(book, CLOCK_MS, MAX_COUNT_AGE_MS)?;

    let mut missing_cost_stores = BTreeSet::new();
    let mut rev_sum = Decimal::from(0_i64);
    let mut margin_sum = Decimal::from(0_i64);
    for row in &sales.rows {
        let store_id = row.values.get("store_id").map(scalar).unwrap_or_default();
        if let Some(RuntimeValue::Number(rev)) = row.values.get("net_revenue") {
            rev_sum = rev_sum.checked_add(*rev).ok_or("rev overflow")?;
        }
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
    let stale_count_stores: BTreeSet<String> = stale.into_iter().map(|f| f.store_id).collect();
    let complete = sales.complete
        && lots.complete
        && missing_cost_stores.is_empty()
        && stale_count_stores.is_empty();

    Ok(EntityFrontierSnapshot {
        cell_id: envelope.token.context.frontier.cell.as_str().to_owned(),
        legal_entity_id: legal_entity_id.to_owned(),
        book_code,
        currency,
        scope: book.scope.clone(),
        world: envelope.token.context.frontier.world,
        revision: envelope.token.context.frontier.revision,
        definition_version: envelope.token.context.definition_pin.version,
        complete,
        entity_net_revenue: Some(rev_sum.canonical()),
        entity_gross_margin: missing_cost_stores
            .is_empty()
            .then(|| margin_sum.canonical()),
        entity_on_hand_verified: stale_count_stores.is_empty().then_some(true),
        missing_cost_stores,
        stale_count_stores,
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

    let view = CombinedManagementView {
        view_kind: "combined_management_view",
        is_consolidated_financial_statement: false,
        read_only: true,
        entities: vec![
            project_entity_frontier(&sole, "LE-SOLE")?,
            project_entity_frontier(&partner, "LE-PARTNER")?,
        ],
    };

    assert_eq!(view.view_kind, "combined_management_view");
    assert!(!view.is_consolidated_financial_statement);
    assert!(view.read_only);
    assert_eq!(view.entities.len(), 2);

    // Cell A (`LE-SOLE`, EGP) carries its own cell_id & frontier, reports exact revenue (10000 EGP),
    // and WITHHOLDS entity gross margin & verified on-hand because ST-SOLE-CFC-2 lacks cost and ST-SOLE-CFC-3 has a stale count!
    let sole_snap = view.entities.first().ok_or("missing sole snap")?;
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
        "withheld on stale count"
    );
    assert!(sole_snap.missing_cost_stores.contains("ST-SOLE-CFC-2"));
    assert!(sole_snap.stale_count_stores.contains("ST-SOLE-CFC-3"));

    // Cell B (`LE-PARTNER`, USD) carries its distinct cell_id & frontier, stays completely uncontaminated,
    // and reports exact complete USD revenue (3200 USD) and gross margin (1200 USD).
    let partner_snap = view.entities.get(1).ok_or("missing partner snap")?;
    assert_eq!(partner_snap.cell_id, PARTNER_CELL_ID);
    assert_ne!(partner_snap.cell_id, sole_snap.cell_id);
    assert_eq!(partner_snap.book_code, "BOOK-PARTNER-USD");
    assert_eq!(partner_snap.currency, "USD");
    assert!(partner_snap.revision > 0);
    assert!(partner_snap.complete);
    assert_eq!(partner_snap.entity_net_revenue.as_deref(), Some("3200"));
    assert_eq!(partner_snap.entity_gross_margin.as_deref(), Some("1200"));
    assert_eq!(partner_snap.entity_on_hand_verified, Some(true));

    // Optional DuckDB analytical mirror parity across both isolated tenant books.
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

        for (book, expected_rev) in [
            (&sole, ("ST-SOLE-CFC-1", "EGP", "9000")),
            (&partner, ("ST-PART-CFC-1", "USD", "3200")),
        ] {
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
            for q_name in [
                "supply.retail.store_revenue_weekly",
                "supply.retail.store_gross_margin_rate_weekly",
            ] {
                let plan = book.pack.retail_question(q_name)?;
                let exact = book.answer(q_name)?;
                let prepared = store
                    .analytics_prepare(&plan, book.query_context(), &tenant)
                    .map_err(|e| format!("prepare {q_name}: {e}"))?;
                let response = duck
                    .evaluate(prepared.request())
                    .map_err(|e| format!("duckdb {q_name}: {} {}", e.code, e.detail))?;
                let mut answered = store
                    .analytics_answer(prepared, response)
                    .map_err(|e| format!("answer {q_name}: {e}"))?;
                answered.engine = exact.engine;
                assert_eq!(
                    answered, exact,
                    "DuckDB == Cell exact parity for {q_name} in {}",
                    book.scope
                );
                if q_name == "supply.retail.store_revenue_weekly" {
                    assert_eq!(
                        bucket_total(&answered, &[expected_rev.0, expected_rev.1, "2026-08-31"])
                            .as_deref(),
                        Some(expected_rev.2)
                    );
                }
            }
        }
    }

    Ok(())
}
