#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

//! Durable classification snapshots, mapping lineage, and declassification.

use std::collections::BTreeMap;

use norn_cell::{
    CellError, CellStore, CommitBatch, CommitOutcome, FactWriteRow, MAIN_WORLD,
    MarkingDeclassification, Model, StoreOptions,
};
use norn_ir::{FactChange, ObjectTypeDef, ObjectTypeName, PropertyId, RuntimeValue as Value};
use serde_json::{Value as JsonValue, json};

const SCOPE: &str = "org_demo/dev";
const CONTACT: &str = "pii.contact";
const HEALTH: &str = "pii.health";

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "property lineage and export replay share exact versions"
)]
async fn property_lineage_keeps_exact_operands_unknowns_and_restore() {
    let definitions = || {
        model(&[
            json!({"name":"marking.PropertySource","identity":[{"name":"primary","properties":["id"]}],"properties":[{"id":"id","type":{"kind":"text"}},{"id":"contact","type":{"kind":"text"},"markings":[CONTACT]},{"id":"amount","type":{"kind":"text"},"markings":[HEALTH]}]}),
            json!({"name":"marking.PropertyOutput","identity":[{"name":"primary","properties":["id"]}],"properties":[{"id":"id","type":{"kind":"text"}},{"id":"value","type":{"kind":"text"}}]}),
        ])
    };
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("properties.sqlite");
    let mut store = CellStore::open(&path, definitions(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("binding");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.PropertySource",
                "source",
                &[("id", "source"), ("contact", "private"), ("amount", "10")],
            )],
        ))
        .expect("source");
    let source = norn_kernel::FactExpectation {
        key: norn_kernel::FactKey::new("marking.PropertySource", "source"),
        expected: norn_kernel::FactVersion(1),
    };
    // A later source value gains contact lineage; a historical operand must
    // still resolve version one's health label without consulting that value.
    let mut update = batch(
        2,
        vec![put(
            "marking.PropertySource",
            "source",
            &[("id", "source"), ("contact", "private"), ("amount", "20")],
        )],
    );
    update.marking_property_reads = vec![(source.clone(), vec!["contact".to_owned()])];
    store.commit(&update).expect("classified source update");
    for (index, (version, properties, full, expected)) in [
        (1, vec!["amount"], false, vec![HEALTH]),
        (2, vec!["amount"], false, vec![CONTACT, HEALTH]),
        (1, vec![], false, vec![]),
        (1, vec!["missing"], false, vec!["norn.unknown-lineage"]),
        (0, vec!["amount"], false, vec!["norn.unknown-lineage"]),
        (1, vec!["id"], true, vec![CONTACT, HEALTH]),
    ]
    .into_iter()
    .enumerate()
    {
        let read = norn_kernel::FactExpectation {
            expected: norn_kernel::FactVersion(version),
            ..source.clone()
        };
        let key = format!("output-{index}");
        let mut change = batch(
            u64::try_from(index).expect("index") * 2 + 3,
            vec![put(
                "marking.PropertyOutput",
                &key,
                &[("id", &key), ("value", "result")],
            )],
        );
        change.marking_property_reads = vec![(
            read.clone(),
            properties.into_iter().map(str::to_owned).collect(),
        )];
        if full {
            change.marking_reads.push(read);
        }
        store.commit(&change).expect("property output");
        let expected = expected.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            effective_markings(&store, "marking.PropertyOutput", &key, Some(1))["value"],
            expected
        );
        change.logical_time += 1;
        store.commit(&change).expect("conditional no-op");
        assert_eq!(
            store
                .read()
                .fact_occurrence_markings(MAIN_WORLD, "marking.PropertyOutput", &key, Some(2))
                .expect("occurrence")
                .expect("complete"),
            expected.into_iter().collect()
        );
    }
    let expected = store.read().dump().expect("snapshot");
    let mut exporter =
        norn_cell::export::Exporter::local(&directory.path().join("export")).expect("exporter");
    for seq in 1..=store.export_commit_count().expect("count") {
        let record = store.export_record(seq).expect("journal").expect("record");
        exporter.append(&record).await.expect("export");
    }
    let target = directory.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        definitions(),
        SCOPE,
        &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore property operands");
    let restored =
        CellStore::open(&target, definitions(), StoreOptions::default()).expect("restored");
    assert_eq!(restored.read().dump().expect("snapshot"), expected);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "reference classification and its exact restored journal share one fixture"
)]
async fn reference_lineage_preserves_occurrence_unknown_full_reads_and_restore() {
    let reference_model = || {
        model(&[
            json!({"name":"marking.PublicReference","identity":[{"name":"primary","properties":["id"]}],"properties":[{"id":"id","type":{"kind":"text"}},{"id":"contact","type":{"kind":"text"},"markings":[CONTACT]}]}),
            json!({"name":"marking.PrivateReference","identity":[{"name":"primary","properties":["id"]}],"markings":[HEALTH],"properties":[{"id":"id","type":{"kind":"text"}},{"id":"contact","type":{"kind":"text"},"markings":[CONTACT]}]}),
            json!({"name":"marking.PrivateIdentity","identity":[{"name":"primary","properties":["contact"]}],"properties":[{"id":"contact","type":{"kind":"text"},"markings":[CONTACT]}]}),
            json!({"name":"marking.KeylessReference","properties":[{"id":"contact","type":{"kind":"text"},"markings":[CONTACT]}]}),
            json!({"name":"marking.ReferenceOutput","properties":[{"id":"value","type":{"kind":"text"}}]}),
        ])
    };
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("references.sqlite");
    let mut store =
        CellStore::open(&path, reference_model(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("binding");
    let large_contact = "private contact ".repeat(24_000);
    store
        .commit(&batch(
            1,
            vec![
                put(
                    "marking.PublicReference",
                    "public",
                    &[("id", "public"), ("contact", &large_contact)],
                ),
                put(
                    "marking.PrivateReference",
                    "private",
                    &[("id", "private"), ("contact", "secret")],
                ),
                put(
                    "marking.KeylessReference",
                    "keyless",
                    &[("contact", "secret")],
                ),
                put(
                    "marking.PrivateIdentity",
                    "private-id",
                    &[("contact", "private-id")],
                ),
            ],
        ))
        .expect("sources");
    for (index, (source, key, full, labels)) in [
        ("marking.PublicReference", "public", false, vec![]),
        (
            "marking.KeylessReference",
            "keyless",
            false,
            vec![CONTACT.to_owned()],
        ),
        (
            "marking.PrivateIdentity",
            "private-id",
            false,
            vec![CONTACT.to_owned()],
        ),
        (
            "marking.PrivateReference",
            "private",
            false,
            vec![HEALTH.to_owned()],
        ),
        (
            "marking.PublicReference",
            "missing",
            false,
            vec!["norn.unknown-lineage".to_owned()],
        ),
        (
            "marking.PublicReference",
            "public",
            true,
            vec![CONTACT.to_owned()],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let reference = norn_kernel::FactExpectation {
            key: norn_kernel::FactKey::new(source, key),
            expected: norn_kernel::FactVersion(1),
        };
        let output = format!("output-{index}");
        let mut change = batch(
            (u64::try_from(index).expect("index") + 1) * 2,
            vec![put(
                "marking.ReferenceOutput",
                &output,
                &[("value", "public input")],
            )],
        );
        change.marking_reference_reads.push(reference.clone());
        if full {
            change.marking_reads.push(reference);
        }
        store.commit(&change).expect("reference output");
        assert_eq!(
            effective_markings(&store, "marking.ReferenceOutput", &output, Some(1))["value"],
            labels
        );
        // Re-emitting an unchanged value still makes the occurrence depend
        // on these exact references, without reclassifying preserved values.
        change.logical_time += 1;
        store.commit(&change).expect("reference-dependent no-op");
        assert_eq!(
            store
                .read()
                .fact_occurrence_markings(MAIN_WORLD, "marking.ReferenceOutput", &output, Some(2))
                .expect("occurrence")
                .expect("complete"),
            labels
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        );
    }
    let expected = store.read().dump().expect("snapshot");
    let mut exporter =
        norn_cell::export::Exporter::local(&directory.path().join("export")).expect("exporter");
    for seq in 1..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("export");
    }
    let target = directory.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        reference_model(),
        SCOPE,
        &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore exact reference lineage");
    let restored =
        CellStore::open(&target, reference_model(), StoreOptions::default()).expect("restored");
    assert_eq!(restored.read().dump().expect("restored snapshot"), expected);
    let cell = norn_cell::SqliteCell::<Value, Value, Value>::new(restored, SCOPE, |_| Value::Null)
        .expect("cell");
    let key = norn_kernel::FactKey::new("marking.PublicReference", "public");
    assert!(
        norn_kernel::CellHost::load_bounded(&cell, &key, 1024)
            .expect("bounded")
            .is_none()
    );
    let identity = norn_kernel::CellHost::load_reference(&cell, &key, 1024)
        .expect("identity")
        .expect("fits without contact");
    assert_eq!(identity.state.version, norn_kernel::FactVersion(1));
    assert!(identity.state.values.expect("exists").is_empty());
}

#[test]
fn destination_cas_lineage_stays_local_while_explicit_reads_propagate() {
    let definitions = || {
        model(&[
            json!({"name":"marking.Source","properties":[{"id":"value","type":{"kind":"text"},"markings":[CONTACT]}]}),
            json!({"name":"marking.Output","properties":[{"id":"value","type":{"kind":"text"}},{"id":"preserved","type":{"kind":"text"}}]}),
            json!({"name":"norn.RuleRun","properties":[{"id":"value","type":{"kind":"text"}}]}),
        ])
    };
    let expectation = |object_type, key, version| norn_kernel::FactExpectation {
        key: norn_kernel::FactKey::new(object_type, key),
        expected: norn_kernel::FactVersion(version),
    };
    for explicit in [false, true] {
        let mut store = CellStore::open_in_memory(definitions()).expect("store");
        store
            .commit(&batch(
                1,
                vec![
                    put("marking.Source", "source", &[("value", "private")]),
                    put("norn.RuleRun", "protocol", &[("value", "opaque protocol")]),
                ],
            ))
            .expect("classified sources");
        let mut seed = batch(
            2,
            vec![put(
                "marking.Output",
                "old",
                &[("value", "private"), ("preserved", "unchanged")],
            )],
        );
        seed.marking_reads
            .push(expectation("marking.Source", "source", 1));
        store.commit(&seed).expect("classified destination");
        let mut change = batch(
            3,
            vec![
                put(
                    "norn.RuleRun",
                    "protocol",
                    &[("value", "advanced opaque protocol")],
                ),
                put(
                    "marking.Output",
                    "old",
                    &[("value", "replacement"), ("preserved", "unchanged")],
                ),
                put(
                    "marking.Output",
                    "new",
                    &[("value", "public"), ("preserved", "public")],
                ),
            ],
        );
        change.marking_value_reads = vec![
            expectation("norn.RuleRun", "protocol", 1),
            expectation("marking.Output", "old", 1),
        ];
        if explicit {
            change
                .marking_reads
                .push(expectation("marking.Output", "old", 1));
        }
        store.commit(&change).expect("atomic destinations");
        let old = effective_markings(&store, "marking.Output", "old", Some(2));
        for labels in old.values() {
            assert_eq!(
                labels,
                &[CONTACT.to_owned()],
                "same-destination replacement and preserved fields retain prior labels"
            );
        }
        let new = effective_markings(&store, "marking.Output", "new", Some(1));
        for labels in new.values() {
            assert_eq!(
                labels.contains(&CONTACT.to_owned()),
                explicit,
                "explicit value read propagates; a sibling CAS does not"
            );
            assert!(!labels.contains(&"norn.unknown-lineage".to_owned()));
        }
        let protocol = effective_markings(&store, "norn.RuleRun", "protocol", Some(2));
        assert!(protocol["value"].contains(&"norn.unknown-lineage".to_owned()));
        let mut partial = batch(4, Vec::new());
        partial.facts.push(FactChange::Set {
            object_type: ObjectTypeName("marking.Output".into()),
            key: "old".into(),
            property: PropertyId("value".into()),
            value: Value::String("partial replacement".into()),
        });
        store
            .commit(&partial)
            .expect("same-destination partial update");
        assert_eq!(
            effective_markings(&store, "marking.Output", "old", Some(3)),
            old
        );
    }
}

fn model(definitions: &[JsonValue]) -> Model {
    let object_types = definitions
        .iter()
        .cloned()
        .map(|definition| {
            serde_json::from_value::<ObjectTypeDef>(definition)
                .expect("object type definitions decode")
        })
        .collect::<Vec<_>>();
    Model::from_definitions(&object_types, &[]).expect("model projects")
}

fn batch(logical_time: u64, writes: Vec<FactWriteRow>) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        logical_time,
        writes,
        ..CommitBatch::default()
    }
}

fn put(object_type: &str, key: &str, fields: &[(&str, &str)]) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.to_owned(),
        key: key.to_owned(),
        values: fields
            .iter()
            .map(|(property, value)| ((*property).to_owned(), Value::String((*value).to_owned())))
            .collect(),
    }
}

fn runtime(value: JsonValue) -> Value {
    Value::from_json(value).expect("event is an exact runtime value")
}

fn evidence_model() -> Model {
    model(&[
        json!({"name":"marking.Source","properties":[{"id":"email","type":{"kind":"text"},"markings":[CONTACT]}]}),
        json!({"name":"marking.Evidence","properties":[{"id":"message","type":{"kind":"text"}},{"id":"provenance","type":{"kind":"json-ext"}}]}),
    ])
}

