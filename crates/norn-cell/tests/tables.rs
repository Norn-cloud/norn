#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T2-ST-1 (#2540): every fixture object type and link projects to tables
//! that apply on plain SQLite (T1-PROJ-2 #2529 acceptance), and objects and
//! links of every one load into and read back from those tables — also after
//! reopening the file.

mod common;

use std::collections::BTreeMap;

use norn_cell::{CellError, CellStore, CommitBatch, LinkChange, MAIN_WORLD, Model, StoreOptions};
use norn_ir::{FactChange, PropertyKind, RuntimeValue as Value};
use norn_projections::PropertyColumn;
use serde_json::json;

/// An in-memory `json!` value as an exact runtime value.
fn rv(value: serde_json::Value) -> Value {
    Value::from_json(value).expect("runtime value")
}

/// A sample value of `column`'s kind (the store checks encodings, not
/// constraints; those are the checker's and the kernel's).
/// Text samples carry `salt`, so declared keys stay unique across objects.
fn sample(column: &PropertyColumn, salt: &str) -> Value {
    match column.kind {
        PropertyKind::Text | PropertyKind::Enum => {
            Value::String(format!("{}-{salt}", column.property.as_str()))
        }
        PropertyKind::Integer => rv(json!(7)),
        PropertyKind::Decimal => rv(json!(12.5)),
        PropertyKind::Boolean => rv(json!(true)),
        PropertyKind::Timestamp => rv(json!("2026-10-01T09:30:00Z")),
        PropertyKind::Date => rv(json!("2026-10-01")),
        PropertyKind::Duration => rv(json!("PT1H30M")),
        PropertyKind::Record => rv(json!({"value": "record"})),
        PropertyKind::JsonExt => rv(json!({ "channel": "web", "tags": ["a", "b"] })),
        PropertyKind::Ref => panic!("refs are resolved by the projection"),
    }
}

/// Sample values for every stored property of `object_type`.
fn sample_values(model: &Model, object_type: &str, salt: &str) -> BTreeMap<String, Value> {
    let schema = model.schema();
    let mut chain = Vec::new();
    let mut current = Some(object_type.to_owned());
    while let Some(name) = current {
        let table = schema.object_table(&name).expect("table");
        current = table
            .parent
            .as_ref()
            .map(|parent| parent.as_str().to_owned());
        chain.push(table);
    }
    chain
        .iter()
        .flat_map(|table| table.columns.iter())
        .map(|column| (column.property.as_str().to_owned(), sample(column, salt)))
        .collect()
}

fn create(object_type: &str, key: &str, values: &BTreeMap<String, Value>) -> FactChange {
    serde_json::from_value(json!({
        "op": "create",
        "objectType": object_type,
        "key": key,
        "values": values,
    }))
    .expect("fact decodes")
}

fn batch(facts: Vec<FactChange>, links: Vec<LinkChange>) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 1_000,
        facts,
        links,
        ..CommitBatch::default()
    }
}

#[test]
fn the_projected_ddl_applies_on_plain_sqlite() {
    let model = common::corpus_model();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let tables: Vec<String> = model
        .schema()
        .object_tables
        .iter()
        .map(|table| table.table.clone())
        .chain(
            model
                .schema()
                .link_tables
                .iter()
                .map(|table| table.table.clone()),
        )
        .collect();
    let store = CellStore::open(&path, model, StoreOptions::default()).expect("opens");
    let dump = store.read().dump().expect("dumps");
    for table in &tables {
        assert!(dump.contains_key(table), "{table} was created");
    }
    for system in [
        "_norn_meta",
        "_norn_ledger",
        "_norn_receipt",
        "_norn_outbox",
        "_norn_perform_claim",
        "_norn_timer",
        "_norn_run",
        "_norn_inbox",
        "_norn_version",
    ] {
        assert!(dump.contains_key(system), "{system} was created");
    }
}

