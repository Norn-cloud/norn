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

use super::analytics_backend::{Engine, Settings};
use norn_api::analytics_routes::{EngineAnswer, EngineCall, EngineQuestions, Transport};
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_cell::engine::NativeEngine;
use norn_cell_do::olap::{Exchange, InProcess};
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
    for name in names {
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
        assert_eq!(
            engine_document["engine"],
            super::analytics_backend::ENGINE_ID.as_str(),
            "{name}"
        );
        assert_eq!(profile_of(&engine), profile_of(&exact), "{name}");
    }
}

/// A change made to an engine answer.
type Edit = fn(&mut EngineAnswer);

/// A transport whose engine answers `profile` and then has `edit` applied.
struct Edited<F>(InProcess<Engine>, F);

impl<F: Fn(&mut EngineAnswer) + Send + Sync> Transport for Edited<F> {
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        self.0.capabilities()
    }
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        let mut answer = self.0.call(call)?;
        if call.op == "profile" {
            (self.1)(&mut answer);
        }
        Some(answer)
    }
}

fn engine_in(dir: &tempfile::TempDir) -> std::io::Result<InProcess<Engine>> {
    Ok(InProcess(Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })?))
}

fn profile_request(path: &str) -> server::Request<'_> {
    server::Request {
        method: "GET",
        path,
        query: SCOPE,
        authorization: MEMBER,
        now: 0,
        body: "",
    }
}

fn drop_provenance(answer: &mut EngineAnswer) {
    if let Ok(mut body) = serde_json::from_str::<Value>(&answer.body) {
        if let Some(object) = body.as_object_mut() {
            object.remove("provenance");
        }
        answer.body = body.to_string();
    }
}

fn set(answer: &mut EngineAnswer, pointer: &str, value: Value) {
    if let Ok(mut body) = serde_json::from_str::<Value>(&answer.body) {
        if let Some(slot) = body.pointer_mut(pointer) {
            *slot = value;
        }
        answer.body = body.to_string();
    }
}

/// QUERY-FRESH-1 on `:profile`: an engine answer over any snapshot but the
/// one decided, or not read exactly, is a retryable 409 and never a profile;
/// an answer that names no snapshot is not decoded at all.
#[test]
fn a_profile_is_answered_only_over_the_snapshot_decided() -> Result<(), Box<dyn std::error::Error>>
{
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory()?, &scope)?;
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS)?;
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .map_err(|r| format!("{r:?}"))?
        .with_authorizer(policy);
    let names = host.object_type_names().map_err(|r| format!("{r:?}"))?;
    let path = format!("/v1/objects/{}:profile", names.first().ok_or("a type")?);
    let request = profile_request(&path);
    let cases: [(&str, Edit, u16); 6] = [
        (
            "stale read",
            |a| set(a, "/provenance/freshness", json!("stale")),
            409,
        ),
        (
            "other model",
            |a| set(a, "/provenance/snapshot/modelPin", json!("m")),
            409,
        ),
        (
            "other schema",
            |a| set(a, "/provenance/snapshot/schemaPin", json!("s")),
            409,
        ),
        (
            "other tenant",
            |a| set(a, "/provenance/tenant", json!("0".repeat(64))),
            409,
        ),
        (
            "engine stale",
            |a| {
                a.status = 409;
                a.body = json!({"code": "core.compute.stale_input", "detail": ""}).to_string();
            },
            409,
        ),
        ("no provenance", drop_provenance, 500),
    ];
    for (case, edit, status) in cases {
        let dir = tempfile::tempdir()?;
        let transport = Edited(engine_in(&dir)?, edit);
        let response =
            server::analytics(&host, &principals, &transport, None, &request).ok_or(case)?;
        assert_eq!(response.status, status, "{case}: {}", response.body);
        let refusal: Value = serde_json::from_str(&response.body)?;
        assert_eq!(refusal["retryable"], json!(status == 409), "{case}");
        if status == 409 {
            assert!(response.body.contains("object.snapshot_moved"), "{case}");
        }
    }
    Ok(())
}

/// D136: a profile is bound to the cell model it was decided under. Two
/// cells whose profiled type has the same table schema but whose models
/// differ: the first's answer is never the second's. Re-entered, its digest
/// is another version's, so the profile is a moved snapshot (409) and no
/// call is made again; replayed, its snapshot names the other model (409).
#[test]
fn a_profile_of_another_model_is_never_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let item: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "demo.Item",
        "properties": [{"id": "note", "type": {"kind": "text"}}]
    }))?;
    let extra: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "demo.Extra",
        "properties": [{"id": "note", "type": {"kind": "text"}}]
    }))?;
    let build = |objects: &[norn_ir::ObjectTypeDef]| -> Result<_, Box<dyn std::error::Error>> {
        let scope = seed::demo_scope();
        let store = norn_cell::CellStore::open_in_memory(norn_cell::Model::from_definitions(
            objects,
            &[],
        )?)?;
        let pins = (
            store.model().model_hash().to_owned(),
            store.analytics_table("demo.Item")?.pin,
        );
        let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS)?;
        let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
        let host = DevHost::from_store(store, scope, |command| command.payload.clone())
            .map_err(|r| format!("{r:?}"))?
            .with_authorizer(policy);
        Ok((host, principals, pins))
    };
    let (first, first_principals, first_pins) = build(std::slice::from_ref(&item))?;
    let (second, second_principals, second_pins) = build(&[item, extra])?;
    assert_eq!(first_pins.1, second_pins.1);
    assert_ne!(first_pins.0, second_pins.0);

    let request = profile_request("/v1/objects/demo.Item:profile");
    let dir = tempfile::tempdir()?;
    let engine = engine_in(&dir)?;
    let tenant = norn_cell::analytics::TenantId::of_scope(&first.scope().key());
    let (secret, _public) = norn_cell_do::olapd::new_key("k1")?;
    let exchange = Exchange::new(
        norn_api::olap_credential::Issuer::parse(&secret)?,
        tenant,
        norn_cell::analytics::EngineCapabilities::of(super::analytics_backend::ENGINE_ID),
    );
    let mut answered = None;
    for _step in 0..8 {
        let _ended = server::analytics(&first, &first_principals, &exchange, None, &request);
        let call = exchange.take_pending(1_000).ok_or("an engine call")?;
        let answer = engine.call(&call).ok_or("an engine answer")?;
        if call.op == "profile" {
            answered = Some(answer);
            break;
        }
        exchange.resume(answer);
    }
    let answered = answered.ok_or("the first cell's profile")?;
    assert_eq!(answered.status, 200, "{}", answered.body);

    exchange.resume(answered.clone());
    let reentered = server::analytics(&second, &second_principals, &exchange, None, &request)
        .ok_or("a profile")?;
    assert_eq!(reentered.status, 409, "{}", reentered.body);
    assert!(reentered.body.contains("object.snapshot_moved"));
    assert!(exchange.take_pending(1_000).is_none());

    let body = answered.body;
    let replayed = Edited(engine, move |a: &mut EngineAnswer| a.body.clone_from(&body));
    let response = server::analytics(&second, &second_principals, &replayed, None, &request)
        .ok_or("a profile")?;
    assert_eq!(response.status, 409, "{}", response.body);
    Ok(())
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
