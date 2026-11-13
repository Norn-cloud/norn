//! VIEW-ENGINE-1: an admitted transform returns the same rows and row lineage
//! from embedded `DuckDB` as from the exact interpreter over one snapshot.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "VIEW-ENGINE-1 native correspondence"
)]

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::compute::Engine;
use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, LinkChange, Model};
use norn_checker::transform::{TransformPlan, admit};
use norn_ir::{ArtifactEnvelope, Decimal, ObjectTypeDef, RuntimeValue as V};
use proptest::prelude::*;
use serde_json::{Value, json};

const WORLD: &str = "synthetic-k1";
const SCOPE: &str = "synthetic";

fn types() -> [Value; 2] {
    types_keyed(false)
}

fn types_keyed(keyed: bool) -> [Value; 2] {
    let mut customer = json!({"name": "shop.Customer", "properties": [
        {"id": "region", "type": {"kind": "text"}},
        {"id": "vip", "type": {"kind": "boolean"}}
    ]});
    if keyed {
        customer["identity"] = json!([{"name": "region", "properties": ["region"],
            "rule": {"normalize": [{"property": "region", "steps": ["lowercase"]}]}}]);
    }
    [
        json!({"name": "shop.Order", "properties": [
            {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
            {"id": "units", "type": {"kind": "integer", "additivity": {"kind": "additive"}}},
            {"id": "placed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
            {"id": "note", "constraints": {"nullable": true}, "type": {"kind": "text"}}
        ], "links": [{"id": "customer", "target": "shop.Customer",
            "cardinality": {"to": "one", "from": "many"}}]}),
        customer,
    ]
}

fn artifact(id: &str, kind: &str, dependencies: Value, payload: Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "shop", "name": id, "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": dependencies,
        "provenance": {"source": "authoring_sdk"}, "payload": payload
    }))
    .expect("artifact")
}

fn plan(output: &str, nodes: Value) -> TransformPlan {
    plan_over(types(), output, nodes)
}

fn plan_over([order, customer]: [Value; 2], output: &str, nodes: Value) -> TransformPlan {
    let entity = |object: Value| {
        let name = object["name"].as_str().expect("name").to_owned();
        artifact(
            &name,
            "entity",
            json!([]),
            json!({"entityVersion": "2.0", "objectType": object}),
        )
    };
    let pin = |id: &str| json!({"artifactId": id, "kind": "entity", "version": "0.1.0", "required": true});
    let input = |id: &str, alias: &str| json!({"artifactId": id, "kind": "entity", "version": "0.1.0", "alias": alias});
    let view = artifact(
        "shop.view",
        "derived",
        json!([pin("shop.Order"), pin("shop.Customer")]),
        json!({
            "viewVersion": "0.1", "viewId": "shop.view",
            "inputs": [input("shop.Order", "order"), input("shop.Customer", "customer")],
            "calc": {"kind": "relational", "output": output, "nodes": nodes},
            "outputSchema": {}, "maintenance": "recompute_on_read"
        }),
    );
    let pool = vec![entity(order), entity(customer), view];
    admit(&pool[2], &pool, &BTreeSet::new()).expect("admitted")
}

