//! Native correspondence for the Durable Object read dispatch: the same
//! `server::handle` over `NativeEngine` that the DO runs over DO SQLite.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "dispatch correspondence"
)]
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::{seed, server};
use serde_json::Value;

const VIEWER: Option<&str> = Some("Bearer fixture:viewer@demo");
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

/// The demo cell with its fixture principals recording into its policy.
struct Demo {
    host: DevHost<NativeEngine>,
    principals: FixturePrincipals,
}

fn host(engine: NativeEngine) -> Demo {
    let scope = seed::demo_scope();
    let store = seed::provision(engine, &scope).expect("demo cell provisions");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    Demo { host, principals }
}

fn call(
    demo: &Demo,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
) -> server::Response {
    let request = server::Request {
        method,
        path,
        query,
        authorization,
        now: 0,
        body: "",
    };
    server::handle(&demo.host, &demo.principals, &request)
}

fn get(host: &Demo, path: &str, query: &str, auth: Option<&str>) -> (u16, Value) {
    let response = call(host, "GET", path, query, auth);
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
}

#[test]
fn viewer_reads_the_admitted_demo_definitions() {
    let host = host(NativeEngine::open_in_memory().unwrap());
    let raw = call(&host, "GET", "/v1/definitions", SCOPE, VIEWER);
    // The DO smoke compares its body bytes with this native body (parity).
    if let Some(out) = std::env::var_os("NORN_CELL_DO_PARITY_OUT") {
        std::fs::write(out, &raw.body).unwrap();
    }
    let (status, document) = (
        raw.status,
        serde_json::from_str::<Value>(&raw.body).unwrap(),
    );
    assert_eq!(status, 200, "{document}");
    assert_eq!(document["admitted"], true);
    assert_eq!(document["version"], 1);
    let retained: Vec<Value> = serde_json::from_str(include_str!(
        "../../../fixtures/demo/explorer/three-systems.definitions.json"
    ))
    .unwrap();
    assert_eq!(document["artifacts"].as_array().unwrap(), &retained);
    let receipt = serde_json::to_string(&document["receipt"]).unwrap();
    assert_eq!(
        norn_checker::admission_receipt_sha256(&receipt).unwrap(),
        document["receiptSha256"].as_str().unwrap()
    );
}

#[test]
fn anonymous_foreign_and_unknown_principals_get_one_denial() {
    let host = host(NativeEngine::open_in_memory().unwrap());
    let denied = call(&host, "GET", "/v1/definitions", SCOPE, None);
    assert_eq!(denied.status, 403);
    for (query, auth) in [
        (SCOPE, Some("Bearer fixture:admin@demo")),
        (SCOPE, Some("Bearer viewer@demo")),
        ("orgId=foreign&environmentId=dev", VIEWER),
        ("orgId=org_demo", VIEWER),
    ] {
        let response = call(&host, "GET", "/v1/definitions", query, auth);
        assert_eq!(
            (response.status, &response.body),
            (403, &denied.body),
            "{query} {auth:?}"
        );
    }
    assert!(!denied.body.contains("org_demo"));
}

#[test]
fn malformed_requests_and_other_routes_refuse_without_reading() {
    let host = host(NativeEngine::open_in_memory().unwrap());
    for query in [
        "orgId=org_demo&environmentId=dev&sql=select",
        "orgId=org_demo&orgId=org_demo&environmentId=dev",
        "orgId=org_demo&environmentId=dev&version=-1",
    ] {
        let (status, refusal) = get(&host, "/v1/definitions", query, VIEWER);
        assert_eq!(
            (status, refusal["code"].as_str()),
            (400, Some("request.invalid")),
            "{query}"
        );
    }
    let long = format!("{SCOPE}&{}", "x".repeat(server::MAX_QUERY_BYTES));
    assert_eq!(get(&host, "/v1/definitions", &long, VIEWER).0, 400);
    assert_eq!(
        get(
            &host,
            "/v1/definitions",
            &format!("{SCOPE}&version=9"),
            VIEWER
        )
        .0,
        404
    );
    assert_eq!(get(&host, "/v1/tables", SCOPE, VIEWER).0, 404);
    let post = call(&host, "POST", "/v1/definitions", SCOPE, VIEWER);
    assert_eq!(post.status, 405);
}

#[test]
fn reprovisioning_a_restarted_cell_reopens_the_same_revision() {
    let directory = std::env::temp_dir().join(format!("norn-cell-do-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("cell.sqlite");
    let open = || NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
    let scope = seed::demo_scope();
    let first = seed::provision(open(), &scope)
        .unwrap()
        .verified_head()
        .unwrap();
    let document = get(&host(open()), "/v1/definitions", SCOPE, VIEWER).1;
    let reopened = seed::provision(open(), &scope)
        .unwrap()
        .verified_head()
        .unwrap();
    assert_eq!(reopened, first, "an identical provision writes nothing");
    assert_eq!(
        get(&host(open()), "/v1/definitions", SCOPE, VIEWER).1,
        document
    );
    std::fs::remove_dir_all(&directory).unwrap();
}
