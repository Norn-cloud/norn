//! Lane B5 (#1028, #1549, #1931): durable indexed link-claim frontier,
//! source `kind == "delete"` bundles, and governed `mapping.dispute.resolve`.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration suite for link-claim frontier, source deletes, and dispute resolution"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use norn_cell::engine::NativeEngine;
use norn_cell::engine_release::{EngineStamp, STAMP_FORMAT, STAMP_KEY, STORAGE_VERSION};
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::{
    CellError, CellStore, DisputeDecision, DisputeResolutionInput, MAIN_WORLD, MappingOutcome,
    SqliteCell, StoreOptions,
};
use norn_ir::{ArtifactEnvelope, RuntimeObject, RuntimeValue};
use norn_kernel::{FactKey, LinkKey, ObservationEvent, ObservationId, Observed};
use serde_json::{Value, json};

#[path = "support/mapped_cell.rs"]
mod mapped_cell;
use mapped_cell::root;

type Cell = SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>;

fn setup(path: &Path) -> Cell {
    setup_with(path, |_| {})
}

fn setup_with(path: &Path, edit: impl FnOnce(&mut Vec<ArtifactEnvelope>)) -> Cell {
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

fn raw_event(
    cell: &Cell,
    system: &str,
    row: &Value,
    event: &str,
    version: &str,
    kind: &str,
    partition: &str,
) -> (Observed<RuntimeValue>, MappingInput) {
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
    let delivery = cell.store().export_commit_count().expect("delivery");
    let raw = Observed {
        id: ObservationId(format!("delivery-{kind}-{event}-{delivery}")),
        provenance: norn_kernel::Provenance {
            source: source.clone(),
            source_id: alias.clone(),
            mapping_version: "synthetic/1".into(),
            observed_at: norn_kernel::LogicalTime::from_millis(
                1_793_702_400_000u64.saturating_add(delivery),
            ),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => source,
            "source_id" => alias,
            "event_id" => event,
            "source_version" => version,
            "pipeline_version" => "synthetic/1",
            "kind" => kind,
            "extracted_at" => 1_793_702_400_000u64,
            "payload" => payload,
            "payload_digest" => digest,
        },
    };
    let input = MappingInput {
        mapping: format!("{system}.three.mapping"),
        dataset: row["dataset"].as_str().expect("dataset").into(),
        connector_instance: "demo".into(),
        partition: partition.into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
    };
    (raw, input)
}

fn capture_kind(
    cell: &mut Cell,
    system: &str,
    row: &Value,
    event: &str,
    version: &str,
    kind: &str,
    partition: &str,
) -> String {
    let (raw, input) = raw_event(cell, system, row, event, version, kind, partition);
    let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("inject");
    cell.process_mapping_bundle(&id).expect("process");
    id
}

fn provider_targets(cell: &Cell) -> BTreeSet<String> {
    cell.store()
        .read()
        .links(
            MAIN_WORLD,
            "canon.OrderLine",
            "provider",
            "demo/order/1/line/1",
        )
        .expect("links")
        .into_iter()
        .map(|l| l.target)
        .collect()
}

fn seed_erpnext_snapshot(cell: &mut Cell) -> Vec<Value> {
    let source = fixture("erpnext");
    let rows = source["snapshot"].as_array().expect("rows").clone();
    for row in &rows {
        capture_kind(cell, "erpnext", row, "snapshot", "0", "upsert", "snapshot");
    }
    rows
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

fn referring_parties(artifacts: &mut Vec<ArtifactEnvelope>) {
    edit_artifact(artifacts, "canon.Party.entity", |a| {
        a["payload"]["objectType"]["links"] = json!([{
            "id": "refers",
            "target": "canon.Party",
            "cardinality": { "from": "many", "to": "many" }
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
            "key": "input.refers_ref",
            "link": "refers",
            "targetMap": "party"
        }]);
    });
}

fn party_ref_row(name: &str, key: &str, label: &str, refers: Option<&str>) -> Value {
    json!({
        "dataset": "Party",
        "record": {
            "name": name,
            "custom_norn_ref": key,
            "label": label,
            "role": "customer",
            "refers_ref": refers,
        }
    })
}

fn export_records_and_head(cell: &Cell) -> (Vec<norn_cell::export::ExportRecord>, String) {
    let records: Vec<_> = (1..=cell.store().export_commit_count().expect("head"))
        .map(|seq| {
            cell.store()
                .export_record(seq)
                .expect("record")
                .expect("present")
        })
        .collect();
    let head = records.last().expect("last").hash().expect("hash");
    (records, head)
}

fn refers_targets(cell: &Cell, source_key: &str) -> BTreeSet<String> {
    cell.store()
        .read()
        .links(MAIN_WORLD, "canon.Party", "refers", source_key)
        .expect("refers")
        .into_iter()
        .map(|l| l.target)
        .collect()
}

fn held_findings_for(
    cell: &Cell,
    object_type: &str,
    key: &str,
) -> Vec<norn_ir::DataQualityFinding> {
    cell.store()
        .read()
        .observation_findings(MAIN_WORLD, object_type, key)
        .expect("findings")
        .into_iter()
        .filter(|f| {
            matches!(
                &f.rule_ref,
                norn_ir::DataQualityRuleRef::Builtin { id, .. } if id == norn_cell::LINK_HELD_RULE
            )
        })
        .collect()
}

/// #1028: More than 1,000 link-bearing bundles on the SAME `(source, role)`
/// succeed without hitting the removed `MAX_CLAIM_BUNDLES = 1000` cap, keep
/// the active frontier O(1), and match the full-history fold.
#[test]
fn link_claim_frontier_scales_past_1000_monotonic_updates_on_one_source() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("scale.sqlite");
    let mut cell = setup(&path);
    // Disable wide-scope double-plan for the 1,050-commit loop; we verify
    // differential equivalence against the full history explicitly below.
    norn_cell::check_link_plans_against_wide_scope(false);
    let rows = seed_erpnext_snapshot(&mut cell);
    let line = rows[6].clone();

    let total_updates = 1_050_u32;
    for step in 1..=total_updates {
        let mut updated = line.clone();
        let target = if step % 2 == 1 {
            "demo/party/custodian"
        } else {
            "demo/party/customer"
        };
        updated["record"]["provider_ref"] = json!(target);
        let ver = format!("{step:06}");
        capture_kind(
            &mut cell, "erpnext", &updated, &ver, &ver, "upsert", "snapshot",
        );
    }

    assert_eq!(
        provider_targets(&cell),
        BTreeSet::from(["demo/party/customer".to_owned()])
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("active frontier equals full-history fold");

    // Active frontier for `demo/order/1/line/1` + `provider` has O(1) active row,
    // and the latest claim's `supersedes` has size 1 (not 1,050).
    let conn = rusqlite::Connection::open(&path).expect("sqlite");
    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM \"_norn_link_claim_frontier\" \
             WHERE \"source_key\" = 'demo/order/1/line/1' AND \"role\" = 'provider' AND \"active\" = 1",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(
        active_count, 1,
        "monotonic updates keep O(1) active frontier"
    );

    let latest_json: String = conn
        .query_row(
            "SELECT \"claim_json\" FROM \"_norn_link_claim_frontier\" \
             WHERE \"source_key\" = 'demo/order/1/line/1' AND \"role\" = 'provider' AND \"active\" = 1",
            [],
            |row| row.get(0),
        )
        .expect("latest claim");
    let latest: norn_ir::mapping::ObservedLinkClaim =
        serde_json::from_str(&latest_json).expect("decode claim");
    assert_eq!(
        latest.supersedes.len(),
        1,
        "monotonic frontier claim supersedes only the immediate active predecessor"
    );
}

/// #1028: v12 -> v13 upgrade backfills `_norn_link_claim_frontier` idempotently
/// from immutable `_norn_fact_values`, and `rebuild_export_history` reproduces
/// identical frontier state.
#[test]
fn link_claim_frontier_upgrades_from_v12_and_rebuilds_from_export() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("upgrade_v12.sqlite");
    let mut cell = setup(&path);
    let rows = seed_erpnext_snapshot(&mut cell);
    let mut updated = rows[6].clone();
    updated["record"]["provider_ref"] = json!("demo/party/custodian");
    capture_kind(
        &mut cell, "erpnext", &updated, "0001", "0001", "upsert", "snapshot",
    );
    let claims_before = cell.store().read().observed_link_claims().expect("claims");
    let (export_records, export_head) = export_records_and_head(&cell);
    let model = cell.store().model().clone();
    drop(cell);

    // Simulate a pre-v13 (storage version 12) cell: drop v13 tables and stamp v12.
    {
        let conn = rusqlite::Connection::open(&path).expect("raw sqlite");
        conn.execute_batch(
            "DROP TABLE \"_norn_link_claim_frontier\";
             DROP TABLE \"_norn_dispute_resolution\";",
        )
        .expect("drop v13 tables");
        let v12_stamp = serde_json::to_string(&EngineStamp {
            format: STAMP_FORMAT.to_owned(),
            storage: 12,
            writer: "v12-sim".to_owned(),
        })
        .expect("stamp");
        conn.execute(
            "UPDATE \"_norn_meta\" SET \"value\" = ?1 WHERE \"key\" = ?2",
            rusqlite::params![v12_stamp, STAMP_KEY],
        )
        .expect("stamp v12");
    }

    // Reopen under current engine (STORAGE_VERSION == 13): upgrades and backfills.
    let reopened =
        CellStore::open(&path, model, StoreOptions::default()).expect("upgrade v12->v13");
    assert_eq!(STORAGE_VERSION, 13);
    reopened
        .read()
        .verify_frontier_equivalence()
        .expect("backfilled frontier equals full history");
    assert_eq!(
        reopened.read().observed_link_claims().expect("claims"),
        claims_before
    );
    let mut upgraded_cell =
        SqliteCell::new(reopened, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    assert_eq!(
        provider_targets(&upgraded_cell),
        BTreeSet::from(["demo/party/custodian".to_owned()])
    );
    // Subsequent update works cleanly after upgrade.
    let mut next_row = rows[6].clone();
    next_row["record"]["provider_ref"] = json!("demo/party/customer");
    capture_kind(
        &mut upgraded_cell,
        "erpnext",
        &next_row,
        "0002",
        "0002",
        "upsert",
        "snapshot",
    );
    assert_eq!(
        provider_targets(&upgraded_cell),
        BTreeSet::from(["demo/party/customer".to_owned()])
    );
    upgraded_cell
        .store()
        .read()
        .verify_frontier_equivalence()
        .expect("post-upgrade update preserves frontier equivalence");

    // Export replay reproduces identical frontier state.
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("mem"),
        &export_records,
        &export_head,
    )
    .expect("rebuild");
    rebuilt
        .read()
        .verify_frontier_equivalence()
        .expect("rebuilt frontier equals full history");
    assert_eq!(
        rebuilt.read().observed_link_claims().expect("claims"),
        claims_before
    );
}

/// #1028 / #1931: `kind == "delete"` tombstones the subject (or transitions
/// `onDelete`), emits superseding withdrawal claims (`target: None`), removes
/// sole-supported edges, preserves multi-source edges until the last source
/// deletes, and preserves `OBS-PAGE-EQUIV`.
#[test]
fn source_delete_tombstones_and_withdraws_only_its_own_link_claims() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(&dir.path().join("delete.sqlite"), referring_parties);

    let target_a = party_ref_row("P-TARGET", "demo/party/target", "Target", None);
    let src_p1 = party_ref_row(
        "P-SRC",
        "demo/party/src",
        "Source",
        Some("demo/party/target"),
    );
    let src_p2 = party_ref_row(
        "P-SRC",
        "demo/party/src",
        "Source",
        Some("demo/party/target"),
    );

    capture_kind(&mut cell, "erpnext", &target_a, "e0", "0", "upsert", "p1");
    // Two independent source partitions support the same edge `src -refers-> target`.
    capture_kind(&mut cell, "erpnext", &src_p1, "e1", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &src_p2, "e1", "1", "upsert", "p2");
    assert_eq!(
        refers_targets(&cell, "demo/party/src"),
        BTreeSet::from(["demo/party/target".to_owned()])
    );

    // Partition p1 sends `kind: "delete"` -> p1's claim is withdrawn, tombstone
    // is recorded, and `src -refers-> target` STILL stands because p2 supports it.
    capture_kind(&mut cell, "erpnext", &src_p1, "e2", "2", "delete", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/src"),
        BTreeSet::from(["demo/party/target".to_owned()]),
        "second undisputed source partition keeps the edge alive"
    );
    let tombstone = norn_kernel::snapshot_observe::tombstone_key(&FactKey::new(
        "canon.Party",
        "demo/party/src",
    ));
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, tombstone.object_type.as_str(), &tombstone.key)
            .expect("tombstone read")
            .is_some(),
        "source delete records norn.ObservedTombstone"
    );
    assert!(
        cell.store()
            .read()
            .ledger(0)
            .expect("ledger")
            .iter()
            .any(|e| e.event.get("op").and_then(RuntimeValue::as_str) == Some("observed_deleted")),
        "ledger records ObservationEvent::ObservedDeleted"
    );

    // Partition p2 also deletes -> no standing claim remains -> edge removed!
    capture_kind(&mut cell, "erpnext", &src_p2, "e2", "2", "delete", "p2");
    assert!(
        refers_targets(&cell, "demo/party/src").is_empty(),
        "deleting the final supporting source removes the edge"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after deletes");
}

