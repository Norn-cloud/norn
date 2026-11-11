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

use norn_api::definition_routes::{
    AgentAdmissionContext, AgentToolBinding, DefinitionKinds, connector_well_formed,
};
use norn_api::host::DevHost;
use norn_cell::compute::lineage::{keyed_lineage_handle, keyed_receipt_handle};
use norn_cell::engine::NativeEngine;
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server::{self, Response};
use norn_checker::{Diagnostic, DiagnosticCode, DiagnosticSeverity, Span};
use norn_ir::{MetricDefinition, ResourceLimitProfile};
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
/// The fixture principal holding `admin`.
const ADMIN: Option<&str> = Some("Bearer fixture:viewer@demo");
/// The fixture principal holding `viewer`.
const VIEWER: Option<&str> = Some("Bearer fixture:guest@demo");
/// The host secret the cell's lineage handle key derives from.
const HANDLE_KEY: [u8; 32] = [5; 32];

/// The keyed receipt handle a reader without whole-world Values sees: the
/// cell derives its handle key from the host secret and its scope.
fn receipt_handle(raw: &str) -> String {
    let key = keyed_lineage_handle(&HANDLE_KEY, &analytics::analytics_scope().key());
    keyed_receipt_handle(key.as_bytes(), raw)
}

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
        .with_handle_key(&HANDLE_KEY)
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
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
}

fn get(cell: &Cell, path: &str, query: &str, auth: Option<&str>) -> (u16, Value) {
    let response = call(cell, "GET", path, &format!("{SCOPE}{query}"), auth, "");
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
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
    let (status, before) = get(&cell, "/v1/definitions", "", ADMIN);
    // The analytics cell is provisioned as a governed definitions version.
    assert_eq!(
        (status, before["version"].clone()),
        (200, json!(1)),
        "{before}"
    );
    let body = json!({ "kind": "dashboard", "id": "studio.dashboard.a", "payload": dashboard("Refinery") });
    let (status, admitted) = post(&cell, "/v1/definitions:admit", ADMIN, &body);
    assert_eq!(status, 200, "{admitted}");
    assert_eq!(
        admitted,
        json!({ "admitted": true, "diagnostics": [], "restatement": null, "restatementSha256": null })
    );
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
    let body =
        json!({ "kind": "dashboard", "id": "studio.dashboard.a", "payload": dashboard("A") });
    let denied: Vec<Response> = [
        ("/v1/definitions:admit", VIEWER, SCOPE),
        ("/v1/definitions:install", VIEWER, SCOPE),
        ("/v1/definitions:install", None, SCOPE),
        (
            "/v1/definitions:install",
            Some("Bearer fixture:nobody@demo"),
            SCOPE,
        ),
        // An out-of-scope request is the same denial, even for an admin.
        (
            "/v1/definitions:install",
            ADMIN,
            "orgId=org_demo&environmentId=dev",
        ),
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
    let anonymous = call(
        &cell,
        "GET",
        "/v1/definitions:list",
        &format!("{SCOPE}&kind=dashboard"),
        None,
        "",
    );
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
    assert_eq!(
        first["receipt"]["recordedAtMs"],
        json!(1_800_000_000_000_u64)
    );
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
        [(
            "definition.tile_title_invalid".to_owned(),
            "/title".to_owned()
        )]
    );

    let (status, current) = get(&cell, &format!("/v1/definitions/{id}"), "", VIEWER);
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["kind"], json!("dashboard"));
    assert_eq!(current["version"], json!(2));
    assert_eq!(current["lifecycle"], json!("active"));
    assert_eq!(current["payload"], dashboard("Refinery v2"));
    // The viewer holds Population only: the receipt's keyed handle (#1164).
    let handle = |receipt: &Value| {
        let mut receipt = receipt.clone();
        let raw = receipt["receiptHash"].as_str().unwrap().to_owned();
        receipt["receiptHash"] = json!(receipt_handle(&raw));
        receipt
    };
    assert_eq!(current["receipt"], handle(&second["receipt"]));
    let (status, past) = get(
        &cell,
        &format!("/v1/definitions/{id}"),
        "&version=1",
        VIEWER,
    );
    assert_eq!(status, 200, "{past}");
    assert_eq!(past["payload"], dashboard("Refinery"));
    assert_eq!(past["receipt"], handle(&first["receipt"]));
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
    for id in [
        "studio.dashboard.c",
        "studio.dashboard.a",
        "studio.dashboard.b",
    ] {
        assert_eq!(install(&cell, id, &dashboard(id), None).0, 200);
    }
    let (status, page) = get(
        &cell,
        "/v1/definitions:list",
        "&kind=dashboard&limit=2",
        VIEWER,
    );
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
    for query in [
        "",
        "&kind=dashboard&limit=0",
        "&kind=dashboard&limit=101",
        "&kind=dashboard&include=all",
    ] {
        assert_eq!(
            get(&cell, "/v1/definitions:list", query, VIEWER).0,
            400,
            "{query}"
        );
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
    let (_, all) = get(
        &cell,
        "/v1/definitions:list",
        "&kind=dashboard&include=retired",
        VIEWER,
    );
    assert_eq!(all["items"][1]["lifecycle"], json!("retired"));
    let (_, history) = get(
        &cell,
        "/v1/definitions/studio.dashboard.b",
        "&version=1",
        VIEWER,
    );
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
    assert_eq!(
        call(&cell, "GET", "/v1/definitions:install", SCOPE, ADMIN, "").status,
        405
    );
    assert_eq!(
        call(&cell, "POST", "/v1/definitions:list", SCOPE, ADMIN, "{}").status,
        405
    );
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
        [(
            "agent.validation.unknown_model".to_owned(),
            "/model".to_owned()
        )]
    );
    let (status, installed) = post(
        &cell,
        "/v1/definitions:install",
        ADMIN,
        &agent("claude-opus-5-5"),
    );
    assert_eq!(status, 200, "{installed}");
    let (_, page) = get(&cell, "/v1/definitions:list", "&kind=agent", VIEWER);
    assert_eq!(page["items"][0]["title"], json!("Triage"));
    // The id is the agent's; a dashboard cannot take it over.
    let (status, taken) = install(&cell, "studio.agent.triage", &dashboard("A"), Some(1));
    assert_eq!(status, 422, "{taken}");
    assert_eq!(pointers(&taken)[0].0, "definition.id_in_use");
}

