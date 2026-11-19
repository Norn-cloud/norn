//! Retained definitions from the project's sources (DEV-DEFINITIONS-1).
//!
//! An approval (`Norn.approval`, or a `{ kind, id, payload }` JSON document
//! in a source directory) is not an artifact of the definition set: the cell
//! retains it beside the set, and on a hosted cell it is installed through
//! `POST /v1/definitions:admit`, then `:install` with the signer's sign-off
//! of the server's restatement. The dev host installs each one from the
//! sources through exactly that path, in process, as the manifest's actor
//! (DEV-ONE-AUTHORITY-1: still no network route installs): the same
//! admission, the same restatement, the same sign-off check. The developer
//! signs by saving the source; the recorded sign-off names them.
//!
//! - A definition whose payload equals the installed one installs nothing.
//! - A changed one installs as the next version of its id.
//! - An approval the sources no longer declare is retired.
//! - A definition the cell refuses changes nothing; its findings are the
//!   compile's diagnostics, and every other definition still syncs.
//! - A declaration that is not `{ kind, id, payload }`, or an id declared
//!   twice, refuses the whole sync: nothing installs and nothing retires, so
//!   a malformed source never retires the approval it meant to declare.
use norn_api::definition_routes::{
    self, AdmitDefinitionRequest, DefinitionKinds, GetDefinitionQuery, InstallDefinitionRequest,
    ListDefinitionsQuery, RetireDefinitionRequest, SignoffRequest,
};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::SqlEngine;
use serde_json::value::RawValue;
use serde_json::{Value, json};

/// The retained kinds a project's sources may declare.
pub(crate) const SOURCE_KINDS: [&str; 1] = ["approval"];

/// What one sync did: one event per definition installed or retired, and one
/// diagnostic per definition refused.
#[derive(Debug, Default)]
pub(crate) struct Synced {
    pub(crate) changed: Vec<Value>,
    pub(crate) refused: Vec<Value>,
}

fn finding(id: &str, code: &str, message: impl Into<String>) -> Value {
    json!({ "code": code, "severity": "error", "message": message.into(), "definitionId": id })
}

fn tagged(id: &str, diagnostics: &[norn_api::refusal::RefusalDiagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| serde_json::to_value(diagnostic).ok())
        .map(|mut diagnostic| {
            if let Some(object) = diagnostic.as_object_mut() {
                object.insert("definitionId".to_owned(), json!(id));
            }
            diagnostic
        })
        .collect()
}

fn from_refusal(id: &str, refusal: &Refusal) -> Vec<Value> {
    if refusal.diagnostics.is_empty() {
        vec![finding(id, &refusal.code, &refusal.explanation)]
    } else {
        tagged(id, &refusal.diagnostics)
    }
}

/// One declared definition: `{ kind, id, payload }`.
struct Declared<'a> {
    kind: &'a str,
    id: &'a str,
    payload: &'a Value,
}

fn declared(value: &Value) -> Result<Declared<'_>, Value> {
    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    let kind = value.get("kind").and_then(Value::as_str);
    let payload = value.get("payload").filter(|payload| payload.is_object());
    match (kind, payload) {
        (Some(kind), Some(payload)) if SOURCE_KINDS.contains(&kind) && !id.is_empty() => {
            Ok(Declared { kind, id, payload })
        }
        (Some(kind), Some(_)) if !SOURCE_KINDS.contains(&kind) => Err(finding(
            id,
            "norn.dev.definition_kind",
            format!(
                "a source declares a {kind:?} definition; sources declare {}",
                SOURCE_KINDS.join(", ")
            ),
        )),
        _ => Err(finding(
            id,
            "norn.dev.definition_shape",
            "a source definition is { kind, id, payload }",
        )),
    }
}

/// Install, revise or retire the cell's source-declared definitions so they
/// equal `definitions`.
pub(crate) fn sync<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &CellScope,
    actor: &str,
    kinds: &DefinitionKinds,
    definitions: &[Value],
    now_ms: u64,
) -> Synced {
    let query = ScopeQuery {
        org_id: Some(scope.org_id.clone()),
        environment_id: Some(scope.environment_id.clone()),
    };
    let mut synced = Synced::default();
    let mut declarations = Vec::new();
    let mut declared_ids: Vec<String> = Vec::new();
    for value in definitions {
        match declared(value) {
            Ok(definition) if declared_ids.iter().any(|id| id == definition.id) => {
                synced.refused.push(finding(
                    definition.id,
                    "norn.dev.definition_duplicate",
                    format!("{} is declared more than once", definition.id),
                ));
            }
            Ok(definition) => {
                declared_ids.push(definition.id.to_owned());
                declarations.push(definition);
            }
            Err(refused) => synced.refused.push(refused),
        }
    }
    if !synced.refused.is_empty() {
        return synced;
    }
    for definition in declarations {
        match install(host, &query, actor, kinds, &definition, now_ms) {
            Ok(Some(event)) => synced.changed.push(event),
            Ok(None) => {}
            Err(refusal) => synced.refused.extend(from_refusal(definition.id, &refusal)),
        }
    }
    for kind in SOURCE_KINDS {
        match retire_undeclared(host, &query, actor, kind, &declared_ids, now_ms) {
            Ok(events) => synced.changed.extend(events),
            Err(refusal) => synced.refused.extend(from_refusal(kind, &refusal)),
        }
    }
    synced
}

