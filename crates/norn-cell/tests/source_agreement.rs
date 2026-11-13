#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "SOURCE-AGREE-1 conformance over the three systems' mapped holdings"
)]
//! SOURCE-AGREE-1: `ERPNext` and Odoo report the north gold holding on hand,
//! Shopify reports it available. Each later source's other basis is a warning
//! finding recomputed from verified history; it never excludes the holding,
//! and an answer counts it, identically on the exact and engine paths, at the
//! answer's own frontier and only where the reader may see both versions.

use norn_cell::compute::QueryContext;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::source_agreement::{SourceDisagreementCount, SourceDisagreementCounts};
use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, Model, SqliteCell, StoreOptions};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{
    ArtifactEnvelope, DataQualityFindingStatus, DataQualityOutcome, InterfaceDef, LawRoot,
    ObjectTypeDef, PropertyTypeDef, QuestionIR, RuntimeObject, RuntimeValue,
};
use norn_kernel::{ObservationEvent, ObservationId, Observed};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const NORTH: &str = "demo/holding/owner/north/gold";

type Cell = SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>;
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn fixture_artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts: Vec<ArtifactEnvelope> = Vec::new();
    for dir in [
        "core",
        "packs/canon",
        "packs/erpnext",
        "packs/shopify",
        "packs/odoo",
        "packs/sources",
    ] {
        for file in
            fs::read_dir(root().join(format!("registry/{dir}/artifacts"))).expect("artifacts")
        {
            let artifact: ArtifactEnvelope = serde_json::from_str(
                &fs::read_to_string(file.expect("file").path()).expect("read"),
            )
            .expect("artifact");
            if ![
                "commerce.SalesOrder.entity",
                "core.Order.interface",
                "core.Entitlement.entity",
                "identity.BusinessUnit.entity",
                "location.Region.entity",
                "location.Space.entity",
                "party.Brand.entity",
            ]
            .contains(&artifact.artifact_id.as_str())
            {
                artifacts.push(artifact);
            }
        }
    }
    artifacts
}

fn setup(path: &Path) -> Cell {
    setup_with(path, |_| {})
}

fn setup_with(path: &Path, edit: impl FnOnce(&mut Vec<ArtifactEnvelope>)) -> Cell {
    // Every plan in these tests is also checked against the wide journal scope.
    norn_cell::check_link_plans_against_wide_scope(true);
    let mut artifacts = fixture_artifacts();
    edit(&mut artifacts);
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
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
    let mut types: Vec<ObjectTypeDef> = vec![];
    let mut interfaces: Vec<InterfaceDef> = vec![];
    let mut properties: Vec<PropertyTypeDef> = vec![];
    for artifact in &artifacts {
        let payload = serde_json::to_value(artifact).expect("artifact")["payload"].clone();
        if let Some(value) = payload.get("objectType") {
            types.push(serde_json::from_value(value.clone()).expect("object"));
        }
        if let Some(value) = payload.get("interface") {
            interfaces.push(serde_json::from_value(value.clone()).expect("interface"));
        }
        if let Some(value) = payload.get("propertyTypes") {
            properties.extend(
                serde_json::from_value::<Vec<PropertyTypeDef>>(value.clone()).expect("properties"),
            );
        }
    }
    let model = Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("laws");
    let mut store = CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
    // Bound before the first commit, so every exported record carries it.
    store.bind("scope", "org_demo/dev").expect("scope");
    store
        .install_definitions(
            model,
            InstalledDefinitionInput {
                canonical_set: String::from_utf8(
                    norn_ir::canonical::canonical_json_of(&artifacts).expect("set"),
                )
                .expect("UTF8"),
                registry: vec![],
                // A well-formed digest, so the cell's history exports.
                receipt_sha256: norn_ir::canonical::domain_hash_hex(
                    b"norn.test.receipt\0",
                    &"synthetic-mapped-links",
                )
                .expect("receipt digest"),
                admission_receipt: None,
                readmission: None,
                diff: json!({}),
                installed_at: 1_793_702_400_000,
                layout: None,
            },
            None,
            "org_demo/dev",
        )
        .expect("install");
    SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell")
}
fn fixture(system: &str) -> Value {
    serde_json::from_str(
        &fs::read_to_string(root().join(format!("registry/packs/{system}/synthetic.json")))
            .expect("fixture"),
    )
    .expect("json")
}
/// Source versions order lexicographically; the snapshot precedes numbered events.
fn retain_in(cell: &mut Cell, system: &str, row: &Value, event: &str, partition: &str) -> String {
    let record = &row["record"];
    let alias = record
        .get("name")
        .or_else(|| record.get("id"))
        .expect("source alias")
        .as_str()
        .map_or_else(|| record["id"].to_string(), str::to_owned);
    let payload = RuntimeValue::from_json(record.clone()).expect("payload");
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("json"))
        .expect("digest");
    let source = format!("synthetic-{system}");
    let delivery = cell
        .store()
        .export_commit_count()
        .expect("delivery metadata");
    let raw = Observed {
        id: ObservationId(format!("delivery-{event}-{delivery}")),
        provenance: norn_kernel::Provenance {
            source: source.clone(),
            source_id: alias.clone(),
            mapping_version: "synthetic/1".into(),
            observed_at: norn_kernel::LogicalTime::from_millis(
                1_793_702_400_000u64
                    .checked_add(delivery)
                    .expect("test clock"),
            ),
        },
        input: norn_ir::runtime_object! {
            "contract"=>"norn.raw_observation/v1", "source"=>source, "source_id"=>alias,
            "event_id"=>event, "source_version"=>if event == "snapshot" { "0" } else { event }, "pipeline_version"=>"synthetic/1", "kind"=>"upsert", "extracted_at"=>1_793_702_400_000u64, "payload"=>payload, "payload_digest"=>digest,
        },
    };
    let input = MappingInput {
        mapping: format!("{system}.three.mapping"),
        dataset: row["dataset"].as_str().expect("dataset").into(),
        connector_instance: "demo".into(),
        partition: partition.into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
    };
    let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
    id
}

