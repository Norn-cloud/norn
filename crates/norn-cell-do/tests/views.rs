//! SAVED VIEWS (#1424): end-to-end integration tests over real provisioned
//! tenant cells (`Tenant<NativeEngine>`) on two unrelated domains:
//!
//! 1. **Application Funnel (`funnel.Application`)**: issue #1424's exact
//!    scenario ("Applications needing help" view, server-admitted Space and
//!    offered actions, per-row `offers` filtered by lifecycle state AND the
//!    tenant PDP, single-record GET wire preservation, retire & undo).
//! 2. **Equipment Fleet Tickets (`fleet.Ticket`)**: an unrelated domain proving
//!    zero domain coupling in saved views and per-row `offers`.

use std::error::Error;
use std::path::Path;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::provision::PROVISION_PATH;
use norn_api::scope::CellScope;
use norn_cell::StoreOptions;
use norn_cell::engine::NativeEngine;
use norn_cell_do::base;
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::server::{Request, Response};
use norn_cell_do::tenant::Tenant;
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const ISSUER: &str = "https://studio-preview.example";
const NOW: u64 = 1_800_000_000;
const HANDLE_KEY: [u8; 32] = [11; 32];
const QUERY: &str = "orgId=org_a&environmentId=prod";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[3; 32])
}

fn jwks() -> String {
    json!({ "keys": [{
        "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
        "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes())
    }] })
    .to_string()
}

fn user(role: &str) -> String {
    let claims = json!({
        "iss": ISSUER, "aud": "norn-host", "sub": format!("user-{role}-of-org_a"),
        "iat": NOW.saturating_sub(10), "exp": NOW.saturating_add(290),
        "norn": { "kind": "user", "org": "org_a", "role": role, "amr": "password" }
    });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let sig = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    )
}

fn scope() -> CellScope {
    CellScope {
        org_id: "org_a".into(),
        environment_id: "prod".into(),
    }
}

fn ptr<'a>(value: &'a Value, pointer: &str) -> TestResult<&'a Value> {
    value
        .pointer(pointer)
        .ok_or_else(|| format!("missing pointer {pointer} in {value}").into())
}

fn ptr_u64(value: &Value, pointer: &str) -> TestResult<u64> {
    ptr(value, pointer)?
        .as_u64()
        .ok_or_else(|| format!("expected u64 at {pointer} in {value}").into())
}

fn ptr_str<'a>(value: &'a Value, pointer: &str) -> TestResult<&'a str> {
    ptr(value, pointer)?
        .as_str()
        .ok_or_else(|| format!("expected str at {pointer} in {value}").into())
}

fn ptr_array<'a>(value: &'a Value, pointer: &str) -> TestResult<&'a [Value]> {
    ptr(value, pointer)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("expected array at {pointer} in {value}").into())
}

struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
    now: u64,
}

impl Cell {
    fn call_query(
        &mut self,
        method: &str,
        path: &str,
        extra_query: &str,
        auth: Option<&str>,
        body: &str,
    ) -> Response {
        let query = if extra_query.is_empty() {
            QUERY.to_owned()
        } else {
            format!("{QUERY}&{extra_query}")
        };
        self.tenant.handle(
            &self.tokens,
            &Request {
                method,
                path,
                query: &query,
                authorization: auth,
                now: self.now,
                body,
            },
        )
    }

    fn post(&mut self, path: &str, role: &str, body: &Value) -> (u16, Value) {
        let resp = self.call_query("POST", path, "", Some(&user(role)), &body.to_string());
        let val = serde_json::from_str(&resp.body).unwrap_or_else(|_| json!({ "raw": resp.body }));
        (resp.status, val)
    }

    fn get(&mut self, path: &str, extra_query: &str, role: &str) -> (u16, Value) {
        let resp = self.call_query("GET", path, extra_query, Some(&user(role)), "");
        let val = serde_json::from_str(&resp.body).unwrap_or_else(|_| json!({ "raw": resp.body }));
        (resp.status, val)
    }

