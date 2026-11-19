//! Studio's transform fixture is the served documents: one synthetic shop's
//! weekly revenue by region and currency, read through the real routes, and
//! two checked queries (one refused for mixing currencies). `NORN_BLESS=1`
//! rewrites `apps/studio/test/fixtures/transforms.json`; otherwise the file
//! must equal what the routes serve now.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    missing_docs,
    reason = "Studio fixture correspondence: one synthetic shop is built in one function"
)]

use norn_api::authz::{AccessRequest, Authorizer, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_routes::ObjectAccess;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_api::transform_routes::{
    CompileTransformRequest, TransformRowsParams, compile_transform, get_transform_plan,
    get_transform_row_lineage, get_transform_rows,
};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD, ObjectRecord};
use norn_ir::{Decimal, ObjectTypeDef, RuntimeValue as V};
use norn_kernel::LogicalTime;
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TRANSFORM: &str = "shop.revenue_by_region";
/// The read time: the cell's commits are at this instant.
const NOW: LogicalTime = LogicalTime::from_millis(1_792_972_800_000);

struct Everyone;
impl Authorizer for Everyone {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, _: &ReadRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
}

struct Mapping;
impl ObjectAccess for Mapping {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: norn_api::authz::ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(PdpRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: format!("fixture.{capability:?}"),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: row
                    .map_or(object_type, |row| row.object_type.as_str())
                    .into(),
                id: row.map_or(object_type, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}

fn types() -> [Value; 2] {
    [
        json!({"name": "shop.Order", "properties": [
            {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"},
                "currencySource": {"property": "currency"}}},
            {"id": "currency", "type": {"kind": "enum", "values": [{"value": "USD"}, {"value": "EUR"}]}},
            {"id": "status", "type": {"kind": "enum", "values": [{"value": "paid"}, {"value": "void"}]}},
            {"id": "placed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
            {"id": "channel", "type": {"kind": "enum", "values": [{"value": "web"}, {"value": "store"}]}}
        ], "links": [{"id": "customer", "target": "shop.Customer",
            "cardinality": {"to": "one", "from": "many"}}]}),
        json!({"name": "shop.Customer", "properties": [
            {"id": "name", "type": {"kind": "text"}},
            {"id": "region", "type": {"kind": "text"}}
        ]}),
    ]
}

fn artifact(id: &str, kind: &str, title: &str, dependencies: Value, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "shop", "name": title, "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": dependencies,
        "provenance": {"source": "authoring_sdk"}, "payload": payload
    })
}

fn definitions() -> Vec<Value> {
    let [order, customer] = types();
    let pin = |id: &str| json!({"artifactId": id, "kind": "entity", "version": "0.1.0", "required": true});
    let input = |id: &str, alias: &str| json!({"artifactId": id, "kind": "entity", "version": "0.1.0", "alias": alias});
    vec![
        artifact(
            "shop.Order.entity",
            "entity",
            "Order",
            json!([]),
            json!({"entityVersion": "2.0", "objectType": order}),
        ),
        artifact(
            "shop.Customer.entity",
            "entity",
            "Customer",
            json!([]),
            json!({"entityVersion": "2.0", "objectType": customer}),
        ),
        artifact(
            TRANSFORM,
            "derived",
            "Weekly revenue by region",
            json!([pin("shop.Order.entity"), pin("shop.Customer.entity")]),
            json!({
                "viewVersion": "0.1", "viewId": TRANSFORM,
                "inputs": [input("shop.Order.entity", "order"), input("shop.Customer.entity", "customer")],
                "calc": {"kind": "relational", "output": "ranked", "nodes": {
                    "order": {"op": "scan", "input": "order"},
                    "customer": {"op": "scan", "input": "customer"},
                    "paid": {"op": "filter", "input": "order", "predicate": "input.status == \"paid\""},
                    "joined": {"op": "join", "left": "paid", "right": "customer",
                        "on": {"link": {"column": "id", "link": "customer"}}, "kind": "left", "prefix": "customer_"},
                    "regional": {"op": "project", "input": "joined", "columns": [
                        {"name": "id", "expression": "input.id"},
                        {"name": "region", "expression": "input.customer_region ?? \"unassigned\"", "label": "Region"},
                        {"name": "amount", "expression": "input.amount"},
                        {"name": "currency", "expression": "input.currency"},
                        {"name": "placed_at", "expression": "input.placed_at"}]},
                    "weekly": {"op": "aggregate", "input": "regional",
                        "groupBy": [{"column": "region"}, {"column": "placed_at", "grain": "week", "label": "Week"},
                            {"column": "currency", "label": "Currency"}],
                        "measures": [{"name": "revenue", "fn": "sum", "column": "amount", "label": "Revenue"},
                            {"name": "orders", "fn": "count", "label": "Orders"},
                            {"name": "largest", "fn": "max", "column": "amount", "label": "Largest order"}]},
                    "ranked": {"op": "window", "input": "weekly", "partitionBy": ["region", "currency"],
                        "orderBy": [{"column": "placed_at", "direction": "asc"}],
                        "columns": [{"name": "week_number", "fn": "row_number", "label": "Week no."},
                            {"name": "to_date", "fn": "running_sum", "column": "revenue", "label": "Revenue to date"},
                            {"name": "previous_week", "fn": "lag", "column": "revenue", "label": "Previous week"}]}
                }},
                "outputSchema": {}, "maintenance": "recompute_on_read"
            }),
        ),
    ]
}

