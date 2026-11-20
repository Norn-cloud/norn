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
use norn_cell::source_resolution::{PropertyDecision, SOURCE_AGREE_2};
use norn_cell::{
    CellStore, InstalledDefinitionInput, MAIN_WORLD, MappingOutcome, Model, SqliteCell,
    StoreOptions,
};
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

/// The fixture set with `edit` applied, sorted, and the model it defines.
fn edited_set(edit: impl FnOnce(&mut Vec<ArtifactEnvelope>)) -> (Vec<ArtifactEnvelope>, Model) {
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
    (artifacts, model)
}

/// The install input of `artifacts`, installed at `installed_at`.
fn definition_input(artifacts: &[ArtifactEnvelope], installed_at: u64) -> InstalledDefinitionInput {
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
        installed_at,
        layout: None,
    }
}

fn setup_with(path: &Path, edit: impl FnOnce(&mut Vec<ArtifactEnvelope>)) -> Cell {
    // Every plan in these tests is also checked against the wide journal scope.
    norn_cell::check_link_plans_against_wide_scope(true);
    let (artifacts, model) = edited_set(edit);
    let mut store = CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
    // Bound before the first commit, so every exported record carries it.
    store.bind("scope", "org_demo/dev").expect("scope");
    store
        .install_definitions(
            model,
            definition_input(&artifacts, 1_793_702_400_000),
            None,
            "org_demo/dev",
        )
        .expect("install");
    SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell")
}
/// SOURCE-AGREE-1 reads disagreement in a holding's version history, so these
/// tests rank the holding's writers by precedence (Odoo, then Shopify, then
/// `ERPNext`): each better-ranked report then replaces the canonical value,
/// where the registry's system of record (`ERPNext`) would keep it.
#[allow(clippy::ptr_arg, reason = "the shape setup_with edits")]
fn holding_precedence(artifacts: &mut Vec<ArtifactEnvelope>) {
    for (system, rank) in [("erpnext", 2), ("shopify", 1), ("odoo", 0)] {
        let artifact = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == format!("{system}.three.mapping"))
            .expect("mapping");
        let mut wire = serde_json::to_value(&*artifact).expect("wire");
        for map in wire["payload"]["maps"].as_array_mut().expect("maps") {
            if map["id"] == "holding" {
                map["authority"] = json!({"kind": "precedence", "rank": rank});
            }
        }
        *artifact = serde_json::from_value(wire).expect("edited mapping");
    }
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

/// [`load`], with the north gold holding reporting `grams` on hand.
fn load_north(cell: &mut Cell, system: &str, grams: u64) {
    for row in fixture(system)["snapshot"].as_array().expect("rows") {
        let mut row = row.clone();
        let record = row["record"].as_object_mut().expect("record");
        let north = ["custom_norn_ref", "x_norn_ref", "metafield_norn_ref"]
            .iter()
            .any(|field| record.get(*field).and_then(Value::as_str) == Some(NORTH));
        if north && record.contains_key("resource_code") {
            record.insert("quantity".into(), json!(grams));
        }
        capture(cell, system, &row, "snapshot");
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
    let mut cell = setup_with(&directory.path().join("flip.sqlite"), holding_precedence);
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
    let mut cell = setup_with(&directory.path().join("answer.sqlite"), holding_precedence);
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

/// SOURCE-RESOLVE-1 on the real analytics engine (D102): the mirror holds
/// version history only, so a snapshot question over holdings several
/// sources resolve is refused there (`core.compute.unsupported_engine`)
/// rather than answered from a history the sync order wrote; the exact
/// engine answers it from per-slice resolution.
#[cfg(feature = "duckdb")]
#[test]
fn the_analytics_engine_refuses_a_snapshot_over_resolved_sources() {
    use norn_cell::analytics::TenantId;
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("engine.sqlite"), holding_precedence);
    load(&mut cell, "erpnext");
    load(&mut cell, "shopify");
    let plan = stock_plan();
    let store = cell.store();
    let exact = store
        .evaluate_question(&plan, context(), |_| Ok(true))
        .expect("the exact engine answers");
    assert!(exact.source_disagreements.is_some(), "{exact:#?}");
    let refused = store
        .analytics_prepare(&plan, context(), &TenantId::of_scope("org_demo/dev"))
        .expect_err("the engine refuses");
    assert!(
        matches!(
            refused,
            norn_cell::CellError::ComputeRefused("core.compute.unsupported_engine")
        ),
        "{refused:?}"
    );
}

/// The canonical values and resolution of the north gold holding, without
/// the commit each report landed in (which differs by order by construction).
/// Each property's decision with every (mapping, value) report.
type Decisions = Vec<(String, PropertyDecision, Vec<(String, RuntimeValue)>)>;
/// Each finding's (property, mapping, value).
type Findings = Vec<(String, String, RuntimeValue)>;

fn north_state(cell: &Cell) -> (Option<norn_ir::RuntimeObject>, Decisions, Findings) {
    let reader = cell.store().read();
    let values = reader
        .object(MAIN_WORLD, "canon.Holding", NORTH)
        .expect("read")
        .map(|record| record.values);
    let resolution = reader
        .source_resolution("canon.Holding", NORTH, None)
        .expect("resolution")
        .expect("the holding is resolved across sources");
    let properties = resolution
        .properties
        .iter()
        .map(|p| {
            (
                p.property.clone(),
                p.decision.clone(),
                p.reports
                    .iter()
                    .map(|r| (r.mapping.clone(), r.value.clone()))
                    .collect(),
            )
        })
        .collect();
    let findings = resolution
        .findings
        .iter()
        .map(|f| {
            (
                f.property.clone(),
                f.report.mapping.clone(),
                f.report.value.clone(),
            )
        })
        .collect();
    (values, properties, findings)
}

/// `ERPNext` the holding's system of record, Odoo and Shopify checks.
#[allow(clippy::ptr_arg, reason = "the shape setup_with edits")]
fn holding_system_of_record(artifacts: &mut Vec<ArtifactEnvelope>) {
    for (system, authority) in [
        ("erpnext", json!({"kind": "system_of_record"})),
        ("shopify", json!({"kind": "check"})),
        ("odoo", json!({"kind": "check"})),
    ] {
        let artifact = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == format!("{system}.three.mapping"))
            .expect("mapping");
        let mut wire = serde_json::to_value(&*artifact).expect("wire");
        for map in wire["payload"]["maps"].as_array_mut().expect("maps") {
            if map["id"] == "holding" {
                map["authority"] = authority.clone();
            }
        }
        *artifact = serde_json::from_value(wire).expect("edited mapping");
    }
}