    fn action(&mut self, action_id: &str, role: &str, input: &Value, key: &str) -> (u16, Value) {
        self.post(
            &format!("/v1/actions/{action_id}"),
            role,
            &json!({ "input": input, "idempotencyKey": key }),
        )
    }
}

fn provisioned(path: &Path, artifacts: &[Value]) -> TestResult<Cell> {
    let file = path.to_owned();
    let tenant = Tenant::open(
        scope(),
        &DEMO_GRANTS,
        Box::new(move || {
            NativeEngine::open(&file, StoreOptions::default()).map_err(|error| error.to_string())
        }),
        Some(&HANDLE_KEY),
    )?;
    let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope(), tenant.caller(), NOW)
        .map_err(|refusal| format!("{refusal:?}"))?;
    let mut cell = Cell {
        tenant,
        tokens,
        now: NOW,
    };
    let body = json!({
        "baseSha256": base::trusted_base()?.sha256(),
        "artifacts": artifacts,
    });
    let (status, resp) = cell.post(PROVISION_PATH, "owner", &body);
    assert_eq!(status, 200, "provision failed: {resp}");
    Ok(cell)
}

fn scope_json() -> Value {
    json!({ "orgId": "org_a", "environmentId": "prod" })
}

fn provenance() -> Value {
    json!({ "source": "authoring_sdk", "sourcePath": "tests/views.rs" })
}

