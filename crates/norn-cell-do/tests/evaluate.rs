//! F2b: ad-hoc questions over the analytics cell's declared metrics, end to
//! end through the fetch dispatch. A question is authorized as the union of
//! every metric and type it reads before admission, so an inadmissible
//! question's diagnostics never describe an ungranted resource; an admitted
//! one is answered under the same row gates and D53 redaction as a named one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "route correspondence"
)]
use norn_api::authz::ReadCapability;
use norn_api::host::DevHost;
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, ObjectRecord};
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server::{self, Response};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::MetricDefinition;
use norn_kernel::authz::AccessRequest;
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
const ADMIN: Option<&str> = Some("Bearer fixture:viewer@demo");
const VIEWER: Option<&str> = Some("Bearer fixture:guest@demo");

/// The analytics runtime with overridable bounds and one metric whose access
/// mapping is withheld from the caller (an ungranted metric).
struct Bounded {
    inner: DemoQuestions,
    rows: u32,
    buckets: usize,
    ungranted: Option<&'static str>,
}

impl QuestionRuntime<NativeEngine> for Bounded {
    fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal> {
        QuestionRuntime::<NativeEngine>::plan(&self.inner, question)
    }
    fn snapshot(
        &self,
        store: &CellStore<NativeEngine>,
        plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        self.inner.snapshot(store, plan, actor)
    }
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        question: &str,
        capability: ReadCapability,
        row: Option<&ObjectRecord>,
    ) -> Result<AccessRequest, Refusal> {
        let mut access = QuestionRuntime::<NativeEngine>::access(
            &self.inner,
            scope,
            actor,
            question,
            capability,
            row,
        )?;
        if row.is_none() && self.ungranted == Some(question) {
            // The PDP request for this metric names no grant the caller holds.
            access.principal.subject_id = "nobody".into();
        }
        Ok(access)
    }
    fn min_disclosed_bucket(&self) -> usize {
        QuestionRuntime::<NativeEngine>::min_disclosed_bucket(&self.inner)
    }
    fn handle_key(&self) -> &[u8] {
        QuestionRuntime::<NativeEngine>::handle_key(&self.inner)
    }
}

impl AdHocQuestions<NativeEngine> for Bounded {
    fn definitions(&self) -> &[MetricDefinition] {
        AdHocQuestions::<NativeEngine>::definitions(&self.inner)
    }
    fn catalog(&self) -> &EffectiveCatalog {
        AdHocQuestions::<NativeEngine>::catalog(&self.inner)
    }
    fn max_rows(&self) -> u32 {
        self.rows
    }
    fn max_buckets(&self) -> usize {
        self.buckets
    }
}

struct Cell {
    host: DevHost<NativeEngine>,
    runtime: Bounded,
    principals: FixturePrincipals,
}

fn cell() -> Cell {
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    let inner = DemoQuestions::new(opened.plan, scope)
        .with_handle_key(&[5; 32])
        .with_metrics(opened.definitions, opened.catalog);
    Cell {
        host,
        runtime: Bounded {
            inner,
            rows: analytics::AD_HOC_MAX_ROWS,
            buckets: analytics::AD_HOC_MAX_BUCKETS,
            ungranted: None,
        },
        principals,
    }
}

fn call(cell: &Cell, method: &str, path: &str, auth: Option<&str>, body: &str) -> Response {
    let request = server::Request {
        method,
        path,
        query: SCOPE,
        authorization: auth,
        now: 0,
        body,
    };
    server::handle_with(&cell.host, &cell.principals, Some(&cell.runtime), &request)
}

