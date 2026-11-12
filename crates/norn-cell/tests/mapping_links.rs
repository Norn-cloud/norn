#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    missing_docs,
    reason = "synthetic mapped-link correspondence"
)]
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, Model, SqliteCell, StoreOptions};
use norn_ir::{
    ArtifactEnvelope, InterfaceDef, ObjectTypeDef, PropertyTypeDef, RuntimeObject, RuntimeValue,
};
use norn_kernel::{FactKey, LinkKey, ObservationEvent, ObservationId, Observed};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

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
fn graph(cell: &Cell) -> BTreeSet<(String, String, String, String, String)> {
    let claims = cell.store().read().observed_link_claims().expect("claims");
    let mut graph = BTreeSet::new();
    for claim in claims {
        for link in cell
            .store()
            .read()
            .links_touching(MAIN_WORLD, claim.source_type.as_str(), &claim.source_key)
            .expect("links")
        {
            graph.insert((
                link.source_type,
                link.source,
                link.link,
                link.target_type,
                link.target,
            ));
        }
    }
    graph
}
fn count_snapshot(cell: &Cell) -> (usize, usize) {
    let mut captured = 0usize;
    let mut quarantined = 0usize;
    for ty in [
        "canon.Item",
        "canon.Party",
        "canon.PartyRole",
        "canon.Holding",
        "canon.OrderLine",
    ] {
        let page = cell
            .store()
            .read()
            .authoritative_page(MAIN_WORLD, ty, None, 100)
            .expect("page");
        captured = captured
            .checked_add(page.records.len())
            .expect("record count");
        quarantined = quarantined
            .checked_add(page.quarantine_count)
            .expect("quarantine count");
    }
    (captured, quarantined)
}
#[test]
fn obs_link_all_three_snapshots_capture_and_converge_with_source_provenance() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("three.sqlite"));
    for system in ["erpnext", "shopify", "odoo"] {
        let source = fixture(system);
        let rows = source["snapshot"].as_array().expect("rows");
        for row in rows {
            capture(&mut cell, system, row, "snapshot");
        }
        assert_eq!(count_snapshot(&cell), (10, 0));
        assert_eq!(graph(&cell).len(), 12);
        println!(
            "{system}: captured {}/{} snapshot source rows; 0 quarantined; 10 canonical objects; 12 links",
            rows.len(),
            rows.len()
        );
    }
    let key = LinkKey::new(
        FactKey::new("canon.OrderLine", "demo/order/1/line/1"),
        "provider",
        FactKey::new("canon.Party", "demo/party/owner"),
    );
    let claims = cell
        .store()
        .read()
        .observed_link_provenance(&key)
        .expect("provenance");
    assert_eq!(claims.len(), 3);
    assert_eq!(
        claims
            .iter()
            .map(|c| c.provenance.source.system.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        cell.store()
            .read()
            .observed_link_claims()
            .expect("claims")
            .len(),
        36
    );
    assert_eq!(
        cell.store()
            .read()
            .list_after(
                MAIN_WORLD,
                norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
                None,
                100
            )
            .expect("materialized claims")
            .len(),
        36
    );
}
#[test]
fn obs_link_pending_targets_resolve_without_source_redelivery_and_reopen_is_identical() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("pending.sqlite");
    let mut cell = setup(&path);
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    capture(&mut cell, "erpnext", &rows[6], "snapshot");
    assert!(graph(&cell).is_empty());
    assert_eq!(
        cell.store()
            .read()
            .observed_link_claims()
            .expect("pending claims")
            .len(),
        3
    );
    for row in rows[..6].iter().rev() {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    assert_eq!(graph(&cell).len(), 12);
    let graph_before = graph(&cell);
    let claims_before = cell.store().read().observed_link_claims().expect("claims");
    let dump = cell.store().read().dump().expect("dump");
    let model = cell.store().model().clone();
    drop(cell);
    let store = CellStore::open(&path, model, StoreOptions::default()).expect("reopen");
    assert_eq!(store.read().dump().expect("dump"), dump);
    let mut cell = SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    assert_eq!(graph(&cell), graph_before);
    assert_eq!(
        cell.store().read().observed_link_claims().expect("claims"),
        claims_before
    );
    let records: Vec<_> = (1..=cell.store().export_commit_count().expect("head"))
        .map(|seq| {
            cell.store()
                .export_record(seq)
                .expect("record")
                .expect("present")
        })
        .collect();
    let restored = CellStore::rebuild_export_history(
        norn_cell::engine::NativeEngine::open_in_memory().expect("engine"),
        &records,
        &records.last().expect("last").hash().expect("hash"),
    )
    .expect("replay");
    let replayed =
        SqliteCell::new(restored, "org_demo/dev", |_| BTreeMap::new()).expect("replayed cell");
    assert_eq!(replayed.store().read().dump().expect("replayed dump"), dump);
    assert_eq!(graph(&replayed), graph_before);
    assert_eq!(
        replayed
            .store()
            .read()
            .observed_link_claims()
            .expect("replayed claims"),
        claims_before
    );
    let before = cell.store().export_commit_count().expect("head");
    for row in rows {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    assert_eq!(cell.store().export_commit_count().expect("head"), before);
    assert_eq!(graph(&cell), graph_before);
}
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(12))]
    #[test]
    fn obs_link_pending_order_and_redelivery_independence(priorities in proptest::collection::vec(0u8..=255,7), repeat in 1usize..4) {
        let directory = tempfile::tempdir().expect("directory"); let mut cell = setup(&directory.path().join("generated.sqlite"));
        let source=fixture("erpnext"); let rows=source["snapshot"].as_array().expect("rows");
        let mut order:Vec<_>=(0..7).collect();order.sort_by_key(|i|(priorities[*i],*i));
        for i in order { for _ in 0..repeat { capture(&mut cell,"erpnext",&rows[i],"snapshot"); } }
        proptest::prop_assert_eq!(count_snapshot(&cell),(10,0));
        let expected:BTreeSet<_>=cell.store().read().observed_link_claims().expect("claims").into_iter().filter_map(|c| c.target.map(|t|(c.source_type.0,c.source_key,c.role.0,t.object_type.0,t.key))).collect();
        proptest::prop_assert_eq!(graph(&cell),expected);
        proptest::prop_assert_eq!(cell.store().read().observed_link_claims().expect("claims").len(),12);
    }
}

