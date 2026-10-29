#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

use std::collections::BTreeMap;

use norn_cell::engine::NativeEngine;
use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, Model, StoreOptions};
use norn_ir::{FactChange, ObjectTypeName, PropertyId, RuntimeValue as Value};
use serde_json::json;

fn runtime(value: serde_json::Value) -> Value {
    Value::from_json(value).expect("runtime")
}
fn model() -> Model {
    let definitions = [
        json!({"name":"proof.Item","properties":[{"id":"value","type":{"kind":"text"}}]}),
        json!({"name":"proof.Input","properties":[{"id":"value","type":{"kind":"text"},"markings":["pii.contact"]}]}),
        json!({"name":"proof.Empty","properties":[]}),
        json!({"name":"proof.Observed","properties":[{"id":"observationJson","type":{"kind":"text"}}]}),
        json!({"name":"proof.Defaulted","properties":[{"id":"value","type":{"kind":"text"}},{"id":"optional","type":{"kind":"text"},"constraints":{"nullable":true}},{"id":"defaulted","type":{"kind":"text"},"default":"declared"}]}),
        json!({"name":"proof.State","properties":[{"id":"status","type":{"kind":"enum","values":[{"value":"draft"},{"value":"approved"}]}}],"lifecycles":[{"property":"status","initial":["draft"],"terminal":["approved"],"transitions":[{"id":"approve","from":["draft"],"to":"approved"}]}]}),
        json!({"name":"proof.Number","properties":[{"id":"value","type":{"kind":"integer"}}]}),
        json!({"name":"proof.PrivateEmpty","properties":[],"markings":["pii.contact"]}),
        json!({"name":"norn.RuleRun","properties":[{"id":"outcome","type":{"kind":"text"}},{"id":"triggerSeq","type":{"kind":"integer"}},{"id":"detail","type":{"kind":"json-ext"}}]}),
    ].into_iter().map(|value| serde_json::from_value(value).expect("definition")).collect::<Vec<_>>();
    Model::from_definitions(&definitions, &[]).expect("model")
}
fn batch(writes: Vec<FactWriteRow>) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org/dev".to_owned(),
        writes,
        ..CommitBatch::default()
    }
}
fn put(ty: &str, key: &str, value: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: ty.to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([("value".to_owned(), Value::from(value))]),
    }
}
fn empty(ty: &str, key: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: ty.to_owned(),
        key: key.to_owned(),
        values: BTreeMap::new(),
    }
}
fn modified(before: &str, after: &str, before_version: u64, after_version: u64) -> Value {
    runtime(
        json!({"op":"modified","key":{"objectType":"proof.Item","key":"item"},"beforeVersion":before_version,"afterVersion":after_version,"before":{"value":before},"after":{"value":after},"changedProperties":["value"]}),
    )
}

#[test]
fn commitments_capture_each_materialized_version_and_survive_restart() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    store
        .commit(&batch(vec![
            put("proof.Item", "item", "one"),
            put("proof.Item", "item", "two"),
        ]))
        .expect("two writes");
    drop(store);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("restart");
    for (version, value) in [(1, "one"), (2, "two")] {
        let record = store
            .read()
            .object_at_version(MAIN_WORLD, "proof.Item", "item", version)
            .expect("proven version")
            .expect("live");
        assert_eq!(record.values["value"], Value::from(value));
    }
    assert!(
        store
            .read()
            .verify_event_snapshots(MAIN_WORLD, &modified("one", "two", 1, 2))
            .is_ok()
    );
    assert!(matches!(
        store
            .read()
            .verify_event_snapshots(MAIN_WORLD, &modified("secret", "two", 1, 2)),
        Err(CellError::HistoricalValueUnproven(_))
    ));
    let conn = rusqlite::Connection::open(path).expect("corruption fixture");
    conn.execute(
        "UPDATE _norn_fact_values SET snapshot = '{}' WHERE version = 1",
        [],
    )
    .expect("corrupt proof");
    assert!(matches!(
        store
            .read()
            .object_at_version(MAIN_WORLD, "proof.Item", "item", 1),
        Err(CellError::HistoricalValueUnproven(_))
    ));
}

#[test]
fn direct_modified_snapshot_substitution_refuses_atomically() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(vec![put("proof.Item", "item", "one")]))
        .expect("seed");
    for wrap in [false, true] {
        let event = modified("one", "classified scalar", 1, 2);
        let event = if wrap {
            runtime(json!({"op":"observed_fact","event":event}))
        } else {
            event
        };
        let forged = CommitBatch {
            events: vec![event],
            ..batch(vec![put("proof.Item", "item", "two")])
        };
        assert!(matches!(
            store.commit(&forged),
            Err(CellError::HistoricalValueUnproven(_))
        ));
        assert_eq!(store.export_commit_count().expect("head"), 1);
        assert!(matches!(
            store
                .read()
                .object_at_version(MAIN_WORLD, "proof.Item", "item", 2),
            Err(CellError::HistoricalValueUnproven(_))
        ));
    }
    store
        .commit(&CommitBatch {
            events: vec![modified("one", "two", 1, 2)],
            ..batch(vec![put("proof.Item", "item", "two")])
        })
        .expect("authentic snapshots");
}