fn policy_artifact(id: &str, namespace: &str, resource: &str, actions: &[&str]) -> Value {
    let policy_id = id.trim_end_matches(".policy");
    let mut all_actions: Vec<&str> = actions.to_vec();
    all_actions.push("norn.read");
    json!({
        "artifactId": id, "dependencies": [], "irVersion": "0.1", "kind": "policy",
        "lifecycle": "active", "name": format!("{namespace} access"), "namespace": namespace,
        "payload": {
            "accessVersion": "0.1", "policyId": policy_id,
            "rules": [{
                "ruleId": "operators", "actions": all_actions, "resourceType": resource,
                "scope": { "dimensions": ["org", "environment"] },
                "allOf": [{ "role": { "anyOf": ["admin", "member"] } }]
            }]
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    })
}

fn transition_action(
    (artifact_id, action_id, title, namespace): (&str, &str, &str, &str),
    (entity_kind, entity_artifact, lifecycle, transition): (&str, &str, &str, &str),
    access: &Value,
) -> Value {
    json!({
        "artifactId": artifact_id,
        "dependencies": [{ "artifactId": entity_artifact, "kind": "entity", "required": true, "version": "1.0.0" }],
        "irVersion": "0.1", "kind": "action", "lifecycle": "active",
        "name": title, "namespace": namespace,
        "payload": {
            "actionId": action_id, "title": title,
            "access": access, "idempotent": true, "ledgered": true,
            "scopeRequired": ["orgId", "environmentId"],
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "required": ["id"], "properties": { "id": { "type": "string" } }
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "writes": [{ "entityKind": entity_kind, "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition", "objectKind": entity_kind,
                "objectIdField": "id", "entityArtifactId": entity_artifact,
                "entityArtifactVersion": "1.0.0", "lifecycle": lifecycle, "transition": transition
            }
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    })
}

fn funnel_entity_and_law() -> [Value; 2] {
    let entity = json!({
        "artifactId": "funnel.Application.entity", "dependencies": [], "irVersion": "0.1",
        "kind": "entity", "lifecycle": "active", "name": "Application", "namespace": "funnel",
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "funnel.Application",
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "applicant_name", "type": { "kind": "text" } },
                    { "id": "stage", "default": "started", "type": { "kind": "enum", "values": [
                        { "value": "started" }, { "value": "in_review" },
                        { "value": "stalled" }, { "value": "assisted" }, { "value": "completed" }
                    ] } },
                    { "id": "score", "type": { "kind": "integer" } }
                ],
                "lifecycles": [{
                    "property": "stage",
                    "initial": ["started"],
                    "terminal": ["completed"],
                    "transitions": [
                        { "id": "begin_review", "from": ["started"], "to": "in_review" },
                        { "id": "mark_stalled", "from": ["started", "in_review"], "to": "stalled" },
                        { "id": "offer_assistance", "from": ["in_review", "stalled"], "to": "assisted" },
                        { "id": "complete", "from": ["in_review", "assisted"], "to": "completed" }
                    ]
                }]
            }
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    });
    let law = json!({
        "artifactId": "funnel.assist_score_cap.law",
        "dependencies": [{ "artifactId": "funnel.Application.entity", "kind": "entity", "required": true, "version": "1.0.0" }],
        "irVersion": "0.1", "kind": "law", "lifecycle": "active",
        "name": "Only applications below score 80 may receive assistance",
        "namespace": "funnel",
        "payload": {
            "id": "funnel.assist_score_cap", "lawVersion": "0.1",
            "modality": { "kind": "always" }, "owner": "funnel",
            "provenance": provenance(),
            "rationale": "High-scoring applications (>= 80) do not qualify for assisted triage.",
            "response": { "kind": "predicate", "expression": "self.stage != 'assisted' or self.score < 80" },
            "scope": { "objectTypes": ["funnel.Application"], "root": { "id": "funnel.Application", "kind": "entity" } },
            "severity": "error", "subject": "funnel.Application"
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    });
    [entity, law]
}

/// Synthetic Domain 1 (issue #1424): Application Funnel.
fn funnel_artifacts() -> Vec<Value> {
    let policy_id = "funnel.application.access.policy";
    let access = json!({ "artifactId": policy_id, "version": "1.0.0" });
    let [entity, law] = funnel_entity_and_law();
    let create = json!({
        "artifactId": "funnel.application.create.action",
        "dependencies": [{ "artifactId": "funnel.Application.entity", "kind": "entity", "required": true, "version": "1.0.0" }],
        "irVersion": "0.1", "kind": "action", "lifecycle": "active",
        "name": "Create application", "namespace": "funnel",
        "payload": {
            "actionId": "funnel.application.create", "title": "Create application",
            "access": access, "idempotent": true, "ledgered": true,
            "scopeRequired": ["orgId", "environmentId"],
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "required": ["id", "applicant_name", "score"],
                "properties": {
                    "id": { "type": "string" },
                    "applicant_name": { "type": "string" },
                    "score": { "type": "integer" }
                }
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "required": ["id"], "properties": { "id": { "type": "string" } } },
            "writes": [{ "entityKind": "funnel.Application", "change": "created" }],
            "effectProgram": {
                "kind": "upsert_input_object", "objectKind": "funnel.Application",
                "objectIdField": "id", "entityArtifactId": "funnel.Application.entity",
                "entityArtifactVersion": "1.0.0", "writeMode": { "kind": "create_only" }
            }
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    });
    let mark_stalled = transition_action(
        (
            "funnel.application.mark_stalled.action",
            "funnel.application.mark_stalled",
            "Mark stalled",
            "funnel",
        ),
        (
            "funnel.Application",
            "funnel.Application.entity",
            "stage",
            "mark_stalled",
        ),
        &access,
    );
    let offer_assistance = transition_action(
        (
            "funnel.application.offer_assistance.action",
            "funnel.application.offer_assistance",
            "Offer assistance",
            "funnel",
        ),
        (
            "funnel.Application",
            "funnel.Application.entity",
            "stage",
            "offer_assistance",
        ),
        &access,
    );
    let policy = policy_artifact(
        policy_id,
        "funnel",
        "funnel.Application",
        &[
            "funnel.application.create",
            "funnel.application.mark_stalled",
            "funnel.application.offer_assistance",
        ],
    );
    vec![entity, law, create, mark_stalled, offer_assistance, policy]
}

/// Synthetic Domain 2 (unrelated domain proving genericity): Equipment Fleet Tickets.
fn fleet_artifacts() -> Vec<Value> {
    let policy_id = "fleet.ticket.access.policy";
    let access = json!({ "artifactId": policy_id, "version": "1.0.0" });
    let entity = json!({
        "artifactId": "fleet.Ticket.entity", "dependencies": [], "irVersion": "0.1",
        "kind": "entity", "lifecycle": "active", "name": "Fleet ticket", "namespace": "fleet",
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "fleet.Ticket",
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "asset_code", "type": { "kind": "text" } },
                    { "id": "status", "default": "open", "type": { "kind": "enum", "values": [
                        { "value": "open" }, { "value": "waiting_parts" },
                        { "value": "escalated" }, { "value": "closed" }
                    ] } },
                    { "id": "downtime_hours", "type": { "kind": "integer" } }
                ],
                "lifecycles": [{
                    "property": "status",
                    "initial": ["open"],
                    "terminal": ["closed"],
                    "transitions": [
                        { "id": "wait_parts", "from": ["open"], "to": "waiting_parts" },
                        { "id": "escalate", "from": ["open", "waiting_parts"], "to": "escalated" },
                        { "id": "close", "from": ["open", "waiting_parts", "escalated"], "to": "closed" }
                    ]
                }]
            }
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    });
    let create = json!({
        "artifactId": "fleet.ticket.create.action",
        "dependencies": [{ "artifactId": "fleet.Ticket.entity", "kind": "entity", "required": true, "version": "1.0.0" }],
        "irVersion": "0.1", "kind": "action", "lifecycle": "active",
        "name": "Create ticket", "namespace": "fleet",
        "payload": {
            "actionId": "fleet.ticket.create", "title": "Create ticket",
            "access": access, "idempotent": true, "ledgered": true,
            "scopeRequired": ["orgId", "environmentId"],
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "required": ["id", "asset_code", "downtime_hours"],
                "properties": {
                    "id": { "type": "string" },
                    "asset_code": { "type": "string" },
                    "downtime_hours": { "type": "integer" }
                }
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "required": ["id"], "properties": { "id": { "type": "string" } } },
            "writes": [{ "entityKind": "fleet.Ticket", "change": "created" }],
            "effectProgram": {
                "kind": "upsert_input_object", "objectKind": "fleet.Ticket",
                "objectIdField": "id", "entityArtifactId": "fleet.Ticket.entity",
                "entityArtifactVersion": "1.0.0", "writeMode": { "kind": "create_only" }
            }
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    });
    let wait_parts = transition_action(
        (
            "fleet.ticket.wait_parts.action",
            "fleet.ticket.wait_parts",
            "Mark waiting parts",
            "fleet",
        ),
        (
            "fleet.Ticket",
            "fleet.Ticket.entity",
            "status",
            "wait_parts",
        ),
        &access,
    );
    let escalate = transition_action(
        (
            "fleet.ticket.escalate.action",
            "fleet.ticket.escalate",
            "Escalate ticket",
            "fleet",
        ),
        ("fleet.Ticket", "fleet.Ticket.entity", "status", "escalate"),
        &access,
    );
    let policy = policy_artifact(
        policy_id,
        "fleet",
        "fleet.Ticket",
        &[
            "fleet.ticket.create",
            "fleet.ticket.wait_parts",
            "fleet.ticket.escalate",
        ],
    );
    vec![entity, create, wait_parts, escalate, policy]
}