#[test]
fn obs_link_delivery_incremental_reports_converge_without_duplicate_contributions() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("deliveries.sqlite"));
    for system in ["erpnext", "shopify", "odoo"] {
        let source = fixture(system);
        for row in source["snapshot"].as_array().expect("snapshot") {
            capture(&mut cell, system, row, "snapshot");
        }
        for row in source["incremental"].as_array().expect("incremental") {
            capture(&mut cell, system, row, &row["sequence"].to_string());
        }
    }
    let key = LinkKey::new(
        FactKey::new("canon.Delivery", "demo/delivery/1/line/1"),
        "fulfils",
        FactKey::new("canon.OrderLine", "demo/order/1/line/1"),
    );
    assert_eq!(
        cell.store()
            .read()
            .observed_link_provenance(&key)
            .expect("claims")
            .len(),
        3
    );
    let delivery = cell
        .store()
        .read()
        .authoritative_object(MAIN_WORLD, "canon.Delivery", "demo/delivery/1/line/1")
        .expect("read")
        .expect("delivery");
    assert_eq!(delivery.version, 1);
    assert_eq!(
        cell.store()
            .read()
            .list_after(MAIN_WORLD, "canon.Delivery", None, 100)
            .expect("deliveries")
            .len(),
        1
    );
}

fn edit_artifact(artifacts: &mut [ArtifactEnvelope], id: &str, edit: impl FnOnce(&mut Value)) {
    let artifact = artifacts
        .iter_mut()
        .find(|a| a.artifact_id == id)
        .expect("artifact");
    let mut wire = serde_json::to_value(&*artifact).expect("wire");
    edit(&mut wire);
    *artifact = serde_json::from_value(wire).expect("edited fixture declaration");
}

/// A separate to-many relation keeps the production owner contract unchanged.
fn many_holding(artifacts: &mut [ArtifactEnvelope]) {
    edit_artifact(artifacts, "canon.Holding.entity", |a| {
        a["payload"]["objectType"]["links"]
            .as_array_mut()
            .expect("links")
            .push(json!({
                "id":"peers", "target":"canon.Party", "cardinality":{"to":"many","from":"many"}
            }));
    });
    for system in ["erpnext", "odoo", "shopify"] {
        edit_artifact(artifacts, &format!("{system}.three.mapping"), |a| {
            let map = a["payload"]["maps"]
                .as_array_mut()
                .expect("maps")
                .iter_mut()
                .find(|m| m["id"] == "holding")
                .expect("holding");
            let link = map["links"]
                .as_array_mut()
                .expect("links")
                .iter_mut()
                .find(|l| l["link"] == "owner")
                .expect("owner");
            link["link"] = json!("peers");
            link["key"] = json!("input.owner_ref == 'clear' ? null : input.owner_ref");
        });
    }
}

