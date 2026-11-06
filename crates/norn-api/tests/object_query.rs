//! HOST-READ-3 correspondences: grid query grammar v1 under D67 withholding.
//! A Population-only caller sees a withheld row's id and nothing derived from
//! its values: filters never match it, sorts put it in the id-ordered tail,
//! and counts and cursors are the same whatever its values are.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "object query correspondence"
)]
#[path = "../../norn-cell/tests/query_support/mod.rs"]
mod query_support;

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_query::ObjectQueryParams;
use norn_api::object_routes::{ObjectAccess, get_object_query, get_queried_object};
use norn_api::objects::NornObjectQueryPage;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, ObjectRecord};
use norn_ir::{Decimal, RuntimeValue as V};
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use query_support::{CLOCK, TYPE, object_type};
use serde_json::json;
use std::collections::BTreeMap;

/// Rows `pop` holds Population on but not Values.
const WITHHELD: [&str; 2] = ["R-03", "R-06"];

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

/// `full` reads everything; `pop` reads every row's existence and the values
/// of every row but [`WITHHELD`]; `few` reads only the `R-` rows.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let resource = request.access.resource.id.as_str();
        let allowed = match request.access.principal.subject_id.as_str() {
            "full" => true,
            "few" => resource == TYPE || resource.starts_with("R-"),
            "pop" => {
                request.capability == ReadCapability::Population || !WITHHELD.contains(&resource)
            }
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

/// One conversion; `None` leaves the property missing.
fn row(key: &str, input: Option<&str>, metal: Option<&str>) -> FactWriteRow {
    let mut values = BTreeMap::from([("occurred_at".to_owned(), V::from("2026-07-10T12:00:00Z"))]);
    if let Some(input) = input {
        values.insert("input".into(), V::from(Decimal::parse(input).unwrap()));
    }
    if let Some(metal) = metal {
        values.insert("metal".into(), V::from(metal));
    }
    FactWriteRow::Put {
        object_type: TYPE.into(),
        key: key.into(),
        values,
    }
}

/// Six readable conversions and the two [`WITHHELD`] ones, whose values are
/// `secret`'s.
fn writes(secret: [(Option<&str>, Option<&str>); 2]) -> Vec<FactWriteRow> {
    vec![
        row("R-01", Some("100.5"), Some("gold")),
        row("R-02", Some("20"), Some("silver")),
        row("R-03", secret[0].0, secret[0].1),
        row("R-04", None, Some("gold")),
        row("R-05", Some("100.50"), None),
        row("R-06", secret[1].0, secret[1].1),
        row("R-07", Some("7"), Some("copper")),
        row("R-08", Some("999"), Some("gold")),
    ]
}

/// The conversion type with every property optional, so a value may be missing.
fn nullable_type() -> norn_ir::ObjectTypeDef {
    let mut encoded = serde_json::to_value(object_type()).unwrap();
    for property in encoded["properties"].as_array_mut().unwrap() {
        property["constraints"] = json!({"nullable": true});
    }
    serde_json::from_value(encoded).unwrap()
}

fn host_of(rows: Vec<FactWriteRow>) -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let object = nullable_type();
    let model = norn_cell::Model::from_definitions(std::slice::from_ref(&object), &[]).unwrap();
    let artifact = json!({"irVersion":"0.1","artifactId":"demo.Conversion.entity","kind":"entity","namespace":"demo","name":"Conversion","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}});
    let key = physical().key();
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&vec![artifact]).unwrap(),
                    registry: vec![],
                    receipt_sha256: "synthetic-query-v1".into(),
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
                writes: rows,
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

fn params(limit: Option<&str>, sort: Option<&str>, filter: Option<&str>) -> ObjectQueryParams {
    ObjectQueryParams {
        limit: limit.map(str::to_owned),
        sort: sort.map(str::to_owned),
        filter: filter.map(str::to_owned),
        after: None,
    }
}

fn query(
    host: &DevHost<NativeEngine>,
    actor: &str,
    params: &ObjectQueryParams,
) -> Result<NornObjectQueryPage, Refusal> {
    get_object_query(host, &scope(), Some(actor), TYPE, params, &Mapping)
}

/// Every page of `params`, following `next`.
fn pages(
    host: &DevHost<NativeEngine>,
    actor: &str,
    params: &ObjectQueryParams,
) -> Vec<NornObjectQueryPage> {
    let mut all = Vec::new();
    let mut params = params.clone();
    loop {
        let page = query(host, actor, &params).unwrap();
        params.after.clone_from(&page.next);
        all.push(page);
        if params.after.is_none() {
            return all;
        }
    }
}

fn ids(pages: &[NornObjectQueryPage]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| page.records.iter().map(|record| record.id.clone()))
        .collect()
}

fn filter(clauses: &serde_json::Value) -> String {
    json!({ "all": clauses }).to_string()
}

/// The queries a grid issues, sorted, filtered and paged.
fn grid_queries() -> Vec<ObjectQueryParams> {
    let mut queries = vec![
        params(None, None, None),
        params(Some("2"), None, None),
        params(None, Some("input:desc"), None),
        params(Some("3"), Some("input:asc"), None),
        params(None, Some("metal:asc,input:desc"), None),
    ];
    for clause in [
        json!({"property":"input","op":"gt","value":"50"}),
        json!({"property":"input","op":"lte","value":"100.5"}),
        json!({"property":"input","op":"between","value":["0","1000"]}),
        json!({"property":"metal","op":"eq","value":"gold"}),
        json!({"property":"metal","op":"ne","value":"gold"}),
        json!({"property":"metal","op":"in","value":["gold","silver"]}),
        json!({"property":"metal","op":"prefix","value":"go"}),
        json!({"property":"metal","op":"empty"}),
        json!({"property":"metal","op":"present"}),
        json!({"property":"input","op":"empty"}),
    ] {
        queries.push(params(None, None, Some(&filter(&json!([clause])))));
        queries.push(params(
            Some("2"),
            Some("input:desc"),
            Some(&filter(&json!([clause]))),
        ));
    }
    queries
}

/// D67: two cells that differ only in the withheld rows' values give a
/// Population-only caller byte-identical responses, page by page.
#[test]
fn withheld_values_never_change_a_population_only_response() {
    let (low, _low) = host_of(writes([(Some("1"), Some("gold")), (None, None)]));
    let (high, _high) = host_of(writes([(Some("5000"), None), (Some("60"), Some("silver"))]));
    for params in grid_queries() {
        let one = serde_json::to_string(&pages(&low, "pop", &params)).unwrap();
        let other = serde_json::to_string(&pages(&high, "pop", &params)).unwrap();
        assert_eq!(one, other, "{params:?}");
    }
}

/// The bisection probe stays refused: no threshold makes a withheld row
/// match, so a filter never narrows its hidden value, and the count is the
/// readable rows' alone.
#[test]
fn a_filter_never_matches_a_withheld_row() {
    let (host, _dir) = host_of(writes([(Some("512"), Some("gold")), (Some("8"), None)]));
    for threshold in ["0", "8", "256", "511", "512", "513", "100000"] {
        for op in ["lt", "lte", "gt", "gte", "eq", "ne"] {
            let clause = json!([{"property":"input","op":op,"value":threshold}]);
            let all = pages(&host, "pop", &params(None, None, Some(&filter(&clause))));
            let seen = ids(&all);
            assert!(
                WITHHELD.iter().all(|id| !seen.contains(&(*id).to_owned())),
                "{op} {threshold}: {seen:?}"
            );
            assert_eq!(
                all[0].total.as_ref().map(|total| total.count),
                Some(u64::try_from(seen.len()).unwrap()),
                "{op} {threshold}"
            );
        }
    }
    for op in ["empty", "present"] {
        let clause = json!([{"property":"metal","op":op}]);
        let seen = ids(&pages(
            &host,
            "pop",
            &params(None, None, Some(&filter(&clause))),
        ));
        assert!(
            WITHHELD.iter().all(|id| !seen.contains(&(*id).to_owned())),
            "{op}"
        );
    }
}

/// A withheld record carries its id and existence only.
#[test]
fn a_withheld_record_shows_no_values() {
    let (host, _dir) = host_of(writes([(Some("512"), Some("gold")), (Some("8"), None)]));
    let page = query(&host, "pop", &params(None, None, None)).unwrap();
    let wire = serde_json::to_value(&page).unwrap();
    let withheld: Vec<_> = wire["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["withheld"] == json!(true))
        .collect();
    assert_eq!(withheld.len(), 2);
    for record in withheld {
        assert_eq!(record["values"], json!(null));
        assert_eq!(record["version"], json!(null));
        assert_eq!(record["withheldProperties"], json!([]));
    }
    let readable = &wire["records"][0];
    assert_eq!(readable["withheld"], json!(false));
    assert_eq!(readable["withheldProperties"], json!([]));
    let one = get_queried_object(&host, &scope(), Some("pop"), TYPE, "R-03", &Mapping).unwrap();
    assert!(one.withheld && one.values.is_none() && one.version.is_none());
    let full = get_queried_object(&host, &scope(), Some("full"), TYPE, "R-03", &Mapping).unwrap();
    assert!(!full.withheld && full.values.is_some());
}

/// Missing and withheld values sort last in both directions, as one tail in
/// id order; decimals compare exactly and `id` breaks ties.
#[test]
fn a_sort_puts_missing_and_withheld_values_last_in_id_order() {
    let (host, _dir) = host_of(writes([(Some("512"), Some("gold")), (Some("8"), None)]));
    let order =
        |actor, sort, limit| ids(&pages(&host, actor, &params(Some(limit), Some(sort), None)));
    assert_eq!(
        order("pop", "input:desc", "3"),
        [
            "R-08", "R-01", "R-05", "R-02", "R-07", "R-03", "R-04", "R-06"
        ]
    );
    assert_eq!(
        order("pop", "input:asc", "2"),
        [
            "R-07", "R-02", "R-01", "R-05", "R-08", "R-03", "R-04", "R-06"
        ]
    );
    assert_eq!(
        order("full", "input:asc", "50"),
        [
            "R-07", "R-06", "R-02", "R-01", "R-05", "R-03", "R-08", "R-04"
        ]
    );
    let page = query(&host, "pop", &params(None, Some("input:desc"), None)).unwrap();
    assert_eq!(page.query.sort[0].property, "input");
    assert_eq!(page.query.sort[0].direction, "desc");
    let total = page.total.unwrap();
    assert_eq!((total.count, total.approximate), (8, false));
}

/// Every violation is reported at once, each at its pointer.
#[test]
fn an_invalid_query_lists_every_violation() {
    let (host, _dir) = host_of(writes([(None, None), (None, None)]));
    let bad = filter(&json!([
        {"property":"ghost","op":"eq","value":"x"},
        {"property":"input","op":"contains","value":"1"},
        {"property":"input","op":"gt","value":"1,5"},
        {"property":"metal","op":"in","value":[]},
    ]));
    let refused = query(
        &host,
        "full",
        &params(
            Some("0"),
            Some("input:up,ghost:asc,metal:asc,input:desc"),
            Some(&bad),
        ),
    )
    .unwrap_err();
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (400, "query.invalid")
    );
    let found: Vec<(String, String)> = refused
        .diagnostics
        .iter()
        .map(|d| (d.code.clone(), d.span.as_ref().unwrap().pointer.clone()))
        .collect();
    let expected = [
        ("query.limit_invalid", "/limit"),
        ("query.too_many", "/sort"),
        ("query.sort_invalid", "/sort/0"),
        ("query.property_unknown", "/sort/1"),
        ("query.property_unknown", "/filter/all/0/property"),
        ("query.op_invalid", "/filter/all/1/op"),
        ("query.value_invalid", "/filter/all/2/value"),
        ("query.value_invalid", "/filter/all/3/value"),
    ];
    for (code, pointer) in expected {
        assert!(
            found.contains(&(code.to_owned(), pointer.to_owned())),
            "{code} at {pointer}: {found:?}"
        );
    }
    for limit in ["201", "-1", "1.0", ""] {
        let refused = query(&host, "full", &params(Some(limit), None, None)).unwrap_err();
        assert_eq!(
            refused.diagnostics[0].code, "query.limit_invalid",
            "{limit}"
        );
    }
    assert!(query(&host, "full", &params(Some("200"), None, None)).is_ok());
}

/// A cursor continues only its own query, for its own caller, at its own
/// snapshot: anything else is 400 at `/after`, and a commit since is 409.
#[test]
fn a_cursor_binds_its_query_caller_and_snapshot() {
    let (host, dir) = host_of(writes([(Some("512"), Some("gold")), (Some("8"), None)]));
    let sorted = params(Some("2"), Some("input:desc"), None);
    let first = query(&host, "full", &sorted).unwrap();
    let next = first.next.unwrap();
    let with = |base: &ObjectQueryParams, after: &str| ObjectQueryParams {
        after: Some(after.to_owned()),
        ..base.clone()
    };
    let invalid = |refused: Refusal| {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.diagnostics[0].code, "query.cursor_invalid");
        assert_eq!(
            refused.diagnostics[0].span.as_ref().unwrap().pointer,
            "/after"
        );
    };
    invalid(query(&host, "pop", &with(&sorted, &next)).unwrap_err());
    invalid(
        query(
            &host,
            "full",
            &with(&params(Some("2"), Some("input:asc"), None), &next),
        )
        .unwrap_err(),
    );
    invalid(query(&host, "full", &with(&sorted, "not-a-cursor")).unwrap_err());
    invalid(query(&host, "full", &with(&sorted, &format!("{next}x"))).unwrap_err());
    // The page size may change between pages; the query may not.
    assert!(
        query(
            &host,
            "full",
            &with(&params(Some("5"), Some("input:desc"), None), &next)
        )
        .is_ok()
    );
    let plain = query(&host, "full", &params(Some("2"), None, None)).unwrap();
    let plain_next = plain.next.unwrap();
    let object = nullable_type();
    let model = norn_cell::Model::from_definitions(std::slice::from_ref(&object), &[]).unwrap();
    let mut store = CellStore::open(
        &dir.path().join("cell.sqlite"),
        model,
        norn_cell::StoreOptions::default(),
    )
    .unwrap();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: physical().key(),
            logical_time: CLOCK + 1_000,
            writes: vec![row("R-09", Some("1"), Some("gold"))],
            ..CommitBatch::default()
        })
        .unwrap();
    drop(store);
    for (base, after) in [
        (&sorted, &next),
        (&params(Some("2"), None, None), &plain_next),
    ] {
        let moved = query(&host, "full", &with(base, after)).unwrap_err();
        assert_eq!(
            (moved.status, moved.code.as_str(), moved.retryable),
            (409, "object.snapshot_moved", true)
        );
    }
    assert_eq!(ids(&pages(&host, "full", &sorted)).len(), 9);
}

