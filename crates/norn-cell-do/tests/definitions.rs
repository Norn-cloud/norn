//! Retained definitions (dashboards, agents) end to end through the fetch
//! dispatch: admit is side-effect free, install and retire append a version
//! through the governed definitions install, reads need only Population, and
//! every denied request gets the one opaque 403.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "route correspondence"
)]
use std::collections::BTreeSet;

use norn_api::definition_routes::{AgentAdmissionContext, DefinitionKinds};
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server::{self, Response};
use norn_checker::{Diagnostic, DiagnosticCode, DiagnosticSeverity, Span};
use norn_ir::ResourceLimitProfile;
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
/// The fixture principal holding `admin`.
const ADMIN: Option<&str> = Some("Bearer fixture:viewer@demo");
/// The fixture principal holding `viewer`.
const VIEWER: Option<&str> = Some("Bearer fixture:guest@demo");

struct Cell {
    host: DevHost<NativeEngine>,
    questions: DemoQuestions,
    principals: FixturePrincipals,
    kinds: DefinitionKinds,
}

fn cell(kinds: DefinitionKinds) -> Cell {
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
    let questions = DemoQuestions::new(opened.plan, scope)
        .with_handle_key(&[5; 32])
        .with_metrics(opened.definitions, opened.catalog);
    Cell {
        host,
        questions,
        principals,
        kinds,
    }
}

fn call(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    auth: Option<&str>,
    body: &str,
) -> Response {
    let request = server::Request {
        method,
        path,
        query,
        authorization: auth,
        now: 1_800_000_000,
        body,
    };
    server::handle_all(
        &cell.host,
        &cell.principals,
        Some(&cell.questions),
        &cell.kinds,
        &request,
    )
}

fn post(cell: &Cell, path: &str, auth: Option<&str>, body: &Value) -> (u16, Value) {
    let response = call(cell, "POST", path, SCOPE, auth, &body.to_string());
    (response.status, serde_json::from_str(&response.body).unwrap())
}

fn get(cell: &Cell, path: &str, query: &str, auth: Option<&str>) -> (u16, Value) {
    let response = call(cell, "GET", path, &format!("{SCOPE}{query}"), auth, "");
    (response.status, serde_json::from_str(&response.body).unwrap())
}