fn evidence_provenance(markings: &[&str], upstream: Vec<norn_ir::provenance::FactRef>) -> Value {
    let provenance = norn_ir::InstanceProvenance {
        source: norn_ir::provenance::SourceRecordRef::new("internal", "evidence"),
        mapping_version: norn_ir::provenance::MappingVersion("evidence@1".to_owned()),
        transform: None,
        observed_at: None,
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-10-08T00:00:00Z".to_owned()),
        source_fields: Vec::new(),
        markings: markings
            .iter()
            .map(|marking| norn_ir::MarkingId((*marking).to_owned()))
            .collect(),
        upstream,
    };
    runtime(serde_json::to_value(provenance).expect("typed provenance"))
}

fn evidence_create(key: &str, provenance: Value) -> FactChange {
    FactChange::Create {
        object_type: ObjectTypeName("marking.Evidence".to_owned()),
        key: key.to_owned(),
        values: BTreeMap::from([
            (
                "message".to_owned(),
                Value::String("derived evidence".to_owned()),
            ),
            ("provenance".to_owned(), provenance),
        ]),
    }
}

fn evidence_set(key: &str, property: &str, value: Value) -> FactChange {
    FactChange::Set {
        object_type: ObjectTypeName("marking.Evidence".to_owned()),
        key: key.to_owned(),
        property: PropertyId(property.to_owned()),
        value,
    }
}

fn mapping_event(object_type: &str, key: &str, property_provenance: &JsonValue) -> Value {
    runtime(json!({
        "type": "observation.recorded",
        "mapped": [{
            "objectType": object_type,
            "key": key,
            "propertyProvenance": property_provenance
        }]
    }))
}

fn effective_markings(
    store: &CellStore,
    object_type: &str,
    key: &str,
    version: Option<u64>,
) -> BTreeMap<String, Vec<String>> {
    store
        .read()
        .effective_markings(MAIN_WORLD, object_type, key, version)
        .expect("marking snapshot reads")
        .expect("marking snapshot is complete")
}

fn contact_model() -> Model {
    model(&[json!({
        "name": "marking.Contact",
        "properties": [{
            "id": "email",
            "type": {"kind": "text"},
            "markings": [CONTACT]
        }]
    })])
}

fn system_lineage_model() -> Model {
    model(&[
        json!({
            "name": "marking.UnmarkedTarget",
            "properties": [{"id":"value","type":{"kind":"text"}}]
        }),
        json!({
            "name": "marking.MarkedTarget",
            "properties": [
                {"id":"contact","type":{"kind":"text"},"markings":[CONTACT]},
                {"id":"health","type":{"kind":"text"},"markings":[HEALTH]}
            ]
        }),
        json!({
            "name": "norn.ActionCall",
            "properties": [
                {"id":"actionId","type":{"kind":"text"}},
                {"id":"object","type":{"kind":"json-ext"}}
            ]
        }),
        json!({
            "name": "norn.CommandTask",
            "properties": [
                {"id":"actionCall","type":{"kind":"text"}},
                {"id":"actionId","type":{"kind":"text"}},
                {"id":"objectType","type":{"kind":"text"}},
                {"id":"objectKey","type":{"kind":"text"}},
                {"id":"attributed","type":{"kind":"boolean"}}
            ]
        }),
    ])
}

fn action_call(key: &str, target_type: &str, target_key: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: "norn.ActionCall".to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            (
                "actionId".to_owned(),
                Value::String("test.action".to_owned()),
            ),
            (
                "object".to_owned(),
                runtime(json!({"objectType":target_type,"key":target_key})),
            ),
        ]),
    }
}

fn command_task(
    key: &str,
    action_call_id: &str,
    target_type: &str,
    target_key: &str,
) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: "norn.CommandTask".to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            (
                "actionCall".to_owned(),
                Value::String(action_call_id.to_owned()),
            ),
            (
                "actionId".to_owned(),
                Value::String("test.action".to_owned()),
            ),
            (
                "objectType".to_owned(),
                Value::String(target_type.to_owned()),
            ),
            ("objectKey".to_owned(), Value::String(target_key.to_owned())),
            ("attributed".to_owned(), Value::Bool(true)),
        ]),
    }
}

fn unattributed_command_task(key: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: "norn.CommandTask".to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            ("actionCall".to_owned(), Value::String(String::new())),
            ("actionId".to_owned(), Value::String(String::new())),
            ("objectType".to_owned(), Value::String(String::new())),
            ("objectKey".to_owned(), Value::String(String::new())),
            ("attributed".to_owned(), Value::Bool(false)),
        ]),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one regression covers both system lineage paths and incomplete target snapshots"
)]
fn action_and_command_outputs_preserve_empty_unions_and_fail_closed_on_partial_snapshots() {
    let model = system_lineage_model();
    let directory = tempfile::tempdir().expect("temporary cell directory");
    let path = directory.path().join("cell.sqlite");
    let mut store =
        CellStore::open(&path, model.clone(), StoreOptions::default()).expect("cell opens");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.UnmarkedTarget", "public", &[("value", "public")]),
                put(
                    "marking.MarkedTarget",
                    "marked",
                    &[("contact", "email"), ("health", "diagnosis")],
                ),
                put(
                    "marking.MarkedTarget",
                    "partial",
                    &[("contact", "email"), ("health", "diagnosis")],
                ),
            ],
        ))
        .expect("target facts commit with complete snapshots");
    drop(store);

    // Keep one real marking row but remove another. A union over the rows
    // left behind would understate the target's complete classification.
    let raw = rusqlite::Connection::open(&path).expect("raw SQLite connection opens");
    let removed = raw
        .execute(
            "DELETE FROM \"_norn_marking_current\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"id\" = ?3 AND \"property\" = ?4",
            rusqlite::params![MAIN_WORLD, "marking.MarkedTarget", "partial", "health"],
        )
        .expect("remove one row from the current marking snapshot");
    assert_eq!(removed, 1);
    drop(raw);

    let mut store = CellStore::open(&path, model, StoreOptions::default())
        .expect("cell reopens after the partial-snapshot fixture");
    assert!(
        store
            .read()
            .effective_markings(MAIN_WORLD, "marking.MarkedTarget", "partial", None)
            .expect("partial target snapshot reads")
            .is_none()
    );
    store
        .commit(&batch(
            2,
            vec![
                action_call("call-public", "marking.UnmarkedTarget", "public"),
                command_task(
                    "task-public",
                    "call-public",
                    "marking.UnmarkedTarget",
                    "public",
                ),
                action_call("call-marked", "marking.MarkedTarget", "marked"),
                command_task(
                    "task-marked",
                    "call-marked",
                    "marking.MarkedTarget",
                    "marked",
                ),
                action_call("call-partial", "marking.MarkedTarget", "partial"),
                command_task(
                    "task-partial",
                    "call-partial",
                    "marking.MarkedTarget",
                    "partial",
                ),
                action_call("call-malformed", "missing.Target", "ghost"),
                command_task(
                    "task-malformed",
                    "call-malformed",
                    "missing.Target",
                    "ghost",
                ),
            ],
        ))
        .expect("action and command outputs commit");

    for (object_type, key, property) in [
        ("norn.ActionCall", "call-public", "object"),
        ("norn.CommandTask", "task-public", "objectKey"),
    ] {
        assert!(
            effective_markings(&store, object_type, key, None)[property].is_empty(),
            "a complete empty target union remains public"
        );
    }
    for (object_type, key, property) in [
        ("norn.ActionCall", "call-marked", "object"),
        ("norn.CommandTask", "task-marked", "objectKey"),
    ] {
        assert_eq!(
            effective_markings(&store, object_type, key, None)[property],
            vec![CONTACT.to_owned(), HEALTH.to_owned()],
            "complete target labels still propagate as a union"
        );
    }
    for (object_type, key, property) in [
        ("norn.ActionCall", "call-partial", "object"),
        ("norn.CommandTask", "task-partial", "objectKey"),
        ("norn.ActionCall", "call-malformed", "object"),
        ("norn.CommandTask", "task-malformed", "objectKey"),
    ] {
        assert_eq!(
            effective_markings(&store, object_type, key, None)[property],
            vec!["norn.unknown-lineage".to_owned()],
            "an incomplete or malformed target cannot expose a known label subset"
        );
    }
}

#[test]
fn command_tasks_keep_the_attributed_action_call_snapshot_after_target_declassification() {
    let mut store = CellStore::open_in_memory(system_lineage_model()).expect("cell opens");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.UnmarkedTarget", "public", &[("value", "public")]),
                put(
                    "marking.MarkedTarget",
                    "private",
                    &[("contact", "email"), ("health", "diagnosis")],
                ),
                action_call("call-public", "marking.UnmarkedTarget", "public"),
                action_call("call-private", "marking.MarkedTarget", "private"),
            ],
        ))
        .expect("targets and action calls commit");

    let declassifications = [("contact", CONTACT), ("health", HEALTH)]
        .into_iter()
        .map(|(property, marking)| MarkingDeclassification {
            object_type: "marking.MarkedTarget".to_owned(),
            key: "private".to_owned(),
            property: property.to_owned(),
            expected_version: 1,
            markings: vec![marking.to_owned()],
            actor: "privacy.officer".to_owned(),
            reason: "target labels are removed after action enqueue".to_owned(),
        })
        .collect::<Vec<_>>();
    let mut declass = batch(2, Vec::new());
    declass.events = declassifications
        .iter()
        .map(|change| {
            change
                .ledger_event()
                .expect("declassification ledger event")
        })
        .collect();
    declass.marking_declassifications = declassifications;
    store
        .commit(&declass)
        .expect("target labels are declassified after enqueue");
    assert!(
        effective_markings(&store, "marking.MarkedTarget", "private", None)
            .values()
            .all(Vec::is_empty)
    );
    assert_eq!(
        effective_markings(&store, "norn.ActionCall", "call-private", Some(1))["object"],
        vec![CONTACT.to_owned(), HEALTH.to_owned()],
        "the action call retains the immutable target labels captured at enqueue"
    );

    store
        .commit(&batch(
            3,
            vec![
                command_task(
                    "task-private",
                    "call-private",
                    "marking.MarkedTarget",
                    "private",
                ),
                command_task(
                    "task-public",
                    "call-public",
                    "marking.UnmarkedTarget",
                    "public",
                ),
                command_task(
                    "task-missing-call",
                    "missing-action-call",
                    "marking.MarkedTarget",
                    "private",
                ),
                unattributed_command_task("task-unattributed"),
            ],
        ))
        .expect("command tasks commit after target declassification");

    assert_eq!(
        effective_markings(&store, "norn.CommandTask", "task-private", None)["objectKey"],
        vec![CONTACT.to_owned(), HEALTH.to_owned()],
        "a task uses its attributed action call's immutable marking snapshot"
    );
    assert!(
        effective_markings(&store, "norn.CommandTask", "task-public", None)["objectKey"].is_empty()
    );
    for key in ["task-missing-call", "task-unattributed"] {
        assert_eq!(
            effective_markings(&store, "norn.CommandTask", key, None)["objectKey"],
            vec!["norn.unknown-lineage".to_owned()],
            "missing or untrusted attribution remains concealed"
        );
    }
}