/// Every operator, joined, bucketed, ranked and deduplicated.
fn plans() -> Vec<TransformPlan> {
    let joined = || {
        json!({
            "order": {"op": "scan", "input": "order"},
            "customer": {"op": "scan", "input": "customer"},
            "joined": {"op": "join", "left": "order", "right": "customer",
                "on": {"link": {"column": "id", "link": "customer"}}, "kind": "left", "prefix": "c_"}
        })
    };
    let with = |mut nodes: Value, extra: Value| {
        for (id, node) in extra.as_object().expect("nodes") {
            nodes[id] = node.clone();
        }
        nodes
    };
    vec![
        plan(
            "ranked",
            with(
                joined(),
                json!({
                    "big": {"op": "filter", "input": "joined",
                        "predicate": "input.amount >= 1 and (input.note ?? \"\") != \"skip\""},
                    "regional": {"op": "project", "input": "big", "columns": [
                        {"name": "id", "expression": "input.id"},
                        {"name": "region", "expression": "input.c_region ?? \"unknown\""},
                        {"name": "amount", "expression": "input.amount"},
                        {"name": "units", "expression": "input.units"},
                        {"name": "placed_at", "expression": "input.placed_at"},
                        {"name": "weighted", "expression": "input.amount * input.units - 1"},
                        {"name": "vip", "expression": "input.c_vip ?? false"}]},
                    "weekly": {"op": "aggregate", "input": "regional",
                        "groupBy": [{"column": "region"}, {"column": "placed_at", "grain": "week"}],
                        "measures": [{"name": "revenue", "fn": "sum", "column": "amount"},
                            {"name": "units", "fn": "sum", "column": "units"},
                            {"name": "orders", "fn": "count"},
                            {"name": "vips", "fn": "count_distinct", "column": "vip"},
                            {"name": "first", "fn": "min", "column": "placed_at"},
                            {"name": "biggest", "fn": "max", "column": "amount"}]},
                    "ranked": {"op": "window", "input": "weekly", "partitionBy": ["region"],
                        "orderBy": [{"column": "placed_at", "direction": "asc"}],
                        "columns": [{"name": "week_number", "fn": "row_number"},
                            {"name": "rank", "fn": "rank"},
                            {"name": "dense", "fn": "dense_rank"},
                            {"name": "to_date", "fn": "running_sum", "column": "revenue"},
                            {"name": "previous", "fn": "lag", "column": "revenue"},
                            {"name": "next_first", "fn": "lead", "column": "first", "offset": 2}]}
                }),
            ),
        ),
        plan(
            "latest",
            with(
                joined(),
                json!({
                    "latest": {"op": "dedupe", "input": "joined", "key": ["c_id"],
                        "orderBy": [{"column": "placed_at", "direction": "desc"}, {"column": "id", "direction": "asc"}],
                        "keep": "last"}
                }),
            ),
        ),
        plan(
            "both",
            json!({
                "order": {"op": "scan", "input": "order"},
                "customer": {"op": "scan", "input": "customer"},
                "a": {"op": "project", "input": "order", "columns": [
                    {"name": "id", "expression": "input.id"}, {"name": "label", "expression": "input.note ?? \"none\""}]},
                "b": {"op": "project", "input": "customer", "columns": [
                    {"name": "id", "expression": "input.id"}, {"name": "label", "expression": "input.region"}]},
                "both": {"op": "union", "inputs": ["a", "b"], "branch": "system"}
            }),
        ),
        plan(
            "by_note",
            json!({
                "order": {"op": "scan", "input": "order"},
                "by_note": {"op": "aggregate", "input": "order", "groupBy": [{"column": "note"}],
                    "measures": [{"name": "total", "fn": "sum", "column": "amount"},
                        {"name": "n", "fn": "count", "column": "note"}]}
            }),
        ),
    ]
}

fn store() -> CellStore {
    let objects: Vec<ObjectTypeDef> = types()
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("type"))
        .collect();
    CellStore::open_in_memory(Model::from_definitions(&objects, &[]).expect("model"))
        .expect("store")
}

fn put(object_type: &str, key: &str, values: Vec<(&str, V)>) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.into(),
        key: key.into(),
        values: values
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    }
}

type Order = (i64, i64, u32, u32, Option<u8>, Option<usize>);

