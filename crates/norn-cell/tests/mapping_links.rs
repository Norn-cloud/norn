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
use norn_cell::{CellStore, MAIN_WORLD, SqliteCell, StoreOptions};
use norn_ir::{ArtifactEnvelope, RuntimeObject, RuntimeValue};
use norn_kernel::{FactKey, LinkKey, ObservationEvent, ObservationId, Observed};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

type Cell = SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>;
#[path = "support/mapped_cell.rs"]
mod mapped_cell;
use mapped_cell::{fixture_artifacts, root};
fn setup(path: &Path) -> Cell {
    setup_with(path, |_| {})
}

fn setup_with(path: &Path, edit: impl FnOnce(&mut Vec<ArtifactEnvelope>)) -> Cell {
    // Every plan in these tests is also checked against the wide journal scope.
    norn_cell::check_link_plans_against_wide_scope(true);
    let store = mapped_cell::store(path, edit);
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
    #![proptest_config(proptest::test_runner::Config::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(24)
    ))]
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
    raw_in(row, event, "snapshot")
}

/// [`raw_of`] read from `partition`: another contributor of the same records.
fn raw_in(row: &Value, event: &str, partition: &str) -> (Observed<RuntimeValue>, MappingInput) {
    let record = &row["record"];
    let alias = record["name"].as_str().expect("source alias").to_owned();
    let payload = RuntimeValue::from_json(record.clone()).expect("payload");
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("json"))
        .expect("digest");
    let source = "synthetic-erpnext".to_owned();
    let raw = Observed {
        id: ObservationId(if partition == "snapshot" {
            format!("delivery-{event}-{alias}")
        } else {
            format!("delivery-{partition}-{event}-{alias}")
        }),
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
        partition: partition.into(),
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

/// One generated step: (erpnext snapshot row, party it points at, source version).
type Step = (u8, u8, u8);

/// Both cells of an OBS-PAGE-EQUIV run, `many` choosing the to-many `peers` relation.
fn equiv_cells(directory: &Path, many: bool) -> (Cell, Cell) {
    let open = |name: &str| {
        let path = directory.join(name);
        if many {
            setup_with(&path, |artifacts| many_holding(artifacts))
        } else {
            setup(&path)
        }
    };
    (open("single.sqlite"), open("paged.sqlite"))
}

/// Deliver `batches` to `single` one record at a time and to `paged` a page
/// at a time. Batches are split before a repeated identity, as a host does,
/// and per dataset (a page reads one dataset).
fn deliver(single: &mut Cell, by_page: &mut Cell, many: bool, batches: &[Vec<Step>]) {
    let rows = fixture("erpnext")["snapshot"]
        .as_array()
        .expect("rows")
        .clone();
    let parties = [
        "demo/party/owner",
        "demo/party/custodian",
        "demo/party/customer",
        "clear",
    ];
    for batch in batches {
        let batch: Vec<(Value, String)> = batch
            .iter()
            .map(|&(row, party, version)| {
                let mut row = rows[usize::from(row)].clone();
                let record = row["record"].as_object_mut().expect("record");
                let party = parties[usize::from(party)];
                if record.contains_key("provider_ref") && party != "clear" {
                    record.insert("provider_ref".into(), json!(party));
                }
                if many && record.contains_key("owner_ref") {
                    record.insert("owner_ref".into(), json!(party));
                }
                (row, version.to_string())
            })
            .collect();
        deliver_rows(single, by_page, &batch);
    }
}

/// Deliver one batch of (erpnext row, source version) to `single` one record
/// at a time and to `by_page` a page at a time, split as [`deliver`] splits.
fn deliver_rows(single: &mut Cell, by_page: &mut Cell, batch: &[(Value, String)]) {
    {
        let mut pages: Vec<Vec<(Observed<RuntimeValue>, MappingInput)>> = Vec::new();
        let mut seen = BTreeSet::new();
        for (row, version) in batch {
            let (raw, input) = raw_of(row, version);
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
            let delivered: Vec<_> = page.iter().map(|(raw, _)| raw.clone()).collect();
            deliver_page(by_page, &delivered, &page[0].1);
        }
    }
}

/// Deliver one page as a host does (`DevHost::ingest`): captured and
/// committed whole, or, when the cell refuses a page that would capture a
/// dispute after a record it commits (OBS-LINK-DISPUTE-1), one record per
/// page, each captured and committed in order.
fn deliver_page(cell: &mut Cell, raws: &[Observed<RuntimeValue>], input: &MappingInput) {
    match cell.inject_mapping_page(raws, input) {
        Ok(ids) => {
            for (id, _) in ids.pages {
                cell.process_mapping_page(&id).expect("page");
            }
        }
        Err(norn_cell::CellError::MappingPageSpansDispute) if raws.len() > 1 => {
            for raw in raws {
                deliver_page(cell, std::slice::from_ref(raw), input);
            }
        }
        Err(error) => panic!("page capture: {error}"),
    }
}

/// OBS-PAGE-EQUIV over committed end state, explained when it fails.
fn same_end_state(single: &Cell, paged: &Cell) -> Result<(), String> {
    claims_follow_their_referents(paged)?;
    let (single, paged) = (end_state(single), end_state(paged));
    if single == paged {
        return Ok(());
    }
    let only = |a: &[String], b: &[String]| -> Vec<String> {
        a.iter().filter(|line| !b.contains(line)).cloned().collect()
    };
    Err(format!(
        "one by one only: {:#?}\npaged only: {:#?}",
        only(&single, &paged),
        only(&paged, &single),
    ))
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(24)
    ))]
    /// OBS-PAGE-EQUIV: consuming pages equals consuming their records one by
    /// one in page order, over committed end state (objects with versions,
    /// values and lineage; links; claims; events; quarantine), modulo commit
    /// count. Identities repeat across pages; generated rows mix intra-page
    /// links in both orders (orders before or after their holdings and
    /// parties), re-pointed claims, cleared parties, older redeliveries and
    /// conflicting re-sends of an event (FEED-CONFLICT).
    #[test]
    fn obs_page_equiv_pages_equal_one_by_one(
        many in proptest::bool::ANY,
        batches in proptest::collection::vec(
            proptest::collection::vec((0u8..7, 0u8..4, 1u8..6), 1..9),
            1..4,
        ),
    ) {
        let directory = tempfile::tempdir().expect("directory");
        let (mut single, mut paged) = equiv_cells(directory.path(), many);
        deliver(&mut single, &mut paged, many, &batches);
        let same = same_end_state(&single, &paged);
        proptest::prop_assert!(same.is_ok(), "{}", same.err().unwrap_or_default());
    }
}