/// #1549: Governed `mapping.dispute.resolve` with `accept-first` releases held
/// edges, unquarantines the subject, preserves immutable conflict evidence,
/// records `_norn_dispute_resolution` + receipt, resists redelivery reopening,
/// refuses double-resolve, and replays identically from export.
#[test]
fn dispute_resolve_accept_first_releases_held_edge_and_replays() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(&dir.path().join("accept_first.sqlite"), referring_parties);

    let src_v1 = party_ref_row("P-A", "demo/party/a", "A v1", Some("demo/party/b"));
    let src_v1_conflict = party_ref_row("P-A", "demo/party/a", "A CONFLICT", Some("demo/party/c"));
    let target_b = party_ref_row("P-B", "demo/party/b", "B", None);
    let target_c = party_ref_row("P-C", "demo/party/c", "C", None);

    // Capture and process A (target B absent -> pending link claim).
    capture_kind(&mut cell, "erpnext", &src_v1, "e1", "1", "upsert", "p1");
    // Conflicting delivery for the same event_id `e1` creates `mapping-conflict:...`.
    let (raw_conflict, input_conflict) = raw_event(
        &cell,
        "erpnext",
        &src_v1_conflict,
        "e1",
        "1",
        "upsert",
        "p1",
    );
    let (conflict_id, inserted) = cell
        .inject_mapping_bundle(&raw_conflict, &input_conflict)
        .expect("capture conflict");
    assert!(inserted && conflict_id.starts_with("mapping-conflict:"));
    // Target B and C now arrive while A is disputed -> edge A -> B is HELD!
    capture_kind(&mut cell, "erpnext", &target_b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &target_c, "ec", "1", "upsert", "p1");
    assert!(refers_targets(&cell, "demo/party/a").is_empty());
    assert_eq!(
        held_findings_for(&cell, "canon.Party", "demo/party/a").len(),
        1
    );
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        1
    );

    // Blank actor or reason must fail closed.
    assert!(
        cell.resolve_mapping_dispute(DisputeResolutionInput {
            dispute_id: conflict_id.clone(),
            decision: DisputeDecision::AcceptFirst,
            actor: "   ".into(),
            reason: "valid".into(),
        })
        .is_err()
    );

    // Resolve with `accept-first`.
    let outcome = cell
        .resolve_mapping_dispute(DisputeResolutionInput {
            dispute_id: conflict_id.clone(),
            decision: DisputeDecision::AcceptFirst,
            actor: "ops:alice".into(),
            reason: "first delivery matches ERP audit log".into(),
        })
        .expect("resolve accept-first");
    assert_eq!(outcome.decision, DisputeDecision::AcceptFirst);

    // Held finding disappears, withheld edge A -> B appears, subject unquarantined!
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );
    assert!(held_findings_for(&cell, "canon.Party", "demo/party/a").is_empty());
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        0
    );

    // Immutable conflict evidence preserved + resolution audit record present.
    let retained = cell
        .store()
        .read()
        .retained_source_conflicts()
        .expect("retained");
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].2, Some(outcome.commit_seq));
    let res = cell
        .store()
        .read()
        .dispute_resolution(&conflict_id)
        .expect("resolution")
        .expect("recorded");
    assert_eq!(res.actor, "ops:alice");
    assert_eq!(res.decision, "accept-first");
    assert_eq!(res.receipt_hash, outcome.receipt_hash);

    // Double-resolve fails closed (`NotPending`).
    assert!(matches!(
        cell.resolve_mapping_dispute(DisputeResolutionInput {
            dispute_id: conflict_id.clone(),
            decision: DisputeDecision::AcceptFirst,
            actor: "ops:alice".into(),
            reason: "repeat".into(),
        }),
        Err(CellError::NotPending(_))
    ));

    // Redelivering the exact same conflicting payload does NOT reopen the dispute.
    let (redeliv_id, redeliv_inserted) = cell
        .inject_mapping_bundle(&raw_conflict, &input_conflict)
        .expect("redeliver conflict");
    assert_eq!(redeliv_id, conflict_id);
    assert!(!redeliv_inserted);
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        0
    );

    // Delivering a NEW distinct conflicting payload opens a fresh dispute.
    let src_v1_conflict2 =
        party_ref_row("P-A", "demo/party/a", "A CONFLICT 2", Some("demo/party/c"));
    let (raw_c2, input_c2) = raw_event(
        &cell,
        "erpnext",
        &src_v1_conflict2,
        "e1",
        "1",
        "upsert",
        "p1",
    );
    let (conflict2_id, inserted2) = cell.inject_mapping_bundle(&raw_c2, &input_c2).expect("c2");
    assert!(inserted2 && conflict2_id != conflict_id);
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        1
    );

    // Export/restore carries the first resolution cleanly.
    let (records, head) = export_records_and_head(&cell);
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("mem"),
        &records,
        &head,
    )
    .expect("rebuild");
    assert_eq!(
        rebuilt.read().dispute_resolutions().expect("resolutions"),
        vec![res]
    );
    rebuilt
        .read()
        .verify_frontier_equivalence()
        .expect("rebuilt frontier equivalence");
}

