//! "See the records" on every card kind (D102): on the seeded demo cell, an
//! additive, a semi-additive (end-of-period stock) and a ratio metric are
//! answered on the engine, then traced at the answer's own frontier
//! (`knownAsOf`, `nowMs`) and at the current one. Each trace reaches the
//! members of the bucket it names, and both traces agree while nothing has
//! been committed since.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "trace correspondence over the seeded demo cell"
)]

use norn_api::analytics_routes::EngineQuestions;
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::engine::NativeEngine;
use norn_cell_do::olap::InProcess;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::questions::DerivedQuestions;
use norn_cell_do::{seed, server};
use serde_json::{Value, json};

const MEMBER: Option<&str> = Some("Bearer fixture:admin@demo");
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

/// The first listed metric `pick` accepts, with the concept it reads.
fn metric(listed: &Value, pick: impl Fn(&Value) -> bool) -> Option<(Value, String)> {
    let metrics = listed["metrics"].as_array()?;
    let concept = |name: &Value| {
        metrics
            .iter()
            .find(|m| &m["name"] == name)
            .and_then(|m| m["objectType"].as_str().map(str::to_owned))
    };
    metrics.iter().find(|m| pick(m)).and_then(|m| {
        let object_type = m["objectType"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| concept(&m["numerator"]))?;
        Some((m.clone(), object_type))
    })
}

/// The time a metric buckets by: its own, or its numerator's.
fn time_of(listed: &Value, metric: &Value) -> String {
    metric["timeProperty"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            listed["metrics"]
                .as_array()?
                .iter()
                .find(|m| m["name"] == metric["numerator"])
                .and_then(|m| m["timeProperty"].as_str().map(str::to_owned))
        })
        .expect("a time property")
}

#[test]
fn every_card_kind_traces_at_its_answers_frontier() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).expect("demo");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let dir = tempfile::tempdir().unwrap();
    let transport = InProcess(
        Engine::new(Settings {
            root: dir.path().to_path_buf(),
            memory_limit: "256MB".into(),
            threads: 1,
        })
        .unwrap(),
    );
    let derived = DerivedQuestions::new([7; 32], scope);
    let runtime = derived
        .questions(&host)
        .expect("the demo's derived metrics");
    let on_engine = EngineQuestions::new(&runtime, &transport, host.scope());
    let questions: &dyn AdHocQuestions<NativeEngine> = &on_engine;
    let call = |method: &'static str, path: &'static str, body: &str| {
        let response = server::handle_with(
            &host,
            &principals,
            Some(questions),
            &server::Request {
                method,
                path,
                query: SCOPE,
                authorization: MEMBER,
                now: 0,
                body,
            },
        );
        let document: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        (response.status, document)
    };
    let (status, listed) = call("GET", "/v1/questions:metrics", "");
    assert_eq!(status, 200, "{listed}");
    let kinds: [(&str, Box<dyn Fn(&Value) -> bool>); 3] = [
        (
            "additive",
            Box::new(|m: &Value| m["kind"] == "simple" && m["additivity"]["kind"] == "additive"),
        ),
        (
            "semi-additive",
            Box::new(|m: &Value| m["additivity"]["kind"] == "semi_additive"),
        ),
        ("ratio", Box::new(|m: &Value| m["kind"] == "ratio")),
    ];
    for (kind, pick) in kinds {
        let (metric, object_type) =
            metric(&listed, pick).unwrap_or_else(|| panic!("the demo has a {kind} metric"));
        let question = json!({
            "irVersion": "0.1", "name": "demo.card", "objectSet": object_type,
            "aggregate": [{"metric": metric["name"]}],
            "groupBy": [{"path": {"property": time_of(&listed, &metric)}, "grain": "year"}],
            "window": {"start": "2020-01-01T00:00:00Z", "end": "2030-01-01T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 60000},
        });
        let (status, answer) = call(
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": question }).to_string(),
        );
        assert_eq!(status, 200, "{kind}: {answer}");
        assert_eq!(answer["engine"], "duckdb", "{kind}");
        let (bucket, row) = answer["rows"]
            .as_object()
            .and_then(|rows| {
                rows.iter()
                    .find(|(_, row)| row["members"].as_object().is_some_and(|m| !m.is_empty()))
            })
            .unwrap_or_else(|| panic!("{kind}: a bucket with members: {answer}"));
        let members = row["members"].as_object().unwrap().len();
        let trace = |at_answer: bool| {
            let mut body = json!({
                "question": question,
                "lineageRef": answer["lineageRef"],
                "bucket": bucket,
            });
            if at_answer {
                body["knownAsOf"] = answer["knownAsOf"].clone();
                body["nowMs"] = answer["nowMs"].clone();
            }
            call("POST", "/v1/questions:trace", &body.to_string())
        };
        let (status, at_frontier) = trace(true);
        assert_eq!(status, 200, "{kind} at its frontier: {at_frontier}");
        assert_eq!(
            at_frontier["contributions"].as_array().map(Vec::len),
            Some(members),
            "{kind}: every member of the bucket"
        );
        let (status, current) = trace(false);
        assert_eq!(status, 200, "{kind} now: {current}");
        assert_eq!(
            current["contributions"], at_frontier["contributions"],
            "{kind}: nothing was committed since"
        );
    }
}