/// North gold synced `ERPNext` (2765 g), Odoo (2458 g), Shopify, and in the
/// reverse order; the reverse cell is also returned after its first two
/// sources, before `ERPNext` reports.
fn both_orders(
    directory: &Path,
    edit: fn(&mut Vec<ArtifactEnvelope>),
) -> (
    Cell,
    Cell,
    (Option<norn_ir::RuntimeObject>, Decisions, Findings),
) {
    let mut forward = setup_with(&directory.join("forward.sqlite"), edit);
    load_north(&mut forward, "erpnext", 2765);
    load_north(&mut forward, "odoo", 2458);
    load(&mut forward, "shopify");
    let mut backward = setup_with(&directory.join("backward.sqlite"), edit);
    load(&mut backward, "shopify");
    load_north(&mut backward, "odoo", 2458);
    let before = north_state(&backward);
    load_north(&mut backward, "erpnext", 2765);
    (forward, backward, before)
}

/// The decision of `property` in a [`north_state`].
fn decision_of<'a>(decisions: &'a Decisions, property: &str) -> &'a PropertyDecision {
    &decisions
        .iter()
        .find(|(p, ..)| p == property)
        .expect("reported property")
        .1
}

fn grams(value: u64) -> RuntimeValue {
    RuntimeValue::from_json(json!(value)).expect("decimal")
}

/// SOURCE-RESOLVE-1 (#1402): the registry ranks the holding's writers
/// `ERPNext` (2765 g), Odoo (2458 g), Shopify. Syncing in either order yields
/// `ERPNext`'s value and the same SOURCE-AGREE-2 findings, never an overwrite
/// by whichever source synced last.
#[test]
fn source_resolve_1_precedence_decides_the_same_value_in_either_sync_order() {
    let directory = tempfile::tempdir().expect("directory");
    let (forward, backward, before) = both_orders(directory.path(), |_| {});
    // Before ERPNext reports, the best-ranked report that exists decides.
    assert_eq!(
        before.0.as_ref().and_then(|v| v.get("quantity")),
        Some(&grams(2458))
    );
    assert_eq!(
        decision_of(&before.1, "quantity"),
        &PropertyDecision::Decided {
            mapping: "odoo.three.mapping".into(),
            value: grams(2458),
        }
    );
    let (values, properties, findings) = north_state(&forward);
    assert_eq!(
        north_state(&backward),
        (values.clone(), properties.clone(), findings.clone())
    );
    let values = values.expect("the holding exists");
    assert_eq!(values.get("quantity"), Some(&grams(2765)));
    assert_eq!(
        values.get("quantity_basis"),
        Some(&RuntimeValue::from("on_hand"))
    );
    assert_eq!(
        decision_of(&properties, "quantity"),
        &PropertyDecision::Decided {
            mapping: "erpnext.three.mapping".into(),
            value: grams(2765),
        }
    );
    let quantity = properties
        .iter()
        .find(|(p, ..)| p == "quantity")
        .expect("quantity");
    assert_eq!(quantity.2.len(), 3, "every source's report is kept");
    // Each report that differs is a SOURCE-AGREE-2 finding.
    assert!(findings.contains(&("quantity".into(), "odoo.three.mapping".into(), grams(2458),)));
    assert!(findings.contains(&(
        "quantity_basis".into(),
        "shopify.three.mapping".into(),
        RuntimeValue::from("available"),
    )));
    assert!(
        findings
            .iter()
            .all(|(_, mapping, _)| mapping != "erpnext.three.mapping")
    );
    // Every report carries its lineage: the source record that said it.
    let resolution = forward
        .store()
        .read()
        .source_resolution("canon.Holding", NORTH, None)
        .expect("resolution")
        .expect("resolved");
    let odoo = resolution
        .findings
        .iter()
        .find(|f| f.property == "quantity")
        .expect("odoo finding");
    assert_eq!(odoo.rule, SOURCE_AGREE_2);
    assert_eq!(odoo.report.system, "synthetic-odoo");
    assert!(
        odoo.report
            .lineage
            .as_ref()
            .is_some_and(|l| l.source.record.contains("SYN-ODOO-holding-north")),
        "{odoo:#?}"
    );
}