#[test]
fn every_fixture_object_and_link_round_trips_through_its_tables() {
    let model = common::corpus_model();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("opens");

    // One object of every type.
    let mut expected = BTreeMap::new();
    let mut facts = Vec::new();
    for table in &model.schema().object_tables {
        let object_type = table.object_type.as_str();
        let key = format!("{object_type}#1");
        let values = sample_values(&model, object_type, &key);
        facts.push(create(object_type, &key, &values));
        expected.insert(object_type.to_owned(), (key, values));
    }
    // One link of every link table, between the sample objects (plus a
    // second source object so N:1 and 1:1 ends stay within cardinality).
    let mut links = Vec::new();
    let mut expected_links = Vec::new();
    for table in &model.schema().link_tables {
        let source = format!("{}#1", table.source.as_str());
        let target = format!("{}#1", table.target.as_str());
        let values: BTreeMap<String, Value> = table
            .columns
            .iter()
            .map(|column| (column.property.as_str().to_owned(), sample(column, "link")))
            .collect();
        links.push(LinkChange::Add {
            object_type: table.source.as_str().to_owned(),
            link: table.link.as_str().to_owned(),
            source: source.clone(),
            target: target.clone(),
            values: values.clone(),
            target_type: None,
        });
        expected_links.push((
            table.source.clone(),
            table.link.clone(),
            source,
            target,
            values,
        ));
    }
    store.commit(&batch(facts, links)).expect("commits");

    let check = |store: &CellStore| {
        for (object_type, (key, values)) in &expected {
            let record = store
                .read()
                .object(MAIN_WORLD, object_type, key)
                .expect("reads")
                .unwrap_or_else(|| panic!("{object_type} {key} exists"));
            assert_eq!(&record.object_type, object_type);
            assert_eq!(record.version, 1);
            assert_eq!(record.scope, "org_demo/dev");
            assert_eq!(&record.values, values, "{object_type}");
        }
        for (source_type, link, source, target, values) in &expected_links {
            let found = store
                .read()
                .links(MAIN_WORLD, source_type.as_str(), link.as_str(), source)
                .expect("reads links");
            assert_eq!(found.len(), 1, "{}.{}", source_type.as_str(), link.as_str());
            assert_eq!(&found[0].target, target);
            assert_eq!(&found[0].values, values);
        }
    };
    check(&store);
    store.read().verify_ledger().expect("ledger verifies");
    drop(store);

    let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopens");
    check(&reopened);
    reopened.read().verify_ledger().expect("ledger verifies");
}

#[test]
fn model_ancestry_is_reflexive_directional_and_rejects_unknown_types() {
    let model = common::corpus_model();
    assert!(model.is_a("crm.Customer", "crm.Customer"));
    assert!(model.is_a("crm.KeyAccount", "crm.Customer"));
    assert!(!model.is_a("crm.Customer", "crm.KeyAccount"));
    assert!(!model.is_a("crm.KeyAccount", "hr.Employee"));
    assert!(!model.is_a("missing.Type", "crm.Customer"));
    assert!(!model.is_a("crm.Customer", "missing.Type"));
    assert!(!model.is_a("missing.Type", "missing.Type"));
}

#[test]
fn a_subtype_object_reads_through_its_supertype_and_deletes_with_it() {
    let model = common::corpus_model();
    let mut store = CellStore::open_in_memory(model.clone()).expect("opens");
    let values = sample_values(&model, "crm.KeyAccount", "K1");
    store
        .commit(&batch(
            vec![create("crm.KeyAccount", "K1", &values)],
            vec![],
        ))
        .expect("commits");
    let through_parent = store
        .read()
        .object(MAIN_WORLD, "crm.Customer", "K1")
        .expect("reads")
        .expect("exists");
    assert_eq!(through_parent.object_type, "crm.KeyAccount");
    assert_eq!(through_parent.values, values);
    // A sibling type does not see it.
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "hr.Employee", "K1")
            .expect("reads"),
        None
    );

    let delete: FactChange = serde_json::from_value(json!({
        "op": "delete", "objectType": "crm.Customer", "key": "K1"
    }))
    .expect("decodes");
    store.commit(&batch(vec![delete], vec![])).expect("commits");
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "crm.KeyAccount", "K1")
            .expect("reads"),
        None
    );
    let dump = store.read().dump().expect("dumps");
    assert!(
        dump["o:crm.KeyAccount"].is_empty(),
        "the subtype row cascaded"
    );
}