fn seed_funnel_applications(cell: &mut Cell, entries: &[(&str, &str, i64)], stall: &[&str]) {
    for (id, name, score) in entries {
        let (st, resp) = cell.action(
            "funnel.application.create",
            "admin",
            &json!({ "id": id, "applicant_name": name, "score": score }),
            &format!("create-{id}"),
        );
        assert_eq!(st, 200, "{resp}");
    }
    for id in stall {
        let (st, resp) = cell.action(
            "funnel.application.mark_stalled",
            "admin",
            &json!({ "id": id }),
            &format!("stall-{id}"),
        );
        assert_eq!(st, 200, "{resp}");
    }
}

fn assert_retire_and_undo(
    cell: &mut Cell,
    kind: &str,
    id: &str,
    payload: &Value,
    expected_version: u64,
) -> TestResult {
    let (status, retired) = cell.post(
        &format!("/v1/definitions/{id}:retire"),
        "admin",
        &json!({ "expectedVersion": expected_version }),
    );
    assert_eq!(status, 200, "{retired}");
    let retired_version = ptr_u64(&retired, "/version")?;
    let (status, restored) = cell.post(
        "/v1/definitions:install",
        "admin",
        &json!({ "kind": kind, "id": id, "payload": payload, "expectedVersion": retired_version }),
    );
    assert_eq!(status, 200, "{restored}");
    assert_eq!(
        ptr_u64(&restored, "/version")?,
        retired_version.saturating_add(1)
    );
    Ok(())
}

