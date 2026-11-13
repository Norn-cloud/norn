#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    missing_docs,
    reason = "VIEW-ROWS-1 native conformance"
)]

//! A `ViewIR` v1 transform evaluated over one authorized cell snapshot
//! (VIEW-ROWS-1, VIEW-LINEAGE-1, VIEW-ENGINE-1).

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::compute::transform::{RowAccess, TransformMember};
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, LinkChange, Model, ObjectRecord};
use norn_checker::transform::{TransformPlan, admit};
use norn_ir::{ArtifactEnvelope, Decimal, ObjectTypeDef, RuntimeValue as V};
use serde_json::{Value, json};

const WORLD: &str = "synthetic-k1";
const SCOPE: &str = "synthetic";

fn order_type() -> Value {
    json!({"name": "shop.Order", "properties": [
        {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
        {"id": "placed_at", "type": {"kind": "timestamp", "timezone": "utc"}}
    ], "links": [{"id": "customer", "target": "shop.Customer",
        "cardinality": {"to": "one", "from": "many"}}]})
}

fn customer_type() -> Value {
    json!({"name": "shop.Customer", "properties": [{"id": "region", "type": {"kind": "text"}}]})
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

/// Revenue by customer region per ISO week, ranked within the region.
fn plan() -> TransformPlan {
    plan_with(json!({"maintenance": "recompute_on_read"}))
}

/// The weekly-revenue plan with `maintenance` (and `refresh`) from `upkeep`.
fn plan_with(upkeep: Value) -> TransformPlan {
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
        "shop.revenue_by_region",
        "derived",
        json!([pin("shop.Order"), pin("shop.Customer")]),
        json!({
            "viewVersion": "0.1", "viewId": "shop.revenue_by_region",
            "inputs": [input("shop.Order", "order"), input("shop.Customer", "customer")],
            "calc": {"kind": "relational", "output": "ranked", "nodes": {
                "order": {"op": "scan", "input": "order"},
                "customer": {"op": "scan", "input": "customer"},
                "joined": {"op": "join", "left": "order", "right": "customer",
                    "on": {"link": {"column": "id", "link": "customer"}}, "kind": "left", "prefix": "c_"},
                "regional": {"op": "project", "input": "joined", "columns": [
                    {"name": "id", "expression": "input.id"},
                    {"name": "region", "expression": "input.c_region ?? \"unknown\""},
                    {"name": "amount", "expression": "input.amount"},
                    {"name": "placed_at", "expression": "input.placed_at"}]},
                "weekly": {"op": "aggregate", "input": "regional",
                    "groupBy": [{"column": "region"}, {"column": "placed_at", "grain": "week"}],
                    "measures": [{"name": "revenue", "fn": "sum", "column": "amount"},
                        {"name": "orders", "fn": "count"}]},
                "ranked": {"op": "window", "input": "weekly", "partitionBy": ["region"],
                    "orderBy": [{"column": "placed_at", "direction": "asc"}],
                    "columns": [{"name": "week_number", "fn": "row_number"},
                        {"name": "to_date", "fn": "running_sum", "column": "revenue"}]}
            }},
            "outputSchema": {}
        }),
    );
    let mut view = serde_json::to_value(&view).expect("view");
    for (key, value) in upkeep.as_object().expect("upkeep") {
        view["payload"][key] = value.clone();
    }
    let view: ArtifactEnvelope = serde_json::from_value(view).expect("view");
    let pool = vec![entity(order_type()), entity(customer_type()), view];
    admit(&pool[2], &pool, &BTreeSet::new()).expect("admitted")
}

fn store() -> CellStore {
    let objects: Vec<ObjectTypeDef> = [order_type(), customer_type()]
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("type"))
        .collect();
    CellStore::open_in_memory(Model::from_definitions(&objects, &[]).expect("model"))
        .expect("store")
}

fn put(object_type: &str, key: &str, values: &[(&str, V)]) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.into(),
        key: key.into(),
        values: values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    }
}