/// An admitted ad-hoc question over the analytics cell (F2b's fixture shape).
fn question() -> Value {
    let mut question: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .unwrap();
    let fields = question.as_object_mut().unwrap();
    fields.remove("compare");
    fields.insert("name".into(), json!("studio.tile.yield"));
    fields.insert("objectSet".into(), json!(analytics::TYPE));
    fields.insert("aggregate".into(), json!([{"metric": "demo.yield"}]));
    fields.insert(
        "groupBy".into(),
        json!([{"path": {"property": "occurred_at"}, "grain": "week"}]),
    );
    fields.remove("filter");
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

fn dashboard(title: &str) -> Value {
    json!({
        "title": title,
        "tiles": [{
            "id": "t1",
            "title": "Yield by week",
            "question": question(),
            "view": { "kind": "chart", "chart": "line" },
            "layout": { "x": 0, "y": 0, "w": 6, "h": 4 }
        }]
    })
}

fn install(cell: &Cell, id: &str, payload: &Value, expected: Option<u64>) -> (u16, Value) {
    let mut body = json!({ "kind": "dashboard", "id": id, "payload": payload });
    if let Some(expected) = expected {
        body["expectedVersion"] = json!(expected);
    }
    post(cell, "/v1/definitions:install", ADMIN, &body)
}

fn pointers(document: &Value) -> Vec<(String, String)> {
    document["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_owned(),
                d["span"]["pointer"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

#[test]
fn admit_runs_the_dashboard_validator_without_side_effects() {
    let cell = cell(DefinitionKinds::default());
    let (_, before) = get(&cell, "/v1/definitions", "", ADMIN);
    let body = json!({ "kind": "dashboard", "id": "studio.dashboard.a", "payload": dashboard("Refinery") });
    let (status, admitted) = post(&cell, "/v1/definitions:admit", ADMIN, &body);
    assert_eq!(status, 200, "{admitted}");
    assert_eq!(admitted, json!({ "admitted": true, "diagnostics": [] }));
    // Nothing was installed: the catalog version did not move.
    let (_, after) = get(&cell, "/v1/definitions", "", ADMIN);
    assert_eq!(before["version"], after["version"]);

    let mut bad = dashboard("  ");
    bad["extra"] = json!(1);
    bad["tiles"][0]["title"] = json!("two\nlines");
    bad["tiles"][0]["view"] = json!({ "kind": "table", "chart": "bar" });
    bad["tiles"][0]["layout"] = json!({ "x": -1, "y": 0, "w": 0, "h": 4 });
    let mut unknown_property = dashboard("x")["tiles"][0].clone();
    unknown_property["id"] = json!("t2");
    unknown_property["question"]["groupBy"] =
        json!([{"path": {"property": "no_such_property"}, "grain": "week"}]);
    let mut undecodable = unknown_property.clone();
    undecodable["id"] = json!("t3");
    undecodable["question"] = json!({ "name": 7 });
    bad["tiles"]
        .as_array_mut()
        .unwrap()
        .extend([unknown_property, undecodable]);
    let body = json!({ "kind": "dashboard", "id": "studio.dashboard.a", "payload": bad });
    let (status, refused) = post(&cell, "/v1/definitions:admit", ADMIN, &body);
    assert_eq!(status, 200, "{refused}");
    assert_eq!(refused["admitted"], json!(false));
    let found = pointers(&refused);
    for expected in [
        ("definition.invalid_payload", "/extra"),
        ("definition.tile_title_invalid", "/title"),
        ("definition.tile_title_invalid", "/tiles/0/title"),
        ("definition.view_invalid", "/tiles/0/view"),
        ("definition.layout_invalid", "/tiles/0/layout/x"),
        ("definition.layout_invalid", "/tiles/0/layout/w"),
        ("definition.question_invalid", "/tiles/2/question"),
    ] {
        assert!(
            found.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "{expected:?} in {found:?}"
        );
    }
    // The tile question is admitted by F2b's checker, its pointers nested.
    assert!(
        found
            .iter()
            .any(|(code, pointer)| !code.starts_with("definition.")
                && pointer.starts_with("/tiles/1/question/")),
        "{found:?}"
    );
}

#[test]
fn writes_need_a_writer_grant_and_every_denial_is_the_same_document() {
    let cell = cell(DefinitionKinds::default());
    let body = json!({ "kind": "dashboard", "id": "studio.dashboard.a", "payload": dashboard("A") });
    let denied: Vec<Response> = [
        ("/v1/definitions:admit", VIEWER, SCOPE),
        ("/v1/definitions:install", VIEWER, SCOPE),
        ("/v1/definitions:install", None, SCOPE),
        ("/v1/definitions:install", Some("Bearer fixture:nobody@demo"), SCOPE),
        // An out-of-scope request is the same denial, even for an admin.
        ("/v1/definitions:install", ADMIN, "orgId=org_demo&environmentId=dev"),
        ("/v1/definitions:install", ADMIN, ""),
        ("/v1/definitions/studio.dashboard.a:retire", VIEWER, SCOPE),
    ]
    .into_iter()
    .map(|(path, auth, query)| call(&cell, "POST", path, query, auth, &body.to_string()))
    .collect();
    for response in &denied {
        assert_eq!(response.status, 403, "{}", response.body);
        assert_eq!(response.body, denied[0].body);
    }
    // Denied before the body is read: malformed bodies are denied the same.
    let response = call(&cell, "POST", "/v1/definitions:install", SCOPE, VIEWER, "{");
    assert_eq!((response.status, &response.body), (403, &denied[0].body));
    // Reads are denied the same to anonymous callers.
    let anonymous = call(&cell, "GET", "/v1/definitions:list", &format!("{SCOPE}&kind=dashboard"), None, "");
    assert_eq!((anonymous.status, &anonymous.body), (403, &denied[0].body));
    // A viewer holds Population, so reads are open to it.
    let (status, page) = get(&cell, "/v1/definitions:list", "&kind=dashboard", VIEWER);
    assert_eq!(status, 200, "{page}");
    assert_eq!(page, json!({ "items": [], "cursor": null }));
}

#[test]
fn install_versions_compare_and_set_and_read_back() {
    let cell = cell(DefinitionKinds::default());
    let id = "studio.dashboard.refinery";
    let (status, first) = install(&cell, id, &dashboard("Refinery"), None);
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["installed"], json!(true));
    assert_eq!(first["id"], json!(id));
    assert_eq!(first["version"], json!(1));
    assert_eq!(first["receipt"]["recordedAtMs"], json!(1_800_000_000_000_u64));
    assert_eq!(first["receipt"]["receiptHash"].as_str().unwrap().len(), 64);
    let commit = first["receipt"]["commitSeq"].as_u64().unwrap();

    // Without expectedVersion the id must be unused; a stale version conflicts.
    for expected in [None, Some(0), Some(2)] {
        let (status, conflict) = install(&cell, id, &dashboard("Again"), expected);
        assert_eq!(status, 409, "{conflict}");
        assert_eq!(conflict["code"], json!("definition.version_conflict"));
    }
    let (status, second) = install(&cell, id, &dashboard("Refinery v2"), Some(1));
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["version"], json!(2));
    assert!(second["receipt"]["commitSeq"].as_u64().unwrap() > commit);

    // An inadmissible install is refused with its diagnostics; nothing changes.
    let (status, refused) = install(&cell, id, &dashboard(""), Some(2));
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["code"], json!("definition.refused"));
    assert_eq!(
        pointers(&refused),
        [("definition.tile_title_invalid".to_owned(), "/title".to_owned())]
    );

    let (status, current) = get(&cell, &format!("/v1/definitions/{id}"), "", VIEWER);
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["kind"], json!("dashboard"));
    assert_eq!(current["version"], json!(2));
    assert_eq!(current["lifecycle"], json!("active"));
    assert_eq!(current["payload"], dashboard("Refinery v2"));
    assert_eq!(current["receipt"], second["receipt"]);
    let (status, past) = get(&cell, &format!("/v1/definitions/{id}"), "&version=1", VIEWER);
    assert_eq!(status, 200, "{past}");
    assert_eq!(past["payload"], dashboard("Refinery"));
    assert_eq!(past["receipt"], first["receipt"]);
    for (path, query) in [
        ("/v1/definitions/studio.dashboard.none", ""),
        (&*format!("/v1/definitions/{id}"), "&version=9"),
    ] {
        let (status, missing) = get(&cell, path, query, VIEWER);
        assert_eq!(status, 404, "{missing}");
        assert_eq!(missing["code"], json!("definition.not_found"));
    }
    // The catalog read is unchanged and still served.
    assert_eq!(get(&cell, "/v1/definitions", "", VIEWER).0, 200);
}