#[test]
fn obs_link_retarget_and_redelivery_replace_only_the_latest_contribution() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("retarget.sqlite"));
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let mut changed = rows[6].clone();
    changed["record"]["provider_ref"] = json!("demo/party/custodian");
    capture(&mut cell, "erpnext", &changed, "2");
    let reader = cell.store().read();
    assert!(
        reader
            .link(
                MAIN_WORLD,
                "canon.OrderLine",
                "provider",
                "demo/order/1/line/1",
                "demo/party/owner"
            )
            .expect("old")
            .is_none()
    );
    assert!(
        reader
            .link(
                MAIN_WORLD,
                "canon.OrderLine",
                "provider",
                "demo/order/1/line/1",
                "demo/party/custodian"
            )
            .expect("new")
            .is_some()
    );
    assert_eq!(graph(&cell).len(), 12);
    capture(&mut cell, "erpnext", &rows[6], "3");
    let before = cell.store().export_commit_count().expect("head");
    capture(&mut cell, "erpnext", &changed, "2");
    assert_eq!(cell.store().export_commit_count().expect("head"), before);
    assert!(
        cell.store()
            .read()
            .link(
                MAIN_WORLD,
                "canon.OrderLine",
                "provider",
                "demo/order/1/line/1",
                "demo/party/owner"
            )
            .expect("restored")
            .is_some()
    );
}

#[test]
fn obs_link_withdrawal_preserves_unbacked_authored_edges() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("authored.sqlite"), |artifacts| {
        many_holding(artifacts);
    });
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let key = LinkKey::new(
        FactKey::new("canon.Holding", "demo/holding/owner/north/gold"),
        "peers",
        FactKey::new("canon.Party", "demo/party/custodian"),
    );
    norn_kernel::CellHost::commit(
        &mut cell,
        norn_kernel::CellCommit {
            links: vec![norn_kernel::LinkWrite {
                key,
                expected: norn_kernel::FactVersion(0),
                mutation: norn_kernel::LinkMutation::Add {
                    values: RuntimeObject::new(),
                },
            }],
            ..norn_kernel::CellCommit::empty()
        },
    )
    .expect("authored edge");
    let mut changed = rows[4].clone();
    changed["record"]["owner_ref"] = json!("demo/party/customer");
    capture(&mut cell, "erpnext", &changed, "2");
    let reader = cell.store().read();
    let peers = reader
        .links(
            MAIN_WORLD,
            "canon.Holding",
            "peers",
            "demo/holding/owner/north/gold",
        )
        .expect("peers");
    assert_eq!(
        peers.into_iter().map(|l| l.target).collect::<BTreeSet<_>>(),
        BTreeSet::from(["demo/party/customer".into(), "demo/party/custodian".into()])
    );
}

#[test]
fn obs_link_mixed_clear_and_assert_preserve_other_contributors_without_oscillation() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("clear.sqlite"), |a| many_holding(a));
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let second = fixture("odoo");
    capture(&mut cell, "odoo", &second["snapshot"][4], "snapshot");
    let expected = graph(&cell);
    let mut changed = rows[4].clone();
    changed["record"]["owner_ref"] = json!("clear");
    for event in ["2", "3", "4"] {
        capture(&mut cell, "erpnext", &changed, event);
        assert_eq!(graph(&cell), expected);
    }
}

#[test]
fn obs_link_partitions_with_identical_row_aliases_are_independent_contributors() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("connectors.sqlite"));
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let id = retain_in(&mut cell, "erpnext", &rows[6], "snapshot", "second");
    cell.process_mapping_bundle(&id).expect("second partition");
    let claims: Vec<_> = cell
        .store()
        .read()
        .observed_link_claims()
        .expect("claims")
        .into_iter()
        .filter(|c| c.source_key == "demo/order/1/line/1" && c.role.as_str() == "provider")
        .collect();
    assert_eq!(claims.len(), 2);
    assert_ne!(claims[0].source_scope, claims[1].source_scope);
    let mut changed = rows[6].clone();
    changed["record"]["provider_ref"] = json!("demo/party/custodian");
    let id = retain_in(&mut cell, "erpnext", &changed, "2", "snapshot");
    assert!(matches!(
        cell.process_mapping_bundle(&id),
        Err(norn_cell::CellError::UnsupportedMappingProfile)
    ));
    assert!(
        cell.store()
            .read()
            .link(
                MAIN_WORLD,
                "canon.OrderLine",
                "provider",
                "demo/order/1/line/1",
                "demo/party/owner"
            )
            .expect("old")
            .is_some()
    );
}