#[test]
fn typed_function_reconciliation_metadata_inherits_its_pinned_reads() {
    for source_marked in [false, true] {
        let source_markings: Vec<&str> = if source_marked {
            vec![CONTACT]
        } else {
            Vec::new()
        };
        let model = model(&[
            json!({"name":"marking.Input","properties":[{
                "id":"email","type":{"kind":"text"},"markings":source_markings
            }]}),
            json!({"name":"norn.FunctionCommand","properties":[{
                "id":"state","type":{"kind":"json-ext"}
            }]}),
        ]);
        let mut store = CellStore::open_in_memory(model).expect("cell opens");
        store
            .commit(&batch(
                1,
                vec![put(
                    "marking.Input",
                    "I-1",
                    &[("email", "private@example.test")],
                )],
            ))
            .expect("input commits");
        let state =
            runtime(json!({"generation":1,"cancelled":false,"accepted":null,"reschedules":0}));
        let mut commit = batch(
            2,
            vec![FactWriteRow::Put {
                object_type: "norn.FunctionCommand".to_owned(),
                key: "command-1".to_owned(),
                values: BTreeMap::from([("state".to_owned(), state)]),
            }],
        );
        commit.marking_reads = vec![norn_kernel::FactExpectation {
            key: norn_kernel::FactKey::new("marking.Input", "I-1"),
            expected: norn_kernel::FactVersion(1),
        }];
        store
            .commit(&commit)
            .expect("pinned reconciliation commits");
        let labels = effective_markings(&store, "norn.FunctionCommand", "command-1", None);
        assert_eq!(
            labels.get("state"),
            Some(
                &source_markings
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            )
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one lineage scenario verifies current, historical, and missing source snapshots together"
)]
fn mapped_labels_follow_exact_historical_snapshots_and_unknowns_fail_closed() {
    let model = model(&[
        json!({
            "name": "marking.Source",
            "properties": [{
                "id": "email",
                "type": {"kind": "text"},
                "markings": [CONTACT]
            }]
        }),
        json!({
            "name": "marking.Target",
            "properties": [
                {"id": "direct", "type": {"kind": "text"}},
                {"id": "historical", "type": {"kind": "text"}},
                {"id": "missing", "type": {"kind": "text"}}
            ]
        }),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.Source",
                "source-1",
                &[("email", "alice@example.test")],
            )],
        ))
        .expect("source fact commits");

    // Current visibility is removed while the immutable version 1 labels stay
    // attached to the source snapshot named by the mapping's FactRef.
    let change = MarkingDeclassification {
        object_type: "marking.Source".to_owned(),
        key: "source-1".to_owned(),
        property: "email".to_owned(),
        expected_version: 1,
        markings: vec![CONTACT.to_owned()],
        actor: "privacy.officer".to_owned(),
        reason: "approved source snapshot".to_owned(),
    };
    let mut declass = batch(2, Vec::new());
    declass.events = vec![change.ledger_event().expect("audit event encodes")];
    declass.marking_declassifications = vec![change];
    store
        .commit(&declass)
        .expect("source current marking is removed");
    assert!(effective_markings(&store, "marking.Source", "source-1", None)["email"].is_empty());
    assert_eq!(
        effective_markings(&store, "marking.Source", "source-1", Some(1))["email"],
        vec![CONTACT.to_owned()]
    );

    let mut mapped = batch(
        3,
        vec![put(
            "marking.Target",
            "target-1",
            &[
                ("direct", "raw direct value"),
                ("historical", "raw historical value"),
                ("missing", "raw missing value"),
            ],
        )],
    );
    mapped.events = vec![mapping_event(
        "marking.Target",
        "target-1",
        &json!({
            "direct": {"markings": [HEALTH]},
            "historical": {
                "upstream": [{
                    "objectType": "marking.Source",
                    "key": "source-1",
                    "version": 1
                }]
            },
            "missing": {
                "upstream": [{
                    "objectType": "marking.Source",
                    "key": "never-created",
                    "version": 41
                }]
            }
        }),
    )];
    store
        .commit(&mapped)
        .expect("mapping output and lineage commit");

    let labels = effective_markings(&store, "marking.Target", "target-1", None);
    assert_eq!(labels["direct"], vec![HEALTH.to_owned()]);
    assert_eq!(labels["historical"], vec![CONTACT.to_owned()]);
    assert_eq!(
        labels["missing"],
        vec!["norn.unknown-lineage".to_owned()],
        "a missing exact upstream snapshot is not treated as unmarked"
    );
    let target = store
        .read()
        .object(MAIN_WORLD, "marking.Target", "target-1")
        .expect("target reads")
        .expect("target exists");
    assert_eq!(
        target.values["historical"],
        Value::String("raw historical value".to_owned()),
        "labels affect the read projection, not the stored fact"
    );
}

#[test]
fn read_set_markings_cover_each_emitted_version_and_only_changed_put_properties() {
    let model = model(&[
        json!({
            "name": "marking.Input",
            "properties": [{"id":"private","type":{"kind":"text"},"markings":[CONTACT]}]
        }),
        json!({
            "name": "marking.Output",
            "properties": [
                {"id":"derived","type":{"kind":"text"}},
                {"id":"untouched","type":{"kind":"text"}}
            ]
        }),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.Input", "input", &[("private", "secret")]),
                put(
                    "marking.Output",
                    "put-output",
                    &[("derived", "old"), ("untouched", "steady")],
                ),
            ],
        ))
        .expect("initial facts commit");

    let mut derived = batch(
        2,
        vec![
            put(
                "marking.Output",
                "put-output",
                &[("derived", "first"), ("untouched", "steady")],
            ),
            put(
                "marking.Output",
                "put-output",
                &[("derived", "second"), ("untouched", "steady")],
            ),
        ],
    );
    derived.marking_reads = vec![norn_kernel::FactExpectation {
        key: norn_kernel::FactKey::new("marking.Input", "input"),
        expected: norn_kernel::FactVersion(1),
    }];
    derived.facts = vec![
        FactChange::Create {
            object_type: ObjectTypeName("marking.Output".to_owned()),
            key: "fact-output".to_owned(),
            values: BTreeMap::from([
                ("derived".to_owned(), Value::String("first".to_owned())),
                ("untouched".to_owned(), Value::String("steady".to_owned())),
            ]),
        },
        FactChange::Set {
            object_type: ObjectTypeName("marking.Output".to_owned()),
            key: "fact-output".to_owned(),
            property: PropertyId("derived".to_owned()),
            value: Value::String("second".to_owned()),
        },
    ];
    store.commit(&derived).expect("derived outputs commit");

    for version in [2, 3] {
        let labels = effective_markings(&store, "marking.Output", "put-output", Some(version));
        assert_eq!(labels["derived"], vec![CONTACT.to_owned()]);
        assert!(labels["untouched"].is_empty());
    }
    assert!(
        effective_markings(&store, "marking.Output", "put-output", Some(1))["derived"].is_empty()
    );
    for version in [1, 2] {
        assert_eq!(
            effective_markings(&store, "marking.Output", "fact-output", Some(version))["derived"],
            vec![CONTACT.to_owned()],
            "Create and Set each retain the read-set classification"
        );
    }
}

#[test]
fn same_batch_pending_lineage_uses_full_fact_identity_and_version() {
    let model = model(&[
        json!({
            "name": "marking.Source",
            "properties": [{"id":"email","type":{"kind":"text"},"markings":[CONTACT]}]
        }),
        json!({
            "name": "marking.OtherSource",
            "properties": [{"id":"value","type":{"kind":"text"}}]
        }),
        json!({
            "name": "marking.Evidence",
            "properties": [
                {"id":"message","type":{"kind":"text"}},
                {"id":"provenance","type":{"kind":"json-ext"}}
            ]
        }),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.Source",
                "shared-key",
                &[("email", "private@example.test")],
            )],
        ))
        .expect("source v1 commits");

    let source_v1 = evidence_provenance(
        &[],
        vec![norn_ir::provenance::FactRef::new(
            "marking.Source",
            "shared-key",
            1,
        )],
    );
    store
        .commit(&batch(
            2,
            vec![
                put("marking.OtherSource", "shared-key", &[("value", "other")]),
                FactWriteRow::Put {
                    object_type: "marking.Evidence".to_owned(),
                    key: "unrelated-root".to_owned(),
                    values: BTreeMap::from([
                        ("message".to_owned(), Value::String("derived".to_owned())),
                        ("provenance".to_owned(), source_v1.clone()),
                    ]),
                },
            ],
        ))
        .expect("unrelated same-key output resolves exact source v1");
    assert_eq!(
        effective_markings(&store, "marking.Evidence", "unrelated-root", None)["message"],
        vec![CONTACT.to_owned()]
    );

    store
        .commit(&batch(
            3,
            vec![
                put(
                    "marking.Source",
                    "shared-key",
                    &[("email", "new-private@example.test")],
                ),
                FactWriteRow::Put {
                    object_type: "marking.Evidence".to_owned(),
                    key: "historical-version".to_owned(),
                    values: BTreeMap::from([
                        ("message".to_owned(), Value::String("derived".to_owned())),
                        ("provenance".to_owned(), source_v1),
                    ]),
                },
            ],
        ))
        .expect("historical source v1 remains resolvable while v2 is emitted");
    assert_eq!(
        effective_markings(&store, "marking.Evidence", "historical-version", None)["message"],
        vec![CONTACT.to_owned()]
    );
}

#[test]
fn same_batch_forward_lineage_remains_unknown_until_its_exact_snapshot_exists() {
    let model = evidence_model();
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.Source",
                "source-1",
                &[("email", "private@example.test")],
            )],
        ))
        .expect("source v1 commits");

    let future = evidence_provenance(
        &[],
        vec![norn_ir::provenance::FactRef::new(
            "marking.Source",
            "source-1",
            2,
        )],
    );
    store
        .commit(&batch(
            2,
            vec![
                FactWriteRow::Put {
                    object_type: "marking.Evidence".to_owned(),
                    key: "forward-reference".to_owned(),
                    values: BTreeMap::from([
                        ("message".to_owned(), Value::String("derived".to_owned())),
                        ("provenance".to_owned(), future),
                    ]),
                },
                put(
                    "marking.Source",
                    "source-1",
                    &[("email", "new-private@example.test")],
                ),
            ],
        ))
        .expect("forward reference remains fail-closed");
    let labels = effective_markings(&store, "marking.Evidence", "forward-reference", None);
    assert!(labels["message"].contains(&CONTACT.to_owned()));
    assert!(labels["message"].contains(&"norn.unknown-lineage".to_owned()));
}

#[test]
fn runtime_mapping_labels_close_over_computed_property_dependencies() {
    let model = model(&[json!({
        "name": "marking.Derived",
        "properties": [
            {"id": "source", "type": {"kind": "text"}},
            {"id": "first", "type": {"kind": "text"}, "computed": "self.source"},
            {"id": "second", "type": {"kind": "text"}, "computed": "self.first"}
        ]
    })]);
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    let mut mapped = batch(
        1,
        vec![put(
            "marking.Derived",
            "derived-1",
            &[("source", "mapped private value")],
        )],
    );
    // No type-level mark is declared: both computed labels must be learned
    // from this runtime mapping's source provenance.
    mapped.events = vec![mapping_event(
        "marking.Derived",
        "derived-1",
        &json!({"source": {"markings": [HEALTH]}}),
    )];
    store.commit(&mapped).expect("mapped fact commits");

    let labels = effective_markings(&store, "marking.Derived", "derived-1", None);
    assert_eq!(labels["source"], vec![HEALTH.to_owned()]);
    assert_eq!(labels["first"], vec![HEALTH.to_owned()]);
    assert_eq!(labels["second"], vec![HEALTH.to_owned()]);
}

#[test]
fn typed_declassification_is_ledgered_idempotent_and_reinherited_on_write() {
    let model = contact_model();
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.Contact",
                "alice",
                &[("email", "alice@example.test")],
            )],
        ))
        .expect("source fact commits");

    let change = MarkingDeclassification {
        object_type: "marking.Contact".to_owned(),
        key: "alice".to_owned(),
        property: "email".to_owned(),
        expected_version: 1,
        markings: vec![CONTACT.to_owned()],
        actor: "privacy.officer".to_owned(),
        reason: "approved for this exact contact fact".to_owned(),
    };
    let audit = change.ledger_event().expect("audit event encodes");
    let mut request = batch(2, Vec::new());
    request.idempotency_key = Some("declass-contact-v1".to_owned());
    request.events = vec![audit.clone()];
    request.marking_declassifications = vec![change];
    let committed = store
        .commit(&request)
        .expect("typed declassification commits");
    let first_receipt = committed.receipt().clone();
    let head = store.read().verify_ledger().expect("ledger verifies");
    let ledger = store.read().ledger(0).expect("ledger reads");
    assert_eq!(
        ledger.iter().filter(|entry| entry.event == audit).count(),
        1
    );
    assert!(effective_markings(&store, "marking.Contact", "alice", None)["email"].is_empty());
    assert_eq!(
        effective_markings(&store, "marking.Contact", "alice", Some(1))["email"],
        vec![CONTACT.to_owned()],
        "declassification changes current visibility, not historical lineage"
    );
    let raw = store
        .read()
        .object(MAIN_WORLD, "marking.Contact", "alice")
        .expect("object reads")
        .expect("contact exists");
    assert_eq!(
        raw.values["email"],
        Value::String("alice@example.test".to_owned())
    );

    let replay = store.commit(&request).expect("same request replays");
    let CommitOutcome::Replayed(replay_receipt) = replay else {
        panic!("same declassification request is idempotently replayed");
    };
    assert_eq!(replay_receipt, first_receipt);
    assert_eq!(store.read().verify_ledger().expect("ledger verifies"), head);

    store
        .commit(&batch(
            3,
            vec![put(
                "marking.Contact",
                "alice",
                &[("email", "alice.new@example.test")],
            )],
        ))
        .expect("ordinary fact write commits");
    assert_eq!(
        effective_markings(&store, "marking.Contact", "alice", None)["email"],
        vec![CONTACT.to_owned()],
        "the declared marking applies to a new fact version"
    );
    assert_eq!(
        effective_markings(&store, "marking.Contact", "alice", Some(1))["email"],
        vec![CONTACT.to_owned()]
    );
    let updated = store
        .read()
        .object(MAIN_WORLD, "marking.Contact", "alice")
        .expect("object reads")
        .expect("contact exists");
    assert_eq!(
        updated.values["email"],
        Value::String("alice.new@example.test".to_owned())
    );
}

fn install_input(
    label: &str,
    installed_at: u64,
    model: &Model,
) -> norn_cell::InstalledDefinitionInput {
    let mut artifacts = model
        .object_types()
        .map(|object_type| {
            let type_name = object_type.name.as_str();
            let (namespace, name) = type_name.rsplit_once('.').unwrap_or(("marking", type_name));
            json!({
                "irVersion": "0.1",
                "artifactId": format!("{type_name}.entity"),
                "kind": "entity",
                "namespace": namespace,
                "name": name,
                "version": "0.1.0",
                "lifecycle": "draft",
                "scope": {"orgId": "org_demo", "environmentId": "dev"},
                "dependencies": [],
                "provenance": {"source": "authoring_sdk"},
                "payload": {"entityVersion": "2.0", "objectType": object_type}
            })
        })
        .collect::<Vec<_>>();
    artifacts.push(json!({
        "irVersion": "0.1",
        "artifactId": "core.markings.entity",
        "kind": "entity",
        "namespace": "core",
        "name": "Markings",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": {
            "source": "system",
            "sourcePath": "registry/core/artifacts/core.markings.entity.json"
        },
        "payload": {
            "entityVersion": "2.0",
            "markings": [{
                "id": CONTACT,
                "label": "Contact information",
                "severity": "moderate",
                "category": "pii.contact",
                "mask": {"kind": "redacted"}
            }]
        }
    }));
    let canonical_set = String::from_utf8(
        Value::from_json(json!(artifacts))
            .expect("artifact set is exact JSON")
            .canonical_json()
            .expect("artifact set canonicalizes"),
    )
    .expect("canonical artifact set is UTF-8");
    norn_cell::InstalledDefinitionInput {
        canonical_set,
        registry: Vec::new(),
        receipt_sha256: format!("receipt-{label}"),
        admission_receipt: None,
        readmission: None,
        diff: json!({"changed": {"count": 1, "ids": [label]}}),
        installed_at,
        layout: None,
    }
}

