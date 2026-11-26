#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "adversarial multi-source dirty-data and reconciliation stress wedge"
)]
//! DIRTY-DATA-WEDGE: adversarial multi-source data-plane stress tests on a real
//! file-backed `SqliteCell`.
//!
//! Exercises realistic dirty-data failures across `ERPNext`, Shopify, Odoo and
//! CRM/Billing multi-source feeds:
//! 1. Dirty nulls, partial updates and mapping-gap provenance (`SOURCE-AGREE-1`).
//! 2. Multi-source `IncrementalQuestion` vs exact recompute parity across source
//!    flips, steward accept/reopen reviews and restart (`QUERY-ENGINE-1`).
//! 3. Duplicate redelivery, `FEED-CONFLICT`, non-NFC identity disputes and
//!    inbox-only IVM invalidation (`OBS-EVIDENCE-1`).
//! 4. Out-of-order deliveries (valid-time snapshot ordering & mixed dirty/clean
//!    paged delivery equivalence, `OBS-PAGE-EQUIV`).
//! 5. Upstream schema drift (added/renamed/missing/mistyped/over-scale/non-object
//!    columns) and structured `MAP-EXPLAIN-1` quarantine reasons.
//! 6. Late-arriving bitemporal corrections, `K1`/`K2` historical reproducibility
//!    and anti-leakage refusal (`ASOF-KNOWLEDGE-1`).
//! 7. Quarantined imported observations vs atomic governed write refusal, plus
//!    supertype `observation_findings` parity on inherited types.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_cell::compute::ivm::IncrementalQuestion;
use norn_cell::compute::{Engine, QueryContext, QueryResult};
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::source_agreement::{SourceDisagreementCount, SourceDisagreementCounts};
use norn_cell::{
    AdmittedDefinitions, CellError, CellStore, InstalledDefinitionInput, KnowledgeCutoff,
    MAIN_WORLD, MappingOutcome, Model, SqliteCell, StoreOptions,
};
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{
    ArtifactEnvelope, DataQualityFindingStatus, DataQualityOutcome, InterfaceDef, LawRoot,
    ObjectTypeDef, PropertyTypeDef, QuestionIR, RuntimeObject, RuntimeValue,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, DecisionTurn, FactEvent, LogicalTime, ObservationEvent,
    ObservationId, Observed, commit_action,
};
use serde_json::{Value, json};

const SCOPE: &str = "org_demo/dev";
const NORTH: &str = "demo/holding/owner/north/gold";
const CLOCK: u64 = 1_793_702_400_000;

type ObsCell = SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn three_systems_artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts = Vec::new();
    for dir in [
        "core",
        "packs/canon",
        "packs/erpnext",
        "packs/shopify",
        "packs/odoo",
        "packs/sources",
    ] {
        for entry in
            fs::read_dir(repo().join(format!("registry/{dir}/artifacts"))).expect("artifacts dir")
        {
            let artifact: ArtifactEnvelope = serde_json::from_str(
                &fs::read_to_string(entry.expect("entry").path()).expect("read artifact"),
            )
            .expect("decode artifact");
            if ![
                "commerce.SalesOrder.entity",
                "core.Order.interface",
                "core.Entitlement.entity",
                "identity.BusinessUnit.entity",
                "identity.Directory.interface",
                "location.Region.entity",
                "location.Space.entity",
                "party.Brand.entity",
                "party.Organization.entity",
            ]
            .contains(&artifact.artifact_id.as_str())
            {
                artifacts.push(artifact);
            }
        }
    }
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    artifacts
}

fn build_model(artifacts: &[ArtifactEnvelope]) -> Model {
    let mut types: Vec<ObjectTypeDef> = Vec::new();
    let mut interfaces: Vec<InterfaceDef> = Vec::new();
    let mut properties: Vec<PropertyTypeDef> = Vec::new();
    for artifact in artifacts {
        let payload = serde_json::to_value(artifact).expect("artifact json")["payload"].clone();
        if let Some(value) = payload.get("objectType") {
            types.push(serde_json::from_value(value.clone()).expect("objectType"));
        }
        if let Some(value) = payload.get("interface") {
            interfaces.push(serde_json::from_value(value.clone()).expect("interface"));
        }
        if let Some(value) = payload.get("propertyTypes") {
            properties.extend(
                serde_json::from_value::<Vec<PropertyTypeDef>>(value.clone())
                    .expect("propertyTypes"),
            );
        }
    }
    Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(artifacts)
        .expect("governing laws")
}

fn install_set(path: &Path, artifacts: &[ArtifactEnvelope], model: Model, label: &str) -> ObsCell {
    let mut store = CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    store
        .install_definitions(
            model,
            InstalledDefinitionInput {
                canonical_set: String::from_utf8(
                    norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical set"),
                )
                .expect("utf8"),
                registry: vec![],
                receipt_sha256: norn_ir::canonical::domain_hash_hex(b"norn.test.receipt\0", &label)
                    .expect("receipt digest"),
                admission_receipt: None,
                readmission: None,
                diff: json!({}),
                installed_at: CLOCK,
                layout: None,
            },
            None,
            SCOPE,
        )
        .expect("install");
    SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("cell")
}

fn open_three_systems(path: &Path) -> ObsCell {
    let artifacts = three_systems_artifacts();
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let diagnostics = norn_checker::check_artifact_set(
        &candidate,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| d.severity != norn_ir::DiagnosticSeverity::Error),
        "{diagnostics:#?}"
    );
    let model = build_model(&artifacts);
    install_set(path, &artifacts, model, "dirty-data-wedge")
}

fn fixture(system: &str) -> Value {
    serde_json::from_str(
        &fs::read_to_string(repo().join(format!("registry/packs/{system}/synthetic.json")))
            .expect("fixture"),
    )
    .expect("json")
}

fn raw_envelope(
    system: &str,
    source_id: &str,
    event_id: &str,
    source_version: &str,
    record: &Value,
    clock_offset: u64,
) -> Observed<RuntimeValue> {
    let payload = RuntimeValue::from_json(record.clone()).expect("payload");
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("json"))
        .expect("digest");
    let source = format!("synthetic-{system}");
    Observed {
        id: ObservationId(format!("delivery-{system}-{event_id}-{clock_offset}")),
        provenance: norn_kernel::Provenance {
            source: source.clone(),
            source_id: source_id.to_owned(),
            mapping_version: "synthetic/1".into(),
            observed_at: LogicalTime::from_millis(CLOCK.saturating_add(clock_offset)),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => source,
            "source_id" => source_id,
            "event_id" => event_id,
            "source_version" => source_version,
            "pipeline_version" => "synthetic/1",
            "kind" => "upsert",
            "extracted_at" => CLOCK.saturating_add(clock_offset),
            "payload" => payload,
            "payload_digest" => digest,
        },
    }
}