fn capture(cell: &mut Cell, system: &str, row: &Value, event: &str) -> String {
    let id = retain_in(cell, system, row, event, "snapshot");
    cell.process_mapping_bundle(&id).expect("process");
    id
}
/// A steward's review of finding `id`, as `source.agreement.accept` or
/// `reopen` writes it.
fn review(cell: &mut Cell, id: &str, status: &str, expected: u64) {
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
                            RuntimeValue::from("Shopify counts what is free to sell"),
                        ),
                        ("reviewed_by".to_owned(), RuntimeValue::from("steward")),
                    ]),
                },
            }],
            ..norn_kernel::CellCommit::empty()
        },
    )
    .expect("review");
}

fn north_status(cell: &Cell, index: usize) -> DataQualityFindingStatus {
    cell.store()
        .read()
        .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
        .expect("history")[index]
        .finding
        .status
}

fn load(cell: &mut Cell, system: &str) {
    for row in fixture(system)["snapshot"].as_array().expect("rows") {
        capture(cell, system, row, "snapshot");
    }
}

/// The derived "stock on hand as of each week" question over holdings.
fn stock_plan() -> QuestionPlan {
    let space = norn_checker::metric::derive::derive_metric_space(
        &fixture_artifacts(),
        &BTreeSet::new(),
        &LawRoot::Entity("canon.Holding".to_owned()),
    )
    .expect("derived space");
    let metric = space
        .candidates()
        .iter()
        .find(|candidate| {
            candidate
                .definition
                .name
                .ends_with(".quantity.as_of_observed_at")
                && candidate
                    .definition
                    .type_params
                    .metric_aggregation_params
                    .as_ref()
                    .is_some_and(|params| params.semantic_model.0 == "canon.Holding")
        })
        .expect("the holding snapshot");
    let question: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1", "name": "canon.stock_by_week", "objectSet": "canon.Holding",
        "aggregate": [{"metric": metric.definition.name}],
        "groupBy": [{"path": {"property": "observed_at"}, "grain": "week"}],
        "window": {"start": "2026-10-05T00:00:00Z", "end": "2026-11-30T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 60000},
    }))
    .expect("question");
    space.admit(&question, 1000).expect("admitted")
}

fn context() -> QueryContext {
    QueryContext {
        world: MAIN_WORLD.into(),
        scope: "org_demo/dev".into(),
        population_pin: "all-holdings".into(),
        authorization_epoch: 1,
        mapping_pin: "three-systems".into(),
        now_ms: 1_793_702_400_000 + 86_400_000,
        source_cursors: BTreeMap::new(),
        watermarks: BTreeMap::new(),
    }
}

fn counts(subjects: u64) -> SourceDisagreementCounts {
    SourceDisagreementCounts {
        groups: vec![SourceDisagreementCount {
            property: "quantity_basis".into(),
            subjects,
        }],
        complete: true,
        bound: 500,
    }
}