fn put(object_type: &str, key: &str, values: &[(&str, V)]) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.into(),
        key: key.into(),
        values: values
            .iter()
            .map(|(n, v)| ((*n).to_owned(), v.clone()))
            .collect(),
    }
}

fn host() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let objects: Vec<ObjectTypeDef> = types()
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
    let model = norn_cell::Model::from_definitions(&objects, &[]).unwrap();
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let key = scope.key();
    let customers = [
        ("c-amal", "Amal Stores", "north"),
        ("c-badr", "Badr Trading", "north"),
        ("c-cairo", "Cairo Corner", "south"),
        ("c-delta", "Delta Supply", "east"),
    ];
    let orders = [
        (
            "o-1001",
            "c-amal",
            "120.00",
            "2026-10-26T09:10:00Z",
            "web",
            "USD",
            "paid",
        ),
        (
            "o-1002",
            "c-badr",
            "75.50",
            "2026-10-27T11:00:00Z",
            "store",
            "USD",
            "paid",
        ),
        (
            "o-1003",
            "c-cairo",
            "240.00",
            "2026-10-28T14:30:00Z",
            "web",
            "USD",
            "paid",
        ),
        (
            "o-1004",
            "c-delta",
            "18.25",
            "2026-10-29T08:45:00Z",
            "store",
            "USD",
            "paid",
        ),
        (
            "o-1005",
            "c-amal",
            "60.00",
            "2026-11-02T10:00:00Z",
            "web",
            "USD",
            "paid",
        ),
        (
            "o-1006",
            "c-cairo",
            "310.40",
            "2026-11-03T16:20:00Z",
            "store",
            "USD",
            "paid",
        ),
        (
            "o-1007",
            "c-cairo",
            "95.00",
            "2026-11-05T12:00:00Z",
            "web",
            "EUR",
            "paid",
        ),
        (
            "o-1008",
            "c-badr",
            "142.75",
            "2026-11-09T09:30:00Z",
            "web",
            "USD",
            "paid",
        ),
        (
            "o-1009",
            "c-delta",
            "66.00",
            "2026-11-10T15:00:00Z",
            "store",
            "USD",
            "paid",
        ),
        (
            "o-1010",
            "c-amal",
            "205.00",
            "2026-11-11T17:45:00Z",
            "web",
            "USD",
            "paid",
        ),
        (
            "o-1011",
            "",
            "40.00",
            "2026-11-11T18:05:00Z",
            "store",
            "USD",
            "paid",
        ),
        (
            "o-1012",
            "c-cairo",
            "0.00",
            "2026-11-12T08:00:00Z",
            "web",
            "USD",
            "void",
        ),
    ];
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&definitions()).unwrap(),
                    registry: vec![],
                    receipt_sha256: "synthetic-studio-transform-v1".into(),
                    admission_receipt: None,
                    readmission: None,
                    diff: json!({}),
                    installed_at: 1_792_972_800_000,
                    layout: None,
                },
                Some(0),
                &key,
            )
            .unwrap();
        let mut writes: Vec<FactWriteRow> = customers
            .iter()
            .map(|(id, name, region)| {
                put(
                    "shop.Customer",
                    id,
                    &[("name", V::from(*name)), ("region", V::from(*region))],
                )
            })
            .collect();
        let mut links = Vec::new();
        for (id, customer, amount, at, channel, currency, status) in orders {
            writes.push(put(
                "shop.Order",
                id,
                &[
                    ("amount", V::from(Decimal::parse(amount).unwrap())),
                    ("placed_at", V::from(at)),
                    ("channel", V::from(channel)),
                    ("currency", V::from(currency)),
                    ("status", V::from(status)),
                ],
            ));
            if !customer.is_empty() {
                links.push(LinkChange::Add {
                    object_type: "shop.Order".into(),
                    link: "customer".into(),
                    source: id.into(),
                    target: customer.into(),
                    target_type: Some("shop.Customer".into()),
                    values: BTreeMap::new(),
                });
            }
        }
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key,
                logical_time: 1_792_972_800_000,
                writes,
                links,
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Everyone);
    (host, directory)
}