/// #1490: the shrunk OBS-PAGE-EQUIV counterexample (its seed is also in
/// `mapping_links.proptest-regressions`). Before the fix, one by one
/// quarantined holding-north with a narrowed/wide plan mismatch while the
/// page landed it.
#[test]
fn obs_page_equiv_conflicting_resend_seed_1490() {
    let directory = tempfile::tempdir().expect("directory");
    let (mut single, mut paged) = equiv_cells(directory.path(), true);
    let batches = [
        vec![(5, 2, 1), (5, 0, 4)],
        vec![(0, 0, 1), (2, 0, 1), (5, 1, 4)],
        vec![(4, 0, 1), (5, 0, 1)],
    ];
    deliver(&mut single, &mut paged, true, &batches);
    same_end_state(&single, &paged).expect("pages equal one by one");
}

/// Deliver `steps` (one batch each) to both paths of a `many` cell, and
/// list where either path's `peers` links or pending rows differ from
/// `expected` with only the conflicting re-send quarantined, or the paths
/// differ from each other.
fn dispute_mismatches(steps: &[Vec<Step>], expected: &[(&str, &str)]) -> Vec<String> {
    let directory = tempfile::tempdir().expect("directory");
    let (mut single, mut paged) = equiv_cells(directory.path(), true);
    for step in steps {
        deliver(&mut single, &mut paged, true, std::slice::from_ref(step));
    }
    let expected: BTreeSet<(String, String)> = expected
        .iter()
        .map(|(source, target)| ((*source).to_owned(), (*target).to_owned()))
        .collect();
    let mut mismatches = Vec::new();
    for (path, cell) in [("one by one", &single), ("paged", &paged)] {
        let peers: BTreeSet<(String, String)> = graph(cell)
            .into_iter()
            .filter(|(_, _, link, _, _)| link == "peers")
            .map(|(_, source, _, _, target)| (source, target))
            .collect();
        if peers != expected {
            mismatches.push(format!("{path}: peers {peers:?}"));
        }
        let pending: Vec<String> = cell
            .store()
            .read()
            .pending_observations()
            .expect("pending")
            .into_iter()
            .map(|pending| pending.observation_id)
            .filter(|id| !id.starts_with("mapping-page:"))
            .collect();
        if !(pending.len() == 1 && pending[0].starts_with("mapping-conflict:")) {
            mismatches.push(format!("{path}: pending {pending:?}"));
        }
    }
    if let Err(differ) = same_end_state(&single, &paged) {
        mismatches.push(differ);
    }
    mismatches
}

const NORTH: &str = "demo/holding/owner/north/gold";
const SOUTH: &str = "demo/holding/owner/south/gold";
const OWNER: &str = "demo/party/owner";

