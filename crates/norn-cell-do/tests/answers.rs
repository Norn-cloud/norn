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
use norn_cell_do::principal::DEMO_PRINCIPALS;
use norn_cell_do::server::{self, Response};
use serde_json::Value;

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
const VIEWER: Option<&str> = Some("Bearer fixture:viewer@demo");
const GUEST: Option<&str> = Some("Bearer fixture:guest@demo");

fn cell(engine: NativeEngine) -> (DevHost<NativeEngine>, DemoQuestions) {
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(engine, &scope).unwrap();
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, DEMO_GRANTS).unwrap();
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    (host, DemoQuestions::new(opened.plan, scope))
}

fn get(
    cell: &(DevHost<NativeEngine>, DemoQuestions),
    path: &str,
    query: &str,
    auth: Option<&str>,
) -> Response {
    server::handle_with(
        &cell.0,
        &DEMO_PRINCIPALS,
        Some(&cell.1),
        "GET",
        path,
        query,
        auth,
    )
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
    let answer = json(&viewer);
    assert_eq!(answer["freshness"], "exact");
    assert_eq!(answer["complete"], true);
    assert!(!answer["rows"].as_object().unwrap().is_empty());
    // Population alone answers the question; the pin names the population.
    let guest = get(&cell, &answer_path(), SCOPE, GUEST);
    assert_eq!(guest.status, 200, "{}", guest.body);
    assert_eq!(json(&guest)["rows"], answer["rows"]);
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
        let guest = get(&cell, &path, &query, GUEST);
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
    let post = server::handle_with(
        &cell.0,
        &DEMO_PRINCIPALS,
        Some(&cell.1),
        "POST",
        &answer_path(),
        SCOPE,
        VIEWER,
    );
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
