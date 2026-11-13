//! VIEW-READ-1 / VIEW-ROWS-1 correspondences: a transform's rows, row lineage
//! and plan through the read routes, admitted against the installed set.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "transform route correspondence"
)]

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_routes::ObjectAccess;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_api::transform_routes::{
    TransformRowsParams, get_transform_plan, get_transform_row_lineage, get_transform_rows,
};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD, ObjectRecord};
use norn_ir::{Decimal, ObjectTypeDef, RuntimeValue as V};
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TRANSFORM: &str = "shop.revenue_by_region";
const CLOCK: u64 = 1_792_972_800_000;

fn physical() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

fn scope() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

/// `full` reads everything; `pop` may count customer `c-north` but not read
/// it; `noview` has no grant on the transform; `nocust` none on customers.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let resource = request.access.resource.id.as_str();
        let allowed = match request.access.principal.subject_id.as_str() {
            "full" => true,
            "pop" => request.capability == ReadCapability::Population || resource != "c-north",
            "noview" => resource != TRANSFORM,
            "nocust" => resource != "shop.Customer",
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(Refusal::invalid("private policy diagnostic"))
        }
    }
}

struct Mapping;
impl ObjectAccess for Mapping {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
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
            {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}}
        ], "links": [{"id": "customer", "target": "shop.Customer",
            "cardinality": {"to": "one", "from": "many"}}]}),
        json!({"name": "shop.Customer", "properties": [{"id": "region", "type": {"kind": "text"}}]}),
    ]
}

fn artifact(id: &str, kind: &str, dependencies: Value, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "shop", "name": id, "version": "0.1.0", "lifecycle": "draft",
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
            "shop.Order",
            "entity",
            json!([]),
            json!({"entityVersion": "2.0", "objectType": order}),
        ),
        artifact(
            "shop.Customer",
            "entity",
            json!([]),
            json!({"entityVersion": "2.0", "objectType": customer}),
        ),
        artifact(
            TRANSFORM,
            "derived",
            json!([pin("shop.Order"), pin("shop.Customer")]),
            json!({
                "viewVersion": "0.1", "viewId": TRANSFORM,
                "inputs": [input("shop.Order", "order"), input("shop.Customer", "customer")],
                "calc": {"kind": "relational", "output": "by_region", "nodes": {
                    "order": {"op": "scan", "input": "order"},
                    "customer": {"op": "scan", "input": "customer"},
                    "joined": {"op": "join", "left": "order", "right": "customer",
                        "on": {"link": {"column": "id", "link": "customer"}}, "kind": "left", "prefix": "c_"},
                    "regional": {"op": "project", "input": "joined", "columns": [
                        {"name": "id", "expression": "input.id"},
                        {"name": "region", "expression": "input.c_region ?? \"unknown\""},
                        {"name": "amount", "expression": "input.amount"}]},
                    "by_region": {"op": "aggregate", "input": "regional",
                        "groupBy": [{"column": "region"}],
                        "measures": [{"name": "revenue", "fn": "sum", "column": "amount"}]}
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
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    }
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

fn host() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let objects: Vec<ObjectTypeDef> = types()
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
    let model = norn_cell::Model::from_definitions(&objects, &[]).unwrap();
    let key = physical().key();
    let amount = |text: &str| V::from(Decimal::parse(text).unwrap());
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&definitions()).unwrap(),
                    registry: vec![],
                    receipt_sha256: "synthetic-transform-v1".into(),
                    admission_receipt: None,
                    readmission: None,
                    diff: json!({}),
                    installed_at: CLOCK,
                    layout: None,
                },
                Some(0),
                &key,
            )
            .unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key,
                logical_time: CLOCK,
                writes: vec![
                    put("shop.Customer", "c-north", &[("region", V::from("north"))]),
                    put("shop.Customer", "c-south", &[("region", V::from("south"))]),
                    put("shop.Order", "o1", &[("amount", amount("10.00"))]),
                    put("shop.Order", "o2", &[("amount", amount("2.50"))]),
                    put("shop.Order", "o3", &[("amount", amount("4.00"))]),
                    put("shop.Order", "o4", &[("amount", amount("1.00"))]),
                ],
                links: vec![
                    link("o1", "c-north"),
                    link("o2", "c-north"),
                    link("o3", "c-south"),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy);
    (host, directory)
}

fn params(limit: Option<&str>, after: Option<&str>) -> TransformRowsParams {
    TransformRowsParams {
        limit: limit.map(str::to_owned),
        after: after.map(str::to_owned),
    }
}