#[test]
fn propertyless_occurrences_and_canonical_noop_sets_keep_lineage() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(vec![
            put("proof.Input", "input", "secret"),
            put("proof.Item", "item", "same"),
            empty("proof.PrivateEmpty", "private"),
        ]))
        .expect("seed");
    assert_eq!(
        store
            .read()
            .fact_occurrence_markings(MAIN_WORLD, "proof.PrivateEmpty", "private", Some(1))
            .expect("occurrence")
            .expect("complete"),
        std::collections::BTreeSet::from(["pii.contact".to_owned()])
    );
    let read = norn_kernel::FactExpectation {
        key: norn_kernel::FactKey::new("proof.Input", "input"),
        expected: norn_kernel::FactVersion(1),
    };
    for (index, fact) in [
        FactChange::Set {
            object_type: ObjectTypeName("proof.Item".to_owned()),
            key: "item".to_owned(),
            property: PropertyId("value".to_owned()),
            value: Value::from("same"),
        },
        FactChange::Create {
            object_type: ObjectTypeName("proof.Empty".to_owned()),
            key: "empty".to_owned(),
            values: BTreeMap::new(),
        },
        FactChange::Delete {
            object_type: ObjectTypeName("proof.Empty".to_owned()),
            key: "empty".to_owned(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .commit(&CommitBatch {
                facts: vec![fact],
                marking_reads: vec![read.clone()],
                ..batch(Vec::new())
            })
            .expect("classified conditional fact");
        let (ty, key, version) = if index == 0 {
            ("proof.Item", "item", 2)
        } else {
            ("proof.Empty", "empty", u64::try_from(index).expect("index"))
        };
        assert_eq!(
            store
                .read()
                .fact_occurrence_markings(MAIN_WORLD, ty, key, Some(version))
                .expect("occurrence")
                .expect("complete"),
            std::collections::BTreeSet::from(["pii.contact".to_owned()])
        );
    }
    assert!(
        store
            .read()
            .effective_markings(MAIN_WORLD, "proof.Item", "item", Some(2))
            .expect("markings")
            .expect("complete")["value"]
            .is_empty()
    );
}

#[test]
fn authentic_trigger_does_not_authenticate_unrelated_rule_detail() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    let trigger = runtime(
        json!({"op":"created","key":{"objectType":"proof.Item","key":"item"},"values":{"value":"public"}}),
    );
    store
        .commit(&CommitBatch {
            events: vec![trigger.clone()],
            ..batch(vec![put("proof.Item", "item", "public")])
        })
        .expect("public trigger");
    let rule = |key: &str, event: Value| FactWriteRow::Put {
        object_type: "norn.RuleRun".to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            ("outcome".to_owned(), Value::from("decided")),
            ("triggerSeq".to_owned(), Value::from(1_i64)),
            (
                "detail".to_owned(),
                runtime(
                    json!({"inputs":{"event":event,"unrelated":"classified scalar"},"explanation":"classified explanation"}),
                ),
            ),
        ]),
    };
    store
        .commit(&CommitBatch {
            marking_events: vec![1],
            ..batch(vec![
                rule("real", trigger),
                rule("forged", runtime(json!({"secret":"classified"}))),
            ])
        })
        .expect("direct boundary");
    for (key, labels) in [
        ("real", vec!["norn.unknown-lineage".to_owned()]),
        ("forged", vec!["norn.unknown-lineage".to_owned()]),
    ] {
        assert_eq!(
            store
                .read()
                .effective_markings(MAIN_WORLD, "norn.RuleRun", key, Some(1))
                .expect("labels")
                .expect("complete")["detail"],
            labels
        );
    }
}

#[test]
fn current_rebuild_replays_values_and_refuses_legacy_or_unprovable_history() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(vec![put("proof.Item", "item", "one")]))
        .expect("first");
    store
        .commit(&batch(vec![put("proof.Item", "item", "two")]))
        .expect("second");
    let mut records = (1..=2)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect::<Vec<_>>();
    let previous = records.last().expect("head").hash().expect("envelope");
    for version in [2, 3, 4, 5, 6] {
        let mut legacy = records.clone();
        legacy[0].store_format = format!("norn.cell.store/{version}");
        assert!(matches!(
            CellStore::rebuild_export_history(
                NativeEngine::open_in_memory().expect("engine"),
                &legacy,
                &previous
            ),
            Err(CellError::HistoricalValueUnproven(_))
        ));
    }
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        &records,
        &previous,
    )
    .expect("verified replay");
    assert_eq!(
        rebuilt
            .read()
            .object_at_version(MAIN_WORLD, "proof.Item", "item", 1)
            .expect("proof")
            .expect("live")
            .values["value"],
        Value::from("one")
    );
    assert!(matches!(
        CellStore::rebuild_export_history(
            NativeEngine::open_in_memory().expect("engine"),
            &records[..1],
            &previous
        ),
        Err(CellError::HistoricalValueUnproven(_))
    ));
    records[0].initial_model = None;
    assert!(matches!(
        CellStore::rebuild_export_history(
            NativeEngine::open_in_memory().expect("engine"),
            &records,
            &previous
        ),
        Err(CellError::HistoricalValueUnproven(_))
    ));
}

#[test]
fn canonical_noop_set_compares_engine_normalized_values() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(vec![
            put("proof.Input", "input", "secret"),
            FactWriteRow::Put {
                object_type: "proof.Number".to_owned(),
                key: "number".to_owned(),
                values: BTreeMap::from([("value".to_owned(), Value::from(1_i64))]),
            },
        ]))
        .expect("seed");
    store
        .commit(&CommitBatch {
            facts: vec![FactChange::Set {
                object_type: ObjectTypeName("proof.Number".to_owned()),
                key: "number".to_owned(),
                property: PropertyId("value".to_owned()),
                value: Value::from("1"),
            }],
            marking_reads: vec![norn_kernel::FactExpectation {
                key: norn_kernel::FactKey::new("proof.Input", "input"),
                expected: norn_kernel::FactVersion(1),
            }],
            ..batch(Vec::new())
        })
        .expect("same stored integer");
    assert_eq!(
        store
            .read()
            .fact_occurrence_markings(MAIN_WORLD, "proof.Number", "number", Some(2))
            .expect("occurrence")
            .expect("complete"),
        std::collections::BTreeSet::from(["pii.contact".to_owned()])
    );
    assert!(
        store
            .read()
            .effective_markings(MAIN_WORLD, "proof.Number", "number", Some(2))
            .expect("labels")
            .expect("complete")["value"]
            .is_empty()
    );
}

#[test]
fn export_envelope_hash_does_not_authenticate_substituted_snapshots() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    store
        .commit(&batch(vec![put("proof.Item", "item", "one")]))
        .expect("seed");
    store
        .commit(&CommitBatch {
            events: vec![modified("one", "two", 1, 2)],
            ..batch(vec![put("proof.Item", "item", "two")])
        })
        .expect("authentic change");
    let mut record = store.export_record(2).expect("journal").expect("record");
    record.entries[0]
        .event
        .as_object_mut()
        .expect("event")
        .insert(
            "after".to_owned(),
            runtime(json!({"value":"unrelated classified scalar"})),
        );
    let conn = rusqlite::Connection::open(&path).expect("corruption fixture");
    conn.execute(
        "UPDATE _norn_export_journal SET hash = ?1, record = ?2 WHERE commit_seq = 2",
        [
            record.hash().expect("recomputed envelope hash"),
            serde_json::to_string(&record).expect("record"),
        ],
    )
    .expect("substitute independently hashed envelope");
    assert!(matches!(
        store.export_record(2),
        Err(CellError::HistoricalValueUnproven(_))
    ));
}

