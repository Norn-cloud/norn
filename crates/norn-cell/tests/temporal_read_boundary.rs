#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Legacy temporal spellings are canonical at every object-value read boundary.

use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_cell::reactive::{BudgetedRead, DecodeBudget, ReactiveLimits};
use norn_cell::{CellStore, CommitBatch, LinkChange, MAIN_WORLD, Model, StoreOptions};
use norn_ir::{
    DecisionOutput, FactChange, ObjectTypeDef, ObjectTypeName, PropertyTypeDef, RuntimeValue,
};
use norn_kernel::{
    DecisionTurn, FactKey, FactSlice, FactState, FactVersion, LogicalTime, TypeCatalog, World,
    prepare_decision,
};
use norn_projections::sqlite_ddl::quote_identifier;
use rusqlite::Connection;
use serde_json::json;

fn definitions() -> (Vec<ObjectTypeDef>, Vec<PropertyTypeDef>) {
    let property_types = vec![
        serde_json::from_value(json!({
            "name": "temporal.instant",
            "type": { "kind": "timestamp", "timezone": "offset" }
        }))
        .expect("timestamp property type"),
    ];
    let wide_properties = (0..105)
        .map(|index| {
            json!({
                "id": format!("p{index:03}"),
                "type": { "kind": "text" },
                "constraints": { "nullable": true }
            })
        })
        .chain(std::iter::once(json!({
            "id": "tail_at",
            "type": { "kind": "ref", "name": "temporal.instant" }
        })))
        .collect::<Vec<_>>();
    let object_types = vec![
        serde_json::from_value(json!({
            "name": "temporal.Record",
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [
                    { "value": "draft" }, { "value": "accepted" }
                ] } },
                { "id": "a", "type": { "kind": "ref", "name": "temporal.instant" } },
                { "id": "b", "type": { "kind": "ref", "name": "temporal.instant" } },
                { "id": "invalid_at", "type": { "kind": "ref", "name": "temporal.instant" } },
                { "id": "optional_at", "type": { "kind": "ref", "name": "temporal.instant" },
                  "constraints": { "nullable": true } },
                { "id": "note", "type": { "kind": "text" } }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["accepted"],
                "transitions": [{
                    "id": "accept", "from": ["draft"], "to": "accepted",
                    "guards": ["self.a != self.b"]
                }]
            }],
            "links": [{
                "id": "temporal",
                "target": "temporal.Record",
                "cardinality": { "to": "many", "from": "many" },
                "properties": [
                    { "id": "occurred_at", "type": { "kind": "ref", "name": "temporal.instant" } }
                ]
            }]
        }))
        .expect("record type"),
        serde_json::from_value(json!({
            "name": "temporal.Child",
            "extends": "temporal.Record",
            "properties": [
                { "id": "a", "type": { "kind": "ref", "name": "temporal.instant" } }
            ]
        }))
        .expect("child override"),
        serde_json::from_value(json!({
            "name": "temporal.Wide",
            "properties": wide_properties
        }))
        .expect("wide type"),
    ];
    (object_types, property_types)
}

fn column_name(model: &Model, object_type: &str, property: &str) -> String {
    model
        .schema()
        .object_tables
        .iter()
        .find(|table| table.object_type.as_str() == object_type)
        .expect("object table")
        .columns
        .iter()
        .find(|column| column.property.as_str() == property)
        .and_then(|column| column.column.clone())
        .expect("physical property column")
}