#[test]
fn definition_marking_additions_backfill_history_and_removal_is_refused() {
    let unmarked = model(&[json!({
        "name": "marking.Contact",
        "properties": [{"id": "email", "type": {"kind": "text"}}]
    })]);
    let marked = contact_model();
    let mut store = CellStore::open_in_memory(unmarked.clone()).expect("store opens");
    store
        .install_definitions(
            unmarked.clone(),
            install_input("v1", 1, &unmarked),
            Some(0),
            SCOPE,
        )
        .expect("initial definition version installs");
    store
        .commit(&batch(
            2,
            vec![put(
                "marking.Contact",
                "alice",
                &[("email", "alice@example.test")],
            )],
        ))
        .expect("unmarked fact commits");
    assert!(effective_markings(&store, "marking.Contact", "alice", Some(1))["email"].is_empty());

    let before_check = store.read().verify_ledger().expect("ledger verifies");
    store
        .check_definitions(&marked, &install_input("v2", 3, &marked), Some(1))
        .expect("check accepts a complete marking backfill");
    assert_eq!(
        store.read().verify_ledger().expect("ledger verifies"),
        before_check
    );
    assert!(effective_markings(&store, "marking.Contact", "alice", Some(1))["email"].is_empty());

    store
        .install_definitions(
            marked.clone(),
            install_input("v2", 3, &marked),
            Some(1),
            SCOPE,
        )
        .expect("new marking is installed with a complete backfill");
    assert_eq!(
        effective_markings(&store, "marking.Contact", "alice", None)["email"],
        vec![CONTACT.to_owned()]
    );
    assert_eq!(
        effective_markings(&store, "marking.Contact", "alice", Some(1))["email"],
        vec![CONTACT.to_owned()],
        "new definitions classify retained historical fact versions too"
    );

    let before = store.read().verify_ledger().expect("ledger verifies");
    let checked = store.check_definitions(&unmarked, &install_input("v3", 4, &unmarked), Some(2));
    assert!(matches!(
        checked,
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger verifies"),
        before
    );
    let refused = store.install_definitions(
        unmarked.clone(),
        install_input("v3", 4, &unmarked),
        Some(2),
        SCOPE,
    );
    assert!(matches!(
        refused,
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger verifies"),
        before
    );
    assert_eq!(
        effective_markings(&store, "marking.Contact", "alice", None)["email"],
        vec![CONTACT.to_owned()],
        "a refused narrowing leaves the active classification intact"
    );
}

#[test]
fn new_properties_keep_complete_snapshots_and_computed_inputs_inherit_runtime_labels() {
    let previous = model(&[
        json!({"name":"marking.Contact","properties":[{"id":"email","type":{"kind":"text"}}]}),
    ]);
    let candidate = model(&[json!({"name":"marking.Contact","properties":[
        {"id":"email","type":{"kind":"text"}},
        {"id":"note","type":{"kind":"text"},"constraints":{"nullable":true}},
        {"id":"copy","type":{"kind":"text"},"computed":"self.email"}
    ]})]);
    let mut store = CellStore::open_in_memory(previous.clone()).expect("store");
    store
        .install_definitions(
            previous.clone(),
            install_input("v1", 1, &previous),
            Some(0),
            SCOPE,
        )
        .expect("original definitions");
    let mut observed = batch(
        2,
        vec![put(
            "marking.Contact",
            "alice",
            &[("email", "alice@example.test")],
        )],
    );
    observed.events = vec![mapping_event(
        "marking.Contact",
        "alice",
        &json!({"email":{"markings":[CONTACT]}}),
    )];
    store.commit(&observed).expect("runtime classified input");
    store
        .install_definitions(
            candidate.clone(),
            install_input("v2", 3, &candidate),
            Some(1),
            SCOPE,
        )
        .expect("new nullable and computed properties install");
    for version in [None, Some(1)] {
        let labels = effective_markings(&store, "marking.Contact", "alice", version);
        assert!(
            labels["note"].is_empty(),
            "a new absent nullable property has complete unmarked evidence"
        );
        assert_eq!(labels["email"], vec![CONTACT.to_owned()]);
        assert_eq!(
            labels["copy"],
            vec![CONTACT.to_owned()],
            "new computed fields inherit the historical runtime inputs"
        );
    }
    assert_eq!(
        store
            .read()
            .property_marking_summary(MAIN_WORLD, "marking.Contact", "note")
            .expect("summary"),
        Some(std::collections::BTreeSet::new())
    );
}

#[test]
fn definition_install_classifies_current_and_historical_snapshots_in_every_world() {
    let previous = model(&[
        json!({"name":"marking.Contact","properties":[{"id":"email","type":{"kind":"text"}}]}),
    ]);
    let candidate = model(&[json!({"name":"marking.Contact","properties":[
        {"id":"email","type":{"kind":"text"},"markings":[CONTACT]},
        {"id":"copy","type":{"kind":"text"},"computed":"self.email"}
    ]})]);
    let mut store = CellStore::open_in_memory(previous.clone()).expect("store");
    store
        .install_definitions(
            previous.clone(),
            install_input("v1", 1, &previous),
            Some(0),
            SCOPE,
        )
        .expect("original definitions");
    for (world, time) in [(MAIN_WORLD, 2), ("alternate", 3)] {
        let mut request = batch(
            time,
            vec![put(
                "marking.Contact",
                "alice",
                &[("email", "alice@example.test")],
            )],
        );
        request.world = world.to_owned();
        store.commit(&request).expect("fact in its world");
    }
    store
        .install_definitions(
            candidate.clone(),
            install_input("v2", 4, &candidate),
            Some(1),
            SCOPE,
        )
        .expect("classification backfills every world");
    for world in [MAIN_WORLD, "alternate"] {
        for version in [None, Some(1)] {
            let labels = store
                .read()
                .effective_markings(world, "marking.Contact", "alice", version)
                .expect("labels read")
                .expect("complete snapshot");
            assert_eq!(labels["email"], vec![CONTACT.to_owned()]);
            assert_eq!(labels["copy"], vec![CONTACT.to_owned()]);
        }
    }
}

#[test]
fn declassified_runtime_lineage_is_reinherited_by_an_ordinary_rewrite() {
    let model = model(&[json!({
        "name":"marking.Runtime","properties":[{"id":"email","type":{"kind":"text"}}]
    })]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    let mut first = batch(
        1,
        vec![put(
            "marking.Runtime",
            "R-1",
            &[("email", "private@example.test")],
        )],
    );
    first.events = vec![mapping_event(
        "marking.Runtime",
        "R-1",
        &json!({"email":{"markings":[CONTACT]}}),
    )];
    store.commit(&first).expect("runtime source marker");
    let change = MarkingDeclassification {
        object_type: "marking.Runtime".to_owned(),
        key: "R-1".to_owned(),
        property: "email".to_owned(),
        expected_version: 1,
        markings: vec![CONTACT.to_owned()],
        actor: "privacy.officer".to_owned(),
        reason: "this version may be public".to_owned(),
    };
    let mut declass = batch(2, Vec::new());
    declass.events = vec![change.ledger_event().expect("audit")];
    declass.marking_declassifications = vec![change];
    store.commit(&declass).expect("declassification");
    assert!(effective_markings(&store, "marking.Runtime", "R-1", None)["email"].is_empty());
    store
        .commit(&batch(
            3,
            vec![put(
                "marking.Runtime",
                "R-1",
                &[("email", "rewritten@example.test")],
            )],
        ))
        .expect("ordinary rewrite");
    assert_eq!(
        effective_markings(&store, "marking.Runtime", "R-1", None)["email"],
        vec![CONTACT.to_owned()]
    );
}

#[test]
fn record_provenance_protects_derived_evidence_outside_mapping_events() {
    let mut store = CellStore::open_in_memory(evidence_model()).expect("store");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.Source",
                "S-1",
                &[("email", "private@example.test")],
            )],
        ))
        .expect("classified source");
    let provenance = norn_ir::InstanceProvenance {
        source: norn_ir::provenance::SourceRecordRef::new("internal", "evidence"),
        mapping_version: norn_ir::provenance::MappingVersion("evidence@1".to_owned()),
        transform: None,
        observed_at: None,
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-10-08T00:00:00Z".to_owned()),
        source_fields: Vec::new(),
        markings: Vec::new(),
        upstream: vec![norn_ir::provenance::FactRef::new(
            "marking.Source",
            "S-1",
            1,
        )],
    };
    let evidence = FactWriteRow::Put {
        object_type: "marking.Evidence".to_owned(),
        key: "E-1".to_owned(),
        values: BTreeMap::from([
            (
                "message".to_owned(),
                Value::String("validation failed for private@example.test".to_owned()),
            ),
            (
                "provenance".to_owned(),
                runtime(serde_json::to_value(provenance).expect("typed provenance")),
            ),
        ]),
    };
    store
        .commit(&batch(2, vec![evidence]))
        .expect("derived evidence");
    assert_eq!(
        effective_markings(&store, "marking.Evidence", "E-1", None)["message"],
        vec![CONTACT.to_owned()]
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "canonical provenance, intermediate snapshots, restart and typed restore share one durable fixture"
)]
async fn canonical_fact_provenance_survives_declassification_restart_and_restore() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let mut store =
        CellStore::open(&path, evidence_model(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("binding");
    store
        .commit(&batch(
            1,
            vec![put(
                "marking.Source",
                "S-1",
                &[("email", "private@example.test")],
            )],
        ))
        .expect("source");
    let change = MarkingDeclassification {
        object_type: "marking.Source".to_owned(),
        key: "S-1".to_owned(),
        property: "email".to_owned(),
        expected_version: 1,
        markings: vec![CONTACT.to_owned()],
        actor: "privacy.officer".to_owned(),
        reason: "approved current visibility".to_owned(),
    };
    store
        .commit(&CommitBatch {
            events: vec![change.ledger_event().expect("audit event")],
            marking_declassifications: vec![change],
            ..batch(2, Vec::new())
        })
        .expect("current source declassified");
    assert!(effective_markings(&store, "marking.Source", "S-1", None)["email"].is_empty());
    let provenance = evidence_provenance(
        &[HEALTH],
        vec![norn_ir::provenance::FactRef::new(
            "marking.Source",
            "S-1",
            1,
        )],
    );
    store
        .commit(&CommitBatch {
            facts: vec![
                evidence_create("created", provenance.clone()),
                evidence_create("set", runtime(json!({}))),
            ],
            ..batch(3, Vec::new())
        })
        .expect("canonical creates");
    assert!(effective_markings(&store, "marking.Evidence", "set", Some(1))["message"].is_empty());
    store
        .commit(&CommitBatch {
            facts: vec![evidence_set("set", "provenance", provenance)],
            ..batch(4, Vec::new())
        })
        .expect("canonical provenance Set");
    for (key, version) in [("created", 1), ("set", 2)] {
        for selected in [None, Some(version)] {
            assert_eq!(
                effective_markings(&store, "marking.Evidence", key, selected)["message"],
                vec![CONTACT.to_owned(), HEALTH.to_owned()],
                "explicit labels and immutable upstream protect unchanged data after Set"
            );
        }
    }
    store
        .commit(&CommitBatch {
            facts: vec![
                evidence_create("intermediate", evidence_provenance(&[HEALTH], Vec::new())),
                evidence_set("intermediate", "provenance", runtime(json!({}))),
            ],
            ..batch(5, Vec::new())
        })
        .expect("envelope replacement in one batch");
    for version in 1..=2 {
        assert_eq!(
            effective_markings(&store, "marking.Evidence", "intermediate", Some(version))["message"],
            vec![HEALTH.to_owned()]
        );
    }
    let expected = store.read().dump().expect("snapshot");
    drop(store);
    let store = CellStore::open(&path, evidence_model(), StoreOptions::default()).expect("restart");
    assert_eq!(store.read().dump().expect("restarted snapshot"), expected);
    let mut exporter =
        norn_cell::export::Exporter::local(&directory.path().join("export")).expect("exporter");
    for seq in 1..=store.export_commit_count().expect("commit count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("export");
    }
    let target = directory.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        evidence_model(),
        SCOPE,
        &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
        StoreOptions::default(),
    )
    .await
    .expect("typed replay restores");
    let restored = CellStore::open(&target, evidence_model(), StoreOptions::default())
        .expect("restored store");
    assert_eq!(restored.read().dump().expect("restored snapshot"), expected);
}

