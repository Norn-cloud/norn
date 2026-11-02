#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "GRAPH-CLASS-1 scenarios"
)]
use norn_cell::classification::{ClassificationError, evaluate};
use norn_cell::reactive::{Dependency, VisibleItem};
use norn_cell::{
    CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model, StoreOptions,
};
use norn_checker::{EntityArtifactSetContext, classification::Classifier};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn fixture(max_records: u32) -> (Model, InstalledDefinitionInput, Classifier) {
    let definition = json!({"name":"test.Scrap", "properties":[
        {"id":"fine_g", "type":{"kind":"integer"}},
        {"id":"note", "type":{"kind":"text"}, "constraints":{"nullable":true}}
    ]});
    let obligation =
        json!({"name":"test.Approval", "properties":[{"id":"state", "type":{"kind":"text"}}]});
    let model = Model::from_definitions(
        &[
            serde_json::from_value(definition.clone()).expect("base"),
            serde_json::from_value(obligation.clone()).expect("obligation"),
        ],
        &[],
    )
    .expect("model");
    let base: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":"test.Scrap.entity", "version":"1.0.0", "kind":"entity",
        "namespace":"test", "name":"Scrap", "lifecycle":"draft", "scope":{}, "dependencies":[],
        "provenance":{"source":"system"}, "payload":{"entityVersion":"2.0", "objectType":definition}
    }))
    .expect("artifact");
    let mut approval = serde_json::to_value(&base).expect("approval envelope");
    approval["artifactId"] = json!("test.Approval.entity");
    approval["name"] = json!("Approval");
    approval["payload"]["objectType"] = obligation;
    let mut bounds = ResourceLimitProfile::norn_limits_v1();
    bounds.slice.max_records = max_records;
    let class: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":"test.HighValue", "version":"1.0.0", "kind":"defined_concept",
        "namespace":"test", "name":"HighValue", "lifecycle":"draft", "scope":{},
        "provenance":{"source":"system"}, "dependencies":[{"artifactId":"test.Scrap.entity", "version":"1.0.0", "kind":"entity", "required":true}],
        "payload":{"id":"test.HighValue", "version":"1.0.0", "base":{"artifactId":"test.Scrap.entity", "version":"1.0.0",
            "member":{"kind":"type", "typeName":"test.Scrap"}}, "predicate":"self.fine_g > 100 and self.note == null", "dependencies":[], "bounds":bounds}
    })).expect("classification");
    let classifier = Classifier::prepare(
        &class,
        &EntityArtifactSetContext {
            base: std::slice::from_ref(&base),
            registry: &BTreeSet::new(),
            tenant: None,
        },
    )
    .expect("admission");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(json!([base, class, approval]))
                .expect("set")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("utf8"),
        registry: vec![],
        receipt_sha256: "test-admission".into(),
        admission_receipt: None,
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    };
    (model, input, classifier)
}
fn put(amount: i64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.into(),
        scope: "test".into(),
        logical_time: 2,
        writes: vec![FactWriteRow::Put {
            object_type: "test.Scrap".into(),
            key: "lot-7".into(),
            values: BTreeMap::from([
                (
                    "fine_g".into(),
                    RuntimeValue::from_json(json!(amount)).expect("integer"),
                ),
                ("note".into(), RuntimeValue::Null),
            ]),
        }],
        ..CommitBatch::default()
    }
}
#[test]
fn graph_classify_changes_membership_without_events_or_obligation_changes() {
    let (model, input, class) = fixture(10);
    let mut store = CellStore::open_in_memory(model.clone()).expect("cell");
    store
        .install_definitions(model, input, Some(0), "test")
        .expect("install");
    let empty =
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible).expect("empty");
    assert!(empty.members.is_empty());
    assert!(
        empty
            .dependencies
            .entries
            .iter()
            .any(|d| matches!(d, Dependency::Range { .. }))
    );
    let mut activated = put(101);
    activated.writes.push(FactWriteRow::Put {
        object_type: "test.Approval".into(),
        key: "approval-1".into(),
        values: BTreeMap::from([("state".into(), RuntimeValue::String("activated".into()))]),
    });
    store.commit(&activated).expect("activate");
    let head = store.verified_head().expect("head");
    let read =
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible).expect("member");
    assert_eq!(read.members, BTreeSet::from(["lot-7".into()]));
    assert_eq!(store.verified_head().expect("head"), head);
    store.commit(&put(99)).expect("permitted reconciliation");
    let head = store.verified_head().expect("head");
    assert!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible)
            .expect("exit")
            .members
            .is_empty()
    );
    assert_eq!(
        store.verified_head().expect("no classification effects"),
        head
    );
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "test.Approval", "approval-1")
            .expect("read")
            .expect("durable")
            .values["state"],
        RuntimeValue::String("activated".into())
    );
}
#[test]
fn graph_classify_refuses_uninstalled_and_exhausted_reads_and_captures_hidden_rows() {
    let (model, input, class) = fixture(10);
    let mut store = CellStore::open_in_memory(model.clone()).expect("cell");
    assert!(matches!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible),
        Err(ClassificationError::Incomplete)
    ));
    store
        .install_definitions(model, input, Some(0), "test")
        .expect("install");
    store.commit(&put(101)).expect("base");
    assert!(matches!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| {
            VisibleItem::Exhausted
        }),
        Err(ClassificationError::Incomplete)
    ));
    let hidden = evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Hidden)
        .expect("authorized subset");
    assert!(hidden.members.is_empty());
    assert!(
        hidden
            .dependencies
            .entries
            .iter()
            .any(|d| matches!(d, Dependency::Object { key, .. } if key == "lot-7"))
    );
    let (_, _, foreign) = fixture(9);
    assert!(matches!(
        evaluate(&store, &foreign, MAIN_WORLD, &mut |_, _| {
            VisibleItem::Visible
        }),
        Err(ClassificationError::Incomplete)
    ));
}
#[test]
fn graph_classify_refuses_a_commit_during_visibility() {
    let (model, input, class) = fixture(10);
    let dir = tempfile::tempdir().expect("temporary cell");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("cell");
    store
        .install_definitions(model.clone(), input, Some(0), "test")
        .expect("install");
    store.commit(&put(101)).expect("base");
    let mut writer =
        CellStore::open(&path, model, StoreOptions::default()).expect("concurrent writer");
    assert!(matches!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| {
            writer.commit(&put(99)).expect("concurrent reconciliation");
            VisibleItem::Visible
        }),
        Err(ClassificationError::Incomplete)
    ));
}