fn budget() -> DecodeBudget {
    DecodeBudget::new(&ReactiveLimits {
        max_records: 10,
        max_canonical_bytes: 100_000,
        max_fanout_per_commit: 10,
        max_scan: 100,
        max_candidate_bytes_per_commit: 100_000,
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end legacy database fixture covers all object and link read routes"
)]
fn legacy_rows_are_canonical_at_every_object_read_boundary() {
    let (object_types, property_types) = definitions();
    let model = Model::from_definitions(&object_types, &property_types).expect("projects model");
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("opens");
    let record_values = serde_json::from_value(json!({
        "status": "draft",
        "a": "2024-02-29T10:00:00Z",
        "b": "2024-02-29T12:00:00+02:00",
        "invalid_at": "2024-02-29T10:00:00Z",
        "optional_at": null,
        "note": "2024-02-29T12:00:00+02:00"
    }))
    .expect("record values");
    let wide_values = BTreeMap::from([(
        "tail_at".to_owned(),
        RuntimeValue::String("2024-02-29T12:00:00+02:00".to_owned()),
    )]);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            facts: vec![
                FactChange::Create {
                    object_type: ObjectTypeName("temporal.Child".to_owned()),
                    key: "legacy".to_owned(),
                    values: record_values,
                },
                FactChange::Create {
                    object_type: ObjectTypeName("temporal.Wide".to_owned()),
                    key: "wide".to_owned(),
                    values: wide_values,
                },
            ],
            links: vec![LinkChange::Add {
                object_type: "temporal.Child".to_owned(),
                link: "temporal".to_owned(),
                source: "legacy".to_owned(),
                target: "legacy".to_owned(),
                values: BTreeMap::from([(
                    "occurred_at".to_owned(),
                    RuntimeValue::String("2024-02-29T12:00:00+02:00".to_owned()),
                )]),
                target_type: None,
            }],
            ..CommitBatch::default()
        })
        .expect("seeds objects");
    drop(store);

    let record_table = model
        .schema()
        .object_tables
        .iter()
        .find(|table| table.object_type.as_str() == "temporal.Record")
        .expect("record table");
    let wide_table = model
        .schema()
        .object_tables
        .iter()
        .find(|table| table.object_type.as_str() == "temporal.Wide")
        .expect("wide table");
    let tail = wide_table
        .columns
        .iter()
        .find(|column| column.property.as_str() == "tail_at")
        .expect("overflow timestamp");
    assert!(tail.column.is_none(), "tail_at must exercise JSON overflow");

    let raw = Connection::open(&path).expect("opens raw SQLite connection");
    let record_update = format!(
        "UPDATE {} SET {} = ?1, {} = ?2, {} = ?3, {} = ?4, {} = ?5 WHERE \"_world\" = ?6 AND \"_id\" = ?7",
        quote_identifier(&record_table.table),
        quote_identifier(&column_name(&model, "temporal.Record", "a")),
        quote_identifier(&column_name(&model, "temporal.Record", "b")),
        quote_identifier(&column_name(&model, "temporal.Record", "invalid_at")),
        quote_identifier(&column_name(&model, "temporal.Record", "optional_at")),
        quote_identifier(&column_name(&model, "temporal.Record", "note")),
    );
    raw.execute(
        &record_update,
        rusqlite::params![
            "2024-02-29T10:00:00Z",
            "2024-02-29T12:00:00+02:00",
            "not-a-timestamp",
            Option::<String>::None,
            "2024-02-29T12:00:00+02:00",
            MAIN_WORLD,
            "legacy",
        ],
    )
    .expect("writes legacy physical spellings");
    raw.execute(
        &format!(
            "UPDATE {} SET \"_overflow\" = ?1 WHERE \"_world\" = ?2 AND \"_id\" = ?3",
            quote_identifier(&wide_table.table)
        ),
        rusqlite::params![
            r#"{"tail_at":"2024-02-29T12:00:00+02:00"}"#,
            MAIN_WORLD,
            "wide",
        ],
    )
    .expect("writes legacy overflow spelling");
    let link_table = model
        .schema()
        .link_tables
        .iter()
        .find(|table| {
            table.source.as_str() == "temporal.Record" && table.link.as_str() == "temporal"
        })
        .expect("inherited temporal link table");
    let link_timestamp_column = link_table
        .columns
        .iter()
        .find(|column| column.property.as_str() == "occurred_at")
        .and_then(|column| column.column.as_deref())
        .expect("physical temporal link property");
    raw.execute(
        &format!(
            "UPDATE {} SET {} = ?1 WHERE \"_world\" = ?2 AND \"_source\" = ?3 AND \"_target\" = ?4",
            quote_identifier(&link_table.table),
            quote_identifier(link_timestamp_column),
        ),
        rusqlite::params!["2024-02-29T12:00:00+02:00", MAIN_WORLD, "legacy", "legacy"],
    )
    .expect("writes legacy inherited link spelling");
    drop(raw);

    let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reopens");
    let expected = RuntimeValue::String("2024-02-29T10:00:00Z".to_owned());
    let record = store
        .read()
        .object(MAIN_WORLD, "temporal.Record", "legacy")
        .expect("reads object")
        .expect("object exists");
    assert_eq!(record.object_type, "temporal.Child");
    assert_eq!(record.values.get("a"), Some(&expected));
    assert_eq!(record.values.get("b"), Some(&expected));
    assert_eq!(
        record.values.get("invalid_at"),
        Some(&RuntimeValue::String("not-a-timestamp".to_owned()))
    );
    assert_eq!(record.values.get("optional_at"), Some(&RuntimeValue::Null));
    assert_eq!(
        record.values.get("note"),
        Some(&RuntimeValue::String(
            "2024-02-29T12:00:00+02:00".to_owned()
        ))
    );

    let listed = store
        .read()
        .list(MAIN_WORLD, "temporal.Record", 10)
        .expect("lists objects");
    assert_eq!(listed[0].values.get("b"), Some(&expected));
    let mut full_budget = budget();
    let BudgetedRead::Found(budgeted) = store
        .read()
        .object_with_budget(MAIN_WORLD, "temporal.Record", "legacy", &mut full_budget)
        .expect("reads budgeted object")
    else {
        panic!("object should fit the budget")
    };
    assert_eq!(budgeted.values.get("b"), Some(&expected));
    let mut selected_budget = budget();
    let BudgetedRead::Found(selected) = store
        .read()
        .object_properties_with_budget(
            MAIN_WORLD,
            "temporal.Record",
            "legacy",
            &["b"],
            &mut selected_budget,
        )
        .expect("reads selected property")
    else {
        panic!("selected property should fit the budget")
    };
    assert_eq!(selected.values.get("b"), Some(&expected));

    let wide = store
        .read()
        .object(MAIN_WORLD, "temporal.Wide", "wide")
        .expect("reads overflow object")
        .expect("wide object exists");
    assert_eq!(wide.values.get("tail_at"), Some(&expected));

    let links = store
        .read()
        .links(MAIN_WORLD, "temporal.Child", "temporal", "legacy")
        .expect("reads inherited link values");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].values.get("occurred_at"), Some(&expected));

    let mut slice = FactSlice::new();
    slice.insert(
        FactKey::new("temporal.Child", "legacy"),
        FactState {
            version: FactVersion(record.version),
            values: Some(record.values),
        },
    );
    let catalog = TypeCatalog::new(object_types.iter()).with_property_types(&property_types);
    let output: DecisionOutput = serde_json::from_value(json!({
        "facts": [{
            "op": "transition",
            "objectType": "temporal.Child",
            "key": "legacy",
            "lifecycle": "status",
            "transition": "accept"
        }],
        "commands": [],
        "timers": []
    }))
    .expect("transition decision");
    let turn = DecisionTurn {
        id: "legacy-offsets".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };
    let refusal = prepare_decision::<Infallible>(&catalog, &slice, &turn, &output)
        .expect_err("equal instants must fail the inequality guard");
    assert_eq!(refusal.code(), "decision.guard_false");
}