/// OBS-LINK-DISPUTE-1 (#1490): a conflicting re-send of a landed event
/// freezes that record's claims. Its claim keeps its standing and its edge,
/// so the older claim it superseded never comes back, on either path and
/// whichever later commit reads the group.
#[test]
fn obs_link_dispute_freezes_the_disputed_records_links() {
    let (owner, custodian, customer) = (0, 1, 2);
    let (party_owner, party_customer, north, south) = (0, 2, 4, 5);
    // 1. south v1 points at the absent customer: a pending claim A.
    // 2. south v4 points at the owner: claim B supersedes A.
    let history = [vec![(south, customer, 1)], vec![(south, owner, 4)]];
    // 4. south v4 again with other content: FEED-CONFLICT on B's event.
    let conflict = vec![(south, custodian, 4)];
    // The issue's sequence: 3. both parties land (south -peers-> owner);
    // 4.; 5. north v1 points at the owner. Before the fix the paths
    // disagreed on north (one by one refused its plan as differing from the
    // wide one, which read south's revived claim A).
    let issue = [
        history.to_vec(),
        vec![vec![(party_owner, owner, 1), (party_customer, owner, 1)]],
        vec![conflict.clone(), vec![(north, owner, 1)]],
    ]
    .concat();
    // The owner lands only after the conflict: the disputed claim B keeps
    // its standing but adds no edge, on either path.
    let absent = [
        history.to_vec(),
        vec![conflict.clone(), vec![(party_owner, owner, 1)]],
    ]
    .concat();
    // The customer lands only after the conflict, so its commit reads
    // south's group on both paths and in production too. Before the fix it
    // revived A and added south -peers-> customer.
    let late = [
        history.to_vec(),
        vec![
            vec![(party_owner, owner, 1)],
            conflict,
            vec![(party_customer, owner, 1)],
        ],
    ]
    .concat();
    let mismatches = [
        (
            "issue sequence",
            dispute_mismatches(&issue, &[(NORTH, OWNER), (SOUTH, OWNER)]),
        ),
        ("late target", dispute_mismatches(&late, &[(SOUTH, OWNER)])),
        ("owner absent at dispute", dispute_mismatches(&absent, &[])),
    ];
    assert!(
        mismatches.iter().all(|(_, found)| found.is_empty()),
        "{mismatches:#?}"
    );
}

/// A `refers` relation between parties, mapped from one party row: the
/// referred party (`REFERRED`) refers to `ZED`. Its target is a record of the
/// same dataset, so the target and a re-send of the referring row can share
/// one page.
fn referring_parties(artifacts: &mut [ArtifactEnvelope]) {
    edit_artifact(artifacts, "canon.Party.entity", |a| {
        a["payload"]["objectType"]["links"] = json!([{
            "id":"refers", "target":"canon.Party", "cardinality":{"to":"many","from":"many"}
        }]);
    });
    edit_artifact(artifacts, "erpnext.three.mapping", |a| {
        let map = a["payload"]["maps"]
            .as_array_mut()
            .expect("maps")
            .iter_mut()
            .find(|m| m["id"] == "party")
            .expect("party");
        map["links"] = json!([{
            "key": "input.name == 'SYN-ERPNEXT-referred' ? 'demo/party/zed' : null",
            "link": "refers",
            "targetMap": "party",
        }]);
    });
}

const REFERRED: &str = "demo/party/referred";
const ZED: &str = "demo/party/zed";

/// An erpnext Party row for `name` (`SYN-ERPNEXT-<name>`), labelled `label`.
fn party_row(name: &str, label: &str) -> Value {
    let mut row = fixture("erpnext")["snapshot"][2].clone();
    let record = row["record"].as_object_mut().expect("record");
    record.insert("name".into(), json!(format!("SYN-ERPNEXT-{name}")));
    record.insert(
        "custom_norn_ref".into(),
        json!(format!("demo/party/{name}")),
    );
    record.insert("label".into(), json!(label));
    row
}

/// The `refers` links and held-link findings of each path, after `batches`.
#[allow(clippy::type_complexity, reason = "both paths' links and findings")]
fn refers_after(
    batches: &[Vec<(Value, String)>],
) -> Vec<(BTreeSet<(String, String)>, Vec<String>)> {
    let directory = tempfile::tempdir().expect("directory");
    let open = |name: &str| setup_with(&directory.path().join(name), |a| referring_parties(a));
    let (mut single, mut paged) = (open("single.sqlite"), open("paged.sqlite"));
    for batch in batches {
        deliver_rows(&mut single, &mut paged, batch);
    }
    same_end_state(&single, &paged).expect("pages equal one by one");
    [&single, &paged]
        .into_iter()
        .map(|cell| {
            (
                refers(cell),
                held(cell).into_iter().map(|f| f.message).collect(),
            )
        })
        .collect()
}

/// A cell's `refers` links, as (source, target).
fn refers(cell: &Cell) -> BTreeSet<(String, String)> {
    graph(cell)
        .into_iter()
        .filter(|(_, _, link, _, _)| link == "refers")
        .map(|(_, source, _, _, target)| (source, target))
        .collect()
}

/// The referred party's held-link findings.
fn held(cell: &Cell) -> Vec<norn_ir::DataQualityFinding> {
    cell.store()
        .read()
        .observation_findings(MAIN_WORLD, "canon.Party", REFERRED)
        .expect("findings")
        .into_iter()
        .filter(|f| matches!(&f.rule_ref, norn_ir::DataQualityRuleRef::Builtin { id, .. } if id == norn_cell::LINK_HELD_RULE))
        .collect()
}

/// A referring-parties cell named `name` in `directory`.
fn referring_cell(directory: &Path, name: &str) -> Cell {
    setup_with(&directory.join(name), |a| referring_parties(a))
}

/// Capture `(row, event, partition)` and process it one by one.
fn one_by_one(cell: &mut Cell, (row, event, partition): (&Value, &str, &str)) -> String {
    let (raw, input) = raw_in(row, event, partition);
    let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
    cell.process_mapping_outcome(&id).expect("one by one");
    id
}