/// The installed version and payload of `id`, if it is active.
fn installed<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ScopeQuery,
    actor: &str,
    kinds: &DefinitionKinds,
    id: &str,
) -> Result<Option<(u64, bool, Value)>, Refusal> {
    let get = GetDefinitionQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
        version: None,
    };
    match definition_routes::get_definition(host, &get, Some(actor), kinds, None, id) {
        Ok(document) => {
            let payload = serde_json::from_str(document.payload.get()).map_err(Refusal::host)?;
            Ok(Some((
                document.version,
                document.lifecycle == "active",
                payload,
            )))
        }
        Err(refusal) if refusal.code == "definition.not_found" => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

fn install<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ScopeQuery,
    actor: &str,
    kinds: &DefinitionKinds,
    definition: &Declared<'_>,
    now_ms: u64,
) -> Result<Option<Value>, Refusal> {
    let current = installed(host, query, actor, kinds, definition.id)?;
    if let Some((_, true, payload)) = &current
        && payload == definition.payload
    {
        return Ok(None);
    }
    let raw = || RawValue::from_string(definition.payload.to_string()).map_err(Refusal::host);
    let admitted = definition_routes::admit_definition(
        host,
        query,
        Some(actor),
        kinds,
        None,
        &AdmitDefinitionRequest {
            kind: definition.kind.to_owned(),
            id: definition.id.to_owned(),
            payload: raw()?,
        },
    )?;
    if !admitted.admitted {
        let mut refusal = Refusal::invalid("the definition is not admissible");
        "definition.refused".clone_into(&mut refusal.code);
        refusal.diagnostics = admitted.diagnostics;
        return Err(refusal);
    }
    let installed = definition_routes::install_definition(
        host,
        query,
        Some(actor),
        kinds,
        None,
        &InstallDefinitionRequest {
            kind: definition.kind.to_owned(),
            id: definition.id.to_owned(),
            payload: raw()?,
            expected_version: current.map(|(version, _, _)| version),
            signoff: admitted
                .restatement_sha256
                .map(|restatement_sha256| SignoffRequest { restatement_sha256 }),
        },
        now_ms,
    )?;
    Ok(Some(json!({
        "event": "definition", "kind": definition.kind, "id": installed.id,
        "version": installed.version, "lifecycle": "active",
        "restatement": admitted.restatement,
    })))
}

fn retire_undeclared<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ScopeQuery,
    actor: &str,
    kind: &str,
    declared: &[String],
    now_ms: u64,
) -> Result<Vec<Value>, Refusal> {
    let mut events = Vec::new();
    let mut cursor = None;
    loop {
        let page = definition_routes::list_definitions(
            host,
            &ListDefinitionsQuery {
                org_id: query.org_id.clone(),
                environment_id: query.environment_id.clone(),
                kind: Some(kind.to_owned()),
                include: None,
                limit: Some(100),
                cursor: cursor.take(),
                uses: None,
            },
            Some(actor),
        )?;
        for item in page.items {
            if declared.contains(&item.id) {
                continue;
            }
            let retired = definition_routes::retire_definition(
                host,
                query,
                Some(actor),
                None,
                &item.id,
                &RetireDefinitionRequest {
                    expected_version: item.version,
                },
                now_ms,
            )?;
            events.push(json!({
                "event": "definition", "kind": kind, "id": retired.id,
                "version": retired.version, "lifecycle": "retired",
            }));
        }
        match page.cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(events),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "sync fixtures"
)]
mod tests {
    //! DEV-DEFINITIONS-1: a malformed or duplicated declaration refuses the
    //! whole sync before the cell is asked anything, so nothing installs and
    //! no installed approval retires.
    use super::*;
    use crate::dev::policy::tests::{host, scope};

    fn approval(id: &str) -> Value {
        json!({ "kind": "approval", "id": id, "payload": {
            "action": "shop.sale.record", "decider": { "roles": ["admin"] },
            "question": "Record this sale?", "risk": "low" } })
    }

    fn sync_of(definitions: &[Value]) -> Synced {
        let dir = tempfile::tempdir().expect("tempdir");
        let host = host(dir.path());
        let kinds = crate::dev::policy::dev_kinds();
        sync(&host, &scope(), "dev@example", &kinds, definitions, 1)
    }

    fn codes(synced: &Synced) -> Vec<&str> {
        synced
            .refused
            .iter()
            .filter_map(|finding| finding.get("code").and_then(Value::as_str))
            .collect()
    }

    #[test]
    fn a_malformed_declaration_refuses_the_sync() {
        let mut malformed = approval("shop.b");
        malformed["payload"] = json!([]);
        let synced = sync_of(&[approval("shop.a"), malformed]);
        assert!(synced.changed.is_empty());
        assert_eq!(codes(&synced), ["norn.dev.definition_shape"]);
    }

    #[test]
    fn a_duplicated_id_refuses_the_sync() {
        let synced = sync_of(&[approval("shop.a"), approval("shop.a")]);
        assert!(synced.changed.is_empty());
        assert_eq!(codes(&synced), ["norn.dev.definition_duplicate"]);
        assert_eq!(synced.refused[0]["definitionId"], json!("shop.a"));
    }
}