fn mapping_input(system: &str, dataset: &str, partition: &str) -> MappingInput {
    MappingInput {
        mapping: format!("{system}.three.mapping"),
        dataset: dataset.to_owned(),
        connector_instance: "demo".into(),
        partition: partition.to_owned(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
    }
}

fn source_alias(record: &Value) -> String {
    record.get("name").or_else(|| record.get("id")).map_or_else(
        || "unknown".to_owned(),
        |v| v.as_str().map_or_else(|| v.to_string(), str::to_owned),
    )
}

fn ingest_envelope(
    cell: &mut ObsCell,
    raw: &Observed<RuntimeValue>,
    input: &MappingInput,
) -> (String, bool, MappingOutcome) {
    let commits = cell.store().export_commit_count().expect("commits");
    cell.advance_to(LogicalTime::from_millis(
        CLOCK.saturating_add(commits.saturating_mul(1_000).saturating_add(1_000)),
    ));
    let (id, fresh) = cell.inject_mapping_bundle(raw, input).expect("inject");
    let outcome = cell.process_mapping_outcome(&id).expect("process outcome");
    (id, fresh, outcome)
}

fn ingest_row(
    cell: &mut ObsCell,
    system: &str,
    dataset: &str,
    record: &Value,
    event_id: &str,
    source_version: &str,
) -> (String, bool, MappingOutcome) {
    let commits = cell.store().export_commit_count().expect("commits");
    let alias = source_alias(record);
    let raw = raw_envelope(system, &alias, event_id, source_version, record, commits);
    let input = mapping_input(system, dataset, "snapshot");
    ingest_envelope(cell, &raw, &input)
}

fn load_snapshot(cell: &mut ObsCell, system: &str) {
    for row in fixture(system)["snapshot"].as_array().expect("snapshot") {
        let dataset = row["dataset"].as_str().expect("dataset");
        let (_, _, outcome) = ingest_row(cell, system, dataset, &row["record"], "snapshot", "0");
        assert!(
            matches!(
                outcome,
                MappingOutcome::Landed | MappingOutcome::AlreadyLanded
            ),
            "{system}/{dataset}: {outcome:?}"
        );
    }
}

fn find_snapshot_row(system: &str, dataset: &str, ref_field: &str, target: &str) -> Value {
    fixture(system)["snapshot"]
        .as_array()
        .expect("snapshot")
        .iter()
        .find(|row| {
            row["dataset"].as_str() == Some(dataset)
                && row["record"][ref_field].as_str() == Some(target)
        })
        .expect("snapshot row")["record"]
        .clone()
}

fn stock_plan() -> QuestionPlan {
    let space = norn_checker::metric::derive::derive_metric_space(
        &ScopedView::uniform(&three_systems_artifacts(), &BTreeSet::new()),
        &LawRoot::Entity("canon.Holding".to_owned()),
    )
    .expect("derived space");
    let metric = space
        .candidates()
        .iter()
        .find(|c| {
            c.definition.name.ends_with(".quantity.as_of_observed_at")
                && c.definition
                    .type_params
                    .metric_aggregation_params
                    .as_ref()
                    .is_some_and(|p| p.semantic_model.0 == "canon.Holding")
        })
        .expect("holding snapshot metric");
    let question: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1",
        "name": "canon.stock_by_week",
        "objectSet": "canon.Holding",
        "aggregate": [{"metric": metric.definition.name}],
        "groupBy": [{"path": {"property": "observed_at"}, "grain": "week"}],
        "window": {"start": "2026-10-05T00:00:00Z", "end": "2026-11-30T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 60000},
    }))
    .expect("question");
    space.admit(&question, 1000).expect("admit")
}

fn query_context(now_ms: u64) -> QueryContext {
    QueryContext {
        world: MAIN_WORLD.into(),
        scope: SCOPE.into(),
        population_pin: "all-holdings".into(),
        authorization_epoch: 1,
        mapping_pin: "three-systems".into(),
        now_ms,
        source_cursors: BTreeMap::new(),
        watermarks: BTreeMap::new(),
    }
}

fn review_disagreement(cell: &mut ObsCell, id: &str, status: &str, expected: u64) {
    let commits = cell.store().export_commit_count().expect("commits");
    cell.advance_to(LogicalTime::from_millis(
        CLOCK.saturating_add(commits.saturating_mul(1_000).saturating_add(1_000)),
    ));
    norn_kernel::CellHost::commit(
        cell,
        norn_kernel::CellCommit {
            writes: vec![norn_kernel::FactWrite {
                key: norn_kernel::FactKey::new("source.AgreementReview", id),
                expected: norn_kernel::FactVersion(expected),
                mutation: norn_kernel::FactMutation::Put {
                    values: BTreeMap::from([
                        ("id".to_owned(), RuntimeValue::from(id)),
                        ("status".to_owned(), RuntimeValue::from(status)),
                        (
                            "reason".to_owned(),
                            RuntimeValue::from("steward verified availability vs on-hand"),
                        ),
                        (
                            "reviewed_by".to_owned(),
                            RuntimeValue::from("steward:nadia"),
                        ),
                    ]),
                },
            }],
            ..norn_kernel::CellCommit::empty()
        },
    )
    .expect("steward review");
}

fn assert_ivm_matches_recompute(
    cell: &ObsCell,
    plan: &QuestionPlan,
    view: &mut IncrementalQuestion,
    ctx: &QueryContext,
    step: &str,
) -> QueryResult {
    let advanced = view
        .advance(cell.store(), ctx.clone(), |_| Ok(true))
        .unwrap_or_else(|e| panic!("ivm advance failed at {step}: {e}"))
        .clone();
    assert!(!view.was_rebuilt(), "{step}: must be incremental");
    let mut exact = cell
        .store()
        .evaluate_question(plan, ctx.clone(), |_| Ok(true))
        .unwrap_or_else(|e| panic!("exact evaluate failed at {step}: {e}"));
    exact.engine = Engine::Ivm;
    assert_eq!(advanced, exact, "QUERY-ENGINE-1 IVM == recompute at {step}");
    advanced
}

fn property_disagreements(
    cell: &ObsCell,
    property: &str,
) -> Vec<norn_cell::source_agreement::SourceDisagreement> {
    cell.store()
        .read()
        .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
        .expect("disagreements")
        .into_iter()
        .filter(|d| {
            matches!(
                &d.finding.outcome,
                DataQualityOutcome::SourceDisagreement { property: prop, .. }
                    if prop.as_str() == property
            )
        })
        .collect()
}