/// A party row named `name` whose record is the canonical `demo/party/<of>`:
/// another source row for that party, which claims no `refers` link.
fn alias_row(name: &str, of: &str, label: &str) -> Value {
    let mut row = party_row(name, label);
    row["record"]["custom_norn_ref"] = json!(format!("demo/party/{of}"));
    row
}

/// Write a page row naming `members`, as an earlier version's capture did.
fn page_row(cell: &mut Cell, members: &[String], provenance: norn_kernel::Provenance) -> String {
    let page = format!(
        "mapping-page:{}",
        norn_ir::canonical::domain_hash_hex(b"norn.mapping.page.v1\0", members)
            .expect("page digest")
    );
    cell.inject(&Observed {
        id: ObservationId(page.clone()),
        provenance,
        input: RuntimeObject::from([
            (
                "contract".to_owned(),
                RuntimeValue::from("norn.mapping_page/v1".to_owned()),
            ),
            (
                "members".to_owned(),
                RuntimeValue::Array(members.iter().cloned().map(RuntimeValue::from).collect()),
            ),
        ]),
    })
    .expect("page row");
    page
}

/// OBS-LINK-DISPUTE-1 (Sol round 2, S1): a page captured by an earlier
/// version can hold a dispute row between its members: here an unrelated
/// party, the conflicting re-send of the referred party, then zed. One by
/// one, zed's commit sees the conflict and holds the edge. The freeze reads
/// the disputes that exist when a commit runs (round 4), so the old page's
/// one commit, which runs after the conflict was captured, holds it too.
#[test]
fn obs_link_dispute_inside_a_page_captured_before_the_split_applies_record_by_record() {
    let directory = tempfile::tempdir().expect("directory");
    let mut single = referring_cell(directory.path(), "single.sqlite");
    let mut paged = referring_cell(directory.path(), "paged.sqlite");
    let referred = party_row("referred", "Referred");
    for cell in [&mut single, &mut paged] {
        one_by_one(cell, (&referred, "1", "snapshot"));
    }
    let batch = [
        party_row("other", "Other"),
        party_row("referred", "Referred, re-sent"),
        party_row("zed", "Zed"),
    ];
    for row in &batch {
        one_by_one(&mut single, (row, "1", "snapshot"));
    }
    // The previous version's capture: each record's own row, then one page
    // row naming both mapped records, with the conflict row between them.
    let mut members = Vec::new();
    let mut provenance = None;
    for row in &batch {
        let (raw, input) = raw_of(row, "1");
        let (id, _) = paged.inject_mapping_bundle(&raw, &input).expect("capture");
        if id.starts_with("mapping-bundle:") {
            provenance.get_or_insert_with(|| raw.provenance.clone());
            members.push(id);
        }
    }
    assert_eq!(
        members.len(),
        2,
        "the re-send is a conflict row, not a member"
    );
    let page = page_row(&mut paged, &members, provenance.expect("provenance"));
    paged.process_mapping_page(&page).expect("page");
    same_end_state(&single, &paged).expect("the old page equals one by one");
    for (cell, path) in [(&single, "one by one"), (&paged, "paged")] {
        assert_eq!(refers(cell), BTreeSet::new(), "{path}: refers links");
        assert_eq!(held(cell).len(), 1, "{path}: held findings");
    }
}