fn granular(op: &str, value: &str) -> Value {
    match op {
        "created" => runtime(
            json!({"op":"created","key":{"objectType":"proof.Item","key":"item"},"values":{"value":value}}),
        ),
        "set" => runtime(
            json!({"op":"set","key":{"objectType":"proof.Item","key":"item"},"property":"value","value":value}),
        ),
        _ => modified("one", value, 1, 2),
    }
}

#[test]
fn all_value_event_kinds_require_the_emitting_commit_before_disclosure() {
    for op in ["created", "set", "modified"] {
        for observed in [false, true] {
            let mut store = CellStore::open_in_memory(model()).expect("store");
            if op != "created" {
                store
                    .commit(&batch(vec![put("proof.Item", "item", "one")]))
                    .expect("seed");
            }
            let count = store.export_commit_count().expect("head");
            let event = granular(op, "unrelated classified scalar");
            let event = if observed {
                runtime(json!({"op":"observed_fact","event":event}))
            } else {
                event
            };
            let outcome = store.commit(&CommitBatch {
                events: vec![event],
                ..batch(vec![put("proof.Item", "item", "two")])
            });
            if matches!(op, "created" | "modified") {
                assert!(matches!(
                    outcome,
                    Err(CellError::HistoricalValueUnproven(_))
                ));
                assert_eq!(store.export_commit_count().expect("head"), count);
            } else {
                outcome.expect("opaque granular durability record");
                let entry = store
                    .read()
                    .ledger(0)
                    .expect("ledger")
                    .pop()
                    .expect("entry");
                assert!(
                    store
                        .read()
                        .verified_history_event(&entry)
                        .expect("proof")
                        .is_none()
                );
                assert!(
                    store
                        .read()
                        .fact_event_markings(&entry)
                        .expect("lineage")
                        .is_none()
                );
            }
            let event = granular(op, "two");
            let event = if observed {
                runtime(json!({"op":"observed_fact","event":event}))
            } else {
                event
            };
            store
                .commit(&CommitBatch {
                    events: vec![event],
                    ..batch(vec![put("proof.Item", "item", "two")])
                })
                .expect("authentic proof");
            let entry = store
                .read()
                .ledger(0)
                .expect("ledger")
                .pop()
                .expect("entry");
            assert!(
                store
                    .read()
                    .verified_history_event(&entry)
                    .expect("proof")
                    .is_some()
            );
        }
    }
}

#[test]
fn created_proofs_use_the_writer_contract_for_defaults_and_nullable_fields() {
    for observed in [false, true] {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        let event = runtime(
            json!({"op":"created","key":{"objectType":"proof.Defaulted","key":"item"},"values":{"value":"public"}}),
        );
        let event = if observed {
            runtime(json!({"op":"observed_fact","event":event}))
        } else {
            event
        };
        store
            .commit(&CommitBatch {
                events: vec![event.clone()],
                ..batch(vec![put("proof.Defaulted", "item", "public")])
            })
            .expect("created");
        let reader = store.read();
        let mut entry = reader.ledger(0).expect("ledger").pop().expect("entry");
        assert_eq!(
            reader.verified_history_event(&entry).expect("proof"),
            Some(event)
        );
        assert_eq!(
            reader.fact_event_markings(&entry).expect("labels"),
            Some(std::collections::BTreeSet::default())
        );
        for property in ["value", "optional", "defaulted", "undeclared"] {
            let mut forged = entry.event.clone();
            let native = if observed {
                forged
                    .as_object_mut()
                    .expect("wrapper")
                    .get_mut("event")
                    .expect("native")
            } else {
                &mut forged
            };
            native
                .as_object_mut()
                .expect("native")
                .get_mut("values")
                .expect("values")
                .as_object_mut()
                .expect("values")
                .insert(property.to_owned(), "unrelated classified scalar".into());
            let authentic = std::mem::replace(&mut entry.event, forged);
            assert!(
                reader
                    .verified_history_event(&entry)
                    .expect("proof")
                    .is_none(),
                "{property}/{observed}"
            );
            assert!(
                reader
                    .fact_event_markings(&entry)
                    .expect("labels")
                    .is_none()
            );
            entry.event = authentic;
        }
    }
}

#[test]
fn shared_history_accessor_refuses_each_forged_fact_and_masks_opaque_text() {
    for op in ["created", "set", "modified"] {
        for observed in [false, true] {
            let mut store = CellStore::open_in_memory(model()).expect("store");
            if op != "created" {
                store
                    .commit(&batch(vec![put("proof.Item", "item", "one")]))
                    .expect("seed");
            }
            let event = granular(op, "two");
            let event = if observed {
                runtime(json!({"op":"observed_fact","event":event}))
            } else {
                event
            };
            store
                .commit(&CommitBatch {
                    events: vec![event],
                    ..batch(vec![put("proof.Item", "item", "two")])
                })
                .expect("authentic");
            let reader = store.read();
            let mut entry = reader.ledger(0).expect("entries").pop().expect("entry");
            assert!(
                reader
                    .verified_history_event(&entry)
                    .expect("proof")
                    .is_some()
            );
            entry.event = granular(op, "unrelated classified scalar");
            if observed {
                entry.event = runtime(json!({"op":"observed_fact","event":entry.event}));
            }
            assert!(
                reader
                    .verified_history_event(&entry)
                    .expect("proof")
                    .is_none(),
                "{op}/{observed}"
            );
            assert!(
                reader
                    .fact_event_markings(&entry)
                    .expect("lineage")
                    .is_none(),
                "unproven inputs cannot borrow public labels"
            );
        }
    }
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&CommitBatch {
            events: vec![runtime(
                json!({"type":"command.result","message":"unrelated classified scalar"}),
            )],
            ..batch(vec![put("proof.Item", "item", "public")])
        })
        .expect("opaque durability record");
    let entry = store
        .read()
        .ledger(0)
        .expect("entries")
        .pop()
        .expect("entry");
    assert!(
        store
            .read()
            .verified_history_event(&entry)
            .expect("proof")
            .is_none()
    );
    assert!(
        store
            .read()
            .ledger_markings(&entry)
            .expect("lineage")
            .is_none()
    );
    let page = store
        .read()
        .verified_ledger_page(0, 10)
        .expect("projection");
    assert_eq!(page[0].event, runtime(json!({"masked":true})));
    assert!(
        !serde_json::to_string(&page)
            .expect("projection encodes")
            .contains("unrelated classified scalar")
    );
}