#[test]
fn replacing_provenance_in_one_batch_preserves_every_immutable_version() {
    let mut store = CellStore::open_in_memory(evidence_model()).expect("store");
    let provenance = evidence_provenance(&[HEALTH], Vec::new());
    let FactChange::Create { values, .. } = evidence_create("unused", provenance.clone()) else {
        panic!("create fixture");
    };
    let mut replaced = values.clone();
    replaced.insert("provenance".to_owned(), runtime(json!({})));
    store
        .commit(&CommitBatch {
            writes: vec![
                FactWriteRow::Put {
                    object_type: "marking.Evidence".to_owned(),
                    key: "put".to_owned(),
                    values,
                },
                FactWriteRow::Put {
                    object_type: "marking.Evidence".to_owned(),
                    key: "put".to_owned(),
                    values: replaced,
                },
            ],
            facts: vec![
                evidence_create("fact", provenance),
                evidence_set("fact", "provenance", runtime(json!({}))),
                evidence_set("fact", "message", Value::String("replacement".to_owned())),
            ],
            ..batch(1, Vec::new())
        })
        .expect("mixed canonical batch");
    for (key, last_version) in [("put", 2), ("fact", 3)] {
        for version in 1..=last_version {
            assert_eq!(
                effective_markings(&store, "marking.Evidence", key, Some(version))["message"],
                vec![HEALTH.to_owned()],
                "replacing an envelope cannot clear labels or leave an intermediate snapshot public"
            );
        }
        assert_eq!(
            effective_markings(&store, "marking.Evidence", key, None)["message"],
            vec![HEALTH.to_owned()]
        );
    }
}

#[test]
fn canonical_fact_provenance_with_unavailable_or_malformed_lineage_fails_closed() {
    let mut store = CellStore::open_in_memory(evidence_model()).expect("store");
    let envelopes = [
        evidence_provenance(
            &[],
            vec![norn_ir::provenance::FactRef::new(
                "marking.Source",
                "absent",
                1,
            )],
        ),
        evidence_provenance(
            &[],
            vec![norn_ir::provenance::FactRef::new(
                "marking.Source",
                "absent",
                0,
            )],
        ),
        runtime(
            json!({"source": {"connector":"internal", "recordId":"evidence"}, "markings": [42]}),
        ),
    ];
    for (index, provenance) in envelopes.into_iter().enumerate() {
        let create_key = format!("create-{index}");
        let set_key = format!("set-{index}");
        store
            .commit(&CommitBatch {
                facts: vec![
                    evidence_create(&create_key, provenance.clone()),
                    evidence_create(&set_key, runtime(json!({}))),
                    evidence_set(&set_key, "provenance", provenance),
                ],
                ..batch(
                    u64::try_from(index)
                        .expect("index")
                        .checked_add(1)
                        .expect("logical time"),
                    Vec::new(),
                )
            })
            .expect("unknown lineage is retained");
        for (key, version) in [(&create_key, 1), (&set_key, 2)] {
            for selected in [None, Some(version)] {
                assert_eq!(
                    effective_markings(&store, "marking.Evidence", key, selected)["message"],
                    vec!["norn.unknown-lineage".to_owned()]
                );
            }
        }
    }
}

#[test]
fn provenance_referencing_a_source_changed_in_the_batch_keeps_uncertainty() {
    let mut store = CellStore::open_in_memory(evidence_model()).expect("store");
    store
        .commit(&CommitBatch {
            events: vec![mapping_event(
                "marking.Source",
                "S-1",
                &json!({"email":{"markings":[HEALTH]}}),
            )],
            facts: vec![evidence_create(
                "derived",
                evidence_provenance(
                    &[],
                    vec![norn_ir::provenance::FactRef::new(
                        "marking.Source",
                        "S-1",
                        1,
                    )],
                ),
            )],
            ..batch(
                1,
                vec![put(
                    "marking.Source",
                    "S-1",
                    &[("email", "private@example.test")],
                )],
            )
        })
        .expect("source and derived fact");
    assert_eq!(
        effective_markings(&store, "marking.Evidence", "derived", Some(1))["message"],
        vec![
            "norn.unknown-lineage".to_owned(),
            CONTACT.to_owned(),
            HEALTH.to_owned()
        ],
        "uncertainty stays protected while the final pass additionally inherits mapping labels"
    );
}

#[test]
fn naming_series_identity_inherits_runtime_scope_markings_without_fact_refs() {
    let model = model(&[
        json!({
            "name": "marking.GeneratedInvoice",
            "properties": [
                {"id": "number", "type": {"kind": "text"}},
                {"id": "tenant", "type": {"kind": "text"}}
            ],
            "identity": [{
                "name": "business",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "INV-{tenant}-{#####}",
                    "timezone": "Etc/UTC",
                    "scopeFields": ["tenant"],
                    "resetPolicy": "never",
                    "gapPolicy": "gapless_within_commit"
                }
            }]
        }),
        json!({
            "name": "marking.PublicInvoice",
            "properties": [
                {"id": "number", "type": {"kind": "text"}},
                {"id": "tenant", "type": {"kind": "text"}}
            ],
            "identity": [{
                "name": "business",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "PUB-{tenant}-{#####}",
                    "timezone": "Etc/UTC",
                    "scopeFields": ["tenant"],
                    "resetPolicy": "never",
                    "gapPolicy": "gapless_within_commit"
                }
            }]
        }),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store opens");
    let mut mapped = batch(
        1,
        vec![
            put(
                "marking.GeneratedInvoice",
                "invoice-1",
                &[("number", "INV-acme-00001"), ("tenant", "acme")],
            ),
            put(
                "marking.PublicInvoice",
                "public-invoice-1",
                &[("number", "PUB-acme-00001"), ("tenant", "acme")],
            ),
        ],
    );
    // Runtime labels come directly from the scope property's provenance. No
    // source FactRef is needed to classify the generated identity component.
    mapped.events = vec![mapping_event(
        "marking.GeneratedInvoice",
        "invoice-1",
        &json!({"tenant": {"markings": [HEALTH]}}),
    )];
    store.commit(&mapped).expect("mapped invoices commit");

    let generated = effective_markings(&store, "marking.GeneratedInvoice", "invoice-1", None);
    assert_eq!(generated["tenant"], vec![HEALTH.to_owned()]);
    assert_eq!(generated["number"], vec![HEALTH.to_owned()]);
    assert!(!generated["number"].contains(&"norn.unknown-lineage".to_owned()));

    let public = effective_markings(&store, "marking.PublicInvoice", "public-invoice-1", None);
    assert!(
        public["number"].is_empty(),
        "an unclassified key stays public"
    );
}

fn reincarnation_model() -> Model {
    model(&[
        json!({"name":"marking.Public","properties":[{"id":"value","type":{"kind":"text"}}]}),
        json!({"name":"marking.Private","properties":[{"id":"value","type":{"kind":"text"},"markings":[CONTACT]}]}),
    ])
}

fn assert_reincarnation_markings(store: &CellStore, object_type: &str, classified: bool) {
    let expected = if classified {
        vec![CONTACT.to_owned()]
    } else {
        Vec::new()
    };
    for version in [None, Some(1), Some(2), Some(3)] {
        assert_eq!(
            effective_markings(store, object_type, "same-key", version)["value"],
            expected
        );
    }
}

#[tokio::test]
async fn public_and_classified_delete_recreate_preserve_tombstone_lineage_after_restore() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let mut store =
        CellStore::open(&path, reincarnation_model(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    for (index, object_type) in ["marking.Public", "marking.Private"]
        .into_iter()
        .enumerate()
    {
        let time = u64::try_from(index)
            .expect("index")
            .checked_mul(3)
            .expect("time");
        store
            .commit(&batch(
                time.checked_add(1).expect("create time"),
                vec![put(object_type, "same-key", &[("value", "first")])],
            ))
            .expect("create");
        store
            .commit(&batch(
                time.checked_add(2).expect("delete time"),
                vec![FactWriteRow::Delete {
                    object_type: object_type.to_owned(),
                    key: "same-key".to_owned(),
                }],
            ))
            .expect("delete");
        assert!(
            store
                .read()
                .object(MAIN_WORLD, object_type, "same-key")
                .expect("read")
                .is_none()
        );
        store
            .commit(&batch(
                time.checked_add(3).expect("recreate time"),
                vec![put(object_type, "same-key", &[("value", "recreated")])],
            ))
            .expect("recreate");
        assert_eq!(
            store
                .read()
                .version(MAIN_WORLD, object_type, "same-key")
                .expect("version"),
            3
        );
        assert_reincarnation_markings(&store, object_type, index != 0);
    }
    let expected = store.read().dump().expect("snapshot");
    let mut exporter =
        norn_cell::export::Exporter::local(&directory.path().join("export")).expect("exporter");
    for seq in 1..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("export");
    }
    let target = directory.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        reincarnation_model(),
        SCOPE,
        &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored =
        CellStore::open(&target, reincarnation_model(), StoreOptions::default()).expect("restored");
    assert_eq!(restored.read().dump().expect("snapshot"), expected);
    assert_reincarnation_markings(&restored, "marking.Public", false);
    assert_reincarnation_markings(&restored, "marking.Private", true);
}

#[test]
fn ancestor_sets_close_subtype_computed_markings_on_every_immutable_version() {
    let model = model(&[
        json!({"name":"marking.Input","properties":[{"id":"secret","type":{"kind":"text"},"markings":[CONTACT]}]}),
        json!({"name":"marking.Base","properties":[{"id":"source","type":{"kind":"text"}}]}),
        json!({"name":"marking.Child","extends":"marking.Base","properties":[
            {"id":"derived","type":{"kind":"text"},"computed":"self.source"},
            {"id":"transitive","type":{"kind":"text"},"computed":"self.derived"}
        ]}),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.Input", "input", &[("secret", "private")]),
                put("marking.Child", "child", &[("source", "public")]),
            ],
        ))
        .expect("seed");
    let mutation = |value: &str| FactChange::Set {
        object_type: ObjectTypeName("marking.Base".to_owned()),
        key: "child".to_owned(),
        property: PropertyId("source".to_owned()),
        value: Value::String(value.to_owned()),
    };
    store
        .commit(&CommitBatch {
            marking_reads: vec![norn_kernel::FactExpectation {
                key: norn_kernel::FactKey::new("marking.Input", "input"),
                expected: norn_kernel::FactVersion(1),
            }],
            facts: vec![mutation("private first"), mutation("private second")],
            ..batch(2, Vec::new())
        })
        .expect("ancestor addressed repeated sets");
    for property in ["source", "derived", "transitive"] {
        assert!(effective_markings(&store, "marking.Child", "child", Some(1))[property].is_empty());
        for version in [None, Some(2), Some(3)] {
            assert_eq!(
                effective_markings(&store, "marking.Child", "child", version)[property],
                vec![CONTACT.to_owned()]
            );
        }
    }
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "marking.Base", "child")
            .expect("row")
            .expect("child")
            .values["source"],
        Value::String("private second".to_owned())
    );
}

#[test]
fn pre_marking_store_with_public_and_classified_live_facts_is_refused_atomically() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("legacy.sqlite");
    let model = reincarnation_model();
    // Construct the actual old schema without invoking the marking writer.
    let raw = rusqlite::Connection::open(&path).expect("legacy file");
    for ddl in norn_cell::system::SYSTEM_DDL
        .iter()
        .filter(|ddl| !ddl.contains("_norn_marking_"))
    {
        raw.execute_batch(ddl).expect("pre-marking system schema");
    }
    for ddl in model.schema().statements() {
        raw.execute_batch(ddl).expect("fact schema");
    }
    raw.execute("INSERT INTO _norn_meta (key, value) VALUES ('format', 'norn.cell.store/2'), ('ddl_hash', ?1)", [model.ddl_hash()]).expect("old format");
    for object_type in ["marking.Public", "marking.Private"] {
        raw.execute(&format!("INSERT INTO \"o:{object_type}\" (_world, _id, _type, _scope, _version, value) VALUES (?1, 'legacy', ?2, ?3, 1, 'retained raw value')"), rusqlite::params![MAIN_WORLD, object_type, SCOPE]).expect("old live row");
        raw.execute("INSERT INTO _norn_version (world, root_type, id, version) VALUES (?1, ?2, 'legacy', 1)", rusqlite::params![MAIN_WORLD, object_type]).expect("old high-water version");
    }
    drop(raw);
    assert!(
        matches!(CellStore::open(&path, model, StoreOptions::default()), Err(CellError::StoreFormat { found, expected }) if found == "norn.cell.store/2" && expected == "norn.cell.store/7")
    );
    let raw = rusqlite::Connection::open(&path).expect("refused file remains readable");
    let tables: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '_norn_marking_%'",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    assert_eq!(tables, 0, "refusal rolls back marking DDL");
    for object_type in ["marking.Public", "marking.Private"] {
        let value: String = raw
            .query_row(
                &format!("SELECT value FROM \"o:{object_type}\" WHERE _id = 'legacy'"),
                [],
                |row| row.get(0),
            )
            .expect("retained fact");
        assert_eq!(value, "retained raw value");
    }
}