/// SOURCE-RESOLVE-1 with `ERPNext` the system of record and Odoo and Shopify
/// checks: a holding only checks have reported is not created from a guess,
/// its reports are kept, and the final value is the same in either order.
#[test]
fn source_resolve_1_checks_never_decide_and_the_system_of_record_does_in_either_order() {
    let directory = tempfile::tempdir().expect("directory");
    let (forward, backward, before) = both_orders(directory.path(), holding_system_of_record);
    assert_eq!(
        before.0, None,
        "withheld until the system of record reports"
    );
    assert_eq!(
        decision_of(&before.1, "quantity"),
        &PropertyDecision::Undecided {
            awaiting: vec!["erpnext.three.mapping".into()]
        }
    );
    let state = north_state(&forward);
    assert_eq!(north_state(&backward), state);
    assert_eq!(
        state.0.as_ref().and_then(|v| v.get("quantity")),
        Some(&grams(2765))
    );
    assert!(
        state
            .2
            .contains(&("quantity".into(), "odoo.three.mapping".into(), grams(2458),))
    );
}

/// Whether `record` is the north gold holding's source record.
fn is_north(record: &serde_json::Map<String, Value>) -> bool {
    record.contains_key("resource_code")
        && ["custom_norn_ref", "x_norn_ref", "metafield_norn_ref"]
            .iter()
            .any(|field| record.get(*field).and_then(Value::as_str) == Some(NORTH))
}

type Edit = fn(&mut serde_json::Map<String, Value>);

/// A stock answer's rows: totals and each member's amounts, by bucket.
type Answer = BTreeMap<String, (Vec<String>, BTreeMap<String, String>)>;

/// One sync of `system`'s snapshot as source version `event`, with `edit`
/// applied to the north gold holding's record: each row's outcome, flagged
/// when it is the holding's.
fn sync_north(
    cell: &mut Cell,
    system: &str,
    event: &str,
    edit: impl Fn(&mut serde_json::Map<String, Value>),
) -> Vec<(bool, MappingOutcome)> {
    let mut outcomes = Vec::new();
    for row in fixture(system)["snapshot"].as_array().expect("rows") {
        let mut row = row.clone();
        let record = row["record"].as_object_mut().expect("record");
        let north = is_north(record);
        if north {
            edit(record);
        }
        let id = retain_in(cell, system, &row, event, "snapshot");
        outcomes.push((north, cell.process_mapping_outcome(&id).expect("process")));
    }
    outcomes
}

/// The north holding row's outcome of a [`sync_north`].
fn north_outcome(outcomes: &[(bool, MappingOutcome)]) -> MappingOutcome {
    outcomes
        .iter()
        .find(|(north, _)| *north)
        .map(|(_, outcome)| outcome.clone())
        .expect("the north holding row")
}

fn quantity_of(cell: &Cell) -> Option<RuntimeValue> {
    cell.store()
        .read()
        .object(MAIN_WORLD, "canon.Holding", NORTH)
        .expect("read")
        .and_then(|record| record.values.get("quantity").cloned())
}

/// SOURCE-RESOLVE-1 (Sol S1): one source's versions decide by source
/// version, not by the order they were consumed in: `ERPNext` v2 (3000 g)
/// then v1 (2765 g) and v1 then v2 both leave v2. A historical read sees what
/// was consumed by its frontier and picks the highest version of it; a
/// reopened cell reads the same.
#[test]
fn source_resolve_1_a_sources_highest_version_decides_in_either_delivery_order() {
    let directory = tempfile::tempdir().expect("directory");
    let v1: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
    };
    let v2: Edit = |r| {
        r.insert("quantity".into(), json!(3000));
    };
    let late_path = directory.path().join("late.sqlite");
    let mut late = setup_with(&late_path, |_| {});
    sync_north(&mut late, "erpnext", "v2", v2);
    let after_v2 = late.store().read().head_commit_seq().expect("head");
    sync_north(&mut late, "erpnext", "v1", v1);
    let mut ordered = setup_with(&directory.path().join("ordered.sqlite"), |_| {});
    sync_north(&mut ordered, "erpnext", "v1", v1);
    let after_v1 = ordered.store().read().head_commit_seq().expect("head");
    sync_north(&mut ordered, "erpnext", "v2", v2);
    let decided = |grams_: u64| PropertyDecision::Decided {
        mapping: "erpnext.three.mapping".into(),
        value: grams(grams_),
    };
    for cell in [&late, &ordered] {
        assert_eq!(quantity_of(cell), Some(grams(3000)));
        assert_eq!(
            decision_of(&north_state(cell).1, "quantity"),
            &decided(3000)
        );
    }
    let at = |cell: &Cell, frontier: u64| {
        cell.store()
            .read()
            .source_resolution("canon.Holding", NORTH, Some(frontier))
            .expect("historical read")
            .expect("resolved")
            .decision("quantity")
            .cloned()
    };
    assert_eq!(at(&late, after_v2), Some(decided(3000)));
    assert_eq!(at(&ordered, after_v1), Some(decided(2765)));
    drop(late);
    let (_, model) = edited_set(|_| {});
    let store = CellStore::open(&late_path, model, StoreOptions::default()).expect("reopen");
    let reader = store.read();
    assert_eq!(
        reader
            .object(MAIN_WORLD, "canon.Holding", NORTH)
            .expect("read")
            .and_then(|record| record.values.get("quantity").cloned()),
        Some(grams(3000))
    );
    assert_eq!(
        reader
            .source_resolution("canon.Holding", NORTH, None)
            .expect("read")
            .expect("resolved")
            .decision("quantity"),
        Some(&decided(3000))
    );
}