#[test]
fn defaults_fill_omitted_properties_and_set_bumps_the_version() {
    let model = common::corpus_model();
    let mut store = CellStore::open_in_memory(model).expect("opens");
    let line: BTreeMap<String, Value> = [
        ("order_number", rv(json!("SO-1"))),
        ("line_number", rv(json!("3"))),
        ("unit_price", rv(json!("9.99"))),
        ("currency", rv(json!("EGP"))),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    store
        .commit(&batch(
            vec![create("commerce.SalesOrderLine", "SO-1/3", &line)],
            vec![],
        ))
        .expect("commits");
    let record = store
        .read()
        .object(MAIN_WORLD, "commerce.SalesOrderLine", "SO-1/3")
        .expect("reads")
        .expect("exists");
    assert_eq!(
        record.values["line_number"],
        rv(json!(3)),
        "integer strings normalize"
    );
    assert_eq!(record.values["quantity"], rv(json!(1)), "declared default");
    assert_eq!(
        record.values["status"],
        rv(json!("draft")),
        "declared default"
    );
    assert_eq!(
        record.values["requested_on"],
        Value::Null,
        "nullable, no default"
    );
    assert!(
        !record.values.contains_key("line_total"),
        "computed is not stored"
    );

    let set: FactChange = serde_json::from_value(json!({
        "op": "set", "objectType": "commerce.SalesOrderLine", "key": "SO-1/3",
        "property": "unit_price", "value": "10.50"
    }))
    .expect("decodes");
    store.commit(&batch(vec![set], vec![])).expect("commits");
    let record = store
        .read()
        .object(MAIN_WORLD, "commerce.SalesOrderLine", "SO-1/3")
        .expect("reads")
        .expect("exists");
    assert_eq!(record.version, 2);
    assert_eq!(record.values["unit_price"], rv(json!(10.5)));
}

#[test]
fn transitions_follow_the_declared_lifecycle() {
    let model = common::corpus_model();
    let mut store = CellStore::open_in_memory(model.clone()).expect("opens");
    let mut values = sample_values(&model, "commerce.PurchaseOrder", "PO-1");
    values.insert("status".to_owned(), rv(json!("draft")));
    values.insert("payment_status".to_owned(), rv(json!("unpaid")));
    store
        .commit(&batch(
            vec![create("commerce.PurchaseOrder", "PO-1", &values)],
            vec![],
        ))
        .expect("commits");
    let transition = |name: &str| -> FactChange {
        serde_json::from_value(json!({
            "op": "transition", "objectType": "commerce.PurchaseOrder", "key": "PO-1",
            "lifecycle": "status", "transition": name
        }))
        .expect("decodes")
    };
    store
        .commit(&batch(vec![transition("submit")], vec![]))
        .expect("draft → submitted");
    let error = store
        .commit(&batch(vec![transition("submit")], vec![]))
        .expect_err("submit does not leave submitted");
    assert!(
        matches!(error, CellError::TransitionNotAllowed { ref state, .. } if state == "submitted"),
        "{error}"
    );
    let record = store
        .read()
        .object(MAIN_WORLD, "commerce.PurchaseOrder", "PO-1")
        .expect("reads")
        .expect("exists");
    assert_eq!(record.values["status"], rv(json!("submitted")));
    assert_eq!(record.version, 2, "the refused commit wrote nothing");

    let direct: FactChange = serde_json::from_value(json!({
        "op": "set", "objectType": "commerce.PurchaseOrder", "key": "PO-1",
        "property": "status", "value": "approved"
    }))
    .expect("decodes");
    assert!(matches!(
        store.commit(&batch(vec![direct], vec![])),
        Err(CellError::LifecycleWrite { .. })
    ));
}

#[test]
fn encodings_and_cardinality_fail_closed() {
    let model = common::corpus_model();
    let mut store = CellStore::open_in_memory(model.clone()).expect("opens");
    let mut values = sample_values(&model, "commerce.SalesOrderLine", "L1");
    values.insert("unit_price".to_owned(), rv(json!("1e3")));
    assert!(matches!(
        store.commit(&batch(
            vec![create("commerce.SalesOrderLine", "L1", &values)],
            vec![]
        )),
        Err(CellError::ValueType { .. })
    ));
    let mut values = sample_values(&model, "commerce.SalesOrderLine", "L1");
    values.remove("order_number");
    assert!(matches!(
        store.commit(&batch(
            vec![create("commerce.SalesOrderLine", "L1", &values)],
            vec![]
        )),
        Err(CellError::MissingValue { .. })
    ));

    // Two objects with the same declared key in one scope and world.
    let customer = sample_values(&model, "crm.Customer", "C");
    assert!(matches!(
        store.commit(&batch(
            vec![
                create("crm.Customer", "C1", &customer),
                create("crm.Customer", "C2", &customer)
            ],
            vec![]
        )),
        Err(CellError::Sqlite(_))
    ));

    // `to: one`: a sales order has one customer.
    let mut first = customer.clone();
    first.insert("customer_number".to_owned(), rv(json!("C-1")));
    let mut second = customer;
    second.insert("customer_number".to_owned(), rv(json!("C-2")));
    let order = sample_values(&model, "commerce.SalesOrder", "SO");
    let link_from = |source: &str, target: &str| LinkChange::Add {
        object_type: "commerce.SalesOrder".to_owned(),
        link: "customer".to_owned(),
        source: source.to_owned(),
        target: target.to_owned(),
        values: BTreeMap::new(),
        target_type: None,
    };
    let link = |target: &str| link_from("SO-1", target);
    store
        .commit(&batch(
            vec![
                create("crm.Customer", "C1", &first),
                create("crm.Customer", "C2", &second),
                create("commerce.SalesOrder", "SO-1", &order),
            ],
            vec![link("C1")],
        ))
        .expect("commits");
    assert!(matches!(
        store.commit(&batch(vec![], vec![link("C2")])),
        Err(CellError::Sqlite(_))
    ));
    // A link to a missing target violates its foreign key.
    let mut other_order = sample_values(&model, "commerce.SalesOrder", "SO-2");
    other_order.insert("order_number".to_owned(), rv(json!("SO-2")));
    assert!(matches!(
        store.commit(&batch(
            vec![create("commerce.SalesOrder", "SO-2", &other_order)],
            vec![link_from("SO-2", "missing")]
        )),
        Err(CellError::Sqlite(_))
    ));
    let store_links = store
        .read()
        .links(MAIN_WORLD, "commerce.SalesOrder", "customer", "SO-1")
        .expect("reads");
    assert_eq!(store_links.len(), 1);
}

#[test]
fn reopening_with_another_model_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    drop(CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("opens"));
    let other = common::model_of(&[serde_json::from_value(json!({
        "name": "acme.Other",
        "properties": [{ "id": "note", "type": { "kind": "text" } }]
    }))
    .expect("decodes")]);
    assert!(matches!(
        CellStore::open(&path, other, StoreOptions::default()),
        Err(CellError::SchemaMismatch { .. })
    ));
}