#[test]
fn tool_bindings_follow_the_connector_naming_contract() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let binding =
        AgentToolBinding::new("b1", "executor:github/create_issue@1.2.0", &digest).unwrap();
    assert_eq!(binding.connector, "executor:github/create_issue@1.2.0");
    for connector in [
        "executor:github/create_issue",
        "github/create_issue@1",
        "executor:github@1",
        "executor:/create_issue@1",
        "executor:github/ create_issue@1",
        "executor:git/hub/create_issue@1",
        "",
    ] {
        assert!(!connector_well_formed(connector), "{connector:?}");
        assert!(AgentToolBinding::new("b1", connector, &digest).is_err());
    }
    for digest in [
        "a".repeat(64),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "a".repeat(63)),
    ] {
        assert!(AgentToolBinding::new("b1", "executor:github/create_issue@1", digest).is_err());
    }
    assert!(
        AgentToolBinding::new(
            " ",
            "executor:github/create_issue@1",
            format!("sha256:{}", "0".repeat(64))
        )
        .is_err()
    );
}

/// A stand-in for the shared restatement generator: deterministic in the
/// admitted payload.
fn stub_restate(kind: &str, payload: &str, metrics: &[MetricDefinition]) -> Result<String, String> {
    let payload: Value = serde_json::from_str(payload).map_err(|e| e.to_string())?;
    // The host passes the cell's admitted declarations.
    assert!(metrics.iter().any(|metric| metric.name == "demo.yield"));
    Ok(format!("A {kind} titled {}", payload["title"]))
}