/// #1549: `accept-second-as-correction` applies candidate facts & link claims,
/// removing the old-only edge and adding the candidate's edge; `reject`
/// removes rejected-only edges while preserving multi-source undisputed edges.
#[test]
fn dispute_resolve_accept_second_and_reject_update_facts_and_edges() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("second_and_reject.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &c, "ec", "1", "upsert", "p1");

    // 1. Test `accept-second-as-correction`: A initially points to B, then
    //    conflicting e1 points to C with updated label.
    let a_first = party_ref_row("P-A", "demo/party/a", "Label Old", Some("demo/party/b"));
    let a_second = party_ref_row(
        "P-A",
        "demo/party/a",
        "Label Corrected",
        Some("demo/party/c"),
    );
    capture_kind(&mut cell, "erpnext", &a_first, "e1", "1", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );
    let (raw_second, input_second) =
        raw_event(&cell, "erpnext", &a_second, "e1", "1", "upsert", "p1");
    let (conflict_a, inserted) = cell
        .inject_mapping_bundle(&raw_second, &input_second)
        .expect("conflict a");
    assert!(inserted && conflict_a.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_a,
        decision: DisputeDecision::AcceptSecondAsCorrection,
        actor: "ops:bob".into(),
        reason: "corrected source payload".into(),
    })
    .expect("resolve accept-second-as-correction");

    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/c".to_owned()]),
        "old-only edge removed and candidate edge added"
    );
    let party_a = cell
        .store()
        .read()
        .object(MAIN_WORLD, "canon.Party", "demo/party/a")
        .expect("read a")
        .expect("exists");
    assert_eq!(
        party_a
            .values
            .get("display_name")
            .and_then(RuntimeValue::as_str),
        Some("Label Corrected")
    );

    // 2. Test `reject`: D has edge D -> B from BOTH p1 and p2, and edge D -> C
    //    ONLY from p1's disputed event `ed_only`. Rejecting p1's conflict removes
    //    D -> C while preserving D -> B (supported by undisputed p2).
    let d_p1_b = party_ref_row("P-D", "demo/party/d", "D", Some("demo/party/b"));
    let d_p2_b = party_ref_row("P-D", "demo/party/d", "D", Some("demo/party/b"));
    let d_p1_c = party_ref_row("P-D-EXTRA", "demo/party/d", "D", Some("demo/party/c"));
    let d_p1_c_bad = party_ref_row("P-D-EXTRA", "demo/party/d", "D BAD", Some("demo/party/b"));
    capture_kind(&mut cell, "erpnext", &d_p1_b, "ed1", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &d_p2_b, "ed1", "1", "upsert", "p2");
    capture_kind(
        &mut cell, "erpnext", &d_p1_c, "ed_only", "1", "upsert", "p1",
    );
    assert_eq!(
        refers_targets(&cell, "demo/party/d"),
        BTreeSet::from(["demo/party/b".to_owned(), "demo/party/c".to_owned()])
    );
    let (raw_bad, input_bad) = raw_event(
        &cell,
        "erpnext",
        &d_p1_c_bad,
        "ed_only",
        "1",
        "upsert",
        "p1",
    );
    let (conflict_d, inserted_d) = cell
        .inject_mapping_bundle(&raw_bad, &input_bad)
        .expect("conflict d");
    assert!(inserted_d && conflict_d.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_d,
        decision: DisputeDecision::Reject,
        actor: "ops:carol".into(),
        reason: "bogus duplicate event".into(),
    })
    .expect("resolve reject");

    assert_eq!(
        refers_targets(&cell, "demo/party/d"),
        BTreeSet::from(["demo/party/b".to_owned()]),
        "rejected-only edge removed; multi-source undisputed edge preserved"
    );
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        0
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after accept-second and reject");

    // Verify provenance for the corrected link A -> C includes the resolution claim.
    let prov = cell
        .store()
        .read()
        .observed_link_provenance(&LinkKey::new(
            FactKey::new("canon.Party", "demo/party/a"),
            "refers",
            FactKey::new("canon.Party", "demo/party/c"),
        ))
        .expect("provenance");
    assert_eq!(prov.len(), 1);
    assert!(prov[0].observation_id.starts_with("mapping-conflict:"));
}