fn load(store: &mut CellStore, customers: &[(u8, bool)], orders: &[Order]) {
    let regions = ["north", "south", "east"];
    let notes = ["skip", "gift", "rush"];
    let mut writes = Vec::new();
    for (index, (region, vip)) in customers.iter().enumerate() {
        writes.push(put(
            "shop.Customer",
            &format!("c{index}"),
            vec![
                ("region", V::from(regions[usize::from(*region) % 3])),
                ("vip", V::Bool(*vip)),
            ],
        ));
    }
    let mut links = Vec::new();
    for (index, (cents, units, day, minute, note, customer)) in orders.iter().enumerate() {
        let id = format!("o{index}");
        let mut values = vec![
            (
                "amount",
                V::from(
                    Decimal::parse(&format!(
                        "{}{}.{:02}",
                        if *cents < 0 { "-" } else { "" },
                        cents.abs() / 100,
                        cents.abs() % 100
                    ))
                    .expect("amount"),
                ),
            ),
            ("units", V::from(Decimal::from(*units))),
            (
                "placed_at",
                V::from(format!(
                    "2026-10-{:02}T{:02}:{:02}:00+02:00",
                    1 + day % 28,
                    minute / 60 % 24,
                    minute % 60
                )),
            ),
        ];
        if let Some(note) = note {
            values.push(("note", V::from(notes[usize::from(*note) % 3])));
        }
        writes.push(put("shop.Order", &id, values));
        if let Some(customer) = customer.filter(|c| *c < customers.len()) {
            links.push(LinkChange::Add {
                object_type: "shop.Order".into(),
                link: "customer".into(),
                source: id,
                target: format!("c{customer}"),
                target_type: Some("shop.Customer".into()),
                values: BTreeMap::new(),
            });
        }
    }
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: SCOPE.into(),
            logical_time: 1_792_972_800_000,
            writes,
            links,
            ..CommitBatch::default()
        })
        .expect("commit");
}

fn same(exact: Result<TransformResult, CellError>, duck: Result<TransformResult, CellError>) {
    let exact = exact.expect("exact");
    let mut duck = duck.expect("duckdb");
    assert_eq!(duck.engine, Engine::Duckdb);
    duck.engine = Engine::Cell;
    assert_eq!(exact, duck);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn duckdb_rows_and_lineage_equal_the_exact_interpreter(
        customers in prop::collection::vec((0u8..3, any::<bool>()), 0..5),
        orders in prop::collection::vec(
            (-500i64..50_000, 0i64..9, 0u32..60, 0u32..2_880, prop::option::of(0u8..3), prop::option::of(0usize..6)),
            0..24,
        ),
        hidden in prop::option::of(0usize..5),
    ) {
        let mut store = store();
        load(&mut store, &customers, &orders);
        let access = |record: &norn_cell::ObjectRecord| {
            Ok(if hidden.is_some_and(|n| record.id == format!("c{n}")) {
                RowAccess::Count
            } else {
                RowAccess::Read
            })
        };
        for plan in plans() {
            same(
                store.evaluate_transform(&plan, WORLD, SCOPE, access),
                store.evaluate_transform_duckdb(&plan, WORLD, SCOPE, access),
            );
        }
    }
}

#[test]
fn a_normalized_key_join_refuses_the_lowering_not_the_answer() {
    let keyed = plan_over(
        types_keyed(true),
        "j",
        json!({
            "order": {"op": "scan", "input": "order"},
            "customer": {"op": "scan", "input": "customer"},
            "labelled": {"op": "project", "input": "order", "columns": [
                {"name": "id", "expression": "input.id"},
                {"name": "label", "expression": "input.note ?? \"none\""}]},
            "j": {"op": "join", "left": "labelled", "right": "customer",
                "on": {"key": {"left": ["label"], "right": "region"}}, "kind": "inner", "prefix": "c_"}
        }),
    );
    let objects: Vec<ObjectTypeDef> = types_keyed(true)
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("type"))
        .collect();
    let mut store =
        CellStore::open_in_memory(Model::from_definitions(&objects, &[]).expect("model"))
            .expect("store");
    load(&mut store, &[(0, true)], &[(100, 1, 1, 1, None, Some(0))]);
    assert!(
        store
            .evaluate_transform(&keyed, WORLD, SCOPE, |_| Ok(RowAccess::Read))
            .is_ok()
    );
    assert!(matches!(
        store.evaluate_transform_duckdb(&keyed, WORLD, SCOPE, |_| Ok(RowAccess::Read)),
        Err(CellError::ComputeRefused("core.compute.unsupported_engine"))
    ));
}

