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
    // Exclusive to this run and removed on drop, including after a failure.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
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
}

/// The demo rows are listed for a Population-only caller with their values
/// withheld (D67), and read whole by a caller holding Values; both see the
/// same ids and total.
#[test]
fn demo_rows_are_withheld_for_a_viewer_and_read_by_an_admin() {
    let demo = host(NativeEngine::open_in_memory().unwrap());
    let guest = Some("Bearer fixture:guest@demo");
    let list = |auth| get(&demo, "/v1/objects/canon.Party", SCOPE, auth);
    let (status, viewer) = list(guest);
    assert_eq!(status, 200, "{viewer}");
    let (status, admin) = list(VIEWER);
    assert_eq!(status, 200, "{admin}");
    let ids = |page: &Value| -> Vec<String> {
        page["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let expected: Vec<String> = seed::SEED_PARTIES
        .iter()
        .map(|(id, _, _)| (*id).to_owned())
        .collect();
    assert_eq!(ids(&viewer), expected);
    assert_eq!(ids(&admin), expected);
    assert_eq!(viewer["total"], admin["total"]);
    for record in viewer["records"].as_array().unwrap() {
        assert_eq!(record["withheld"], Value::Bool(true));
        assert!(record["values"].is_null() && record["version"].is_null());
    }
    for record in admin["records"].as_array().unwrap() {
        assert_eq!(record["withheld"], Value::Bool(false));
        assert!(record["values"]["display_name"].is_string());
    }
    // A filter never matches a withheld row; the admin's narrows.
    let filtered = |auth| {
        get(
            &demo,
            "/v1/objects/canon.Party",
            &format!(
                "{SCOPE}&filter=%7B%22all%22%3A%5B%7B%22property%22%3A%22kind%22%2C%22op%22%3A%22eq%22%2C%22value%22%3A%22organization%22%7D%5D%7D"
            ),
            auth,
        )
    };
    assert_eq!(filtered(guest).1["total"]["count"], 0);
    assert_eq!(filtered(VIEWER).1["total"]["count"], 2);
    let (status, items) = get(
        &demo,
        "/v1/objects/canon.Item",
        &format!("{SCOPE}&sort=label:desc&limit=2"),
        VIEWER,
    );
    assert_eq!(status, 200, "{items}");
    assert_eq!(items["records"][0]["id"], "SYN-ITEM-05");
    assert!(items["next"].is_string());
}

/// A restarted cell replays the demo rows' commit: nothing is written twice
/// and nothing is wiped.
#[test]
fn a_restarted_demo_cell_replays_its_rows_without_writing() {
    let directory = std::env::temp_dir().join(format!("norn-cell-do-seed-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("cell.sqlite");
    let open = || NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
    let first = seed::provision(open(), &seed::demo_scope()).unwrap();
    let head = first.verified_head().unwrap().commits;
    drop(first);
    let again = seed::provision(open(), &seed::demo_scope()).unwrap();
    assert_eq!(again.verified_head().unwrap().commits, head);
    drop(again);
    std::fs::remove_dir_all(&directory).unwrap();
}

/// Object path segments are percent-decoded only: a raw `&` is part of the
/// segment, never a query separator that truncates it.
#[test]
fn object_path_segments_keep_a_raw_ampersand() {
    let demo = host(NativeEngine::open_in_memory().unwrap());
    let status = |path| get(&demo, path, SCOPE, VIEWER).0;
    assert_eq!(status("/v1/objects/canon.Party"), 200);
    assert_eq!(status("/v1/objects/canon%2EParty"), 200);
    assert_ne!(status("/v1/objects/canon.Party&x=1"), 200);
    assert_eq!(status("/v1/objects/canon.Party/SYN-PARTY-01"), 200);
    assert_eq!(status("/v1/objects/canon.Party/SYN%2DPARTY-01"), 200);
    assert_eq!(status("/v1/objects/canon.Party/SYN-PARTY-01&x"), 404);
}

/// The dispatch serves an object's links and lineage, and refuses any other
/// leaf as an unknown route.
#[test]
fn object_links_and_lineage_routes_dispatch() {
    let demo = host(NativeEngine::open_in_memory().unwrap());
    let (status, page) = get(
        &demo,
        "/v1/objects/canon.Party/SYN-PARTY-01/links",
        SCOPE,
        VIEWER,
    );
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["id"], "SYN-PARTY-01");
    assert!(page["items"].as_array().unwrap().is_empty());
    assert!(page["next"].is_null());
    // The demo rows were seeded, not mapped from a source: no lineage to show.
    let (status, lineage) = get(
        &demo,
        "/v1/objects/canon.Party/SYN-PARTY-01/lineage",
        SCOPE,
        VIEWER,
    );
    assert_eq!(
        (status, lineage["code"].as_str()),
        (404, Some("object.not_found"))
    );
    let (status, other) = get(
        &demo,
        "/v1/objects/canon.Party/SYN-PARTY-01/history",
        SCOPE,
        VIEWER,
    );
    assert_eq!(
        (status, other["code"].as_str()),
        (404, Some("route.not_found"))
    );
    let (status, _) = get(
        &demo,
        "/v1/objects/canon.Party/SYN-PARTY-01/links/x",
        SCOPE,
        VIEWER,
    );
    assert_eq!(status, 404);
}

/// A path segment that is not UTF-8 once decoded names nothing: it refuses
/// rather than alias another id through the replacement character.
#[test]
fn a_non_utf8_object_path_segment_refuses() {
    let demo = host(NativeEngine::open_in_memory().unwrap());
    for path in [
        "/v1/objects/canon.Party/%FF",
        "/v1/objects/canon.Party/%FE",
        "/v1/objects/canon%FF",
    ] {
        let (status, body) = get(&demo, path, SCOPE, VIEWER);
        assert_eq!(status, 400, "{path}: {body}");
    }
}

/// A cursor up to the host's cursor bound is never refused for the query
/// string's length: it reaches the route, which judges the cursor itself.
#[test]
fn a_cursor_at_the_cursor_bound_reaches_the_route() {
    let demo = host(NativeEngine::open_in_memory().unwrap());
    let after = "A".repeat(norn_api::object_query::MAX_CURSOR_BYTES);
    let (status, body) = get(
        &demo,
        "/v1/objects/canon.Party",
        &format!("{SCOPE}&limit=2&after={after}"),
        VIEWER,
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["diagnostics"][0]["code"], "query.cursor_invalid");
}

/// Served routes the `OpenAPI` document does not describe, each with its reason.
const UNDOCUMENTED: &[server::Route] = &[
    // Served only by the published host's entry point, for the access view:
    // the dev host and `server::handle` refuse it. Its document types live in
    // this crate, outside the generated contract.
    server::Route::Access,
];

/// The generated document describes exactly what the host serves: every
/// route is documented or named above, and the document names no route the
/// host does not serve.
#[test]
fn every_served_route_is_documented_or_named_as_undocumented() {
    let document: Value =
        serde_json::from_str(&norn_api::openapi::openapi_json().unwrap()).unwrap();
    let documented: std::collections::BTreeSet<(String, String)> = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .unwrap()
                .iter()
                .filter(|(_, operation)| operation.get("operationId").is_some())
                .map(move |(method, _)| (method.to_uppercase(), path.clone()))
        })
        .collect();
    let served: std::collections::BTreeSet<(String, String)> = server::Route::all()
        .filter(|route| !UNDOCUMENTED.contains(route))
        .map(|route| {
            let (method, template) = route.signature();
            (method.to_owned(), template.to_owned())
        })
        .collect();
    assert_eq!(documented, served);

    // Each template names its own route, so the list and the router agree.
    for route in server::Route::all() {
        let (_, template) = route.signature();
        let mut path = String::new();
        let mut parameter = false;
        for character in template.chars() {
            match character {
                '{' => {
                    parameter = true;
                    path.push('x');
                }
                '}' => parameter = false,
                _ if parameter => {}
                other => path.push(other),
            }
        }
        assert_eq!(server::Route::of(&path), Some(route), "{template}");
    }

    // A path the list does not name is not served, and neither is the one
    // route this dispatch leaves to the published host's entry point.
    let demo = host(NativeEngine::open_in_memory().unwrap());
    for path in ["/v1/nothing", "/v1/access", "/v1/objects"] {
        let (status, body) = get(&demo, path, SCOPE, VIEWER);
        assert_eq!(
            (status, body["code"].as_str()),
            (404, Some("route.not_found")),
            "{path}"
        );
    }
}