/// Wedge 1: Dirty nulls, partial updates, and `SOURCE-AGREE-1` mapping-gap attribution.
#[test]
fn wedge1_dirty_nulls_partial_updates_and_gap_provenance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge1.sqlite"));
    load_snapshot(&mut cell, "erpnext");
    load_snapshot(&mut cell, "shopify");

    let holding = cell
        .store()
        .read()
        .object(MAIN_WORLD, "canon.Holding", NORTH)
        .expect("read")
        .expect("north holding");
    assert_eq!(
        holding.values.get("valuation_method"),
        Some(&RuntimeValue::Null)
    );
    assert_eq!(
        holding.values.get("cost_currency"),
        Some(&RuntimeValue::from("USD"))
    );

    // Dirty null on required field -> structured MAP-EXPLAIN-1 quarantine.
    let mut null_qty =
        find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    null_qty["quantity"] = Value::Null;
    let MappingOutcome::Quarantined(reasons) = ingest_row(
        &mut cell,
        "erpnext",
        "Warehouse Lot Balance",
        &null_qty,
        "dirty-null-qty",
        "10",
    )
    .2
    else {
        panic!("null required quantity must quarantine");
    };
    assert_eq!(reasons[0].code, "mapping.runtime.invalid_record");
    assert_eq!(reasons[0].field.as_deref(), Some("quantity"));

    // ERPNext updates its own `cost_currency` to EUR after Shopify's gap row.
    let mut erp_eur =
        find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    erp_eur["cost_currency"] = json!("EUR");
    erp_eur["modified"] = json!("2026-11-04T09:00:00Z");
    assert_eq!(
        ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &erp_eur,
            "erp-eur",
            "11",
        )
        .2,
        MappingOutcome::Landed
    );
    assert!(
        property_disagreements(&cell, "cost_currency").is_empty(),
        "Shopify declared a gap on cost_currency; ERPNext updating its own currency must not hallucinate a disagreement"
    );

    // Odoo now reports GBP -> disagreement attributes ERPNext ("EUR") vs Odoo ("GBP").
    let mut odoo_gbp = find_snapshot_row("odoo", "stock.quant", "x_norn_ref", NORTH);
    odoo_gbp["cost_currency"] = json!("GBP");
    odoo_gbp["write_date"] = json!("2026-11-05T09:00:00Z");
    assert_eq!(
        ingest_row(
            &mut cell,
            "odoo",
            "stock.quant",
            &odoo_gbp,
            "odoo-gbp",
            "12"
        )
        .2,
        MappingOutcome::Landed
    );
    let found = property_disagreements(&cell, "cost_currency");
    assert_eq!(found.len(), 1, "{found:#?}");
    let DataQualityOutcome::SourceDisagreement { earlier, later, .. } = &found[0].finding.outcome
    else {
        panic!("expected SourceDisagreement");
    };
    assert_eq!(earlier.system, "synthetic-erpnext");
    assert_eq!(earlier.value.as_deref(), Some("EUR"));
    assert_eq!(later.system, "synthetic-odoo");
    assert_eq!(later.value.as_deref(), Some("GBP"));
}

fn commit_write(
    cell: &mut ObsCell,
    object_type: &str,
    id: &str,
    expected: u64,
    mutation: norn_kernel::FactMutation,
) {
    norn_kernel::CellHost::commit(
        cell,
        norn_kernel::CellCommit {
            writes: vec![norn_kernel::FactWrite {
                key: norn_kernel::FactKey::new(object_type, id),
                expected: norn_kernel::FactVersion(expected),
                mutation,
            }],
            ..norn_kernel::CellCommit::empty()
        },
    )
    .expect("commit fact write");
}

/// Wedge 1b: Deleting a record introduces an incarnation boundary so neither
/// `SOURCE-AGREE-1` change detection nor mapping-gap walkback crosses a
/// tombstone to cite pre-deletion reports.
#[test]
fn wedge1b_deletion_tombstone_stops_disagreement_walkback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge1b.sqlite"));
    load_snapshot(&mut cell, "erpnext");

    // Delete NORTH (version 1 -> tombstone version 2).
    commit_write(
        &mut cell,
        "canon.Holding",
        NORTH,
        1,
        norn_kernel::FactMutation::Delete,
    );

    // Shopify recreates NORTH after deletion with `quantity_basis = available`
    // and a gap on `cost_currency`. Must NOT disagree with pre-deletion ERPNext.
    let mut shopify_row =
        find_snapshot_row("shopify", "InventoryLevel", "metafield_norn_ref", NORTH);
    let (_, _, r1) = ingest_row(
        &mut cell,
        "shopify",
        "InventoryLevel",
        &shopify_row,
        "recreate-shopify",
        "10",
    );
    assert_eq!(r1, MappingOutcome::Landed);
    assert!(
        cell.store()
            .read()
            .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
            .expect("disagreements")
            .is_empty(),
        "recreation after deletion must not cross tombstone to disagree with pre-deletion source"
    );

    // Seed non-null `cost_currency = USD` on the new incarnation, then apply a
    // Shopify gap update carrying that inherited `"USD"` value, followed by Odoo
    // reporting `"EUR"` and `quantity_basis = on_hand`.
    let mut recreated = cell
        .store()
        .read()
        .object(MAIN_WORLD, "canon.Holding", NORTH)
        .expect("read recreated")
        .expect("recreated holding");
    recreated
        .values
        .insert("cost_currency".into(), RuntimeValue::from("USD"));
    let version = recreated.version;
    commit_write(
        &mut cell,
        "canon.Holding",
        NORTH,
        version,
        norn_kernel::FactMutation::Put {
            values: recreated.values,
        },
    );
    shopify_row["quantity"] = json!(72);
    shopify_row["updated_at"] = json!("2026-11-04T09:00:00Z");
    let (_, _, r2) = ingest_row(
        &mut cell,
        "shopify",
        "InventoryLevel",
        &shopify_row,
        "shopify-gap-after-delete",
        "11",
    );
    assert_eq!(r2, MappingOutcome::Landed);
    let mut odoo_row = find_snapshot_row("odoo", "stock.quant", "x_norn_ref", NORTH);
    odoo_row["cost_currency"] = json!("EUR");
    assert_eq!(
        ingest_row(&mut cell, "odoo", "stock.quant", &odoo_row, "odoo-1", "12").2,
        MappingOutcome::Landed
    );
    assert!(property_disagreements(&cell, "cost_currency").is_empty());
    let basis = property_disagreements(&cell, "quantity_basis");
    assert_eq!(basis.len(), 1, "{basis:#?}");
    let DataQualityOutcome::SourceDisagreement { earlier, later, .. } = &basis[0].finding.outcome
    else {
        panic!("expected SourceDisagreement");
    };
    assert_eq!(earlier.system, "synthetic-shopify");
    assert_eq!(later.system, "synthetic-odoo");
}

/// Wedge 2: Multi-source `IncrementalQuestion` vs exact recompute parity across
/// source basis flips, steward `accept`/`reopen` reviews, and SQLite restart.
#[test]
fn wedge2_ivm_maintains_source_disagreements_across_flips_and_steward_reviews() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("wedge2.sqlite");
    let mut cell = open_three_systems(&path);
    let plan = stock_plan();
    let ctx = query_context(CLOCK + 86_400_000);

    load_snapshot(&mut cell, "erpnext");
    let mut view = IncrementalQuestion::rebuild(cell.store(), &plan, ctx.clone(), |_| Ok(true))
        .expect("rebuild");
    assert_eq!(view.result().source_disagreements, None);

    let shopify_holding =
        find_snapshot_row("shopify", "InventoryLevel", "metafield_norn_ref", NORTH);
    assert_eq!(
        ingest_row(
            &mut cell,
            "shopify",
            "InventoryLevel",
            &shopify_holding,
            "shop-flip",
            "1",
        )
        .2,
        MappingOutcome::Landed
    );
    let after_flip = assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "shopify flip");
    assert_eq!(
        after_flip.source_disagreements,
        Some(SourceDisagreementCounts {
            groups: vec![SourceDisagreementCount {
                property: "quantity_basis".into(),
                subjects: 1,
            }],
            complete: true,
            bound: 500,
        })
    );

    let finding_id = property_disagreements(&cell, "quantity_basis")[0]
        .finding
        .id
        .clone();
    review_disagreement(&mut cell, &finding_id, "accepted", 0);
    let after_accept =
        assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "steward accept");
    assert_eq!(after_accept.source_disagreements, None);

    review_disagreement(&mut cell, &finding_id, "reopened", 1);
    let after_reopen =
        assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "steward reopen");
    assert_eq!(
        after_reopen.source_disagreements,
        after_flip.source_disagreements
    );

    let model = cell.store().model().clone();
    drop(cell);
    let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopen store");
    let rebuilt =
        IncrementalQuestion::rebuild(&reopened, &plan, ctx.clone(), |_| Ok(true)).expect("rebuild");
    let mut exact = reopened
        .evaluate_question(&plan, ctx, |_| Ok(true))
        .expect("exact after reopen");
    exact.engine = Engine::Ivm;
    assert_eq!(rebuilt.result(), &exact);
}

