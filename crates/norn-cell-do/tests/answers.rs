//! Live answers and numeric traces over the analytics cell, end to end: the
//! fetch dispatch, the shared question services, HOST-READ-1 and the kernel
//! PDP policy compiled against the cell's own model.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "route correspondence"
)]
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::analytics::{self, DemoQuestions, QUESTION};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server::{self, Response};
use serde_json::Value;

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
const VIEWER: Option<&str> = Some("Bearer fixture:viewer@demo");
const GUEST: Option<&str> = Some("Bearer fixture:guest@demo");
/// The test stage's lineage key (the `NORN_LINEAGE_KEY` Worker secret in a deploy).
const SECRET: [u8; 32] = [5; 32];

type Cell = (DevHost<NativeEngine>, DemoQuestions, FixturePrincipals);

fn cell(engine: NativeEngine) -> Cell {
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(engine, &scope).unwrap();
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    (
        host,
        DemoQuestions::new(opened.plan, scope).with_handle_key(&SECRET),
        principals,
    )
}

fn call(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
) -> Response {
    let request = server::Request {
        method,
        path,
        query,
        authorization,
        now: 0,
    };
    server::handle_with(&cell.0, &cell.2, Some(&cell.1), &request)
}

fn get(cell: &Cell, path: &str, query: &str, auth: Option<&str>) -> Response {
    call(cell, "GET", path, query, auth)
}