fn decimal(text: &str) -> V {
    V::from(Decimal::parse(text).expect("decimal"))
}

fn order(id: &str, amount: &str, at: &str) -> FactWriteRow {
    put(
        "shop.Order",
        id,
        &[("amount", decimal(amount)), ("placed_at", V::from(at))],
    )
}

fn link(order: &str, customer: &str) -> LinkChange {
    LinkChange::Add {
        object_type: "shop.Order".into(),
        link: "customer".into(),
        source: order.into(),
        target: customer.into(),
        target_type: Some("shop.Customer".into()),
        values: BTreeMap::new(),
    }
}

fn load(store: &mut CellStore) {
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: SCOPE.into(),
            logical_time: 1_792_972_800_000,
            writes: vec![
                put("shop.Customer", "c-north", &[("region", V::from("north"))]),
                put("shop.Customer", "c-south", &[("region", V::from("south"))]),
                order("o1", "10.00", "2026-11-02T09:00:00Z"),
                order("o2", "5.50", "2026-11-03T09:00:00Z"),
                order("o3", "7.25", "2026-11-10T09:00:00Z"),
                order("o4", "1.00", "2026-11-10T10:00:00Z"),
                order("o5", "2.00", "2026-11-11T10:00:00Z"),
            ],
            links: vec![
                link("o1", "c-north"),
                link("o2", "c-north"),
                link("o3", "c-north"),
                link("o4", "c-south"),
            ],
            ..CommitBatch::default()
        })
        .expect("commit");
}

fn row<'r>(
    result: &'r norn_cell::compute::transform::TransformResult,
    region: &str,
    week: &str,
) -> &'r norn_cell::compute::transform::TransformRow {
    result
        .rows
        .iter()
        .find(|row| {
            row.values["region"] == V::from(region) && row.values["placed_at"] == V::from(week)
        })
        .expect("row")
}

#[test]
fn a_transform_reads_the_cell_snapshot_with_row_lineage() {
    let plan = plan();
    let mut store = store();
    load(&mut store);
    let result = store
        .evaluate_transform(&plan, WORLD, SCOPE, |_| Ok(RowAccess::Read))
        .expect("evaluated");
    assert!(result.complete);
    assert_eq!(result.grain, ["region", "placed_at"]);
    assert_eq!(result.revision, store.verified_head().expect("head"));
    let north = row(&result, "north", "2026-11-02");
    assert_eq!(north.values["revenue"], decimal("15.5"));
    assert_eq!(north.values["orders"], decimal("2"));
    assert_eq!(north.values["week_number"], decimal("1"));
    let north_next = row(&result, "north", "2026-11-09");
    assert_eq!(north_next.values["to_date"], decimal("22.75"));
    // o5 has no customer: the left join keeps it under "unknown".
    assert_eq!(
        row(&result, "unknown", "2026-11-09").values["revenue"],
        decimal("2")
    );
    // Row lineage names every order and customer version the row derives from.
    let ids: Vec<&str> = north.members.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["c-north", "o1", "o2"]);
    assert!(north.members.contains(&TransformMember {
        alias: "order".into(),
        object_type: "shop.Order".into(),
        id: "o1".into(),
        version: 1,
    }));
    // The same snapshot gives the same pins; a new commit moves them.
    let again = store
        .evaluate_transform(&plan, WORLD, SCOPE, |_| Ok(RowAccess::Read))
        .expect("again");
    assert_eq!(again, result);
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: SCOPE.into(),
            logical_time: 1_792_972_900_000,
            writes: vec![order("o2", "6.50", "2026-11-03T09:00:00Z")],
            ..CommitBatch::default()
        })
        .expect("correction");
    let corrected = store
        .evaluate_transform(&plan, WORLD, SCOPE, |_| Ok(RowAccess::Read))
        .expect("corrected");
    assert_eq!(
        row(&corrected, "north", "2026-11-02").values["revenue"],
        decimal("16.5")
    );
    assert_ne!(corrected.lineage_pin, result.lineage_pin);
}