#[test]
fn obs_link_pending_subtype_claim_uses_inherited_link_defaults() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup_with(&directory.path().join("subtype.sqlite"), |artifacts| {
        edit_artifact(artifacts, "canon.Holding.entity", |a| {
            let owner = a["payload"]["objectType"]["links"]
                .as_array_mut()
                .expect("links")
                .iter_mut()
                .find(|l| l["id"] == "owner")
                .expect("owner");
            owner["properties"] =
                json!([{"id":"marker","type":{"kind":"text"},"default":"special"}]);
        });
        holding_subtype(artifacts, false);
        edit_artifact(artifacts, "erpnext.three.mapping", |a| {
            a["dependencies"]
                .as_array_mut()
                .expect("dependencies")
                .push(json!({"artifactId":"canon.SpecialHolding.entity","kind":"entity","required":true,"version":"0.1.0"}));
            for map in a["payload"]["maps"].as_array_mut().expect("maps") {
                if map["target"]["entity"] == "canon.Holding.entity" {
                    map["target"]["entity"] = json!("canon.SpecialHolding.entity");
                }
            }
        });
    });
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    capture(&mut cell, "erpnext", &rows[3], "snapshot");
    capture(&mut cell, "erpnext", &rows[4], "snapshot");
    capture(&mut cell, "erpnext", &rows[0], "snapshot");
    let link = cell
        .store()
        .read()
        .link(
            MAIN_WORLD,
            "canon.SpecialHolding",
            "owner",
            "demo/holding/owner/north/gold",
            "demo/party/owner",
        )
        .expect("read")
        .expect("resolved");
    assert_eq!(
        link.values.get("marker"),
        Some(&RuntimeValue::from("special"))
    );
}

/// Subtype relationship properties are inherited and cannot be redeclared.
fn holding_subtype(artifacts: &mut Vec<ArtifactEnvelope>, redeclare: bool) {
    let mut subtype = serde_json::to_value(
        artifacts
            .iter()
            .find(|a| a.artifact_id == "canon.Holding.entity")
            .expect("base"),
    )
    .expect("wire");
    subtype["artifactId"] = json!("canon.SpecialHolding.entity");
    let mut object = json!({"name":"canon.SpecialHolding", "extends":"canon.Holding"});
    if redeclare {
        object["links"] = json!([{"id":"owner", "target":"canon.Party", "cardinality":{"to":"one","from":"many"}, "properties":[{"id":"marker","type":{"kind":"text"},"default":"different"}]}]);
    }
    subtype["payload"] = json!({"entityVersion":"2.0", "objectType":object});
    artifacts.push(serde_json::from_value(subtype).expect("subtype fixture"));
}

#[test]
fn obs_link_subtype_default_redeclarations_refuse_at_catalog_admission() {
    let mut artifacts = fixture_artifacts();
    holding_subtype(&mut artifacts, true);
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .into_iter()
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
            .any(|d| d.code.as_str() == "object_type.validation.link_override_redeclares")
    );
}

#[test]
fn obs_link_unrelated_item_labels_do_not_spend_the_claim_budget() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("labels.sqlite"));
    let source = fixture("erpnext");
    let template = source["snapshot"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["dataset"] == "Item")
        .expect("Item");
    for index in 0..33 {
        let mut row = template.clone();
        row["record"]["name"] = json!(format!("SYN-OTHER-{index}"));
        row["record"]["custom_norn_ref"] = json!(format!("demo/item/other/{index}"));
        row["record"]["code"] = json!(format!("OTHER-{index}-{}", "x".repeat(160_000)));
        row["record"]["label"] = json!("demo/item/probe");
        capture(&mut cell, "erpnext", &row, "snapshot");
    }
    let mut probe = template.clone();
    probe["record"]["name"] = json!("SYN-PROBE");
    probe["record"]["custom_norn_ref"] = json!("demo/item/probe");
    let id = retain_in(&mut cell, "erpnext", &probe, "snapshot", "snapshot");
    cell.process_mapping_bundle(&id)
        .expect("unrelated labels are not link claims");
}

fn provider(cell: &Cell) -> BTreeSet<String> {
    cell.store()
        .read()
        .links(
            MAIN_WORLD,
            "canon.OrderLine",
            "provider",
            "demo/order/1/line/1",
        )
        .expect("provider")
        .into_iter()
        .map(|l| l.target)
        .collect()
}

#[test]
fn obs_link_late_older_occurrence_is_contested_until_a_newer_one_records_order() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("late.sqlite"));
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let mut custodian = rows[6].clone();
    custodian["record"]["provider_ref"] = json!("demo/party/custodian");
    let mut customer = rows[6].clone();
    customer["record"]["provider_ref"] = json!("demo/party/customer");
    capture(&mut cell, "erpnext", &custodian, "3");
    assert_eq!(
        provider(&cell),
        BTreeSet::from(["demo/party/custodian".into()])
    );
    capture(&mut cell, "erpnext", &customer, "2");
    assert_eq!(
        provider(&cell),
        BTreeSet::from(["demo/party/custodian".into()]),
        "an older occurrence arriving late cannot supersede a newer stored claim"
    );
    let late = cell
        .store()
        .read()
        .observed_link_claims()
        .expect("claims")
        .into_iter()
        .find(|c| {
            c.role.as_str() == "provider"
                && c.target
                    .as_ref()
                    .is_some_and(|t| t.key == "demo/party/customer")
        })
        .expect("late claim retained as evidence");
    assert_eq!(late.supersedes.len(), 1, "it orders only over the snapshot");
    capture(&mut cell, "erpnext", &customer, "4");
    assert_eq!(
        provider(&cell),
        BTreeSet::from(["demo/party/customer".into()])
    );
}