#[test]
fn seed_summary_preserves_creation_proofs_and_markings_after_replay() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    let summary = runtime(json!({"type":"seed.loaded","objects":[
        {"objectType":"proof.Item","key":"public"},
        {"objectType":"proof.Input","key":"private"}
    ]}));
    let mut event = summary.clone();
    event
        .as_object_mut()
        .expect("object")
        .insert("message".to_owned(), Value::from("unproven free text"));
    store
        .commit(&CommitBatch {
            events: vec![event],
            ..batch(vec![
                put("proof.Item", "public", "public value"),
                put("proof.Input", "private", "private value"),
            ])
        })
        .expect("seed");
    let record = store.export_record(1).expect("export").expect("record");
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        std::slice::from_ref(&record),
        &record.hash().expect("head"),
    )
    .expect("replay");
    for reader in [store.read(), rebuilt.read()] {
        let entry = reader.ledger(0).expect("entries").pop().expect("entry");
        assert_eq!(
            reader.verified_history_event(&entry).expect("proof"),
            Some(summary.clone()),
            "only receipt-bound creation identities are revealed"
        );
        assert_eq!(
            reader.ledger_markings(&entry).expect("labels"),
            Some(std::collections::BTreeSet::from(["pii.contact".to_owned()])),
            "summary retains the complete receipt's classifications"
        );
    }
}

#[test]
fn seed_summary_uses_receipt_snapshots_not_decoded_observation_payloads() {
    let cases = [
        (json!({"note":"ordinary observation JSON"}), "item", true),
        (
            json!({
                "world":"another-world", "rootType":"proof.Item",
                "objectType":"proof.Item", "lineage":["proof.Item"],
                "modelHash":"unproven", "key":"unwritten", "scope":"org/dev",
                "version":99, "values":{"value":"not a committed snapshot"}
            }),
            "unwritten",
            false,
        ),
    ];
    for (payload, claimed_key, revealable) in cases {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        let summary = runtime(json!({"type":"seed.loaded","objects":[
            {"objectType":"proof.Observed","key":"source"},
            {"objectType":"proof.Item","key":claimed_key}
        ]}));
        store
            .commit(&CommitBatch {
                events: vec![summary.clone()],
                ..batch(vec![
                    FactWriteRow::Put {
                        object_type: "proof.Observed".to_owned(),
                        key: "source".to_owned(),
                        values: BTreeMap::from([(
                            "observationJson".to_owned(),
                            Value::from(payload.to_string()),
                        )]),
                    },
                    put("proof.Item", "item", "actual committed value"),
                ])
            })
            .expect("durability record");
        let reader = store.read();
        let entry = reader.ledger(0).expect("entries").pop().expect("entry");
        assert_eq!(
            reader.verified_history_event(&entry).expect("proof"),
            revealable.then_some(summary),
        );
    }
}

#[test]
fn seed_summary_cannot_substitute_omit_duplicate_or_reorder_creations() {
    let claims = [
        json!([{"objectType":"proof.Item","key":"unwritten"},{"objectType":"proof.Item","key":"two"}]),
        json!([{"objectType":"proof.Input","key":"one"},{"objectType":"proof.Item","key":"two"}]),
        json!([{"objectType":"proof.Item","key":"one"}]),
        json!([{"objectType":"proof.Item","key":"one"},{"objectType":"proof.Item","key":"one"}]),
        json!([{"objectType":"proof.Item","key":"two"},{"objectType":"proof.Item","key":"one"}]),
        json!([{"objectType":"proof.Item","key":"one","message":"unproven"},{"objectType":"proof.Item","key":"two"}]),
        json!([]),
        json!("not an object list"),
        serde_json::Value::Null,
    ];
    for objects in claims {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&CommitBatch {
                events: vec![runtime(json!({"type":"seed.loaded","objects":objects}))],
                ..batch(vec![
                    put("proof.Item", "one", "first"),
                    put("proof.Item", "two", "second"),
                ])
            })
            .expect("durability record");
        let reader = store.read();
        let entry = reader.ledger(0).expect("entries").pop().expect("entry");
        assert!(
            reader
                .verified_history_event(&entry)
                .expect("proof")
                .is_none()
        );
        assert!(reader.ledger_markings(&entry).expect("labels").is_none());
    }
}

#[test]
fn seed_summary_requires_new_creations_in_its_own_receipt() {
    for updating in [false, true] {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&batch(vec![put("proof.Item", "item", "original")]))
            .expect("existing object");
        store
            .commit(&CommitBatch {
                events: vec![runtime(json!({"type":"seed.loaded","objects":[
                    {"objectType":"proof.Item","key":"item"}
                ]}))],
                ..batch(if updating {
                    vec![put("proof.Item", "item", "updated")]
                } else {
                    Vec::new()
                })
            })
            .expect("durability record");
        let reader = store.read();
        let entry = reader.ledger(0).expect("entries").pop().expect("entry");
        assert!(
            reader
                .verified_history_event(&entry)
                .expect("proof")
                .is_none()
        );
    }
}

#[test]
fn seed_summary_keeps_the_existing_history_proof_byte_budget() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&CommitBatch {
            events: vec![runtime(json!({"type":"seed.loaded","objects":[
                {"objectType":"proof.Item","key":"large"}
            ]}))],
            ..batch(vec![put("proof.Item", "large", &"x".repeat(1024 * 1024))])
        })
        .expect("large creation");
    let reader = store.read();
    let entry = reader.ledger(0).expect("entries").pop().expect("entry");
    assert!(
        reader
            .verified_history_event(&entry)
            .expect("proof")
            .is_none()
    );
}

#[test]
fn created_and_set_replay_cannot_authorize_a_substituted_export_payload() {
    for op in ["created", "set"] {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        if op == "set" {
            store
                .commit(&batch(vec![put("proof.Item", "item", "one")]))
                .expect("seed");
        }
        store
            .commit(&CommitBatch {
                events: vec![granular(op, "two")],
                ..batch(vec![put("proof.Item", "item", "two")])
            })
            .expect("authentic");
        let count = store.export_commit_count().expect("head");
        let mut records = (1..=count)
            .map(|seq| store.export_record(seq).expect("record").expect("present"))
            .collect::<Vec<_>>();
        let target = records.last_mut().expect("target");
        target.batch.events[0] = granular(op, "unrelated classified scalar");
        target.entries[0].event = target.batch.events[0].clone();
        let trusted = target
            .hash()
            .expect("independently recomputed envelope hash");
        assert!(
            matches!(
                CellStore::rebuild_export_history(
                    NativeEngine::open_in_memory().expect("engine"),
                    &records,
                    &trusted
                ),
                Err(CellError::HistoricalValueUnproven(_))
            ),
            "{op}"
        );
    }
}