#[test]
fn rows_the_reader_may_only_count_are_excluded_and_counted() {
    let plan = plan();
    let mut store = store();
    load(&mut store);
    let result = store
        .evaluate_transform(&plan, WORLD, SCOPE, |record| {
            Ok(match record.id.as_str() {
                "c-north" => RowAccess::Count,
                "o4" => RowAccess::None,
                _ => RowAccess::Read,
            })
        })
        .expect("evaluated");
    // VIEW-ROWS-1: north's orders lose their region instead of revealing it.
    assert!(!result.complete);
    assert_eq!(
        result.withheld,
        BTreeMap::from([("customer".to_owned(), 1)])
    );
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.values["region"] != V::from("north"))
    );
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.values["region"] != V::from("south"))
    );
    assert!(
        result
            .rows
            .iter()
            .flat_map(|row| &row.members)
            .all(|member| member.id != "c-north" && member.id != "o4")
    );
    assert_eq!(
        row(&result, "unknown", "2026-11-02").values["revenue"],
        decimal("15.5")
    );
}

#[test]
fn a_transform_refuses_a_foreign_scope() {
    let plan = plan();
    let mut store = store();
    store.bind("scope", SCOPE).expect("bound scope");
    load(&mut store);
    assert!(matches!(
        store.evaluate_transform(&plan, WORLD, "other", |_| Ok(RowAccess::Read)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
}

/// A correction to `o2`: north's first week goes from 15.5 to 16.5.
fn correct(store: &mut CellStore) {
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: SCOPE.into(),
            logical_time: 1_792_972_900_000,
            writes: vec![order("o2", "6.50", "2026-11-03T09:00:00Z")],
            ..CommitBatch::default()
        })
        .expect("correction");
}

fn whole(_record: &ObjectRecord) -> Result<RowAccess, CellError> {
    Ok(RowAccess::Read)
}

fn north_first_week(result: &norn_cell::compute::transform::TransformResult) -> V {
    row(result, "north", "2026-11-02").values["revenue"].clone()
}

#[test]
fn a_materialization_is_served_exact_until_its_inputs_change() {
    let plan = plan_with(json!({"maintenance": "materialized"}));
    let mut store = store();
    load(&mut store);
    let first = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 1_000)
        .expect("served");
    let label = first.materialization.clone().expect("stored");
    assert_eq!((label.refreshed_at_ms, label.stale), (1_000, false));
    // The stored result is the live evaluation's envelope.
    let live = store
        .evaluate_transform(&plan, WORLD, SCOPE, whole)
        .expect("live");
    assert_eq!(first.result, live);
    // The same snapshot is answered from the store: same rows, same refresh time.
    let again = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 2_000)
        .expect("served");
    assert_eq!(again.result, first.result);
    assert_eq!(again.materialization, Some(label));
    // A changed snapshot refreshes on read.
    correct(&mut store);
    let refreshed = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 3_000)
        .expect("refreshed");
    let label = refreshed.materialization.clone().expect("stored");
    assert_eq!((label.refreshed_at_ms, label.stale), (3_000, false));
    assert_eq!(north_first_week(&refreshed.result), decimal("16.5"));

    // With maxAgeMs, a younger stored result is served for a changed
    // snapshot, labelled stale with its frontier; an older one refreshes.
    let aged = plan_with(json!({"maintenance": "materialized",
        "refresh": {"policy": "on_read", "maxAgeMs": 10_000}}));
    let mut store = crate::store();
    load(&mut store);
    let stored = store
        .serve_transform(&aged, (WORLD, SCOPE), whole, 1_000)
        .expect("served");
    let frontier = stored.result.revision;
    correct(&mut store);
    let young = store
        .serve_transform(&aged, (WORLD, SCOPE), whole, 5_000)
        .expect("served");
    let label = young.materialization.expect("stored");
    assert!(label.stale);
    assert_eq!(label.frontier, frontier);
    assert_eq!(north_first_week(&young.result), decimal("15.5"));
    let old = store
        .serve_transform(&aged, (WORLD, SCOPE), whole, 20_000)
        .expect("served");
    let label = old.materialization.expect("stored");
    assert_eq!((label.refreshed_at_ms, label.stale), (20_000, false));
    assert_eq!(north_first_week(&old.result), decimal("16.5"));
}

