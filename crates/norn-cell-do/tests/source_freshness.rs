//! QUERY-FRESH-1 (Lane B6): model-derived `RequiredSources` and real
//! connector cursors/watermarks/heartbeats in `DerivedRuntime::snapshot`.
//!
//! 1. Native-only types (no installed `Mapping` targeting the question's grain
//!    or linked types) retain `Freshness::Exact` with empty `source_cursors`.
//! 2. Source-backed types fail closed to `Freshness::Incomplete` (never
//!    `Exact`) across ExactCell, IVM, and hosted `DerivedRuntime::snapshot`
//!    whenever required connector cursors are missing.
//! 3. After a real `Sources` sync commits `_norn_source_cursor` and a healthy
//!    `source.Connection`, hosted `/v1/questions:evaluate` reports `Exact`
//!    (and `:trace` succeeds on its deterministic `lineageRef`), transitions
//!    to `Fresh` within `maxAgeMs`, transitions to `Stale` past `maxAgeMs`,
//!    and returns to `Incomplete` when a sync fails or another required
//!    mapping has not synced yet.

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "QUERY-FRESH-1 end-to-end integration tests"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use norn_api::host::DevHost;
    use norn_api::pull::PullProfile;
    use norn_api::question_routes::AdHocQuestions;
    use norn_api::scope::CellScope;
    use norn_api::sources::{Sources, Step, Trigger};
    use norn_cell::compute::ivm::IncrementalQuery;
    use norn_cell::compute::{Freshness, QueryContext, RequiredSources};
    use norn_cell::engine::NativeEngine;
    use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, Model};
    use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
    use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
    use norn_cell_do::questions::DerivedQuestions;
    use norn_cell_do::server::{self, Response};
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::{Value, json};

    const SCOPE: &str = "orgId=org_demo&environmentId=dev";
    const ADMIN: Option<&str> = Some("Bearer fixture:admin@demo");
    const T0: u64 = 1_794_484_800_000;
    const SANDBOX_HOSTS: &str = "erpnext-rest,odoo-rest";

    const ERPNEXT_CONFIG: &str = r#"[
  {"id": "erpnext.rest", "system": "erpnext", "label": "ERPNext REST",
   "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
   "instance": "sandbox", "baseUrl": "http://erpnext-rest:8000",
   "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"],
   "headers": {"X-Frappe-Site-Name": "frontend"}}
]"#;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn load_artifact(path: &str) -> ArtifactEnvelope {
        serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
    }

    fn catalog_artifacts(systems: &[&str]) -> Vec<ArtifactEnvelope> {
        let mut set: Vec<ArtifactEnvelope> = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/demo/explorer/three-systems.definitions.json"),
            )
            .expect("retained catalog"),
        )
        .expect("artifacts");
        for system in systems {
            for kind in ["source", "mapping", "connector"] {
                set.push(load_artifact(&format!(
                    "fixtures/packs/{system}-rest/artifacts/{system}.rest.{kind}.json"
                )));
            }
        }
        if !systems.is_empty() {
            for name in [
                "source.Synced.interface",
                "source.Connection.entity",
                "source.SyncRun.entity",
                "source.Upload.entity",
                "norn.connect.source",
                "norn.connect.mapping",
            ] {
                set.push(load_artifact(&format!(
                    "registry/packs/sources/artifacts/{name}.json"
                )));
            }
        }
        set
    }

    fn open_hosted(systems: &[&str]) -> (DevHost<NativeEngine>, FixturePrincipals, CellScope) {
        let artifacts = catalog_artifacts(systems);
        let (base, candidate): (Vec<_>, Vec<_>) = artifacts
            .iter()
            .cloned()
            .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
        let to_rv = |items: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
            items
                .iter()
                .map(|a| {
                    RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("rv")
                })
                .collect()
        };
        let sources = ReceiptSources::new(&to_rv(&candidate), &to_rv(&base)).expect("sources");
        let checked = check_artifact_set_with_receipt(
            &sources,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("checked");
        assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
        let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
        for artifact in artifacts
            .iter()
            .filter(|a| a.kind() == ArtifactKind::Entity)
        {
            let root: EntityRoot = serde_json::from_value(
                serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
            )
            .expect("entity root");
            match root {
                EntityRoot::V2(root) => objects.push(root.object_type),
                EntityRoot::V2Interface(root) => interfaces.push(root.interface),
                EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
                EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {}
            }
        }
        let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
            .expect("model")
            .with_governing_laws(&artifacts)
            .expect("laws");
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let input = InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                RuntimeValue::from_json(serde_json::to_value(&artifacts).expect("set"))
                    .expect("rv")
                    .canonical_json()
                    .expect("canonical"),
            )
            .expect("utf8"),
            registry: vec![],
            receipt_sha256: checked.receipt.content_sha256().expect("digest"),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().expect("receipt"))
                    .expect("utf8"),
            ),
            readmission: None,
            diff: json!({}),
            installed_at: 1,
            layout: None,
        };
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        let policy =
            ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
        let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
        let host = DevHost::from_store(store, scope.clone(), |_| BTreeMap::new())
            .expect("host")
            .with_authorizer(policy);
        (host, principals, scope)
    }

    fn call_http(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        questions: &dyn AdHocQuestions<NativeEngine>,
        method: &'static str,
        path: &'static str,
        body: &str,
    ) -> Response {
        server::handle_with(
            host,
            principals,
            Some(questions),
            &server::Request {
                method,
                path,
                query: SCOPE,
                authorization: ADMIN,
                now: 0,
                body,
            },
        )
    }

    fn metric_name_for(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        questions: &dyn AdHocQuestions<NativeEngine>,
        object_type: &str,
        expression: &str,
    ) -> String {
        let listed = call_http(
            host,
            principals,
            questions,
            "GET",
            "/v1/questions:metrics",
            "",
        );
        assert_eq!(listed.status, 200, "{}", listed.body);
        let doc: Value = serde_json::from_str(&listed.body).unwrap();
        doc["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["objectType"] == object_type && m["expression"] == expression)
            .unwrap_or_else(|| panic!("missing derived metric {object_type}.{expression}"))["name"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn holding_question(metric: &str, max_age_ms: u64) -> Value {
        json!({
            "irVersion": "0.1",
            "name": "freshness.holding_qty",
            "objectSet": "canon.Holding",
            "aggregate": [{"metric": metric}],
            "groupBy": [{"path": {"property": "observed_at"}, "grain": "day"}],
            "window": {"start": "2026-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": max_age_ms},
        })
    }

    fn delivery_question(metric: &str, max_age_ms: u64) -> Value {
        json!({
            "irVersion": "0.1",
            "name": "freshness.delivery_qty",
            "objectSet": "canon.Delivery",
            "aggregate": [{"metric": metric}],
            "groupBy": [{"path": {"property": "occurred_at"}, "grain": "day"}],
            "window": {"start": "2026-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": max_age_ms},
        })
    }

    fn sync_erpnext(host: &DevHost<NativeEngine>, scope: &CellScope, now_ms: u64) {
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        let profile = PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("profile");
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/connectors/rest/erpnext.responses.json"),
            )
            .expect("responses"),
        )
        .expect("json");
        let mut pages: BTreeMap<String, (u16, Value)> = doc["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["dataset"].as_str().unwrap().to_owned(),
                    (
                        u16::try_from(p["status"].as_u64().unwrap()).unwrap(),
                        p["body"].clone(),
                    ),
                )
            })
            .collect();
        let secrets = BTreeMap::from([(
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        )]);
        let mut sources =
            Sources::with_allowed_hosts(scope, ERPNEXT_CONFIG, &secrets, SANDBOX_HOSTS)
                .expect("src");
        sources
            .claim("erpnext.rest", Trigger::Manual, "user:admin@demo", now_ms)
            .expect("claim");
        let mut step = sources.step(host, "erpnext.rest", None, now_ms);
        let mut t = now_ms;
        loop {
            match step {
                Step::Request(req) => {
                    let feed = profile
                        .feeds
                        .iter()
                        .find(|f| req.url.contains(f.request.path.as_str()))
                        .expect("feed");
                    let (status, body) = pages
                        .remove(&feed.dataset)
                        .unwrap_or_else(|| (200, json!({"data": []})));
                    t = t.saturating_add(1);
                    step = sources.step(
                        host,
                        "erpnext.rest",
                        Some((status, body.to_string().as_bytes())),
                        t,
                    );
                }
                Step::Continue(_) => {
                    t = t.saturating_add(1);
                    step = sources.step(host, "erpnext.rest", None, t);
                }
                Step::Done(summary) => {
                    assert_eq!(summary.outcome, "ok", "{summary:#?}");
                    break;
                }
            }
        }
    }

    #[test]
    fn native_only_questions_remain_exact_with_empty_source_cursors() {
        let (host, principals, scope) = open_hosted(&[]);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let question = holding_question(&metric, 60_000);
        let resp = call_http(
            &host,
            &principals,
            &runtime,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": question }).to_string(),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["freshness"], "exact");
        assert_eq!(body["complete"], true);
        assert_eq!(body["sourceCursors"], json!({}));
    }

    #[test]
    fn unsynced_source_backed_question_is_incomplete_across_cell_ivm_and_http() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let holding_metric =
            metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let delivery_metric =
            metric_name_for(&host, &principals, &runtime, "canon.Delivery", "quantity");

        // Unmapped native type (`canon.Delivery`) in the same cell stays `exact`.
        let delivery_resp = call_http(
            &host,
            &principals,
            &runtime,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": delivery_question(&delivery_metric, 60_000) }).to_string(),
        );
        assert_eq!(delivery_resp.status, 200, "{}", delivery_resp.body);
        let delivery_body: Value = serde_json::from_str(&delivery_resp.body).unwrap();
        assert_eq!(delivery_body["freshness"], "exact");
        assert_eq!(delivery_body["complete"], true);

        // Mapped type (`canon.Holding`) before any connector sync MUST be `incomplete`.
        let q = holding_question(&holding_metric, 60_000);
        let http_resp = call_http(
            &host,
            &principals,
            &runtime,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": q }).to_string(),
        );
        assert_eq!(http_resp.status, 200, "{}", http_resp.body);
        let http_body: Value = serde_json::from_str(&http_resp.body).unwrap();
        assert_eq!(http_body["freshness"], "incomplete");
        assert_eq!(http_body["complete"], false);

        // Direct CellStore::evaluate_question and IncrementalQuery (IVM) with empty
        // source_cursors also fail closed to Incomplete.
        let plan = runtime
            .admit(&serde_json::from_value(q).unwrap(), 100)
            .expect("plan");
        let cell = host.cell().expect("cell");
        let store = cell.store();
        let required = store
            .required_sources_for_plan(MAIN_WORLD, &plan)
            .expect("required");
        assert_eq!(
            required,
            RequiredSources(BTreeMap::from([(
                "erpnext.rest.mapping".into(),
                BTreeSet::from(["Warehouse Lot Balance".into()]),
            )]))
        );
        let empty_ctx = QueryContext {
            world: MAIN_WORLD.into(),
            scope: scope.key(),
            population_pin: "test".into(),
            authorization_epoch: 1,
            mapping_pin: "test".into(),
            now_ms: T0,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        };
        let exact = store
            .evaluate_question(&plan, empty_ctx.clone(), |_| Ok(true))
            .expect("eval");
        assert_eq!(exact.freshness, Freshness::Incomplete);
        assert!(!exact.complete);
        let mut ivm =
            IncrementalQuery::open(store, plan, empty_ctx.clone(), |_| Ok(true)).expect("ivm");
        assert_eq!(ivm.result().freshness, Freshness::Incomplete);
        assert!(!ivm.result().complete);
        ivm.refresh(store, empty_ctx, |_| Ok(true))
            .expect("refresh");
        assert_eq!(ivm.result().freshness, Freshness::Incomplete);
        assert!(!ivm.result().complete);
    }

    fn run_failing_erpnext_sync(host: &DevHost<NativeEngine>, scope: &CellScope, t_fail: u64) {
        let secrets = BTreeMap::from([(
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        )]);
        let mut failing =
            Sources::with_allowed_hosts(scope, ERPNEXT_CONFIG, &secrets, SANDBOX_HOSTS)
                .expect("src");
        failing
            .claim("erpnext.rest", Trigger::Manual, "user:admin@demo", t_fail)
            .expect("claim");
        let Step::Request(_) = failing.step(host, "erpnext.rest", None, t_fail) else {
            panic!("expected request");
        };
        let Step::Done(failed_summary) =
            failing.step(host, "erpnext.rest", Some((500, b"boom")), t_fail + 1)
        else {
            panic!("expected failed sync");
        };
        assert_eq!(failed_summary.outcome, "failed");
    }

    fn eval_question(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        derived: &DerivedQuestions,
        question: &Value,
    ) -> Value {
        let runtime = derived.questions(host).expect("runtime");
        let resp = call_http(
            host,
            principals,
            &runtime,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": question }).to_string(),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        serde_json::from_str(&resp.body).unwrap()
    }

    #[test]
    fn synced_connector_reports_exact_fresh_stale_and_incomplete_on_failure() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let q = holding_question(&metric, 60_000);

        // 1. Immediately after a clean sync, freshness is `exact` with real cursors
        //    and watermarks, and `:trace` succeeds on the deterministic lineageRef.
        let exact_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(exact_body["freshness"], "exact", "{exact_body}");
        assert_eq!(exact_body["complete"], true);
        assert!(
            exact_body["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "sourceCursors must carry durable cursor: {exact_body}"
        );
        assert_eq!(
            exact_body["watermarks"]["erpnext.rest"]["Warehouse Lot Balance"]["complete"],
            true
        );
        let bucket = exact_body["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .expect("at least one synced holding bucket")
            .clone();
        let trace_resp = call_http(
            &host,
            &principals,
            &runtime,
            "POST",
            "/v1/questions:trace",
            &json!({
                "question": q,
                "lineageRef": exact_body["lineageRef"],
                "bucket": bucket,
            })
            .to_string(),
        );
        assert_eq!(trace_resp.status, 200, "{}", trace_resp.body);

        // 2. Advance cell clock by 5s (<= 60s maxAgeMs) -> `fresh`.
        let heartbeat =
            exact_body["watermarks"]["erpnext.rest"]["Warehouse Lot Balance"]["heartbeatMs"]
                .as_u64()
                .expect("heartbeatMs");
        host.install_artifacts(&[], heartbeat.saturating_add(5_000))
            .expect("advance clock +5s");
        let fresh_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(fresh_body["freshness"], "fresh", "{fresh_body}");
        assert_eq!(fresh_body["complete"], true);

        // 3. Advance cell clock by 120s (> 60s maxAgeMs) -> `stale`.
        host.install_artifacts(&[], heartbeat.saturating_add(120_000))
            .expect("advance clock +120s");
        let stale_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(stale_body["freshness"], "stale", "{stale_body}");
        assert_eq!(stale_body["complete"], true);

        // 4. Failed sync transitions `source.Connection.status` to `failed` -> `incomplete`.
        run_failing_erpnext_sync(&host, &scope, heartbeat.saturating_add(130_000));
        let failed_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(failed_body["freshness"], "incomplete", "{failed_body}");
        assert_eq!(failed_body["complete"], false);
    }

    #[test]
    fn multi_source_cell_stays_incomplete_until_all_required_sources_sync() {
        let (host, principals, scope) = open_hosted(&["erpnext", "odoo"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let q = holding_question(&metric, 60_000);
        let resp = call_http(
            &host,
            &principals,
            &runtime,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": q }).to_string(),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body["freshness"], "incomplete",
            "missing odoo.rest.mapping cursor must keep canon.Holding incomplete: {body}"
        );
        assert_eq!(body["complete"], false);
        assert!(
            body["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"]
                .as_str()
                .is_some(),
            "synced erpnext.rest cursor should still be reported: {body}"
        );
    }
}