/// SOURCE-RESOLVE-1 (Sol S2): `ERPNext` and Odoo name different owners of
/// the north holding. Neither bundle is refused: in either order the
/// quantity `ERPNext` decides is written, and the owner link is an explicit
/// conflict listing both sources' targets.
#[test]
fn source_resolve_1_sources_naming_different_owners_keep_the_conflict_and_the_decided_values() {
    let directory = tempfile::tempdir().expect("directory");
    let erpnext: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
        r.insert("owner_ref".into(), json!("demo/party/custodian"));
    };
    let mut forward = setup_with(&directory.path().join("forward.sqlite"), |_| {});
    let mut outcomes = sync_north(&mut forward, "erpnext", "snapshot", erpnext);
    outcomes.extend(sync_north(&mut forward, "odoo", "snapshot", odoo));
    let mut backward = setup_with(&directory.path().join("backward.sqlite"), |_| {});
    let mut reverse = sync_north(&mut backward, "odoo", "snapshot", odoo);
    reverse.extend(sync_north(&mut backward, "erpnext", "snapshot", erpnext));
    for (cell, outcomes) in [(&forward, outcomes), (&backward, reverse)] {
        assert!(
            outcomes.iter().all(|(_, o)| *o == MappingOutcome::Landed),
            "{outcomes:#?}"
        );
        assert_eq!(quantity_of(cell), Some(grams(2765)));
        let resolution = cell
            .store()
            .read()
            .source_resolution("canon.Holding", NORTH, None)
            .expect("read")
            .expect("resolved");
        let links: Vec<_> = resolution
            .links
            .iter()
            .map(|conflict| {
                (
                    conflict.link.clone(),
                    conflict
                        .claims
                        .iter()
                        .map(|(mapping, target, _)| (mapping.clone(), target.clone()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            links,
            [(
                "owner".to_owned(),
                vec![
                    (
                        "erpnext.three.mapping".to_owned(),
                        Some("demo/party/owner".to_owned())
                    ),
                    (
                        "odoo.three.mapping".to_owned(),
                        Some("demo/party/custodian".to_owned())
                    ),
                ]
            )]
        );
    }
}

/// SOURCE-RESOLVE-1 (Sol S3): Odoo reports north gold at 2458 g on
/// 2026-10-20 and `ERPNext` at 2765 g on 2026-11-10. The weekly stock answer
/// is the same in either sync order: Odoo's report until `ERPNext`'s valid
/// time, `ERPNext`'s (it outranks Odoo) from then on.
#[test]
fn source_resolve_1_a_stock_answer_resolves_each_valid_time_in_either_sync_order() {
    let directory = tempfile::tempdir().expect("directory");
    let erpnext: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
        r.insert("modified".into(), json!("2026-11-10T00:00:00Z"));
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
        r.insert("write_date".into(), json!("2026-10-20T00:00:00Z"));
    };
    let mut forward = setup_with(&directory.path().join("forward.sqlite"), |_| {});
    sync_north(&mut forward, "erpnext", "snapshot", erpnext);
    sync_north(&mut forward, "odoo", "snapshot", odoo);
    let mut backward = setup_with(&directory.path().join("backward.sqlite"), |_| {});
    sync_north(&mut backward, "odoo", "snapshot", odoo);
    sync_north(&mut backward, "erpnext", "snapshot", erpnext);
    let answer = |cell: &Cell| -> Answer {
        cell.store()
            .evaluate_question(&stock_plan(), context(), |_| Ok(true))
            .expect("answer")
            .rows
            .into_iter()
            .map(|(key, row)| {
                (
                    key,
                    (
                        row.totals.iter().map(|t| format!("{t:?}")).collect(),
                        row.members
                            .iter()
                            .map(|(id, m)| (id.clone(), format!("{:?}", m.amounts)))
                            .collect(),
                    ),
                )
            })
            .collect()
    };
    let rows = answer(&forward);
    assert_eq!(answer(&backward), rows);
    let north_in = |week: &str| {
        rows.iter()
            .filter(|(key, _)| key.starts_with(&format!("[\"{week}\"")))
            .find_map(|(_, (_, members))| members.get(NORTH))
            .cloned()
    };
    let (odoo_week, erpnext_week) = (north_in("2026-10-26"), north_in("2026-11-16"));
    assert!(odoo_week.is_some(), "{rows:#?}");
    assert!(erpnext_week.is_some(), "{rows:#?}");
    assert_ne!(odoo_week, erpnext_week);
}

/// SOURCE-RESOLVE-1 (Sol S4): with `ERPNext` the holding's system of record,
/// a check's report on a holding `ERPNext` has not reported is withheld as
/// an explicit outcome naming `ERPNext`, counted as held back; once
/// `ERPNext` reports, the holding lands, and either order ends the same.
#[test]
fn source_resolve_1_a_withheld_holding_is_an_explicit_outcome_in_either_order() {
    let directory = tempfile::tempdir().expect("directory");
    let mut backward = setup_with(
        &directory.path().join("backward.sqlite"),
        holding_system_of_record,
    );
    for system in ["shopify", "odoo"] {
        let outcome = north_outcome(&sync_north(&mut backward, system, "snapshot", |_| {}));
        let MappingOutcome::Withheld(subjects) = &outcome else {
            panic!("{system}: {outcome:?}")
        };
        let north = subjects
            .iter()
            .find(|s| s.key == NORTH)
            .expect("the north holding");
        assert!(
            north
                .awaiting
                .iter()
                .any(|(p, by)| p == "quantity" && by == &["erpnext.three.mapping".to_owned()]),
            "{north:?}"
        );
        assert_eq!(
            outcome.held_back(),
            Some(vec![norn_cell::WITHHELD_REASON.to_owned()])
        );
        assert_eq!(quantity_of(&backward), None);
    }
    assert_eq!(
        north_outcome(&sync_north(&mut backward, "erpnext", "snapshot", |_| {})),
        MappingOutcome::Landed
    );
    let mut forward = setup_with(
        &directory.path().join("forward.sqlite"),
        holding_system_of_record,
    );
    for system in ["erpnext", "odoo", "shopify"] {
        assert_eq!(
            north_outcome(&sync_north(&mut forward, system, "snapshot", |_| {})),
            MappingOutcome::Landed,
            "{system}"
        );
    }
    assert_eq!(north_state(&forward), north_state(&backward));
}

/// `system`'s snapshot rows that `pick` selects, synced as source version
/// `event` with `edit` applied: each row's outcome.
fn sync_rows(
    cell: &mut Cell,
    system: &str,
    event: &str,
    pick: impl Fn(&serde_json::Map<String, Value>) -> bool,
    edit: impl Fn(&mut serde_json::Map<String, Value>),
) -> Vec<MappingOutcome> {
    let mut outcomes = Vec::new();
    for row in fixture(system)["snapshot"].as_array().expect("rows") {
        let mut row = row.clone();
        let record = row["record"].as_object_mut().expect("record");
        if !pick(record) {
            continue;
        }
        edit(record);
        let id = retain_in(cell, system, &row, event, "snapshot");
        outcomes.push(cell.process_mapping_outcome(&id).expect("process"));
    }
    outcomes
}

/// The holding's writers ranked Odoo, `ERPNext`, Shopify.
#[allow(clippy::ptr_arg, reason = "the shape setup_with edits")]
fn odoo_first(artifacts: &mut Vec<ArtifactEnvelope>) {
    for (system, rank) in [("odoo", 0), ("erpnext", 1), ("shopify", 2)] {
        let artifact = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == format!("{system}.three.mapping"))
            .expect("mapping");
        let mut wire = serde_json::to_value(&*artifact).expect("wire");
        for map in wire["payload"]["maps"].as_array_mut().expect("maps") {
            if map["id"] == "holding" {
                map["authority"] = json!({"kind": "precedence", "rank": rank});
            }
        }
        *artifact = serde_json::from_value(wire).expect("edited mapping");
    }
}

fn decision_at(cell: &Cell, frontier: Option<u64>) -> PropertyDecision {
    cell.store()
        .read()
        .source_resolution("canon.Holding", NORTH, frontier)
        .expect("read")
        .expect("resolved")
        .decision("quantity")
        .cloned()
        .expect("quantity is reported")
}

/// SOURCE-RESOLVE-1 (Sol S7): activating definitions that rank Odoo over
/// `ERPNext` reconciles the north holding at once (the stored value is the
/// one the current read decides), while a read at a frontier before the
/// activation still decides under the definitions in force then.
#[test]
fn source_resolve_1_an_activated_authority_reconciles_and_history_keeps_the_old_one() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("activate.sqlite"), |_| {});
    let erpnext: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
    };
    sync_north(&mut cell, "erpnext", "snapshot", erpnext);
    sync_north(&mut cell, "odoo", "snapshot", odoo);
    let before = cell.store().read().head_commit_seq().expect("head");
    let active = cell
        .store()
        .read()
        .installed_definitions(None)
        .expect("read")
        .expect("active")
        .version;
    let (artifacts, model) = edited_set(odoo_first);
    cell.install_definitions(
        model,
        definition_input(&artifacts, 1_793_702_500_000),
        Some(active),
    )
    .expect("activate");
    assert!(cell.reconcile_source_resolution().expect("reconcile") > 0);
    let odoo_decides = PropertyDecision::Decided {
        mapping: "odoo.three.mapping".into(),
        value: grams(2458),
    };
    assert_eq!(decision_at(&cell, None), odoo_decides);
    assert_eq!(quantity_of(&cell), Some(grams(2458)));
    assert_eq!(
        decision_at(&cell, Some(before)),
        PropertyDecision::Decided {
            mapping: "erpnext.three.mapping".into(),
            value: grams(2765),
        }
    );
    // Reconciled: a second pass writes nothing.
    assert_eq!(cell.reconcile_source_resolution().expect("again"), 0);
}