fn sha256(text: &str) -> String {
    norn_cell::retained::payload_sha256(text)
}

#[test]
fn a_signoff_is_verified_against_the_recomputed_restatement_and_recorded_by_the_host() {
    let signed = |cell: &Cell, digest: &str, expected: Option<u64>| {
        let mut body = json!({
            "kind": "dashboard", "id": "studio.dashboard.signed", "payload": dashboard("Signed"),
            "signoff": { "restatementSha256": digest },
        });
        if let Some(expected) = expected {
            body["expectedVersion"] = json!(expected);
        }
        post(cell, "/v1/definitions:install", ADMIN, &body)
    };
    let right = sha256(r#"A dashboard titled "Signed""#);

    // Without a generator the cell cannot verify a sign-off, so it refuses.
    let unverifiable = cell(DefinitionKinds::default());
    let (status, unverifiable_refusal) = signed(&unverifiable, &right, None);
    assert_eq!(status, 422, "{unverifiable_refusal}");
    assert_eq!(
        unverifiable_refusal["code"],
        json!("definition.signoff_unavailable")
    );

    let cell = cell(DefinitionKinds::default().with_restater(stub_restate));
    let (status, refused) = signed(&cell, &sha256("something else"), None);
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["code"], json!("definition.signoff_mismatch"));
    // The server's words for the caller's own payload come back to be signed.
    assert_eq!(
        refused["restatement"],
        json!(r#"A dashboard titled "Signed""#)
    );
    assert_eq!(refused["restatementSha256"], json!(right));
    // Other refusals carry no restatement.
    assert!(unverifiable_refusal.get("restatement").is_none());
    assert_eq!(
        get(&cell, "/v1/definitions/studio.dashboard.signed", "", VIEWER).0,
        404
    );

    let (status, installed) = signed(&cell, &right, None);
    assert_eq!(status, 200, "{installed}");
    let (_, document) = get(&cell, "/v1/definitions/studio.dashboard.signed", "", VIEWER);
    // Signer and time are the host's: the installing principal and the commit.
    assert_eq!(
        document["signoff"],
        json!({
            "signer": "viewer@demo",
            "signedAtMs": installed["receipt"]["recordedAtMs"],
            "restatementSha256": right,
        })
    );
    // A client cannot name a signer or a time.
    let forged = json!({
        "kind": "dashboard", "id": "studio.dashboard.signed", "payload": dashboard("Signed"),
        "expectedVersion": 1, "signoff": { "restatementSha256": right, "signer": "ceo@demo" },
    });
    assert_eq!(
        post(&cell, "/v1/definitions:install", ADMIN, &forged).0,
        400
    );
    // An unsigned next version and a retirement carry no sign-off; history keeps it.
    assert_eq!(
        install(
            &cell,
            "studio.dashboard.signed",
            &dashboard("Signed"),
            Some(1)
        )
        .0,
        200
    );
    let (_, unsigned) = get(&cell, "/v1/definitions/studio.dashboard.signed", "", VIEWER);
    assert_eq!(unsigned["signoff"], Value::Null);
    let (_, first) = get(
        &cell,
        "/v1/definitions/studio.dashboard.signed",
        "&version=1",
        VIEWER,
    );
    assert_eq!(first["signoff"]["restatementSha256"], json!(right));
}

#[test]
fn the_checker_restatement_signs_off_saved_questions_and_dashboards() {
    // The worker's generator: the checker's words, which Studio also shows.
    let cell = cell(
        DefinitionKinds::default().with_restater(norn_api::definition_routes::checker_restatement),
    );
    let admit = |kind: &str, payload: Value| {
        let (status, admitted) = post(
            &cell,
            "/v1/definitions:admit",
            ADMIN,
            &json!({ "kind": kind, "payload": payload }),
        );
        assert_eq!(status, 200, "{admitted}");
        assert_eq!(admitted["admitted"], json!(true), "{admitted}");
        let words = admitted["restatement"].as_str().unwrap().to_owned();
        assert_eq!(admitted["restatementSha256"], json!(sha256(&words)));
        words
    };
    let asked = admit("question", question());
    let shown = admit("dashboard", dashboard("Signed"));
    assert_eq!(shown, format!("Signed\n1. Yield by week: {asked}"));
    for (kind, id, payload, words) in [
        ("question", "studio.question.signed", question(), &asked),
        ("dashboard", "studio.dashboard.signed", dashboard("Signed"), &shown),
    ] {
        let body = json!({
            "kind": kind, "id": id, "payload": payload,
            "signoff": { "restatementSha256": sha256(words) },
        });
        let (status, installed) = post(&cell, "/v1/definitions:install", ADMIN, &body);
        assert_eq!(status, 200, "{installed}");
    }
}

#[test]
fn a_saved_question_is_a_bare_question_ir_admitted_as_a_tile_is() {
    let cell = cell(DefinitionKinds::default());
    let saved = |payload: &Value| {
        let body = json!({ "kind": "question", "id": "studio.question.yield", "payload": payload });
        post(&cell, "/v1/definitions:install", ADMIN, &body)
    };
    let mut bad = question();
    bad["groupBy"] = json!([{"path": {"property": "no_such_property"}, "grain": "week"}]);
    let (status, refused) = saved(&bad);
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["code"], json!("definition.refused"));
    // The same checker as a tile, its pointers straight into the question.
    let tile = json!({ "kind": "dashboard", "id": "studio.dashboard.q", "payload": {
        "title": "Q", "tiles": [{ "id": "t", "title": "T", "question": bad,
        "view": { "kind": "table" }, "layout": { "x": 0, "y": 0, "w": 1, "h": 1 } }] } });
    let (_, as_tile) = post(&cell, "/v1/definitions:admit", ADMIN, &tile);
    let nested: Vec<(String, String)> = pointers(&as_tile)
        .into_iter()
        .map(|(code, pointer)| {
            (
                code,
                pointer.trim_start_matches("/tiles/0/question").to_owned(),
            )
        })
        .collect();
    assert_eq!(pointers(&refused), nested);
    let (status, undecodable) = saved(&json!({ "name": 7 }));
    assert_eq!(status, 422, "{undecodable}");
    assert_eq!(
        pointers(&undecodable),
        [("definition.question_invalid".to_owned(), String::new())]
    );

    let (status, installed) = saved(&question());
    assert_eq!(status, 200, "{installed}");
    let (_, page) = get(&cell, "/v1/definitions:list", "&kind=question", VIEWER);
    assert_eq!(page["items"][0]["title"], json!("studio.tile.yield"));
    let (_, document) = get(&cell, "/v1/definitions/studio.question.yield", "", VIEWER);
    assert_eq!(document["payload"], question());
}