#[test]
fn ancestor_action_and_command_task_use_complete_public_and_classified_subtype_snapshots() {
    let mut types = system_lineage_model()
        .object_types()
        .cloned()
        .collect::<Vec<_>>();
    for (name, base) in [
        ("marking.PublicChild", "marking.UnmarkedTarget"),
        ("marking.PrivateChild", "marking.MarkedTarget"),
    ] {
        types.push(
            serde_json::from_value(json!({"name":name,"extends":base,"properties":[]}))
                .expect("child"),
        );
    }
    let model = Model::from_definitions(&types, &[]).expect("model");
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.PublicChild", "public", &[("value", "public")]),
                put(
                    "marking.PrivateChild",
                    "private",
                    &[("contact", "private contact"), ("health", "private health")],
                ),
            ],
        ))
        .expect("targets");
    store
        .commit(&batch(
            2,
            vec![
                action_call("public-call", "marking.UnmarkedTarget", "public"),
                command_task(
                    "public-task",
                    "public-call",
                    "marking.UnmarkedTarget",
                    "public",
                ),
                action_call("private-call", "marking.MarkedTarget", "private"),
                command_task(
                    "private-task",
                    "private-call",
                    "marking.MarkedTarget",
                    "private",
                ),
            ],
        ))
        .expect("ancestor action targets");
    for (object_type, key, labels) in [
        ("norn.ActionCall", "public-call", vec![]),
        ("norn.CommandTask", "public-task", vec![]),
        (
            "norn.ActionCall",
            "private-call",
            vec![CONTACT.to_owned(), HEALTH.to_owned()],
        ),
        (
            "norn.CommandTask",
            "private-task",
            vec![CONTACT.to_owned(), HEALTH.to_owned()],
        ),
    ] {
        for version in [None, Some(1)] {
            assert!(
                effective_markings(&store, object_type, key, version)
                    .values()
                    .all(|actual| *actual == labels)
            );
        }
    }
}

#[test]
fn fact_trigger_lineage_uses_its_immutable_source_without_unrelated_staging_labels() {
    for source_event in [
        json!({"op":"created","key":{"objectType":"marking.Public","key":"source"}, "values":{"value":"public value"}}),
        json!({"op":"observed_fact","event":{"op":"created","key":{"objectType":"marking.Public","key":"source"}, "values":{"value":"public value"}},"provenance":{}}),
        json!({"op":"observed_transition","key":{"objectType":"marking.Public","key":"source"},"property":"status","transition":"submit","from":"draft","to":"submitted","provenance":{}}),
        json!({"op":"observed_deleted","key":{"objectType":"marking.Public","key":"source"},"tombstoned":true,"provenance":{}}),
    ] {
        let model = model(&[
            json!({"name":"marking.Public","properties":[{"id":"value","type":{"kind":"text"}}]}),
            json!({"name":"norn.ObservationHead","properties":[{"id":"payload","type":{"kind":"text"}}]}),
        ]);
        let mut store = CellStore::open_in_memory(model).expect("store");
        let mut source = batch(
            1,
            vec![
                put("marking.Public", "source", &[("value", "public value")]),
                put(
                    "norn.ObservationHead",
                    "staging",
                    &[("payload", "unattributed staging")],
                ),
            ],
        );
        let proven = matches!(
            source_event.get("op").and_then(serde_json::Value::as_str),
            Some("created" | "observed_fact")
        );
        source.events = vec![runtime(source_event)];
        store
            .commit(&source)
            .expect("public fact and unknown staging share a receipt");
        let entry = store
            .read()
            .ledger(0)
            .expect("ledger")
            .pop()
            .expect("source event");
        assert_eq!(
            store
                .read()
                .ledger_markings(&entry)
                .expect("receipt lineage"),
            proven.then(|| std::collections::BTreeSet::from(["norn.unknown-lineage".to_owned()]))
        );
        assert_eq!(
            store
                .read()
                .fact_event_markings(&entry)
                .expect("source lineage"),
            proven.then(std::collections::BTreeSet::new)
        );
        store
            .commit(&CommitBatch {
                marking_events: vec![entry.seq],
                ..batch(
                    2,
                    vec![put("marking.Public", "copy", &[("value", "public value")])],
                )
            })
            .expect("fact-derived output");
        for version in [None, Some(1)] {
            assert_eq!(
                effective_markings(&store, "marking.Public", "copy", version)["value"].is_empty(),
                proven
            );
        }
    }
}

#[test]
fn repeated_and_canonical_system_writes_classify_every_immutable_version() {
    let mut store = CellStore::open_in_memory(system_lineage_model()).expect("store");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.UnmarkedTarget", "public", &[("value", "public")]),
                put(
                    "marking.MarkedTarget",
                    "private",
                    &[("contact", "email"), ("health", "diagnosis")],
                ),
            ],
        ))
        .expect("targets");
    let FactWriteRow::Put {
        object_type,
        key,
        values,
    } = action_call("canonical", "marking.MarkedTarget", "private")
    else {
        panic!("put")
    };
    store
        .commit(&CommitBatch {
            facts: vec![FactChange::Create {
                object_type: ObjectTypeName(object_type),
                key,
                values,
            }],
            ..batch(
                2,
                vec![
                    action_call("repeated", "marking.MarkedTarget", "private"),
                    action_call("repeated", "marking.UnmarkedTarget", "public"),
                    command_task("task", "repeated", "marking.UnmarkedTarget", "public"),
                ],
            )
        })
        .expect("ordered system records");
    for (object_type, key, versions) in [
        ("norn.ActionCall", "repeated", vec![None, Some(1), Some(2)]),
        ("norn.ActionCall", "canonical", vec![None, Some(1)]),
        ("norn.CommandTask", "task", vec![None, Some(1)]),
    ] {
        for version in versions {
            assert!(
                effective_markings(&store, object_type, key, version)
                    .values()
                    .all(|labels| labels == &[CONTACT.to_owned(), HEALTH.to_owned()])
            );
        }
    }
}