/// SOURCE-RESOLVE-1 (Sol S8): `ERPNext` and Odoo report north gold (2765 g
/// and 2458 g; `ERPNext` decides). A newer `ERPNext` upsert of the same
/// source record that moves it to another key, or that no longer matches
/// the holding map, withdraws its contribution: north is decided again by
/// Odoo's report.
#[test]
fn source_resolve_1_a_record_that_moves_or_stops_matching_withdraws_its_contribution() {
    let directory = tempfile::tempdir().expect("directory");
    let erpnext: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
    };
    let moved = format!("{NORTH}/moved");
    let only_when_stocked = |artifacts: &mut Vec<ArtifactEnvelope>| {
        let artifact = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == "erpnext.three.mapping")
            .expect("mapping");
        let mut wire = serde_json::to_value(&*artifact).expect("wire");
        for map in wire["payload"]["maps"].as_array_mut().expect("maps") {
            if map["id"] == "holding" {
                map["when"] = json!("input.quantity > 0");
            }
        }
        *artifact = serde_json::from_value(wire).expect("edited mapping");
    };
    for (case, stop) in [("move", false), ("stop", true)] {
        let mut cell = if stop {
            setup_with(&directory.path().join("stop.sqlite"), only_when_stocked)
        } else {
            setup_with(&directory.path().join("move.sqlite"), |_| {})
        };
        sync_north(&mut cell, "erpnext", "snapshot", erpnext);
        sync_north(&mut cell, "odoo", "snapshot", odoo);
        assert_eq!(quantity_of(&cell), Some(grams(2765)), "{case}");
        let moved_ref = moved.clone();
        let outcomes = sync_rows(&mut cell, "erpnext", "v2", is_north, move |r| {
            if stop {
                r.insert("quantity".into(), json!(0));
            } else {
                r.insert("custom_norn_ref".into(), json!(moved_ref.clone()));
                r.insert("quantity".into(), json!(2765));
            }
        });
        assert_eq!(outcomes, [MappingOutcome::Landed], "{case}");
        assert_eq!(quantity_of(&cell), Some(grams(2458)), "{case}");
        assert_eq!(
            decision_at(&cell, None),
            PropertyDecision::Decided {
                mapping: "odoo.three.mapping".into(),
                value: grams(2458),
            },
            "{case}"
        );
        let moved_quantity = cell
            .store()
            .read()
            .object(MAIN_WORLD, "canon.Holding", &moved)
            .expect("read")
            .and_then(|record| record.values.get("quantity").cloned());
        assert_eq!(
            moved_quantity,
            (!stop).then(|| grams(2765)),
            "{case}: the record's new subject"
        );
    }
}