/// #1549 + MAP-DERIVATION-1: legacy `derivation == 2` quarantined delete
/// bundles still recheck under their own derivation and can be reprocessed
/// into derivation 3 via `reprocess_quarantined_bundle`.
#[test]
fn legacy_derivation_2_delete_bundle_rechecks_and_reprocesses_to_derivation_3() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("legacy_del.sqlite");
    let mut cell = setup_with(&path, referring_parties);

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let a = party_ref_row("P-A", "demo/party/a", "A", Some("demo/party/b"));
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a, "ea", "1", "upsert", "p1");

    // Capture a fresh delete (derivation 3), then rewrite it in-place as a
    // pre-B5 derivation 2 quarantined delete bundle to test MAP-DERIVATION-1.
    let (raw_del, input_del) = raw_event(&cell, "erpnext", &a, "ea-del", "2", "delete", "p1");
    let (del_id, _) = cell
        .inject_mapping_bundle(&raw_del, &input_del)
        .expect("capture delete");
    {
        let conn = rusqlite::Connection::open(&path).expect("sqlite");
        let text: String = conn
            .query_row(
                "SELECT observed FROM _norn_inbox WHERE observation_id = ?1",
                [&del_id],
                |row| row.get(0),
            )
            .expect("read inbox");
        let mut obs: Value = serde_json::from_str(&text).expect("json");
        obs["input"]["derivation"] = json!(2);
        obs["input"].as_object_mut().expect("obj").remove("deletes");
        obs["input"]["diagnostics"] = json!([{
            "code": norn_checker::mapping::codes::EVALUATION_FAILED,
            "message": "CP2 mapping bundle does not apply source deletes"
        }]);
        conn.execute(
            "UPDATE _norn_inbox SET observed = ?1 WHERE observation_id = ?2",
            rusqlite::params![serde_json::to_string(&obs).expect("enc"), &del_id],
        )
        .expect("rewrite v2 delete");
    }

    // Legacy v2 delete bundle stays quarantined when processed directly.
    assert!(matches!(
        cell.process_mapping_outcome(&del_id).expect("outcome"),
        MappingOutcome::Quarantined(_)
    ));
    // Reprocessing promotes it under derivation 3, withdrawing A -> B!
    assert_eq!(
        cell.reprocess_quarantined_bundle(&del_id)
            .expect("reprocess"),
        MappingOutcome::Landed
    );
    assert!(refers_targets(&cell, "demo/party/a").is_empty());
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after reprocessed delete");
}

/// #1549: Identity dispute (`mapping-identity:...`) resolved via
/// `mapping.dispute.resolve` unquarantines the subject, preserves the retained
/// identity dispute row, and resists double-resolve.
#[test]
fn dispute_resolve_handles_identity_dispute_and_unquarantines() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("identity_resolve.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let a_nfc = party_ref_row(
        "P-CAF\u{00C9}",
        "demo/party/cafe",
        "Cafe",
        Some("demo/party/b"),
    );
    let a_nfd = party_ref_row(
        "P-CAFE\u{0301}",
        "demo/party/cafe",
        "Cafe NFD",
        Some("demo/party/b"),
    );
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_nfc, "ec1", "1", "upsert", "p1");
    let (raw_nfd, input_nfd) = raw_event(&cell, "erpnext", &a_nfd, "ec1", "1", "upsert", "p1");
    let (identity_id, inserted) = cell
        .inject_mapping_bundle(&raw_nfd, &input_nfd)
        .expect("capture identity dispute");
    assert!(inserted && identity_id.starts_with("mapping-identity:"));
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        1
    );

    let outcome = cell
        .resolve_mapping_dispute(DisputeResolutionInput {
            dispute_id: identity_id.clone(),
            decision: DisputeDecision::AcceptFirst,
            actor: "ops:dana".into(),
            reason: "NFC spelling is canonical".into(),
        })
        .expect("resolve identity dispute");
    assert_eq!(outcome.decision, DisputeDecision::AcceptFirst);
    assert_eq!(
        cell.store()
            .read()
            .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
            .expect("page")
            .quarantine_count,
        0
    );
    assert_eq!(
        refers_targets(&cell, "demo/party/cafe"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );
    assert!(
        cell.store()
            .read()
            .dispute_resolution(&identity_id)
            .expect("resolution")
            .is_some()
    );
}

/// #1028 / OBS-PAGE-EQUIV: `kind == "delete"` delivered via `mapping-page:`
/// produces the same end state, links, and tombstones as one-by-one delivery.
#[test]
fn source_delete_page_delivery_equals_one_by_one() {
    let dir = tempfile::tempdir().expect("dir");
    let mut single = setup_with(&dir.path().join("single.sqlite"), referring_parties);
    let mut paged = setup_with(&dir.path().join("paged.sqlite"), referring_parties);

    let target = party_ref_row("P-T", "demo/party/t", "Target", None);
    let src1 = party_ref_row("P-1", "demo/party/1", "One", Some("demo/party/t"));
    let src2 = party_ref_row("P-2", "demo/party/2", "Two", Some("demo/party/t"));

    for cell in [&mut single, &mut paged] {
        capture_kind(cell, "erpnext", &target, "e0", "0", "upsert", "p1");
        capture_kind(cell, "erpnext", &src1, "e1", "1", "upsert", "p1");
        capture_kind(cell, "erpnext", &src2, "e1", "1", "upsert", "p1");
    }

    // One-by-one deletes on `single`.
    capture_kind(&mut single, "erpnext", &src1, "e2", "2", "delete", "p1");
    capture_kind(&mut single, "erpnext", &src2, "e2", "2", "delete", "p1");

    // Paged deletes on `paged`.
    let (r1, input) = raw_event(&paged, "erpnext", &src1, "e2", "2", "delete", "p1");
    let (r2, _) = raw_event(&paged, "erpnext", &src2, "e2", "2", "delete", "p1");
    let captured = paged
        .inject_mapping_page(&[r1, r2], &input)
        .expect("inject delete page");
    assert_eq!(captured.pages.len(), 1);
    let outcomes = paged
        .process_mapping_page(&captured.pages[0].0)
        .expect("process delete page")
        .expect("pending");
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|(_, o)| *o == MappingOutcome::Landed));

    assert_eq!(
        refers_targets(&single, "demo/party/1"),
        refers_targets(&paged, "demo/party/1")
    );
    assert_eq!(
        refers_targets(&single, "demo/party/2"),
        refers_targets(&paged, "demo/party/2")
    );
    assert!(refers_targets(&paged, "demo/party/1").is_empty());
    assert!(refers_targets(&paged, "demo/party/2").is_empty());
    assert_eq!(
        single
            .store()
            .read()
            .observed_link_claims()
            .expect("single claims"),
        paged
            .store()
            .read()
            .observed_link_claims()
            .expect("paged claims"),
    );
    paged
        .store()
        .read()
        .verify_frontier_equivalence()
        .expect("paged delete frontier equivalence");
}