const VECTORS: &str = include_str!(
    "../../../tools/norn-ts/src/builder/fixtures/binding-manifest-digest.vectors.json"
);
const ROLES: [&str; 4] = ["viewer", "member", "admin", "owner"];

/// JCS (RFC 8785) over CONNECT's golden vectors, from the raw manifest text.
#[test]
fn manifest_digests_match_the_shared_jcs_vectors() {
    use norn_api::binding_definitions::derived_version;
    use norn_api::jcs::canonicalize_text;
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    assert_eq!(
        vectors["format"],
        json!("norn.binding-manifest-digest.vectors/1")
    );
    let vectors = vectors["vectors"].as_array().unwrap();
    assert!(vectors.len() >= 5);
    for vector in vectors {
        let name = vector["name"].as_str().unwrap();
        let canonical = canonicalize_text(vector["manifestJson"].as_str().unwrap()).unwrap();
        assert_eq!(canonical, vector["canonical"].as_str().unwrap(), "{name}");
        let digest = format!("sha256:{}", sha256(&canonical));
        assert_eq!(digest, vector["manifestDigest"].as_str().unwrap(), "{name}");
        assert_eq!(
            derived_version(&digest),
            vector["version"].as_str().unwrap(),
            "{name}"
        );
    }
}

fn binding(label: &str, run_role: &str, mapping: &Value) -> Value {
    let input = json!({ "type": "object", "properties": {
        "repo": { "type": "string" }, "limit": { "type": "integer" }, "state": { "enum": ["open", "closed"] } } });
    binding_over(&input, label, run_role, mapping)
}