#[test]
fn untyped_finding_payload_is_masked_by_the_shared_accessor_after_verified_replay() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    let finding = norn_ir::DataQualityFinding::v2(
        norn_ir::DataQualityRuleRef::Builtin {
            id: "untyped".to_owned(),
            version: "1".to_owned(),
        },
        vec![norn_ir::DataQualitySubject::new(
            ObjectTypeName("proof.Item".to_owned()),
            "item",
        )],
        norn_ir::DataQualitySeverity::Error,
        "unrelated classified scalar",
        norn_ir::DataQualityOutcome::NotSatisfied,
        Vec::new(),
        norn_ir::UtcTimestamp("2026-10-08T00:00:00Z".to_owned()),
        norn_ir::UtcTimestamp("2026-10-08T00:00:00Z".to_owned()),
        norn_ir::DataQualityFindingStatus::Open,
    )
    .expect("canonical finding");
    store.commit(&CommitBatch {
        events: vec![norn_ir::runtime_object! { "op" => "data_quality_finding", "fact" => norn_ir::runtime::encode(&finding).expect("finding encodes") }],
        ..batch(vec![put("proof.Item", "item", "public")])
    }).expect("durability record");
    let record = store.export_record(1).expect("export").expect("present");
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        std::slice::from_ref(&record),
        &record.hash().expect("head"),
    )
    .expect("verified durability replay");
    for reader in [store.read(), rebuilt.read()] {
        let entry = reader.ledger(0).expect("entry").pop().expect("present");
        assert!(
            reader
                .verified_history_event(&entry)
                .expect("proof")
                .is_none()
        );
        assert_eq!(
            reader.verified_ledger_page(0, 1).expect("projection")[0].event,
            runtime(json!({"masked":true}))
        );
    }
}

#[test]
fn other_value_bearing_history_kinds_cannot_borrow_public_receipt_labels() {
    let private = "unrelated classified scalar";
    let payloads = [
        json!({"op":"command_enqueued","commandId":"command","generation":1,"idempotencyKey":"command","actionCall":"call","payload":{"message":private}}),
        json!({"op":"naming_series_allocated","key":{"objectType":"proof.Item","key":"item"},"counter":{"objectType":"proof.Item","key":"item"},"pattern":private,"timezone":"Etc/UTC","epochMillis":0,"tzdbVersion":"2026b","scope":{},"period":"","value":1,"resetPolicy":"never","gapPolicy":"gapless_within_commit"}),
        json!({"op":"naming_series_counter_seeded","counter":{"objectType":"proof.Item","key":"item"},"objectType":"proof.Item","keyName":"value","scope":{"value":private},"period":"","current":1}),
        json!({"op":"transitioned","key":{"objectType":"proof.Item","key":"item"},"lifecycle":"value","transition":"change","from":private,"to":"public"}),
        json!({"op":"invariant_refused","key":{"objectType":"proof.Item","key":"item"},"owner":{"kind":"object","name":"proof.Item"},"id":"check","message":private,"outcome":{"kind":"not_satisfied"},"attemptId":"attempt"}),
        json!({"op":"observed_transition","key":{"objectType":"proof.Item","key":"item"},"property":"value","transition":"change","from":private,"to":"public"}),
        json!({"op":"observed_deleted","key":{"objectType":"proof.Item","key":"item"},"tombstoned":false,"provenance":{"source":{"system":"public","record":private}}}),
        json!({"type":"observation.recorded","payload":{"message":private}}),
    ];
    for payload in payloads {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&CommitBatch {
                events: vec![runtime(payload)],
                ..batch(vec![put("proof.Item", "item", "public")])
            })
            .expect("durability record");
        let reader = store.read();
        let entry = reader.ledger(0).expect("entry").pop().expect("present");
        assert!(
            reader
                .verified_history_event(&entry)
                .expect("proof")
                .is_none()
        );
        assert!(
            reader
                .fact_event_markings(&entry)
                .expect("labels")
                .is_none()
        );
        assert!(
            !serde_json::to_string(&reader.verified_ledger_page(0, 1).expect("projection"))
                .expect("JSON")
                .contains(private)
        );
    }
}