/// #1028 / #1931: Recreation after delete lifts `norn.ObservedTombstone` and
/// emits `ObservationEvent::ObservedRecreated`; multi-source `to: one` link
/// withdrawal converges cleanly when another source still asserts the target.
#[test]
fn source_delete_recreation_lifts_tombstone_and_to_one_delete_converges() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup(&dir.path().join("recreate_to_one.sqlite"));
    let rows = seed_erpnext_snapshot(&mut cell);
    let mut line = rows[6].clone();
    line["record"]["provider_ref"] = json!("demo/party/custodian");

    // Two partitions assert the same `to: one` `provider` edge.
    capture_kind(&mut cell, "erpnext", &line, "e1", "1", "upsert", "snapshot");
    capture_kind(&mut cell, "erpnext", &line, "e1", "1", "upsert", "p2");
    assert_eq!(
        provider_targets(&cell),
        BTreeSet::from(["demo/party/custodian".to_owned()])
    );

    // Partition `snapshot` deletes -> tombstones OrderLine and withdraws its
    // `to: one` claim while `p2` still supports `demo/party/custodian`.
    capture_kind(&mut cell, "erpnext", &line, "e2", "2", "delete", "snapshot");
    assert_eq!(
        provider_targets(&cell),
        BTreeSet::from(["demo/party/custodian".to_owned()]),
        "to:one withdrawal does not refuse while another source supports the sole target"
    );
    let tombstone = norn_kernel::snapshot_observe::tombstone_key(&FactKey::new(
        "canon.OrderLine",
        "demo/order/1/line/1",
    ));
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, tombstone.object_type.as_str(), &tombstone.key)
            .expect("tombstone")
            .is_some()
    );

    // A later upsert recreates the tombstoned object and lifts its tombstone!
    capture_kind(&mut cell, "erpnext", &line, "e3", "3", "upsert", "snapshot");
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, tombstone.object_type.as_str(), &tombstone.key)
            .expect("tombstone lifted")
            .is_none(),
        "recreation upsert lifts norn.ObservedTombstone"
    );
    assert!(
        cell.store()
            .read()
            .ledger(0)
            .expect("ledger")
            .iter()
            .any(|e| e.event.get("op").and_then(RuntimeValue::as_str) == Some("observed_recreated")),
        "recreation emits ObservationEvent::ObservedRecreated"
    );
}

/// #1549: If the first capture is still pending when a conflict is resolved
/// with `accept-second-as-correction` or `reject`, the resolution atomically
/// consumes that pending first capture so it cannot later land and undo it.
#[test]
fn dispute_resolve_consumes_pending_first_capture_and_prevents_undo() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(&dir.path().join("pending_first.sqlite"), referring_parties);

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &c, "ec", "1", "upsert", "p1");

    let a_first = party_ref_row("P-A", "demo/party/a", "First Pending", Some("demo/party/b"));
    let a_second = party_ref_row(
        "P-A",
        "demo/party/a",
        "Second Authoritative",
        Some("demo/party/c"),
    );

    // Capture first bundle WITHOUT processing it yet, then capture conflict.
    let (raw1, input1) = raw_event(&cell, "erpnext", &a_first, "e1", "1", "upsert", "p1");
    let (first_id, _) = cell
        .inject_mapping_bundle(&raw1, &input1)
        .expect("inject 1");
    let (raw2, input2) = raw_event(&cell, "erpnext", &a_second, "e1", "1", "upsert", "p1");
    let (conflict_id, inserted) = cell
        .inject_mapping_bundle(&raw2, &input2)
        .expect("inject 2");
    assert!(inserted && conflict_id.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id,
        decision: DisputeDecision::AcceptSecondAsCorrection,
        actor: "ops:erin".into(),
        reason: "second supersedes unlanded first".into(),
    })
    .expect("resolve");

    // Attempting to process the original pending bundle is a no-op (`Ok(false)`),
    // leaving the correction intact!
    assert!(!cell.process_mapping_bundle(&first_id).expect("no-op"));
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/c".to_owned()])
    );
    let party_a = cell
        .store()
        .read()
        .object(MAIN_WORLD, "canon.Party", "demo/party/a")
        .expect("read a")
        .expect("exists");
    assert_eq!(
        party_a
            .values
            .get("display_name")
            .and_then(RuntimeValue::as_str),
        Some("Second Authoritative")
    );

    // Export replay reproduces the same consumed state cleanly.
    let (records, head) = export_records_and_head(&cell);
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("mem"),
        &records,
        &head,
    )
    .expect("rebuild");
    assert!(
        rebuilt
            .read()
            .pending_observation(&first_id)
            .expect("pending")
            .is_none()
    );
}

/// #1028 / OBS-PAGE-EQUIV: A page combining a target's arrival with a delete
/// of a source waiting on that target splits runs so link CAS versions match
/// one-by-one delivery (`0 -> 1 -> 2`), and tampering `_norn_link_claim_frontier`
/// is detected and healed on reopen from immutable `_norn_fact_values`.
#[test]
fn obs_page_equiv_target_arrival_then_source_delete_and_frontier_heal_on_open() {
    let dir = tempfile::tempdir().expect("dir");
    let single_path = dir.path().join("single_td.sqlite");
    let paged_path = dir.path().join("paged_td.sqlite");
    let mut single = setup_with(&single_path, referring_parties);
    let mut paged = setup_with(&paged_path, referring_parties);

    // Source S arrives first with pending claim `S -refers-> T` (T absent).
    let src = party_ref_row("P-S", "demo/party/s", "Source", Some("demo/party/t"));
    let tgt = party_ref_row("P-T", "demo/party/t", "Target", None);
    for cell in [&mut single, &mut paged] {
        capture_kind(cell, "erpnext", &src, "e1", "1", "upsert", "p1");
    }

    // One-by-one: target arrives (adds link v1), then source deletes (removes link v2).
    capture_kind(&mut single, "erpnext", &tgt, "et", "1", "upsert", "p1");
    capture_kind(&mut single, "erpnext", &src, "e2", "2", "delete", "p1");

    // Paged: both delivered in one `inject_mapping_page` call -> splits into 2 runs!
    let (rt, input) = raw_event(&paged, "erpnext", &tgt, "et", "1", "upsert", "p1");
    let (rd, _) = raw_event(&paged, "erpnext", &src, "e2", "2", "delete", "p1");
    let captured = paged
        .inject_mapping_page(&[rt, rd], &input)
        .expect("inject page");
    assert_eq!(
        captured.pages.len(),
        2,
        "upsert and delete split into separate runs"
    );
    for (page_id, _) in &captured.pages {
        paged.process_mapping_page(page_id).expect("process run");
    }

    let single_v = single
        .store()
        .read()
        .link_version(
            MAIN_WORLD,
            "canon.Party",
            "refers",
            "demo/party/s",
            "canon.Party",
            "demo/party/t",
        )
        .expect("single link version");
    let paged_v = paged
        .store()
        .read()
        .link_version(
            MAIN_WORLD,
            "canon.Party",
            "refers",
            "demo/party/s",
            "canon.Party",
            "demo/party/t",
        )
        .expect("paged link version");
    assert_eq!(single_v, 2);
    assert_eq!(
        paged_v, single_v,
        "OBS-PAGE-EQUIV preserves exact link CAS versions"
    );

    // Simulate tampered/deleted frontier row on a current-version (v13) cell.
    let model = paged.store().model().clone();
    drop(paged);
    {
        let conn = rusqlite::Connection::open(&paged_path).expect("raw sqlite");
        conn.execute("DELETE FROM \"_norn_link_claim_frontier\"", [])
            .expect("wipe frontier");
    }
    let healed =
        CellStore::open(&paged_path, model, StoreOptions::default()).expect("reopen heals");
    healed
        .read()
        .verify_frontier_equivalence()
        .expect("reopen detects drift and rebuilds frontier from immutable _norn_fact_values");
    assert_eq!(
        healed.read().observed_link_claims().expect("claims").len(),
        single
            .store()
            .read()
            .observed_link_claims()
            .expect("claims")
            .len(),
    );
}