#[test]
fn rows_page_with_lineage_under_the_installed_plan() {
    let (host, _directory) = host();
    let page = get_transform_rows(
        &host,
        &scope(),
        Some("full"),
        TRANSFORM,
        &params(None, None),
        &Mapping,
    )
    .expect("rows");
    assert!(page.complete);
    assert_eq!(page.grain, ["region"]);
    assert_eq!(page.total_rows, 3);
    let regions: Vec<&V> = page.rows.iter().map(|row| &row.values["region"]).collect();
    assert_eq!(
        regions,
        [&V::from("north"), &V::from("south"), &V::from("unknown")]
    );
    assert_eq!(
        page.rows[0].values["revenue"],
        V::from(Decimal::parse("12.5").unwrap())
    );
    assert_eq!(page.rows[0].member_count, 3);
    let revenue = page
        .columns
        .iter()
        .find(|c| c.name == "revenue")
        .expect("column");
    assert_eq!(revenue.additivity, "additive");
    assert_eq!(revenue.origins[0].property, "amount");

    let first = get_transform_rows(
        &host,
        &scope(),
        Some("full"),
        TRANSFORM,
        &params(Some("1"), None),
        &Mapping,
    )
    .expect("first page");
    let cursor = first.next_after.expect("more rows");
    let second = get_transform_rows(
        &host,
        &scope(),
        Some("full"),
        TRANSFORM,
        &params(Some("2"), Some(&cursor)),
        &Mapping,
    )
    .expect("second page");
    assert_eq!(second.rows.len(), 2);
    assert_eq!(second.next_after, None);
    let forged = get_transform_rows(
        &host,
        &scope(),
        Some("full"),
        TRANSFORM,
        &params(None, Some("0:nope")),
        &Mapping,
    )
    .expect_err("a cursor from another snapshot");
    assert_eq!(forged.code, "transform.snapshot_moved");

    let lineage = get_transform_row_lineage(
        &host,
        &scope(),
        Some("full"),
        TRANSFORM,
        &page.rows[0].row_id,
        &Mapping,
    )
    .expect("lineage");
    let members: Vec<(&str, &str)> = lineage
        .members
        .iter()
        .map(|m| (m.input.as_str(), m.id.as_str()))
        .collect();
    assert_eq!(
        members,
        [("customer", "c-north"), ("order", "o1"), ("order", "o2")]
    );

    let plan =
        get_transform_plan(&host, &scope(), Some("full"), TRANSFORM, &Mapping).expect("plan");
    let ops: Vec<&str> = plan.nodes.iter().map(|node| node.op.as_str()).collect();
    assert_eq!(ops.last(), Some(&"aggregate"));
    assert_eq!(plan.output, "by_region");
    assert_eq!(plan.inputs.len(), 2);
}

#[test]
fn every_gate_is_one_opaque_refusal_before_evaluation() {
    let (host, _directory) = host();
    let denied = |actor: Option<&str>| {
        get_transform_rows(
            &host,
            &scope(),
            actor,
            TRANSFORM,
            &params(None, None),
            &Mapping,
        )
        .expect_err("denied")
    };
    let none = denied(None);
    let view = denied(Some("noview"));
    let input = denied(Some("nocust"));
    assert_eq!(none.status, 403);
    assert_eq!(
        serde_json::to_string(&view).unwrap(),
        serde_json::to_string(&none).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        serde_json::to_string(&none).unwrap()
    );
    let missing = get_transform_rows(
        &host,
        &scope(),
        Some("full"),
        "shop.absent",
        &params(None, None),
        &Mapping,
    )
    .expect_err("undeclared");
    assert_eq!(
        (missing.status, missing.code.as_str()),
        (404, "transform.not_found")
    );
}

#[test]
fn a_reader_who_may_only_count_a_row_sees_it_withheld() {
    let (host, _directory) = host();
    let page = get_transform_rows(
        &host,
        &scope(),
        Some("pop"),
        TRANSFORM,
        &params(None, None),
        &Mapping,
    )
    .expect("rows");
    assert!(!page.complete);
    assert_eq!(page.withheld, BTreeMap::from([("customer".to_owned(), 1)]));
    let regions: Vec<&V> = page.rows.iter().map(|row| &row.values["region"]).collect();
    assert_eq!(regions, [&V::from("south"), &V::from("unknown")]);
    assert_eq!(
        page.rows[1].values["revenue"],
        V::from(Decimal::parse("13.5").unwrap())
    );
}

#[test]
fn installing_over_an_installed_set_readmits_the_whole_set() {
    use norn_api::kernel_set::{KernelRefusal, admit_kernel_set, admit_over, retained_artifacts};
    let envelopes = |values: Vec<Value>| -> Vec<norn_ir::ArtifactEnvelope> {
        values
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect()
    };
    let installed = admit_kernel_set(&envelopes(definitions())).expect("installed");
    // A new version of an installed artifact replaces it: the set keeps one.
    let mut transform = definitions().pop().unwrap();
    transform["version"] = json!("0.2.0");
    let again = admit_over(
        &installed.canonical_set,
        Some(&installed.receipt),
        &envelopes(vec![transform.clone()]),
    )
    .expect("admitted");
    let versions: Vec<Value> = retained_artifacts(&again.canonical_set)
        .unwrap()
        .iter()
        .filter(|artifact| artifact.artifact_id == TRANSFORM)
        .map(|artifact| serde_json::to_value(artifact).unwrap()["version"].clone())
        .collect();
    assert_eq!(versions, [json!("0.2.0")]);
    // What the checker refuses in the set is the install's refusal.
    transform["payload"]["calc"]["nodes"]["by_region"]["measures"][0]["column"] = json!("missing");
    let refused = admit_over(
        &installed.canonical_set,
        Some(&installed.receipt),
        &envelopes(vec![transform]),
    );
    let findings = match refused {
        Err(KernelRefusal::Refused(findings)) => findings,
        // Neither is the checker's refusal; the assertion below names it.
        Err(KernelRefusal::Invalid(_)) | Ok(_) => Vec::new(),
    };
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "derived.validation.unknown_column"),
        "{findings:#?}"
    );
}
