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

    // A malformed or oversized page refuses before anything is captured.
    let (status, _) = load(&mut connect, &host, &principals, ADMIN, "{}");
    assert_eq!(status, 400);
    let mut big = export.clone();
    big["snapshot"] = Value::Array(vec![export["snapshot"][0].clone(); 501]);
    let (status, _) = load(&mut connect, &host, &principals, ADMIN, &big.to_string());
    assert_eq!(status, 400);
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
    // One full page: 500 order lines.
    for n in 1..=(500 - snapshot.len()) {
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
        path: "/v1/objects/canon.OrderLine/demo%2Forder%2F00123%2Fline%2F1/lineage",
        query: SCOPE,
        authorization: ADMIN,
        now: 0,
        body: "",
    };
    let response = server::handle(&host, &principals, &request);
    assert_eq!(response.status, 200, "{}", response.body);
    let lineage: Value = serde_json::from_str(&response.body).unwrap();
    assert!(
        lineage.to_string().contains("SYN-ERPNEXT-order-00123"),
        "{lineage}"
    );
}