#[test]
fn repeated_mapping_writes_close_computed_classifications_on_each_immutable_version() {
    let model = model(&[json!({"name":"marking.Mapped","properties":[
        {"id":"input","type":{"kind":"text"}},
        {"id":"derived","type":{"kind":"text"},"computed":"self.input"}
    ]})]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    let mut mapped = batch(
        1,
        vec![
            put("marking.Mapped", "row", &[("input", "first secret")]),
            put("marking.Mapped", "row", &[("input", "second secret")]),
        ],
    );
    mapped.events = vec![mapping_event(
        "marking.Mapped",
        "row",
        &json!({"input":{"markings":[CONTACT]}}),
    )];
    store.commit(&mapped).expect("repeated mapping");
    for version in [None, Some(1), Some(2)] {
        for property in ["input", "derived"] {
            assert_eq!(
                effective_markings(&store, "marking.Mapped", "row", version)[property],
                vec![CONTACT.to_owned()]
            );
        }
    }
}

#[test]
fn mixed_declassification_receipt_keeps_its_immutable_subject_lineage() {
    let model = model(&[
        json!({"name":"marking.Private","properties":[{"id":"value","type":{"kind":"text"},"markings":[CONTACT]}]}),
        json!({"name":"marking.Public","properties":[{"id":"value","type":{"kind":"text"}}]}),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&batch(
            1,
            vec![put("marking.Private", "subject", &[("value", "secret")])],
        ))
        .expect("subject");
    let change = MarkingDeclassification {
        object_type: "marking.Private".to_owned(),
        key: "subject".to_owned(),
        property: "value".to_owned(),
        expected_version: 1,
        markings: vec![CONTACT.to_owned()],
        actor: "privacy.officer".to_owned(),
        reason: "private reason".to_owned(),
    };
    store
        .commit(&CommitBatch {
            events: vec![change.ledger_event().expect("event")],
            marking_declassifications: vec![change],
            ..batch(
                2,
                vec![put("marking.Public", "unrelated", &[("value", "public")])],
            )
        })
        .expect("mixed batch");
    let entry = store
        .read()
        .ledger(0)
        .expect("ledger")
        .pop()
        .expect("event");
    assert_eq!(
        store
            .read()
            .ledger_markings(&entry)
            .expect("receipt lineage"),
        Some(std::collections::BTreeSet::from([CONTACT.to_owned()]))
    );
    assert!(effective_markings(&store, "marking.Private", "subject", None)["value"].is_empty());
    assert_eq!(
        effective_markings(&store, "marking.Private", "subject", Some(1))["value"],
        vec![CONTACT.to_owned()]
    );
}

#[test]
fn deleted_classified_rows_do_not_conceal_live_public_marking_summaries() {
    let mut store = CellStore::open_in_memory(model(&[
        json!({"name":"marking.Row","properties":[{"id":"value","type":{"kind":"text"}}]}),
    ]))
    .expect("store");
    let mut first = batch(
        1,
        vec![
            put("marking.Row", "private", &[("value", "secret")]),
            put("marking.Row", "public", &[("value", "public")]),
        ],
    );
    first.events = vec![mapping_event(
        "marking.Row",
        "private",
        &json!({"value":{"markings":[CONTACT]}}),
    )];
    store.commit(&first).expect("seed");
    store
        .commit(&batch(
            2,
            vec![FactWriteRow::Delete {
                object_type: "marking.Row".to_owned(),
                key: "private".to_owned(),
            }],
        ))
        .expect("delete");
    assert_eq!(
        store
            .read()
            .property_marking_summary(MAIN_WORLD, "marking.Row", "value")
            .expect("summary"),
        Some(std::collections::BTreeSet::new())
    );
    assert_eq!(
        effective_markings(&store, "marking.Row", "private", Some(2))["value"],
        vec![CONTACT.to_owned()]
    );
}

#[test]
fn repeated_absent_deletes_preserve_immutable_lineage_without_live_marking_rows() {
    let mut store = CellStore::open_in_memory(reincarnation_model()).expect("store");
    for (object_type, labels) in [
        ("marking.Public", vec![]),
        ("marking.Private", vec![CONTACT.to_owned()]),
    ] {
        let now = store.read().clock_floor().expect("cell clock");
        let delete = || FactWriteRow::Delete {
            object_type: object_type.to_owned(),
            key: "absent".to_owned(),
        };
        store
            .commit(&batch(now.saturating_add(1), vec![delete(), delete()]))
            .expect("repeated absent deletes");
        for version in [1, 2] {
            assert_eq!(
                effective_markings(&store, object_type, "absent", Some(version))["value"],
                labels
            );
        }
        assert_eq!(
            store
                .read()
                .property_marking_summary(MAIN_WORLD, object_type, "value")
                .expect("no live rows"),
            Some(std::collections::BTreeSet::new())
        );
        store
            .commit(&batch(
                now.saturating_add(2),
                vec![put(object_type, "absent", &[("value", "created")])],
            ))
            .expect("create after absence");
        assert_eq!(
            store
                .read()
                .version(MAIN_WORLD, object_type, "absent")
                .expect("version"),
            3
        );
        assert_eq!(
            effective_markings(&store, object_type, "absent", None)["value"],
            labels
        );
        store
            .commit(&batch(
                now.saturating_add(3),
                vec![
                    delete(),
                    delete(),
                    put(object_type, "absent", &[("value", "recreated")]),
                ],
            ))
            .expect("delete absent delete recreate");
        for version in [4, 5, 6] {
            assert_eq!(
                effective_markings(&store, object_type, "absent", Some(version))["value"],
                labels
            );
        }
        assert_eq!(
            effective_markings(&store, object_type, "absent", None)["value"],
            labels
        );
    }
}

#[test]
fn repeated_ancestor_deletes_retain_the_concrete_subtype_snapshot() {
    let model = model(&[
        json!({"name":"marking.Base","properties":[{"id":"value","type":{"kind":"text"}}]}),
        json!({"name":"marking.PublicChild","extends":"marking.Base","properties":[]}),
        json!({"name":"marking.PrivateChild","extends":"marking.Base","properties":[{"id":"private","type":{"kind":"text"},"constraints":{"nullable":true},"markings":[CONTACT]}]}),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    for (key, concrete, labels) in [
        ("public", "marking.PublicChild", vec![]),
        ("private", "marking.PrivateChild", vec![CONTACT.to_owned()]),
    ] {
        let now = store.read().clock_floor().expect("cell clock");
        store
            .commit(&batch(
                now.saturating_add(1),
                vec![put(concrete, key, &[("value", "value")])],
            ))
            .expect("child");
        let delete = || FactWriteRow::Delete {
            object_type: "marking.Base".to_owned(),
            key: key.to_owned(),
        };
        store
            .commit(&batch(
                now.saturating_add(2),
                vec![
                    delete(),
                    delete(),
                    put(concrete, key, &[("value", "recreated")]),
                ],
            ))
            .expect("ancestor deletes and concrete recreate");
        for version in [None, Some(1), Some(2), Some(3), Some(4)] {
            let snapshot = effective_markings(&store, concrete, key, version);
            assert!(snapshot["value"].is_empty());
            if concrete == "marking.PrivateChild" {
                assert_eq!(snapshot["private"], labels);
            }
            assert!(
                !snapshot
                    .values()
                    .flatten()
                    .any(|label| label == "norn.unknown-lineage")
            );
        }
    }
}

#[test]
fn sibling_recreation_inherits_public_classified_and_late_mapping_tombstone_lineage() {
    let model = model(&[
        json!({"name":"marking.Base","properties":[{"id":"value","type":{"kind":"text"}}]}),
        json!({"name":"marking.First","extends":"marking.Base","properties":[]}),
        json!({"name":"marking.PrivateFirst","extends":"marking.Base","properties":[{"id":"private","type":{"kind":"text"},"constraints":{"nullable":true},"markings":[CONTACT]}]}),
        json!({"name":"marking.Second","extends":"marking.Base","properties":[{"id":"extra","type":{"kind":"text"},"constraints":{"nullable":true}}]}),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    for (key, first_type, dynamic, labels) in [
        ("public", "marking.First", false, vec![]),
        (
            "private",
            "marking.PrivateFirst",
            false,
            vec![CONTACT.to_owned()],
        ),
        ("dynamic", "marking.First", true, vec![CONTACT.to_owned()]),
    ] {
        let mut changes = batch(
            1,
            vec![
                put(first_type, key, &[("value", "first")]),
                FactWriteRow::Delete {
                    object_type: "marking.Base".to_owned(),
                    key: key.to_owned(),
                },
                put("marking.Second", key, &[("value", "second")]),
            ],
        );
        if dynamic {
            changes.events = vec![mapping_event(
                first_type,
                key,
                &json!({"value":{"markings":[CONTACT]}}),
            )];
        }
        store.commit(&changes).expect("sibling recreation");
        for version in [1, 2] {
            let union = effective_markings(&store, first_type, key, Some(version))
                .into_values()
                .flatten()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(union, labels.iter().cloned().collect());
        }
        for version in [None, Some(3)] {
            assert!(
                effective_markings(&store, "marking.Second", key, version)
                    .values()
                    .all(|actual| *actual == labels)
            );
        }
    }
}

#[test]
fn sibling_deletes_refuse_atomically_without_changing_live_versions_or_markings() {
    for classified in [false, true] {
        let model = model(&[
            json!({"name":"marking.Base","properties":[{"id":"value","type":{"kind":"text"},"markings":if classified { vec![CONTACT] } else { vec![] }}]}),
            json!({"name":"marking.First","extends":"marking.Base","properties":[]}),
            json!({"name":"marking.Second","extends":"marking.Base","properties":[]}),
        ]);
        let mut store = CellStore::open_in_memory(model).expect("store");
        store
            .commit(&batch(
                1,
                vec![put("marking.First", "live", &[("value", "steady")])],
            ))
            .expect("live sibling");
        let before = store.read().dump().expect("full state");
        for canonical in [false, true] {
            let mut invalid = batch(
                2,
                vec![put("marking.First", "earlier", &[("value", "rollback")])],
            );
            if canonical {
                invalid.facts = vec![FactChange::Delete {
                    object_type: ObjectTypeName("marking.Second".to_owned()),
                    key: "live".to_owned(),
                }];
            } else {
                invalid.writes.push(FactWriteRow::Delete {
                    object_type: "marking.Second".to_owned(),
                    key: "live".to_owned(),
                });
            }
            assert!(
                matches!(store.commit(&invalid), Err(CellError::ObjectMissing { object_type, .. }) if object_type == "marking.Second")
            );
            assert_eq!(store.read().dump().expect("rolled back full state"), before);
            let record = store
                .read()
                .object(MAIN_WORLD, "marking.First", "live")
                .expect("live sibling")
                .expect("still live");
            assert_eq!(record.version, 1);
            assert_eq!(record.values["value"].as_str(), Some("steady"));
            assert_eq!(
                store
                    .read()
                    .version(MAIN_WORLD, "marking.First", "live")
                    .expect("high water"),
                1
            );
            for version in [None, Some(1)] {
                assert_eq!(
                    effective_markings(&store, "marking.First", "live", version)["value"],
                    if classified {
                        vec![CONTACT.to_owned()]
                    } else {
                        vec![]
                    }
                );
            }
        }
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario verifies occurrence, property, trigger, restart and restore state together"
)]
async fn noop_put_occurrence_lineage_preserves_values_and_survives_restart_and_restore() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let model = model(&[
        json!({"name":"marking.Source","properties":[{"id":"email","type":{"kind":"text"},"markings":[CONTACT]}]}),
        json!({"name":"marking.Evidence","properties":[{"id":"message","type":{"kind":"text"}}]}),
    ]);
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    store
        .commit(&batch(
            1,
            vec![put("marking.Source", "source", &[("email", "secret")])],
        ))
        .expect("source");
    for (key, expected_source, labels) in [
        ("classified", 1, vec![CONTACT.to_owned()]),
        ("unresolved", 999, vec!["norn.unknown-lineage".to_owned()]),
        ("public", 0, vec![]),
    ] {
        let now = store.read().clock_floor().expect("cell clock");
        let write = || put("marking.Evidence", key, &[("message", "steady")]);
        store
            .commit(&batch(now.saturating_add(1), vec![write()]))
            .expect("public target");
        let mut noop = batch(now.saturating_add(2), vec![write()]);
        noop.marking_reads = vec![norn_kernel::FactExpectation {
            key: norn_kernel::FactKey::new("marking.Source", "source"),
            expected: norn_kernel::FactVersion(expected_source),
        }];
        noop.events = vec![runtime(
            json!({"op":"set","key":{"objectType":"marking.Evidence","key":key},"property":"message","value":"steady"}),
        )];
        let receipt = store.commit(&noop).expect("no-op").receipt().clone();
        let event = store
            .read()
            .ledger_page(receipt.body.first_event_seq - 1, 1)
            .expect("no-op event")
            .remove(0);
        assert_eq!(
            store
                .read()
                .fact_event_markings(&event)
                .expect("immutable occurrence"),
            Some(labels.iter().cloned().collect())
        );
        assert_eq!(
            store
                .read()
                .ledger_markings(&event)
                .expect("receipt lineage"),
            Some(labels.iter().cloned().collect())
        );
        let mut downstream = batch(
            now.saturating_add(3),
            vec![put(
                "marking.Evidence",
                &format!("{key}-derived"),
                &[("message", "flag")],
            )],
        );
        downstream.marking_events = vec![event.seq];
        store
            .commit(&downstream)
            .expect("empty-slice trigger output");
        assert_eq!(
            effective_markings(
                &store,
                "marking.Evidence",
                &format!("{key}-derived"),
                Some(1)
            )["message"],
            labels
        );
        store
            .commit(&batch(
                now.saturating_add(4),
                vec![put("marking.Evidence", key, &[("message", "later")])],
            ))
            .expect("later version retains occurrence");
        assert!(
            store
                .read()
                .fact_occurrence_markings(MAIN_WORLD, "marking.Evidence", key, Some(1))
                .expect("public history")
                .expect("complete")
                .is_empty()
        );
        for version in [None, Some(2), Some(3)] {
            assert!(
                effective_markings(&store, "marking.Evidence", key, version)
                    .values()
                    .all(Vec::is_empty)
            );
            assert_eq!(
                store
                    .read()
                    .fact_occurrence_markings(MAIN_WORLD, "marking.Evidence", key, version)
                    .expect("occurrence")
                    .expect("complete"),
                labels.iter().cloned().collect()
            );
        }
    }
    let expected = store.read().dump().expect("full state");
    drop(store);
    let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("restart");
    assert_eq!(store.read().dump().expect("restarted state"), expected);
    let mut exporter =
        norn_cell::export::Exporter::local(&directory.path().join("export")).expect("exporter");
    for seq in 1..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("export");
    }
    let target = directory.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        model.clone(),
        SCOPE,
        &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored = CellStore::open(&target, model, StoreOptions::default()).expect("restored");
    assert_eq!(restored.read().dump().expect("restored state"), expected);
    for (key, label) in [
        ("classified", CONTACT),
        ("unresolved", "norn.unknown-lineage"),
    ] {
        assert_eq!(
            restored
                .read()
                .fact_occurrence_markings(MAIN_WORLD, "marking.Evidence", key, None)
                .expect("restored occurrence"),
            Some(std::collections::BTreeSet::from([label.to_owned()]))
        );
    }
    // A lost occurrence row must not make an otherwise complete, public
    // property snapshot look like a public no-op event.
    let connection = rusqlite::Connection::open(&target).expect("corruption fixture");
    connection.execute(
        "DELETE FROM _norn_marking_version WHERE world = ?1 AND object_type = 'marking.Evidence' AND id = 'classified' AND version = 2 AND property = ''",
        [MAIN_WORLD],
    ).expect("remove only immutable occurrence evidence");
    let event = restored
        .read()
        .ledger(0)
        .expect("events")
        .into_iter()
        .find(|entry| {
            entry
                .event
                .get("key")
                .and_then(|key| key.get("key"))
                .and_then(Value::as_str)
                == Some("classified")
        })
        .expect("classified no-op event");
    assert_eq!(
        restored
            .read()
            .fact_event_markings(&event)
            .expect("missing occurrence fails closed"),
        None
    );
    assert_eq!(
        restored
            .read()
            .fact_occurrence_markings(MAIN_WORLD, "marking.Evidence", "classified", Some(2))
            .expect("missing immutable slot"),
        None
    );
}

#[test]
fn tasks_written_before_their_calls_inherit_complete_immutable_classifications() {
    let mut store = CellStore::open_in_memory(system_lineage_model()).expect("store");
    store
        .commit(&batch(
            1,
            vec![
                put("marking.UnmarkedTarget", "public", &[("value", "public")]),
                put(
                    "marking.MarkedTarget",
                    "private",
                    &[("contact", "email"), ("health", "diagnosis")],
                ),
            ],
        ))
        .expect("targets");
    let FactWriteRow::Put {
        object_type,
        key,
        values,
    } = action_call("canonical-call", "marking.MarkedTarget", "private")
    else {
        panic!("put")
    };
    store
        .commit(&CommitBatch {
            facts: vec![FactChange::Create {
                object_type: ObjectTypeName(object_type),
                key,
                values,
            }],
            ..batch(
                2,
                vec![
                    command_task(
                        "private-task",
                        "private-call",
                        "marking.MarkedTarget",
                        "private",
                    ),
                    command_task(
                        "public-task",
                        "public-call",
                        "marking.UnmarkedTarget",
                        "public",
                    ),
                    command_task(
                        "canonical-task",
                        "canonical-call",
                        "marking.MarkedTarget",
                        "private",
                    ),
                    action_call("public-call", "marking.UnmarkedTarget", "public"),
                    action_call("private-call", "marking.MarkedTarget", "private"),
                ],
            )
        })
        .expect("tasks precede calls");
    for (key, labels) in [
        ("public-task", vec![]),
        ("private-task", vec![CONTACT.to_owned(), HEALTH.to_owned()]),
        (
            "canonical-task",
            vec![CONTACT.to_owned(), HEALTH.to_owned()],
        ),
    ] {
        for version in [None, Some(1)] {
            assert!(
                effective_markings(&store, "norn.CommandTask", key, version)
                    .values()
                    .all(|actual| *actual == labels)
            );
        }
    }
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "norn.CommandTask", "private-task")
            .expect("record")
            .expect("present")
            .values["objectKey"]
            .as_str(),
        Some("private")
    );
}

fn source_classified_install(
    model: &Model,
    marked: bool,
    rename_field: bool,
) -> norn_cell::InstalledDefinitionInput {
    let mut input = install_input(
        if marked {
            "source-marked"
        } else {
            "source-unmarked"
        },
        if marked { 1 } else { 3 },
        model,
    );
    let mut artifacts: Vec<JsonValue> =
        serde_json::from_str(&input.canonical_set).expect("canonical artifacts");
    let mut source: JsonValue = serde_json::from_str(include_str!(
        "../../../fixtures/ir/synthetic-contact.source_schema.json"
    ))
    .expect("source schema");
    let field = source["payload"]["fields"]
        .as_array_mut()
        .expect("fields")
        .iter_mut()
        .find(|field| field["name"] == "customer")
        .expect("customer field");
    field["markings"] = if marked { json!([CONTACT]) } else { json!([]) };
    if rename_field {
        field["name"] = json!("renamed_customer");
    }
    artifacts.push(source);
    input.canonical_set = String::from_utf8(
        runtime(json!(artifacts))
            .canonical_json()
            .expect("canonical set"),
    )
    .expect("UTF-8");
    input
}

#[test]
fn source_field_marking_removal_refuses_unchanged_object_model_atomically() {
    let model = model(&[
        json!({"name":"marking.Contact","properties":[{"id":"email","type":{"kind":"text"}}]}),
    ]);
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(
            model.clone(),
            source_classified_install(&model, true, false),
            Some(0),
            SCOPE,
        )
        .expect("classified source installs");
    let mut mapped = batch(
        2,
        vec![put(
            "marking.Contact",
            "existing",
            &[("email", "private@example.test")],
        )],
    );
    mapped.events = vec![mapping_event(
        "marking.Contact",
        "existing",
        &json!({"email":{"markings":[CONTACT]}}),
    )];
    store
        .commit(&mapped)
        .expect("source-derived classification");
    let before = store.read().dump().expect("snapshot");
    for rename in [false, true] {
        let candidate = source_classified_install(&model, false, rename);
        assert!(matches!(
            store.check_definitions(&model, &candidate, Some(1)),
            Err(CellError::DefinitionMigrationRefused(message)) if message.contains("removes source-derived marking")
        ));
        assert!(matches!(
            store.install_definitions(model.clone(), candidate, Some(1), SCOPE),
            Err(CellError::DefinitionMigrationRefused(message)) if message.contains("removes source-derived marking")
        ));
        assert_eq!(store.read().dump().expect("unchanged snapshot"), before);
        for version in [None, Some(1)] {
            assert_eq!(
                effective_markings(&store, "marking.Contact", "existing", version)["email"],
                vec![CONTACT.to_owned()]
            );
        }
    }
    let mut removed = install_input("source-removed", 3, &model);
    removed.diff = json!({"removed":{"count":1}});
    assert!(matches!(
        store.install_definitions(model, removed, Some(1), SCOPE),
        Err(CellError::DefinitionMigrationRefused(message)) if message.contains("removes source-derived marking")
    ));
    assert_eq!(
        store
            .read()
            .dump()
            .expect("source removal refused atomically"),
        before
    );
}

#[test]
fn source_field_marking_addition_refuses_live_and_deleted_immutable_lineage_atomically() {
    let model = model(&[
        json!({"name":"marking.Contact","properties":[{"id":"email","type":{"kind":"text"}}]}),
    ]);
    for deleted in [false, true] {
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store
            .install_definitions(
                model.clone(),
                source_classified_install(&model, false, false),
                Some(0),
                SCOPE,
            )
            .expect("unmarked source installs before observations");
        for (time, email) in [(4, "first@example.test"), (5, "later@example.test")] {
            let mut mapped = batch(
                time,
                vec![put("marking.Contact", "existing", &[("email", email)])],
            );
            mapped.events = vec![mapping_event(
                "marking.Contact",
                "existing",
                &json!({"email":{"markings":[]}}),
            )];
            store.commit(&mapped).expect("unmarked mapped value");
        }
        if deleted {
            store
                .commit(&batch(
                    6,
                    vec![FactWriteRow::Delete {
                        object_type: "marking.Contact".to_owned(),
                        key: "existing".to_owned(),
                    }],
                ))
                .expect("only historical versions remain");
        }
        let before = store.read().dump().expect("snapshot");
        let mut candidate = source_classified_install(&model, true, false);
        candidate.installed_at = 7;
        assert!(matches!(
            store.check_definitions(&model, &candidate, Some(1)),
            Err(CellError::DefinitionMigrationRefused(message)) if message.contains("adds source-derived marking")
        ));
        assert!(matches!(
            store.install_definitions(model.clone(), candidate, Some(1), SCOPE),
            Err(CellError::DefinitionMigrationRefused(message)) if message.contains("adds source-derived marking")
        ));
        assert_eq!(store.read().dump().expect("unchanged snapshot"), before);
        for version in [Some(1), Some(2)] {
            assert!(
                effective_markings(&store, "marking.Contact", "existing", version)["email"]
                    .is_empty()
            );
        }
        let record = store
            .read()
            .object(MAIN_WORLD, "marking.Contact", "existing")
            .expect("record");
        if deleted {
            assert!(record.is_none());
            assert!(
                effective_markings(&store, "marking.Contact", "existing", Some(3))["email"]
                    .is_empty()
            );
        } else {
            assert_eq!(
                record.expect("present").values["email"].as_str(),
                Some("later@example.test")
            );
            assert!(
                effective_markings(&store, "marking.Contact", "existing", None)["email"].is_empty()
            );
        }
    }
}

#[test]
fn ancestor_deletion_versions_inherit_read_and_trigger_labels_before_downstream_rules() {
    let model = model(&[
        json!({"name":"marking.Input","properties":[{"id":"value","type":{"kind":"text"},"markings":[CONTACT]}]}),
        json!({"name":"marking.Trigger","properties":[{"id":"value","type":{"kind":"text"},"markings":[HEALTH]}]}),
        json!({"name":"marking.Base","properties":[{"id":"value","type":{"kind":"text"}}]}),
        json!({"name":"marking.Child","extends":"marking.Base","properties":[{"id":"derived","type":{"kind":"text"},"computed":"self.value"}]}),
        json!({"name":"marking.Output","properties":[{"id":"flag","type":{"kind":"text"}}]}),
    ]);
    for canonical in [false, true] {
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        let mut seed = batch(
            1,
            vec![
                put(
                    "marking.Input",
                    "input",
                    &[("value", "classified predicate")],
                ),
                put(
                    "marking.Trigger",
                    "trigger",
                    &[("value", "classified event")],
                ),
                put("marking.Child", "target", &[("value", "public value")]),
            ],
        );
        seed.events = vec![runtime(
            json!({"op":"created","key":{"objectType":"marking.Trigger","key":"trigger"},"values":{"value":"classified event"}}),
        )];
        store.commit(&seed).expect("seed");
        let trigger_seq = store
            .read()
            .ledger(0)
            .expect("ledger")
            .pop()
            .expect("trigger")
            .seq;
        let mut deletion = batch(2, Vec::new());
        if canonical {
            deletion.facts = vec![FactChange::Delete {
                object_type: ObjectTypeName("marking.Base".to_owned()),
                key: "target".to_owned(),
            }];
        } else {
            deletion.writes = vec![FactWriteRow::Delete {
                object_type: "marking.Base".to_owned(),
                key: "target".to_owned(),
            }];
        }
        deletion.marking_reads = vec![norn_kernel::FactExpectation {
            key: norn_kernel::FactKey::new("marking.Input", "input"),
            expected: norn_kernel::FactVersion(1),
        }];
        deletion.marking_events = vec![trigger_seq];
        deletion.events = vec![runtime(
            json!({"op":"deleted","key":{"objectType":"marking.Base","key":"target"}}),
        )];
        store
            .commit(&deletion)
            .expect("classified conditional deletion");
        let entry = store
            .read()
            .ledger(trigger_seq)
            .expect("ledger")
            .pop()
            .expect("deletion event");
        verify_deletion_lineage(&mut store, &entry);
    }
}

fn verify_deletion_lineage(store: &mut CellStore, entry: &norn_cell::LedgerEntry) {
    let labels = std::collections::BTreeSet::from([CONTACT.to_owned(), HEALTH.to_owned()]);
    assert_eq!(
        store
            .read()
            .ledger_markings(entry)
            .expect("receipt lineage"),
        Some(labels.clone())
    );
    assert_eq!(
        store
            .read()
            .fact_event_markings(entry)
            .expect("trigger lineage"),
        Some(labels.clone())
    );
    for property in ["value", "derived"] {
        assert!(effective_markings(store, "marking.Child", "target", Some(1))[property].is_empty());
        assert_eq!(
            effective_markings(store, "marking.Child", "target", Some(2))[property],
            labels.iter().cloned().collect::<Vec<_>>()
        );
    }
    assert!(
        store
            .read()
            .object(MAIN_WORLD, "marking.Child", "target")
            .expect("read")
            .is_none()
    );
    assert!(
        store
            .read()
            .effective_markings(MAIN_WORLD, "marking.Child", "target", None)
            .expect("current")
            .is_none()
    );
    store
        .commit(&batch(
            3,
            vec![put(
                "marking.Child",
                "target",
                &[("value", "recreated public value")],
            )],
        ))
        .expect("recreate");
    store
        .commit(&CommitBatch {
            marking_events: vec![entry.seq],
            ..batch(
                4,
                vec![put("marking.Output", "output", &[("flag", "deleted")])],
            )
        })
        .expect("downstream rule copies deletion occurrence");
    for version in [None, Some(1)] {
        assert_eq!(
            effective_markings(store, "marking.Output", "output", version)["flag"],
            labels.iter().cloned().collect::<Vec<_>>()
        );
    }
    for version in [None, Some(2), Some(3)] {
        assert_eq!(
            effective_markings(store, "marking.Child", "target", version)["value"],
            labels.iter().cloned().collect::<Vec<_>>()
        );
    }
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "marking.Output", "output")
            .expect("read")
            .expect("output")
            .values["flag"]
            .as_str(),
        Some("deleted")
    );
}