fn post(cell: &Cell, path: &str, auth: Option<&str>, body: &Value) -> Response {
    call(cell, "POST", path, auth, &body.to_string())
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

/// The weekly yield question, as an ad-hoc `QuestionIR` for `metal`.
fn question(metal: &str) -> Value {
    let mut question: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .unwrap();
    let fields = question.as_object_mut().unwrap();
    fields.remove("compare");
    fields.insert("name".into(), json!("adhoc.yield_by_week"));
    fields.insert("objectSet".into(), json!(analytics::TYPE));
    fields.insert("aggregate".into(), json!([{"metric": "demo.yield"}]));
    fields.insert(
        "groupBy".into(),
        json!([{"path": {"property": "occurred_at"}, "grain": "week"}]),
    );
    fields.insert(
        "filter".into(),
        json!([{"path": {"property": "metal"}, "operator": "eq", "value": metal}]),
    );
    fields.insert(
        "window".into(),
        json!({"start": "2026-07-06T00:00:00Z", "end": "2026-10-26T00:00:00Z"}),
    );
    fields.insert(
        "freshness".into(),
        json!({"kind": "live", "maxAgeMs": 5000}),
    );
    question
}

fn evaluate(cell: &Cell, auth: Option<&str>, question: &Value) -> Response {
    post(
        cell,
        "/v1/questions:evaluate",
        auth,
        &json!({ "question": question }),
    )
}

#[test]
fn an_admitted_ad_hoc_question_answers_exactly_as_the_named_one() {
    let cell = cell();
    let adhoc = evaluate(&cell, ADMIN, &question("gold"));
    assert_eq!(adhoc.status, 200, "{}", adhoc.body);
    let named = call(
        &cell,
        "GET",
        &format!("/v1/questions/{}/answer", analytics::QUESTION),
        ADMIN,
        "",
    );
    assert_eq!(named.status, 200, "{}", named.body);
    let (adhoc, named) = (json_of(&adhoc), json_of(&named));
    assert_eq!(adhoc["rows"], named["rows"]);
    assert_eq!(adhoc["freshness"], "exact");
    // Its handle is keyed (D53), and it opens the ad-hoc trace.
    let handle = adhoc["lineageRef"].as_str().unwrap();
    assert_eq!(handle.len(), 64);
    let bucket = adhoc["rows"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let trace = post(
        &cell,
        "/v1/questions:trace",
        ADMIN,
        &json!({"question": question("gold"), "lineageRef": handle, "bucket": bucket}),
    );
    assert_eq!(trace.status, 200, "{}", trace.body);
    assert_eq!(
        json_of(&trace)["contributions"].as_array().unwrap().len(),
        4
    );
}

/// D53 per bucket: a viewer (Population only) gets the aggregate with every
/// member amount withheld; a question whose buckets fall below the minimum
/// (one silver conversion a week) is suppressed for the viewer, not the admin.
#[test]
fn an_ad_hoc_answer_applies_d53_per_bucket() {
    let cell = cell();
    let gold = json_of(&evaluate(&cell, VIEWER, &question("gold")));
    for (bucket, row) in gold["rows"].as_object().unwrap() {
        assert!(
            row["totals"].as_array().is_some_and(|t| !t.is_empty()),
            "{bucket}"
        );
        assert!(row.get("suppressed").is_none(), "{bucket}");
        assert!(
            row["members"]
                .as_object()
                .unwrap()
                .values()
                .all(|m| m["amounts"].is_null() && m["restricted"] == true),
            "{bucket}"
        );
    }
    let viewer = evaluate(&cell, VIEWER, &question("silver"));
    assert_eq!(viewer.status, 200, "{}", viewer.body);
    let viewer = json_of(&viewer);
    let admin = json_of(&evaluate(&cell, ADMIN, &question("silver")));
    assert!(!viewer["rows"].as_object().unwrap().is_empty());
    for (bucket, row) in viewer["rows"].as_object().unwrap() {
        assert_eq!(row["members"].as_object().unwrap().len(), 1, "{bucket}");
        assert_eq!(row["suppressed"], true, "{bucket}");
        assert!(
            row["value"].is_null() && row["totals"] == json!([]),
            "{bucket}"
        );
        let open = &admin["rows"][bucket];
        assert!(
            open.get("suppressed").is_none() && !open["value"].is_null(),
            "{bucket}"
        );
    }
    // The viewer's trace of a suppressed bucket discloses no member value.
    let bucket = viewer["rows"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let trace = json_of(&post(
        &cell,
        "/v1/questions:trace",
        VIEWER,
        &json!({"question": question("silver"), "lineageRef": viewer["lineageRef"], "bucket": bucket}),
    ));
    assert_eq!(trace["result"]["rows"][&bucket]["suppressed"], true);
    assert!(
        trace["contributions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["values"].is_null() && c["amounts"].is_null())
    );
}

/// Authorization precedes admission: an ungranted or undeclared metric, or a
/// caller without grants, gets the one opaque 403, never a diagnostic, even
/// for a question that would not admit.
#[test]
fn an_ungranted_metric_is_the_opaque_denial_never_a_diagnostic() {
    let mut cell = cell();
    let baseline = evaluate(&cell, None, &question("gold"));
    assert_eq!(baseline.status, 403);
    let mut inadmissible = question("gold");
    inadmissible["groupBy"] = json!([{"path": {"property": "occurred_at"}, "grain": "day"}]);
    // Anonymous, inadmissible: the denial, not the 422.
    assert_eq!(evaluate(&cell, None, &inadmissible).body, baseline.body);
    // A declared metric the caller holds no grant on — here the ratio's
    // numerator, read only transitively: the same denial, for any question.
    cell.runtime.ungranted = Some("demo.recovered");
    assert_eq!(
        evaluate(&cell, ADMIN, &question("gold")).body,
        baseline.body
    );
    assert_eq!(evaluate(&cell, ADMIN, &inadmissible).body, baseline.body);
    let trace = post(
        &cell,
        "/v1/questions:trace",
        ADMIN,
        &json!({"question": question("gold"), "lineageRef": "0".repeat(64), "bucket": "\"2026-10-19\""}),
    );
    assert_eq!(trace.body, baseline.body);
    // Granted again, the inadmissible question gets its diagnostics.
    cell.runtime.ungranted = None;
    let refused = evaluate(&cell, ADMIN, &inadmissible);
    assert_eq!(refused.status, 422, "{}", refused.body);
    let refused = json_of(&refused);
    assert_eq!(refused["code"], "question.refused");
    let diagnostics = refused["diagnostics"].as_array().unwrap();
    assert!(!diagnostics.is_empty());
    assert!(
        diagnostics
            .iter()
            .all(|d| d["code"].is_string() && d["severity"].is_string())
    );
}

#[test]
fn bounds_on_rows_read_and_output_buckets_refuse_rather_than_truncate() {
    let mut cell = cell();
    cell.runtime.rows = 10;
    let rows = evaluate(&cell, ADMIN, &question("gold"));
    assert_eq!(rows.status, 413, "{}", rows.body);
    assert_eq!(json_of(&rows)["code"], "core.compute.bound_exceeded");
    cell.runtime.rows = analytics::AD_HOC_MAX_ROWS;
    cell.runtime.buckets = 3;
    let buckets = evaluate(&cell, ADMIN, &question("gold"));
    assert_eq!(buckets.status, 413, "{}", buckets.body);
    assert_eq!(json_of(&buckets)["code"], "question.bucket_bound_exceeded");
    cell.runtime.buckets = 16;
    assert_eq!(
        evaluate(&cell, ADMIN, &question("gold")).status,
        200,
        "sixteen weeks fit sixteen buckets"
    );
}

#[test]
fn submissions_are_posted_json_and_malformed_ones_refuse() {
    let cell = cell();
    assert_eq!(
        call(&cell, "GET", "/v1/questions:evaluate", ADMIN, "").status,
        405
    );
    assert_eq!(
        call(
            &cell,
            "POST",
            "/v1/questions/demo.yield_week/answer",
            ADMIN,
            "{}"
        )
        .status,
        405
    );
    assert_eq!(
        call(&cell, "POST", "/v1/questions:evaluate", ADMIN, "not json").status,
        400
    );
    assert_eq!(
        post(
            &cell,
            "/v1/questions:evaluate",
            ADMIN,
            &json!({"question": question("gold"), "extra": 1})
        )
        .status,
        400,
        "unknown members refuse"
    );
}

/// D62: an undeclared metric is the checker's 422, with its pointer, only for
/// a caller who can read every metric the cell declares. A caller missing any
/// metric grant gets the opaque 403, byte-identical for an undeclared and an
/// ungranted metric: no existence oracle over the catalog.
#[test]
fn an_undeclared_metric_is_a_diagnostic_only_for_a_caller_who_reads_every_metric() {
    let mut cell = cell();
    let mut undeclared = question("gold");
    undeclared["aggregate"] = json!([{"metric": "demo.margin"}]);
    // Full grant: the 422 says where.
    let refused = evaluate(&cell, ADMIN, &undeclared);
    assert_eq!(refused.status, 422, "{}", refused.body);
    let refused = json_of(&refused);
    assert_eq!(refused["code"], "question.refused");
    let unknown: Vec<&Value> = refused["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "metric.validation.unknown_metric")
        .collect();
    assert_eq!(unknown.len(), 1, "{refused}");
    assert_eq!(
        unknown[0]["span"]["pointer"],
        "/question/aggregate/0/metric"
    );
    // Partial grant (no grant on demo.input, which this question does not read):
    // undeclared and ungranted are one denial, byte for byte.
    cell.runtime.ungranted = Some("demo.input");
    let baseline = evaluate(&cell, None, &question("gold"));
    assert_eq!(baseline.status, 403);
    let hidden_undeclared = evaluate(&cell, ADMIN, &undeclared);
    let mut ungranted = question("gold");
    ungranted["aggregate"] = json!([{"metric": "demo.input"}]);
    let hidden_ungranted = evaluate(&cell, ADMIN, &ungranted);
    assert_eq!(hidden_undeclared.status, 403);
    assert_eq!(hidden_undeclared.body, hidden_ungranted.body);
    assert_eq!(hidden_undeclared.body, baseline.body);
    // A question within the partial grant still answers.
    let mut recovered = question("gold");
    recovered["aggregate"] = json!([{"metric": "demo.recovered"}]);
    assert_eq!(evaluate(&cell, ADMIN, &recovered).status, 200);
}