#[test]
fn empty_occurrence_override_cannot_drop_classified_noop_lineage_after_restore() {
    for set in [false, true] {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&batch(vec![
                put("proof.Input", "input", "secret"),
                put("proof.Item", "item", "same"),
            ]))
            .expect("seed");
        let request = CommitBatch {
            writes: if set {
                Vec::new()
            } else {
                vec![put("proof.Item", "item", "same")]
            },
            facts: if set {
                vec![FactChange::Set {
                    object_type: ObjectTypeName("proof.Item".to_owned()),
                    key: "item".to_owned(),
                    property: PropertyId("value".to_owned()),
                    value: Value::from("same"),
                }]
            } else {
                Vec::new()
            },
            marking_reads: vec![norn_kernel::FactExpectation {
                key: norn_kernel::FactKey::new("proof.Input", "input"),
                expected: norn_kernel::FactVersion(1),
            }],
            marking_occurrence_reads: Some(Vec::new()),
            ..batch(Vec::new())
        };
        store
            .commit(&request)
            .expect("classified conditional no-op");
        let records = [
            store.export_record(1).expect("record").expect("seed"),
            store.export_record(2).expect("record").expect("update"),
        ];
        let restored = CellStore::rebuild_export_history(
            NativeEngine::open_in_memory().expect("engine"),
            &records,
            &records[1].hash().expect("head"),
        )
        .expect("restore");
        for selected in [&store, &restored] {
            assert_eq!(
                selected
                    .read()
                    .fact_occurrence_markings(MAIN_WORLD, "proof.Item", "item", Some(2))
                    .expect("labels")
                    .expect("complete"),
                std::collections::BTreeSet::from(["pii.contact".to_owned()])
            );
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "authentic transitions, fabricated history and tombstones exercise one retained chain"
)]
fn transition_and_deletion_require_actual_adjacent_mutations() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    let state = || FactWriteRow::Put {
        object_type: "proof.State".to_owned(),
        key: "state".to_owned(),
        values: BTreeMap::from([("status".to_owned(), Value::from("draft"))]),
    };
    store.commit(&batch(vec![state()])).expect("draft");
    let transition = runtime(
        json!({"op":"transitioned","key":{"objectType":"proof.State","key":"state"},"lifecycle":"status","transition":"approve","from":"draft","to":"approved"}),
    );
    store
        .commit(&CommitBatch {
            facts: vec![FactChange::Transition {
                object_type: ObjectTypeName("proof.State".to_owned()),
                key: "state".to_owned(),
                lifecycle: PropertyId("status".to_owned()),
                transition: norn_ir::TransitionId("approve".to_owned()),
            }],
            events: vec![transition.clone()],
            ..batch(Vec::new())
        })
        .expect("authentic transition");
    let authentic = store
        .read()
        .ledger_page(0, 10)
        .expect("history")
        .pop()
        .expect("transition");
    assert!(
        store
            .read()
            .verified_history_event(&authentic)
            .expect("projection")
            .is_some()
    );
    let mut caller_history = authentic;
    caller_history.event = runtime(
        json!({"op":"transitioned","key":{"objectType":"proof.State","key":"state"},"lifecycle":"status","transition":"approve","from":"invented state","to":"classified scalar"}),
    );
    assert_eq!(
        store
            .read()
            .verified_history_event(&caller_history)
            .expect("authoritative history"),
        None
    );
    let approved = store
        .read()
        .object(MAIN_WORLD, "proof.State", "state")
        .expect("record")
        .expect("live");
    for event in [
        transition,
        runtime(
            json!({"op":"observed_transition","key":{"objectType":"proof.State","key":"state"},"property":"status","transition":"approve","from":"draft","to":"approved"}),
        ),
        runtime(json!({"op":"deleted","key":{"objectType":"proof.State","key":"state"}})),
    ] {
        store
            .commit(&CommitBatch {
                events: vec![event],
                ..batch(vec![FactWriteRow::Put {
                    object_type: "proof.State".to_owned(),
                    key: "state".to_owned(),
                    values: approved.values.clone(),
                }])
            })
            .expect("untrusted event retained");
        let forged = store
            .read()
            .ledger_page(0, 10)
            .expect("history")
            .pop()
            .expect("forged");
        assert!(
            store
                .read()
                .verified_history_event(&forged)
                .expect("projection")
                .is_none()
        );
        assert!(
            store
                .read()
                .fact_event_markings(&forged)
                .expect("lineage")
                .is_none()
        );
    }
    store
        .commit(&CommitBatch {
            facts: vec![FactChange::Delete {
                object_type: ObjectTypeName("proof.State".to_owned()),
                key: "state".to_owned(),
            }],
            events: vec![runtime(
                json!({"op":"deleted","key":{"objectType":"proof.State","key":"state"}}),
            )],
            ..batch(Vec::new())
        })
        .expect("actual deletion");
    let deletion = store
        .read()
        .ledger_page(0, 10)
        .expect("history")
        .pop()
        .expect("deleted");
    assert!(
        store
            .read()
            .verified_history_event(&deletion)
            .expect("projection")
            .is_some()
    );
}

#[test]
fn historical_scope_matches_live_and_tombstone_and_refuses_cross_scope_replace() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(vec![put("proof.Item", "item", "one")]))
        .expect("scope A");
    let before = store.read().dump().expect("before");
    let mut wrong = batch(vec![put("proof.Item", "item", "two")]);
    wrong.scope = "other/dev".to_owned();
    assert!(matches!(
        store.commit(&wrong),
        Err(CellError::HistoricalValueUnproven(_))
    ));
    assert_eq!(before, store.read().dump().expect("atomic refusal"));
    store
        .commit(&batch(vec![put("proof.Item", "item", "two")]))
        .expect("same scope replacement");
    for version in [1, 2] {
        assert_eq!(
            store
                .read()
                .object_at_version(MAIN_WORLD, "proof.Item", "item", version)
                .expect("proof")
                .expect("live version")
                .scope,
            "org/dev"
        );
    }
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "proof.Item", "item")
            .expect("live")
            .expect("row")
            .scope,
        "org/dev"
    );
    store
        .commit(&CommitBatch {
            facts: vec![FactChange::Delete {
                object_type: ObjectTypeName("proof.Item".to_owned()),
                key: "item".to_owned(),
            }],
            ..batch(Vec::new())
        })
        .expect("delete");
    assert!(
        store
            .read()
            .object_at_version(MAIN_WORLD, "proof.Item", "item", 3)
            .expect("tombstone proof")
            .is_none()
    );
    assert!(
        store.commit(&wrong).is_err(),
        "retained tombstone scope also refuses replacement"
    );
    let records = (1..=3)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect::<Vec<_>>();
    let restored = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        &records,
        &records[2].hash().expect("head"),
    )
    .expect("restore");
    assert_eq!(
        store.read().dump().expect("source"),
        restored.read().dump().expect("restored")
    );
}

#[test]
fn bound_cell_scope_refuses_new_facts_and_first_absent_tombstones_atomically() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "org/dev").expect("bound cell");
    let before = store.read().dump().expect("before");
    for write in [
        put("proof.Item", "new", "value"),
        FactWriteRow::Delete {
            object_type: "proof.Item".to_owned(),
            key: "absent".to_owned(),
        },
    ] {
        let mut request = batch(vec![write]);
        request.scope = "other/dev".to_owned();
        assert!(
            matches!(store.commit(&request), Err(CellError::BindingMismatch { key, expected, found }) if key == "scope" && expected == "other/dev" && found == "org/dev")
        );
        assert_eq!(before, store.read().dump().expect("atomic scope refusal"));
    }
    store
        .commit(&batch(vec![FactWriteRow::Delete {
            object_type: "proof.Item".to_owned(),
            key: "absent".to_owned(),
        }]))
        .expect("admitted first tombstone");
    assert!(
        store
            .read()
            .object_at_version(MAIN_WORLD, "proof.Item", "absent", 1)
            .expect("bound tombstone proof")
            .is_none()
    );
}