/// SOURCE-RESOLVE-1 (Sol S9): 4,100 versions of `ERPNext`'s gold item (one
/// source) never exhaust the resolution bound: only each source record's
/// current version is read, so the last version still writes and reads.
#[test]
fn source_resolve_1_accumulated_versions_never_make_a_subject_unwritable() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("versions.sqlite"), |_| {});
    let item = |r: &serde_json::Map<String, Value>| {
        r.get("custom_norn_ref").and_then(Value::as_str) == Some("demo/item/gold")
    };
    for version in 1..=4100_u32 {
        let label = format!("Gold v{version:04}");
        let outcomes = sync_rows(&mut cell, "erpnext", &format!("v{version:04}"), item, |r| {
            r.insert("label".into(), json!(label.clone()));
        });
        assert_eq!(outcomes, [MappingOutcome::Landed], "version {version}");
    }
    let reader = cell.store().read();
    assert_eq!(
        reader
            .object(MAIN_WORLD, "canon.Item", "demo/item/gold")
            .expect("read")
            .and_then(|record| record.values.get("label").cloned()),
        Some(RuntimeValue::from("Gold v4100"))
    );
    let resolution = reader
        .source_resolution("canon.Item", "demo/item/gold", None)
        .expect("the read stays within its bound")
        .expect("resolved");
    assert_eq!(
        resolution.decision("label"),
        Some(&PropertyDecision::Decided {
            mapping: "erpnext.three.mapping".into(),
            value: RuntimeValue::from("Gold v4100"),
        })
    );
}

/// The north holding's amount in each week of a stock answer.
fn north_by_week(result: norn_cell::compute::QueryResult) -> BTreeMap<String, String> {
    result
        .rows
        .into_iter()
        .filter_map(|(key, row)| {
            let week = serde_json::from_str::<Value>(&key)
                .ok()
                .and_then(|k| k.get(0).and_then(Value::as_str).map(str::to_owned))?;
            row.members
                .get(NORTH)
                .map(|member| (week, format!("{:?}", member.amounts)))
        })
        .collect()
}

