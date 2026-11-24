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
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        self.0.capabilities()
    }
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

struct MissingProvenanceProfileEngine(Engine);

impl norn_cell::analytics::AnalyticalEngine for MissingProvenanceProfileEngine {
    fn engine_id(&self) -> norn_cell::analytics::AnalyticalEngineId {
        self.0.engine_id()
    }
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        self.0.capabilities()
    }
    fn mirror_frontier(
        &self,
        request: &norn_cell::analytics::FrontierRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::FrontierResponse> {
        self.0.mirror_frontier(request)
    }
    fn mirror_sync(
        &self,
        request: &norn_cell::analytics::SyncRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::FrontierResponse> {
        self.0.mirror_sync(request)
    }
    fn evaluate_question(
        &self,
        request: &norn_cell::analytics::EvaluateRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::EvaluateResponse> {
        self.0.evaluate_question(request)
    }
    fn query_members(
        &self,
        request: &norn_cell::analytics::MembersRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::MembersResponse> {
        self.0.query_members(request)
    }
    fn load_dataset(
        &self,
        request: &norn_cell::analytics::LoadRequest,
        file: &std::path::Path,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LoadResponse> {
        self.0.load_dataset(request, file)
    }
    fn land_dataset(
        &self,
        request: &norn_cell::analytics::LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> norn_cell::analytics::EngineOutcome<()>,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LandResponse> {
        self.0.land_dataset(request, fetch)
    }
    fn settle_dataset(
        &self,
        request: &norn_cell::analytics::SettleRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LandResponse> {
        self.0.settle_dataset(request)
    }
    fn store_dataset(
        &self,
        request: &norn_cell::analytics::StoreRequest,
        send: &mut dyn FnMut(
            &std::path::Path,
            &std::path::Path,
        ) -> norn_cell::analytics::EngineOutcome<()>,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LandResponse> {
        self.0.store_dataset(request, send)
    }
    fn store_dataset_json(
        &self,
        body: &norn_cell::analytics::StoreBody,
        send: &mut dyn FnMut(
            &std::path::Path,
            &std::path::Path,
        ) -> norn_cell::analytics::EngineOutcome<()>,
    ) -> norn_cell::analytics::EngineOutcome<Box<serde_json::value::RawValue>> {
        self.0.store_dataset_json(body, send)
    }
    fn query_lineage(
        &self,
        request: &norn_cell::analytics::LineageRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LineageResponse> {
        self.0.query_lineage(request)
    }
    fn query_page(
        &self,
        request: &norn_cell::analytics::PageRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::PageResponse> {
        self.0.query_page(request)
    }
    fn profile_objects(
        &self,
        request: &norn_cell::analytics::ProfileRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::ProfileResponse> {
        let mut resp = self.0.profile_objects(request)?;
        resp.provenance = None;
        Ok(resp)
    }
    fn export_dataset(
        &self,
        request: &norn_cell::analytics::ExportRequest,
    ) -> norn_cell::analytics::EngineOutcome<std::path::PathBuf> {
        self.0.export_dataset(request)
    }
    fn admit_transform(
        &self,
        plan: &norn_checker::transform::TransformPlan,
    ) -> Result<Box<dyn norn_cell::analytics::AdmittedTransform>, norn_cell::CellError> {
        self.0.admit_transform(plan)
    }
}

struct ReplayedProfileTransport {
    inner: InProcess,
    stale_profile_body: String,
}

impl norn_api::analytics_routes::Transport for ReplayedProfileTransport {
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        self.inner.capabilities()
    }
    fn resumed(&self) -> Option<norn_api::analytics_routes::EngineAnswer> {
        None
    }
    fn call(
        &self,
        call: &norn_api::analytics_routes::EngineCall,
    ) -> Option<norn_api::analytics_routes::EngineAnswer> {
        if call.op == "v2/profile" {
            return Some(norn_api::analytics_routes::EngineAnswer {
                op: call.op.clone(),
                digest: call.digest.clone(),
                status: 200,
                body: self.stale_profile_body.clone(),
            });
        }
        self.inner.call(call)
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
        let engine_document: Value = serde_json::from_str(&engine.body).ok().unwrap_or_default();
        assert_eq!(
            engine_document.get("engine"),
            Some(&json!("duckdb")),
            "{name}"
        );
        assert_eq!(profile_of(&engine), profile_of(&exact), "{name}");
    }
}

#[test]
fn profile_refuses_stale_and_missing_provenance_with_409_over_inprocess_and_exchange()
-> Result<(), Box<dyn std::error::Error>> {
    use norn_api::analytics_routes::Transport as _;
    use norn_api::olap_credential::Issuer;
    use norn_cell::analytics::{EngineCapabilities, TenantId};
    use norn_cell_do::olap::Exchange;

    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory()?, &scope)?;
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS)?;
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .map_err(|r| format!("{r:?}"))?
        .with_authorizer(policy);
    let names = host.object_type_names().map_err(|r| format!("{r:?}"))?;
    let first = names.first().ok_or("at least one demo type")?;
    let path = format!("/v1/objects/{first}:profile");
    let req = server::Request {
        method: "GET",
        path: &path,
        query: SCOPE,
        authorization: MEMBER,
        now: 0,
        body: "",
    };

    // 1. Stale freshness over InProcess -> 409 object.snapshot_moved.
    let dir = tempfile::tempdir()?;
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })?;
    let stale_transport = StaleProfileTransport(InProcess(engine));
    let stale_resp =
        server::analytics(&host, &principals, &stale_transport, None, &req).ok_or("profile")?;
    assert_eq!(stale_resp.status, 409, "{}", stale_resp.body);
    assert!(stale_resp.body.contains("object.snapshot_moved"));

    // 2. Missing provenance over InProcess -> InProcess refuses 409 core.compute.stale_input -> 409 object.snapshot_moved.
    let dir2 = tempfile::tempdir()?;
    let missing_engine = MissingProvenanceProfileEngine(Engine::new(Settings {
        root: dir2.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })?);
    let missing_transport = InProcess(missing_engine);
    let missing_resp =
        server::analytics(&host, &principals, &missing_transport, None, &req).ok_or("profile")?;
    assert_eq!(missing_resp.status, 409, "{}", missing_resp.body);
    assert!(missing_resp.body.contains("object.snapshot_moved"));

    // 3. Exchange transport receiving olapd's 409 core.compute.stale_input -> 409 object.snapshot_moved.
    let tenant = TenantId::of_scope(&host.scope().key());
    let (secret, _pub_spec) = norn_cell_do::olapd::new_key("k1")?;
    let issuer = Issuer::parse(&secret)?;
    let exchange = Exchange::new(issuer, tenant, EngineCapabilities::duckdb_default());
    let mut final_resp = None;
    for _ in 0..8 {
        let resp = server::analytics(&host, &principals, &exchange, None, &req).ok_or("profile")?;
        let Some(call) = exchange.take_pending(1_000) else {
            final_resp = Some(resp);
            break;
        };
        let answer = if call.op == "v2/profile" {
            norn_api::analytics_routes::EngineAnswer {
                op: call.op,
                digest: call.digest,
                status: 409,
                body: json!({
                    "code": "core.compute.stale_input",
                    "detail": "v2 profile response omitted provenance"
                })
                .to_string(),
            }
        } else {
            missing_transport
                .call(&call)
                .ok_or("in-process sync answer")?
        };
        exchange.resume(answer);
    }
    let exchange_resp = final_resp.ok_or("exchange did not settle")?;
    assert_eq!(exchange_resp.status, 409, "{}", exchange_resp.body);
    assert!(exchange_resp.body.contains("object.snapshot_moved"));
    Ok(())
}

struct ProfileFixture {
    host: DevHost<NativeEngine>,
    principals: FixturePrincipals,
    model_hash: String,
    item_pin: String,
}

fn build_profile_host(
    objects: &[norn_ir::ObjectTypeDef],
) -> Result<ProfileFixture, Box<dyn std::error::Error>> {
    let scope = seed::demo_scope();
    let model = norn_cell::Model::from_definitions(objects, &[])?;
    let store = norn_cell::CellStore::open_in_memory(model)?;
    let model_hash = store.model().model_hash().to_owned();
    let item_pin = store.analytics_table("demo.Item")?.pin;
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS)?;
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .map_err(|r| format!("{r:?}"))?
        .with_authorizer(policy);
    Ok(ProfileFixture {
        host,
        principals,
        model_hash,
        item_pin,
    })
}

#[test]
fn profile_refuses_model_generation_change_with_identical_table_schema_pin()
-> Result<(), Box<dyn std::error::Error>> {
    use norn_api::analytics_routes::Transport as _;
    use norn_api::olap_credential::Issuer;
    use norn_cell::analytics::{EngineCapabilities, TenantId};
    use norn_cell_do::olap::Exchange;

    let item: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "demo.Item",
        "properties": [
            {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}}
        ]
    }))?;
    let extra: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "demo.Extra",
        "properties": [{"id": "note", "type": {"kind": "text"}}]
    }))?;
    let v1 = build_profile_host(std::slice::from_ref(&item))?;
    let v2 = build_profile_host(&[item, extra])?;
    assert_eq!(
        v1.item_pin, v2.item_pin,
        "demo.Item TableSchema.pin stays identical"
    );
    assert_ne!(v1.model_hash, v2.model_hash, "cell model_hash changed");

    let req = server::Request {
        method: "GET",
        path: "/v1/objects/demo.Item:profile",
        query: SCOPE,
        authorization: MEMBER,
        now: 0,
        body: "",
    };
    let dir = tempfile::tempdir()?;
    let in_process = InProcess(Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })?);
    let tenant = TenantId::of_scope(&v1.host.scope().key());
    let (secret, _pub_spec) = norn_cell_do::olapd::new_key("k1")?;
    let exchange = Exchange::new(
        Issuer::parse(&secret)?,
        tenant,
        EngineCapabilities::duckdb_default(),
    );
    let mut v1_profile_answer = None;
    for _ in 0..8 {
        let _ = server::analytics(&v1.host, &v1.principals, &exchange, None, &req);
        let Some(call) = exchange.take_pending(1_000) else {
            break;
        };
        let answer = in_process.call(&call).ok_or("v1 engine call")?;
        if call.op == "v2/profile" {
            v1_profile_answer = Some(answer);
            break;
        }
        exchange.resume(answer);
    }
    let stale_answer = v1_profile_answer.ok_or("v1 v2/profile call")?;
    assert_eq!(stale_answer.status, 200, "{}", stale_answer.body);

    // 1. Exchange re-entry on host_v2 rejects v1's answer because ProfileRequest.model_pin changed the call digest.
    exchange.resume(stale_answer.clone());
    let _ = server::analytics(&v2.host, &v2.principals, &exchange, None, &req);
    let next_call = exchange
        .take_pending(1_000)
        .ok_or("re-entry must not accept v1 answer")?;
    assert_ne!(next_call.digest, stale_answer.digest);

    // 2. Even if a transport replays v1's real ProfileResponse (matching schema_pin, old model_pin) directly,
    // verify_profile_provenance refuses with HTTP 409 object.snapshot_moved.
    let replayed = ReplayedProfileTransport {
        inner: in_process,
        stale_profile_body: stale_answer.body,
    };
    let refused =
        server::analytics(&v2.host, &v2.principals, &replayed, None, &req).ok_or("profile")?;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(refused.body.contains("object.snapshot_moved"));
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