/// #1549: Rejecting a late contested disputed v2 claim in a contribution that
/// already has an undisputed current v3 claim must supersede ONLY v2 and keep
/// v3's edge alive and Current.
#[test]
fn dispute_resolve_reject_preserves_undisputed_newer_claim_in_same_contribution() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(&dir.path().join("reject_late_v2.sqlite"), referring_parties);

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    let a_v3 = party_ref_row("P-A", "demo/party/a", "A v3", Some("demo/party/b"));
    let a_v2 = party_ref_row("P-A", "demo/party/a", "A v2", Some("demo/party/c"));
    let a_v2_conflict = party_ref_row("P-A", "demo/party/a", "A v2 BAD", Some("demo/party/c"));

    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &c, "ec", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_v3, "e3", "3", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );
    capture_kind(&mut cell, "erpnext", &a_v2, "e2", "2", "upsert", "p1");
    let (raw_bad, input_bad) =
        raw_event(&cell, "erpnext", &a_v2_conflict, "e2", "2", "upsert", "p1");
    let (conflict_id, inserted) = cell
        .inject_mapping_bundle(&raw_bad, &input_bad)
        .expect("conflict v2");
    assert!(inserted && conflict_id.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id,
        decision: DisputeDecision::Reject,
        actor: "ops:frank".into(),
        reason: "stale late event".into(),
    })
    .expect("reject v2");

    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()]),
        "rejecting late disputed v2 must not remove undisputed current v3 edge"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after late-v2 reject");
}

/// #1028 / #1549: In-place mutation of `_norn_link_claim_frontier.active` or
/// `_norn_dispute_resolution.actor` is caught by `verify_frontier_equivalence()`
/// and automatically healed from immutable `_norn_fact_values` and `_norn_ledger`
/// on reopen; public `CellHost::commit_observed` cannot forge a resolution.
#[test]
fn in_place_frontier_and_audit_tamper_heals_on_open_and_commit_observed_cannot_forge() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("tamper_heal.sqlite");
    let mut cell = setup_with(&path, referring_parties);

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let a1 = party_ref_row("P-A", "demo/party/a", "A1", Some("demo/party/b"));
    let a2 = party_ref_row("P-A", "demo/party/a", "A2", Some("demo/party/b"));
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a1, "e1", "1", "upsert", "p1");
    let (raw2, input2) = raw_event(&cell, "erpnext", &a2, "e1", "1", "upsert", "p1");
    let (conflict_id, _) = cell
        .inject_mapping_bundle(&raw2, &input2)
        .expect("conflict");

    // Public `CellHost::commit_observed` must fail closed on a forged dispute consumption.
    let mut forged =
        norn_kernel::CellCommit::<ObservationEvent, RuntimeObject, RuntimeObject>::empty()
            .consuming(norn_kernel::Consumed::Observation {
                id: ObservationId(conflict_id.clone()),
            });
    forged.events.push(ObservationEvent::DisputeResolved {
        dispute_id: conflict_id.clone(),
        decision: "accept-first".into(),
        actor: "forger".into(),
        reason: "forged".into(),
    });
    assert!(
        norn_kernel::CellHost::commit_observed(
            &mut cell,
            forged,
            norn_kernel::ObservationAuthorization::empty(),
        )
        .is_err(),
        "public CellHost::commit_observed cannot forge dispute resolution"
    );

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id.clone(),
        decision: DisputeDecision::AcceptFirst,
        actor: "ops:grace".into(),
        reason: "verified".into(),
    })
    .expect("resolve");
    let model = cell.store().model().clone();
    drop(cell);

    {
        let conn = rusqlite::Connection::open(&path).expect("sqlite");
        conn.execute(
            "UPDATE \"_norn_link_claim_frontier\" SET \"active\" = 0",
            [],
        )
        .expect("corrupt active");
        conn.execute(
            "UPDATE \"_norn_dispute_resolution\" SET \"actor\" = 'tampered'",
            [],
        )
        .expect("corrupt actor");
    }

    let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopen heals");
    reopened
        .read()
        .verify_frontier_equivalence()
        .expect("healed frontier matches immutable fact values");
    let res = reopened
        .read()
        .dispute_resolution(&conflict_id)
        .expect("res")
        .expect("present");
    assert_eq!(
        res.actor, "ops:grace",
        "audit actor restored from immutable ledger"
    );
}

/// #1549 / OBS-LINK-DISPUTE-1: Resolving one of two pending conflicts for the
/// same event with `accept-second-as-correction` must keep the link frozen while
/// the sibling dispute remains open; decomposed Unicode actor/reason must match
/// the canonical NFC ledger entry with zero drift; negative `commit_seq` in the
/// audit index heals on reopen rather than aborting open.
#[test]
fn dispute_resolve_sibling_conflict_freezes_fresh_claim_and_nfc_audit_heals() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("sibling_nfc.sqlite");
    let mut cell = setup_with(&path, referring_parties);

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    let d = party_ref_row("P-D", "demo/party/d", "D", None);
    for target in [&b, &c, &d] {
        capture_kind(&mut cell, "erpnext", target, "et", "1", "upsert", "p1");
    }

    let a1 = party_ref_row("P-A", "demo/party/a", "A1", Some("demo/party/b"));
    let a2 = party_ref_row("P-A", "demo/party/a", "A2", Some("demo/party/c"));
    let a3 = party_ref_row("P-A", "demo/party/a", "A3", Some("demo/party/d"));
    capture_kind(&mut cell, "erpnext", &a1, "e1", "1", "upsert", "p1");
    let (raw2, input2) = raw_event(&cell, "erpnext", &a2, "e1", "1", "upsert", "p1");
    let (c1, ins1) = cell
        .inject_mapping_bundle(&raw2, &input2)
        .expect("conflict 1");
    let (raw3, input3) = raw_event(&cell, "erpnext", &a3, "e1", "1", "upsert", "p1");
    let (c2, ins2) = cell
        .inject_mapping_bundle(&raw3, &input3)
        .expect("conflict 2");
    assert!(ins1 && ins2 && c1 != c2);

    // Resolve c1 with decomposed NFD Unicode actor & reason while c2 is still open.
    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: c1.clone(),
        decision: DisputeDecision::AcceptSecondAsCorrection,
        actor: "ops:e\u{0301}rin".into(),
        reason: "cafe\u{0301} correction".into(),
    })
    .expect("resolve c1");

    assert!(
        refers_targets(&cell, "demo/party/a").is_empty(),
        "sibling pending dispute c2 must keep fresh correction claim frozen"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("NFC-normalized audit index must match ledger with zero drift");
    let audit = cell
        .store()
        .read()
        .dispute_resolution(&c1)
        .expect("read audit")
        .expect("audit present");
    assert_eq!(audit.actor, "ops:\u{00e9}rin");
    assert_eq!(audit.reason, "caf\u{00e9} correction");

    // Negative commit_seq in the derived audit index must trigger heal on reopen.
    let model = cell.store().model().clone();
    drop(cell);
    {
        let conn = rusqlite::Connection::open(&path).expect("sqlite");
        conn.execute(
            "UPDATE \"_norn_dispute_resolution\" SET \"commit_seq\" = -1",
            [],
        )
        .expect("corrupt commit_seq");
    }
    let reopened =
        CellStore::open(&path, model, StoreOptions::default()).expect("reopen heals negative seq");
    reopened
        .read()
        .verify_frontier_equivalence()
        .expect("healed audit index matches ledger");
}

