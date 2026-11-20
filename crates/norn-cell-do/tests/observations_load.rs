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
    // More rows, or more work, than one hosted load may make: 413 with a
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
    // INGEST-BATCH-1: a load is a work budget, not a bare commit count. One
    // record's nine versions are nine commits, within the budget (it was
    // refused past 8 commits before); 21 versions are past it.
    let history = |versions: usize| {
        let mut history = json!({ "profile": export["profile"] });
        history["snapshot"] = Value::Array(
            (0..versions)
                .map(|n| {
                    let mut row = export["snapshot"][0].clone();
                    row["record"]["name"] = json!("SYN-ERPNEXT-history");
                    row["sourceVersion"] = json!(format!("v{n:02}"));
                    row
                })
                .collect(),
        );
        history.to_string()
    };
    let (status, loaded) = load(&mut connect, &host, &principals, ADMIN, &history(9));
    assert_eq!(status, 200, "{loaded}");
    assert_eq!(loaded["pages"], 9, "{loaded}");
    let (status, refused) = load(&mut connect, &host, &principals, ADMIN, &history(21));
    assert_eq!(status, 413, "{refused}");
    assert!(
        refused.to_string().contains("21 rows, 21 commits"),
        "{refused}"
    );
}

/// MAP-EXPLAIN-1 and MAP-DECIMAL-TEXT-1 through the route: a decimal sent as
/// exact text lands (it was quarantined as `mapping.runtime.invalid_record`
/// with only the code), and a row that still does not conform is explained:
/// its record, dataset, mapping rule, field, value and declared type.
#[test]
fn a_quarantined_row_is_explained_and_decimal_text_lands() {
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
    let holding = export["snapshot"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["dataset"] == "Warehouse Lot Balance")
        .unwrap()
        .clone();
    let rows = |quantity: Value, name: &str| {
        let mut row = holding.clone();
        row["record"]["quantity"] = quantity;
        row["record"]["name"] = json!(name);
        // Everything but this holding is the export as committed.
        let mut snapshot: Vec<Value> = export["snapshot"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|other| other["dataset"] != "Warehouse Lot Balance")
            .cloned()
            .collect();
        snapshot.push(row);
        json!({ "profile": export["profile"], "snapshot": snapshot }).to_string()
    };
    let (status, loaded) = load(
        &mut connect,
        &host,
        &principals,
        ADMIN,
        &rows(json!("200.50"), "SYN-ERPNEXT-text-decimal"),
    );
    assert_eq!(status, 200, "{loaded}");
    assert_eq!(loaded["quarantined"], 0, "{loaded}");

    let (status, loaded) = load(
        &mut connect,
        &host,
        &principals,
        ADMIN,
        &rows(json!("1,234"), "SYN-ERPNEXT-comma"),
    );
    assert_eq!(status, 200, "{loaded}");
    assert_eq!(loaded["quarantined"], 1, "{loaded}");
    let why = &loaded["quarantine"][0];
    assert_eq!(why["record"], "SYN-ERPNEXT-comma", "{why}");
    assert_eq!(why["dataset"], "Warehouse Lot Balance", "{why}");
    assert_eq!(why["code"], "mapping.runtime.invalid_record", "{why}");
    assert!(
        why["map"].as_str().is_some_and(|map| !map.is_empty()),
        "{why}"
    );
    assert_eq!(why["field"], "quantity", "{why}");
    assert_eq!(why["value"], "\"1,234\"", "{why}");
    assert!(
        why["expected"].as_str().unwrap().starts_with("decimal"),
        "{why}"
    );
    assert!(
        why["reason"].as_str().unwrap().contains("JSON decimal"),
        "{why}"
    );
    assert_eq!(
        loaded["quarantineReasons"]["mapping.runtime.invalid_record"],
        1
    );
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

/// INGEST-YIELD-1: a read during a long load answers between the load's
/// pages, not after the whole load. The load is one record's 120 versions
/// (120 commits); before, the load held the cell for all of them and every
/// read waited for its end.
#[test]
fn a_read_during_a_load_answers_before_the_load_ends() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let export: Value = serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap();
    let mut history = json!({ "profile": export["profile"] });
    history["snapshot"] = Value::Array(
        (0..120)
            .map(|n| {
                let mut row = export["snapshot"][0].clone();
                row["sourceVersion"] = json!(format!("v{n:03}"));
                row
            })
            .collect(),
    );
    let recorded = norn_cell_do::recorded::Recorded::parse(&history).unwrap();
    let rows = recorded.observations().unwrap();
    let loading = std::sync::atomic::AtomicBool::new(true);
    let started = std::sync::Barrier::new(2);
    let (load_ms, reads) = std::thread::scope(|threads| {
        let load = threads.spawn(|| {
            started.wait();
            let at = std::time::Instant::now();
            let ingested = host.ingest(&rows).unwrap();
            loading.store(false, std::sync::atomic::Ordering::Release);
            assert_eq!(ingested.len(), 120);
            at.elapsed().as_millis()
        });
        started.wait();
        let mut reads = Vec::new();
        while loading.load(std::sync::atomic::Ordering::Acquire) {
            let at = std::time::Instant::now();
            let response = server::handle(
                &host,
                &principals,
                &server::Request {
                    method: "GET",
                    path: "/v1/objects/canon.Party",
                    query: SCOPE,
                    authorization: ADMIN,
                    now: 0,
                    body: "",
                },
            );
            assert_eq!(response.status, 200, "{}", response.body);
            reads.push((
                at.elapsed().as_millis(),
                loading.load(std::sync::atomic::Ordering::Acquire),
            ));
        }
        (load.join().unwrap(), reads)
    });
    let during: Vec<u128> = reads
        .iter()
        .filter(|(_, still_loading)| *still_loading)
        .map(|(ms, _)| *ms)
        .collect();
    assert!(
        during.len() >= 2,
        "reads answered while the {load_ms} ms load ran: {reads:?}"
    );
    let slowest = during.iter().max().copied().unwrap_or_default();
    assert!(
        slowest < load_ms / 2,
        "a read waited {slowest} ms of a {load_ms} ms load: {reads:?}"
    );
}