/// VIEW-AGG-1 / METRIC-UNIT-PARTITION-1 parity: quantities aggregated per
/// unit and per currency (sums, extremes and a running sum) lower to `DuckDB`
/// with the exact engine's rows, lineage and volumes.
#[test]
fn quantities_grouped_by_unit_and_currency_lower_identically() {
    let order = json!({"name": "shop.Order", "properties": [
        {"id": "weight", "type": {"kind": "decimal", "scale": 3,
            "additivity": {"kind": "additive"}, "unitSource": ["weight_unit"]}},
        {"id": "weight_unit", "type": {"kind": "enum", "values": [{"value": "kg"}, {"value": "lb"}]}},
        {"id": "price", "type": {"kind": "decimal", "scale": 2,
            "additivity": {"kind": "additive"}, "currencySource": {"property": "currency"}}},
        {"id": "currency", "type": {"kind": "enum", "values": [{"value": "USD"}, {"value": "EGP"}]}}
    ]});
    let [_, customer] = types();
    let grouped = plan_over(
        [order.clone(), customer.clone()],
        "a",
        json!({
            "order": {"op": "scan", "input": "order"},
            "a": {"op": "aggregate", "input": "order",
                "groupBy": [{"column": "weight_unit"}, {"column": "currency"}],
                "measures": [{"name": "w", "fn": "sum", "column": "weight"},
                    {"name": "heaviest", "fn": "max", "column": "weight"},
                    {"name": "p", "fn": "sum", "column": "price"},
                    {"name": "cheapest", "fn": "min", "column": "price"}]}
        }),
    );
    let running = plan_over(
        [order.clone(), customer.clone()],
        "w",
        json!({
            "order": {"op": "scan", "input": "order"},
            "w": {"op": "window", "input": "order", "partitionBy": ["weight_unit"],
                "orderBy": [{"column": "id", "direction": "asc"}],
                "columns": [{"name": "to_date", "fn": "running_sum", "column": "weight"}]}
        }),
    );
    let objects: Vec<ObjectTypeDef> = [order, customer]
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("type"))
        .collect();
    let mut store =
        CellStore::open_in_memory(Model::from_definitions(&objects, &[]).expect("model"))
            .expect("store");
    let amount = |text: &str| V::Number(Decimal::parse(text).expect("decimal"));
    let shipment = |key: &str, weight: &str, unit: &str, price: &str, currency: &str| {
        put(
            "shop.Order",
            key,
            vec![
                ("weight", amount(weight)),
                ("weight_unit", V::from(unit)),
                ("price", amount(price)),
                ("currency", V::from(currency)),
            ],
        )
    };
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: SCOPE.into(),
            logical_time: 1_792_972_800_000,
            writes: vec![
                shipment("o1", "1.500", "kg", "10.00", "USD"),
                shipment("o2", "2.250", "lb", "5.50", "EGP"),
                shipment("o3", "0.750", "kg", "3.25", "USD"),
                shipment("o4", "4.000", "lb", "7.00", "USD"),
            ],
            ..CommitBatch::default()
        })
        .expect("commit");
    for plan in [&grouped, &running] {
        same(
            store.evaluate_transform(plan, WORLD, SCOPE, |_| Ok(RowAccess::Read)),
            store.evaluate_transform_duckdb(plan, WORLD, SCOPE, |_| Ok(RowAccess::Read)),
        );
    }
    // Kilograms in dollars are summed apart from pounds in either currency.
    let exact = store
        .evaluate_transform(&grouped, WORLD, SCOPE, |_| Ok(RowAccess::Read))
        .expect("exact");
    let kg_usd = exact
        .rows
        .iter()
        .find(|row| {
            row.values["weight_unit"] == V::from("kg") && row.values["currency"] == V::from("USD")
        })
        .expect("kg in USD");
    assert_eq!(kg_usd.values["w"], amount("2.250"));
    assert_eq!(kg_usd.values["p"], amount("13.25"));
    assert_eq!(exact.rows.len(), 3);
}