/// Pages of a plain listing stream by id and together list every row once.
#[test]
fn plain_pages_stream_every_row_once() {
    let (host, _dir) = host_of(writes([(Some("512"), Some("gold")), (Some("8"), None)]));
    for limit in ["1", "3", "8", "50"] {
        let all = pages(&host, "pop", &params(Some(limit), None, None));
        assert_eq!(
            ids(&all),
            [
                "R-01", "R-02", "R-03", "R-04", "R-05", "R-06", "R-07", "R-08"
            ],
            "{limit}"
        );
        let total = all[0].total.as_ref().unwrap();
        assert_eq!((total.count, total.approximate), (8, false), "{limit}");
        assert!(
            all.iter().skip(1).all(|page| page.total.is_none()),
            "{limit}"
        );
    }
}

/// An undeclared type is 404 and an anonymous caller the shared denial.
#[test]
fn the_collection_gate_comes_first() {
    let (host, _dir) = host_of(writes([(None, None), (None, None)]));
    let anonymous = get_object_query(
        &host,
        &scope(),
        None,
        TYPE,
        &params(None, None, None),
        &Mapping,
    )
    .unwrap_err();
    assert_eq!(anonymous.status, 403);
    let undeclared = get_object_query(
        &host,
        &scope(),
        Some("full"),
        "demo.Ghost",
        &params(None, None, None),
        &Mapping,
    )
    .unwrap_err();
    assert_eq!(undeclared.status, 404);
}

/// The scan bound counts only rows the caller may see: rows hidden from it
/// never turn its sorted query into a 413, which would reveal that they exist.
#[test]
fn rows_hidden_from_the_caller_never_count_toward_the_scan_bound() {
    let hidden = norn_api::object_routes::MAX_QUERY_SCAN + 50;
    let mut rows = writes([(Some("512"), Some("gold")), (Some("8"), None)]);
    rows.extend((0..hidden).map(|index| row(&format!("H-{index:05}"), Some("1"), Some("gold"))));
    let (host, _dir) = host_of(rows);
    let sorted = params(None, Some("input:desc"), None);
    let page = query(&host, "few", &sorted).unwrap();
    assert_eq!(page.records.len(), 8);
    let total = page.total.unwrap();
    assert_eq!((total.count, total.approximate), (8, false));
    let refused = query(&host, "full", &sorted).unwrap_err();
    assert_eq!((refused.status, refused.code.as_str()), (413, "object.scan_bound_exceeded"));
}