fn json(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

fn answer_path() -> String {
    format!("/v1/questions/{QUESTION}/answer")
}

fn trace_query(answer: &Value, bucket: &str) -> String {
    let lineage = answer["lineageRef"].as_str().unwrap();
    let pins = form_urlencoded::Serializer::new(String::new())
        .append_pair("lineageRef", lineage)
        .append_pair("bucket", bucket)
        .finish();
    format!("{SCOPE}&{pins}")
}

#[test]
fn granted_principals_get_the_exact_live_answer() {
    let cell = cell(NativeEngine::open_in_memory().unwrap());
    let viewer = get(&cell, &answer_path(), SCOPE, VIEWER);
    assert_eq!(viewer.status, 200, "{}", viewer.body);
    // The DO smoke compares its answer bytes with this native answer (parity).
    if let Some(out) = std::env::var_os("NORN_CELL_DO_ANSWER_OUT") {
        std::fs::write(out, &viewer.body).unwrap();
    }
    let answer = json(&viewer);
    assert_eq!(answer["freshness"], "exact");
    assert_eq!(answer["complete"], true);
    assert!(!answer["rows"].as_object().unwrap().is_empty());
    // Population alone answers the question; the pin names the population.
    let guest = get(&cell, &answer_path(), SCOPE, GUEST);
    assert_eq!(guest.status, 200, "{}", guest.body);
    // The same aggregate and membership; the guest's member amounts are
    // withheld (DATA-VALUES-1), every bucket meeting the minimum of three.
    let guest = json(&guest);
    let (ours, theirs) = (
        answer["rows"].as_object().unwrap(),
        guest["rows"].as_object().unwrap(),
    );
    assert_eq!(
        ours.keys().collect::<Vec<_>>(),
        theirs.keys().collect::<Vec<_>>()
    );
    for (bucket, row) in ours {
        let other = &theirs[bucket];
        assert_eq!(row["totals"], other["totals"], "{bucket}");
        assert_eq!(row["value"], other["value"], "{bucket}");
        let members = |row: &Value| {
            row["members"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(members(row), members(other), "{bucket}");
        assert!(
            other["members"]
                .as_object()
                .unwrap()
                .values()
                .all(|m| m["amounts"].is_null()),
            "{bucket}"
        );
        assert!(other.get("suppressed").is_none(), "{bucket}");
    }
}

#[test]
fn ungranted_foreign_and_anonymous_reads_share_one_denial() {
    let cell = cell(NativeEngine::open_in_memory().unwrap());
    let anonymous = get(&cell, &answer_path(), SCOPE, None);
    assert_eq!(anonymous.status, 403);
    for (query, auth) in [
        (SCOPE, Some("Bearer fixture:stranger@demo")),
        ("orgId=org_demo&environmentId=dev", VIEWER),
        ("orgId=org_demo", VIEWER),
    ] {
        let denied = get(&cell, &answer_path(), query, auth);
        assert_eq!(
            (denied.status, &denied.body),
            (403, &anonymous.body),
            "{query}"
        );
    }
    assert!(!anonymous.body.contains("analytics"));
}

#[test]
fn traces_disclose_values_only_under_their_own_grant() {
    let cell = cell(NativeEngine::open_in_memory().unwrap());
    let answer = json(&get(&cell, &answer_path(), SCOPE, VIEWER));
    let path = format!("/v1/questions/{QUESTION}/trace");
    for bucket in answer["rows"].as_object().unwrap().keys() {
        let query = trace_query(&answer, bucket);
        let viewer = get(&cell, &path, &query, VIEWER);
        assert_eq!(viewer.status, 200, "{bucket}: {}", viewer.body);
        let viewer = json(&viewer);
        let contributions = viewer["contributions"].as_array().unwrap();
        assert!(!contributions.is_empty(), "{bucket}");
        assert!(
            contributions
                .iter()
                .all(|c| c["restricted"] == false && c["values"].is_object())
        );
        // The lineage pin covers the principal's population, so each principal
        // traces its own answer.
        let own = json(&get(&cell, &answer_path(), SCOPE, GUEST));
        let guest = get(&cell, &path, &trace_query(&own, bucket), GUEST);
        assert_eq!(guest.status, 200, "{bucket}: {}", guest.body);
        let guest = json(&guest);
        let restricted = guest["contributions"].as_array().unwrap();
        assert_eq!(
            restricted.len(),
            contributions.len(),
            "membership is the same population"
        );
        assert!(
            restricted
                .iter()
                .all(|c| c["restricted"] == true && c["values"].is_null())
        );
        assert!(
            restricted
                .iter()
                .all(|c| c["source"]["status"] != "resolved")
        );
    }
}

#[test]
fn unknown_questions_and_malformed_queries_refuse() {
    let cell = cell(NativeEngine::open_in_memory().unwrap());
    let unknown = get(&cell, "/v1/questions/demo.other/answer", SCOPE, VIEWER);
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    assert_eq!(
        get(&cell, &answer_path(), &format!("{SCOPE}&sql=1"), VIEWER).status,
        400
    );
    assert_eq!(
        get(&cell, "/v1/questions/x/y/answer", SCOPE, VIEWER).status,
        404
    );
    let post = call(&cell, "POST", &answer_path(), SCOPE, VIEWER);
    assert_eq!(post.status, 405);
}

#[test]
fn a_restarted_analytics_cell_answers_identically_without_reloading() {
    let directory =
        std::env::temp_dir().join(format!("norn-cell-do-analytics-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("cell.sqlite");
    let open = || NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
    let first = get(&cell(open()), &answer_path(), SCOPE, VIEWER).body;
    let reopened = analytics::provision(open(), &analytics::analytics_scope()).unwrap();
    assert_eq!(
        reopened.store.verified_head().unwrap().commits,
        1,
        "K1 loads exactly once"
    );
    drop(reopened);
    assert_eq!(
        get(&cell(open()), &answer_path(), SCOPE, VIEWER).body,
        first
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

/// The analytics cell with its runtime keyed by `secret`, or unkeyed.
fn cell_keyed(secret: Option<&[u8]>) -> Cell {
    let (host, _, principals) = cell(NativeEngine::open_in_memory().unwrap());
    let scope = analytics::analytics_scope();
    let plan = analytics::provision(NativeEngine::open_in_memory().unwrap(), &scope)
        .unwrap()
        .plan;
    let runtime = DemoQuestions::new(plan, scope);
    (
        host,
        match secret {
            Some(secret) => runtime.with_handle_key(secret),
            None => runtime,
        },
        principals,
    )
}

/// D53: without a stage lineage key the cell publishes no answer, a secret
/// under 32 bytes is no key, and another stage's secret gives another handle
/// that does not open this cell's traces.
#[test]
fn answers_need_the_stage_lineage_key_and_handles_are_per_secret() {
    assert_eq!(
        get(&cell_keyed(None), &answer_path(), SCOPE, VIEWER).status,
        500
    );
    assert_eq!(
        get(&cell_keyed(Some(&[5; 16])), &answer_path(), SCOPE, VIEWER).status,
        500
    );
    let (ours, theirs) = (cell_keyed(Some(&SECRET)), cell_keyed(Some(&[6; 32])));
    let first = json(&get(&ours, &answer_path(), SCOPE, VIEWER));
    let second = json(&get(&theirs, &answer_path(), SCOPE, VIEWER));
    assert_eq!(first["rows"], second["rows"], "the same answer");
    assert_ne!(first["lineageRef"], second["lineageRef"]);
    let trace = format!("/v1/questions/{QUESTION}/trace");
    assert_eq!(
        get(
            &ours,
            &trace,
            &trace_query(&first, "\"2026-10-19\""),
            VIEWER
        )
        .status,
        200
    );
    assert_ne!(
        get(
            &ours,
            &trace,
            &trace_query(&second, "\"2026-10-19\""),
            VIEWER
        )
        .status,
        200
    );
}

/// The analytics cell as the Worker's constructor builds it: the runtime keyed
/// through [`DemoQuestions::try_with_handle_key`] from the configured
/// `NORN_LINEAGE_KEY` (a string in the Worker `env`), or refused.
fn cell_configured(lineage_key: Option<&str>) -> Result<Cell, norn_cell_do::seed::SeedError> {
    let (host, _, principals) = cell(NativeEngine::open_in_memory().unwrap());
    let scope = analytics::analytics_scope();
    let plan = analytics::provision(NativeEngine::open_in_memory().unwrap(), &scope)
        .unwrap()
        .plan;
    let questions = DemoQuestions::new(plan, scope)
        .try_with_handle_key(lineage_key.map(str::as_bytes))?;
    Ok((host, questions, principals))
}

/// S2: the Worker passes `NORN_LINEAGE_KEY` into the constructor. A missing or
/// short secret refuses to open the cell, with an error naming the secret; a
/// configured one makes the authenticated answer and trace routes work, with a
/// keyed handle that no other stage's secret can redeem.
#[test]
fn the_configured_lineage_key_serves_authenticated_answers_and_traces() {
    for short in [None, Some(""), Some("0123456789012345678901234567890")] {
        let Err(refusal) = cell_configured(short) else {
            panic!("{short:?} must refuse to open");
        };
        assert!(
            refusal.0.contains("NORN_LINEAGE_KEY") && refusal.0.contains("32 bytes"),
            "{refusal}"
        );
    }
    let key = "stage-secret-stage-secret-stage-secret";
    let ours = cell_configured(Some(key)).unwrap();
    let answer = get(&ours, &answer_path(), SCOPE, VIEWER);
    assert_eq!(answer.status, 200, "{}", answer.body);
    let answer = json(&answer);
    // A keyed handle: 64 lowercase hex digits (HMAC-SHA-256), not the content hash.
    let handle = answer["lineageRef"].as_str().unwrap();
    assert!(
        handle.len() == 64 && handle.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{handle}"
    );
    let trace = format!("/v1/questions/{QUESTION}/trace");
    let bucket = answer["rows"].as_object().unwrap().keys().next().unwrap();
    let traced = get(&ours, &trace, &trace_query(&answer, bucket), VIEWER);
    assert_eq!(traced.status, 200, "{}", traced.body);
    // The same configured key reproduces the handle; another stage's does not.
    let again = json(&get(
        &cell_configured(Some(key)).unwrap(),
        &answer_path(),
        SCOPE,
        VIEWER,
    ));
    assert_eq!(again["lineageRef"], answer["lineageRef"]);
    let other = cell_configured(Some("another-stage-secret-another-stage!")).unwrap();
    let foreign = json(&get(&other, &answer_path(), SCOPE, VIEWER));
    assert_ne!(foreign["lineageRef"], answer["lineageRef"]);
    assert_ne!(
        get(&other, &trace, &trace_query(&answer, bucket), VIEWER).status,
        200,
        "another stage's key does not redeem this stage's handle"
    );
}