/// OBS-LINK-DISPUTE-1 (Sol round 2, S3): a retried delivery commits under
/// the disputes that exist when it is retried. An update to the referred
/// party from another partition (it claims no link) is captured before the
/// conflict and left pending; zed lands after the conflict, so the edge is
/// held. Retrying the update re-plans the referred party's group: round 2
/// dated that plan at the update's old position and added the edge. The
/// dispute exists at the retry, so the edge stays held; processed right
/// away, the update finds zed absent and adds nothing. Both orders end in
/// the same state.
#[test]
fn obs_link_dispute_a_retried_update_dates_its_add_by_the_target() {
    let directory = tempfile::tempdir().expect("directory");
    let referred = party_row("referred", "Referred");
    let update = alias_row("referred-desk", "referred", "Referred desk");
    let resend = party_row("referred", "Referred, re-sent");
    let zed = party_row("zed", "Zed");
    let mut cells = Vec::new();
    for (name, retried) in [("right-away.sqlite", false), ("retried.sqlite", true)] {
        let mut cell = referring_cell(directory.path(), name);
        one_by_one(&mut cell, (&referred, "1", "snapshot"));
        let (raw, input) = raw_in(&update, "2", "desk");
        let (pending, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
        if !retried {
            cell.process_mapping_outcome(&pending).expect("right away");
        }
        one_by_one(&mut cell, (&resend, "1", "snapshot"));
        one_by_one(&mut cell, (&zed, "1", "snapshot"));
        if retried {
            cell.process_mapping_outcome(&pending).expect("retried");
        }
        assert_eq!(refers(&cell), BTreeSet::new(), "{name}: refers links");
        assert_eq!(held(&cell).len(), 1, "{name}: held findings");
        cells.push(cell);
    }
    same_end_state(&cells[0], &cells[1]).expect("a retry equals processing right away");
}

/// An inbox row's position, read from the cell's file.
fn position_of(db: &rusqlite::Connection, id: &str) -> i64 {
    db.query_row(
        "SELECT position FROM _norn_inbox WHERE observation_id = ?1",
        [id],
        |row| row.get(0),
    )
    .expect("inbox row")
}

/// The state a version before the run split (round 2) left in `cell` for
/// `rows` (an unrelated party, the referred party's conflicting re-send,
/// zed), captured as one page: each record's own row, one page row naming
/// both mapped records with the conflict row between them, and that page
/// committed as one, holding the edge (the round-1 freeze read the
/// disputes that existed at commit). Round 3 refuses to commit such a page
/// whole, so the test reaches that state by committing it while the
/// unrelated record's inbox row is moved past the page, then moving it
/// back: the positions end as they were captured.
fn legacy_page_committed_across_the_conflict(cell: &mut Cell, path: &Path, rows: &[Value]) {
    let mut members = Vec::new();
    let mut provenance = None;
    for row in rows {
        let (raw, input) = raw_of(row, "1");
        let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
        if id.starts_with("mapping-bundle:") {
            provenance.get_or_insert_with(|| raw.provenance.clone());
            members.push(id);
        }
    }
    assert_eq!(members.len(), 2, "the re-send is a conflict row");
    let page = page_row(cell, &members, provenance.expect("provenance"));
    let db = rusqlite::Connection::open(path).expect("open");
    let captured = position_of(&db, &members[0]);
    let past: i64 = db
        .query_row("SELECT MAX(position) + 1 FROM _norn_inbox", [], |row| {
            row.get(0)
        })
        .expect("last position");
    let moved = |to: i64| {
        db.execute(
            "UPDATE _norn_inbox SET position = ?1 WHERE observation_id = ?2",
            rusqlite::params![to, &members[0]],
        )
        .expect("move");
    };
    moved(past);
    let outcomes = cell
        .process_mapping_page(&page)
        .expect("page")
        .expect("pending");
    moved(captured);
    assert!(
        outcomes
            .iter()
            .all(|(_, outcome)| *outcome == norn_cell::MappingOutcome::Landed),
        "{outcomes:?}"
    );
    let consumed: BTreeSet<i64> = members
        .iter()
        .map(|id| {
            db.query_row(
                "SELECT consumed_commit_seq FROM _norn_inbox WHERE observation_id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("consumed")
        })
        .collect();
    assert_eq!(consumed.len(), 1, "one commit consumed both members");
    let conflict = position_of(
        &db,
        &db.query_row(
            "SELECT observation_id FROM _norn_inbox WHERE observation_id LIKE 'mapping-conflict:%'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("conflict"),
    );
    assert!(
        captured < conflict && conflict < position_of(&db, &members[1]),
        "the page straddles the conflict"
    );
    assert_eq!(refers(cell), BTreeSet::new(), "the page held the edge");
}

/// OBS-LINK-DISPUTE-1 (Sol round 3, S3): an update to the referred party,
/// captured before the conflict, is retried after a page an earlier version
/// committed across the conflict (the unrelated party before it, zed after
/// it). Round 3 dated zed by that commit's earliest member, before the
/// conflict, so the retry added the edge; one by one, zed lands after the
/// conflict and the retry holds it. The freeze reads the dispute that exists
/// at the retry, so both hold it.
#[test]
fn obs_link_dispute_a_retry_after_a_legacy_page_keeps_the_edge_held() {
    let directory = tempfile::tempdir().expect("directory");
    let referred = party_row("referred", "Referred");
    let update = alias_row("referred-desk", "referred", "Referred desk");
    let batch = [
        party_row("other", "Other"),
        party_row("referred", "Referred, re-sent"),
        party_row("zed", "Zed"),
    ];
    let mut cells = Vec::new();
    for (name, legacy) in [("one-by-one.sqlite", false), ("legacy.sqlite", true)] {
        let path = directory.path().join(name);
        let mut cell = referring_cell(directory.path(), name);
        one_by_one(&mut cell, (&referred, "1", "snapshot"));
        let (raw, input) = raw_in(&update, "2", "desk");
        let (pending, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
        if legacy {
            legacy_page_committed_across_the_conflict(&mut cell, &path, &batch);
        } else {
            for row in &batch {
                one_by_one(&mut cell, (row, "1", "snapshot"));
            }
        }
        assert_eq!(
            cell.process_mapping_outcome(&pending).expect("retried"),
            norn_cell::MappingOutcome::Landed
        );
        cells.push((name, cell));
    }
    let found: Vec<_> = cells
        .iter()
        .map(|(name, cell)| (*name, refers(cell), held(cell).len()))
        .collect();
    assert_eq!(
        found,
        [
            ("one-by-one.sqlite", BTreeSet::new(), 1),
            ("legacy.sqlite", BTreeSet::new(), 1)
        ],
        "the edge stays held after the retry"
    );
    same_end_state(&cells[0].1, &cells[1].1).expect("the legacy page equals one by one");
}

/// OBS-LINK-DISPUTE-1 (Sol round 3, S3a): zed is created by an action after
/// the conflict, so no mapping delivery establishes it and round 3 had no
/// inbox position to date it by. An update to the referred party captured
/// before the conflict, retried afterwards, then added the disputed edge.
/// The dispute exists at the retry, so the edge stays held, as it does when
/// the update is processed right away (zed is absent then).
#[test]
fn obs_link_dispute_a_retry_after_an_action_created_target_keeps_the_edge_held() {
    let directory = tempfile::tempdir().expect("directory");
    let referred = party_row("referred", "Referred");
    let update = alias_row("referred-desk", "referred", "Referred desk");
    let resend = party_row("referred", "Referred, re-sent");
    let zed = FactKey::new("canon.Party", ZED);
    // Zed's values as a mapping writes them, for the action to write.
    let values = {
        let mut cell = referring_cell(directory.path(), "values.sqlite");
        one_by_one(&mut cell, (&party_row("zed", "Zed"), "1", "snapshot"));
        norn_kernel::CellHost::load(&cell, &zed)
            .expect("zed")
            .values
            .expect("present")
    };
    let mut found = Vec::new();
    for (name, retried) in [("right-away.sqlite", false), ("retried.sqlite", true)] {
        let mut cell = referring_cell(directory.path(), name);
        one_by_one(&mut cell, (&referred, "1", "snapshot"));
        let (raw, input) = raw_in(&update, "2", "desk");
        let (pending, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
        if !retried {
            cell.process_mapping_outcome(&pending).expect("right away");
        }
        one_by_one(&mut cell, (&resend, "1", "snapshot"));
        norn_kernel::CellHost::commit(
            &mut cell,
            norn_kernel::CellCommit {
                writes: vec![norn_kernel::FactWrite {
                    key: zed.clone(),
                    expected: norn_kernel::FactVersion::ABSENT,
                    mutation: norn_kernel::FactMutation::Put {
                        values: values.clone(),
                    },
                }],
                ..norn_kernel::CellCommit::empty()
            },
        )
        .expect("the action creates zed");
        if retried {
            assert_eq!(
                cell.process_mapping_outcome(&pending).expect("retried"),
                norn_cell::MappingOutcome::Landed
            );
        }
        found.push((name, refers(&cell), held(&cell).len()));
    }
    assert_eq!(
        found,
        [
            ("right-away.sqlite", BTreeSet::new(), 1),
            ("retried.sqlite", BTreeSet::new(), 1)
        ],
        "the edge stays held after the retry"
    );
}

/// OBS-LINK-DISPUTE-1 (round 4): a page that would capture a new dispute row
/// after a record it commits captures nothing (its transaction rolls back),
/// so the host can deliver its records one at a time. A page whose dispute
/// comes before every record it commits is captured as before.
#[test]
fn obs_link_dispute_a_page_never_captures_a_dispute_after_a_member() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = referring_cell(directory.path(), "capture.sqlite");
    one_by_one(
        &mut cell,
        (&party_row("referred", "Referred"), "1", "snapshot"),
    );
    let page = |rows: &[Value]| -> (Vec<Observed<RuntimeValue>>, MappingInput) {
        let mut input = None;
        let raws = rows
            .iter()
            .map(|row| {
                let (raw, row_input) = raw_of(row, "1");
                input.get_or_insert(row_input);
                raw
            })
            .collect();
        (raws, input.expect("input"))
    };
    let (zed, resend) = (
        party_row("zed", "Zed"),
        party_row("referred", "Referred, re-sent"),
    );
    let before = cell.store().read().dump().expect("dump");
    let (raws, input) = page(&[zed.clone(), resend.clone()]);
    let refused = cell
        .inject_mapping_page(&raws, &input)
        .expect_err("the conflict follows zed");
    assert!(
        matches!(refused, norn_cell::CellError::MappingPageSpansDispute),
        "{refused}"
    );
    assert_eq!(
        cell.store().read().dump().expect("dump"),
        before,
        "nothing was captured"
    );
    let (raws, input) = page(&[resend, zed]);
    let captured = cell
        .inject_mapping_page(&raws, &input)
        .expect("the conflict comes first");
    assert!(
        captured.records[0].starts_with("mapping-conflict:"),
        "{captured:?}"
    );
    assert_eq!(captured.pages.len(), 1, "{captured:?}");
}

/// OBS-LINK-DISPUTE-1 (Sol round 2, S4): two disputed contributions holding
/// one edge make one held finding, with both claims' lineage.
#[test]
fn obs_link_dispute_one_held_finding_per_edge() {
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = referring_cell(directory.path(), "two.sqlite");
    for partition in ["snapshot", "desk"] {
        one_by_one(
            &mut cell,
            (&party_row("referred", "Referred"), "1", partition),
        );
    }
    for partition in ["snapshot", "desk"] {
        one_by_one(
            &mut cell,
            (&party_row("referred", "Referred, re-sent"), "1", partition),
        );
    }
    one_by_one(&mut cell, (&party_row("zed", "Zed"), "1", "snapshot"));
    assert_eq!(refers(&cell), BTreeSet::new(), "the edge is held");
    let held = held(&cell);
    assert_eq!(held.len(), 1, "{held:#?}");
    assert_eq!(held[0].provenance.len(), 2, "both claims' lineage");
    assert_eq!(
        held[0].message.matches("conflict ").count(),
        2,
        "{}",
        held[0].message
    );
}

/// OBS-LINK-DISPUTE-1 (Sol, #1544): paging never moves the dispute boundary.
/// The referred party lands with a pending claim to the absent zed; then one
/// batch holds zed and a conflicting re-send of the referred party, in either
/// order. Zed first: one by one commits the edge before the conflict is
/// captured; the page would capture the conflict after zed, so the cell
/// refuses it and the host delivers its records one at a time, adding the
/// edge too. Conflict first: the freeze holds the edge on both paths, and
/// the held edge is a finding on the referred party.
#[test]
fn obs_link_dispute_boundary_is_inbox_order_on_both_paths() {
    let referred = || (party_row("referred", "Referred"), "1".to_owned());
    let resend = || (party_row("referred", "Referred, re-sent"), "1".to_owned());
    let zed = || (party_row("zed", "Zed"), "1".to_owned());
    let edge = BTreeSet::from([(REFERRED.to_owned(), ZED.to_owned())]);
    for (case, batch, links, held) in [
        ("target, then conflict", vec![zed(), resend()], edge, false),
        (
            "conflict, then target",
            vec![resend(), zed()],
            BTreeSet::new(),
            true,
        ),
    ] {
        let paths = refers_after(&[vec![referred()], batch]);
        for ((found, findings), path) in paths.iter().zip(["one by one", "paged"]) {
            assert_eq!(found, &links, "{case}, {path}: refers links");
            assert_eq!(
                findings.len(),
                usize::from(held),
                "{case}, {path}: {findings:?}"
            );
            for message in findings {
                assert!(
                    message.starts_with(&format!(
                        "link {REFERRED} -refers-> {ZED} held: record in dispute (conflict "
                    )),
                    "{case}, {path}: {message}"
                );
            }
        }
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

/// OBS-PAGE-1: an object of a page of 500 records traces to its own source
/// row under the default decode budget of one trace: the page commit's receipt
/// (about 1.3 KB per record) is read within the trace's receipt allowance.
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
    for n in 1..=500 {
        let mut line = order.clone();
        line["record"]["name"] = json!(format!("SYN-ERPNEXT-order-{n:05}"));
        line["record"]["custom_norn_ref"] = json!(format!("demo/order/{n:05}/line/1"));
        let (raw, page_input) = raw_of(&line, "1");
        lines.push(raw);
        input = Some(page_input);
    }
    let input = input.expect("input");
    for (id, _) in cell
        .inject_mapping_page(&lines, &input)
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

/// MAP-DERIVATION-1 / MAP-REPROCESS-1: a holding quarantined before decimal
/// text conformed (its bundle retains the pre-MAP-EXPLAIN-1 diagnostic and no
/// derivation) rechecks under its own derivation, so disclosure and a replay
/// of the same event still work and the row stays quarantined; it lands only
/// through the explicit reprocessing path, which consumes the original.
#[test]
fn a_bundle_quarantined_before_decimal_text_rechecks_under_its_own_derivation_and_lands_only_when_reprocessed()
 {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("legacy.sqlite");
    let mut cell = setup(&path);
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows
        .iter()
        .filter(|row| row["dataset"] != "Warehouse Lot Balance")
    {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let mut holding = rows
        .iter()
        .find(|row| row["dataset"] == "Warehouse Lot Balance")
        .expect("holding")
        .clone();
    holding["record"]["quantity"] = json!("200.50");
    holding["record"]["name"] = json!("SYN-ERPNEXT-legacy");
    let id = retain_in(&mut cell, "erpnext", &holding, "snapshot", "snapshot");
    let bundles = |cell: &Cell| {
        cell.store().read().source_event_mapping_bundles(
            "demo",
            "snapshot",
            "synthetic-erpnext",
            "SYN-ERPNEXT-legacy",
            "snapshot",
        )
    };
    let current = bundles(&cell).expect("captured").remove(0);
    assert!(current.diagnostics.is_empty() && !current.records.is_empty());
    assert_eq!(current.derivation, 2);

    retain_as_before_decimal_text(&path, &id, current);

    // Verified under its own derivation: disclosed, and a replay of the same
    // event is the same row, still quarantined (it was bundle_mismatch).
    assert_eq!(bundles(&cell).expect("rechecks").len(), 1);
    assert_eq!(
        retain_in(&mut cell, "erpnext", &holding, "snapshot", "snapshot"),
        id
    );
    let outcome = cell.process_mapping_outcome(&id).expect("outcome");
    assert_eq!(
        outcome.codes(),
        vec!["mapping.runtime.invalid_record".to_owned()],
        "{outcome:?}"
    );
    assert!(
        cell.store()
            .read()
            .pending_observation(&id)
            .expect("read")
            .is_some()
    );

    // Only the explicit, governed path re-derives it, and it consumes the
    // original.
    let landed = cell.reprocess_quarantined_bundle(&id).expect("reprocess");
    assert_eq!(landed, norn_cell::MappingOutcome::Landed);
    assert!(
        cell.store()
            .read()
            .pending_observation(&id)
            .expect("read")
            .is_none()
    );
    let both = bundles(&cell).expect("both recheck");
    assert_eq!(both.len(), 2);
    assert_eq!(both[1].reprocesses.as_deref(), Some(id.as_str()));
    assert_eq!(
        cell.reprocess_quarantined_bundle(&id).expect("again"),
        norn_cell::MappingOutcome::AlreadyLanded
    );
    retain_in(&mut cell, "erpnext", &holding, "snapshot", "snapshot");
    assert_eq!(
        cell.process_mapping_outcome(&id).expect("replay"),
        norn_cell::MappingOutcome::AlreadyLanded
    );
}

/// Rewrite the captured inbox row `id` as a capture before
/// MAP-DECIMAL-TEXT-1 retained it: the derivation-1 refusal, in the
/// diagnostic shape of that time, and no derivation field.
fn retain_as_before_decimal_text(
    path: &Path,
    id: &str,
    current: norn_cell::mapping_bundle::MappingBundle,
) {
    let mut artifacts = fixture_artifacts();
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    let mapping = artifacts
        .iter()
        .find(|a| a.artifact_id == "erpnext.three.mapping")
        .expect("mapping");
    let compiled = norn_checker::mapping::compile_mapping_artifact(mapping, &artifacts, &[])
        .expect("compiles");
    let payload =
        serde_json::to_string(current.raw.get("payload").expect("payload")).expect("json");
    let document =
        norn_checker::mapping::SourceDocument::from_json("Warehouse Lot Balance", &payload, None)
            .expect("document");
    let diagnostics: Vec<RuntimeValue> = compiled
        .apply_derived(
            &[document],
            &current.input.recorded_at,
            norn_checker::mapping::Derivation::V1,
        )
        .expect_err("decimal text refused before MAP-DECIMAL-TEXT-1")
        .iter()
        .map(|refusal| norn_ir::runtime::encode(&refusal.diagnostic().to_json()).expect("encode"))
        .collect();
    let legacy = norn_cell::mapping_bundle::MappingBundle {
        records: Vec::new(),
        diagnostics,
        derivation: 1,
        ..current
    };
    let encoded =
        serde_json::to_value(norn_ir::runtime::encode(&legacy).expect("encode")).expect("json");
    assert!(encoded.get("derivation").is_none(), "{encoded}");
    let db = rusqlite::Connection::open(path).expect("open");
    let stored: String = db
        .query_row(
            "SELECT observed FROM _norn_inbox WHERE observation_id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("row");
    let mut observed: Value = serde_json::from_str(&stored).expect("observed");
    observed["input"] = encoded;
    db.execute(
        "UPDATE _norn_inbox SET observed = ?1 WHERE observation_id = ?2",
        rusqlite::params![observed.to_string(), id],
    )
    .expect("legacy row");
}

/// The cell, holding row and inbox id of one `ERPNext` holding whose quantity
/// is exact decimal text, retained but not processed. The main-era fixture
/// (`fixtures/mapping-bundle-main-decimal-text.json`) was captured by this
/// exact sequence on the merged main parent's code.
fn decimal_text_holding(path: &Path) -> (Cell, Value, String) {
    let mut cell = setup(path);
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows");
    for row in rows
        .iter()
        .filter(|row| row["dataset"] != "Warehouse Lot Balance")
    {
        capture(&mut cell, "erpnext", row, "snapshot");
    }
    let mut holding = rows
        .iter()
        .find(|row| row["dataset"] == "Warehouse Lot Balance")
        .expect("holding")
        .clone();
    holding["record"]["quantity"] = json!("200.50");
    holding["record"]["name"] = json!("SYN-ERPNEXT-main");
    let id = retain_in(&mut cell, "erpnext", &holding, "snapshot", "snapshot");
    (cell, holding, id)
}

/// S1 (#1488): main already captured exact decimal text in unversioned
/// bundles (MAPPING-DECIMAL-TEXT-1). Such a bundle, as main's code retained
/// it, still rechecks and lands: unversioned evidence verifies under either
/// historical derivation.
#[test]
fn an_unversioned_bundle_main_captured_with_decimal_text_rechecks_and_lands() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("main.sqlite");
    let (mut cell, _holding, id) = decimal_text_holding(&path);
    let main_bundle: Value = serde_json::from_str(include_str!(
        "fixtures/mapping-bundle-main-decimal-text.json"
    ))
    .expect("fixture");
    assert!(main_bundle.get("derivation").is_none(), "unversioned");
    let db = rusqlite::Connection::open(&path).expect("open");
    let stored: String = db
        .query_row(
            "SELECT observed FROM _norn_inbox WHERE observation_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .expect("row");
    let mut observed: Value = serde_json::from_str(&stored).expect("observed");
    observed["input"] = main_bundle;
    db.execute(
        "UPDATE _norn_inbox SET observed = ?1 WHERE observation_id = ?2",
        rusqlite::params![observed.to_string(), id],
    )
    .expect("main-era row");
    drop(db);

    let bundles = cell
        .store()
        .read()
        .source_event_mapping_bundles(
            "demo",
            "snapshot",
            "synthetic-erpnext",
            "SYN-ERPNEXT-main",
            "snapshot",
        )
        .expect("a bundle main retained rechecks");
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].derivation, 1);
    assert!(bundles[0].diagnostics.is_empty() && !bundles[0].records.is_empty());
    assert_eq!(
        cell.process_mapping_outcome(&id).expect("outcome"),
        norn_cell::MappingOutcome::Landed
    );
}