#[test]
fn obs_link_target_arrival_resolves_only_the_current_pending_claim() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("closure.sqlite"));
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    capture(&mut cell, "erpnext", &rows[6], "snapshot");
    let mut custodian = rows[6].clone();
    custodian["record"]["provider_ref"] = json!("demo/party/custodian");
    capture(&mut cell, "erpnext", &custodian, "1");
    capture(&mut cell, "erpnext", &rows[0], "snapshot");
    assert!(
        provider(&cell).is_empty(),
        "owner arrival must read the superseding claim, though it names only the custodian"
    );
    capture(&mut cell, "erpnext", &rows[1], "snapshot");
    assert_eq!(
        provider(&cell),
        BTreeSet::from(["demo/party/custodian".into()])
    );
}

/// Absence through the mapping path, for uploaded data. A nullable property
/// mapped to no value (`canon.OrderLine.currency`, mapped from `null`) is
/// stored as no value and reads back as null: not "", not 0. A null in a
/// source field the source schema declares `required` (`Sales Order Item.unit`)
/// is refused and nothing is written, so an uploader declares the columns that
/// may be empty as `required: false` (the checker then types them nullable).
#[test]
fn absence_maps_to_null_and_a_required_source_field_refuses_it() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("nulls.sqlite"));
    let source = fixture("erpnext");
    for row in source["snapshot"].as_array().expect("rows") {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    assert_eq!(count_snapshot(&cell), (10, 0));
    let line = cell
        .store()
        .read()
        .authoritative_object(MAIN_WORLD, "canon.OrderLine", "demo/order/1/line/1")
        .expect("read")
        .expect("order line");
    let currency = line.values.get("currency");
    assert!(
        currency.is_none_or(|value| matches!(value, RuntimeValue::Null)),
        "currency reads back as null, got {currency:?}"
    );
    assert!(
        line.values
            .get("unit")
            .is_some_and(|value| !matches!(value, RuntimeValue::Null)),
        "present values are untouched"
    );

    let mut row = source["snapshot"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["dataset"] == "Sales Order Item")
        .expect("order row")
        .clone();
    row["record"]["unit"] = Value::Null;
    let id = retain_in(&mut cell, "erpnext", &row, "1", "snapshot");
    assert!(
        cell.process_mapping_bundle(&id).is_err(),
        "a null in a required source field is refused"
    );
    let unchanged = cell
        .store()
        .read()
        .authoritative_object(MAIN_WORLD, "canon.OrderLine", "demo/order/1/line/1")
        .expect("read")
        .expect("order line");
    assert_eq!(unchanged.version, line.version, "nothing was written");
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    /// The narrowed link plan equals the wide one (checked inside every
    /// mapping commit) over generated histories: deliveries in any order,
    /// re-pointed claims, redeliveries of older versions and, on the to-many
    /// `peers` link, cleared claims; for the production links (`from: one`
    /// owner) and the added `from: many` relation.
    #[test]
    fn obs_link_narrowed_plans_equal_wide_plans(
        many in proptest::bool::ANY,
        steps in proptest::collection::vec((0u8..7, 0u8..4, 1u8..6), 1..18),
    ) {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("shadow.sqlite");
        let mut cell = if many { setup_with(&path, |artifacts| many_holding(artifacts)) } else { setup(&path) };
        let source = fixture("erpnext");
        let rows = source["snapshot"].as_array().expect("rows").clone();
        let parties = ["demo/party/owner", "demo/party/custodian", "demo/party/customer", "clear"];
        for (row, party, version) in steps {
            // The erpnext snapshot has seven rows; `row` is drawn from 0..7.
            let mut row = rows[usize::from(row)].clone();
            let record = row["record"].as_object_mut().expect("record");
            let party = parties[usize::from(party)];
            if record.contains_key("provider_ref") && party != "clear" {
                record.insert("provider_ref".into(), json!(party));
            }
            if many && record.contains_key("owner_ref") {
                record.insert("owner_ref".into(), json!(party));
            }
            // A refusal of an unsupported profile is a valid outcome; a plan
            // mismatch surfaces as an encoding error and fails here.
            let event = version.to_string();
            let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                capture(&mut cell, "erpnext", &row, &event)
            }));
            if let Err(panic) = captured {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                proptest::prop_assert!(
                    !message.contains("narrowed link plan differs"),
                    "{message}"
                );
                break;
            }
        }
    }
}