/// #1549: Rejecting a contested explicit link clear (`target: None`) supersedes
/// that disputed clear and restores the surviving undisputed claim's edge.
#[test]
fn dispute_resolve_reject_contested_clear_restores_surviving_claim() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(&dir.path().join("reject_clear.sqlite"), referring_parties);

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let a_v1 = party_ref_row("P-A", "demo/party/a", "A v1", Some("demo/party/b"));
    let a_v2_clear = party_ref_row("P-A", "demo/party/a", "A v2 clear", None);
    let a_v2_alt = party_ref_row("P-A", "demo/party/a", "A v2 alt", None);

    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_v1, "e1", "1", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );

    // Late/contested explicit clear for an out-of-order event `e0` alongside `e1`,
    // or a contested explicit clear on `e2` when `e1` still stands active.
    // Use a second partition `p2` for `e1`? No, same partition with out-of-order
    // `e0` leaves both `e1` (Some(B)) and `e0` (None) active in the contribution!
    capture_kind(&mut cell, "erpnext", &a_v2_clear, "e0", "0", "upsert", "p1");
    let (raw_alt, input_alt) = raw_event(&cell, "erpnext", &a_v2_alt, "e0", "0", "upsert", "p1");
    let (conflict_id, inserted) = cell
        .inject_mapping_bundle(&raw_alt, &input_alt)
        .expect("conflict clear");
    assert!(inserted && conflict_id.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id,
        decision: DisputeDecision::Reject,
        actor: "ops:helen".into(),
        reason: "bogus stale clear".into(),
    })
    .expect("reject contested clear");

    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()]),
        "rejecting contested explicit clear restores undisputed surviving claim"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after rejecting contested clear");
}

/// #1549 / OBS-LINK-DISPUTE-1: When two distinct events in the same contribution
/// are BOTH disputed (`d1` on `e1` and `d2` on `e2`), rejecting `d2` must NOT
/// release `e1`'s edge while `d1` remains open; resolving `d1` afterwards
/// publishes (on accept-first) or removes (on reject) cleanly.
#[test]
fn dispute_resolve_reject_keeps_disputed_survivor_frozen_until_its_dispute_resolves() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("two_events_disputed.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    let a_e2 = party_ref_row("P-A", "demo/party/a", "A e2", Some("demo/party/c"));
    let a_e2_bad = party_ref_row("P-A", "demo/party/a", "A e2 bad", Some("demo/party/c"));
    let a_e1 = party_ref_row("P-A", "demo/party/a", "A e1", Some("demo/party/b"));
    let a_e1_bad = party_ref_row("P-A", "demo/party/a", "A e1 bad", Some("demo/party/b"));

    // Land `e2` (version 2) and late `e1` (version 1) before targets exist so
    // neither edge is published yet, then open disputes on BOTH `e1` and `e2`.
    capture_kind(&mut cell, "erpnext", &a_e2, "e2", "2", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_e1, "e1", "1", "upsert", "p1");
    let (r1, i1) = raw_event(&cell, "erpnext", &a_e1_bad, "e1", "1", "upsert", "p1");
    let (d1, ins1) = cell.inject_mapping_bundle(&r1, &i1).expect("dispute d1");
    let (r2, i2) = raw_event(&cell, "erpnext", &a_e2_bad, "e2", "2", "upsert", "p1");
    let (d2, ins2) = cell.inject_mapping_bundle(&r2, &i2).expect("dispute d2");
    assert!(ins1 && ins2);

    // Targets arrive while both disputes are open -> edges remain held.
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &c, "ec", "1", "upsert", "p1");
    assert!(refers_targets(&cell, "demo/party/a").is_empty());

    // Reject `d2` -> `e1` is STILL disputed by `d1`, so edge `-> B` MUST stay frozen!
    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: d2,
        decision: DisputeDecision::Reject,
        actor: "ops:ivan".into(),
        reason: "reject e2".into(),
    })
    .expect("reject d2");
    assert!(
        refers_targets(&cell, "demo/party/a").is_empty(),
        "OBS-LINK-DISPUTE-1: rejecting d2 must not publish e1's edge while d1 is open"
    );

    // Now reject `d1` as well -> no real surviving claims remain, edge stays absent.
    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: d1,
        decision: DisputeDecision::Reject,
        actor: "ops:ivan".into(),
        reason: "reject e1 too".into(),
    })
    .expect("reject d1");
    assert!(refers_targets(&cell, "demo/party/a").is_empty());
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after rejecting both disputes");
}

/// #1549 / #1028: Accepting a `kind == "delete"` correction when the conflicting
/// v2 upsert was still pending withdraws the deleting source's earlier v1 edge!
#[test]
fn dispute_resolve_accept_delete_correction_withdraws_earlier_v1_edge() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("delete_correction_pending_v2.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let a_v1 = party_ref_row("P-A", "demo/party/a", "A v1", Some("demo/party/b"));
    let a_v2 = party_ref_row("P-A", "demo/party/a", "A v2", Some("demo/party/b"));

    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_v1, "e1", "1", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );

    // Capture v2 upsert WITHOUT processing it, then capture conflicting v2 delete.
    let (r_up, i_up) = raw_event(&cell, "erpnext", &a_v2, "e2", "2", "upsert", "p1");
    let (_pending_v2, _) = cell.inject_mapping_bundle(&r_up, &i_up).expect("inject v2");
    let (r_del, i_del) = raw_event(&cell, "erpnext", &a_v2, "e2", "2", "delete", "p1");
    let (conflict_id, inserted) = cell
        .inject_mapping_bundle(&r_del, &i_del)
        .expect("inject v2 delete conflict");
    assert!(inserted && conflict_id.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id,
        decision: DisputeDecision::AcceptSecondAsCorrection,
        actor: "ops:judy".into(),
        reason: "delete supersedes pending v2 and withdraws v1 edge".into(),
    })
    .expect("resolve with delete correction");

    assert!(
        refers_targets(&cell, "demo/party/a").is_empty(),
        "delete correction must withdraw earlier v1 edge even when v2 upsert was pending"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after delete correction");
}

/// #1549 / OBS-LINK-CONTRIBUTION-1: Rejecting two of three contested targets in
/// one contribution supersedes the intermediate synthetic `None` so the sole
/// surviving claim returns to `Standing::Current` (not permanently contested).
#[test]
fn dispute_resolve_three_way_reject_restores_unique_survivor_to_current() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("three_way_reject.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    let d = party_ref_row("P-D", "demo/party/d", "D", None);
    // Out-of-order descending versions `3 -> 2 -> 1` before targets arrive
    // create three contested active claims (`B`, `C`, `D`) with no edge yet.
    let a3 = party_ref_row("P-A", "demo/party/a", "A3", Some("demo/party/b"));
    let a2 = party_ref_row("P-A", "demo/party/a", "A2", Some("demo/party/c"));
    let a1 = party_ref_row("P-A", "demo/party/a", "A1", Some("demo/party/d"));
    let a2_bad = party_ref_row("P-A", "demo/party/a", "A2 bad", Some("demo/party/c"));
    let a1_bad = party_ref_row("P-A", "demo/party/a", "A1 bad", Some("demo/party/d"));

    capture_kind(&mut cell, "erpnext", &a3, "e3", "3", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a2, "e2", "2", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a1, "e1", "1", "upsert", "p1");
    for t in [&b, &c, &d] {
        capture_kind(&mut cell, "erpnext", t, "et", "1", "upsert", "p1");
    }
    assert!(refers_targets(&cell, "demo/party/a").is_empty());

    let (r2, i2) = raw_event(&cell, "erpnext", &a2_bad, "e2", "2", "upsert", "p1");
    let (d2, _) = cell.inject_mapping_bundle(&r2, &i2).expect("d2");
    let (r1, i1) = raw_event(&cell, "erpnext", &a1_bad, "e1", "1", "upsert", "p1");
    let (d1, _) = cell.inject_mapping_bundle(&r1, &i1).expect("d1");

    // Reject e1 first (leaves two real survivors `B` and `C` -> synthetic None).
    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: d1,
        decision: DisputeDecision::Reject,
        actor: "ops:kate".into(),
        reason: "reject e1".into(),
    })
    .expect("reject d1");

    // Reject e2 next -> supersedes both e2 AND the earlier synthetic None,
    // leaving `B` as the unique surviving `Standing::Current` claim!
    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: d2,
        decision: DisputeDecision::Reject,
        actor: "ops:kate".into(),
        reason: "reject e2".into(),
    })
    .expect("reject d2");

    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()]),
        "unique surviving target B must not remain contested against earlier synthetic None"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after 3-way reject");
}