#[test]
fn list_pages_by_id_with_a_bound_cursor_and_retire_keeps_history() {
    let cell = cell(DefinitionKinds::default());
    for id in ["studio.dashboard.c", "studio.dashboard.a", "studio.dashboard.b"] {
        assert_eq!(install(&cell, id, &dashboard(id), None).0, 200);
    }
    let (status, page) = get(&cell, "/v1/definitions:list", "&kind=dashboard&limit=2", VIEWER);
    assert_eq!(status, 200, "{page}");
    let ids = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(&page), ["studio.dashboard.a", "studio.dashboard.b"]);
    assert_eq!(
        page["items"][0],
        json!({ "id": "studio.dashboard.a", "kind": "dashboard", "title": "studio.dashboard.a", "version": 1, "lifecycle": "active", "updatedAtMs": 1_800_000_000_000_u64 })
    );
    let cursor = page["cursor"].as_str().unwrap().to_owned();
    let (_, next) = get(
        &cell,
        "/v1/definitions:list",
        &format!("&kind=dashboard&limit=2&cursor={cursor}"),
        VIEWER,
    );
    assert_eq!(ids(&next), ["studio.dashboard.c"]);
    assert_eq!(next["cursor"], Value::Null);
    // The cursor is bound to kind and include.
    for query in [
        format!("&kind=agent&cursor={cursor}"),
        format!("&kind=dashboard&include=retired&cursor={cursor}"),
        "&kind=dashboard&cursor=not-a-cursor".to_owned(),
    ] {
        let (status, refused) = get(&cell, "/v1/definitions:list", &query, VIEWER);
        assert_eq!(status, 400, "{refused}");
        assert_eq!(refused["code"], json!("definition.cursor_invalid"));
    }
    for query in ["", "&kind=dashboard&limit=0", "&kind=dashboard&limit=101", "&kind=dashboard&include=all"] {
        assert_eq!(get(&cell, "/v1/definitions:list", query, VIEWER).0, 400, "{query}");
    }

    let retire = |expected: u64| {
        post(
            &cell,
            "/v1/definitions/studio.dashboard.b:retire",
            ADMIN,
            &json!({ "expectedVersion": expected }),
        )
    };
    assert_eq!(retire(7).0, 409);
    let (status, retired) = retire(1);
    assert_eq!(status, 200, "{retired}");
    assert_eq!(retired["version"], json!(2));
    assert_eq!(retired["installed"], json!(true));
    let (status, again) = retire(2);
    assert_eq!(status, 422, "{again}");
    assert_eq!(pointers(&again)[0].0, "definition.already_retired");
    let (_, active) = get(&cell, "/v1/definitions:list", "&kind=dashboard", VIEWER);
    assert_eq!(ids(&active), ["studio.dashboard.a", "studio.dashboard.c"]);
    let (_, all) = get(&cell, "/v1/definitions:list", "&kind=dashboard&include=retired", VIEWER);
    assert_eq!(all["items"][1]["lifecycle"], json!("retired"));
    let (_, history) = get(&cell, "/v1/definitions/studio.dashboard.b", "&version=1", VIEWER);
    assert_eq!(history["lifecycle"], json!("active"));
    let missing = post(
        &cell,
        "/v1/definitions/studio.dashboard.zz:retire",
        ADMIN,
        &json!({ "expectedVersion": 1 }),
    );
    assert_eq!(missing.0, 404);
    // A retired id can be installed again as its next version.
    let (status, revived) = install(&cell, "studio.dashboard.b", &dashboard("B"), Some(2));
    assert_eq!((status, revived["version"].clone()), (200, json!(3)));
}