/// Wedge 3a: Exact redelivery is idempotent; `FEED-CONFLICT` tampering
/// quarantines the subject and invalidates IVM without a new ledger commit.
#[test]
fn wedge3a_duplicates_and_feed_conflicts_invalidate_ivm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge3a.sqlite"));
    let plan = stock_plan();
    let ctx = query_context(CLOCK + 86_400_000);
    load_snapshot(&mut cell, "erpnext");

    let mut view = IncrementalQuestion::rebuild(cell.store(), &plan, ctx.clone(), |_| Ok(true))
        .expect("rebuild");
    let mut row = find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    row["modified"] = json!("2026-11-04T09:00:00Z");
    let alias = source_alias(&row);
    let input = mapping_input("erpnext", "Warehouse Lot Balance", "snapshot");
    let exact_raw = raw_envelope("erpnext", &alias, "ev-redeliver", "1", &row, 50_000);
    let (_, first_fresh, first_outcome) = ingest_envelope(&mut cell, &exact_raw, &input);
    assert!(first_fresh && first_outcome == MappingOutcome::Landed);
    // Exact redelivery of the identical raw envelope is idempotent.
    let (_, fresh, outcome) = ingest_envelope(&mut cell, &exact_raw, &input);
    assert!(!fresh && outcome == MappingOutcome::AlreadyLanded);
    // Advance IVM to the landed commit before injecting the inbox-only conflict
    // so the conflict must invalidate IVM with zero ledger advancement.
    let _ = assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "pre-conflict sync");
    let commits_before_conflict = cell.store().export_commit_count().expect("commits");

    // FEED-CONFLICT: same event_id ("ev-redeliver"), tampered quantity.
    let mut tampered = row;
    tampered["quantity"] = json!("999");
    let tampered_raw = raw_envelope("erpnext", &alias, "ev-redeliver", "1", &tampered, 51_000);
    let (conflict_id, fresh, outcome) = ingest_envelope(&mut cell, &tampered_raw, &input);
    assert!(fresh && conflict_id.starts_with("mapping-conflict:"));
    assert!(matches!(outcome, MappingOutcome::Quarantined(_)));
    assert_eq!(
        cell.store().export_commit_count().expect("commits"),
        commits_before_conflict,
        "FEED-CONFLICT is inbox-only and must not advance the ledger frontier"
    );
    assert!(
        cell.store()
            .read()
            .authoritative_object(MAIN_WORLD, "canon.Holding", NORTH)
            .expect("authoritative")
            .is_none()
    );
    let after_conflict =
        assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "inbox feed conflict");
    assert!(after_conflict.exclusions.contains_key(NORTH) && !after_conflict.complete);

    // Replay of the exact tampered envelope adds no duplicate finding.
    let before_count = cell
        .store()
        .read()
        .observation_findings(MAIN_WORLD, "canon.Holding", NORTH)
        .expect("findings")
        .len();
    let (_, fresh_again, _) = ingest_envelope(&mut cell, &tampered_raw, &input);
    assert!(!fresh_again);
    assert_eq!(
        cell.store()
            .read()
            .observation_findings(MAIN_WORLD, "canon.Holding", NORTH)
            .expect("findings")
            .len(),
        before_count
    );
}

/// Wedge 3b: Non-NFC identity disputes quarantine and refuse canonical promotion.
#[test]
fn wedge3b_non_nfc_identity_disputes_refuse_promotion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge3b.sqlite"));
    load_snapshot(&mut cell, "erpnext");

    let west = "demo/holding/owner/west/gold";
    let mut west_row =
        find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    west_row["name"] = json!("LOT-CAFE\u{0301}");
    west_row["custom_norn_ref"] = json!(west);
    let raw_nfd = raw_envelope(
        "erpnext",
        "LOT-CAFE\u{0301}",
        "ev-nfd",
        "1",
        &west_row,
        50_000,
    );
    let input = mapping_input("erpnext", "Warehouse Lot Balance", "snapshot");
    let (id, fresh) = cell
        .inject_mapping_bundle(&raw_nfd, &input)
        .expect("capture nfd");
    assert!(fresh && id.starts_with("mapping-identity:"));
    assert!(matches!(
        cell.process_mapping_outcome(&id).expect("outcome"),
        MappingOutcome::Quarantined(_)
    ));
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, "canon.Holding", west)
            .expect("raw")
            .is_none()
    );
}

/// Wedge 4a: Out-of-order deliveries preserve valid-time snapshot ordering and
/// maintain IVM == recompute.
#[test]
fn wedge4a_out_of_order_deliveries_preserve_valid_time_ordering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge4a.sqlite"));
    let plan = stock_plan();
    let ctx = query_context(CLOCK + 86_400_000);
    load_snapshot(&mut cell, "erpnext");
    let mut view = IncrementalQuestion::rebuild(cell.store(), &plan, ctx.clone(), |_| Ok(true))
        .expect("rebuild");

    let base_row = find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    let mut newer = base_row.clone();
    newer["quantity"] = json!("140");
    newer["modified"] = json!("2026-11-10T10:00:00Z");
    assert_eq!(
        ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &newer,
            "ooo-new",
            "20"
        )
        .2,
        MappingOutcome::Landed
    );
    assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "ooo newer first");

    let mut older = base_row;
    older["quantity"] = json!("250");
    older["modified"] = json!("2026-10-20T10:00:00Z");
    assert_eq!(
        ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &older,
            "ooo-old",
            "10"
        )
        .2,
        MappingOutcome::Landed
    );
    let result = assert_ivm_matches_recompute(&cell, &plan, &mut view, &ctx, "ooo older second");

    let bucket_total = |week: &str| {
        result
            .rows
            .iter()
            .find(|(k, _)| k.contains(week) && k.contains("on_hand"))
            .map(|(_, r)| r.totals[0].canonical())
    };
    assert_eq!(bucket_total("2026-10-19").as_deref(), Some("250"));
    // Nov 9 carries south (200 from Nov 1) + north (140 from Nov 10) = 340.
    assert_eq!(bucket_total("2026-11-09").as_deref(), Some("340"));
}