#[test]
fn orphan_and_additional_declassification_event_lookalikes_refuse_atomically() {
    let model = model(&[
        json!({"name":"marking.Input","properties":[{"id":"value","type":{"kind":"text"},"markings":[CONTACT]}]}),
        json!({"name":"marking.Public","properties":[{"id":"value","type":{"kind":"text"}}]}),
    ]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&batch(
            1,
            vec![
                put(
                    "marking.Input",
                    "private",
                    &[("value", "classified raw value")],
                ),
                put("marking.Public", "public", &[("value", "public value")]),
            ],
        ))
        .expect("seed");
    let change = MarkingDeclassification {
        object_type: "marking.Input".to_owned(),
        key: "private".to_owned(),
        property: "value".to_owned(),
        expected_version: 1,
        markings: vec![CONTACT.to_owned()],
        actor: "operator".to_owned(),
        reason: "Approved disclosure".to_owned(),
    };
    let public_lookalike = MarkingDeclassification {
        object_type: "marking.Public".to_owned(),
        key: "public".to_owned(),
        ..change.clone()
    }
    .ledger_event()
    .expect("lookalike");
    let with_secret = |mut event: Value| {
        let Value::Object(fields) = &mut event else {
            panic!("event must be an object")
        };
        fields.insert(
            "secretPayload".to_owned(),
            Value::String("classified raw value".to_owned()),
        );
        event
    };
    let exact = change.ledger_event().expect("canonical event");
    let changed_reason = MarkingDeclassification {
        reason: "Different audit text cannot duplicate the occurrence identity".to_owned(),
        ..change.clone()
    };
    let before = store.read().dump().expect("snapshot");
    for (events, changes) in [
        (vec![with_secret(public_lookalike.clone())], Vec::new()),
        (
            vec![exact.clone(), with_secret(public_lookalike)],
            vec![change.clone()],
        ),
        (
            vec![
                exact.clone(),
                changed_reason.ledger_event().expect("alternate event"),
            ],
            vec![change.clone(), changed_reason],
        ),
        (vec![exact.clone(), exact.clone()], vec![change.clone()]),
        (vec![exact.clone()], vec![change.clone(), change.clone()]),
        (vec![exact.clone(), with_secret(exact)], vec![change]),
    ] {
        let forged = CommitBatch {
            events,
            marking_declassifications: changes,
            ..batch(
                2,
                vec![put(
                    "marking.Public",
                    "new-output",
                    &[("value", "must roll back")],
                )],
            )
        };
        assert!(matches!(store.commit(&forged),
            Err(CellError::MarkingDeclassificationInvalid(message)) if message.contains("does not match an authorized typed change")
        ));
        assert_eq!(store.read().dump().expect("unchanged snapshot"), before);
        for version in [None, Some(1)] {
            assert_eq!(
                effective_markings(&store, "marking.Input", "private", version)["value"],
                vec![CONTACT.to_owned()]
            );
            assert!(
                effective_markings(&store, "marking.Public", "public", version)["value"].is_empty()
            );
        }
        assert!(
            store
                .read()
                .object(MAIN_WORLD, "marking.Public", "new-output")
                .expect("read")
                .is_none()
        );
    }
}

#[test]
fn definition_install_backfills_type_markings_on_propertyless_occurrences() {
    let public = model(&[json!({"name":"marking.Empty", "properties":[]})]);
    let private = model(&[json!({"name":"marking.Empty", "properties":[], "markings":[CONTACT]})]);
    let mut store = CellStore::open_in_memory(public.clone()).expect("store");
    store
        .commit(&batch(
            1,
            vec![FactWriteRow::Put {
                object_type: "marking.Empty".to_owned(),
                key: "empty".to_owned(),
                values: BTreeMap::new(),
            }],
        ))
        .expect("seed");
    store
        .install_definitions(
            private.clone(),
            install_input("private-empty", 2, &private),
            None,
            SCOPE,
        )
        .expect("install type marking");
    for version in [None, Some(1)] {
        assert_eq!(
            store
                .read()
                .fact_occurrence_markings(MAIN_WORLD, "marking.Empty", "empty", version)
                .expect("occurrence")
                .expect("complete"),
            std::collections::BTreeSet::from([CONTACT.to_owned()])
        );
    }
    assert!(matches!(
        store.install_definitions(
            public.clone(),
            install_input("public-empty", 3, &public),
            Some(1),
            SCOPE
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
}

fn install_with_mask(
    model: &Model,
    mask: JsonValue,
    at: u64,
) -> norn_cell::InstalledDefinitionInput {
    let mut input = install_input("mask", at, model);
    let mut artifacts: JsonValue = serde_json::from_str(&input.canonical_set).expect("artifacts");
    artifacts
        .as_array_mut()
        .expect("array")
        .last_mut()
        .expect("catalog")["payload"]["markings"][0]["mask"] = mask;
    input.canonical_set = String::from_utf8(
        Value::from_json(artifacts)
            .expect("runtime")
            .canonical_json()
            .expect("canonical"),
    )
    .expect("UTF-8");
    input
}

#[test]
fn mask_policy_weakening_is_refused_without_changing_retained_history() {
    for initial in [
        json!({"kind":"redacted"}),
        json!({"kind":"null"}),
        json!({"kind":"last-n","count":2}),
    ] {
        let model = contact_model();
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store
            .install_definitions(
                model.clone(),
                install_with_mask(&model, initial, 1),
                Some(0),
                SCOPE,
            )
            .expect("initial catalog");
        store
            .commit(&batch(
                2,
                vec![put(
                    "marking.Contact",
                    "alice",
                    &[("email", "alice@example.test")],
                )],
            ))
            .expect("retained fact");
        let before = store.read().dump().expect("state");
        let candidate = install_with_mask(&model, json!({"kind":"last-n","count":4}), 3);
        assert!(matches!(
            store.check_definitions(&model, &candidate, Some(1)),
            Err(CellError::DefinitionMigrationRefused(_))
        ));
        assert!(matches!(
            store.install_definitions(model.clone(), candidate, Some(1), SCOPE),
            Err(CellError::DefinitionMigrationRefused(_))
        ));
        assert_eq!(before, store.read().dump().expect("unchanged state"));
        store
            .install_definitions(
                model.clone(),
                install_with_mask(&model, json!({"kind":"redacted"}), 3),
                Some(1),
                SCOPE,
            )
            .expect("same or stronger mask");
        assert_eq!(
            effective_markings(&store, "marking.Contact", "alice", Some(1))["email"],
            vec![CONTACT.to_owned()]
        );
    }
}
