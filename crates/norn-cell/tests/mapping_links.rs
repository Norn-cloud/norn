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
    store
        .install_definitions(
            model,
            InstalledDefinitionInput {
                canonical_set: String::from_utf8(
                    norn_ir::canonical::canonical_json_of(&artifacts).expect("set"),
                )
                .expect("UTF8"),
                registry: vec![],
                receipt_sha256: "synthetic-mapped-links".into(),
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
            "event_id"=>event, "source_version"=>event, "pipeline_version"=>"synthetic/1", "kind"=>"upsert", "extracted_at"=>1_793_702_400_000u64, "payload"=>payload, "payload_digest"=>digest,
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
    let template = source["snapshot"].as_array().expect("rows").iter()
        .find(|row| row["dataset"] == "Item").expect("Item");
    for index in 0..1001 {
        let mut row = template.clone();
        row["record"]["name"] = json!(format!("SYN-OTHER-{index}"));
        row["record"]["custom_norn_ref"] = json!(format!("demo/item/other/{index}"));
        row["record"]["code"] = json!(format!("OTHER-{index}"));
        row["record"]["label"] = json!("demo/item/probe");
        capture(&mut cell, "erpnext", &row, "snapshot");
    }
    let mut probe = template.clone();
    probe["record"]["name"] = json!("SYN-PROBE");
    probe["record"]["custom_norn_ref"] = json!("demo/item/probe");
    let id = retain_in(&mut cell, "erpnext", &probe, "snapshot", "snapshot");
    cell.process_mapping_bundle(&id).expect("unrelated labels are not link claims");
}