#[test]
fn value_only_lineage_cannot_hide_an_unwritten_classified_decision_input() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(vec![
            put("proof.Input", "input", "secret"),
            put("proof.Item", "item", "same"),
        ]))
        .expect("seed");
    let before = store.read().dump().expect("before");
    let request = CommitBatch {
        marking_value_reads: vec![norn_kernel::FactExpectation {
            key: norn_kernel::FactKey::new("proof.Input", "input"),
            expected: norn_kernel::FactVersion(1),
        }],
        ..batch(vec![put("proof.Item", "item", "same")])
    };
    assert!(matches!(
        store.commit(&request),
        Err(CellError::HistoricalValueUnproven(_))
    ));
    assert_eq!(before, store.read().dump().expect("atomic lineage refusal"));
}

#[test]
fn duplicate_fact_events_refuse_atomically_including_observed_wrappers() {
    for op in ["created", "set", "modified"] {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        if op != "created" {
            store
                .commit(&batch(vec![put("proof.Item", "item", "one")]))
                .expect("seed");
        }
        let before = store.read().dump().expect("state");
        let event = granular(op, "two");
        let duplicate = runtime(json!({"op":"observed_fact","event":event}));
        assert!(matches!(
            store.commit(&CommitBatch {
                events: vec![event, duplicate],
                ..batch(vec![put("proof.Item", "item", "two")])
            }),
            Err(CellError::HistoricalValueUnproven(_))
        ));
        assert_eq!(
            before,
            store
                .read()
                .dump()
                .expect("no partial writes, proofs, events or receipts")
        );
        store
            .commit(&CommitBatch {
                events: vec![granular(op, "two")],
                ..batch(vec![put("proof.Item", "item", "two")])
            })
            .expect("one matching event");
    }
}

#[test]
fn created_requires_absence_and_accepts_tombstone_recreation_after_replay() {
    for observed in [false, true] {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&batch(vec![put("proof.Item", "item", "one")]))
            .expect("seed");
        for value in ["one", "two"] {
            let before = store.read().dump().expect("state");
            let event = granular("created", value);
            let event = if observed {
                runtime(json!({"op":"observed_fact","event":event}))
            } else {
                event
            };
            assert!(matches!(
                store.commit(&CommitBatch {
                    events: vec![event],
                    ..batch(vec![put("proof.Item", "item", value)])
                }),
                Err(CellError::HistoricalValueUnproven(_))
            ));
            assert_eq!(before, store.read().dump().expect("unchanged"));
        }
        store
            .commit(&batch(vec![FactWriteRow::Delete {
                object_type: "proof.Item".to_owned(),
                key: "item".to_owned(),
            }]))
            .expect("delete");
        store
            .commit(&CommitBatch {
                events: vec![granular("created", "two")],
                ..batch(vec![put("proof.Item", "item", "two")])
            })
            .expect("genuine recreation");
        let records = (1..=3)
            .map(|seq| store.export_record(seq).expect("export").expect("record"))
            .collect::<Vec<_>>();
        let rebuilt = CellStore::rebuild_export_history(
            NativeEngine::open_in_memory().expect("engine"),
            &records,
            &records[2].hash().expect("head"),
        )
        .expect("rebuild");
        let entry = rebuilt
            .read()
            .ledger(0)
            .expect("events")
            .pop()
            .expect("created");
        assert!(
            rebuilt
                .read()
                .verified_history_event(&entry)
                .expect("proof")
                .is_some()
        );
    }
}

#[test]
fn consumed_input_cannot_authorize_opaque_public_event_payloads_after_restore() {
    let private = "classified consumed scalar";
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .inject(
            "observed",
            &runtime(json!({"input":{"message":private,"markings":["pii.contact"]}})),
        )
        .expect("inject");
    store
        .commit(&CommitBatch {
            consumes: Some(norn_cell::Consumption::Observation {
                observation_id: "observed".to_owned(),
            }),
            events: vec![runtime(
                json!({"type":"observation.recorded","payload":{"message":private}}),
            )],
            ..batch(vec![put("proof.Item", "item", "public")])
        })
        .expect("durability");
    let record = store.export_record(1).expect("export").expect("record");
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        std::slice::from_ref(&record),
        &record.hash().expect("head"),
    )
    .expect("rebuild");
    for reader in [store.read(), rebuilt.read()] {
        let entry = reader.ledger(0).expect("events").pop().expect("event");
        assert!(
            reader
                .verified_history_event(&entry)
                .expect("proof")
                .is_none()
        );
        assert!(reader.ledger_markings(&entry).expect("lineage").is_none());
        assert!(
            !serde_json::to_string(&reader.verified_ledger_page(0, 10).expect("page"))
                .expect("json")
                .contains(private)
        );
    }
}

