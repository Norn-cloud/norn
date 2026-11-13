//! `POST /v1/observations:load`: a page of a recorded export lands through
//! the admitted mappings, once, for a caller the policy lets sync sources.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "route correspondence"
)]
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::connect::Connect;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::{seed, server};
use serde_json::{Value, json};

const ADMIN: Option<&str> = Some("Bearer fixture:admin@demo");
const GUEST: Option<&str> = Some("Bearer fixture:guest@demo");
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

fn connect() -> Connect {
    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test",
        "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id",
        "NORN_SECRET_S3_SECRET_ACCESS_KEY": "secret",
    });
    Connect::new("[]", &vars.to_string()).expect("configuration")
}

fn load(
    connect: &mut Connect,
    host: &DevHost<NativeEngine>,
    principals: &FixturePrincipals,
    authorization: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let request = server::Request {
        method: "POST",
        path: "/v1/observations:load",
        query: SCOPE,
        authorization,
        now: 0,
        body,
    };
    let routed = connect
        .route(Some(host), principals, &request, 0)
        .expect("a connect route");
    (
        routed.response.status,
        serde_json::from_str(&routed.response.body).unwrap_or(Value::Null),
    )
}

#[test]
fn a_recorded_page_lands_once_through_the_admitted_mapping() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let mut connect = connect();
    let export: Value = serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap();
    let body = json!({ "profile": export["profile"], "snapshot": export["snapshot"] }).to_string();
    let rows = export["snapshot"].as_array().unwrap().len();

    // A caller the policy does not let sync sources is answered as outside.
    let (status, _) = load(&mut connect, &host, &principals, GUEST, &body);
    assert_ne!(status, 200);

    let (status, first) = load(&mut connect, &host, &principals, ADMIN, &body);
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["rows"], rows);
    assert_eq!(first["landed"], rows, "{first}");
    assert_eq!(first["quarantined"], 0, "{first}");

    // Loading the same export again captures nothing new.
    let (status, again) = load(&mut connect, &host, &principals, ADMIN, &body);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["alreadyLanded"], rows, "{again}");
    assert_eq!(again["landed"], 0, "{again}");

    // A malformed or oversized page, or one for a mapping this cell has not
    // admitted, refuses before anything is captured.
    let (status, _) = load(&mut connect, &host, &principals, ADMIN, "{}");
    assert_eq!(status, 400);
    let unknown = json!({ "profile": "synthetic-nowhere-three/1", "snapshot": export["snapshot"] });
    let (status, refused) = load(
        &mut connect,
        &host,
        &principals,
        ADMIN,
        &unknown.to_string(),
    );
    assert_eq!(status, 400);
    assert!(
        refused.to_string().contains("nowhere.three.mapping"),
        "{refused}"
    );
    // More rows, or more page commits, than one load may make: 413 with a
    // typed code, before anything is captured.
    let mut big = export.clone();
    big["snapshot"] = Value::Array(
        (0..101)
            .map(|n| {
                let mut row = export["snapshot"][0].clone();
                row["record"]["name"] = json!(format!("SYN-ERPNEXT-big-{n}"));
                row
            })
            .collect(),
    );
    let (status, refused) = load(&mut connect, &host, &principals, ADMIN, &big.to_string());
    assert_eq!(status, 413, "{refused}");
    assert!(
        refused.to_string().contains("observations.too_large"),
        "{refused}"
    );
    let mut history = export.clone();
    history["snapshot"] = Value::Array(
        (0..9)
            .map(|n| {
                let mut row = export["snapshot"][0].clone();
                row["sourceVersion"] = json!(format!("v{n}"));
                row
            })
            .collect(),
    );
    let (status, refused) = load(
        &mut connect,
        &host,
        &principals,
        ADMIN,
        &history.to_string(),
    );
    assert_eq!(status, 413, "{refused}");
}

/// A large page's objects keep their per-record lineage through the route.
#[test]
fn objects_of_a_large_page_trace_to_their_own_source_row() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let mut connect = connect();
    let export: Value = serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap();
    let rows = export["snapshot"].as_array().unwrap();
    let mut snapshot: Vec<Value> = rows
        .iter()
        .filter(|row| row["dataset"] != "Sales Order Item")
        .cloned()
        .collect();
    let order = rows
        .iter()
        .find(|row| row["dataset"] == "Sales Order Item")
        .unwrap();
    // One full load: 100 rows, the order lines in one page.
    for n in 1..=(100 - snapshot.len()) {
        let mut line = order.clone();
        line["record"]["name"] = json!(format!("SYN-ERPNEXT-order-{n:05}"));
        line["record"]["custom_norn_ref"] = json!(format!("demo/order/{n:05}/line/1"));
        snapshot.push(line);
    }
    let body = json!({ "profile": export["profile"], "snapshot": snapshot }).to_string();
    let (status, loaded) = load(&mut connect, &host, &principals, ADMIN, &body);
    assert_eq!(status, 200, "{loaded}");
    assert_eq!(loaded["quarantined"], 0, "{loaded}");

    let request = server::Request {
        method: "GET",
        path: "/v1/objects/canon.OrderLine/demo%2Forder%2F00042%2Fline%2F1/lineage",
        query: SCOPE,
        authorization: ADMIN,
        now: 0,
        body: "",
    };
    let response = server::handle(&host, &principals, &request);
    assert_eq!(response.status, 200, "{}", response.body);
    let lineage: Value = serde_json::from_str(&response.body).unwrap();
    assert!(
        lineage.to_string().contains("SYN-ERPNEXT-order-00042"),
        "{lineage}"
    );
}

/// A member's service key may ingest recorded rows under the admitted
/// mappings (a source writer), but holds no right to change definitions or
/// to use the uploads that install them.
#[test]
fn a_member_service_key_loads_but_cannot_install_definitions() {
    const PRINCIPALS: &[(&str, &[&str])] = &[("loader@demo", &["member"])];
    const LOADER: Option<&str> = Some("Bearer fixture:loader@demo");
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(PRINCIPALS, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let mut connect = connect();
    let export: Value = serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap();
    let body = json!({ "profile": export["profile"], "snapshot": export["snapshot"] }).to_string();
    let (status, loaded) = load(&mut connect, &host, &principals, LOADER, &body);
    assert_eq!(status, 200, "{loaded}");
    assert_eq!(
        loaded["landed"],
        export["snapshot"].as_array().unwrap().len(),
        "{loaded}"
    );

    let install = server::Request {
        method: "POST",
        path: "/v1/definitions:install",
        query: SCOPE,
        authorization: LOADER,
        now: 0,
        body: "{}",
    };
    let refused = server::handle(&host, &principals, &install);
    assert!(
        matches!(refused.status, 403 | 404),
        "{} {}",
        refused.status,
        refused.body
    );
    let begin = server::Request {
        method: "POST",
        path: "/v1/uploads:begin",
        query: SCOPE,
        authorization: LOADER,
        now: 0,
        body: r#"{"fileName":"rows.csv","contentType":"text/csv","size":10}"#,
    };
    let routed = connect
        .route(Some(&host), &principals, &begin, 0)
        .expect("a connect route");
    assert_ne!(routed.response.status, 200, "{}", routed.response.body);
}