#[test]
fn subtype_keys_are_unique_per_scope() {
    let base: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" } }]
    }))
    .expect("decodes");
    let derived: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "acme.Derived",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" } }],
        "identity": [{ "name": "by_tag", "properties": ["tag"] }]
    }))
    .expect("decodes");
    let mut store = CellStore::open_in_memory(common::model_of(&[base, derived])).expect("opens");
    let values: BTreeMap<String, Value> = [("code", rv(json!("c"))), ("tag", rv(json!("t")))]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    let in_scope = |scope: &str, key: &str| CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: scope.to_owned(),
        facts: vec![create("acme.Derived", key, &values)],
        ..CommitBatch::default()
    };
    store.commit(&in_scope("tenant-a", "D1")).expect("commits");
    store
        .commit(&in_scope("tenant-b", "D2"))
        .expect("the same key in another scope");
    assert!(matches!(
        store.commit(&in_scope("tenant-a", "D3")),
        Err(CellError::Sqlite(_))
    ));
}

#[test]
fn structured_values_round_trip_in_one_sqlite_column() {
    let definition = serde_json::from_value(json!({"name":"test.Point","properties":[{
        "id":"point","type":{"kind":"record","members":[
            {"id":"latitude","type":{"kind":"decimal","scale":6}},
            {"id":"longitude","type":{"kind":"decimal","scale":6}}
        ]},"constraints":{"nullable":true}
    }]}))
    .expect("definition");
    let model = common::model_of(&[definition]);
    let table = model.schema().object_table("test.Point").expect("table");
    assert_eq!(table.columns.len(), 1);
    assert_eq!(table.columns[0].kind, PropertyKind::Record);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("record.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("open");
    let point = Value::from_json_text(r#"{"latitude":30.123456,"longitude":31.234567}"#)
        .expect("exact point");
    let values = BTreeMap::from([("point".to_owned(), point)]);
    store
        .commit(&batch(vec![create("test.Point", "one", &values)], vec![]))
        .expect("commit");
    drop(store);
    let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopen");
    let record = reopened
        .read()
        .object(MAIN_WORLD, "test.Point", "one")
        .expect("read")
        .expect("record");
    assert_eq!(record.values, values);
}

#[test]
fn link_record_defaults_materialize_typed_members() {
    let definition = serde_json::from_value(json!({"name":"test.Node","properties":[],"links":[{
        "id":"next","target":"test.Node","cardinality":{"to":"many","from":"many"},
        "properties":[{"id":"detail","type":{"kind":"record","members":[
            {"id":"amount","type":{"kind":"decimal","scale":2}}
        ]},"default":{"amount":"1.25"}}]
    }]}))
    .expect("definition");
    let model = common::model_of(&[definition]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&batch(
            vec![
                create("test.Node", "a", &BTreeMap::new()),
                create("test.Node", "b", &BTreeMap::new()),
            ],
            vec![LinkChange::Add {
                object_type: "test.Node".to_owned(),
                link: "next".to_owned(),
                source: "a".to_owned(),
                target: "b".to_owned(),
                values: BTreeMap::new(),
                target_type: None,
            }],
        ))
        .expect("commit");
    let links = store
        .read()
        .links(MAIN_WORLD, "test.Node", "next", "a")
        .expect("links");
    let expected = Value::from_json_text(r#"{"amount":1.25}"#).expect("exact record");
    assert_eq!(links[0].values.get("detail"), Some(&expected));
}
