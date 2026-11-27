//! QUERY-FRESH-1 (Lane B6): model-derived `RequiredSources`, real connector
//! cursors/watermarks/heartbeats in `DerivedRuntime::snapshot`, idle-cell
//! wall-clock aging, `Sources`-grant cursor disclosure, historical/pinned
//! `:trace` frontier reconstruction, multi-connection coverage, and strict
//! legacy watermark parsing.

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
    use norn_cell::compute::ivm::IncrementalQuestion;
    use norn_cell::compute::{Freshness, QueryContext, RequiredSources};
    use norn_cell::engine::NativeEngine;
    use norn_cell::{
        CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model,
    };
    use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
    use norn_cell_do::principal::FixturePrincipals;
    use norn_cell_do::questions::DerivedQuestions;
    use norn_cell_do::server::{self, Response};
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::{Value, json};

    const SCOPE: &str = "orgId=org_demo&environmentId=dev";
    const ADMIN: Option<&str> = Some("Bearer fixture:admin@demo");
    const MEMBER: Option<&str> = Some("Bearer fixture:member@demo");
    const T0: u64 = 1_794_484_800_000;
    const SANDBOX_HOSTS: &str = "erpnext-rest,odoo-rest";

    const TEST_FIXTURES: &[(&str, &[&str])] = &[
        ("admin@demo", &["admin"]),
        ("member@demo", &["member"]),
        ("guest@demo", &["viewer"]),
    ];

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

    fn open_store(systems: &[&str]) -> (CellStore<NativeEngine>, CellScope) {
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
        (store, scope)
    }

    fn wrap_host(
        store: CellStore<NativeEngine>,
        scope: CellScope,
    ) -> (DevHost<NativeEngine>, FixturePrincipals, CellScope) {
        let policy =
            ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
        let principals = FixturePrincipals::new(TEST_FIXTURES, policy.caller());
        let host = DevHost::from_store(store, scope.clone(), |_| BTreeMap::new())
            .expect("host")
            .with_authorizer(policy);
        (host, principals, scope)
    }

    fn open_hosted(systems: &[&str]) -> (DevHost<NativeEngine>, FixturePrincipals, CellScope) {
        let (store, scope) = open_store(systems);
        wrap_host(store, scope)
    }

    fn call_http_as(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        questions: &dyn AdHocQuestions<NativeEngine>,
        auth: Option<&str>,
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
                authorization: auth,
                now: 0,
                body,
            },
        )
    }

    fn call_http(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        questions: &dyn AdHocQuestions<NativeEngine>,
        method: &'static str,
        path: &'static str,
        body: &str,
    ) -> Response {
        call_http_as(host, principals, questions, ADMIN, method, path, body)
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

    fn erpnext_pages(bump_modified: Option<&str>) -> BTreeMap<String, (u16, Value)> {
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/connectors/rest/erpnext.responses.json"),
            )
            .expect("responses"),
        )
        .expect("json");
        doc["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let mut body = p["body"].clone();
                if let Some(new_modified) = bump_modified
                    && let Some(rows) = body.get_mut("data").and_then(Value::as_array_mut)
                {
                    for row in rows {
                        if let Some(obj) = row.as_object_mut()
                            && obj.contains_key("modified")
                        {
                            obj.insert("modified".into(), Value::String(new_modified.to_owned()));
                        }
                    }
                }
                (
                    p["dataset"].as_str().unwrap().to_owned(),
                    (u16::try_from(p["status"].as_u64().unwrap()).unwrap(), body),
                )
            })
            .collect()
    }

    fn sync_erpnext_with(
        host: &DevHost<NativeEngine>,
        scope: &CellScope,
        now_ms: u64,
        mut pages: BTreeMap<String, (u16, Value)>,
    ) {
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        let profile = PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("profile");
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
                Step::Continue => {
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

    fn sync_erpnext(host: &DevHost<NativeEngine>, scope: &CellScope, now_ms: u64) {
        sync_erpnext_with(host, scope, now_ms, erpnext_pages(None));
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

    fn eval_question_at(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        derived: &DerivedQuestions,
        auth: Option<&str>,
        now_ms: u64,
        question: &Value,
    ) -> Value {
        let runtime = derived.questions_at(host, now_ms).expect("runtime");
        let resp = call_http_as(
            host,
            principals,
            &runtime,
            auth,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": question }).to_string(),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        serde_json::from_str(&resp.body).unwrap()
    }

    fn eval_question(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        derived: &DerivedQuestions,
        question: &Value,
    ) -> Value {
        eval_question_at(host, principals, derived, ADMIN, 0, question)
    }

    #[test]
    fn native_only_questions_remain_exact_with_empty_source_cursors() {
        let (host, principals, scope) = open_hosted(&[]);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let body = eval_question(
            &host,
            &principals,
            &derived,
            &holding_question(&metric, 60_000),
        );
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
        let delivery_body = eval_question(
            &host,
            &principals,
            &derived,
            &delivery_question(&delivery_metric, 60_000),
        );
        assert_eq!(delivery_body["freshness"], "exact");
        assert_eq!(delivery_body["complete"], true);

        // Mapped type (`canon.Holding`) before any connector sync MUST be `incomplete`.
        let q = holding_question(&holding_metric, 60_000);
        let http_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(http_body["freshness"], "incomplete");
        assert_eq!(http_body["complete"], false);

        // Direct CellStore::evaluate_question and IncrementalQuestion (IVM) with
        // empty source_cursors also fail closed to Incomplete.
        let plan = runtime
            .admit(&serde_json::from_value(q).unwrap(), 100)
            .expect("plan");
        let (store, store_scope) = open_store(&["erpnext"]);
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
            scope: store_scope.key(),
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
        let mut ivm = IncrementalQuestion::rebuild(&store, &plan, empty_ctx.clone(), |_| Ok(true))
            .expect("ivm");
        assert_eq!(ivm.result().freshness, Freshness::Incomplete);
        assert!(!ivm.result().complete);
        ivm.advance(&store, empty_ctx, |_| Ok(true))
            .expect("advance");
        assert_eq!(ivm.result().freshness, Freshness::Incomplete);
        assert!(!ivm.result().complete);
    }

    #[test]
    fn synced_connector_ages_on_idle_cell_and_turns_incomplete_on_failure() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let q = holding_question(&metric, 60_000);

        // 1. Immediately after a clean sync, freshness is `exact` with real cursors.
        let exact_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(exact_body["freshness"], "exact", "{exact_body}");
        assert_eq!(exact_body["complete"], true);
        let heartbeat =
            exact_body["watermarks"]["erpnext.rest"]["Warehouse Lot Balance"]["heartbeatMs"]
                .as_u64()
                .expect("heartbeatMs");

        // 2. Idle cell with ZERO new commits: advancing request clock ages `exact` -> `fresh` -> `stale`.
        let fresh_body =
            eval_question_at(&host, &principals, &derived, ADMIN, heartbeat + 5_000, &q);
        assert_eq!(fresh_body["freshness"], "fresh", "{fresh_body}");
        assert_eq!(fresh_body["complete"], true);

        let stale_body =
            eval_question_at(&host, &principals, &derived, ADMIN, heartbeat + 120_000, &q);
        assert_eq!(stale_body["freshness"], "stale", "{stale_body}");
        assert_eq!(stale_body["complete"], true);

        // 3. Failed sync transitions `source.Connection.status` to `failed` -> `incomplete`.
        run_failing_erpnext_sync(&host, &scope, heartbeat + 130_000);
        let failed_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(failed_body["freshness"], "incomplete", "{failed_body}");
        assert_eq!(failed_body["complete"], false);
    }

    #[test]
    fn cursor_disclosure_requires_sources_grant_on_answer_and_trace() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let q = holding_question(&metric, 60_000);

        // Admin holds `Sources` -> sees raw connector cursor JSON.
        let admin_ans = eval_question_at(&host, &principals, &derived, ADMIN, 0, &q);
        let admin_cursor = admin_ans["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"]
            .as_str()
            .expect("admin cursor");
        assert!(admin_cursor.contains("\"version\""), "{admin_cursor}");
        assert_eq!(
            admin_ans["watermarks"]["erpnext.rest"]["Warehouse Lot Balance"]["observed"].as_str(),
            Some(admin_cursor)
        );

        // Member holds `Values` ONLY (no `Sources`) -> receives 64-char keyed hex handles,
        // never raw upstream record IDs or versions, on both answer and trace.
        let member_ans = eval_question_at(&host, &principals, &derived, MEMBER, 0, &q);
        assert_eq!(member_ans["freshness"], "exact", "{member_ans}");
        assert_eq!(member_ans["complete"], true);
        let member_cursor = member_ans["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"]
            .as_str()
            .expect("member handle");
        assert_ne!(member_cursor, admin_cursor);
        assert_eq!(member_cursor.len(), 64, "{member_cursor}");
        assert!(
            member_cursor
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "expected lowercase hex handle: {member_cursor}"
        );
        assert_eq!(
            member_ans["watermarks"]["erpnext.rest"]["Warehouse Lot Balance"]["observed"].as_str(),
            Some(member_cursor)
        );
        let bucket = member_ans["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .expect("bucket")
            .clone();
        let trace_resp = call_http_as(
            &host,
            &principals,
            &runtime,
            MEMBER,
            "POST",
            "/v1/questions:trace",
            &json!({
                "question": q,
                "lineageRef": member_ans["lineageRef"],
                "bucket": bucket,
            })
            .to_string(),
        );
        assert_eq!(trace_resp.status, 200, "{}", trace_resp.body);
        let trace_body: Value = serde_json::from_str(&trace_resp.body).unwrap();
        assert_eq!(
            trace_body["result"]["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"].as_str(),
            Some(member_cursor)
        );
        assert_eq!(
            trace_body["result"]["watermarks"]["erpnext.rest"]["Warehouse Lot Balance"]["observed"]
                .as_str(),
            Some(member_cursor)
        );
    }

    #[test]
    fn pinned_trace_stays_valid_after_subsequent_sync_advances_cursors() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let q = holding_question(&metric, 60_000);

        let first = eval_question(&host, &principals, &derived, &q);
        assert_eq!(first["freshness"], "exact", "{first}");
        let first_cursor = first["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"]
            .as_str()
            .expect("first cursor")
            .to_owned();
        let first_frontier = first["knownAsOf"].as_u64().expect("knownAsOf");
        let first_now = first["nowMs"].as_u64().expect("nowMs");
        let bucket = first["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .expect("bucket")
            .clone();

        // Second sync advances upstream modified timestamps, live cursors, and heartbeat.
        sync_erpnext_with(
            &host,
            &scope,
            T0 + 30_000,
            erpnext_pages(Some("2026-11-12 18:00:00.000000")),
        );
        let second = eval_question(&host, &principals, &derived, &q);
        assert_eq!(second["freshness"], "exact", "{second}");
        assert_ne!(
            second["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"].as_str(),
            Some(first_cursor.as_str())
        );

        // Pinned `:trace` against the first answer's (knownAsOf, nowMs) reconstructs
        // the historical connector frontier and still succeeds!
        let runtime2 = derived.questions(&host).expect("runtime2");
        let pinned_trace = call_http(
            &host,
            &principals,
            &runtime2,
            "POST",
            "/v1/questions:trace",
            &json!({
                "question": q,
                "lineageRef": first["lineageRef"],
                "bucket": bucket,
                "knownAsOf": first_frontier,
                "nowMs": first_now,
            })
            .to_string(),
        );
        assert_eq!(pinned_trace.status, 200, "{}", pinned_trace.body);
        let trace_doc: Value = serde_json::from_str(&pinned_trace.body).unwrap();
        assert_eq!(
            trace_doc["result"]["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"].as_str(),
            Some(first_cursor.as_str())
        );
    }

    #[test]
    fn multi_source_and_configured_secondary_connection_stay_incomplete_until_synced() {
        // Case 1: Two distinct mappings (`erpnext` + `odoo`); only `erpnext` synced.
        let (host, principals, scope) = open_hosted(&["erpnext", "odoo"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let q = holding_question(&metric, 60_000);
        let body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(body["freshness"], "incomplete", "{body}");
        assert_eq!(body["complete"], false);
        assert!(
            body["sourceCursors"]["erpnext.rest"]["Warehouse Lot Balance"]
                .as_str()
                .is_some()
        );

        // Case 2: Two configured connections sharing ONE mapping (`erpnext.rest.mapping`).
        // Even after `erpnext.rest` syncs cleanly, the unsynced `erpnext.secondary` keeps
        // the answer `incomplete`.
        let (host2, principals2, scope2) = open_hosted(&["erpnext"]);
        sync_erpnext(&host2, &scope2, T0);
        let derived2 =
            DerivedQuestions::new([7; 32], scope2).with_configured_connections(BTreeMap::from([
                ("erpnext.rest".into(), "erpnext.rest.mapping".into()),
                ("erpnext.secondary".into(), "erpnext.rest.mapping".into()),
            ]));
        let runtime2 = derived2.questions(&host2).expect("runtime2");
        let metric2 = metric_name_for(&host2, &principals2, &runtime2, "canon.Holding", "quantity");
        let body2 = eval_question(
            &host2,
            &principals2,
            &derived2,
            &holding_question(&metric2, 60_000),
        );
        assert_eq!(body2["freshness"], "incomplete", "{body2}");
        assert_eq!(body2["complete"], false);
    }

    #[test]
    fn corrupt_or_null_legacy_watermark_is_rejected_as_incomplete() {
        let (mut store, scope) = open_store(&["erpnext"]);
        let values = BTreeMap::from([
            ("id".into(), RuntimeValue::from("erpnext.legacy")),
            ("system".into(), RuntimeValue::from("erpnext")),
            ("label".into(), RuntimeValue::from("Legacy ERPNext")),
            (
                "target".into(),
                RuntimeValue::from("http://erpnext-rest:8000"),
            ),
            ("auth".into(), RuntimeValue::from("api_key")),
            ("status".into(), RuntimeValue::from("ok")),
            (
                "last_sync_started_at".into(),
                RuntimeValue::from("2026-11-12T12:00:00Z"),
            ),
            (
                "last_sync_ended_at".into(),
                RuntimeValue::from("2026-11-12T12:00:00.000Z"),
            ),
            ("last_trigger".into(), RuntimeValue::from("manual")),
            (
                "watermark".into(),
                RuntimeValue::from(r#"{"Warehouse Lot Balance": null}"#),
            ),
            ("rows_read".into(), RuntimeValue::from(0_i64)),
            ("rows_landed".into(), RuntimeValue::from(0_i64)),
            ("rows_quarantined".into(), RuntimeValue::from(0_i64)),
            ("quarantine_reasons".into(), RuntimeValue::from("{}")),
            (
                "connector_id".into(),
                RuntimeValue::from("erpnext.rest.connector"),
            ),
            (
                "source_schema_id".into(),
                RuntimeValue::from("erpnext.rest.source"),
            ),
            (
                "mapping_id".into(),
                RuntimeValue::from("erpnext.rest.mapping"),
            ),
        ]);
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: scope.key(),
                logical_time: T0,
                writes: vec![FactWriteRow::Put {
                    object_type: "source.Connection".into(),
                    key: "erpnext.legacy".into(),
                    values,
                }],
                ..CommitBatch::default()
            })
            .expect("commit legacy connection");
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.Holding", "quantity");
        let body = eval_question(
            &host,
            &principals,
            &derived,
            &holding_question(&metric, 60_000),
        );
        assert_eq!(body["freshness"], "incomplete", "{body}");
        assert_eq!(body["complete"], false);
    }
}