#[test]
fn ids_kinds_and_methods_are_checked() {
    let cell = cell(DefinitionKinds::default());
    for id in ["", "Upper", "9lead", "a/b", &"a".repeat(129)] {
        let body = json!({ "kind": "dashboard", "id": id, "payload": dashboard("A") });
        let (status, refused) = post(&cell, "/v1/definitions:install", ADMIN, &body);
        assert_eq!(status, 400, "{id:?}: {refused}");
        assert_eq!(refused["code"], json!("definition.id_invalid"));
    }
    let body = json!({ "kind": "dashboard", "id": &"a".repeat(128), "payload": dashboard("A") });
    assert_eq!(post(&cell, "/v1/definitions:admit", ADMIN, &body).0, 200);
    // Unknown kinds, and agents before a checker is registered, are typed refusals.
    for kind in ["widget", "agent"] {
        let body = json!({ "kind": kind, "id": "studio.x", "payload": {} });
        let (status, refused) = post(&cell, "/v1/definitions:admit", ADMIN, &body);
        assert_eq!(status, 422, "{refused}");
        assert_eq!(refused["code"], json!("definition.kind_unknown"));
    }
    assert_eq!(call(&cell, "GET", "/v1/definitions:install", SCOPE, ADMIN, "").status, 405);
    assert_eq!(call(&cell, "POST", "/v1/definitions:list", SCOPE, ADMIN, "{}").status, 405);
    let malformed = call(&cell, "POST", "/v1/definitions:install", SCOPE, ADMIN, "{");
    assert_eq!(malformed.status, 400, "{}", malformed.body);
}

/// The agent hook: the registered checker sees the canonical payload and the
/// host's context, and its findings come back with their pointers.
fn stub_agent(payload: &str, context: &AgentAdmissionContext) -> Vec<Diagnostic> {
    let payload: Value = serde_json::from_str(payload).unwrap();
    let model = payload["model"].as_str().unwrap_or_default();
    if context.models.contains(model) {
        return vec![];
    }
    vec![Diagnostic {
        code: DiagnosticCode::new("agent.validation.unknown_model"),
        severity: DiagnosticSeverity::Error,
        message: format!("model {model:?} is not allowed"),
        span: Some(Span {
            pointer: "/model".into(),
            source: None,
        }),
        artifact_id: None,
        remediation: None,
    }]
}

#[test]
fn agents_are_admitted_by_the_registered_checker_against_host_context() {
    let context = AgentAdmissionContext {
        models: BTreeSet::from(["claude-opus-5-5".to_owned()]),
        bindings: vec![],
        laws: BTreeSet::new(),
        limits: ResourceLimitProfile::norn_limits_v1(),
    };
    let cell = cell(DefinitionKinds::default().with_agent(stub_agent, context));
    let agent = |model: &str| json!({ "kind": "agent", "id": "studio.agent.triage", "payload": { "title": "Triage", "model": model } });
    let (status, refused) = post(&cell, "/v1/definitions:install", ADMIN, &agent("gpt-x"));
    assert_eq!(status, 422, "{refused}");
    assert_eq!(
        pointers(&refused),
        [("agent.validation.unknown_model".to_owned(), "/model".to_owned())]
    );
    let (status, installed) = post(&cell, "/v1/definitions:install", ADMIN, &agent("claude-opus-5-5"));
    assert_eq!(status, 200, "{installed}");
    let (_, page) = get(&cell, "/v1/definitions:list", "&kind=agent", VIEWER);
    assert_eq!(page["items"][0]["title"], json!("Triage"));
    // The id is the agent's; a dashboard cannot take it over.
    let (status, taken) = install(&cell, "studio.agent.triage", &dashboard("A"), Some(1));
    assert_eq!(status, 422, "{taken}");
    assert_eq!(pointers(&taken)[0].0, "definition.id_in_use");
}