/// #1549 / OBS-LINK-CONTRIBUTION-1: Accepting a late disputed `v2` delete
/// correction when an undisputed newer `v3` claim already owns the edge must
/// not supersede or erase `v3`'s link membership.
#[test]
fn dispute_resolve_late_v2_delete_correction_preserves_newer_v3_edge() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("late_v2_delete_v3.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let a_v3 = party_ref_row("P-A", "demo/party/a", "A v3", Some("demo/party/b"));
    let a_v2 = party_ref_row("P-A", "demo/party/a", "A v2", Some("demo/party/b"));

    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_v3, "e3", "3", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );

    // Late v2 upsert (pending) + conflicting late v2 delete.
    let (r_up, i_up) = raw_event(&cell, "erpnext", &a_v2, "e2", "2", "upsert", "p1");
    let (_pending_v2, _) = cell.inject_mapping_bundle(&r_up, &i_up).expect("inject v2");
    let (r_del, i_del) = raw_event(&cell, "erpnext", &a_v2, "e2", "2", "delete", "p1");
    let (conflict_id, _) = cell
        .inject_mapping_bundle(&r_del, &i_del)
        .expect("inject v2 delete conflict");

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id,
        decision: DisputeDecision::AcceptSecondAsCorrection,
        actor: "ops:leo".into(),
        reason: "late v2 delete".into(),
    })
    .expect("resolve late v2 delete");

    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()]),
        "OBS-LINK-CONTRIBUTION-1: late v2 delete correction must not erase newer v3 edge"
    );
    let tombstone =
        norn_kernel::snapshot_observe::tombstone_key(&FactKey::new("canon.Party", "demo/party/a"));
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, tombstone.object_type.as_str(), &tombstone.key)
            .expect("tombstone read")
            .is_none(),
        "late v2 delete correction must not tombstone newer v3 subject"
    );
    let page = cell
        .store()
        .read()
        .authoritative_page(MAIN_WORLD, "canon.Party", None, 100)
        .expect("authoritative page");
    assert!(
        page.records.iter().any(|r| r.id == "demo/party/a"),
        "v3 subject must remain visible in untombstoned reads"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after late v2 delete");
}

/// #1549 / OBS-LINK-CONTRIBUTION-1: Rejecting `v3 -> C` with surviving `v1 -> B`
/// restores `B` without leaving a synthetic `v3` claim that would block a
/// subsequent `v2 -> D` update from superseding `v1` and moving the edge to `D`.
#[test]
fn dispute_resolve_reject_v3_does_not_block_subsequent_v2_superseding_v1() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("reject_v3_then_v2.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    let d = party_ref_row("P-D", "demo/party/d", "D", None);
    for t in [&b, &c, &d] {
        capture_kind(&mut cell, "erpnext", t, "et", "1", "upsert", "p1");
    }

    let a_v3 = party_ref_row("P-A", "demo/party/a", "A v3", Some("demo/party/c"));
    let a_v3_bad = party_ref_row("P-A", "demo/party/a", "A v3 bad", Some("demo/party/c"));
    let a_v1 = party_ref_row("P-A", "demo/party/a", "A v1", Some("demo/party/b"));
    let a_v2 = party_ref_row("P-A", "demo/party/a", "A v2", Some("demo/party/d"));

    // v3 -> C lands, late v1 -> B lands (contested), conflict opens on v3 -> C.
    capture_kind(&mut cell, "erpnext", &a_v3, "e3", "3", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &a_v1, "e1", "1", "upsert", "p1");
    let (r3, i3) = raw_event(&cell, "erpnext", &a_v3_bad, "e3", "3", "upsert", "p1");
    let (d3, _) = cell.inject_mapping_bundle(&r3, &i3).expect("d3");

    // Reject v3 -> restores surviving v1 -> B (C removed, B added).
    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: d3,
        decision: DisputeDecision::Reject,
        actor: "ops:mia".into(),
        reason: "reject bogus v3".into(),
    })
    .expect("reject v3");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );

    // Subsequent v2 -> D supersedes v1 AND the synthetic reject carry-forward,
    // cleanly moving the edge from B to D!
    capture_kind(&mut cell, "erpnext", &a_v2, "e2", "2", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/d".to_owned()]),
        "synthetic reject carry-forward must not pin version 3 and block v2 -> D"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after reject-v3 then v2");
}

/// #1549 / OBS-LINK-CONTRIBUTION-1: A lower-version `v2` conflict reusing a
/// newer accepted `v3`'s `event_id` resolved with `accept-second-as-correction`
/// must NOT supersede `v3`'s edge.
#[test]
fn dispute_resolve_lower_version_same_event_id_correction_preserves_v3() {
    let dir = tempfile::tempdir().expect("dir");
    let mut cell = setup_with(
        &dir.path().join("same_event_lower_ver.sqlite"),
        referring_parties,
    );

    let b = party_ref_row("P-B", "demo/party/b", "B", None);
    let c = party_ref_row("P-C", "demo/party/c", "C", None);
    capture_kind(&mut cell, "erpnext", &b, "eb", "1", "upsert", "p1");
    capture_kind(&mut cell, "erpnext", &c, "ec", "1", "upsert", "p1");

    let a_v3 = party_ref_row("P-A", "demo/party/a", "A v3", Some("demo/party/b"));
    let a_v2 = party_ref_row("P-A", "demo/party/a", "A v2", Some("demo/party/c"));
    capture_kind(&mut cell, "erpnext", &a_v3, "e_shared", "3", "upsert", "p1");
    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()])
    );

    // Lower-version v2 reusing the exact same event_id `e_shared` opens conflict.
    let (r2, i2) = raw_event(&cell, "erpnext", &a_v2, "e_shared", "2", "upsert", "p1");
    let (conflict_id, inserted) = cell.inject_mapping_bundle(&r2, &i2).expect("conflict");
    assert!(inserted && conflict_id.starts_with("mapping-conflict:"));

    cell.resolve_mapping_dispute(DisputeResolutionInput {
        dispute_id: conflict_id,
        decision: DisputeDecision::AcceptSecondAsCorrection,
        actor: "ops:noah".into(),
        reason: "lower-version same-event correction".into(),
    })
    .expect("resolve");

    assert_eq!(
        refers_targets(&cell, "demo/party/a"),
        BTreeSet::from(["demo/party/b".to_owned()]),
        "lower-version correction reusing v3's event_id must not supersede v3"
    );
    cell.store()
        .read()
        .verify_frontier_equivalence()
        .expect("frontier equivalence after lower-version same-event correction");
}