/// A raw erpnext row at a fixed delivery clock, as a connector extract writes it.
fn raw_of(row: &Value, event: &str) -> (Observed<RuntimeValue>, MappingInput) {
    let record = &row["record"];
    let alias = record["name"].as_str().expect("source alias").to_owned();
    let payload = RuntimeValue::from_json(record.clone()).expect("payload");
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("json"))
        .expect("digest");
    let source = "synthetic-erpnext".to_owned();
    let raw = Observed {
        id: ObservationId(format!("delivery-{event}-{alias}")),
        provenance: norn_kernel::Provenance {
            source: source.clone(),
            source_id: alias.clone(),
            mapping_version: "synthetic/1".into(),
            observed_at: norn_kernel::LogicalTime::from_millis(1_793_702_400_000),
        },
        input: norn_ir::runtime_object! {
            "contract"=>"norn.raw_observation/v1", "source"=>source, "source_id"=>alias,
            "event_id"=>event, "source_version"=>event, "pipeline_version"=>"synthetic/1", "kind"=>"upsert", "extracted_at"=>1_793_702_400_000u64, "payload"=>payload, "payload_digest"=>digest,
        },
    };
    let input = MappingInput {
        mapping: "erpnext.three.mapping".into(),
        dataset: row["dataset"].as_str().expect("dataset").into(),
        connector_instance: "demo".into(),
        partition: "snapshot".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
    };
    (raw, input)
}

/// The committed end state OBS-PAGE-EQUIV compares: every canonical object
/// with its version, values and source lineage; links; link claims; the
/// ledger's events; and the pending (quarantined) source rows.
/// Page rows themselves are delivery framing and are left out.
fn end_state(cell: &Cell) -> Vec<String> {
    let reader = cell.store().read();
    let mut state = Vec::new();
    let budget = || {
        norn_cell::reactive::DecodeBudget::new(&norn_cell::reactive::ReactiveLimits {
            max_scan: 10_000,
            max_candidate_bytes_per_commit: 100_000_000,
            ..norn_cell::reactive::ReactiveLimits::from_profile(
                &norn_ir::ResourceLimitProfile::norn_limits_v1(),
            )
        })
    };
    for ty in [
        "canon.Item",
        "canon.Party",
        "canon.PartyRole",
        "canon.Holding",
        "canon.OrderLine",
    ] {
        let page = reader
            .authoritative_page(MAIN_WORLD, ty, None, 1_000)
            .expect("page");
        state.push(format!("{ty} quarantine {}", page.quarantine_count));
        for record in page.records {
            let object = reader
                .object(MAIN_WORLD, ty, &record.id)
                .expect("object")
                .expect("present");
            let fact = norn_ir::FactRef::new(ty, &object.id, object.version);
            let lineage = reader
                .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
                .expect("lineage");
            state.push(format!(
                "{ty} {} v{} {:?} {:?}",
                object.id, object.version, object.values, lineage
            ));
        }
    }
    for link in graph(cell) {
        state.push(format!("link {link:?}"));
    }
    for claim in reader.observed_link_claims().expect("claims") {
        state.push(format!("claim {claim:?}"));
    }
    // Events as a multiset (OBS-PAGE-EQUIV): a page commit appends its
    // records' object events in page order and then the page's link-claim
    // events, where one by one interleaves them per record; the ordering
    // that matters inside a commit is `claims_follow_their_referents`.
    let mut events: Vec<String> = reader
        .ledger(0)
        .expect("ledger")
        .into_iter()
        .map(|entry| format!("event {:?}", entry.event))
        .collect();
    events.sort();
    state.extend(events);
    for pending in reader.pending_observations().expect("pending") {
        if !pending.observation_id.starts_with("mapping-page:") {
            state.push(format!("pending {}", pending.observation_id));
        }
    }
    state
}