fn assert_assisted_row_drops_offer(cell: &mut Cell, id: &str) -> TestResult {
    let (status, single) = cell.get(&format!("/v1/objects/funnel.Application/{id}"), "", "admin");
    assert_eq!(status, 200, "{single}");
    assert_eq!(ptr_str(&single, "/values/stage")?, "assisted");
    assert!(
        single.get("offers").is_none(),
        "single-record GET preserves its base wire shape without offers: {single}"
    );
    let (status, actions_after) = cell.get(
        "/v1/actions",
        &format!("objectKind=funnel.Application&objectId={id}"),
        "admin",
    );
    assert_eq!(status, 200, "{actions_after}");
    assert!(
        !ptr_array(&actions_after, "/items")?.iter().any(|a| {
            a.get("actionId").and_then(Value::as_str) == Some("funnel.application.offer_assistance")
        }),
        "assisted row should no longer be offered offer_assistance: {actions_after}"
    );
    let (status, page_after) = cell.get("/v1/objects/funnel.Application", "", "admin");
    assert_eq!(status, 200, "{page_after}");
    let row = ptr_array(&page_after, "/records")?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
        .ok_or("missing record in list after assist")?;
    assert!(
        !ptr_array(row, "/offers")?
            .iter()
            .any(|a| a == "funnel.application.offer_assistance"),
        "assisted row should no longer have offer_assistance in list offers: {row}"
    );
    Ok(())
}

fn assert_viewer_rows_omit_offers(cell: &mut Cell) -> TestResult {
    let (status, viewer_page) = cell.get("/v1/objects/funnel.Application", "", "viewer");
    assert_eq!(status, 200, "{viewer_page}");
    for row in ptr_array(&viewer_page, "/records")? {
        assert!(
            row.get("offers").is_none(),
            "withheld rows must omit offers: {row}"
        );
    }
    Ok(())
}