#[test]
fn consumed_result_payload_requires_a_classified_fact_proof_for_public_history() {
    let private = "classified result scalar";
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&CommitBatch {
            effects: vec![norn_cell::EffectRow::Enqueue {
                command: norn_cell::OutboxCommand {
                    command_id: "command".to_owned(),
                    generation: 1,
                    idempotency_key: "command".to_owned(),
                    deadline: None,
                    command: runtime(json!({"payload":"public"})),
                },
            }],
            ..batch(Vec::new())
        })
        .expect("enqueue");
    store
        .record_performed(
            "command",
            1,
            &runtime(json!({"input":{"message":private,"markings":["pii.contact"]}})),
        )
        .expect("result");
    store.commit(&CommitBatch {
        consumes: Some(norn_cell::Consumption::Result { command_id: "command".to_owned(), generation: 1 }),
        events: vec![runtime(json!({"type":"command.result","commandId":"command","generation":1,"result":{"message":private,"markings":["pii.contact"]}}))],
        ..batch(vec![put("proof.Item", "item", "public")])
    }).expect("consume");
    let records = (1..=2)
        .map(|seq| store.export_record(seq).expect("export").expect("record"))
        .collect::<Vec<_>>();
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        &records,
        &records[1].hash().expect("head"),
    )
    .expect("rebuild");
    for reader in [store.read(), rebuilt.read()] {
        let entry = reader.ledger(0).expect("events").pop().expect("event");
        let event = reader
            .verified_history_event(&entry)
            .expect("proof")
            .expect("typed delivery identity");
        assert_eq!(
            event.get("commandId").and_then(Value::as_str),
            Some("command")
        );
        assert!(event.get("result").is_none());
        assert!(
            !serde_json::to_string(&reader.verified_ledger_page(0, 10).expect("history"))
                .expect("JSON")
                .contains(private)
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each independent corruption is restored before the next"
)]
fn fact_version_receipts_reject_missing_or_tampered_identity_lookup_and_receipt() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let first = store
        .commit(&batch(vec![put("proof.Item", "item", "one")]))
        .expect("first");
    let second = store
        .commit(&batch(vec![put("proof.Item", "item", "two")]))
        .expect("second");
    let unrelated = store
        .commit(&batch(vec![put("proof.Item", "other", "three")]))
        .expect("other");
    let first_seq = i64::try_from(first.receipt().body.commit_seq).expect("seq");
    let second_seq = i64::try_from(second.receipt().body.commit_seq).expect("seq");
    let unrelated_seq = i64::try_from(unrelated.receipt().body.commit_seq).expect("seq");
    let read = |world: &str, ty: &str, key: &str, version| {
        store
            .read()
            .receipt_for_fact_version(world, ty, key, version)
    };
    assert_eq!(
        read(MAIN_WORLD, "proof.Item", "item", 1).expect("receipt"),
        *first.receipt()
    );
    for (world, ty, key, version) in [
        ("wrong", "proof.Item", "item", 1),
        (MAIN_WORLD, "proof.Input", "item", 1),
        (MAIN_WORLD, "proof.Item", "missing", 1),
        (MAIN_WORLD, "proof.Item", "item", 0),
        (MAIN_WORLD, "proof.Item", "item", 3),
    ] {
        assert!(read(world, ty, key, version).is_err());
    }
    let conn = rusqlite::Connection::open(path).expect("corruption fixture");
    // The locator's seq is not itself a proof: even authentic other receipts
    // must refuse a wrong target or a different version of this same target.
    for seq in [second_seq, unrelated_seq, 0, 999_999] {
        conn.execute(
            "UPDATE _norn_fact_values SET commit_seq = ?1 WHERE id = 'item' AND version = 1",
            [seq],
        )
        .expect("wrong seq");
        assert!(read(MAIN_WORLD, "proof.Item", "item", 1).is_err());
    }
    conn.execute(
        "UPDATE _norn_fact_values SET commit_seq = ?1 WHERE id = 'item' AND version = 1",
        [first_seq],
    )
    .expect("restore seq");
    for (column, wrong, original) in [
        ("world", "wrong", MAIN_WORLD),
        ("id", "wrong", "item"),
        ("version", "9", "1"),
    ] {
        conn.execute(
            &format!(
                "UPDATE _norn_fact_values SET {column} = ?1 WHERE id = 'item' AND version = 1"
            ),
            [wrong],
        )
        .expect("change locator");
        let (world, key, version) = match column {
            "world" => ("wrong", "item", 1),
            "id" => (MAIN_WORLD, "wrong", 1),
            _ => (MAIN_WORLD, "item", 9),
        };
        assert!(read(world, "proof.Item", key, version).is_err());
        conn.execute(
            &format!("UPDATE _norn_fact_values SET {column} = ?1 WHERE id = ?2 AND version = ?3"),
            rusqlite::params![original, key, i64::try_from(version).expect("version")],
        )
        .expect("restore locator");
    }
    let (snapshot, hash): (String, String) = conn
        .query_row(
            "SELECT snapshot, hash FROM _norn_fact_values WHERE id = 'item' AND version = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("snapshot");
    for column in ["snapshot", "hash"] {
        conn.execute(
            &format!(
                "UPDATE _norn_fact_values SET {column} = '{{}}' WHERE id = 'item' AND version = 1"
            ),
            [],
        )
        .expect("corrupt proof");
        assert!(read(MAIN_WORLD, "proof.Item", "item", 1).is_err());
        conn.execute("UPDATE _norn_fact_values SET snapshot = ?1, hash = ?2 WHERE id = 'item' AND version = 1", rusqlite::params![snapshot, hash]).expect("restore proof");
    }
    let (receipt, receipt_hash): (String, String) = conn
        .query_row(
            "SELECT receipt, receipt_hash FROM _norn_receipt WHERE commit_seq = ?1",
            [first_seq],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("receipt");
    for column in ["receipt", "receipt_hash", "world"] {
        conn.execute(
            &format!("UPDATE _norn_receipt SET {column} = '{{}}' WHERE commit_seq = ?1"),
            [first_seq],
        )
        .expect("corrupt receipt");
        assert!(read(MAIN_WORLD, "proof.Item", "item", 1).is_err());
        conn.execute("UPDATE _norn_receipt SET receipt = ?1, receipt_hash = ?2, world = ?3 WHERE commit_seq = ?4", rusqlite::params![receipt, receipt_hash, MAIN_WORLD, first_seq]).expect("restore receipt");
    }
    conn.execute(
        "DELETE FROM _norn_fact_values WHERE id = 'item' AND version = 2",
        [],
    )
    .expect("missing snapshot");
    assert!(read(MAIN_WORLD, "proof.Item", "item", 2).is_err());
    conn.execute(
        "DELETE FROM _norn_receipt WHERE commit_seq = ?1",
        [first_seq],
    )
    .expect("missing receipt");
    assert!(read(MAIN_WORLD, "proof.Item", "item", 1).is_err());
}

#[test]
fn exact_fact_receipt_admits_both_proofs_before_decoding() {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = CellStore::open(
        &directory.path().join("cell.sqlite"),
        model(),
        StoreOptions::default(),
    )
    .expect("store");
    let committed = store
        .commit(&batch(vec![put("proof.Item", "item", "retained")]))
        .expect("commit");
    let reader = store.read();
    let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    for (bytes, rows) in [(0, 100), (1_000_000, 1)] {
        let mut bounded = limits;
        bounded.max_candidate_bytes_per_commit = bytes;
        bounded.max_scan = rows;
        assert!(
            reader
                .receipt_for_fact_version_with_budget(
                    MAIN_WORLD,
                    "proof.Item",
                    "item",
                    1,
                    &mut DecodeBudget::new(&bounded),
                )
                .expect("bounded lookup")
                .is_none()
        );
    }
    let mut budget = DecodeBudget::new(&limits);
    let receipt = reader
        .receipt_for_fact_version_with_budget(MAIN_WORLD, "proof.Item", "item", 1, &mut budget)
        .expect("authenticated lookup")
        .expect("admitted proof");
    assert_eq!(receipt, *committed.receipt());
    assert!(budget.remaining_bytes() < limits.max_candidate_bytes_per_commit);
    assert_eq!(budget.remaining_rows(), limits.max_scan - 2);
}