/// OBS-PAGE-1: within a commit no event precedes an event it depends on: a
/// link-claim event comes after the object events of both its endpoints
/// that the same commit creates or changes.
fn claims_follow_their_referents(cell: &Cell) -> Result<(), String> {
    let text = |value: &RuntimeValue, path: &[&str]| -> Option<String> {
        let mut at = value;
        for step in path {
            at = at.get(step)?;
        }
        at.as_str().map(str::to_owned)
    };
    let ledger = cell
        .store()
        .read()
        .ledger(0)
        .map_err(|error| error.to_string())?;
    for (index, entry) in ledger.iter().enumerate() {
        if text(&entry.event, &["event", "key", "objectType"]).as_deref()
            != Some(norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE)
        {
            continue;
        }
        for side in ["source_key", "target_key"] {
            let Some(key) = text(&entry.event, &["event", "values", side]) else {
                continue;
            };
            let later =
                ledger
                    .iter()
                    .enumerate()
                    .skip(index.saturating_add(1))
                    .any(|(_, other)| {
                        other.commit_seq == entry.commit_seq
                            && text(&other.event, &["event", "key", "objectType"]).as_deref()
                                != Some(norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE)
                            && text(&other.event, &["event", "key", "key"]).as_deref()
                                == Some(key.as_str())
                    });
            if later {
                return Err(format!(
                    "commit {}: claim at {index} precedes an event of {key}",
                    entry.commit_seq
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn obs_page_a_repeated_identity_refuses_the_whole_page_before_capture() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("framing.sqlite"));
    let rows = fixture("erpnext")["snapshot"]
        .as_array()
        .expect("rows")
        .clone();
    let (first, input) = raw_of(&rows[0], "1");
    let (again, _) = raw_of(&rows[0], "2");
    let (other, _) = raw_of(&rows[1], "1");
    assert!(
        cell.inject_mapping_page(&[first, other, again], &input)
            .is_err()
    );
    assert!(
        cell.store()
            .read()
            .pending_observations()
            .expect("pending")
            .is_empty(),
        "a refused page captures nothing"
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    /// OBS-PAGE-EQUIV: consuming pages equals consuming their records one by
    /// one in page order, over committed end state (objects with versions,
    /// values and lineage; links; claims; events; quarantine), modulo commit
    /// count. Batches are split before a repeated identity, as a host does,
    /// so identities repeat across pages; generated rows mix intra-page links
    /// in both orders (orders before or after their holdings and parties),
    /// re-pointed claims, cleared parties and older redeliveries.
    #[test]
    fn obs_page_equiv_pages_equal_one_by_one(
        many in proptest::bool::ANY,
        batches in proptest::collection::vec(
            proptest::collection::vec((0u8..7, 0u8..4, 1u8..6), 1..9),
            1..4,
        ),
    ) {
        let directory = tempfile::tempdir().expect("directory");
        let open = |name: &str| {
            let path = directory.path().join(name);
            if many { setup_with(&path, |artifacts| many_holding(artifacts)) } else { setup(&path) }
        };
        let (mut single, mut paged) = (open("single.sqlite"), open("paged.sqlite"));
        let rows = fixture("erpnext")["snapshot"].as_array().expect("rows").clone();
        let parties = ["demo/party/owner", "demo/party/custodian", "demo/party/customer", "clear"];
        for batch in batches {
            // Split before a repeated row identity, and per dataset (a page
            // reads one dataset).
            let mut pages: Vec<Vec<(Observed<RuntimeValue>, MappingInput)>> = Vec::new();
            let mut seen = BTreeSet::new();
            for (row, party, version) in batch {
                let mut row = rows[usize::from(row)].clone();
                let record = row["record"].as_object_mut().expect("record");
                let party = parties[usize::from(party)];
                if record.contains_key("provider_ref") && party != "clear" {
                    record.insert("provider_ref".into(), json!(party));
                }
                if many && record.contains_key("owner_ref") {
                    record.insert("owner_ref".into(), json!(party));
                }
                let (raw, input) = raw_of(&row, &version.to_string());
                let identity = raw.provenance.source_id.clone();
                let split = pages.last().is_none_or(|page| {
                    !seen.insert(identity.clone()) || page[0].1.dataset != input.dataset
                });
                if split {
                    seen = BTreeSet::from([identity]);
                    pages.push(Vec::new());
                }
                pages.last_mut().expect("page").push((raw, input));
            }
            for page in pages {
                for (raw, input) in &page {
                    let (id, _) = single.inject_mapping_bundle(raw, input).expect("capture");
                    single.process_mapping_outcome(&id).expect("one by one");
                }
                let raws: Vec<_> = page.iter().map(|(raw, _)| raw.clone()).collect();
                let ids = paged
                    .inject_mapping_page(&raws, &page[0].1)
                    .expect("page capture");
                for (id, _) in ids.pages {
                    paged.process_mapping_page(&id).expect("page");
                }
            }
        }
        let ordered = claims_follow_their_referents(&paged);
        proptest::prop_assert!(ordered.is_ok(), "{ordered:?}");
        let (single, paged) = (end_state(&single), end_state(&paged));
        let only = |a: &[String], b: &[String]| -> Vec<String> {
            a.iter().filter(|line| !b.contains(line)).cloned().collect()
        };
        proptest::prop_assert!(
            single == paged,
            "one by one only: {:#?}\npaged only: {:#?}\nfirst order difference: {:#?}",
            only(&single, &paged),
            only(&paged, &single),
            single
                .iter()
                .zip(&paged)
                .position(|(a, b)| a != b)
                .map(|at| (&single[at..at.saturating_add(3).min(single.len())], &paged[at..at.saturating_add(3).min(paged.len())]))
        );
    }
}

/// OBS-PAGE-1 export/restore: a page commit's export carries the members it
/// consumed, so a restored cell has the same state and the same per-record
/// lineage as the original.
#[tokio::test]
async fn obs_page_commits_export_and_restore_with_their_members() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("paged.sqlite"));
    let rows = fixture("erpnext")["snapshot"]
        .as_array()
        .expect("rows")
        .clone();
    // Rows of one dataset share a page; the orders link to holdings and
    // parties captured in earlier pages or in their own.
    let mut pages: Vec<(MappingInput, Vec<Observed<RuntimeValue>>)> = Vec::new();
    for row in rows.iter().rev() {
        let (raw, input) = raw_of(row, "1");
        match pages.last_mut() {
            Some((current, members)) if current.dataset == input.dataset => members.push(raw),
            _ => pages.push((input, vec![raw])),
        }
    }
    for (input, raws) in &pages {
        for (id, _) in cell
            .inject_mapping_page(raws, input)
            .expect("capture")
            .pages
        {
            cell.process_mapping_page(&id).expect("page");
        }
    }
    let mut exporter =
        norn_cell::export::Exporter::local(&directory.path().join("export")).expect("exporter");
    for seq in 1..=cell.store().export_commit_count().expect("count") {
        exporter
            .append(
                &cell
                    .store()
                    .export_record(seq)
                    .expect("journal")
                    .expect("record"),
            )
            .await
            .expect("append");
    }
    let manifest = exporter.verify().await.expect("verified export");
    let target = directory.path().join("restored.sqlite");
    let model = cell.store().model().clone();
    CellStore::restore_from(
        &mut exporter,
        &target,
        model.clone(),
        "org_demo/dev",
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored = SqliteCell::new(
        CellStore::open(&target, model, StoreOptions::default()).expect("reopen"),
        "org_demo/dev",
        |_| BTreeMap::new(),
    )
    .expect("cell");
    assert_eq!(end_state(&restored), end_state(&cell));
    assert_eq!(
        restored.store().read().dump().expect("dump"),
        cell.store().read().dump().expect("dump")
    );
}

/// OBS-PAGE-1: an object of a large page traces to its own source row under
/// the default decode budget of one trace.
#[test]
fn obs_page_members_trace_under_the_default_budget() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = setup(&directory.path().join("traced.sqlite"));
    let rows = fixture("erpnext")["snapshot"]
        .as_array()
        .expect("rows")
        .clone();
    let order = rows
        .iter()
        .find(|row| row["dataset"] == "Sales Order Item")
        .expect("an order line")
        .clone();
    for row in rows
        .iter()
        .filter(|row| row["dataset"] != "Sales Order Item")
    {
        let (raw, input) = raw_of(row, "1");
        for (id, _) in cell
            .inject_mapping_page(&[raw], &input)
            .expect("capture")
            .pages
        {
            cell.process_mapping_page(&id).expect("page");
        }
    }
    let mut lines = Vec::new();
    let mut input = None;
    // A host page (norn_api's PAGE_ROWS, 64): its receipt fits the budget.
    for n in 1..=64 {
        let mut line = order.clone();
        line["record"]["name"] = json!(format!("SYN-ERPNEXT-order-{n:05}"));
        line["record"]["custom_norn_ref"] = json!(format!("demo/order/{n:05}/line/1"));
        let (raw, page_input) = raw_of(&line, "1");
        lines.push(raw);
        input = Some(page_input);
    }
    let input = input.expect("input");
    for (id, _) in cell
        .inject_mapping_page(&raws, &input)
        .expect("capture")
        .pages
    {
        cell.process_mapping_page(&id).expect("page");
    }
    let reader = cell.store().read();
    let object = reader
        .object(MAIN_WORLD, "canon.OrderLine", "demo/order/00042/line/1")
        .expect("read")
        .expect("landed");
    let fact = norn_ir::FactRef::new("canon.OrderLine", &object.id, object.version);
    let mut budget =
        norn_cell::reactive::DecodeBudget::new(&norn_cell::reactive::ReactiveLimits::from_profile(
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        ));
    let traced = reader
        .instance_source_provenance(MAIN_WORLD, &fact, &mut budget, |_| Ok(true))
        .expect("traced")
        .expect("source lineage");
    assert!(
        format!("{traced:?}").contains("SYN-ERPNEXT-order-00042"),
        "{traced:?}"
    );
}