/// The stock answer at the cell's current frontier, as a reader then knew it.
fn stock_known_now(cell: &Cell) -> (norn_cell::KnowledgeCutoff, BTreeMap<String, String>) {
    let cutoff = norn_cell::KnowledgeCutoff {
        recorded_at: norn_kernel::LogicalTime::from_millis(context().now_ms),
        frontier: cell.store().verified_head().expect("head").commits,
    };
    (cutoff, stock_at(cell, cutoff))
}

fn stock_at(cell: &Cell, cutoff: norn_cell::KnowledgeCutoff) -> BTreeMap<String, String> {
    north_by_week(
        cell.store()
            .evaluate_question_known_at(&stock_plan(), context(), cutoff, |_| Ok(true))
            .expect("historical answer")
            .result,
    )
}

fn stock_live(cell: &Cell) -> BTreeMap<String, String> {
    north_by_week(
        cell.store()
            .evaluate_question(&stock_plan(), context(), |_| Ok(true))
            .expect("answer"),
    )
}

/// SOURCE-RESOLVE-1 (Sol S3): Odoo reports north gold at 2458 g on
/// 2026-10-20 and `ERPNext` at 2765 g on 2026-11-03; on 2026-11-12 a newer
/// `ERPNext` upsert moves that record to another holding. The live value
/// falls back to Odoo, and so does the stock answer from the move's valid
/// time on, in either delivery order; a reader who knew only what came
/// before the move still sees `ERPNext`'s report there.
#[test]
fn source_resolve_1_a_moved_record_withdraws_from_the_stock_answer_at_its_valid_time() {
    let directory = tempfile::tempdir().expect("directory");
    let moved = format!("{NORTH}/moved");
    let erpnext: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
        r.insert("modified".into(), json!("2026-11-03T00:00:00Z"));
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
        r.insert("write_date".into(), json!("2026-10-20T00:00:00Z"));
    };
    let mut answers = Vec::new();
    for late in [false, true] {
        let mut cell = setup_with(
            &directory.path().join(format!("moved-{late}.sqlite")),
            |_| {},
        );
        let moved_ref = moved.clone();
        let move_it = |cell: &mut Cell| {
            let moved_ref = moved_ref.clone();
            sync_rows(cell, "erpnext", "v2", is_north, move |r| {
                erpnext(r);
                r.insert("custom_norn_ref".into(), json!(moved_ref.clone()));
                r.insert("modified".into(), json!("2026-11-12T00:00:00Z"));
            });
        };
        let before_move = if late {
            sync_north(&mut cell, "odoo", "snapshot", odoo);
            move_it(&mut cell);
            sync_north(&mut cell, "erpnext", "snapshot", erpnext);
            None
        } else {
            sync_north(&mut cell, "erpnext", "snapshot", erpnext);
            sync_north(&mut cell, "odoo", "snapshot", odoo);
            let known = stock_known_now(&cell);
            move_it(&mut cell);
            Some(known)
        };
        assert_eq!(quantity_of(&cell), Some(grams(2458)), "late {late}");
        let live = stock_live(&cell);
        let (_, known) = stock_known_now(&cell);
        assert_eq!(known, live, "late {late}: the snapshot is the live answer");
        let (odoo_week, erpnext_week, moved_week) =
            (&live["2026-10-26"], &live["2026-11-02"], &live["2026-11-16"]);
        assert_ne!(odoo_week, erpnext_week, "late {late}: {live:#?}");
        assert_eq!(moved_week, odoo_week, "late {late}: Odoo after the move");
        if let Some((cutoff, before)) = before_move {
            assert_eq!(&before["2026-11-16"], erpnext_week, "{before:#?}");
            assert_eq!(stock_at(&cell, cutoff), before, "history keeps it");
        }
        answers.push(live);
    }
    assert_eq!(answers[0], answers[1]);
}

/// SOURCE-RESOLVE-1 (Sol S7a): the stock answer at a frontier before an
/// activation that ranks Odoo over `ERPNext` decides under the definitions
/// then in force, as the resolution read at that frontier does.
#[test]
fn source_resolve_1_a_stock_answer_decides_under_its_frontiers_definitions() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("frontier.sqlite"), |_| {});
    let erpnext: Edit = |r| {
        r.insert("quantity".into(), json!(2765));
        r.insert("modified".into(), json!("2026-11-03T00:00:00Z"));
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
        r.insert("write_date".into(), json!("2026-10-20T00:00:00Z"));
    };
    sync_north(&mut cell, "erpnext", "snapshot", erpnext);
    sync_north(&mut cell, "odoo", "snapshot", odoo);
    let (cutoff, before) = stock_known_now(&cell);
    let active = cell
        .store()
        .read()
        .installed_definitions(None)
        .expect("read")
        .expect("active")
        .version;
    let (artifacts, model) = edited_set(odoo_first);
    cell.install_definitions(
        model,
        definition_input(&artifacts, 1_793_702_500_000),
        Some(active),
    )
    .expect("activate");
    cell.reconcile_source_resolution().expect("reconcile");
    let after = stock_live(&cell);
    assert_eq!(after["2026-11-16"], after["2026-10-26"], "Odoo decides now");
    assert_ne!(before["2026-11-16"], before["2026-10-26"], "ERPNext decided");
    assert_eq!(stock_at(&cell, cutoff), before, "history keeps ERPNext");
}

