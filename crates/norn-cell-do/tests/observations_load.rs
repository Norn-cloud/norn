//! `POST /v1/observations:load`: a page of a recorded export lands through
//! the admitted mappings, once, for a caller the policy lets sync sources.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
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
        "NORN_S3_ACCESS_KEY_ID": "key-id",
        "NORN_S3_SECRET_ACCESS_KEY": "secret",
    });
    Connect::new(&seed::demo_scope(), "[]", &vars.to_string())
        .unwrap_or_else(|error| unreachable!("the configuration is well formed: {error}"))
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
    // INGEST-BATCH-1: a load is a work budget charged for the commits the
    // cell makes. One record's nine versions are nine commits, within it; 21
    // versions stop at the budget and answer where to resume, and resuming
    // lands the rest, every row reported once.
    let history = |versions: std::ops::Range<usize>| {
        let mut history = json!({ "profile": export["profile"] });
        history["snapshot"] = Value::Array(
            versions
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
    let (status, loaded) = load(&mut connect, &host, &principals, ADMIN, &history(0..9));
    assert_eq!(status, 200, "{loaded}");
    assert_eq!(loaded["pages"], 9, "{loaded}");
    assert_eq!(loaded["next"], Value::Null, "{loaded}");
    let (status, partial) = load(&mut connect, &host, &principals, ADMIN, &history(9..30));
    assert_eq!(status, 200, "{partial}");
    let next = usize::try_from(partial["next"].as_u64().expect("stopped at the budget")).unwrap();
    assert!(next > 0 && next < 21, "{partial}");
    assert_eq!(partial["rows"], next, "{partial}");
    let (status, rest) = load(
        &mut connect,
        &host,
        &principals,
        ADMIN,
        &history(9 + next..30),
    );
    assert_eq!(status, 200, "{rest}");
    assert_eq!(rest["next"], Value::Null, "{rest}");
    assert_eq!(rest["rows"], 21 - next, "{rest}");
    assert_eq!(rest["landed"], 21 - next, "{rest}");
}

/// The value of the first `key` anywhere in `value`.
fn find<'v>(value: &'v Value, key: &str) -> Option<&'v Value> {
    match value {
        Value::Object(members) => members
            .get(key)
            .or_else(|| members.values().find_map(|member| find(member, key))),
        Value::Array(items) => items.iter().find_map(|item| find(item, key)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn host() -> (DevHost<NativeEngine>, FixturePrincipals) {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    (host, principals)
}

fn export() -> Value {
    serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap()
}

/// `n` Item rows under distinct source ids that all key the same canonical
/// item, labelled `L0`, `L1`, ...
fn same_item(n: usize) -> Vec<Value> {
    let export = export();
    let item = export["snapshot"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["dataset"] == "Item")
        .unwrap()
        .clone();
    (0..n)
        .map(|k| {
            let mut row = item.clone();
            row["record"]["name"] = json!(format!("SYN-ERPNEXT-item-{k:03}"));
            row["record"]["label"] = json!(format!("L{k}"));
            row
        })
        .collect()
}

fn item_label(host: &DevHost<NativeEngine>, principals: &FixturePrincipals) -> Value {
    let response = server::handle(
        host,
        principals,
        &server::Request {
            method: "GET",
            path: "/v1/objects/canon.Item/demo%2Fitem%2Fgold",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let object: Value = serde_json::from_str(&response.body).unwrap();
    find(&object, "label").cloned().unwrap_or(Value::Null)
}

/// S2 (INGEST-GROUP-1): rows A, B, A' of two inputs (two partitions of one
/// dataset) with distinct source ids write the same canonical property. The
/// load ends with A's second value, exactly as delivering them one by one
/// does; grouping by input would have applied A, A', B and left B.
#[test]
fn overlapping_inputs_end_as_one_by_one_delivery() {
    let mut snapshot = same_item(3);
    snapshot[0]["record"]["label"] = json!("A1");
    snapshot[1]["record"]["label"] = json!("B");
    snapshot[2]["record"]["label"] = json!("A2");
    let recorded = norn_cell_do::recorded::Recorded::parse(
        &json!({ "profile": export()["profile"], "snapshot": snapshot }),
    )
    .unwrap();
    let mut rows = recorded.observations().unwrap();
    rows[0].1.partition = "first".into();
    rows[1].1.partition = "second".into();
    rows[2].1.partition = "first".into();

    let (batched, principals) = host();
    let outcomes = batched.ingest(&rows).unwrap();
    assert!(
        outcomes
            .iter()
            .all(|o| o.outcome == norn_cell::MappingOutcome::Landed),
        "{outcomes:?}"
    );
    let (sequential, readers) = host();
    for row in &rows {
        sequential.ingest(std::slice::from_ref(row)).unwrap();
    }
    assert_eq!(item_label(&sequential, &readers), json!("A2"));
    assert_eq!(item_label(&batched, &principals), json!("A2"));
}

/// S3 (INGEST-BATCH-1): a hosted load of 100 distinct source ids the cell
/// splits into 100 commits (every row keys the same item) is charged for
/// those commits: it stops within the budget and answers where to resume,
/// and the resumed loads land the rest with no row lost or reported twice.
#[test]
fn a_load_the_cell_splits_is_charged_per_commit_and_resumes() {
    let (host, principals) = host();
    let mut connect = connect();
    let snapshot = same_item(100);
    let mut from = 0_usize;
    let (mut reported, mut landed, mut calls) = (0_u64, 0_u64, 0);
    while from < snapshot.len() {
        let body =
            json!({ "profile": export()["profile"], "snapshot": snapshot[from..] }).to_string();
        let (status, loaded) = load(&mut connect, &host, &principals, ADMIN, &body);
        assert_eq!(status, 200, "{loaded}");
        // The connect budget (16.4 s: 0.1 s a row, 0.8 s a commit) bounds each
        // call's commits by the rows it captured: 100 rows leave eight.
        let rows = u64::try_from(snapshot.len() - from).unwrap();
        let fits = (16_400 - 100 * rows) / 800;
        assert!(loaded["commits"].as_u64().unwrap() <= fits, "{loaded}");
        reported += loaded["rows"].as_u64().unwrap();
        landed += loaded["landed"].as_u64().unwrap() + loaded["alreadyLanded"].as_u64().unwrap();
        calls += 1;
        assert!(calls < 30, "a load always makes progress: {loaded}");
        match loaded["next"].as_u64() {
            Some(next) => from += usize::try_from(next).unwrap(),
            None => from = snapshot.len(),
        }
    }
    assert!(calls > 1, "the first load stopped at its budget");
    assert_eq!(reported, 100);
    assert_eq!(landed, 100);
    assert_eq!(item_label(&host, &principals), json!("L99"));
}

/// S3 (#1488): a hosted load whose 100-row page starts with four one-member
/// runs the cell refuses (each record's bundle is disputed by a conflicting
/// capture of the same source event) spends a page attempt and a fallback
/// on each, its whole budget, before the valid run. The continuation resumes
/// after those runs, so it never re-attempts them: the load finishes, the
/// four stay quarantined as evidence and the rest land.
#[test]
fn a_load_resumes_after_the_runs_it_refused() {
    let (reported, quarantined, landed, calls) = load_past_disputes(&[0, 1, 2, 3], 96);
    assert!(calls > 1, "the first load stopped at its budget");
    assert_eq!((reported, quarantined, landed), (100, 4, 96));
}

/// S3 (#1488): the same disputed observation five times over and 87 valid
/// rows, 92 in all, so a call's budget leaves an odd nine commits: four
/// refused runs spend eight and the fifth's attempt the ninth. The stop
/// resumes at that run's own row, not the first row repeating its
/// observation id, so every continuation makes progress.
#[test]
fn a_load_resumes_past_a_repeated_refused_observation() {
    let (reported, quarantined, landed, calls) = load_past_disputes(&[0; 5], 87);
    assert!(calls > 1, "the first load stopped at its budget");
    assert_eq!((reported, quarantined, landed), (92, 5, 87));
}

/// Load `disputed` (the disputed records' numbers, in batch order, each a
/// run the cell refuses) then `valid` rows through the hosted route, each
/// call resuming at the `next` the last answered, in at most nine calls;
/// the rows reported, quarantined and landed, and the calls.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded test counters, as in a #[test] body"
)]
fn load_past_disputes(disputed: &[usize], valid: usize) -> (u64, u64, u64, u32) {
    let export = export();
    let holding = export["snapshot"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["dataset"] == "Warehouse Lot Balance")
        .unwrap()
        .clone();
    let row = |name: String, key: String, quantity: i64| {
        let mut row = holding.clone();
        row["record"]["name"] = json!(name);
        row["record"]["custom_norn_ref"] = json!(key);
        row["record"]["quantity"] = json!(quantity);
        row
    };
    // The refused rows key one holding, so each is a run of its own.
    let refused = |k: usize, quantity: i64| {
        row(
            format!("SYN-disputed-{k}"),
            "demo/holding/disputed".into(),
            quantity,
        )
    };
    let mut snapshot: Vec<Value> = disputed.iter().map(|&k| refused(k, 1)).collect();
    snapshot.extend((0..valid).map(|k| {
        row(
            format!("SYN-valid-{k:02}"),
            format!("demo/holding/valid/{k:02}"),
            2,
        )
    }));
    let parse = |snapshot: &[Value]| {
        norn_cell_do::recorded::Recorded::parse(
            &json!({ "profile": export["profile"], "snapshot": snapshot }),
        )
        .unwrap()
        .observations()
        .unwrap()
    };
    let scope = seed::demo_scope();
    let mut store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let mut distinct = disputed.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    let first: Vec<Value> = distinct.iter().map(|&k| refused(k, 1)).collect();
    let conflicting: Vec<Value> = distinct.iter().map(|&k| refused(k, 9)).collect();
    for ((raw, input), (other, _)) in parse(&first).iter().zip(parse(&conflicting)) {
        store.inject_mapping_bundle(raw, input).unwrap();
        store.inject_mapping_bundle(&other, input).unwrap();
    }
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let mut connect = connect();
    let mut from = 0_usize;
    let (mut reported, mut landed, mut quarantined, mut calls) = (0_u64, 0_u64, 0_u64, 0_u32);
    while from < snapshot.len() {
        let body =
            json!({ "profile": export["profile"], "snapshot": snapshot[from..] }).to_string();
        let (status, loaded) = load(&mut connect, &host, &principals, ADMIN, &body);
        assert_eq!(status, 200, "{loaded}");
        reported += loaded["rows"].as_u64().unwrap();
        landed += loaded["landed"].as_u64().unwrap() + loaded["alreadyLanded"].as_u64().unwrap();
        quarantined += loaded["quarantined"].as_u64().unwrap();
        calls += 1;
        assert!(calls < 10, "a continuation always makes progress: {loaded}");
        match loaded["next"].as_u64() {
            Some(next) => from += usize::try_from(next).unwrap(),
            None => from = snapshot.len(),
        }
    }
    (reported, quarantined, landed, calls)
}

/// S8: a malformed field nearly as large as a dev request is retained once
/// (the raw evidence) with a bounded diagnostic, so its page stays under the
/// inbox row bound and the valid rows beside it still land.
#[test]
fn a_near_limit_malformed_row_never_aborts_its_page() {
    let (host, _principals) = host();
    let export = export();
    let mut snapshot: Vec<Value> = export["snapshot"].as_array().unwrap().clone();
    let at = snapshot
        .iter()
        .position(|row| row["dataset"] == "Warehouse Lot Balance")
        .unwrap();
    // Quotes escape on every copy: the request is under the dev body bound
    // (512 KiB), and four escaped copies of the field were over the 2 MiB
    // inbox row bound, which aborted the whole page's capture.
    snapshot[at]["record"]["quantity"] = json!("\"".repeat(200_000));
    let document = json!({ "profile": export["profile"], "snapshot": snapshot });
    assert!(document.to_string().len() < 512 * 1024);
    let recorded = norn_cell_do::recorded::Recorded::parse(&document).unwrap();
    let rows = recorded.observations().unwrap();
    let outcomes = host.ingest(&rows).expect("the page captures");
    for (n, ingested) in outcomes.iter().enumerate() {
        if n == at {
            let norn_cell::MappingOutcome::Quarantined(reasons) = &ingested.outcome else {
                panic!("{ingested:?}");
            };
            let reason = &reasons[0];
            assert_eq!(reason.field.as_deref(), Some("quantity"));
            assert!(reason.value.as_ref().unwrap().len() < 1024, "{reason:?}");
            assert!(reason.reason.len() < 1024, "{reason:?}");
        } else {
            assert_eq!(
                ingested.outcome,
                norn_cell::MappingOutcome::Landed,
                "{ingested:?}"
            );
        }
    }
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
            // A client polls; it does not hold the cell back-to-back.
            std::thread::sleep(std::time::Duration::from_millis(5));
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

/// S4 (INGEST-YIELD-1): a read pins one snapshot for its whole request. A load
/// started while a read is in flight commits nothing until the read ends,
/// however long that is (before, a load yielded to a read for at most 250 ms
/// and then committed under it, moving its snapshot).
#[test]
fn a_load_commits_nothing_while_a_read_is_pinned() {
    let (host, principals) = host();
    let recorded = norn_cell_do::recorded::Recorded::parse(
        &json!({ "profile": export()["profile"], "snapshot": same_item(3) }),
    )
    .unwrap();
    let rows = recorded.observations().unwrap();
    let head = host.verified_head().unwrap();
    let reading = host.reading();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::scope(|threads| {
        threads.spawn(|| {
            let ingested = host.ingest(&rows).unwrap();
            done.send(ingested.len()).unwrap();
        });
        // Several times the 250 ms yield bound.
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_millis(1_500))
                .is_err(),
            "the load committed while a read was pinned"
        );
        assert_eq!(
            host.verified_head().unwrap(),
            head,
            "the commit frontier did not move under the pin"
        );
        drop(reading);
        assert_eq!(
            finished
                .recv_timeout(std::time::Duration::from_secs(120))
                .unwrap(),
            3
        );
    });
    assert_eq!(item_label(&host, &principals), json!("L2"));
}
