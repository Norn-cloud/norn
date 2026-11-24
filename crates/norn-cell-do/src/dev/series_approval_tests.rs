//! AUTH-INDEPENDENT-PERSON (#618) for an approval-gated create whose key a
//! naming series generates, through the dev host's own dispatch: the call
//! parks, the reviewer approves it, and the requester's retry commits only
//! the number the approval was granted for (APPROVAL-INTENT-1).

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
            "sourcePath": "crates/norn-cell-do/src/dev/series_approval_tests.rs" },
        "payload": payload
    })
}

/// `test.Note`, numbered `NO-00001`, … by its naming series, and the action
/// that creates one from a title alone.
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
            "identity": [{ "name": "primary", "properties": ["no"], "namingSeries": {
                "pattern": "NO-{#####}", "timezone": "Etc/UTC",
                "resetPolicy": "never", "gapPolicy": "gapless_within_commit" } }]
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
            "inputSchema": { "type": "object", "properties": { "title": { "type": "string" } },
                "required": ["title"], "additionalProperties": false },
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

fn call(dev: &Dev, token: &str, (method, path): (&str, &str), body: &str) -> Checked<(u16, Value)> {
    let authorization = format!("Bearer {token}");
    let response = answer(
        &dev.host,
        &dev.principals,
        &dev.metrics,
        None,
        &Request {
            method,
            path,
            query: SCOPE,
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
        ("POST", "/v1/definitions:admit"),
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

fn create(dev: &Dev, key: &str) -> Checked<(u16, Value)> {
    let body = json!({ "input": { "title": key }, "idempotencyKey": key }).to_string();
    call(
        dev,
        DEVELOPER_TOKEN,
        ("POST", &format!("/v1/actions/{CREATE}")),
        &body,
    )
}

fn approve(dev: &Dev, parked: &Value) -> Checked {
    let id = parked
        .pointer("/decision/id")
        .and_then(Value::as_str)
        .ok_or("no pending decision")?;
    let choice = json!({ "choice": "approve", "idempotencyKey": "approve-1" }).to_string();
    let path = format!("/v1/decisions/{id}:decide");
    let (status, decided) = call(dev, REVIEWER_TOKEN, ("POST", &path), &choice)?;
    assert_eq!(status, 200, "{decided}");
    Ok(())
}

/// Two creates park while the counter stands at one, so each approval names
/// `NO-00001`. The second's retry commits it; the first's retry would now
/// number its note `NO-00002`, which no one approved: it is refused, finally.
#[test]
fn a_generated_key_create_commits_only_the_number_it_was_approved_for() -> Checked {
    let dev = dev()?;
    gate(&dev)?;
    let (status, first) = create(&dev, "create-1")?;
    assert_eq!(status, 202, "{first}");
    let (status, second) = create(&dev, "create-2")?;
    assert_eq!(status, 202, "{second}");
    approve(&dev, &first)?;
    approve(&dev, &second)?;

    let (status, committed) = create(&dev, "create-2")?;
    assert_eq!(status, 200, "{committed}");
    assert_eq!(committed.get("objectId"), Some(&json!("NO-00001")));
    for _ in 0..2 {
        let (status, refused) = create(&dev, "create-1")?;
        assert_eq!(
            (status, refused.get("code")),
            (409, Some(&json!("action.approved_intent_changed"))),
            "{refused}"
        );
    }
    Ok(())
}