#[test]
fn saved_view_admits_space_and_actions_and_objects_return_per_row_offers() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut cell = provisioned(&dir.path().join("funnel.sqlite"), &funnel_artifacts())?;

    let bad_view = json!({
        "kind": "view",
        "id": "studio.view.funnel_application.bad",
        "payload": {
            "title": "Bad actions view",
            "objectType": "funnel.Application",
            "rowActions": ["funnel.application.does_not_exist"]
        }
    });
    let (status, admitted) = cell.post("/v1/definitions:admit", "admin", &bad_view);
    assert_eq!(status, 200, "{admitted}");
    assert_eq!(ptr(&admitted, "/admitted")?, &json!(false));
    assert_eq!(
        ptr_str(&admitted, "/diagnostics/0/span/pointer")?,
        "/rowActions/0"
    );

    let view_payload = json!({
        "title": "Applications needing help",
        "description": "Stalled applications whose score is below 60.",
        "space": "Operations",
        "objectType": "funnel.Application",
        "filter": { "all": [
            { "property": "stage", "op": "eq", "value": "stalled" },
            { "property": "score", "op": "lt", "value": "60" }
        ] },
        "sort": "score:asc",
        "columns": ["applicant_name", "stage", "score"],
        "rowActions": ["funnel.application.offer_assistance"],
        "bulkActions": ["funnel.application.offer_assistance"]
    });
    let view_id = "studio.view.funnel_application.needing_help";
    let (status, admitted) = cell.post(
        "/v1/definitions:admit",
        "admin",
        &json!({ "kind": "view", "id": view_id, "payload": view_payload }),
    );
    assert_eq!(status, 200, "{admitted}");
    assert_eq!(ptr(&admitted, "/admitted")?, &json!(true));
    assert!(ptr_str(&admitted, "/restatement")?.contains("Operations"));

    let (status, installed) = cell.post(
        "/v1/definitions:install",
        "admin",
        &json!({ "kind": "view", "id": view_id, "payload": view_payload }),
    );
    assert_eq!(status, 200, "{installed}");
    let v1 = ptr_u64(&installed, "/version")?;

    seed_funnel_applications(
        &mut cell,
        &[
            ("app-1", "Ada", 42),
            ("app-2", "Ben", 85),
            ("app-3", "Cid", 30),
        ],
        &["app-1", "app-2"],
    );

    let qs = form_urlencoded::Serializer::new(String::new())
        .append_pair(
            "filter",
            r#"{"all":[{"property":"stage","op":"eq","value":"stalled"},{"property":"score","op":"lt","value":"60"}]}"#,
        )
        .append_pair("sort", "score:asc")
        .finish();
    let (status, page) = cell.get("/v1/objects/funnel.Application", &qs, "admin");
    assert_eq!(status, 200, "{page}");
    assert_eq!(ptr_array(&page, "/records")?.len(), 1, "{page}");
    assert_eq!(ptr_str(&page, "/records/0/id")?, "app-1");
    assert!(
        ptr_array(&page, "/records/0/offers")?
            .iter()
            .any(|a| a == "funnel.application.offer_assistance"),
        "expected offer_assistance in per-row offers: {page}"
    );

    assert_viewer_rows_omit_offers(&mut cell)?;

    let (status, assisted) = cell.action(
        "funnel.application.offer_assistance",
        "admin",
        &json!({ "id": "app-1" }),
        "assist-app-1",
    );
    assert_eq!(status, 200, "{assisted}");
    assert_assisted_row_drops_offer(&mut cell, "app-1")?;
    assert_retire_and_undo(&mut cell, "view", view_id, &view_payload, v1)
}

#[test]
fn per_row_offers_enforce_pdp_role_and_resource_conditions() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut artifacts = funnel_artifacts();
    // Replace the policy so:
    // - `admin` may create/stall anything, and may `offer_assistance` ONLY on `app-1`
    // - `member` has Values read via DEMO_GRANTS, but NO action grant at all.
    artifacts.retain(|a| a.get("kind").and_then(Value::as_str) != Some("policy"));
    artifacts.push(json!({
        "artifactId": "funnel.application.access.policy",
        "dependencies": [], "irVersion": "0.1", "kind": "policy",
        "lifecycle": "active", "name": "funnel scoped access", "namespace": "funnel",
        "payload": {
            "accessVersion": "0.1", "policyId": "funnel.application.access",
            "rules": [
                {
                    "ruleId": "admin_manage",
                    "actions": ["funnel.application.create", "funnel.application.mark_stalled", "norn.read"],
                    "resourceType": "funnel.Application",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [{ "role": { "anyOf": ["admin"] } }]
                },
                {
                    "ruleId": "admin_assist_app1_only",
                    "actions": ["funnel.application.offer_assistance"],
                    "resourceType": "funnel.Application",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [
                        { "role": { "anyOf": ["admin"] } },
                        { "resource": { "name": "id", "op": "eq", "value": "app-1" } }
                    ]
                }
            ]
        },
        "provenance": provenance(), "scope": scope_json(), "version": "1.0.0"
    }));
    let mut cell = provisioned(&dir.path().join("pdp.sqlite"), &artifacts)?;
    seed_funnel_applications(
        &mut cell,
        &[("app-1", "Ada", 42), ("app-2", "Ben", 35)],
        &["app-1", "app-2"],
    );

    // Admin sees `offer_assistance` ONLY on `app-1`, NOT on `app-2` (denied by resource PDP rule).
    let (status, admin_page) = cell.get("/v1/objects/funnel.Application", "", "admin");
    assert_eq!(status, 200, "{admin_page}");
    let find_offers = |page: &Value, id: &str| -> TestResult<Vec<String>> {
        let row = ptr_array(page, "/records")?
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| format!("missing {id} in {page}"))?;
        Ok(ptr_array(row, "/offers")?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect())
    };
    let app1_admin = find_offers(&admin_page, "app-1")?;
    let app2_admin = find_offers(&admin_page, "app-2")?;
    assert!(
        app1_admin
            .iter()
            .any(|id| id == "funnel.application.offer_assistance"),
        "expected offer_assistance for admin on app-1: {app1_admin:?}"
    );
    assert!(
        !app2_admin
            .iter()
            .any(|id| id == "funnel.application.offer_assistance"),
        "PDP-denied row app-2 must not be offered offer_assistance: {app2_admin:?}"
    );

    // Member can read record values (non-withheld) but has no PDP action grant:
    // every row must report empty offers (never the full callable action list).
    let (status, member_page) = cell.get("/v1/objects/funnel.Application", "", "member");
    assert_eq!(status, 200, "{member_page}");
    assert_eq!(find_offers(&member_page, "app-1")?, Vec::<String>::new());
    assert_eq!(find_offers(&member_page, "app-2")?, Vec::<String>::new());
    Ok(())
}