/// SOURCE-AGREE-FLIP and SOURCE-AGREE-RETURN on one holding's history.
#[test]
fn another_sources_basis_is_a_finding_that_resolves_when_the_holding_returns() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("flip.sqlite"));
    load(&mut cell, "erpnext");
    let reader = cell.store().read();
    assert!(
        reader
            .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
            .expect("one source")
            .is_empty()
    );
    load(&mut cell, "shopify");
    let flipped = cell
        .store()
        .read()
        .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
        .expect("two sources");
    assert_eq!(flipped.len(), 1, "{flipped:#?}");
    let finding = &flipped[0].finding;
    assert_eq!(finding.status, DataQualityFindingStatus::Open);
    assert_eq!(finding.severity, norn_ir::DataQualitySeverity::Warning);
    let DataQualityOutcome::SourceDisagreement {
        property,
        earlier,
        later,
    } = &finding.outcome
    else {
        panic!("a source disagreement: {finding:#?}")
    };
    assert_eq!(property.as_str(), "quantity_basis");
    assert_eq!(
        (earlier.value.as_deref(), later.value.as_deref()),
        (Some("on_hand"), Some("available"))
    );
    assert_ne!(earlier.system, later.system);
    assert!(earlier.fact.version < later.fact.version);
    // The holding is never excluded for it.
    assert!(
        cell.store()
            .read()
            .observation_findings(MAIN_WORLD, "canon.Holding", NORTH)
            .expect("findings")
            .iter()
            .all(|f| !matches!(f.outcome, DataQualityOutcome::SourceDisagreement { .. }))
    );
    // A steward accepts it with a reason: acknowledged; reopened, open again.
    let id = finding.id.clone();
    review(&mut cell, &id, "accepted", 0);
    assert_eq!(
        north_status(&cell, 0),
        DataQualityFindingStatus::Acknowledged
    );
    review(&mut cell, &id, "reopened", 1);
    assert_eq!(north_status(&cell, 0), DataQualityFindingStatus::Open);
    load(&mut cell, "odoo");
    let returned = cell
        .store()
        .read()
        .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
        .expect("three sources");
    assert_eq!(
        returned
            .iter()
            .map(|d| d.finding.status)
            .collect::<Vec<_>>(),
        [
            DataQualityFindingStatus::Resolved,
            DataQualityFindingStatus::Open
        ],
        "Odoo's on-hand returns to ERPNext's report and disagrees with Shopify's"
    );
}

/// The stock answer counts the disputed holding and still answers it; the
/// engine path's host function returns the same counts; a reader who cannot
/// see one version gets no count; an earlier answer keeps its frontier.
#[test]
fn an_answer_counts_disputed_members_at_its_frontier_as_its_reader_may_see_them() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("answer.sqlite"));
    let plan = stock_plan();
    load(&mut cell, "erpnext");
    let before = cell
        .store()
        .evaluate_question(&plan, context(), |_| Ok(true))
        .expect("one source answers");
    assert_eq!(before.source_disagreements, None);
    load(&mut cell, "shopify");
    let after = cell
        .store()
        .evaluate_question(&plan, context(), |_| Ok(true))
        .expect("two sources answer");
    let disputed: BTreeSet<String> = after
        .rows
        .values()
        .flat_map(|row| row.members.keys().cloned())
        .collect();
    assert!(disputed.contains(NORTH), "the holding stays in the answer");
    let expected = u64::try_from(
        disputed
            .iter()
            .filter(|id| {
                cell.store()
                    .read()
                    .source_disagreements(MAIN_WORLD, "canon.Holding", id, None)
                    .expect("history")
                    .iter()
                    .any(|d| d.finding.status == DataQualityFindingStatus::Open)
            })
            .count(),
    )
    .expect("count");
    assert!(expected >= 1);
    assert_eq!(after.source_disagreements, Some(counts(expected)));
    // The engine path counts with the same host function (exact/DuckDB parity).
    assert_eq!(
        cell.store()
            .answer_source_disagreements(
                &plan,
                &after.context,
                (after.known_as_of, None),
                &mut |_| Ok(true)
            )
            .expect("engine-path counts"),
        after.source_disagreements
    );
    // SOURCE-AGREE-FRONTIER: the earlier answer's frontier still has none.
    assert_eq!(
        cell.store()
            .answer_source_disagreements(
                &plan,
                &before.context,
                (before.known_as_of, None),
                &mut |_| Ok(true)
            )
            .expect("earlier frontier"),
        None
    );
    // SOURCE-AGREE-HIDDEN: without the later version, no count implies it.
    let shopify_versions: BTreeSet<u64> = cell
        .store()
        .read()
        .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
        .expect("history")
        .iter()
        .map(|d| d.later.version)
        .collect();
    let hidden = cell
        .store()
        .evaluate_question(&plan, context(), |record| {
            Ok(record.id != NORTH || !shopify_versions.contains(&record.version))
        })
        .expect("a narrower reader answers");
    let others = expected.saturating_sub(1);
    assert_eq!(
        hidden.source_disagreements,
        (others > 0).then(|| counts(others)),
        "the hidden holding is neither counted nor implied"
    );
    // An accepted disagreement is acknowledged and no longer counted.
    let north = cell
        .store()
        .read()
        .source_disagreements(MAIN_WORLD, "canon.Holding", NORTH, None)
        .expect("history")
        .remove(0)
        .finding
        .id;
    review(&mut cell, &north, "accepted", 0);
    assert_eq!(
        cell.store()
            .evaluate_question(&plan, context(), |_| Ok(true))
            .expect("after the review")
            .source_disagreements,
        (others > 0).then(|| counts(others))
    );
}