/// A binding over `input` with the host's own JCS digest of its manifest.
fn binding_over(input: &Value, label: &str, run_role: &str, mapping: &Value) -> Value {
    let output = json!({ "type": "object" });
    let digest = norn_api::binding_definitions::manifest_digest(
        "executor",
        "github",
        "list_issues",
        input,
        &output,
    )
    .unwrap();
    json!({
        "connector": { "gateway": "executor", "origin": "https://executor.example", "app": "github",
            "tool": "list_issues", "version": norn_api::binding_definitions::derived_version(&digest) },
        "inputSchema": input, "outputSchema": output, "manifestDigest": digest,
        "label": label, "inputMapping": mapping, "runRole": run_role,
    })
}

fn install_binding(cell: &Cell, id: &str, payload: &Value, expected: Option<u64>) -> (u16, Value) {
    let mut body = json!({ "kind": "binding", "id": id, "payload": payload });
    if let Some(expected) = expected {
        body["expectedVersion"] = json!(expected);
    }
    post(cell, "/v1/definitions:install", ADMIN, &body)
}

#[test]
fn bindings_are_admitted_against_their_recomputed_digest_and_declared_roles() {
    let cell = cell(DefinitionKinds::default().with_roles(ROLES));
    let good = binding(
        "Open issues",
        "member",
        &json!({ "repo": { "fixed": "norn/norn" }, "state": { "fixed": "open" } }),
    );
    let (status, installed) = install_binding(&cell, "studio.binding.issues", &good, None);
    assert_eq!(status, 200, "{installed}");
    let (_, page) = get(&cell, "/v1/definitions:list", "&kind=binding", VIEWER);
    assert_eq!(page["items"][0]["title"], json!("Open issues"));

    let mut tampered = good.clone();
    tampered["inputSchema"]["properties"]["repo"]["type"] = json!("number");
    let mut versioned = good.clone();
    versioned["connector"]["version"] = json!("sha256-000000000000");
    let mut unknown = good.clone();
    unknown["extra"] = json!(true);
    for (payload, expected) in [
        (
            tampered,
            ("definition.binding_digest_mismatch", "/manifestDigest"),
        ),
        (
            versioned,
            ("definition.binding_version_mismatch", "/connector/version"),
        ),
        (
            binding("  ", "member", &json!({})),
            ("definition.binding_label_invalid", "/label"),
        ),
        (
            binding("L", "root", &json!({})),
            ("definition.binding_role_unknown", "/runRole"),
        ),
        (
            binding("L", "member", &json!({ "nope": { "fixed": 1 } })),
            (
                "definition.binding_mapping_unknown_field",
                "/inputMapping/nope",
            ),
        ),
        (
            binding("L", "member", &json!({ "limit": { "fixed": "ten" } })),
            (
                "definition.binding_mapping_invalid",
                "/inputMapping/limit/fixed",
            ),
        ),
        (
            binding("L", "member", &json!({ "state": { "fixed": "merged" } })),
            (
                "definition.binding_mapping_invalid",
                "/inputMapping/state/fixed",
            ),
        ),
        (
            binding("L", "member", &json!({ "repo": "norn/norn" })),
            ("definition.binding_mapping_invalid", "/inputMapping/repo"),
        ),
        (unknown, ("definition.binding_invalid_payload", "/extra")),
    ] {
        let (status, refused) = install_binding(&cell, "studio.binding.bad", &payload, None);
        assert_eq!(status, 422, "{expected:?}: {refused}");
        assert!(
            pointers(&refused).contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "{expected:?} in {refused}"
        );
    }
    let mut blank = good.clone();
    blank["connector"]["app"] = json!(" ");
    let (_, refused) = install_binding(&cell, "studio.binding.bad", &blank, None);
    assert!(pointers(&refused).contains(&(
        "definition.binding_connector_invalid".to_owned(),
        "/connector/app".to_owned()
    )));
    // A cell that declares no roles admits no binding.
    let bare = crate::cell(DefinitionKinds::default());
    let (status, refused) = install_binding(&bare, "studio.binding.issues", &good, None);
    assert_eq!(status, 422, "{refused}");
}