#[test]
fn the_studio_fixture_is_what_the_transform_routes_serve() {
    let (host, _directory) = host();
    let scope = ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    };
    let actor = Some("studio");
    let rows = get_transform_rows(
        &host,
        &scope,
        actor,
        TRANSFORM,
        &TransformRowsParams {
            limit: Some("50".into()),
            after: None,
        },
        &Mapping,
        NOW,
    )
    .expect("rows");
    let lineage: BTreeMap<String, Value> = rows
        .rows
        .iter()
        .map(|row| {
            let document = get_transform_row_lineage(
                &host,
                &scope,
                actor,
                TRANSFORM,
                &row.row_id,
                &Mapping,
                NOW,
            )
            .expect("lineage");
            (row.row_id.clone(), serde_json::to_value(document).unwrap())
        })
        .collect();
    let plan = get_transform_plan(&host, &scope, actor, TRANSFORM, &Mapping).expect("plan");
    // Studio's "Check a query": the same request body it sends. Summing amounts
    // across currencies refuses (METRIC-UNIT-PARTITION-1); grouped by currency
    // it admits.
    let checks: Vec<Value> = [
        "SELECT o.channel AS channel, SUM(o.amount) AS revenue FROM \"shop.Order\" o GROUP BY o.channel",
        "SELECT o.channel AS channel, o.currency AS currency, SUM(o.amount) AS revenue FROM \"shop.Order\" o GROUP BY o.channel, o.currency",
    ]
    .into_iter()
    .map(|sql| {
        let request = CompileTransformRequest {
            id: "studio.checked_query".into(),
            title: "Checked query".into(),
            version: "0.1.0".into(),
            sql: sql.into(),
        };
        let result = compile_transform(&host, &scope, actor, &request).expect("checked");
        json!({"sql": sql, "result": result})
    })
    .collect();
    assert_eq!(checks[0]["result"]["admitted"], false);
    assert_eq!(
        checks[0]["result"]["diagnostics"][0]["code"],
        "metric.validation.mixed_currency"
    );
    assert_eq!(checks[1]["result"]["admitted"], true);
    let served = json!({
        "catalog": {"artifacts": definitions()},
        "rows": rows,
        "lineage": lineage,
        "plan": plan,
        "checks": checks,
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&served).unwrap());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/studio/test/fixtures/transforms.json");
    if std::env::var_os("NORN_BLESS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).expect("fixture; rerun with NORN_BLESS=1"),
        text,
        "the Studio fixture differs from the served documents; rerun with NORN_BLESS=1"
    );
}