#[test]
fn a_manual_materialization_serves_its_frontier_labelled_stale() {
    let plan = plan_with(json!({"maintenance": "materialized", "refresh": {"policy": "manual"}}));
    let mut store = store();
    load(&mut store);
    // Nothing is stored until a refresh: the read is live.
    let before = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 1_000)
        .expect("live");
    assert_eq!(before.materialization, None);
    let refreshed = store
        .refresh_transform(&plan, (WORLD, SCOPE), whole, 2_000)
        .expect("refreshed")
        .expect("a whole reader refreshes");
    assert!(!refreshed.stale);
    correct(&mut store);
    // Reads never refresh a manual transform: the frontier is served, labelled.
    let stale = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 9_000_000)
        .expect("served");
    let label = stale.materialization.expect("stored");
    assert!(label.stale);
    assert_eq!(label.frontier, refreshed.frontier);
    assert_eq!(label.refreshed_at_ms, 2_000);
    assert_eq!(north_first_week(&stale.result), decimal("15.5"));
    store
        .refresh_transform(&plan, (WORLD, SCOPE), whole, 9_500_000)
        .expect("refreshed")
        .expect("a whole reader refreshes");
    let exact = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 9_600_000)
        .expect("served");
    assert_eq!(exact.materialization.map(|label| label.stale), Some(false));
    assert_eq!(north_first_week(&exact.result), decimal("16.5"));
}

#[test]
fn a_partial_reader_gets_a_live_evaluation_and_cannot_refresh() {
    let plan = plan_with(json!({"maintenance": "materialized"}));
    let mut store = store();
    load(&mut store);
    let stored = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 1_000)
        .expect("served");
    assert!(stored.materialization.is_some());
    let counted = |record: &ObjectRecord| -> Result<RowAccess, CellError> {
        Ok(if record.id == "c-north" {
            RowAccess::Count
        } else {
            RowAccess::Read
        })
    };
    let hidden = |record: &ObjectRecord| -> Result<RowAccess, CellError> {
        Ok(if record.id == "o4" {
            RowAccess::None
        } else {
            RowAccess::Read
        })
    };
    // A reader who may not read every input row is evaluated live.
    let partial = store
        .serve_transform(&plan, (WORLD, SCOPE), counted, 2_000)
        .expect("served");
    assert_eq!(partial.materialization, None);
    assert!(!partial.result.complete);
    assert!(
        partial
            .result
            .rows
            .iter()
            .all(|row| row.values["region"] != V::from("north"))
    );
    let invisible = store
        .serve_transform(&plan, (WORLD, SCOPE), hidden, 2_500)
        .expect("served");
    assert_eq!(invisible.materialization, None);
    assert!(
        invisible
            .result
            .rows
            .iter()
            .flat_map(|row| &row.members)
            .all(|member| member.id != "o4")
    );
    // Neither may refresh, and neither changed what is stored.
    assert_eq!(
        store
            .refresh_transform(&plan, (WORLD, SCOPE), counted, 3_000)
            .expect("refused"),
        None
    );
    assert_eq!(
        store
            .refresh_transform(&plan, (WORLD, SCOPE), hidden, 3_000)
            .expect("refused"),
        None
    );
    let again = store
        .serve_transform(&plan, (WORLD, SCOPE), whole, 4_000)
        .expect("served");
    assert_eq!(again.result, stored.result);
    assert_eq!(
        again.materialization.map(|label| label.refreshed_at_ms),
        Some(1_000)
    );
}