/// SOURCE-RESOLVE-1 (Sol S4): with `ERPNext` the holding's system of record,
/// a check's withheld row delivered again is still withheld (never already
/// landed) while `ERPNext` has not reported, and already landed once it has;
/// in the other order it lands and its redelivery is already landed.
#[test]
fn source_resolve_1_a_redelivered_withheld_row_stays_withheld_in_either_order() {
    let directory = tempfile::tempdir().expect("directory");
    let north_ids = |cell: &mut Cell, system: &str| -> Vec<String> {
        fixture(system)["snapshot"]
            .as_array()
            .expect("rows")
            .iter()
            .filter(|row| is_north(row["record"].as_object().expect("record")))
            .map(|row| retain_in(cell, system, row, "snapshot", "snapshot"))
            .collect()
    };
    let mut backward = setup_with(
        &directory.path().join("backward.sqlite"),
        holding_system_of_record,
    );
    let ids = north_ids(&mut backward, "odoo");
    for id in &ids {
        assert!(matches!(
            backward.process_mapping_outcome(id).expect("first"),
            MappingOutcome::Withheld(_)
        ));
        let again = backward.process_mapping_outcome(id).expect("redelivered");
        assert!(matches!(again, MappingOutcome::Withheld(_)), "{again:?}");
        assert_eq!(
            again.held_back(),
            Some(vec![norn_cell::WITHHELD_REASON.to_owned()])
        );
    }
    sync_north(&mut backward, "erpnext", "snapshot", |_| {});
    for id in &ids {
        assert_eq!(
            backward.process_mapping_outcome(id).expect("after ERPNext"),
            MappingOutcome::AlreadyLanded
        );
    }
    let mut forward = setup_with(
        &directory.path().join("forward.sqlite"),
        holding_system_of_record,
    );
    sync_north(&mut forward, "erpnext", "snapshot", |_| {});
    for id in north_ids(&mut forward, "odoo") {
        assert_eq!(
            forward.process_mapping_outcome(&id).expect("first"),
            MappingOutcome::Landed
        );
        assert_eq!(
            forward.process_mapping_outcome(&id).expect("redelivered"),
            MappingOutcome::AlreadyLanded
        );
    }
    assert_eq!(north_state(&forward), north_state(&backward));
}

/// SOURCE-RESOLVE-1 (Sol S8): `ERPNext` v1 reports north gold (2765 g), v2
/// moves that source record to another holding (3000 g), and Odoo reports
/// north (2458 g). Delivered v1 before or after v2, north ends at Odoo's
/// value and the moved holding at v2's; a late v1 never writes north. Alone,
/// a late v1 never creates north at all.
#[test]
fn source_resolve_1_a_late_older_version_never_writes_its_stale_key() {
    let directory = tempfile::tempdir().expect("directory");
    let moved = format!("{NORTH}/moved");
    let v1 = |cell: &mut Cell| {
        sync_rows(cell, "erpnext", "v1", is_north, |r| {
            r.insert("quantity".into(), json!(2765));
        })
    };
    let moved_ref = moved.clone();
    let v2 = move |cell: &mut Cell| {
        let moved_ref = moved_ref.clone();
        sync_rows(cell, "erpnext", "v2", is_north, move |r| {
            r.insert("custom_norn_ref".into(), json!(moved_ref.clone()));
            r.insert("quantity".into(), json!(3000));
        })
    };
    let odoo: Edit = |r| {
        r.insert("quantity".into(), json!(2458));
    };
    let holding = |cell: &Cell, key: &str| {
        cell.store()
            .read()
            .object(MAIN_WORLD, "canon.Holding", key)
            .expect("read")
            .and_then(|record| record.values.get("quantity").cloned())
    };
    let mut states = Vec::new();
    for late in [false, true] {
        let mut cell = setup_with(&directory.path().join(format!("late-{late}.sqlite")), |_| {});
        sync_north(&mut cell, "odoo", "snapshot", odoo);
        if late {
            v2(&mut cell);
            v1(&mut cell);
        } else {
            v1(&mut cell);
            v2(&mut cell);
        }
        assert_eq!(holding(&cell, NORTH), Some(grams(2458)), "late {late}");
        assert_eq!(holding(&cell, &moved), Some(grams(3000)), "late {late}");
        let state = north_state(&cell);
        assert!(
            state.1.iter().all(|(_, _, reports)| reports
                .iter()
                .all(|(mapping, _)| mapping != "erpnext.three.mapping")),
            "late {late}: {state:#?}"
        );
        states.push(state);
    }
    assert_eq!(states[0], states[1]);
    let mut alone = setup_with(&directory.path().join("alone.sqlite"), |_| {});
    v2(&mut alone);
    v1(&mut alone);
    assert_eq!(holding(&alone, NORTH), None, "a late v1 creates nothing");
    assert_eq!(holding(&alone, &moved), Some(grams(3000)));
}