/// Wedge 4b: Mixed dirty/clean paged delivery (`OBS-PAGE-1` / `INGEST-BATCH-1`)
/// quarantines invalid rows without poisoning valid neighbors.
#[test]
fn wedge4b_mixed_dirty_and_clean_pages_quarantine_honestly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge4b.sqlite"));
    load_snapshot(&mut cell, "erpnext");

    let item = find_snapshot_row("erpnext", "Item", "custom_norn_ref", "demo/item/gold");
    let mut clean_a = item.clone();
    clean_a["name"] = json!("ITEM-PAGE-A");
    clean_a["custom_norn_ref"] = json!("demo/item/page-a");
    let mut dirty_mid = item.clone();
    dirty_mid["name"] = json!("ITEM-PAGE-BAD");
    dirty_mid["custom_norn_ref"] = json!("demo/item/page-bad");
    dirty_mid["price"] = json!("not-a-decimal");
    let mut clean_b = item;
    clean_b["name"] = json!("ITEM-PAGE-B");
    clean_b["custom_norn_ref"] = json!("demo/item/page-b");

    let raws = vec![
        raw_envelope("erpnext", "ITEM-PAGE-A", "pg-1", "1", &clean_a, 90_000),
        raw_envelope("erpnext", "ITEM-PAGE-BAD", "pg-2", "1", &dirty_mid, 91_000),
        raw_envelope("erpnext", "ITEM-PAGE-B", "pg-3", "1", &clean_b, 92_000),
    ];
    let captured = cell
        .inject_mapping_page(&raws, &mapping_input("erpnext", "Item", "page-mix"))
        .expect("capture page");
    assert_eq!(captured.pages.len(), 2);
    for (page_id, _) in &captured.pages {
        cell.process_mapping_page(page_id).expect("process page");
    }
    assert!(matches!(
        cell.process_mapping_outcome(&captured.records[1])
            .expect("dirty"),
        MappingOutcome::Quarantined(_)
    ));
    let reader = cell.store().read();
    assert!(
        reader
            .object(MAIN_WORLD, "canon.Item", "demo/item/page-a")
            .expect("a")
            .is_some()
    );
    assert!(
        reader
            .object(MAIN_WORLD, "canon.Item", "demo/item/page-b")
            .expect("b")
            .is_some()
    );
    assert!(
        reader
            .object(MAIN_WORLD, "canon.Item", "demo/item/page-bad")
            .expect("bad")
            .is_none()
    );
}

/// Wedge 5a: Additive upstream schema drift lands cleanly; renamed/missing
/// required columns quarantine with structured `MAP-EXPLAIN-1` field reasons.
#[test]
fn wedge5a_additive_and_breaking_schema_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge5a.sqlite"));
    load_snapshot(&mut cell, "erpnext");
    let template = find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);

    let mut added = template.clone();
    added["name"] = json!("LOT-DRIFT-EXTRA");
    added["custom_norn_ref"] = json!("demo/holding/drift-extra");
    added["erp_v16_warehouse_zone"] = json!("ZONE-9");
    assert_eq!(
        ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &added,
            "drift-extra",
            "1"
        )
        .2,
        MappingOutcome::Landed
    );

    let mut renamed = template;
    renamed["name"] = json!("LOT-DRIFT-RENAME");
    renamed["custom_norn_ref"] = json!("demo/holding/drift-rename");
    renamed.as_object_mut().expect("obj").remove("quantity");
    renamed["qty"] = json!("100");
    let MappingOutcome::Quarantined(reasons) = ingest_row(
        &mut cell,
        "erpnext",
        "Warehouse Lot Balance",
        &renamed,
        "drift-rename",
        "1",
    )
    .2
    else {
        panic!("missing required column must quarantine");
    };
    assert_eq!(reasons[0].code, "mapping.runtime.invalid_record");
    assert_eq!(reasons[0].field.as_deref(), Some("quantity"));
}

/// Wedge 5b: Mistyped columns, exact decimal text (`MAP-DECIMAL-TEXT-1`),
/// over-scale decimals (`PROPERTY-RULE-PIN-1`), and non-object payloads.
#[test]
fn wedge5b_mistyped_overscale_and_non_object_payloads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge5b.sqlite"));
    load_snapshot(&mut cell, "erpnext");
    let template = find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);

    for (event, field, bad_val, expected_code) in [
        (
            "bad-comma",
            "quantity",
            json!("1,234.50"),
            "mapping.runtime.invalid_record",
        ),
        (
            "bad-sci",
            "quantity",
            json!("1e3"),
            "mapping.runtime.invalid_record",
        ),
        (
            "bad-ccy",
            "cost_currency",
            json!("ZZZ"),
            "mapping.runtime.evaluation_failed",
        ),
        (
            "bad-ts",
            "modified",
            json!("2026-13-45T99:00:00Z"),
            "mapping.runtime.evaluation_failed",
        ),
    ] {
        let mut bad = template.clone();
        bad["name"] = json!(format!("LOT-{event}"));
        bad["custom_norn_ref"] = json!(format!("demo/holding/{event}"));
        bad[field] = bad_val;
        let MappingOutcome::Quarantined(reasons) = ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &bad,
            event,
            "1",
        )
        .2
        else {
            panic!("{event} must quarantine");
        };
        assert_eq!(reasons[0].code, expected_code, "{event}: {reasons:?}");
    }

    let mut exact_text = template;
    exact_text["name"] = json!("LOT-EXACT-TEXT");
    exact_text["custom_norn_ref"] = json!("demo/holding/exact-text");
    exact_text["quantity"] = json!("125.500");
    assert_eq!(
        ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &exact_text,
            "exact-text",
            "1"
        )
        .2,
        MappingOutcome::Landed
    );
}

/// Wedge 5c: Over-scale decimals (`PROPERTY-RULE-PIN-1`) never become
/// authoritative, and non-object payloads quarantine without failing capture.
#[test]
fn wedge5c_overscale_and_non_object_payloads_quarantine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge5c.sqlite"));
    load_snapshot(&mut cell, "erpnext");
    let mut overscale =
        find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    overscale["name"] = json!("LOT-OVERSCALE");
    overscale["custom_norn_ref"] = json!("demo/holding/overscale");
    overscale["stock_cost"] = json!("12.1234567");
    let (_, _, outcome) = ingest_row(
        &mut cell,
        "erpnext",
        "Warehouse Lot Balance",
        &overscale,
        "overscale",
        "1",
    );
    assert!(
        cell.store()
            .read()
            .authoritative_object(MAIN_WORLD, "canon.Holding", "demo/holding/overscale")
            .expect("authoritative")
            .is_none(),
        "over-scale decimal must never become authoritative (outcome={outcome:?})"
    );

    let raw_arr = raw_envelope(
        "erpnext",
        "CORRUPT-1",
        "bad-shape",
        "1",
        &json!([1, 2, 3]),
        99_000,
    );
    let (id, fresh) = cell
        .inject_mapping_bundle(
            &raw_arr,
            &mapping_input("erpnext", "Warehouse Lot Balance", "snapshot"),
        )
        .expect("capture");
    assert!(fresh);
    let MappingOutcome::Quarantined(reasons) = cell.process_mapping_outcome(&id).expect("outcome")
    else {
        panic!("non-object payload must quarantine");
    };
    assert_eq!(reasons[0].code, "mapping.runtime.invalid_record");
}

