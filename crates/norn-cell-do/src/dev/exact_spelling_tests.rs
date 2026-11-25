//! AUTH-INDEPENDENT-PERSON (#618) through the dev host's own dispatch: an
//! approval-gated create parks, the reviewer approves it, and the call its
//! requester is served to retry is the exact text the approval was granted
//! for (APPROVAL-INTENT-1).

use std::error::Error;

use super::action_cell::admitted_store;
use super::*;
use crate::dev::policy::{LocalDeveloper, dev_actions, refresh_actions, reviewer};
use norn_api::scope::{CellScope, ScopeQuery};
use serde_json::{Value, json};

const DEVELOPER: &str = "local:developer";
const DEVELOPER_TOKEN: &str = "developer-token-0123456789";
const REVIEWER_TOKEN: &str = "reviewer-token-01234567890";
const SCOPE: &str = "orgId=org_demo&environmentId=dev";
const CREATE: &str = "test.note.create";
const NOW_S: u64 = 1_800_000_000;
const NOW_MS: u64 = 1_800_000_000_000;

type Checked<T = ()> = Result<T, Box<dyn Error>>;

fn artifact(artifact_id: &str, kind: &str, payload: &Value, dependencies: &[&str]) -> Value {
    let dependencies: Vec<Value> = dependencies
        .iter()
        .map(|id| json!({ "artifactId": id, "kind": "entity", "version": "1.0.0", "required": true }))
        .collect();
    json!({
        "irVersion": "0.1", "artifactId": artifact_id, "kind": kind, "namespace": "test",
        "name": artifact_id.split('.').nth(1), "version": "1.0.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": dependencies,
        "provenance": { "source": "authoring_sdk",
            "sourcePath": "crates/norn-cell-do/src/dev/exact_spelling_tests.rs" },
        "payload": payload
    })
}

/// `test.Note`, keyed by `no`, and the action that creates one.
fn documents() -> Vec<Value> {
    let note = artifact(
        "test.Note.entity",
        "entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "test.Note",
            "properties": [
                { "id": "no", "type": { "kind": "text" } },
                { "id": "title", "type": { "kind": "text" } }
            ],
            "identity": [{ "name": "primary", "properties": ["no"] }]
        } }),
        &[],
    );
    let create = artifact(
        "test.Note.create.action",
        "action",
        &json!({
            "actionId": CREATE,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": { "type": "object", "additionalProperties": false,
                "properties": { "no": { "type": "string" }, "title": { "type": "string" } },
                "required": ["no", "title"] },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": [{ "entityKind": "test.Note", "change": "created" }],
            "effectProgram": { "kind": "upsert_input_object", "objectIdField": "no",
                "objectKind": "test.Note", "entityArtifactId": "test.Note.entity",
                "entityArtifactVersion": "1.0.0", "writeMode": { "kind": "create_only" } }
        }),
        &["test.Note.entity"],
    );
    vec![note, create]
}

struct Dev {
    host: DevHost<NativeEngine>,
    principals: DevPrincipals,
    metrics: DevMetrics,
}

fn dev() -> Checked<Dev> {
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let store = admitted_store(
        &documents(),
        norn_api::action_admission::prepare_action_model,
    );
    let actions = dev_actions();
    let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
        .map_err(|refusal| refusal.code)?
        .with_authorizer(LocalDeveloper::for_actor(
            DEVELOPER,
            std::sync::Arc::clone(&actions),
        ));
    refresh_actions(&actions, &host)?;
    Ok(Dev {
        host,
        principals: DevPrincipals::new(vec![
            (DEVELOPER_TOKEN.into(), DEVELOPER.into()),
            (REVIEWER_TOKEN.into(), reviewer(DEVELOPER)),
        ]),
        metrics: DevMetrics::new([9; 32], scope),
    })
}

fn call(
    dev: &Dev,
    token: &str,
    (method, path, query): (&str, &str, &str),
    body: &str,
) -> Checked<(u16, Value)> {
    let authorization = format!("Bearer {token}");
    let response = answer(
        &dev.host,
        &dev.principals,
        &dev.metrics,
        None,
        &Request {
            method,
            path,
            query,
            authorization: Some(&authorization),
            now: NOW_S,
            body,
        },
    );
    Ok((response.status, serde_json::from_str(&response.body)?))
}

/// Install the approval gating the create, deciders `admin`, in process, as
/// norn dev installs from the project's sources.
fn gate(dev: &Dev) -> Checked {
    let approval = json!({ "kind": "approval", "id": "test.note_approval", "payload": {
        "action": CREATE, "decider": { "roles": ["admin"] },
        "question": "Create this note?", "risk": "high" } });
    let (status, admitted) = call(
        dev,
        DEVELOPER_TOKEN,
        ("POST", "/v1/definitions:admit", SCOPE),
        &approval.to_string(),
    )?;
    assert_eq!(status, 200, "{admitted}");
    let restatement = admitted.get("restatementSha256").cloned();
    let signed = json!({ "kind": "approval", "id": "test.note_approval",
        "payload": approval.get("payload"), "signoff": { "restatementSha256": restatement } });
    norn_api::definition_routes::install_definition(
        &dev.host,
        &ScopeQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
        },
        Some(DEVELOPER),
        &crate::dev::policy::dev_kinds(),
        None,
        &serde_json::from_value(signed)?,
        NOW_MS,
    )
    .map_err(|refusal| refusal.code)?;
    Ok(())
}

/// The developer's create under `key`, with `input` as the JSON text it sends.
fn create_with(dev: &Dev, key: &str, input: &str) -> Checked<(u16, Value)> {
    let body = format!(r#"{{"input":{input},"idempotencyKey":{}}}"#, json!(key));
    let path = format!("/v1/actions/{CREATE}");
    call(dev, DEVELOPER_TOKEN, ("POST", &path, SCOPE), &body)
}

fn approve(dev: &Dev, parked: &Value) -> Checked {
    let id = parked
        .pointer("/decision/id")
        .and_then(Value::as_str)
        .ok_or("no pending decision")?;
    let choice = json!({ "choice": "approve", "idempotencyKey": "approve-1" }).to_string();
    let path = format!("/v1/decisions/{id}:decide");
    let (status, decided) = call(dev, REVIEWER_TOKEN, ("POST", &path, SCOPE), &choice)?;
    assert_eq!(status, 200, "{decided}");
    Ok(())
}

/// A title spelled decomposed (NFD) parks and is approved, and the call its
/// requested decision serves for the retry (`encodedInput`) is that spelling,
/// byte for byte: the parked command's Norn1 form never normalizes it into
/// another intent.
#[test]
fn a_decomposed_spelling_is_parked_and_served_as_written() -> Checked {
    let dev = dev()?;
    gate(&dev)?;
    let input = json!({ "no": "N-1", "title": "cafe\u{301}" }).to_string();
    let (status, parked) = create_with(&dev, "spelled-1", &input)?;
    assert_eq!(status, 202, "{parked}");
    approve(&dev, &parked)?;
    let id = parked
        .pointer("/decision/id")
        .and_then(Value::as_str)
        .ok_or("no pending decision")?;
    let query = format!("{SCOPE}&id={id}");
    let (status, requested) = call(
        &dev,
        DEVELOPER_TOKEN,
        ("GET", "/v1/decisions:requested", &query),
        "",
    )?;
    assert_eq!(status, 200, "{requested}");
    let served = requested.pointer("/items/0/call/encodedInput");
    assert_eq!(served, Some(&json!(input)), "{requested}");
    Ok(())
}