#[test]
fn unrelated_fleet_domain_views_and_offers_work_identically() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut cell = provisioned(&dir.path().join("fleet.sqlite"), &fleet_artifacts())?;

    let view_payload = json!({
        "title": "Urgent fleet tickets",
        "space": "Fleet Ops",
        "objectType": "fleet.Ticket",
        "filter": { "all": [
            { "property": "status", "op": "eq", "value": "waiting_parts" },
            { "property": "downtime_hours", "op": "gt", "value": "24" }
        ] },
        "sort": "downtime_hours:desc",
        "columns": ["asset_code", "status", "downtime_hours"],
        "rowActions": ["fleet.ticket.escalate"],
        "bulkActions": ["fleet.ticket.escalate"]
    });
    let (st, inst) = cell.post(
        "/v1/definitions:install",
        "admin",
        &json!({ "kind": "view", "id": "studio.view.fleet_ticket.urgent", "payload": view_payload }),
    );
    assert_eq!(st, 200, "{inst}");

    for (id, asset, hours) in [("tkt-1", "CRANE-07", 36), ("tkt-2", "PUMP-12", 8)] {
        let (st, r) = cell.action(
            "fleet.ticket.create",
            "admin",
            &json!({ "id": id, "asset_code": asset, "downtime_hours": hours }),
            &format!("create-{id}"),
        );
        assert_eq!(st, 200, "{r}");
        let (st, r) = cell.action(
            "fleet.ticket.wait_parts",
            "admin",
            &json!({ "id": id }),
            &format!("wait-{id}"),
        );
        assert_eq!(st, 200, "{r}");
    }

    let (st, page) = cell.get(
        "/v1/objects/fleet.Ticket",
        "sort=downtime_hours:desc",
        "admin",
    );
    assert_eq!(st, 200, "{page}");
    assert!(
        ptr_array(&page, "/records/0/offers")?
            .iter()
            .any(|a| a == "fleet.ticket.escalate"),
        "{page}"
    );

    let (st, escalated) = cell.action(
        "fleet.ticket.escalate",
        "admin",
        &json!({ "id": "tkt-1" }),
        "escalate-tkt-1",
    );
    assert_eq!(st, 200, "{escalated}");

    let (st, obj) = cell.get("/v1/objects/fleet.Ticket/tkt-1", "", "admin");
    assert_eq!(st, 200, "{obj}");
    assert_eq!(ptr_str(&obj, "/values/status")?, "escalated");
    assert!(obj.get("offers").is_none(), "{obj}");
    Ok(())
}