/// Wedge 6: Late-arriving bitemporal corrections, immutable `K1` reproducibility,
/// and anti-leakage refusal (`ASOF-KNOWLEDGE-1`).
#[test]
fn wedge6_late_bitemporal_corrections_and_anti_leakage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_three_systems(&dir.path().join("wedge6.sqlite"));
    let plan = stock_plan();
    load_snapshot(&mut cell, "erpnext");

    let k1 = KnowledgeCutoff {
        recorded_at: cell.now(),
        frontier: cell.store().verified_head().expect("k1 head").commits,
    };
    let k1_answer = cell
        .store()
        .evaluate_question_known_at(&plan, query_context(k1.recorded_at.as_millis()), k1, |_| {
            Ok(true)
        })
        .expect("k1 answer");

    // Late correction adjusts NORTH 200 -> 185 on same effective date (2026-11-01).
    let mut corrected =
        find_snapshot_row("erpnext", "Warehouse Lot Balance", "custom_norn_ref", NORTH);
    corrected["quantity"] = json!("185");
    assert_eq!(
        ingest_row(
            &mut cell,
            "erpnext",
            "Warehouse Lot Balance",
            &corrected,
            "late-restate",
            "2"
        )
        .2,
        MappingOutcome::Landed
    );
    let k2 = KnowledgeCutoff {
        recorded_at: cell.now(),
        frontier: cell.store().verified_head().expect("k2 head").commits,
    };

    let k1_replay = cell
        .store()
        .evaluate_question_known_at(&plan, query_context(k1.recorded_at.as_millis()), k1, |_| {
            Ok(true)
        })
        .expect("k1 replay");
    assert_eq!(k1_replay.result, k1_answer.result);

    let k2_answer = cell
        .store()
        .evaluate_question_known_at(&plan, query_context(k2.recorded_at.as_millis()), k2, |_| {
            Ok(true)
        })
        .expect("k2 answer");
    // 2026-10-26 week carries north (185) + south (200) = 385 (delta -15 from K1's 400).
    let k2_oct26 = k2_answer
        .result
        .rows
        .iter()
        .find(|(k, _)| k.contains("2026-10-26"))
        .map(|(_, r)| r.totals[0].canonical());
    assert_eq!(k2_oct26.as_deref(), Some("385"));

    let leak = KnowledgeCutoff {
        recorded_at: k1.recorded_at,
        frontier: k2.frontier,
    };
    let refused = cell.store().evaluate_question_known_at(
        &plan,
        query_context(k1.recorded_at.as_millis()),
        leak,
        |_| Ok(true),
    );
    assert!(matches!(refused, Err(CellError::TemporalLeakage)));
}

struct AllowAll;
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

fn envelope(id: &str, kind: &str, ns: &str, payload: Value, deps: Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": ns,
        "name": id, "version": "0.1.0", "lifecycle": "active", "scope": {},
        "dependencies": deps, "provenance": {"source": "system"}, "payload": payload
    }))
    .expect("envelope")
}

fn crm_entities_and_law() -> Vec<ArtifactEnvelope> {
    let iface = envelope(
        "crm.Accountable.interface",
        "entity",
        "crm",
        json!({
            "entityVersion": "2.0",
            "interface": {
                "name": "crm.Accountable",
                "properties": [
                    {"id": "id", "type": {"kind": "text"}},
                    {"id": "display_name", "type": {"kind": "text"}}
                ]
            }
        }),
        json!([]),
    );
    let base_account = envelope(
        "crm.Account.entity",
        "entity",
        "crm",
        json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "crm.Account",
                "implements": ["crm.Accountable"],
                "properties": [
                    {"id": "id", "type": {"kind": "text"}},
                    {"id": "display_name", "type": {"kind": "text"}}
                ]
            }
        }),
        json!([]),
    );
    let sub_account = envelope(
        "billing.EnterpriseAccount.entity",
        "entity",
        "billing",
        json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "billing.EnterpriseAccount",
                "extends": "crm.Account",
                "properties": [
                    {"id": "credit_limit", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "semi_additive"}}}
                ]
            }
        }),
        json!([]),
    );
    let law = envelope(
        "billing.credit_non_negative.law",
        "law",
        "billing",
        json!({
            "lawVersion": "0.1",
            "id": "billing.credit_non_negative",
            "owner": "billing",
            "subject": "billing.EnterpriseAccount",
            "scope": {"root": {"kind": "entity", "id": "billing.EnterpriseAccount"}, "objectTypes": ["billing.EnterpriseAccount"]},
            "severity": "error",
            "modality": {"kind": "always"},
            "response": {"kind": "predicate", "expression": "self.credit_limit >= 0"},
            "rationale": "Enterprise account credit limits must be non-negative.",
            "provenance": {"source": "authoring_sdk"}
        }),
        json!([{"artifactId": "billing.EnterpriseAccount.entity", "kind": "entity", "required": true, "version": "0.1.0"}]),
    );
    vec![iface, base_account, sub_account, law]
}

