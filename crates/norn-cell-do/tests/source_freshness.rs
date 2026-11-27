//! QUERY-FRESH-1 (Lane B6): model-derived `RequiredSources`, real connector
//! cursors/watermarks/heartbeats in `DerivedRuntime::snapshot`, idle-cell
//! wall-clock aging, `Sources`-grant scope-bound cursor disclosure,
//! historical/pinned `:trace` frontier reconstruction, production `Tenant::serve`
//! multi-connection coverage, and strict cursor validation.

#[path = "../../norn-cell/tests/support/governed.rs"]
mod governed;

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "QUERY-FRESH-1 end-to-end integration tests"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use super::governed::GovernedCommit as _;

    use norn_api::host::DevHost;
    use norn_api::pull::PullProfile;
    use norn_api::question_routes::AdHocQuestions;
    use norn_api::scope::CellScope;
    use norn_api::sources::{Sources, Step, Trigger};
    use norn_cell::compute::ivm::IncrementalQuestion;
    use norn_cell::compute::lineage::keyed_cursor_handle;
    use norn_cell::compute::{Freshness, QueryContext, RequiredSources};
    use norn_cell::engine::{NativeEngine, SqlEngine as _, SqlRow, SqlValue};
    use norn_cell::{
        CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model,
        StoreOptions,
    };
    use norn_cell_do::connect::Connect;
    use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
    use norn_cell_do::principal::FixturePrincipals;
    use norn_cell_do::questions::DerivedQuestions;
    use norn_cell_do::server::{self, Response};
    use norn_cell_do::tenant::{Served, Tenant};
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::{
        ArtifactEnvelope, ArtifactKind, Decimal, EntityRoot, ResourceLimitProfile, RuntimeValue,
    };
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::{Value, json};

    const SCOPE: &str = "orgId=org_demo&environmentId=dev";
    const ADMIN: Option<&str> = Some("Bearer fixture:admin@demo");
    const MEMBER: Option<&str> = Some("Bearer fixture:member@demo");
    const T0: u64 = 1_794_484_800_000;
    const SANDBOX_HOSTS: &str = "erpnext-rest,odoo-rest";
    const ORDER_DATASET: &str = "Sales Order Item";

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
        let mut set: Vec<ArtifactEnvelope> = serde_json::from_str::<Vec<ArtifactEnvelope>>(
            &std::fs::read_to_string(
                root().join("fixtures/demo/explorer/three-systems.definitions.json"),
            )
            .expect("retained catalog"),
        )
        .expect("artifacts")
        .into_iter()
        .filter(|a| a.kind() != ArtifactKind::Mapping)
        .collect();
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

    fn open_store_at(
        systems: &[&str],
        path: Option<&Path>,
    ) -> (CellStore<NativeEngine>, CellScope) {
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
        let mut store = match path {
            Some(db) => CellStore::open(db, model.clone(), StoreOptions::default()).expect("disk"),
            None => CellStore::open_in_memory(model.clone()).expect("store"),
        };
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        (store, scope)
    }

    fn open_store(systems: &[&str]) -> (CellStore<NativeEngine>, CellScope) {
        open_store_at(systems, None)
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

    fn order_line_question(metric: &str, max_age_ms: u64) -> Value {
        json!({
            "irVersion": "0.1",
            "name": "freshness.order_line_qty",
            "objectSet": "canon.OrderLine",
            "calendar": "iso_utc",
            "aggregate": [{"metric": metric}],
            "groupBy": [
                {"path": {"property": "due_at"}, "grain": "day"},
                {"path": {"property": "unit"}},
                {"path": {"property": "currency"}},
            ],
            "window": {"start": "2026-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": max_age_ms},
        })
    }

    fn delivery_question(metric: &str, max_age_ms: u64) -> Value {
        json!({
            "irVersion": "0.1",
            "name": "freshness.delivery_qty",
            "objectSet": "canon.Delivery",
            "calendar": "iso_utc",
            "aggregate": [{"metric": metric}],
            "groupBy": [
                {"path": {"property": "occurred_at"}, "grain": "day"},
                {"path": {"property": "unit"}},
                {"path": {"property": "currency"}},
            ],
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
        let Step::Done(failed_summary) = failing.step(
            host,
            "erpnext.rest",
            Some((500, b"boom")),
            t_fail.saturating_add(1),
        ) else {
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
    fn native_only_questions_remain_exact_and_trace_across_wall_clock_ticks() {
        let (mut store, scope) = open_store(&[]);
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: scope.key(),
                logical_time: T0,
                writes: vec![FactWriteRow::Put {
                    object_type: "canon.OrderLine".into(),
                    key: "order-native-1".into(),
                    values: BTreeMap::from([
                        ("status".into(), RuntimeValue::from("promised")),
                        ("due_at".into(), RuntimeValue::from("2026-08-06T12:00:00Z")),
                        (
                            "quantity".into(),
                            RuntimeValue::Number("5".parse::<Decimal>().unwrap()),
                        ),
                        ("unit".into(), RuntimeValue::from("g")),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .expect("commit native order line");
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope);
        let rt0 = derived.questions_at(&host, T0).expect("rt0");
        let metric = metric_name_for(&host, &principals, &rt0, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let body = eval_question_at(&host, &principals, &derived, ADMIN, T0, &q);
        assert_eq!(body["freshness"], "exact");
        assert_eq!(body["complete"], true);
        assert_eq!(body["sourceCursors"], json!({}));
        let bucket = body["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .expect("bucket")
            .clone();
        // Unpinned `:trace` 5 seconds later must still succeed on an unchanged native cell.
        let rt5 = derived
            .questions_at(&host, T0.saturating_add(5_000))
            .expect("rt5");
        let trace_resp = call_http(
            &host,
            &principals,
            &rt5,
            "POST",
            "/v1/questions:trace",
            &json!({
                "question": q,
                "lineageRef": body["lineageRef"],
                "bucket": bucket,
            })
            .to_string(),
        );
        assert_eq!(trace_resp.status, 200, "{}", trace_resp.body);
    }

    #[test]
    fn unsynced_source_backed_question_is_incomplete_across_cell_ivm_and_http() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let order_metric =
            metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
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

        // Mapped type (`canon.OrderLine`) before any connector sync MUST be `incomplete`.
        let q = order_line_question(&order_metric, 60_000);
        let http_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(http_body["freshness"], "incomplete");
        assert_eq!(http_body["complete"], false);

        // Direct CellStore::evaluate_question and IncrementalQuestion (IVM) with
        // empty source_cursors also fail closed to Incomplete.
        let plan = AdHocQuestions::<NativeEngine>::admit(
            &runtime,
            &serde_json::from_value(q).unwrap(),
            100,
        )
        .expect("plan");
        let (store, store_scope) = open_store(&["erpnext"]);
        let required = store
            .required_sources_for_plan(MAIN_WORLD, &plan)
            .expect("required");
        assert_eq!(
            required,
            RequiredSources(BTreeMap::from([(
                "erpnext.rest.mapping".into(),
                BTreeSet::from([ORDER_DATASET.into()]),
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
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);

        let exact_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(exact_body["freshness"], "exact", "{exact_body}");
        assert_eq!(exact_body["complete"], true);
        let heartbeat = exact_body["watermarks"]["erpnext.rest"][ORDER_DATASET]["heartbeatMs"]
            .as_u64()
            .expect("heartbeatMs");

        // Idle cell with ZERO new commits: advancing request clock ages `exact` -> `fresh` -> `stale`.
        let fresh_body = eval_question_at(
            &host,
            &principals,
            &derived,
            ADMIN,
            heartbeat.saturating_add(5_000),
            &q,
        );
        assert_eq!(fresh_body["freshness"], "fresh", "{fresh_body}");
        assert_eq!(fresh_body["complete"], true);

        let stale_body = eval_question_at(
            &host,
            &principals,
            &derived,
            ADMIN,
            heartbeat.saturating_add(120_000),
            &q,
        );
        assert_eq!(stale_body["freshness"], "stale", "{stale_body}");
        assert_eq!(stale_body["complete"], true);

        // Failed sync transitions `source.Connection.status` to `failed` -> `incomplete`.
        run_failing_erpnext_sync(&host, &scope, heartbeat.saturating_add(130_000));
        let failed_body = eval_question(&host, &principals, &derived, &q);
        assert_eq!(failed_body["freshness"], "incomplete", "{failed_body}");
        assert_eq!(failed_body["complete"], false);
    }

    #[test]
    fn cursor_disclosure_requires_sources_grant_and_is_scope_bound() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);

        let admin_ans = eval_question_at(&host, &principals, &derived, ADMIN, 0, &q);
        let admin_cursor = admin_ans["sourceCursors"]["erpnext.rest"][ORDER_DATASET]
            .as_str()
            .expect("admin cursor");
        assert!(admin_cursor.contains("\"version\""), "{admin_cursor}");
        assert_eq!(
            admin_ans["watermarks"]["erpnext.rest"][ORDER_DATASET]["observed"].as_str(),
            Some(admin_cursor)
        );

        let member_ans = eval_question_at(&host, &principals, &derived, MEMBER, 0, &q);
        assert_eq!(member_ans["freshness"], "exact", "{member_ans}");
        assert_eq!(member_ans["complete"], true);
        let member_cursor = member_ans["sourceCursors"]["erpnext.rest"][ORDER_DATASET]
            .as_str()
            .expect("member handle");
        assert_ne!(member_cursor, admin_cursor);
        assert_eq!(member_cursor.len(), 64, "{member_cursor}");
        assert!(
            member_cursor
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(
            member_ans["watermarks"]["erpnext.rest"][ORDER_DATASET]["observed"].as_str(),
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
            trace_body["result"]["sourceCursors"]["erpnext.rest"][ORDER_DATASET].as_str(),
            Some(member_cursor)
        );
        assert_eq!(
            trace_body["result"]["watermarks"]["erpnext.rest"][ORDER_DATASET]["observed"].as_str(),
            Some(member_cursor)
        );
        assert_eq!(
            member_cursor,
            keyed_cursor_handle(&[7; 32], "org_demo/dev", admin_cursor)
        );
        assert_ne!(
            member_cursor,
            keyed_cursor_handle(&[7; 32], "org_other/dev", admin_cursor)
        );
    }

    #[test]
    fn pinned_trace_stays_valid_after_subsequent_sync_advances_cursors() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);

        let first = eval_question(&host, &principals, &derived, &q);
        assert_eq!(first["freshness"], "exact", "{first}");
        let first_cursor = first["sourceCursors"]["erpnext.rest"][ORDER_DATASET]
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

        sync_erpnext_with(
            &host,
            &scope,
            T0.saturating_add(30_000),
            erpnext_pages(Some("2026-11-12 18:00:00.000000")),
        );
        let second = eval_question(&host, &principals, &derived, &q);
        assert_eq!(second["freshness"], "exact", "{second}");
        assert_ne!(
            second["sourceCursors"]["erpnext.rest"][ORDER_DATASET].as_str(),
            Some(first_cursor.as_str())
        );

        // Pinned `:trace` remains valid even after a second sync advances live
        // cursors AND even if a new connection is later added to config!
        let derived_later = derived.with_configured_connections(BTreeMap::from([
            ("erpnext.rest".into(), "erpnext.rest.mapping".into()),
            ("erpnext.added_later".into(), "erpnext.rest.mapping".into()),
        ]));
        let runtime2 = derived_later.questions(&host).expect("runtime2");
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
            trace_doc["result"]["sourceCursors"]["erpnext.rest"][ORDER_DATASET].as_str(),
            Some(first_cursor.as_str())
        );
    }

    #[test]
    fn multi_source_cell_stays_incomplete_until_all_required_sources_sync() {
        let (host, principals, scope) = open_hosted(&["erpnext", "odoo"]);
        sync_erpnext(&host, &scope, T0);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let body = eval_question(
            &host,
            &principals,
            &derived,
            &order_line_question(&metric, 60_000),
        );
        assert_eq!(body["freshness"], "incomplete", "{body}");
        assert_eq!(body["complete"], false);
        assert!(
            body["sourceCursors"]["erpnext.rest"][ORDER_DATASET]
                .as_str()
                .is_some()
        );
    }

    #[test]
    fn tenant_serve_enforces_configured_secondary_connection_via_connect() {
        let dir = std::env::temp_dir().join(format!(
            "norn-b6-tenant-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("cell.sqlite");
        let (disk, scope2) = open_store_at(&["erpnext"], Some(&db));
        let (seed_host, seed_principals, _) = wrap_host(disk, scope2.clone());
        sync_erpnext(&seed_host, &scope2, T0);
        let seed_derived = DerivedQuestions::new([7; 32], scope2.clone());
        let seed_rt = seed_derived.questions(&seed_host).expect("seed_rt");
        let metric = metric_name_for(
            &seed_host,
            &seed_principals,
            &seed_rt,
            "canon.OrderLine",
            "quantity",
        );
        drop(seed_host);

        let db_open = db.clone();
        let mut tenant = Tenant::open(
            scope2.clone(),
            &DEMO_GRANTS,
            Box::new(move || {
                NativeEngine::open(&db_open, StoreOptions::default()).map_err(|e| e.to_string())
            }),
            Some(&[7; 32]),
        )
        .expect("tenant open");
        let principals2 = FixturePrincipals::new(TEST_FIXTURES, tenant.caller());
        let two_conns = r#"[
  {"id": "erpnext.rest", "system": "erpnext", "label": "ERPNext Primary",
   "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
   "instance": "primary", "baseUrl": "http://erpnext-rest:8000",
   "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"]},
  {"id": "erpnext.secondary", "system": "erpnext", "label": "ERPNext Secondary",
   "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
   "instance": "secondary", "baseUrl": "http://erpnext-rest:8000",
   "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"]}
]"#;
        let vars = json!({
            "NORN_SECRET_ERPNEXT_REST_TOKEN": "key:secret",
            "NORN_SOURCE_ALLOWED_HOSTS": SANDBOX_HOSTS,
        })
        .to_string();
        let mut connect = Connect::new(&scope2, two_conns, &vars).expect("connect");
        let eval_body = json!({ "question": order_line_question(&metric, 60_000) }).to_string();
        let Served::Cell(resp) = tenant.serve(
            Some(&mut connect),
            &principals2,
            &server::Request {
                method: "POST",
                path: "/v1/questions:evaluate",
                query: SCOPE,
                authorization: ADMIN,
                now: T0 / 1_000,
                body: &eval_body,
            },
            T0,
        ) else {
            panic!("expected cell response");
        };
        assert_eq!(resp.status, 200, "{}", resp.body);
        let doc: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(doc["freshness"], "incomplete", "{doc}");
        assert_eq!(doc["complete"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn commit_connection(
        store: &mut CellStore<NativeEngine>,
        scope: &CellScope,
        mapping_id: &str,
        watermark: &str,
    ) {
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
            ("watermark".into(), RuntimeValue::from(watermark)),
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
            ("mapping_id".into(), RuntimeValue::from(mapping_id)),
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
    }

    #[test]
    fn corrupt_or_object_id_legacy_watermark_is_rejected_as_incomplete() {
        for (mapping_id, bad_watermark) in [
            ("erpnext.rest.mapping", r#"{"Sales Order Item": null}"#),
            (
                "erpnext.rest.mapping",
                r#"{"Sales Order Item": {"version": "2026-11-12", "id": {"nested": 1}}}"#,
            ),
            // Connection report stamped for a different mapping cannot satisfy
            // a rebound mapping even if the dataset name and cursor shape match.
            (
                "other.old.mapping",
                r#"{"Sales Order Item": {"version": "2026-11-12", "id": "SO-1"}}"#,
            ),
        ] {
            let (mut store, scope) = open_store(&["erpnext"]);
            commit_connection(&mut store, &scope, mapping_id, bad_watermark);
            let (host, principals, scope) = wrap_host(store, scope);
            let derived = DerivedQuestions::new([7; 32], scope).with_configured_connections(
                BTreeMap::from([("erpnext.legacy".into(), "erpnext.rest.mapping".into())]),
            );
            let runtime = derived.questions(&host).expect("runtime");
            let metric =
                metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
            let body = eval_question(
                &host,
                &principals,
                &derived,
                &order_line_question(&metric, 60_000),
            );
            assert_eq!(body["freshness"], "incomplete", "{bad_watermark}: {body}");
            assert_eq!(body["complete"], false);
        }
    }

    fn sync_named_erpnext(
        host: &DevHost<NativeEngine>,
        scope: &CellScope,
        conn_id: &str,
        now_ms: u64,
        mut pages: BTreeMap<String, (u16, Value)>,
        expected_outcome: &str,
    ) {
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        let profile = PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("profile");
        let config = json!([{
            "id": conn_id, "system": "erpnext", "label": conn_id,
            "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
            "instance": "sandbox", "baseUrl": "http://erpnext-rest:8000",
            "orgId": "org_demo", "environmentId": "dev",
            "secretRefs": ["erpnext_rest_token"],
        }])
        .to_string();
        let secrets = BTreeMap::from([(
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        )]);
        let mut sources =
            Sources::with_allowed_hosts(scope, &config, &secrets, SANDBOX_HOSTS).expect("src");
        sources
            .claim(conn_id, Trigger::Manual, "user:admin@demo", now_ms)
            .expect("claim");
        let mut step = sources.step(host, conn_id, None, now_ms);
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
                        conn_id,
                        Some((status, body.to_string().as_bytes())),
                        t,
                    );
                }
                Step::Continue => {
                    t = t.saturating_add(1);
                    step = sources.step(host, conn_id, None, t);
                }
                Step::Done(summary) => {
                    assert_eq!(summary.outcome, expected_outcome, "{summary:#?}");
                    break;
                }
            }
        }
    }

    fn capture_same_ms_order_quarantine(host: &DevHost<NativeEngine>, conn_id: &str, now_ms: u64) {
        let conn = host
            .host_object("source.Connection", conn_id)
            .expect("read")
            .expect("conn");
        let report_ts = conn
            .get("last_sync_ended_at")
            .and_then(RuntimeValue::as_str)
            .expect("ended")
            .to_owned();
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        let profile = PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("profile");
        let raw = norn_api::pull::observation(
            &profile,
            &norn_api::pull::Row {
                dataset: ORDER_DATASET.into(),
                source_id: "SOI-BAD-SAME-MS".into(),
                version: "2026-11-12 23:59:59".into(),
                payload: norn_ir::runtime_object! {
                    "name" => "SOI-BAD-SAME-MS", "parent" => RuntimeValue::Null
                },
            },
            now_ms,
        )
        .expect("obs");
        let input = norn_cell::mapping_bundle::MappingInput {
            mapping: "erpnext.rest.mapping".into(),
            dataset: ORDER_DATASET.into(),
            connector_instance: "sandbox".into(),
            partition: ORDER_DATASET.into(),
            recorded_at: norn_ir::provenance::UtcTimestamp(report_ts),
        };
        let prev_cursor = host
            .source_cursors(conn_id)
            .expect("cursors")
            .get(ORDER_DATASET)
            .map(|c| c.cursor_json.clone());
        let lease = host
            .acquire_source_lease(
                conn_id,
                "run-q-same-ms",
                now_ms,
                norn_api::sources::RUN_LEASE_MS,
            )
            .expect("lease");
        let adv = norn_cell::source_cursor::CursorAdvanceSpec {
            connection_id: conn_id.into(),
            dataset: ORDER_DATASET.into(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms,
            expected_previous_cursor: prev_cursor,
            next_cursor: Some(r#"{"version":"2026-11-12 23:59:59","id":"SOI-BAD-SAME-MS"}"#.into()),
            renew_ttl_ms: Some(norn_api::sources::RUN_LEASE_MS),
        };
        let ingested = host
            .ingest_source_page(&[(raw, input)], &adv)
            .expect("quarantine");
        assert!(matches!(
            ingested[0].outcome,
            norn_cell::MappingOutcome::Quarantined(_)
        ));
        host.release_source_lease(conn_id, &lease.lease_id, lease.generation)
            .expect("release");
    }

    #[test]
    fn same_ms_post_answer_quarantine_preserves_pinned_trace_and_sibling_connection() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        sync_named_erpnext(&host, &scope, "erpnext.a", T0, erpnext_pages(None), "ok");
        sync_named_erpnext(&host, &scope, "erpnext.b", T0, erpnext_pages(None), "ok");
        let derived = DerivedQuestions::new([7; 32], scope.clone()).with_configured_connections(
            BTreeMap::from([
                ("erpnext.a".into(), "erpnext.rest.mapping".into()),
                ("erpnext.b".into(), "erpnext.rest.mapping".into()),
            ]),
        );
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let first = eval_question(&host, &principals, &derived, &q);
        assert_eq!(first["freshness"], "exact", "{first}");
        assert_eq!(
            first["watermarks"]["erpnext.a"][ORDER_DATASET]["complete"],
            true
        );
        assert_eq!(
            first["watermarks"]["erpnext.b"][ORDER_DATASET]["complete"],
            true
        );
        let first_frontier = first["knownAsOf"].as_u64().expect("knownAsOf");
        let first_now = first["nowMs"].as_u64().expect("nowMs");
        let bucket = first["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();

        capture_same_ms_order_quarantine(&host, "erpnext.a", first_now);

        let trace = call_http(
            &host,
            &principals,
            &runtime,
            "POST",
            "/v1/questions:trace",
            &json!({
                "question": q, "lineageRef": first["lineageRef"], "bucket": bucket,
                "knownAsOf": first_frontier, "nowMs": first_now,
            })
            .to_string(),
        );
        assert_eq!(trace.status, 200, "{}", trace.body);

        let after = eval_question(&host, &principals, &derived, &q);
        assert_eq!(after["freshness"], "incomplete", "{after}");
        assert_eq!(
            after["watermarks"]["erpnext.a"][ORDER_DATASET]["complete"],
            false
        );
        assert_eq!(
            after["watermarks"]["erpnext.b"][ORDER_DATASET]["complete"],
            true
        );
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.b",
            first_now.saturating_add(10),
            BTreeMap::new(),
            "ok",
        );
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.a",
            first_now.saturating_add(20),
            BTreeMap::new(),
            "partial",
        );
    }

    fn erpnext_profile() -> PullProfile {
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("profile")
    }

    fn ingest_party_row(
        host: &DevHost<NativeEngine>,
        conn: &str,
        obs: norn_kernel::Observed<RuntimeValue>,
        party_name: &str,
        ver: &str,
        now_ms: u64,
    ) -> Vec<norn_api::host::Ingested> {
        let input = norn_cell::mapping_bundle::MappingInput {
            mapping: "erpnext.rest.mapping".into(),
            dataset: "Party".into(),
            connector_instance: "sandbox".into(),
            partition: "Party".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-12T12:00:01Z".into()),
        };
        let prev = host
            .source_cursors(conn)
            .expect("cur")
            .get("Party")
            .map(|c| c.cursor_json.clone());
        let lease = host
            .acquire_source_lease(
                conn,
                &format!("run-{conn}-{now_ms}"),
                now_ms,
                norn_api::sources::RUN_LEASE_MS,
            )
            .expect("lease");
        let adv = norn_cell::source_cursor::CursorAdvanceSpec {
            connection_id: conn.into(),
            dataset: "Party".into(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms,
            expected_previous_cursor: prev,
            next_cursor: Some(json!({"version": ver, "id": party_name}).to_string()),
            renew_ttl_ms: Some(norn_api::sources::RUN_LEASE_MS),
        };
        let out = host
            .ingest_source_page(&[(obs, input)], &adv)
            .expect("ingest");
        host.release_source_lease(conn, &lease.lease_id, lease.generation)
            .expect("rel");
        out
    }

    fn party_obs(
        profile: &PullProfile,
        party_name: &str,
        ver: &str,
        payload: Value,
        now_ms: u64,
    ) -> norn_kernel::Observed<RuntimeValue> {
        norn_api::pull::observation(
            profile,
            &norn_api::pull::Row {
                dataset: "Party".into(),
                source_id: party_name.to_owned(),
                version: ver.to_owned(),
                payload: RuntimeValue::from_json(payload).expect("rv"),
            },
            now_ms,
        )
        .expect("party obs")
    }

    #[test]
    fn pinned_trace_unaffected_by_later_correction_replay_from_sibling() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        let profile = erpnext_profile();
        let pages = erpnext_pages(None);
        sync_named_erpnext(&host, &scope, "erpnext.a", T0, pages.clone(), "ok");
        sync_named_erpnext(&host, &scope, "erpnext.b", T0, pages.clone(), "ok");

        let party_row = pages["Party"].1["data"][0].clone();
        let name = party_row["name"].as_str().unwrap().to_owned();
        let mut bad_payload = party_row.clone();
        bad_payload["custom_norn_ref"] = Value::Null;
        let bad = party_obs(
            &profile,
            &name,
            "2026-11-12 20:00:00",
            bad_payload,
            T0.saturating_add(100),
        );
        let out_q = ingest_party_row(
            &host,
            "erpnext.a",
            bad,
            &name,
            "2026-11-12 20:00:00",
            T0.saturating_add(100),
        );
        assert!(matches!(
            out_q[0].outcome,
            norn_cell::MappingOutcome::Quarantined(_)
        ));

        // Sibling `erpnext.b` lands a strictly newer correction BEFORE A's answer.
        let corr = party_obs(
            &profile,
            &name,
            "2099-01-01 00:00:00",
            party_row,
            T0.saturating_add(110),
        );
        let out_b = ingest_party_row(
            &host,
            "erpnext.b",
            corr.clone(),
            &name,
            "2099-01-01 00:00:00",
            T0.saturating_add(110),
        );
        assert!(matches!(
            out_b[0].outcome,
            norn_cell::MappingOutcome::Landed | norn_cell::MappingOutcome::AlreadyLanded
        ));
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.a",
            T0.saturating_add(120),
            BTreeMap::new(),
            "partial",
        );
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.b",
            T0.saturating_add(120),
            BTreeMap::new(),
            "ok",
        );

        let derived = DerivedQuestions::new([7; 32], scope.clone()).with_configured_connections(
            BTreeMap::from([
                ("erpnext.a".into(), "erpnext.rest.mapping".into()),
                ("erpnext.b".into(), "erpnext.rest.mapping".into()),
            ]),
        );
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let ans = eval_question(&host, &principals, &derived, &q);
        assert_eq!(ans["freshness"], "exact", "{ans}");
        let now_ms = ans["nowMs"].as_u64().expect("now");

        // AFTER `frontier`, `erpnext.a` redelivers the exact correction row B landed earlier.
        ingest_party_row(
            &host,
            "erpnext.a",
            corr,
            &name,
            "2099-01-01 00:00:00",
            now_ms.saturating_add(50),
        );
        assert_pinned_trace_ok(&host, &principals, &runtime, &q, &ans);
    }

    fn ingest_nonterminal_party_row(
        host: &DevHost<NativeEngine>,
        conn: &str,
        obs: norn_kernel::Observed<RuntimeValue>,
        now_ms: u64,
    ) {
        let input = norn_cell::mapping_bundle::MappingInput {
            mapping: "erpnext.rest.mapping".into(),
            dataset: "Party".into(),
            connector_instance: "sandbox".into(),
            partition: "Party".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-12T12:00:01Z".into()),
        };
        let prev = host
            .source_cursors(conn)
            .expect("cur")
            .get("Party")
            .map(|c| c.cursor_json.clone());
        let lease = host
            .acquire_source_lease(
                conn,
                &format!("run-nonterm-{conn}-{now_ms}"),
                now_ms,
                norn_api::sources::RUN_LEASE_MS,
            )
            .expect("lease");
        let adv = norn_cell::source_cursor::CursorAdvanceSpec {
            connection_id: conn.into(),
            dataset: "Party".into(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms,
            expected_previous_cursor: prev,
            next_cursor: None,
            renew_ttl_ms: Some(norn_api::sources::RUN_LEASE_MS),
        };
        let out = host
            .ingest_source_page(&[(obs, input)], &adv)
            .expect("ingest");
        assert!(matches!(
            out[0].outcome,
            norn_cell::MappingOutcome::Landed | norn_cell::MappingOutcome::AlreadyLanded
        ));
        host.release_source_lease(conn, &lease.lease_id, lease.generation)
            .expect("rel");
    }

    fn assert_pinned_trace_ok(
        host: &DevHost<NativeEngine>,
        principals: &FixturePrincipals,
        runtime: &dyn AdHocQuestions<NativeEngine>,
        q: &Value,
        ans: &Value,
    ) {
        let bucket = ans["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        let trace = call_http(
            host,
            principals,
            runtime,
            "POST",
            "/v1/questions:trace",
            &json!({
                "question": q, "lineageRef": ans["lineageRef"], "bucket": bucket,
                "knownAsOf": ans["knownAsOf"], "nowMs": ans["nowMs"],
            })
            .to_string(),
        );
        assert_eq!(trace.status, 200, "{}", trace.body);
    }

    #[test]
    fn nonterminal_sibling_correction_before_answer_matches_pinned_trace() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        let profile = erpnext_profile();
        let pages = erpnext_pages(None);
        sync_named_erpnext(&host, &scope, "erpnext.a", T0, pages.clone(), "ok");
        sync_named_erpnext(&host, &scope, "erpnext.b", T0, pages.clone(), "ok");

        let party_row = pages["Party"].1["data"][0].clone();
        let name = party_row["name"].as_str().unwrap().to_owned();
        let mut bad_payload = party_row.clone();
        bad_payload["custom_norn_ref"] = Value::Null;
        let (t100, t110) = (T0.saturating_add(100), T0.saturating_add(110));
        let bad = party_obs(&profile, &name, "2026-11-12 20:00:00", bad_payload, t100);
        let q_out = ingest_party_row(&host, "erpnext.a", bad, &name, "2026-11-12 20:00:00", t100);
        assert!(matches!(
            q_out[0].outcome,
            norn_cell::MappingOutcome::Quarantined(_)
        ));
        let corr = party_obs(&profile, &name, "2099-01-01 00:00:00", party_row, t110);
        ingest_party_row(
            &host,
            "erpnext.b",
            corr.clone(),
            &name,
            "2099-01-01 00:00:00",
            t110,
        );
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.a",
            T0.saturating_add(120),
            BTreeMap::new(),
            "partial",
        );
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.b",
            T0.saturating_add(120),
            BTreeMap::new(),
            "ok",
        );

        // BEFORE answering, `erpnext.a` receives `corr` on a nonterminal page (0 new commits).
        ingest_nonterminal_party_row(&host, "erpnext.a", corr, T0.saturating_add(125));

        let derived = DerivedQuestions::new([7; 32], scope.clone()).with_configured_connections(
            BTreeMap::from([
                ("erpnext.a".into(), "erpnext.rest.mapping".into()),
                ("erpnext.b".into(), "erpnext.rest.mapping".into()),
            ]),
        );
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let ans = eval_question(&host, &principals, &derived, &q);
        assert_eq!(ans["freshness"], "exact", "{ans}");
        assert_pinned_trace_ok(&host, &principals, &runtime, &q, &ans);

        // Once `erpnext.a` finishes its sync, it reports `ok` and advances `commit_seq`.
        let now_ms = ans["nowMs"].as_u64().expect("now");
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.a",
            now_ms.saturating_add(20),
            BTreeMap::new(),
            "ok",
        );
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.b",
            now_ms.saturating_add(20),
            BTreeMap::new(),
            "ok",
        );
        let after = eval_question(&host, &principals, &derived, &q);
        assert_eq!(after["freshness"], "exact", "{after}");
        assert_pinned_trace_ok(&host, &principals, &runtime, &q, &after);
        assert_pinned_trace_ok(&host, &principals, &runtime, &q, &ans);
    }

    #[test]
    fn uncommitted_unrelated_quarantine_cannot_authorize_partial_report() {
        let (host, principals, scope) = open_hosted(&["erpnext"]);
        let profile = erpnext_profile();
        let pages = erpnext_pages(None);
        sync_named_erpnext(&host, &scope, "erpnext.a", T0, pages.clone(), "ok");

        let party_row = pages["Party"].1["data"][0].clone();
        let name = party_row["name"].as_str().unwrap().to_owned();
        let mut bad1 = party_row.clone();
        bad1["custom_norn_ref"] = Value::Null;
        let obs1 = party_obs(
            &profile,
            &name,
            "2026-11-12 20:00:00",
            bad1.clone(),
            T0.saturating_add(100),
        );
        let q1 = ingest_party_row(
            &host,
            "erpnext.a",
            obs1,
            &name,
            "2026-11-12 20:00:00",
            T0.saturating_add(100),
        );
        assert!(matches!(
            q1[0].outcome,
            norn_cell::MappingOutcome::Quarantined(_)
        ));
        sync_named_erpnext(
            &host,
            &scope,
            "erpnext.a",
            T0.saturating_add(120),
            BTreeMap::new(),
            "partial",
        );

        // Land a correction clearing the committed quarantine while `source.Connection`
        // still reports `partial`.
        let corr = party_obs(
            &profile,
            &name,
            "2099-01-01 00:00:00",
            party_row,
            T0.saturating_add(130),
        );
        ingest_party_row(
            &host,
            "erpnext.a",
            corr,
            &name,
            "2099-01-01 00:00:00",
            T0.saturating_add(130),
        );

        // Capture a NEW uncommitted quarantine on unrelated `Party` (0 new commits).
        let obs2 = party_obs(
            &profile,
            &name,
            "2099-01-02 00:00:00",
            bad1,
            T0.saturating_add(140),
        );
        let q2 = ingest_party_row(
            &host,
            "erpnext.a",
            obs2,
            &name,
            "2099-01-02 00:00:00",
            T0.saturating_add(140),
        );
        assert!(matches!(
            q2[0].outcome,
            norn_cell::MappingOutcome::Quarantined(_)
        ));

        // Because the committed frontier has no quarantine explaining `partial`,
        // live evaluation on `OrderLine` must fail closed (`incomplete`), matching `:trace`.
        let derived = DerivedQuestions::new([7; 32], scope).with_configured_connections(
            BTreeMap::from([("erpnext.a".into(), "erpnext.rest.mapping".into())]),
        );
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let ans = eval_question(
            &host,
            &principals,
            &derived,
            &order_line_question(&metric, 60_000),
        );
        assert_eq!(ans["freshness"], "incomplete", "{ans}");
    }

    fn commit_sample_order_line(
        store: &mut CellStore<NativeEngine>,
        scope: &CellScope,
        logical_time: u64,
    ) {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: scope.key(),
                logical_time,
                writes: vec![FactWriteRow::Put {
                    object_type: "canon.OrderLine".into(),
                    key: "order-poll-1".into(),
                    values: BTreeMap::from([
                        ("status".into(), RuntimeValue::from("promised")),
                        ("due_at".into(), RuntimeValue::from("2026-08-06T12:00:00Z")),
                        (
                            "quantity".into(),
                            RuntimeValue::Number("5".parse::<Decimal>().unwrap()),
                        ),
                        ("unit".into(), RuntimeValue::from("g")),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .expect("commit order line");
    }

    fn commit_connection_poll_status(
        store: &mut CellStore<NativeEngine>,
        scope: &CellScope,
        conn_id: &str,
        mapping_id: &str,
        status: &str,
        watermark: &str,
        logical_time: u64,
    ) {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: scope.key(),
                logical_time,
                writes: vec![FactWriteRow::Put {
                    object_type: "source.Connection".into(),
                    key: conn_id.into(),
                    values: BTreeMap::from([
                        ("id".into(), RuntimeValue::from(conn_id)),
                        ("system".into(), RuntimeValue::from("erpnext")),
                        ("label".into(), RuntimeValue::from("ERPNext REST")),
                        (
                            "target".into(),
                            RuntimeValue::from("http://erpnext-rest:8000"),
                        ),
                        ("auth".into(), RuntimeValue::from("api_key")),
                        ("status".into(), RuntimeValue::from(status)),
                        (
                            "last_sync_started_at".into(),
                            RuntimeValue::from("2026-11-12T12:00:00Z"),
                        ),
                        (
                            "last_sync_ended_at".into(),
                            RuntimeValue::from("2026-11-12T12:00:10Z"),
                        ),
                        ("last_trigger".into(), RuntimeValue::from("manual")),
                        ("watermark".into(), RuntimeValue::from(watermark)),
                        ("rows_read".into(), RuntimeValue::Number(Decimal::from(0))),
                        ("rows_landed".into(), RuntimeValue::Number(Decimal::from(0))),
                        (
                            "rows_quarantined".into(),
                            RuntimeValue::Number(Decimal::from(0)),
                        ),
                        ("quarantine_reasons".into(), RuntimeValue::from("{}")),
                        ("last_error_code".into(), RuntimeValue::Null),
                        ("last_error_message".into(), RuntimeValue::Null),
                        (
                            "connector_id".into(),
                            RuntimeValue::from("erpnext.rest.connector"),
                        ),
                        (
                            "source_schema_id".into(),
                            RuntimeValue::from("erpnext.rest.source"),
                        ),
                        ("mapping_id".into(), RuntimeValue::from(mapping_id)),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .expect("commit connection poll");
    }

    fn commit_connection_poll(
        store: &mut CellStore<NativeEngine>,
        scope: &CellScope,
        conn_id: &str,
        mapping_id: &str,
        watermark: &str,
        logical_time: u64,
    ) {
        commit_connection_poll_status(
            store,
            scope,
            conn_id,
            mapping_id,
            "ok",
            watermark,
            logical_time,
        );
    }

    fn seed_order_cursor_binding(
        store: &mut CellStore<NativeEngine>,
        conn_id: &str,
        cursor_json: &str,
        now_ms: u64,
    ) {
        let binding = store
            .read()
            .active_mapping_binding("erpnext.rest.mapping")
            .expect("binding read")
            .expect("binding present");
        let lease = store
            .acquire_source_lease(conn_id, "seed-lease", now_ms, 60_000)
            .expect("lease");
        let cursors = BTreeMap::from([(ORDER_DATASET.to_owned(), cursor_json.to_owned())]);
        store
            .seed_source_cursors(
                conn_id,
                &lease,
                now_ms,
                &cursors,
                Some(("erpnext.rest.mapping", binding.as_str())),
            )
            .expect("seed cursors");
        store
            .release_source_lease(conn_id, &lease.lease_id, lease.generation)
            .expect("release");
    }

    #[test]
    fn empty_polls_over_4096_without_rebind_preserve_pinned_trace() {
        const POLL_COUNT: u64 = 4_105;
        let (mut store, scope) = open_store(&["erpnext"]);
        let cursor_json = "\"2026-11-12 12:00:00\"";
        let watermark = json!({ ORDER_DATASET: "2026-11-12 12:00:00" }).to_string();
        commit_sample_order_line(&mut store, &scope, T0);
        for step in 1..=POLL_COUNT {
            commit_connection_poll(
                &mut store,
                &scope,
                "erpnext.rest",
                "erpnext.rest.mapping",
                &watermark,
                T0.saturating_add(step),
            );
        }
        seed_order_cursor_binding(
            &mut store,
            "erpnext.rest",
            cursor_json,
            T0.saturating_add(POLL_COUNT),
        );
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let ans = eval_question(&host, &principals, &derived, &q);
        assert_eq!(ans["freshness"], "exact", "{ans}");
        assert_pinned_trace_ok(&host, &principals, &runtime, &q, &ans);
    }

    #[test]
    fn empty_polls_over_4096_after_rebind_preserve_pinned_trace_and_reject_stale() {
        const POLL_COUNT: u64 = 4_105;
        let prior_watermark = json!({ ORDER_DATASET: "2026-11-12 12:00:00" }).to_string();
        let advanced_cursor_json = "\"2026-11-12 13:00:00\"";
        let advanced_watermark = json!({ ORDER_DATASET: "2026-11-12 13:00:00" }).to_string();
        let eval_now = T0.saturating_add(POLL_COUNT).saturating_add(2);

        // Stale cursor carried over across a mapping rebind must stay incomplete even after >4,096 empty polls.
        let (mut stale_store, stale_scope) = open_store(&["erpnext"]);
        commit_sample_order_line(&mut stale_store, &stale_scope, T0);
        commit_connection_poll(
            &mut stale_store,
            &stale_scope,
            "erpnext.rest",
            "other.mapping",
            &prior_watermark,
            T0.saturating_add(1),
        );
        for step in 2..=POLL_COUNT.saturating_add(1) {
            commit_connection_poll(
                &mut stale_store,
                &stale_scope,
                "erpnext.rest",
                "erpnext.rest.mapping",
                &prior_watermark,
                T0.saturating_add(step),
            );
        }
        let (stale_host, stale_principals, stale_scope) = wrap_host(stale_store, stale_scope);
        let stale_derived = DerivedQuestions::new([7; 32], stale_scope);
        let stale_rt = stale_derived
            .questions_at(&stale_host, eval_now)
            .expect("stale runtime");
        let stale_metric = metric_name_for(
            &stale_host,
            &stale_principals,
            &stale_rt,
            "canon.OrderLine",
            "quantity",
        );
        let stale_ans = eval_question_at(
            &stale_host,
            &stale_principals,
            &stale_derived,
            ADMIN,
            eval_now,
            &order_line_question(&stale_metric, 60_000),
        );
        assert_eq!(stale_ans["freshness"], "incomplete", "{stale_ans}");

        // Advancing cursor after rebind and repeating >4,096 empty polls must stay Exact and trace cleanly.
        let (mut fresh_store, fresh_scope) = open_store(&["erpnext"]);
        commit_sample_order_line(&mut fresh_store, &fresh_scope, T0);
        commit_connection_poll(
            &mut fresh_store,
            &fresh_scope,
            "erpnext.rest",
            "other.mapping",
            &prior_watermark,
            T0.saturating_add(1),
        );
        for step in 2..=POLL_COUNT.saturating_add(2) {
            commit_connection_poll(
                &mut fresh_store,
                &fresh_scope,
                "erpnext.rest",
                "erpnext.rest.mapping",
                &advanced_watermark,
                T0.saturating_add(step),
            );
        }
        seed_order_cursor_binding(
            &mut fresh_store,
            "erpnext.rest",
            advanced_cursor_json,
            eval_now,
        );
        let (fresh_host, fresh_principals, fresh_scope) = wrap_host(fresh_store, fresh_scope);
        let fresh_derived = DerivedQuestions::new([7; 32], fresh_scope);
        let fresh_rt = fresh_derived.questions(&fresh_host).expect("fresh runtime");
        let fresh_metric = metric_name_for(
            &fresh_host,
            &fresh_principals,
            &fresh_rt,
            "canon.OrderLine",
            "quantity",
        );
        let q = order_line_question(&fresh_metric, 60_000);
        let fresh_ans = eval_question(&fresh_host, &fresh_principals, &fresh_derived, &q);
        assert_eq!(fresh_ans["freshness"], "exact", "{fresh_ans}");
        assert_pinned_trace_ok(&fresh_host, &fresh_principals, &fresh_rt, &q, &fresh_ans);
    }

    fn tamper_connection_snapshot(path: &Path, version: i64, field: &str, replacement: &str) {
        let engine = NativeEngine::open(path, StoreOptions::default()).expect("sqlite");
        let rows = engine
            .query(
                "SELECT snapshot FROM _norn_fact_values \
                 WHERE root_type='source.Connection' AND id='erpnext.rest' AND version=?1",
                &[SqlValue::Integer(version)],
            )
            .expect("query snapshot");
        let raw: String = SqlRow(rows[0].clone()).get(0).expect("snapshot text");
        let mut parsed: Value = serde_json::from_str(&raw).expect("snapshot json");
        parsed["values"][field] = Value::String(replacement.to_owned());
        engine
            .execute(
                "UPDATE _norn_fact_values SET snapshot=?1 \
                 WHERE root_type='source.Connection' AND id='erpnext.rest' AND version=?2",
                &[
                    SqlValue::Text(parsed.to_string()),
                    SqlValue::Integer(version),
                ],
            )
            .expect("update snapshot");
    }

    fn assert_tampered_eval_refused(
        version: i64,
        field: &str,
        replacement: &str,
        stale_watermark: &str,
    ) {
        let db = tempfile::NamedTempFile::new().expect("temp db");
        let (mut store, scope) = open_store_at(&["erpnext"], Some(db.path()));
        commit_sample_order_line(&mut store, &scope, T0);
        commit_connection_poll(
            &mut store,
            &scope,
            "erpnext.rest",
            "other.mapping",
            stale_watermark,
            T0.saturating_add(1),
        );
        for step in 2..=5 {
            commit_connection_poll(
                &mut store,
                &scope,
                "erpnext.rest",
                "erpnext.rest.mapping",
                stale_watermark,
                T0.saturating_add(step),
            );
        }
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived
            .questions_at(&host, T0.saturating_add(10))
            .expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let baseline = eval_question_at(
            &host,
            &principals,
            &derived,
            ADMIN,
            T0.saturating_add(10),
            &q,
        );
        assert_eq!(baseline["freshness"], "incomplete", "{baseline}");

        tamper_connection_snapshot(db.path(), version, field, replacement);
        let resp = call_http_as(
            &host,
            &principals,
            &runtime,
            ADMIN,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": q }).to_string(),
        );
        assert_ne!(
            resp.status, 200,
            "tampered {field} on v{version} must fail receipt authentication: {}",
            resp.body
        );
    }

    #[test]
    fn tampered_intermediate_connection_snapshot_cannot_forge_historical_cursor_proof() {
        let stale_watermark = json!({ ORDER_DATASET: "2026-11-12 12:00:00" }).to_string();
        let forged_watermark = json!({ ORDER_DATASET: "2026-11-12 11:00:00" }).to_string();
        assert_tampered_eval_refused(3, "watermark", &forged_watermark, &stale_watermark);
        assert_tampered_eval_refused(1, "mapping_id", "erpnext.rest.mapping", &stale_watermark);

        // Case 3 (A -> B -> A): tampering the skipped middle foreign mapping_id must fail closed.
        let db = tempfile::NamedTempFile::new().expect("temp db");
        let (mut store, scope) = open_store_at(&["erpnext"], Some(db.path()));
        commit_sample_order_line(&mut store, &scope, T0);
        commit_connection_poll(
            &mut store,
            &scope,
            "erpnext.rest",
            "erpnext.rest.mapping",
            &stale_watermark,
            T0.saturating_add(1),
        );
        commit_connection_poll(
            &mut store,
            &scope,
            "erpnext.rest",
            "other.mapping",
            &stale_watermark,
            T0.saturating_add(2),
        );
        commit_connection_poll(
            &mut store,
            &scope,
            "erpnext.rest",
            "erpnext.rest.mapping",
            &stale_watermark,
            T0.saturating_add(3),
        );
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let baseline = eval_question(&host, &principals, &derived, &q);
        assert_eq!(baseline["freshness"], "incomplete", "{baseline}");

        tamper_connection_snapshot(db.path(), 2, "mapping_id", "erpnext.rest.mapping");
        let resp = call_http_as(
            &host,
            &principals,
            &runtime,
            ADMIN,
            "POST",
            "/v1/questions:evaluate",
            &json!({ "question": q }).to_string(),
        );
        assert_ne!(
            resp.status, 200,
            "tampered middle B in A->B->A must fail receipt authentication: {}",
            resp.body
        );
    }

    #[test]
    fn foreign_failed_report_blocks_stale_cursor_reuse_across_rebind() {
        let stale_watermark = json!({ ORDER_DATASET: "2026-11-12 12:00:00" }).to_string();
        let (mut store, scope) = open_store(&["erpnext"]);
        commit_sample_order_line(&mut store, &scope, T0);
        commit_connection_poll_status(
            &mut store,
            &scope,
            "erpnext.rest",
            "other.mapping",
            "failed",
            &stale_watermark,
            T0.saturating_add(1),
        );
        commit_connection_poll(
            &mut store,
            &scope,
            "erpnext.rest",
            "erpnext.rest.mapping",
            &stale_watermark,
            T0.saturating_add(2),
        );
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope);
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let ans = eval_question(
            &host,
            &principals,
            &derived,
            &order_line_question(&metric, 60_000),
        );
        assert_eq!(ans["freshness"], "incomplete", "{ans}");
    }

    #[test]
    fn multi_dataset_rewind_and_post_pin_polls_preserve_pinned_trace() {
        let db = tempfile::NamedTempFile::new().expect("temp db");
        let (mut store, scope) = open_store_at(&["erpnext"], Some(db.path()));
        let model = store.model().clone();
        commit_sample_order_line(&mut store, &scope, T0);
        let foreign_pair = json!({ ORDER_DATASET: "X", "Party": "a" }).to_string();
        let middle_pair = json!({ ORDER_DATASET: "X", "Party": "b" }).to_string();
        let target_pair = json!({ ORDER_DATASET: "X", "Party": "c" }).to_string();
        let rewind_pair = json!({ ORDER_DATASET: "Y", "Party": "d" }).to_string();
        commit_connection_poll(
            &mut store,
            &scope,
            "erpnext.rest",
            "other.mapping",
            &foreign_pair,
            T0.saturating_add(1),
        );
        for (idx, wm) in [&middle_pair, &target_pair, &rewind_pair, &target_pair]
            .into_iter()
            .enumerate()
        {
            let offset = u64::try_from(idx).unwrap_or(0).saturating_add(2);
            commit_connection_poll(
                &mut store,
                &scope,
                "erpnext.rest",
                "erpnext.rest.mapping",
                wm,
                T0.saturating_add(offset),
            );
        }
        seed_order_cursor_binding(&mut store, "erpnext.rest", "\"X\"", T0.saturating_add(6));
        let (host, principals, scope) = wrap_host(store, scope);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let runtime = derived.questions(&host).expect("runtime");
        let metric = metric_name_for(&host, &principals, &runtime, "canon.OrderLine", "quantity");
        let q = order_line_question(&metric, 60_000);
        let ans = eval_question(&host, &principals, &derived, &q);
        assert_eq!(ans["freshness"], "exact", "{ans}");
        assert_pinned_trace_ok(&host, &principals, &runtime, &q, &ans);
        drop(host);

        // Append >4,096 post-pin polls with distinct watermarks/mappings sorting before `v4`.
        let mut reopened =
            CellStore::open(db.path(), model, StoreOptions::default()).expect("open");
        for step in 0_u64..4_105 {
            let later_wm =
                json!({ ORDER_DATASET: format!("0000-{step:05}"), "Party": "z" }).to_string();
            let later_map = format!("0000-map-{step:05}");
            commit_connection_poll(
                &mut reopened,
                &scope,
                "erpnext.rest",
                &later_map,
                &later_wm,
                T0.saturating_add(100).saturating_add(step),
            );
        }
        let (reopened_host, reopened_principals, reopened_scope) = wrap_host(reopened, scope);
        let reopened_derived = DerivedQuestions::new([7; 32], reopened_scope);
        let reopened_rt = reopened_derived
            .questions(&reopened_host)
            .expect("reopened runtime");
        assert_pinned_trace_ok(&reopened_host, &reopened_principals, &reopened_rt, &q, &ans);
    }
}
