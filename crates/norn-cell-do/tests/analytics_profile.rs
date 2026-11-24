//! D102 / HOST-READ-5: on a host with an analytics engine, `:profile` is
//! computed by the engine and equals the exact profile of the same snapshot,
//! normalizing only the engine label and the scan bound it reports. A derived
//! question on the engine is admitted through the derived metric space
//! (METRIC-UNIT-PARTITION-1), as on a host without one.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    missing_docs,
    reason = "engine correspondence over the seeded demo cell"
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

fn profile_of(response: &server::Response) -> Value {
    assert_eq!(response.status, 200, "{}", response.body);
    let mut document: Value = serde_json::from_str(&response.body).unwrap();
    let object = document.as_object_mut().unwrap();
    object.remove("engine");
    object.remove("scanLimit");
    document
}

struct StaleProfileTransport(InProcess);

impl norn_api::analytics_routes::Transport for StaleProfileTransport {
    fn resumed(&self) -> Option<norn_api::analytics_routes::EngineAnswer> {
        self.0.resumed()
    }
    fn call(
        &self,
        call: &norn_api::analytics_routes::EngineCall,
    ) -> Option<norn_api::analytics_routes::EngineAnswer> {
        let mut answer = self.0.call(call)?;
        if call.op == "v2/profile"
            && answer.status == 200
            && let Ok(mut resp) =
                serde_json::from_str::<norn_cell::analytics::ProfileResponse>(&answer.body)
        {
            if let Some(prov) = &mut resp.provenance {
                prov.freshness = Some(norn_cell::compute::Freshness::Stale);
            }
            if let Ok(encoded) = serde_json::to_string(&resp) {
                answer.body = encoded;
            }
        }
        Some(answer)
    }
}

#[test]
fn the_engine_profile_equals_the_exact_profile_of_every_demo_type() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).expect("demo");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
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
    let names = host.object_type_names().unwrap();
    assert_ne!(names.len(), 0);
    for name in &names {
        let path = format!("/v1/objects/{name}:profile");
        let request = server::Request {
            method: "GET",
            path: &path,
            query: SCOPE,
            authorization: MEMBER,
            now: 0,
            body: "",
        };
        let exact = server::handle(&host, &principals, &request);
        let engine = server::analytics(&host, &principals, &transport, None, &request)
            .expect("an engine route");
        let engine_document: Value = serde_json::from_str(&engine.body).unwrap();
        assert_eq!(engine_document["engine"], "duckdb", "{name}");
        assert_eq!(profile_of(&engine), profile_of(&exact), "{name}");
    }

    // Stale or non-exact profile provenance fails closed with HTTP 409 (matching OpenAPI `:profile`).
    let stale_transport = StaleProfileTransport(transport);
    let first = names.first().expect("at least one demo type");
    let stale_req = server::Request {
        method: "GET",
        path: &format!("/v1/objects/{first}:profile"),
        query: SCOPE,
        authorization: MEMBER,
        now: 0,
        body: "",
    };
    let stale_resp = server::analytics(&host, &principals, &stale_transport, None, &stale_req)
        .expect("profile route");
    assert_eq!(stale_resp.status, 409, "{}", stale_resp.body);
}

/// The alpha's shape: the seeded demo cell's derived metrics answered on its
/// engine. The order-line quantity's unit lives in `unit` and `currency`; a
/// question that groups by neither is grouped by both, never refused
/// `metric.validation.mixed_unit`.
#[test]
fn a_derived_question_on_the_engine_is_grouped_by_the_unit_sources_it_leaves_free() {
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
        server::handle_with(
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
        )
    };
    let listed = call("GET", "/v1/questions:metrics", "");
    assert_eq!(listed.status, 200, "{}", listed.body);
    let listed: Value = serde_json::from_str(&listed.body).unwrap();
    let metric = listed["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|metric| {
            metric["objectType"] == "canon.OrderLine" && metric["expression"] == "quantity"
        })
        .expect("the derived order-line quantity")
        .clone();
    assert_eq!(
        metric["unitPartition"],
        json!(["unit", "currency"]),
        "{metric}"
    );
    let question = json!({
        "irVersion": "0.1", "name": "demo.ordered_by_week", "objectSet": "canon.OrderLine",
        "aggregate": [{"metric": metric["name"]}],
        "groupBy": [{"path": {"property": "due_at"}, "grain": "week"}],
        "window": {"start": "2026-01-05T00:00:00Z", "end": "2027-01-04T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 60000},
    });
    let plan = questions
        .admit(&serde_json::from_value(question.clone()).unwrap(), 100)
        .expect("admitted on the engine");
    let grouped: Vec<_> = plan
        .question()
        .dimensions
        .iter()
        .map(|dimension| dimension.path.property.as_str())
        .collect();
    assert_eq!(grouped, ["due_at", "unit", "currency"]);
    let answered = call(
        "POST",
        "/v1/questions:evaluate",
        &json!({ "question": question }).to_string(),
    );
    assert_eq!(answered.status, 200, "{}", answered.body);
}