fn crm_source_mapping_and_action() -> Vec<ArtifactEnvelope> {
    let source = envelope(
        "billing.feed.source",
        "source_schema",
        "billing",
        json!({
            "format": "norn.source-schema/1",
            "source": {"system": "synthetic-billing", "instance": "demo", "version": "billing/1"},
            "observedAt": "2026-11-02T00:00:00Z",
            "adapter": {"name": "synthetic-record-replay", "version": "0.1.0"},
            "rawMetadata": {"uri": "synthetic://billing", "sha256": "aa".repeat(32)},
            "datasets": [{"id": "EnterpriseAccount", "nativeName": "EnterpriseAccount", "kind": "document"}],
            "fields": [
                {"dataset": "EnterpriseAccount", "name": "id", "ordinal": 0, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                {"dataset": "EnterpriseAccount", "name": "name", "ordinal": 1, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                {"dataset": "EnterpriseAccount", "name": "credit_limit", "ordinal": 2, "nativeType": "Decimal", "typeCandidate": {"type": "decimal", "origin": "declared"}, "required": true}
            ],
            "codeSets": [],
            "constraints": [{"id": "pk", "kind": "primary_key", "dataset": "EnterpriseAccount", "fields": ["id"], "origin": "declared"}]
        }),
        json!([]),
    );
    let pin = norn_checker::ArtifactRef::of(&source).expect("source pin");
    let mapping = envelope(
        "billing.feed.mapping",
        "mapping",
        "billing",
        json!({
            "format": "norn.mapping/1",
            "extensions": {"norn.mapping.generic/1.0.0": {"parity": "unqualified"}},
            "source": {"schemaId": pin.artifact_id, "version": pin.version, "sha256": pin.sha256},
            "interfaces": ["crm.Accountable.interface"],
            "maps": [{
                "id": "enterprise_account",
                "source": {"dataset": "EnterpriseAccount"},
                "target": {"entity": "billing.EnterpriseAccount.entity", "interface": "crm.Accountable.interface"},
                "subject": "input.id",
                "sourceRecord": "'billing/demo/EnterpriseAccount/' + input.id",
                "properties": [
                    {"property": "id", "value": "input.id"},
                    {"property": "display_name", "value": "input.name"},
                    {"property": "credit_limit", "value": "input.credit_limit"}
                ]
            }]
        }),
        json!([
            {"artifactId": "billing.feed.source", "kind": "source_schema", "required": true, "version": "0.1.0"},
            {"artifactId": "crm.Accountable.interface", "kind": "entity", "required": true, "version": "0.1.0"},
            {"artifactId": "billing.EnterpriseAccount.entity", "kind": "entity", "required": true, "version": "0.1.0"}
        ]),
    );
    let action = envelope(
        "billing.account.upsert.action",
        "action",
        "billing",
        json!({
            "actionId": "billing.account.upsert",
            "title": "Upsert enterprise account",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "required": ["id", "display_name", "credit_limit"],
                "properties": {
                    "id": {"type": "string"},
                    "display_name": {"type": "string"},
                    "credit_limit": {"type": "string"}
                },
                "additionalProperties": false
            },
            "outputSchema": {"type": "object", "required": ["id"], "properties": {"id": {"type": "string"}}, "additionalProperties": false},
            "writes": [
                {"entityKind": "billing.EnterpriseAccount", "change": "created"},
                {"entityKind": "billing.EnterpriseAccount", "change": "modified"}
            ],
            "effectProgram": {
                "kind": "upsert_input_object",
                "entityArtifactId": "billing.EnterpriseAccount.entity",
                "entityArtifactVersion": "0.1.0",
                "objectKind": "billing.EnterpriseAccount",
                "objectIdField": "id",
                "writeMode": {"kind": "upsert"}
            }
        }),
        json!([]),
    );
    vec![source, mapping, action]
}

fn crm_billing_artifacts() -> Vec<ArtifactEnvelope> {
    let base_core = repo().join("registry/core/artifacts");
    let mut artifacts: Vec<ArtifactEnvelope> = [
        "core.value_types.entity.json",
        "std.value_types.entity.json",
        "core.units.json",
        "core.time.json",
    ]
    .into_iter()
    .map(|name| {
        serde_json::from_str(&fs::read_to_string(base_core.join(name)).expect("core"))
            .expect("decode core")
    })
    .collect();
    artifacts.extend(crm_entities_and_law());
    artifacts.extend(crm_source_mapping_and_action());
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    artifacts
}

/// Wedge 7a: Quarantined invalid observation on a subtype is visible identically
/// through concrete subtype and supertype `observation_findings` reads.
#[test]
fn wedge7a_quarantined_subtype_observation_and_supertype_findings_parity() {
    let artifacts = crm_billing_artifacts();
    let dir = tempfile::tempdir().expect("tempdir");

    let mut obs_cell = install_set(
        &dir.path().join("wedge7a.sqlite"),
        &artifacts,
        build_model(&artifacts),
        "wedge7",
    );
    let bad_row = json!({"id": "ACCT-1", "name": "Acme Corp", "credit_limit": "-100.00"});
    let raw = raw_envelope("billing", "ACCT-1", "ev-bad", "1", &bad_row, 1_000);
    let input = MappingInput {
        mapping: "billing.feed.mapping".into(),
        dataset: "EnterpriseAccount".into(),
        connector_instance: "demo".into(),
        partition: "default".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
    };
    let (bundle_id, _) = obs_cell
        .inject_mapping_bundle(&raw, &input)
        .expect("capture");
    assert_eq!(
        obs_cell
            .process_mapping_outcome(&bundle_id)
            .expect("process"),
        MappingOutcome::Landed
    );

    let reader = obs_cell.store().read();
    assert!(
        reader
            .authoritative_object(MAIN_WORLD, "billing.EnterpriseAccount", "ACCT-1")
            .expect("sub")
            .is_none()
    );
    assert!(
        reader
            .authoritative_object(MAIN_WORLD, "crm.Account", "ACCT-1")
            .expect("super")
            .is_none()
    );
    let sub_findings = reader
        .observation_findings(MAIN_WORLD, "billing.EnterpriseAccount", "ACCT-1")
        .expect("sub findings");
    let super_findings = reader
        .observation_findings(MAIN_WORLD, "crm.Account", "ACCT-1")
        .expect("super findings");
    assert_eq!(sub_findings.len(), 1);
    assert_eq!(sub_findings[0].status, DataQualityFindingStatus::Open);
    assert_eq!(super_findings, sub_findings);
}

/// Wedge 7b: The exact same invalid proposal via a governed native action
/// refuses atomically and leaves store state byte-unchanged.
#[test]
fn wedge7b_governed_write_refuses_atomically() {
    let artifacts = crm_billing_artifacts();
    let definitions = AdmittedDefinitions::from_envelopes(&artifacts).expect("admitted defs");
    let dir = tempfile::tempdir().expect("tempdir");

    let mut act_store = CellStore::open(
        &dir.path().join("wedge7b.sqlite"),
        definitions.model().clone(),
        StoreOptions::default(),
    )
    .expect("act store");
    act_store.bind("scope", SCOPE).expect("scope");
    let mut act_cell: SqliteCell<FactEvent, RuntimeObject, RuntimeObject> =
        SqliteCell::new(act_store, SCOPE, |_| BTreeMap::new()).expect("act cell");
    let before = act_cell.store().read().dump().expect("dump before");
    let admitted = definitions
        .action("billing.account.upsert")
        .expect("action");
    let catalog = definitions.catalog();
    let call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: "ops:billing".into(),
        approved_intent: None,
        input: serde_json::from_value(
            json!({"id": "ACCT-1", "display_name": "Acme Corp", "credit_limit": "-100.00"}),
        )
        .expect("input"),
        idempotency_key: "gov-bad-1".into(),
        object_type: None,
        turn: DecisionTurn {
            id: "gov-bad-1".into(),
            now: LogicalTime::from_millis(CLOCK + 2_000),
            world: act_cell.store().world(),
            consumes: None,
        },
        trace: None,
    };
    let err = commit_action(&mut act_cell, &catalog, &call, &AllowAll).expect_err("must refuse");
    assert_eq!(err.code(), "core.laws.violated", "{err}");
    assert_eq!(act_cell.store().read().dump().expect("dump after"), before);
}

fn crm_base_feed_and_law() -> Vec<ArtifactEnvelope> {
    let base_law = envelope(
        "crm.account_named.law",
        "law",
        "crm",
        json!({
            "lawVersion": "0.1",
            "id": "crm.account_named",
            "owner": "crm",
            "subject": "crm.Account",
            "scope": {"root": {"kind": "entity", "id": "crm.Account"}, "objectTypes": ["crm.Account"]},
            "severity": "error",
            "modality": {"kind": "always"},
            "response": {"kind": "predicate", "expression": "self.display_name != \"INVALID\""},
            "rationale": "Account display names must be valid.",
            "provenance": {"source": "authoring_sdk"}
        }),
        json!([{"artifactId": "crm.Account.entity", "kind": "entity", "required": true, "version": "0.1.0"}]),
    );
    let source = envelope(
        "crm.feed.source",
        "source_schema",
        "crm",
        json!({
            "format": "norn.source-schema/1",
            "source": {"system": "synthetic-crm", "instance": "demo", "version": "crm/1"},
            "observedAt": "2026-11-02T00:00:00Z",
            "adapter": {"name": "synthetic-record-replay", "version": "0.1.0"},
            "rawMetadata": {"uri": "synthetic://crm", "sha256": "bb".repeat(32)},
            "datasets": [{"id": "Account", "nativeName": "Account", "kind": "document"}],
            "fields": [
                {"dataset": "Account", "name": "id", "ordinal": 0, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                {"dataset": "Account", "name": "name", "ordinal": 1, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true}
            ],
            "codeSets": [],
            "constraints": [{"id": "pk", "kind": "primary_key", "dataset": "Account", "fields": ["id"], "origin": "declared"}]
        }),
        json!([]),
    );
    let pin = norn_checker::ArtifactRef::of(&source).expect("crm source pin");
    let mapping = envelope(
        "crm.feed.mapping",
        "mapping",
        "crm",
        json!({
            "format": "norn.mapping/1",
            "extensions": {"norn.mapping.generic/1.0.0": {"parity": "unqualified"}},
            "source": {"schemaId": pin.artifact_id, "version": pin.version, "sha256": pin.sha256},
            "interfaces": ["crm.Accountable.interface"],
            "maps": [{
                "id": "account",
                "source": {"dataset": "Account"},
                "target": {"entity": "crm.Account.entity", "interface": "crm.Accountable.interface"},
                "subject": "input.id",
                "sourceRecord": "'crm/demo/Account/' + input.id",
                "properties": [
                    {"property": "id", "value": "input.id"},
                    {"property": "display_name", "value": "input.name"}
                ]
            }]
        }),
        json!([
            {"artifactId": "crm.feed.source", "kind": "source_schema", "required": true, "version": "0.1.0"},
            {"artifactId": "crm.Accountable.interface", "kind": "entity", "required": true, "version": "0.1.0"},
            {"artifactId": "crm.Account.entity", "kind": "entity", "required": true, "version": "0.1.0"}
        ]),
    );
    vec![base_law, source, mapping]
}

fn crm_v1_artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts: Vec<ArtifactEnvelope> = crm_billing_artifacts()
        .into_iter()
        .filter(|a| !a.artifact_id.starts_with("billing."))
        .collect();
    artifacts.extend(crm_base_feed_and_law());
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    artifacts
}

