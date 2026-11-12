#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]

//! ViewIR v1 typed transforms: admission laws and the exact interpreter
//! (VIEW-TYPE-1 … VIEW-ENGINE-1, docs/spec/core/03-query-compute.md).

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::transform::eval::{InputRow, TransformInput, evaluate};
use norn_checker::transform::{admit, codes};
use norn_checker::{Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, Decimal, RuntimeValue, TimeGrain};
use proptest::prelude::*;
use serde_json::{Value, json};

fn artifact(id: &str, kind: &str, dependencies: Value, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "shop", "name": id, "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": dependencies,
        "provenance": {"source": "authoring_sdk"}, "payload": payload
    })
}

fn entity(name: &str, object_type: Value) -> Value {
    let mut object_type = object_type;
    object_type["name"] = json!(name);
    artifact(
        name,
        "entity",
        json!([]),
        json!({"entityVersion": "2.0", "objectType": object_type}),
    )
}

fn money() -> Value {
    json!({"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"},
        "currencySource": {"property": "currency"}})
}

fn ontology() -> Vec<Value> {
    vec![
        entity(
            "shop.Customer",
            json!({
                "properties": [
                    {"id": "email", "type": {"kind": "text"}},
                    {"id": "region", "type": {"kind": "text"}},
                    {"id": "credit", "type": {"kind": "decimal", "scale": 2,
                        "additivity": {"kind": "additive"}}}
                ],
                "keys": [{"name": "email", "properties": ["email"],
                    "rule": {"normalize": [{"property": "email", "steps": ["trim", "lowercase"]}]}}]
            }),
        ),
        entity(
            "shop.Tag",
            json!({
                "properties": [{"id": "label", "type": {"kind": "text"}}]
            }),
        ),
        entity(
            "shop.Order",
            json!({
                "properties": [
                    {"id": "placed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                    {"id": "currency", "type": {"kind": "enum", "values": [{"value": "EGP"}, {"value": "USD"}]}},
                    {"id": "amount", "type": money()},
                    {"id": "flat", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                    {"id": "balance", "type": {"kind": "decimal", "scale": 2,
                        "additivity": {"kind": "semi_additive"}}},
                    {"id": "rate", "type": {"kind": "decimal", "scale": 4}},
                    {"id": "note", "constraints": {"nullable": true}, "type": {"kind": "text"}}
                ],
                "links": [
                    {"id": "customer", "target": "shop.Customer",
                        "cardinality": {"to": "one", "from": "many", "required": true}},
                    {"id": "tags", "target": "shop.Tag", "cardinality": {"to": "many", "from": "many"}}
                ]
            }),
        ),
        entity(
            "shop.Lead",
            json!({
                "properties": [
                    {"id": "email", "type": {"kind": "text"}},
                    {"id": "source", "type": {"kind": "text"}},
                    {"id": "score", "type": {"kind": "integer"}}
                ]
            }),
        ),
    ]
}

const TYPES: [(&str, &str); 4] = [
    ("customer", "shop.Customer"),
    ("tag", "shop.Tag"),
    ("order", "shop.Order"),
    ("lead", "shop.Lead"),
];

fn view(nodes: Value, output: &str) -> Value {
    let dependencies: Vec<Value> = TYPES
        .iter()
        .map(|(_, id)| json!({"artifactId": id, "kind": "entity", "version": "0.1.0", "required": true}))
        .collect();
    let inputs: Vec<Value> = TYPES
        .iter()
        .map(|(alias, id)| json!({"artifactId": id, "kind": "entity", "version": "0.1.0", "alias": alias}))
        .collect();
    artifact(
        "shop.view",
        "derived",
        json!(dependencies),
        json!({
            "viewVersion": "0.1", "viewId": "shop.view", "inputs": inputs,
            "calc": {"kind": "relational", "nodes": nodes, "output": output},
            "outputSchema": {}, "maintenance": "recompute_on_read"
        }),
    )
}

fn envelopes(documents: Vec<Value>) -> Vec<ArtifactEnvelope> {
    documents
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact"))
        .collect()
}

fn findings(nodes: Value, output: &str) -> Vec<Diagnostic> {
    let mut documents = ontology();
    documents.push(view(nodes, output));
    check_artifact_set(
        &envelopes(documents),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
}

fn admitted(nodes: Value, output: &str) -> norn_checker::transform::TransformPlan {
    let mut documents = ontology();
    documents.push(view(nodes, output));
    let pool = envelopes(documents);
    let found = findings_of(&pool);
    assert!(found.is_empty(), "{found:#?}");
    admit(pool.last().expect("view"), &pool, &BTreeSet::new()).expect("admitted")
}

fn findings_of(pool: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    check_artifact_set(
        pool,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
}

#[track_caller]
fn refuses(nodes: Value, output: &str, code: &str) {
    let found = findings(nodes, output);
    assert!(
        found.iter().any(|finding| finding.code == code),
        "expected {code}, found {found:#?}"
    );
}

#[track_caller]
fn admits(nodes: Value, output: &str) {
    let found = findings(nodes, output);
    assert!(found.is_empty(), "{found:#?}");
}

fn num(text: &str) -> RuntimeValue {
    RuntimeValue::Number(Decimal::parse(text).expect("decimal"))
}

fn s(text: &str) -> RuntimeValue {
    RuntimeValue::String(text.to_owned())
}

fn row(id: &str, values: &[(&str, RuntimeValue)]) -> InputRow {
    InputRow {
        id: id.to_owned(),
        version: 1,
        values: values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    }
}

fn order(id: &str, placed_at: &str, currency: &str, amount: &str) -> InputRow {
    row(
        id,
        &[
            ("placed_at", s(placed_at)),
            ("currency", s(currency)),
            ("amount", num(amount)),
            ("flat", num(amount)),
            ("balance", num("0")),
            ("rate", num("1")),
        ],
    )
}

fn scans() -> serde_json::Map<String, Value> {
    let mut nodes = serde_json::Map::new();
    for (alias, _) in TYPES {
        nodes.insert(alias.to_owned(), json!({"op": "scan", "input": alias}));
    }
    nodes
}

fn with(extra: Value) -> Value {
    let mut nodes = extra.as_object().expect("nodes").clone();
    // Add the scan of every alias a node reads that no node defines; the plan
    // refuses unreachable nodes, so only read aliases are scanned.
    let mut read = BTreeSet::new();
    for node in nodes.values() {
        for member in ["input", "left", "right"] {
            if let Some(Value::String(id)) = node.get(member) {
                read.insert(id.clone());
            }
        }
        if let Some(Value::Array(ids)) = node.get("inputs") {
            read.extend(ids.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    for (alias, node) in scans() {
        if read.contains(&alias) && !nodes.contains_key(&alias) {
            nodes.insert(alias, node);
        }
    }
    Value::Object(nodes)
}

#[test]
fn link_and_key_joins_are_the_only_joins() {
    let link = json!({"j": {"op": "join", "left": "order", "right": "customer",
        "on": {"link": {"column": "id", "link": "customer"}}, "kind": "inner", "prefix": "customer_"}});
    let plan = admitted(with(link), "j");
    let schema = plan.output_schema();
    assert_eq!(schema.grain, ["id"]);
    assert!(schema.column("customer_email").is_some());
    assert_eq!(
        plan.links,
        BTreeSet::from([("order".to_owned(), "customer".to_owned())])
    );

    // A declared key equivalence: an uploaded lead's email to the customer's email key.
    let key = json!({"j": {"op": "join", "left": "lead", "right": "customer",
        "on": {"key": {"left": ["email"], "right": "email"}}, "kind": "left", "prefix": "c_"}});
    let plan = admitted(with(key.clone()), "j");
    let input = TransformInput {
        rows: BTreeMap::from([
            (
                "lead".to_owned(),
                vec![
                    row(
                        "l1",
                        &[
                            ("email", s("  Ada@Example.COM ")),
                            ("source", s("csv")),
                            ("score", num("3")),
                        ],
                    ),
                    row(
                        "l2",
                        &[
                            ("email", s("nobody@example.com")),
                            ("source", s("csv")),
                            ("score", num("1")),
                        ],
                    ),
                ],
            ),
            (
                "customer".to_owned(),
                vec![row(
                    "c1",
                    &[
                        ("email", s("ada@example.com")),
                        ("region", s("north")),
                        ("credit", num("10.00")),
                    ],
                )],
            ),
        ]),
        links: BTreeMap::new(),
    };
    let out = evaluate(&plan, &input).expect("evaluated");
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.rows[0].values["c_id"], s("c1"));
    assert_eq!(out.rows[0].members.len(), 2);
    assert_eq!(out.rows[1].values["c_id"], RuntimeValue::Null);

    refuses(
        with(
            json!({"j": {"op": "join", "left": "order", "right": "customer",
        "on": {"link": {"column": "id", "link": "owner"}}, "kind": "inner", "prefix": "c_"}}),
        ),
        "j",
        codes::JOIN_UNDECLARED,
    );
    refuses(
        with(json!({"j": {"op": "join", "left": "order", "right": "tag",
        "on": {"link": {"column": "id", "link": "customer"}}, "kind": "inner", "prefix": "c_"}})),
        "j",
        codes::JOIN_UNDECLARED,
    );
    refuses(
        with(
            json!({"j": {"op": "join", "left": "lead", "right": "customer",
        "on": {"key": {"left": ["email"], "right": "phone"}}, "kind": "inner", "prefix": "c_"}}),
        ),
        "j",
        codes::JOIN_UNDECLARED,
    );
    refuses(
        with(
            json!({"j": {"op": "join", "left": "lead", "right": "customer",
        "on": {"key": {"left": ["score"], "right": "email"}}, "kind": "inner", "prefix": "c_"}}),
        ),
        "j",
        codes::TYPE_MISMATCH,
    );
    // A link join starts from an id column, not from an arbitrary value.
    refuses(
        with(
            json!({"j": {"op": "join", "left": "lead", "right": "customer",
        "on": {"link": {"column": "email", "link": "customer"}}, "kind": "inner", "prefix": "c_"}}),
        ),
        "j",
        codes::JOIN_UNDECLARED,
    );
    // The right side must be one type's rows at their identity grain.
    refuses(
        with(json!({
        "by": {"op": "aggregate", "input": "customer", "groupBy": [{"column": "region"}],
            "measures": [{"name": "n", "fn": "count"}]},
        "j": {"op": "join", "left": "order", "right": "by",
            "on": {"link": {"column": "id", "link": "customer"}}, "kind": "inner", "prefix": "c_"}})),
        "j",
        codes::JOIN_UNDECLARED,
    );
    refuses(
        with(
            json!({"j": {"op": "join", "left": "order", "right": "customer",
        "on": {"link": {"column": "id", "link": "customer"}}, "kind": "inner", "prefix": ""}}),
        ),
        "j",
        codes::INVALID_PLAN,
    );
}

#[test]
fn fanned_sums_refuse_and_aggregating_first_counts_once() {
    // QUERY-FANOUT: summing an order's amount after a to-many tag join would
    // count it once per tag.
    refuses(
        with(json!({
        "j": {"op": "join", "left": "order", "right": "tag",
            "on": {"link": {"column": "id", "link": "tags"}}, "kind": "inner", "prefix": "tag_"},
        "a": {"op": "aggregate", "input": "j", "groupBy": [{"column": "tag_label"}],
            "measures": [{"name": "total", "fn": "sum", "column": "flat"}]}})),
        "a",
        codes::AMBIGUOUS_GRAIN,
    );
    // A to-one join repeats the customer for each of its orders.
    refuses(
        with(json!({
        "j": {"op": "join", "left": "order", "right": "customer",
            "on": {"link": {"column": "id", "link": "customer"}}, "kind": "inner", "prefix": "c_"},
        "a": {"op": "aggregate", "input": "j", "groupBy": [],
            "measures": [{"name": "credit", "fn": "sum", "column": "c_credit"}]}})),
        "a",
        codes::AMBIGUOUS_GRAIN,
    );
    // Deduplicating by the owner's id makes the value single again.
    admits(
        with(json!({
        "j": {"op": "join", "left": "order", "right": "customer",
            "on": {"link": {"column": "id", "link": "customer"}}, "kind": "inner", "prefix": "c_"},
        "d": {"op": "dedupe", "input": "j", "key": ["c_id"],
            "orderBy": [{"column": "id", "direction": "asc"}], "keep": "first"},
        "a": {"op": "aggregate", "input": "d", "groupBy": [],
            "measures": [{"name": "credit", "fn": "sum", "column": "c_credit"}]}})),
        "a",
    );
    // One order worth 80 with two tags: summing at the order grain is 80, and
    // counting the joined rows is two.
    let input = TransformInput {
        rows: BTreeMap::from([
            (
                "order".to_owned(),
                vec![order("o1", "2026-11-02T10:00:00Z", "EGP", "80.00")],
            ),
            (
                "tag".to_owned(),
                vec![
                    row("t1", &[("label", s("gold"))]),
                    row("t2", &[("label", s("gift"))]),
                ],
            ),
        ]),
        links: BTreeMap::from([(
            ("order".to_owned(), "tags".to_owned()),
            BTreeMap::from([("o1".to_owned(), vec!["t1".to_owned(), "t2".to_owned()])]),
        )]),
    };
    let total = admitted(
        with(
            json!({"t": {"op": "aggregate", "input": "order", "groupBy": [],
        "measures": [{"name": "total", "fn": "sum", "column": "flat"}]}}),
        ),
        "t",
    );
    assert_eq!(
        evaluate(&total, &input).expect("evaluated").rows[0].values["total"],
        num("80")
    );
    let joined = admitted(
        with(json!({
        "j": {"op": "join", "left": "order", "right": "tag",
            "on": {"link": {"column": "id", "link": "tags"}}, "kind": "inner", "prefix": "tag_"},
        "n": {"op": "aggregate", "input": "j", "groupBy": [],
            "measures": [{"name": "pairs", "fn": "count"}]}})),
        "n",
    );
    let out = evaluate(&joined, &input).expect("evaluated");
    assert_eq!(out.rows[0].values["pairs"], num("2"));
    assert_eq!(out.rows[0].members.len(), 3);
}

#[test]
fn aggregates_follow_the_metric_additivity_law() {
    let sum = |column: &str, group: Value| {
        with(
            json!({"a": {"op": "aggregate", "input": "order", "groupBy": group,
            "measures": [{"name": "m", "fn": "sum", "column": column}]}}),
        )
    };
    refuses(
        sum("balance", json!([])),
        "a",
        "question.validation.snapshot_aggregation",
    );
    refuses(
        sum("rate", json!([])),
        "a",
        "metric.validation.undeclared_additivity",
    );
    refuses(sum("note", json!([])), "a", codes::TYPE_MISMATCH);
    refuses(
        sum("amount", json!([])),
        "a",
        "metric.validation.mixed_currency",
    );
    admits(sum("amount", json!([{"column": "currency"}])), "a");
    refuses(
        with(
            json!({"a": {"op": "aggregate", "input": "order", "groupBy": [],
        "measures": [{"name": "m", "fn": "max", "column": "rate"}]}}),
        ),
        "a",
        "metric.validation.undeclared_additivity",
    );
    refuses(
        with(json!({"p": {"op": "project", "input": "order", "columns": [
            {"name": "id", "expression": "input.id"},
            {"name": "doubled", "expression": "input.flat + input.flat"}]},
        "a": {"op": "aggregate", "input": "p", "groupBy": [],
            "measures": [{"name": "m", "fn": "sum", "column": "doubled"}]}})),
        "a",
        "metric.validation.undeclared_additivity",
    );
    let plan = admitted(
        with(json!({"a": {"op": "aggregate", "input": "order",
        "groupBy": [{"column": "currency"}, {"column": "placed_at", "grain": "month"}],
        "measures": [{"name": "total", "fn": "sum", "column": "amount"},
            {"name": "orders", "fn": "count"},
            {"name": "latest", "fn": "max", "column": "placed_at"}]}})),
        "a",
    );
    let input = TransformInput {
        rows: BTreeMap::from([(
            "order".to_owned(),
            vec![
                order("o1", "2026-10-31T23:30:00-02:00", "EGP", "10.50"),
                order("o2", "2026-11-01T00:10:00Z", "EGP", "4.50"),
                order("o3", "2026-11-20T08:00:00Z", "USD", "7.00"),
            ],
        )]),
        links: BTreeMap::new(),
    };
    let out = evaluate(&plan, &input).expect("evaluated");
    // o1 is 2026-11-01T01:30Z: both EGP orders fall in November (UTC).
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.rows[0].values["total"], num("15"));
    assert_eq!(out.rows[0].values["orders"], num("2"));
    assert_eq!(out.rows[0].values["placed_at"], s("2026-11-01"));
    assert_eq!(out.rows[0].values["latest"], s("2026-10-31T23:30:00-02:00"));
    assert_eq!(out.rows[1].values["currency"], s("USD"));
}

#[test]
fn windows_and_dedupes_need_a_total_order() {
    refuses(
        with(
            json!({"w": {"op": "window", "input": "order", "partitionBy": ["currency"],
        "orderBy": [{"column": "placed_at", "direction": "asc"}],
        "columns": [{"name": "n", "fn": "row_number"}]}}),
        ),
        "w",
        codes::ORDER_NOT_TOTAL,
    );
    refuses(
        with(
            json!({"d": {"op": "dedupe", "input": "lead", "key": ["email"],
        "orderBy": [{"column": "score", "direction": "desc"}], "keep": "first"}}),
        ),
        "d",
        codes::ORDER_NOT_TOTAL,
    );
    let plan = admitted(
        with(
            json!({"w": {"op": "window", "input": "order", "partitionBy": ["currency"],
        "orderBy": [{"column": "placed_at", "direction": "asc"}, {"column": "id", "direction": "asc"}],
        "columns": [{"name": "n", "fn": "row_number"},
            {"name": "running", "fn": "running_sum", "column": "amount"},
            {"name": "previous", "fn": "lag", "column": "amount"}]}}),
        ),
        "w",
    );
    let input = TransformInput {
        rows: BTreeMap::from([(
            "order".to_owned(),
            vec![
                order("o2", "2026-11-02T00:00:00Z", "EGP", "2.00"),
                order("o1", "2026-11-01T00:00:00Z", "EGP", "1.00"),
                order("o3", "2026-11-01T00:00:00Z", "USD", "5.00"),
            ],
        )]),
        links: BTreeMap::new(),
    };
    let out = evaluate(&plan, &input).expect("evaluated");
    let by_id: BTreeMap<_, _> = out
        .rows
        .iter()
        .map(|r| (r.values["id"].clone(), r))
        .collect();
    assert_eq!(by_id[&s("o2")].values["n"], num("2"));
    assert_eq!(by_id[&s("o2")].values["running"], num("3"));
    assert_eq!(by_id[&s("o2")].values["previous"], num("1"));
    assert_eq!(by_id[&s("o1")].values["previous"], RuntimeValue::Null);
    assert_eq!(by_id[&s("o3")].values["n"], num("1"));
    // A running sum of money partitions by its currency.
    refuses(
        with(
            json!({"w": {"op": "window", "input": "order", "partitionBy": [],
        "orderBy": [{"column": "id", "direction": "asc"}],
        "columns": [{"name": "running", "fn": "running_sum", "column": "amount"}]}}),
        ),
        "w",
        "metric.validation.mixed_currency",
    );
    let dedupe = admitted(
        with(
            json!({"d": {"op": "dedupe", "input": "lead", "key": ["email"],
        "orderBy": [{"column": "score", "direction": "desc"}, {"column": "id", "direction": "asc"}],
        "keep": "first"}}),
        ),
        "d",
    );
    let input = TransformInput {
        rows: BTreeMap::from([(
            "lead".to_owned(),
            vec![
                row(
                    "a",
                    &[
                        ("email", s("x@example.com")),
                        ("source", s("csv")),
                        ("score", num("2")),
                    ],
                ),
                row(
                    "b",
                    &[
                        ("email", s("x@example.com")),
                        ("source", s("form")),
                        ("score", num("5")),
                    ],
                ),
            ],
        )]),
        links: BTreeMap::new(),
    };
    let out = evaluate(&dedupe, &input).expect("evaluated");
    assert_eq!(out.grain, ["email"]);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].values["id"], s("b"));
}

#[test]
fn unions_need_one_shape() {
    let shape = |column: &str| {
        json!({"op": "project", "input": "lead", "columns": [
            {"name": "id", "expression": "input.id"},
            {"name": "email", "expression": format!("input.{column}")}]})
    };
    refuses(
        with(json!({
        "a": shape("email"),
        "b": {"op": "project", "input": "customer", "columns": [
            {"name": "id", "expression": "input.id"},
            {"name": "region", "expression": "input.region"}]},
        "u": {"op": "union", "inputs": ["a", "b"], "branch": "system"}})),
        "u",
        codes::UNION_SHAPE,
    );
    let plan = admitted(
        with(json!({
        "a": shape("email"),
        "b": {"op": "project", "input": "customer", "columns": [
            {"name": "id", "expression": "input.id"},
            {"name": "email", "expression": "input.email"}]},
        "u": {"op": "union", "inputs": ["a", "b"], "branch": "system"}})),
        "u",
    );
    // Two types' ids name different objects: the union identity is the branch.
    assert_eq!(plan.output_schema().grain, ["system", "id"]);
}

#[test]
fn projections_keep_the_grain() {
    refuses(
        with(json!({"p": {"op": "project", "input": "order", "columns": [
        {"name": "amount", "expression": "input.amount"}]}})),
        "p",
        codes::GRAIN_DROPPED,
    );
    refuses(
        with(json!({"p": {"op": "project", "input": "order", "columns": [
        {"name": "id", "expression": "input.note ?? input.id"}]}})),
        "p",
        codes::GRAIN_DROPPED,
    );
    refuses(
        with(json!({"f": {"op": "filter", "input": "order", "predicate": "input.amount"}})),
        "f",
        codes::TYPE_MISMATCH,
    );
    refuses(
        with(json!({"f": {"op": "filter", "input": "order", "predicate": "input.missing > 1"}})),
        "f",
        codes::TYPE_MISMATCH,
    );
    refuses(
        with(json!({"p": {"op": "project", "input": "order", "columns": [
        {"name": "id", "expression": "input.id"}, {"name": "id", "expression": "input.id"}]}})),
        "p",
        codes::COLUMN_CONFLICT,
    );
    refuses(
        json!({"o": {"op": "scan", "input": "order"}, "c": {"op": "scan", "input": "customer"}}),
        "o",
        codes::INVALID_PLAN,
    );
    refuses(
        json!({"a": {"op": "filter", "input": "b", "predicate": "true"},
        "b": {"op": "filter", "input": "a", "predicate": "true"}}),
        "a",
        codes::INVALID_PLAN,
    );
}

fn pipeline() -> norn_checker::transform::TransformPlan {
    admitted(
        with(json!({
        "f": {"op": "filter", "input": "order", "predicate": "input.flat > 0"},
        "j": {"op": "join", "left": "f", "right": "customer",
            "on": {"link": {"column": "id", "link": "customer"}}, "kind": "left", "prefix": "c_"},
        "p": {"op": "project", "input": "j", "columns": [
            {"name": "id", "expression": "input.id"},
            {"name": "region", "expression": "input.c_region ?? \"none\""},
            {"name": "flat", "expression": "input.flat"},
            {"name": "placed_at", "expression": "input.placed_at"}]},
        "a": {"op": "aggregate", "input": "p",
            "groupBy": [{"column": "region"}, {"column": "placed_at", "grain": "week"}],
            "measures": [{"name": "total", "fn": "sum", "column": "flat"}, {"name": "orders", "fn": "count"}]},
        "w": {"op": "window", "input": "a", "partitionBy": ["region"],
            "orderBy": [{"column": "placed_at", "direction": "asc"}],
            "columns": [{"name": "rank", "fn": "rank"}]}})),
        "w",
    )
}

prop_compose! {
    fn orders()(rows in prop::collection::vec((0u32..40, 0i64..5_000, 0usize..4), 0..30)) -> Vec<(String, String, String, String)> {
        rows.into_iter().enumerate().map(|(index, (day, cents, customer))| {
            (format!("o{index}"), format!("{}T12:00:00Z", add_days("2026-10-01", day)),
                format!("{}.{:02}", cents / 100, cents % 100), format!("c{customer}"))
        }).collect()
    }
}

fn bucket(date: &str, grain: TimeGrain) -> String {
    let value = norn_expr::Value::String(date.to_owned());
    match norn_checker::transform::eval::bucket(&value, &norn_expr::Type::Date, grain)
        .expect("bucket")
    {
        norn_expr::Value::String(text) => text,
        other => panic!("{other:?}"),
    }
}

proptest! {
    #[test]
    fn evaluation_is_deterministic_under_input_order(rows in orders(), seed in any::<u64>()) {
        let plan = pipeline();
        let customers = vec![
            row("c0", &[("email", s("a@x")), ("region", s("north")), ("credit", num("0"))]),
            row("c1", &[("email", s("b@x")), ("region", s("south")), ("credit", num("0"))]),
            row("c2", &[("email", s("c@x")), ("region", s("north")), ("credit", num("0"))]),
        ];
        let build = |rows: &[(String, String, String, String)]| TransformInput {
            rows: BTreeMap::from([
                ("order".to_owned(), rows.iter().map(|(id, at, amount, _)| order(id, at, "EGP", amount)).collect()),
                ("customer".to_owned(), customers.clone()),
            ]),
            links: BTreeMap::from([(
                ("order".to_owned(), "customer".to_owned()),
                rows.iter().map(|(id, _, _, customer)| (id.clone(), vec![customer.clone()])).collect(),
            )]),
        };
        let forward = evaluate(&plan, &build(&rows)).expect("forward");
        let mut shuffled = rows.clone();
        let len = shuffled.len().max(1);
        shuffled.rotate_left(usize::try_from(seed % (len as u64)).unwrap_or(0));
        shuffled.reverse();
        let backward = evaluate(&plan, &build(&shuffled)).expect("backward");
        prop_assert_eq!(&forward, &backward);
        // Every positive order is counted exactly once across the buckets.
        let positive = rows.iter().filter(|(_, _, amount, _)| Decimal::parse(amount).expect("d") > Decimal::ZERO).count();
        let counted: i64 = forward.rows.iter().map(|r| match &r.values["orders"] {
            RuntimeValue::Number(n) => n.to_i64().expect("count"),
            _ => 0,
        }).sum();
        prop_assert_eq!(usize::try_from(counted).expect("count"), positive);
        let members: usize = forward.rows.iter().map(|r| r.members.iter().filter(|m| m.alias == "order").count()).sum();
        prop_assert_eq!(members, positive);
    }

    #[test]
    fn calendar_buckets_follow_iso_utc(day in 0u32..3_000) {
        // Dates from 2020-01-01 on, built by repeated addition from a known Monday.
        let date = add_days("2019-12-30", day);
        let week = bucket(&date, TimeGrain::Week);
        prop_assert_eq!(weekday(&week), 0);
        let gap = days_between(&week, &date);
        prop_assert!((0..7).contains(&gap));
        let month = bucket(&date, TimeGrain::Month);
        prop_assert!(month.ends_with("-01") && date.starts_with(&month[..8]));
        let quarter = bucket(&date, TimeGrain::Quarter);
        prop_assert!(matches!(&quarter[5..], "01-01" | "04-01" | "07-01" | "10-01"));
        prop_assert_eq!(bucket(&date, TimeGrain::Year), format!("{}-01-01", &date[..4]));
        prop_assert_eq!(bucket(&date, TimeGrain::Day), date.clone());
    }
}

const MONTH_DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn leap(year: u32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn add_days(start: &str, days: u32) -> String {
    let mut year: u32 = start[..4].parse().expect("y");
    let mut month: u32 = start[5..7].parse().expect("m");
    let mut day: u32 = start[8..].parse().expect("d");
    for _ in 0..days {
        let length = MONTH_DAYS[(month - 1) as usize] + u32::from(month == 2 && leap(year));
        day += 1;
        if day > length {
            day = 1;
            month += 1;
            if month > 12 {
                month = 1;
                year += 1;
            }
        }
    }
    format!("{year:04}-{month:02}-{day:02}")
}

/// 0 = Monday, counted from the Monday 2019-12-30.
fn weekday(date: &str) -> u32 {
    days_between("2019-12-30", date) % 7
}

fn days_between(from: &str, to: &str) -> u32 {
    let mut n = 0;
    let mut current = from.to_owned();
    while current != to {
        current = add_days(&current, 1);
        n += 1;
        assert!(n < 10_000, "{from} is after {to}");
    }
    n
}