/// An agent may name only the cell's installed, active bindings, by the
/// connector string the browser check uses.
fn binding_agent(payload: &str, context: &AgentAdmissionContext) -> Vec<Diagnostic> {
    let payload: Value = serde_json::from_str(payload).unwrap();
    let tool = payload["tools"][0].as_str().unwrap_or_default();
    match context.bindings.iter().find(|binding| binding.id == tool) {
        Some(binding)
            if binding
                .connector
                .starts_with("executor:github/list_issues@sha256-") =>
        {
            vec![]
        }
        _ => vec![Diagnostic {
            code: DiagnosticCode::new("agent.validation.unbound_tool"),
            severity: DiagnosticSeverity::Error,
            message: format!("{tool} is not bound"),
            span: Some(Span {
                pointer: "/tools/0".into(),
                source: None,
            }),
            artifact_id: None,
            remediation: None,
        }],
    }
}

#[test]
fn agents_see_installed_active_bindings_only() {
    let context = AgentAdmissionContext {
        models: BTreeSet::new(),
        bindings: vec![],
        laws: BTreeSet::new(),
        limits: ResourceLimitProfile::norn_limits_v1(),
    };
    let cell = cell(
        DefinitionKinds::default()
            .with_roles(ROLES)
            .with_agent(binding_agent, context),
    );
    let agent = json!({ "kind": "agent", "id": "studio.agent.triage",
        "payload": { "title": "Triage", "tools": ["studio.binding.issues"] } });
    let (status, unbound) = post(&cell, "/v1/definitions:admit", ADMIN, &agent);
    assert_eq!(
        (status, unbound["admitted"].clone()),
        (200, json!(false)),
        "{unbound}"
    );
    assert_eq!(
        install_binding(
            &cell,
            "studio.binding.issues",
            &binding("Issues", "member", &json!({})),
            None
        )
        .0,
        200
    );
    let (_, bound) = post(&cell, "/v1/definitions:admit", ADMIN, &agent);
    assert_eq!(bound["admitted"], json!(true), "{bound}");
    let retire = json!({ "expectedVersion": 1 });
    assert_eq!(
        post(
            &cell,
            "/v1/definitions/studio.binding.issues:retire",
            ADMIN,
            &retire
        )
        .0,
        200
    );
    let (_, retired) = post(&cell, "/v1/definitions:admit", ADMIN, &agent);
    assert_eq!(
        pointers(&retired),
        [(
            "agent.validation.unbound_tool".to_owned(),
            "/tools/0".to_owned()
        )]
    );
}

fn reworded(kind: &str, payload: &str, _metrics: &[MetricDefinition]) -> Result<String, String> {
    let payload: Value = serde_json::from_str(payload).map_err(|e| e.to_string())?;
    Ok(format!("The {kind} called {}", payload["title"]))
}