fn crm_v2_artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts = crm_billing_artifacts();
    artifacts.extend(crm_base_feed_and_law());
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    artifacts
}

fn upgrade_and_delete_base_account(cell: &mut ObsCell, v2: &[ArtifactEnvelope], id: &str) {
    cell.install_definitions(
        build_model(v2),
        InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                norn_ir::canonical::canonical_json_of(&v2).expect("v2 canonical"),
            )
            .expect("utf8"),
            registry: vec![],
            receipt_sha256: norn_ir::canonical::domain_hash_hex(
                b"norn.test.receipt\0",
                &"wedge7c-v2",
            )
            .expect("v2 receipt"),
            admission_receipt: None,
            readmission: None,
            diff: json!({}),
            installed_at: CLOCK + 10_000,
            layout: None,
        },
        Some(1),
    )
    .expect("install v2");
    norn_kernel::CellHost::commit(
        cell,
        norn_kernel::CellCommit {
            writes: vec![norn_kernel::FactWrite {
                key: norn_kernel::FactKey::new("crm.Account", id),
                expected: norn_kernel::FactVersion(1),
                mutation: norn_kernel::FactMutation::Delete,
            }],
            ..norn_kernel::CellCommit::empty()
        },
    )
    .expect("delete base account");
}

fn account_mapping_input(mapping: &str, dataset: &str, ts: &str) -> MappingInput {
    MappingInput {
        mapping: mapping.into(),
        dataset: dataset.into(),
        connector_instance: "demo".into(),
        partition: "default".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp(ts.into()),
    }
}

/// Wedge 7c: An ancestor finding pinned under an older definitions version
/// (before a subtype was installed) recovers cleanly when the record is deleted
/// and recreated as the new subtype.
#[test]
fn wedge7c_ancestor_finding_before_subtype_install_recovers_cleanly() {
    let v1 = crm_v1_artifacts();
    let v2 = crm_v2_artifacts();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = install_set(
        &dir.path().join("wedge7c.sqlite"),
        &v1,
        build_model(&v1),
        "wedge7c-v1",
    );

    // Record an open ancestor finding pinned at definitions_version = 1.
    let crm_input = account_mapping_input("crm.feed.mapping", "Account", "2026-11-03T00:00:00Z");
    let bad_base = json!({"id": "ACCT-MIG", "name": "INVALID"});
    let raw_base = raw_envelope("crm", "ACCT-MIG", "ev-base-bad", "1", &bad_base, 1_000);
    assert_eq!(
        ingest_envelope(&mut cell, &raw_base, &crm_input).2,
        MappingOutcome::Landed
    );
    let v1_findings = cell
        .store()
        .read()
        .observation_findings(MAIN_WORLD, "crm.Account", "ACCT-MIG")
        .expect("v1 findings");
    assert_eq!(v1_findings.len(), 1, "{v1_findings:#?}");
    let v1_finding_id = v1_findings[0].id.clone();
    assert!(
        cell.store()
            .read()
            .authoritative_object(MAIN_WORLD, "crm.Account", "ACCT-MIG")
            .expect("v1 auth")
            .is_none()
    );

    // Upgrade to definitions v2 introducing subtype `billing.EnterpriseAccount`,
    // delete base incarnation (v1 -> tombstone v2), and recreate as the subtype.
    upgrade_and_delete_base_account(&mut cell, &v2, "ACCT-MIG");
    let billing_input = account_mapping_input(
        "billing.feed.mapping",
        "EnterpriseAccount",
        "2026-11-04T00:00:00Z",
    );
    let still_bad = json!({"id": "ACCT-MIG", "name": "INVALID", "credit_limit": "500.00"});
    let raw_sub_bad = raw_envelope("billing", "ACCT-MIG", "ev-sub-bad", "1", &still_bad, 11_000);
    assert_eq!(
        ingest_envelope(&mut cell, &raw_sub_bad, &billing_input).2,
        MappingOutcome::Landed
    );
    let open = cell
        .store()
        .read()
        .observation_findings(MAIN_WORLD, "billing.EnterpriseAccount", "ACCT-MIG")
        .expect("subtype findings while invalid");
    assert!(
        open.iter()
            .any(|f| f.id == v1_finding_id && f.status == DataQualityFindingStatus::Open),
        "v1 ancestor finding {v1_finding_id} must remain open: {open:#?}"
    );
    assert!(
        cell.store()
            .read()
            .authoritative_object(MAIN_WORLD, "billing.EnterpriseAccount", "ACCT-MIG")
            .expect("auth while invalid")
            .is_none()
    );

    // Fix the ancestor property on the subtype -> v1 ancestor finding resolves.
    let fixed = json!({"id": "ACCT-MIG", "name": "Acme Valid", "credit_limit": "500.00"});
    let raw_sub_good = raw_envelope("billing", "ACCT-MIG", "ev-sub-good", "2", &fixed, 12_000);
    assert_eq!(
        ingest_envelope(&mut cell, &raw_sub_good, &billing_input).2,
        MappingOutcome::Landed
    );
    let resolved = cell
        .store()
        .read()
        .observation_findings(MAIN_WORLD, "billing.EnterpriseAccount", "ACCT-MIG")
        .expect("subtype findings after fix");
    assert!(
        resolved
            .iter()
            .any(|f| f.id == v1_finding_id && f.status == DataQualityFindingStatus::Resolved)
            && resolved
                .iter()
                .all(|f| f.status == DataQualityFindingStatus::Resolved),
        "v1 ancestor finding {v1_finding_id} must resolve: {resolved:#?}"
    );
    let auth = cell
        .store()
        .read()
        .authoritative_object(MAIN_WORLD, "billing.EnterpriseAccount", "ACCT-MIG")
        .expect("auth after fix")
        .expect("must recover");
    assert_eq!(auth.object_type, "billing.EnterpriseAccount");
}