/// Admit and GET return the server's restatement; GET recomputes it at read
/// time, so a recorded sign-off whose words now restate differently shows it.
#[test]
fn admit_and_get_return_the_recomputed_restatement() {
    let mut cell = cell(DefinitionKinds::default().with_restater(stub_restate));
    let words = r#"A dashboard titled "Signed""#;
    let body = json!({ "kind": "dashboard", "id": "studio.dashboard.signed", "payload": dashboard("Signed") });
    let (status, admitted) = post(&cell, "/v1/definitions:admit", ADMIN, &body);
    assert_eq!(status, 200, "{admitted}");
    assert_eq!(admitted["restatement"], json!(words));
    assert_eq!(admitted["restatementSha256"], json!(sha256(words)));
    // Nothing to sign when the payload is not admitted.
    let bad =
        json!({ "kind": "dashboard", "id": "studio.dashboard.signed", "payload": dashboard("") });
    let (_, refused) = post(&cell, "/v1/definitions:admit", ADMIN, &bad);
    assert_eq!(
        (
            refused["restatement"].clone(),
            refused["restatementSha256"].clone()
        ),
        (Value::Null, Value::Null)
    );

    let mut signed = body;
    signed["signoff"] = json!({ "restatementSha256": sha256(words) });
    assert_eq!(
        post(&cell, "/v1/definitions:install", ADMIN, &signed).0,
        200
    );
    let read = |cell: &Cell| get(cell, "/v1/definitions/studio.dashboard.signed", "", VIEWER).1;
    let document = read(&cell);
    assert_eq!(document["restatement"], json!(words));
    assert_eq!(
        document["restatementSha256"],
        document["signoff"]["restatementSha256"]
    );

    // The same stored payload now restates differently: the recorded
    // sign-off stays as it was, and the read shows the drift.
    cell.kinds = DefinitionKinds::default().with_restater(reworded);
    let drifted = read(&cell);
    assert_eq!(
        drifted["restatement"],
        json!(r#"The dashboard called "Signed""#)
    );
    assert_eq!(drifted["signoff"], document["signoff"]);
    assert_ne!(
        drifted["restatementSha256"],
        drifted["signoff"]["restatementSha256"]
    );
    // A cell without a restater has none to give.
    cell.kinds = DefinitionKinds::default();
    let bare = read(&cell);
    assert_eq!(
        (
            bare["restatement"].clone(),
            bare["restatementSha256"].clone()
        ),
        (Value::Null, Value::Null)
    );
}

/// #1164 (CWE-200): a retained definition's receipt commits to the ledger
/// head, so to every earlier event's values. The analytics cell's K1 load
/// commits Values-gated records before any definition is installed, so a
/// Population-only reader gets the keyed receipt handle, on the current and a
/// historical version alike, while the definition itself stays readable to
/// it; a reader with Values over every object type gets the raw hash.
#[test]
fn a_population_reader_never_sees_a_definition_receipt_hash() {
    let cell = cell(DefinitionKinds::default());
    let id = "studio.dashboard.receipts";
    let (status, first) = install(&cell, id, &dashboard("Receipts"), None);
    assert_eq!(status, 200, "{first}");
    let (status, second) = install(&cell, id, &dashboard("Receipts v2"), Some(1));
    assert_eq!(status, 200, "{second}");
    let raws: Vec<String> = [&first, &second]
        .map(|installed| {
            installed["receipt"]["receiptHash"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .to_vec();
    for raw in &raws {
        assert_eq!(raw.len(), 64, "{raw}");
    }
    for (query, raw, title) in [
        ("&version=1", &raws[0], "Receipts"),
        ("", &raws[1], "Receipts v2"),
    ] {
        let path = format!("/v1/definitions/{id}");
        let full = call(&cell, "GET", &path, &format!("{SCOPE}{query}"), ADMIN, "");
        let document: Value = serde_json::from_str(&full.body).unwrap();
        assert_eq!(full.status, 200, "{document}");
        assert_eq!(document["receipt"]["receiptHash"], json!(raw));

        let viewer = call(&cell, "GET", &path, &format!("{SCOPE}{query}"), VIEWER, "");
        let document: Value = serde_json::from_str(&viewer.body).unwrap();
        assert_eq!(viewer.status, 200, "{document}");
        assert_eq!(document["payload"], dashboard(title));
        assert_eq!(
            document["receipt"]["receiptHash"],
            json!(receipt_handle(raw))
        );
        for any in &raws {
            assert!(!viewer.body.contains(any.as_str()), "{}", viewer.body);
        }
    }
    let listed = call(
        &cell,
        "GET",
        "/v1/definitions:list",
        &format!("{SCOPE}&kind=dashboard&include=retired"),
        VIEWER,
        "",
    );
    assert_eq!(listed.status, 200, "{}", listed.body);
    for raw in &raws {
        assert!(!listed.body.contains(raw.as_str()), "{}", listed.body);
    }

    // A cell without a question runtime holds no handle key: no receipt hash.
    let bare = crate::cell(DefinitionKinds::default().with_roles(ROLES));
    let good = binding("Open issues", "member", &json!({}));
    let body = json!({ "kind": "binding", "id": "studio.binding.bare", "payload": good });
    for (method, path, auth, body) in [
        ("POST", "/v1/definitions:install", ADMIN, body.to_string()),
        (
            "GET",
            "/v1/definitions/studio.binding.bare",
            VIEWER,
            String::new(),
        ),
        (
            "GET",
            "/v1/definitions/studio.binding.bare",
            ADMIN,
            String::new(),
        ),
    ] {
        let request = server::Request {
            method,
            path,
            query: SCOPE,
            authorization: auth,
            now: 1_800_000_000,
            body: &body,
        };
        let response =
            server::handle_all(&bare.host, &bare.principals, None, &bare.kinds, &request);
        let document: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(response.status, 200, "{document}");
        assert_eq!(
            document["receipt"]["receiptHash"],
            Value::Null,
            "{document}"
        );
    }
}

/// #1164 (CWE-354): a binding is checked and digested as submitted (JCS) but
/// stored as Norn1, which NFC-normalizes strings and member names. A
/// decomposed string or property name is refused, so the checked, digested
/// and stored manifest are the same bytes; the composed form installs and
/// reads back with a digest that still recomputes.
#[test]
fn binding_strings_and_member_names_are_nfc_so_the_stored_manifest_keeps_its_digest() {
    let cell = cell(DefinitionKinds::default().with_roles(ROLES));
    let decomposed_enum = json!({ "type": "object", "properties": {
        "state": { "enum": ["e\u{301}"] } } });
    let decomposed_name = json!({ "type": "object", "properties": {
        "e\u{301}tat": { "type": "string" } } });
    for (input, pointer) in [
        (decomposed_enum, "/inputSchema/properties/state/enum/0"),
        (decomposed_name, "/inputSchema/properties/e\u{301}tat"),
    ] {
        let payload = binding_over(&input, "Accents", "member", &json!({}));
        // The submitted digest is the correct JCS one over the decomposed text.
        assert!(
            payload["manifestDigest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        let (status, refused) = install_binding(&cell, "studio.binding.nfd", &payload, None);
        assert_eq!(status, 422, "{refused}");
        assert_eq!(
            pointers(&refused),
            [("definition.binding_not_nfc".to_owned(), pointer.to_owned())],
            "{refused}"
        );
        let (status, missing) = get(&cell, "/v1/definitions/studio.binding.nfd", "", VIEWER);
        assert_eq!(status, 404, "{missing}");
    }

    let composed = json!({ "type": "object", "properties": {
        "\u{e9}tat": { "type": "string" }, "state": { "enum": ["\u{e9}"] } } });
    let payload = binding_over(
        &composed,
        "Accents",
        "member",
        &json!({ "state": { "fixed": "\u{e9}" } }),
    );
    let (status, installed) = install_binding(&cell, "studio.binding.nfc", &payload, None);
    assert_eq!(status, 200, "{installed}");
    let (status, read) = get(&cell, "/v1/definitions/studio.binding.nfc", "", VIEWER);
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["payload"], payload);
    let stored = &read["payload"];
    let recomputed = norn_api::binding_definitions::manifest_digest(
        stored["connector"]["gateway"].as_str().unwrap(),
        stored["connector"]["app"].as_str().unwrap(),
        stored["connector"]["tool"].as_str().unwrap(),
        &stored["inputSchema"],
        &stored["outputSchema"],
    )
    .unwrap();
    assert_eq!(stored["manifestDigest"], json!(recomputed));
}
