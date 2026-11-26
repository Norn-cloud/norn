//! Retained definitions from the project's sources (DEV-DEFINITIONS-1).
//!
//! An approval (`Norn.approval`, or a `{ kind, id, payload }` JSON document
//! in a source directory) is not an artifact of the definition set: the cell
//! retains it beside the set. On a hosted cell a pack's approvals ride its
//! `:apply`: admitted against the candidate set's actions, restated, signed
//! and installed with the set in one transaction. The dev host commits the
//! sources' approvals exactly that way, in process, as the manifest's actor
//! (DEV-ONE-AUTHORITY-1: still no network route installs): the same
//! admission, the same restatement, the same sign-off check. The developer
//! signs by saving the source; the recorded sign-off names them.
//!
//! - DEV-DEFINITIONS-ATOMIC-1 (#1469 S2, S1 of #1477): the set, every changed
//!   approval, every retirement and the action policy are committed in one
//!   step under the serving write lock: one store transaction, one
//!   definitions version, and the policy recompiled before any call is
//!   served again. A refused approval publishes nothing: the set stays, the
//!   active gates stay, and the new action is not installed.
//! - A definition whose payload equals the installed one appends nothing.
//! - A changed one appends the next revision of its id.
//! - An approval the sources no longer declare retires in the same commit,
//!   so a renamed approval takes over its action with no ungated window.
//! - A declaration that is not `{ kind, id, payload }`, or an id declared
//!   twice, refuses before the cell is asked anything.
//!
//! DEV-READ-DEFINITIONS-1: a question or dashboard (`{ kind, id, payload }`
//! JSON in a source directory) changes what the cell answers, never what it
//! does, so it does not ride the set: once a revision is serving, the dev host
//! installs each changed one in process, as the manifest's actor, through the
//! same admission, restatement and sign-off as `POST /v1/definitions:install`
//! (still no network route installs), and retires each one the sources no
//! longer declare. One the cell refuses is reported and left as it was.
use norn_api::cell_definitions::source_approvals;
use norn_api::definition_routes::{
    AdmitDefinitionRequest, DefinitionKinds, GetDefinitionQuery, InstallDefinitionRequest,
    ListDefinitionsQuery, MAX_LIST_LIMIT, RetireDefinitionRequest, SignoffRequest,
    admit_definition, get_definition, install_definition, list_definitions, retire_definition,
};
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_api::refusal::Refusal;
use norn_api::scope::ScopeQuery;
use norn_cell::InstalledDefinitionSet;
use norn_cell::engine::SqlEngine;
use norn_cell::retained::{RetainedDefinitionChange, RetainedLifecycle};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::admit::Admitted;
use super::policy::{self, DevActions};

/// The retained kinds a project's sources may declare.
pub(crate) const SOURCE_KINDS: [&str; 3] = ["approval", "question", "dashboard"];

/// The source kinds installed after the set, not with it
/// (DEV-READ-DEFINITIONS-1), in install order.
pub(crate) const READ_KINDS: [&str; 2] = ["question", "dashboard"];

/// The approval revisions to commit with a set, and the events that report
/// them once committed.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    pub(crate) riding: Vec<RetainedDefinitionChange>,
    pub(crate) events: Vec<Value>,
}

/// Why nothing was published: the phase and its diagnostics.
#[derive(Debug)]
pub(crate) struct Rejected {
    pub(crate) phase: &'static str,
    pub(crate) diagnostics: Vec<Value>,
}

/// What one commit did.
#[derive(Debug)]
pub(crate) enum Committed {
    /// The set and every approval were already active: nothing was written.
    Unchanged,
    /// One new revision: the installed set and the approval events.
    Installed(Box<InstalledDefinitionSet>, Vec<Value>),
}

fn finding(id: &str, code: &str, message: impl Into<String>) -> Value {
    json!({ "code": code, "severity": "error", "message": message.into(), "definitionId": id })
}

fn from_refusal(id: &str, refusal: &Refusal) -> Vec<Value> {
    if refusal.diagnostics.is_empty() {
        return vec![finding(id, &refusal.code, &refusal.explanation)];
    }
    refusal
        .diagnostics
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

/// One declared definition `{ kind, id, payload }`, as `(kind, id, payload)`.
fn declared(value: &Value) -> Result<(String, String, Box<RawValue>), Value> {
    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    let kind = value.get("kind").and_then(Value::as_str);
    let payload = value.get("payload").filter(|payload| payload.is_object());
    match (kind, payload) {
        (Some(kind), Some(payload)) if SOURCE_KINDS.contains(&kind) && !id.is_empty() => {
            let raw = RawValue::from_string(payload.to_string())
                .map_err(|error| finding(id, "norn.dev.definition_shape", error.to_string()))?;
            Ok((kind.to_owned(), id.to_owned(), raw))
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

/// One declared definition: its kind, id and payload.
type Declared = (String, String, Box<RawValue>);

/// Every declared definition, or the findings of the malformed and the
/// duplicated ones.
fn declarations(definitions: &[Value]) -> Result<Vec<Declared>, Vec<Value>> {
    let mut refused = Vec::new();
    let mut declarations: Vec<Declared> = Vec::new();
    for value in definitions {
        match declared(value) {
            Ok((_, id, _)) if declarations.iter().any(|(_, other, _)| *other == id) => {
                refused.push(finding(
                    &id,
                    "norn.dev.definition_duplicate",
                    format!("{id} is declared more than once"),
                ));
            }
            Ok(declaration) => declarations.push(declaration),
            Err(finding) => refused.push(finding),
        }
    }
    if refused.is_empty() {
        Ok(declarations)
    } else {
        Err(refused)
    }
}

/// What a read-definition sync did: the revisions it appended (as
/// `definition` events) and the findings of the definitions the cell refused.
#[derive(Debug, Default)]
pub(crate) struct Synced {
    pub(crate) events: Vec<Value>,
    pub(crate) refused: Vec<Value>,
}

/// DEV-READ-DEFINITIONS-1: install each declared question and dashboard that
/// differs from its active revision, then retire each active one of those
/// kinds the sources no longer declare, as `actor`, over the serving `host`.
pub(crate) fn sync_reads<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    kinds: &DefinitionKinds,
    questions: Option<&dyn AdHocQuestions<E>>,
    definitions: &[Value],
    now_ms: u64,
) -> Synced {
    let mut synced = Synced::default();
    let declared = match declarations(definitions) {
        Ok(declared) => declared,
        Err(refused) => {
            synced.refused = refused;
            return synced;
        }
    };
    let reads = Reads {
        host,
        actor,
        kinds,
        questions,
        now_ms,
        scope: ScopeQuery {
            org_id: Some(host.scope().org_id.clone()),
            environment_id: Some(host.scope().environment_id.clone()),
        },
    };
    for kind in READ_KINDS {
        for (_, id, payload) in declared.iter().filter(|(declared, _, _)| declared == kind) {
            reads.install(&mut synced, kind, id, payload);
        }
    }
    for kind in READ_KINDS.iter().rev() {
        reads.retire_undeclared(&mut synced, kind, &declared);
    }
    synced
}

/// The serving host and the actor a read-definition sync installs as.
struct Reads<'a, E: SqlEngine> {
    host: &'a DevHost<E>,
    actor: &'a str,
    kinds: &'a DefinitionKinds,
    questions: Option<&'a dyn AdHocQuestions<E>>,
    now_ms: u64,
    scope: ScopeQuery,
}

fn definition_event(kind: &str, id: &str, version: u64, lifecycle: &str) -> Value {
    json!({ "event": "definition", "kind": kind, "id": id, "version": version, "lifecycle": lifecycle })
}

impl<E: SqlEngine> Reads<'_, E> {
    /// Install `payload` as `id` unless its active revision already is it.
    fn install(&self, synced: &mut Synced, kind: &str, id: &str, payload: &RawValue) {
        let read = GetDefinitionQuery {
            org_id: self.scope.org_id.clone(),
            environment_id: self.scope.environment_id.clone(),
            version: None,
        };
        // Only "not installed" means there is no current revision; any other
        // read failure is reported, never taken for a first install.
        let current = match get_definition(
            self.host,
            &read,
            Some(self.actor),
            self.kinds,
            self.questions,
            id,
        ) {
            Ok(current) => Some(current),
            Err(refusal) if refusal.code == norn_api::definition_routes::codes::NOT_FOUND => None,
            Err(refusal) => {
                synced.refused.extend(from_refusal(id, &refusal));
                return;
            }
        };
        if let Some(current) = &current {
            if current.kind != kind {
                synced.refused.push(finding(
                    id,
                    "norn.dev.definition_kind",
                    format!("{id} is already a {} definition", current.kind),
                ));
                return;
            }
            let same = serde_json::from_str::<Value>(current.payload.get()).ok()
                == serde_json::from_str::<Value>(payload.get()).ok();
            if current.lifecycle == "active" && same {
                return;
            }
        }
        let request = AdmitDefinitionRequest {
            kind: kind.to_owned(),
            id: id.to_owned(),
            payload: payload.to_owned(),
        };
        let admitted = match admit_definition(
            self.host,
            &self.scope,
            Some(self.actor),
            self.kinds,
            self.questions,
            &request,
        ) {
            Ok(admitted) if admitted.admitted => admitted,
            Ok(admitted) => {
                synced
                    .refused
                    .extend(admitted.diagnostics.iter().filter_map(|diagnostic| {
                        let mut value = serde_json::to_value(diagnostic).ok()?;
                        value
                            .as_object_mut()?
                            .insert("definitionId".to_owned(), json!(id));
                        Some(value)
                    }));
                return;
            }
            Err(refusal) => {
                synced.refused.extend(from_refusal(id, &refusal));
                return;
            }
        };
        let install = InstallDefinitionRequest {
            kind: kind.to_owned(),
            id: id.to_owned(),
            payload: payload.to_owned(),
            expected_version: current.as_ref().map(|current| current.version),
            signoff: admitted
                .restatement_sha256
                .map(|restatement_sha256| SignoffRequest { restatement_sha256 }),
        };
        match install_definition(
            self.host,
            &self.scope,
            Some(self.actor),
            self.kinds,
            self.questions,
            &install,
            self.now_ms,
        ) {
            Ok(installed) => {
                synced
                    .events
                    .push(definition_event(kind, id, installed.version, "active"));
            }
            Err(refusal) => synced.refused.extend(from_refusal(id, &refusal)),
        }
    }

    /// Retire every active `kind` definition `declared` does not name.
    fn retire_undeclared(&self, synced: &mut Synced, kind: &str, declared: &[Declared]) {
        let mut cursor = None;
        loop {
            let query = ListDefinitionsQuery {
                org_id: self.scope.org_id.clone(),
                environment_id: self.scope.environment_id.clone(),
                kind: Some(kind.to_owned()),
                include: None,
                limit: Some(MAX_LIST_LIMIT),
                cursor: cursor.take(),
                uses: None,
            };
            let page = match list_definitions(self.host, &query, Some(self.actor)) {
                Ok(page) => page,
                Err(refusal) => {
                    synced.refused.extend(from_refusal(kind, &refusal));
                    return;
                }
            };
            for summary in page.items.iter().filter(|summary| {
                summary.lifecycle == "active"
                    && !declared.iter().any(|(_, id, _)| *id == summary.id)
            }) {
                let request = RetireDefinitionRequest {
                    expected_version: summary.version,
                };
                match retire_definition(
                    self.host,
                    &self.scope,
                    Some(self.actor),
                    self.questions,
                    &summary.id,
                    &request,
                    self.now_ms,
                ) {
                    Ok(retired) => {
                        synced.events.push(definition_event(
                            kind,
                            &summary.id,
                            retired.version,
                            "retired",
                        ));
                    }
                    Err(refusal) => synced.refused.extend(from_refusal(&summary.id, &refusal)),
                }
            }
            match page.cursor {
                Some(next) => cursor = Some(next),
                None => return,
            }
        }
    }
}

/// Parse `definitions` and admit, restate and sign each approval against
/// `canonical_set` (the candidate set), with the retirement of every active
/// approval the sources no longer declare. Writes nothing.
pub(crate) fn prepare<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    kinds: &DefinitionKinds,
    definitions: &[Value],
    canonical_set: &str,
    now_ms: u64,
) -> Result<Prepared, Vec<Value>> {
    let declarations: Vec<(String, Box<RawValue>)> = declarations(definitions)?
        .into_iter()
        .filter(|(kind, _, _)| kind == "approval")
        .map(|(_, id, payload)| (id, payload))
        .collect();
    let approvals = source_approvals(host, actor, kinds, canonical_set, &declarations, now_ms)
        .map_err(|refused| from_refusal(&refused.0, &refused.1))?;
    let mut prepared = Prepared::default();
    for approval in approvals {
        let definition = &approval.change.definition;
        let lifecycle = match definition.lifecycle {
            RetainedLifecycle::Active => "active",
            RetainedLifecycle::Retired => "retired",
        };
        let mut event = json!({
            "event": "definition", "kind": definition.kind, "id": definition.id,
            "version": definition.revision, "lifecycle": lifecycle,
        });
        if let (Some(restatement), Some(object)) = (approval.restatement, event.as_object_mut()) {
            object.insert("restatement".to_owned(), json!(restatement));
        }
        prepared.events.push(event);
        prepared.riding.push(approval.change);
    }
    Ok(prepared)
}

/// DEV-DEFINITIONS-ATOMIC-1: commit `admitted` over the active revision
/// `expected` with the sources' approvals, in one install, then recompile
/// `actions` from the new revision. The caller holds the serving write lock
/// across this call, so no call is served between the install and the
/// policy. Any refusal writes nothing and leaves the policy as it was.
#[allow(
    clippy::too_many_arguments,
    reason = "the one commit takes the host, the actor's kinds, the set and the policy slot"
)]
pub(crate) fn commit<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    kinds: &DefinitionKinds,
    definitions: &[Value],
    admitted: Admitted,
    active: &InstalledDefinitionSet,
    actions: &DevActions,
    now_ms: u64,
) -> Result<Committed, Rejected> {
    let prepared = prepare(
        host,
        actor,
        kinds,
        definitions,
        &admitted.input.canonical_set,
        now_ms,
    )
    .map_err(|diagnostics| Rejected {
        phase: "definitions",
        diagnostics,
    })?;
    if active.canonical_set == admitted.input.canonical_set && prepared.riding.is_empty() {
        return Ok(Committed::Unchanged);
    }
    let installed = host
        .install_definitions_riding(
            admitted.model,
            admitted.input,
            Some(active.version),
            &prepared.riding,
        )
        .map_err(|refusal| Rejected {
            phase: "install",
            diagnostics: vec![serde_json::to_value(&refusal).unwrap_or(Value::Null)],
        })?;
    // The report is read later; the policy of the committed revision (as
    // its transaction wrote it, no readback) is in the slot before the lock
    // is released (#1469 S1).
    let _ungoverned = policy::install_actions(actions, &installed.canonical_set, host.scope());
    Ok(Committed::Installed(Box::new(installed), prepared.events))
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::arithmetic_side_effects,
    reason = "commit fixtures"
)]
mod tests {
    //! DEV-DEFINITIONS-1 and DEV-DEFINITIONS-ATOMIC-1: a malformed or
    //! duplicated declaration refuses before the cell is asked anything; a
    //! refused approval publishes nothing (no new action, the old gate
    //! stays); a renamed approval retires and replaces in the set's one
    //! commit, so no call can see the action ungated in between.
    use super::*;
    use crate::dev::http::action_cell::{
        DERIVES_LOSS, MELT_ACTION, melt_install_edited, melt_store,
    };
    use crate::dev::policy::{dev_actions, dev_kinds};
    use norn_api::scope::CellScope;
    use norn_cell::engine::NativeEngine;

    const ACTOR: &str = "local:developer";
    const RECONVERT: &str = "constitution.melt.reconvert";

    fn cell() -> DevHost<NativeEngine> {
        let store = melt_store(
            DERIVES_LOSS,
            norn_api::action_admission::prepare_action_model,
        );
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        DevHost::from_store(store, scope, |command| command.payload.clone()).unwrap()
    }

    /// The melt set, with a second action `RECONVERT` when `extra`.
    fn candidate(extra: bool, installed_at: u64) -> Admitted {
        let (model, input) = melt_install_edited(
            DERIVES_LOSS,
            |documents| {
                if extra {
                    let mut action = documents
                        .iter()
                        .find(|document| document["kind"] == "action")
                        .cloned()
                        .unwrap();
                    action["artifactId"] = json!(format!("{RECONVERT}.action"));
                    action["payload"]["actionId"] = json!(RECONVERT);
                    documents.push(action);
                }
            },
            norn_api::action_admission::prepare_action_model,
            installed_at,
        );
        Admitted { model, input }
    }

    fn approval(id: &str, action: &str, role: &str) -> Value {
        json!({ "kind": "approval", "id": id, "payload": {
            "action": action, "decider": { "roles": [role] },
            "question": "Convert this melt?", "risk": "high" } })
    }

    fn commit_of(
        host: &DevHost<NativeEngine>,
        extra: bool,
        definitions: &[Value],
        at: u64,
    ) -> Result<Committed, Rejected> {
        let active = host.active_definitions().unwrap().unwrap();
        commit(
            host,
            ACTOR,
            &dev_kinds(),
            definitions,
            candidate(extra, at),
            &active,
            &dev_actions(),
            at,
        )
    }

    fn record(
        host: &DevHost<NativeEngine>,
        id: &str,
    ) -> Option<norn_cell::retained::RetainedDefinitionRecord> {
        host.retained_definition(id).unwrap()
    }

    fn codes(refused: &[Value]) -> Vec<&str> {
        refused
            .iter()
            .filter_map(|finding| finding.get("code").and_then(Value::as_str))
            .collect()
    }

    #[test]
    fn a_malformed_declaration_refuses_before_the_cell() {
        let host = cell();
        let mut malformed = approval("ops.b", MELT_ACTION, "admin");
        malformed["payload"] = json!([]);
        let refused = prepare(
            &host,
            ACTOR,
            &dev_kinds(),
            &[approval("ops.a", MELT_ACTION, "admin"), malformed],
            "[]",
            1,
        )
        .expect_err("refused");
        assert_eq!(codes(&refused), ["norn.dev.definition_shape"]);
    }

    #[test]
    fn a_duplicated_id_refuses_before_the_cell() {
        let host = cell();
        let gate = approval("ops.a", MELT_ACTION, "admin");
        let refused = prepare(&host, ACTOR, &dev_kinds(), &[gate.clone(), gate], "[]", 1)
            .expect_err("refused");
        assert_eq!(codes(&refused), ["norn.dev.definition_duplicate"]);
        assert_eq!(refused[0]["definitionId"], json!("ops.a"));
    }

    /// #1469 S2: the set adds an action whose approval is refused: nothing
    /// is published, so the action is never live ungated.
    #[test]
    fn a_refused_approval_installs_no_new_action() {
        let host = cell();
        let before = host.active_definitions().unwrap().unwrap();
        let rejected = commit_of(
            &host,
            true,
            &[approval("ops.reconvert", RECONVERT, "no-such-role")],
            5_000,
        )
        .expect_err("the approval is refused");
        assert_eq!(rejected.phase, "definitions");
        let after = host.active_definitions().unwrap().unwrap();
        assert_eq!(after.version, before.version);
        assert!(!after.canonical_set.contains(RECONVERT));
        assert!(record(&host, "ops.reconvert").is_none());
    }

    /// #1469 S2 + S1 of #1477: a rename retires the old approval and
    /// installs the new one in the set's one commit: both revisions name the
    /// same definitions version and commit, so no call is served between.
    #[test]
    fn a_rename_retires_and_replaces_in_one_commit() {
        let host = cell();
        let first = commit_of(
            &host,
            true,
            &[approval("ops.old", RECONVERT, "admin")],
            5_000,
        )
        .expect("gated");
        let Committed::Installed(first, _) = first else {
            panic!("the gate and the new action install");
        };
        assert!(first.canonical_set.contains(RECONVERT));
        let old = record(&host, "ops.old").unwrap();
        assert_eq!(
            old.definitions_version, first.version,
            "set and gate together"
        );

        let renamed = commit_of(
            &host,
            true,
            &[approval("ops.new", RECONVERT, "admin")],
            6_000,
        )
        .expect("renamed");
        let Committed::Installed(renamed, events) = renamed else {
            panic!("the rename installs");
        };
        assert_eq!(renamed.version, first.version + 1, "one commit");
        let old = record(&host, "ops.old").unwrap();
        let new = record(&host, "ops.new").unwrap();
        assert_eq!(old.definition.lifecycle, RetainedLifecycle::Retired);
        assert_eq!(new.definition.lifecycle, RetainedLifecycle::Active);
        assert_eq!(old.commit_seq, new.commit_seq);
        assert_eq!(old.definitions_version, renamed.version);
        assert_eq!(new.definitions_version, renamed.version);
        let lifecycles: Vec<_> = events
            .iter()
            .map(|event| event["lifecycle"].clone())
            .collect();
        assert_eq!(lifecycles, [json!("active"), json!("retired")]);
    }

    /// A refused replacement keeps the old gate active and the set as it was.
    #[test]
    fn a_refused_replacement_keeps_the_old_gate() {
        let host = cell();
        commit_of(
            &host,
            false,
            &[approval("ops.old", MELT_ACTION, "admin")],
            5_000,
        )
        .expect("gated");
        let before = host.active_definitions().unwrap().unwrap();
        let rejected = commit_of(
            &host,
            false,
            &[approval("ops.new", MELT_ACTION, "no-such-role")],
            6_000,
        )
        .expect_err("refused");
        assert_eq!(rejected.phase, "definitions");
        assert_eq!(
            host.active_definitions().unwrap().unwrap().version,
            before.version
        );
        let old = record(&host, "ops.old").unwrap();
        assert_eq!(old.definition.lifecycle, RetainedLifecycle::Active);
        assert!(record(&host, "ops.new").is_none());
    }

    /// #1469 S2: uniqueness is of the final declared set. Moving gate G
    /// from action A to B while a new G' gates A admits, and both revisions
    /// commit with the set; two declared gates on one action still refuse.
    #[test]
    fn a_gate_moves_while_its_old_action_gets_a_replacement() {
        let host = cell();
        let Committed::Installed(first, _) = commit_of(
            &host,
            true,
            &[approval("ops.g", MELT_ACTION, "admin")],
            5_000,
        )
        .expect("gated") else {
            panic!("the gate installs");
        };
        let moved = commit_of(
            &host,
            true,
            &[
                approval("ops.g", RECONVERT, "admin"),
                approval("ops.g2", MELT_ACTION, "admin"),
            ],
            6_000,
        )
        .expect("a move and a replacement admit together");
        let Committed::Installed(moved, _) = moved else {
            panic!("the move installs");
        };
        assert_eq!(moved.version, first.version + 1, "one commit");
        let g = record(&host, "ops.g").unwrap();
        let g2 = record(&host, "ops.g2").unwrap();
        assert!(
            g.definition.payload.contains(RECONVERT),
            "{}",
            g.definition.payload
        );
        assert!(
            g2.definition.payload.contains(MELT_ACTION),
            "{}",
            g2.definition.payload
        );
        assert_eq!(g.definition.revision, 2);
        assert_eq!(g.commit_seq, g2.commit_seq);

        let both = commit_of(
            &host,
            true,
            &[
                approval("ops.g", MELT_ACTION, "admin"),
                approval("ops.g2", MELT_ACTION, "admin"),
            ],
            7_000,
        )
        .expect_err("two gates on one action");
        assert_eq!(both.phase, "definitions");
    }

    /// The same set with the same approvals writes nothing.
    #[test]
    fn an_unchanged_source_commits_nothing() {
        let host = cell();
        let gate = [approval("ops.old", MELT_ACTION, "admin")];
        commit_of(&host, false, &gate, 5_000).expect("gated");
        assert!(matches!(
            commit_of(&host, false, &gate, 6_000).expect("same"),
            Committed::Unchanged
        ));
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "read-definition fixtures"
)]
mod read_tests {
    //! DEV-READ-DEFINITIONS-1: the sources' questions and dashboards install
    //! over the serving revision, a re-sync appends nothing, a changed one
    //! appends its next revision, a refused one keeps its active revision, an
    //! id of another kind is refused, and a dropped dashboard retires before
    //! a dropped question. Each install or retirement is a definitions
    //! version, which the revision `norn dev` reports names.
    use super::*;
    use crate::dev::policy::tests::{ACTOR, artifacts, host, metric, question, scope};
    use crate::dev::policy::{DevMetrics, dev_actions, dev_kinds};
    use crate::dev::{Events, Manifest, Session};
    use norn_cell::engine::NativeEngine;
    use std::cell::RefCell;

    const QUESTION: &str = "dev.amount_by_region";
    const DASHBOARD: &str = "dev.board";

    fn question_def(metric: &str, end: &str) -> Value {
        let mut payload = question(metric);
        payload["name"] = json!(QUESTION);
        payload["window"]["end"] = json!(end);
        json!({ "kind": "question", "id": QUESTION, "payload": payload })
    }

    fn dashboard_def(metric: &str) -> Value {
        json!({ "kind": "dashboard", "id": DASHBOARD, "payload": {
            "title": "Sales", "origin": { "by": "code" },
            "tiles": [{ "id": "t1", "title": "Amount by region", "question": question(metric),
                "view": { "kind": "table" }, "layout": { "x": 0, "y": 0, "w": 2, "h": 1 } }] } })
    }

    fn sync(host: &DevHost<NativeEngine>, definitions: &[Value], now_ms: u64) -> Synced {
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let runtime = metrics.questions(host).expect("runtime");
        let questions: &dyn AdHocQuestions<NativeEngine> = &runtime;
        sync_reads(
            host,
            ACTOR,
            &dev_kinds(),
            Some(questions),
            definitions,
            now_ms,
        )
    }

    fn version(host: &DevHost<NativeEngine>) -> u64 {
        host.active_definitions().unwrap().unwrap().version
    }

    fn revision(host: &DevHost<NativeEngine>, id: &str) -> (u64, RetainedLifecycle) {
        let record = host.retained_definition(id).unwrap().expect("a revision");
        (record.definition.revision, record.definition.lifecycle)
    }

    fn summary(synced: &Synced) -> Vec<(String, String, u64, String)> {
        synced
            .events
            .iter()
            .map(|event| {
                (
                    event["kind"].as_str().unwrap().to_owned(),
                    event["id"].as_str().unwrap().to_owned(),
                    event["version"].as_u64().unwrap(),
                    event["lifecycle"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn row(kind: &str, id: &str, version: u64, lifecycle: &str) -> (String, String, u64, String) {
        (kind.into(), id.into(), version, lifecycle.into())
    }

    #[test]
    fn reads_install_once_update_and_retire_dashboards_first() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        let base = version(&host);
        let declared = [
            question_def(&amount, "2026-11-08T00:00:00Z"),
            dashboard_def(&amount),
        ];

        // Install: each is its next definitions version.
        let installed = sync(&host, &declared, 10_000);
        assert!(installed.refused.is_empty(), "{:?}", installed.refused);
        assert_eq!(
            summary(&installed),
            [
                row("question", QUESTION, 1, "active"),
                row("dashboard", DASHBOARD, 1, "active")
            ]
        );
        assert_eq!(version(&host), base + 2);

        // The same sources again append nothing.
        let again = sync(&host, &declared, 11_000);
        assert!(
            again.events.is_empty() && again.refused.is_empty(),
            "{again:?}"
        );
        assert_eq!(version(&host), base + 2);

        // A changed question appends its next revision; the dashboard stays.
        let changed = [
            question_def(&amount, "2026-11-15T00:00:00Z"),
            dashboard_def(&amount),
        ];
        let updated = sync(&host, &changed, 12_000);
        assert!(updated.refused.is_empty(), "{:?}", updated.refused);
        assert_eq!(summary(&updated), [row("question", QUESTION, 2, "active")]);
        assert_eq!(version(&host), base + 3);

        // Dropping both retires the dashboard before the question.
        let dropped = sync(&host, &[], 13_000);
        assert!(dropped.refused.is_empty(), "{:?}", dropped.refused);
        assert_eq!(
            summary(&dropped),
            [
                row("dashboard", DASHBOARD, 2, "retired"),
                row("question", QUESTION, 3, "retired")
            ]
        );
        assert_eq!(revision(&host, QUESTION), (3, RetainedLifecycle::Retired));
    }

    #[test]
    fn a_refused_question_keeps_its_active_revision() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        let first = sync(
            &host,
            &[question_def(&amount, "2026-11-08T00:00:00Z")],
            10_000,
        );
        assert!(first.refused.is_empty(), "{:?}", first.refused);
        let base = version(&host);

        let unknown = question_def("analysis.no_such_metric", "2026-11-15T00:00:00Z");
        let refused = sync(&host, &[unknown], 11_000);
        assert!(refused.events.is_empty(), "{refused:?}");
        assert!(!refused.refused.is_empty(), "the unknown metric is refused");
        assert!(
            refused
                .refused
                .iter()
                .all(|finding| finding["definitionId"] == json!(QUESTION)),
            "{:?}",
            refused.refused
        );
        assert_eq!(revision(&host, QUESTION), (1, RetainedLifecycle::Active));
        assert_eq!(version(&host), base, "nothing was written");
    }

    /// The local developer's grants, except that no definition is readable.
    struct Unreadable(crate::dev::policy::LocalDeveloper);

    impl norn_api::authz::Authorizer for Unreadable {
        fn read(&self, request: &norn_api::authz::ReadRequest<'_>) -> Result<(), Refusal> {
            self.0.read(request)
        }
        fn authorize(&self, request: &norn_api::authz::AccessRequest<'_>) -> Result<(), Refusal> {
            if matches!(
                request.operation,
                norn_api::authz::Operation::ReadDefinitions
            ) {
                return Err(norn_api::authz::read_denied());
            }
            self.0.authorize(request)
        }
    }

    /// Only "not installed" is a first install: a current revision that
    /// cannot be read is reported, and nothing is installed over it.
    #[test]
    fn an_unreadable_definition_is_refused_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let readable = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &readable, "amount", false);
        let host = readable.with_authorizer(Unreadable(
            crate::dev::policy::LocalDeveloper::for_actor(ACTOR, dev_actions()),
        ));
        let base = version(&host);
        let synced = sync(
            &host,
            &[question_def(&amount, "2026-11-08T00:00:00Z")],
            10_000,
        );
        assert!(synced.events.is_empty(), "{synced:?}");
        // The read of the question is refused, and so is each kind's retire pass.
        let refused: Vec<(&Value, &Value)> = synced
            .refused
            .iter()
            .map(|finding| (&finding["definitionId"], &finding["code"]))
            .collect();
        let denied = json!(norn_api::refusal::codes::AUTHZ_DENIED);
        assert_eq!(
            refused,
            [
                (&json!(QUESTION), &denied),
                (&json!("dashboard"), &denied),
                (&json!("question"), &denied)
            ]
        );
        assert!(host.retained_definition(QUESTION).unwrap().is_none());
        assert_eq!(version(&host), base, "nothing was written");
    }

    /// A readback that fails after an install committed is a warning, not
    /// the session's end; the version reported is never a stale one.
    #[test]
    fn a_failed_readback_after_an_install_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let installed = host.active_definitions().unwrap().unwrap();
        let after = InstalledDefinitionSet {
            version: installed.version + 1,
            ..installed.clone()
        };
        let mut out = Events { json: true };
        let mut reported = |moved, readback: Result<Option<InstalledDefinitionSet>, String>| {
            crate::dev::reported(&mut out, installed.clone(), moved, readback)
        };
        // No read moved the version: the install is what serves.
        assert_eq!(reported(false, Err("gone".into())), Some(installed.clone()));
        // A read moved it and the readback failed: no version is claimed.
        assert_eq!(reported(true, Err("gone".into())), None);
        assert_eq!(reported(true, Ok(Some(after.clone()))), Some(after));
        assert_eq!(reported(false, Ok(None)), Some(installed.clone()));
    }

    #[test]
    fn an_id_of_another_kind_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        let first = sync(
            &host,
            &[question_def(&amount, "2026-11-08T00:00:00Z")],
            10_000,
        );
        assert!(first.refused.is_empty(), "{:?}", first.refused);
        let base = version(&host);

        let mut board = dashboard_def(&amount);
        board["id"] = json!(QUESTION);
        let refused = sync(&host, &[board], 11_000);
        assert!(refused.events.is_empty(), "{refused:?}");
        let codes: Vec<&Value> = refused.refused.iter().map(|f| &f["code"]).collect();
        assert_eq!(codes, [&json!("norn.dev.definition_kind")]);
        assert_eq!(revision(&host, QUESTION), (1, RetainedLifecycle::Active));
        assert_eq!(version(&host), base);
    }

    /// The revision `norn dev` reports (and writes to `.norn/dev.json`) after
    /// a save is the one serving once the reads synced: a save that changes
    /// only a question is a new revision, and a save that installs a set and
    /// a question names the version after both.
    #[test]
    fn a_save_reports_the_version_after_its_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cell.sqlite");
        let serving: crate::dev::http::Serving = std::sync::Arc::new(std::sync::RwLock::new(None));
        let manifest: Manifest = serde_json::from_value(json!({
            "format": "norn-project/1", "name": "shop", "sources": [],
            "scope": { "orgId": scope().org_id, "environmentId": scope().environment_id },
            "dev": { "port": 0, "actor": ACTOR }, "cell": "cell.sqlite"
        }))
        .unwrap();
        let cell_scope = scope();
        let session = Session {
            loads: &[],
            loaded: RefCell::new(None),
            root: directory.path(),
            manifest: &manifest,
            named: std::sync::Arc::default(),
            metrics: std::sync::Arc::new(DevMetrics::new([7_u8; 32], cell_scope.clone())),
            scope: &cell_scope,
            url: "http://127.0.0.1:0",
            started_at: 0,
            actions: dev_actions(),
            ungoverned: RefCell::new(None),
        };
        let save = |artifacts: &[Value], definitions: &[Value]| {
            let event = json!({ "ok": true, "gen": 1, "sourceDigest": "test",
                "artifacts": artifacts, "sourceMap": [], "definitions": definitions });
            session
                .compiled(&mut Events { json: true }, &serving, &path, &event)
                .map_err(|failure| failure.message)
                .expect("the save does not end the session")
        };
        let active = || {
            serving
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .active_definitions()
                .unwrap()
                .unwrap()
                .version
        };
        let first = save(&artifacts(true), &[]).expect("provisioned");
        let amount = {
            let guard = serving.read().unwrap();
            metric(&session.metrics, guard.as_ref().unwrap(), "amount", false)
        };

        // The set is unchanged; only a question is new.
        let asked = save(
            &artifacts(true),
            &[question_def(&amount, "2026-11-08T00:00:00Z")],
        )
        .expect("a new question is a new revision");
        assert_eq!(asked.version, first.version + 1);
        assert_eq!(asked.version, active());
        assert!(
            save(
                &artifacts(true),
                &[question_def(&amount, "2026-11-08T00:00:00Z")]
            )
            .is_none()
        );

        // A new set and a changed question: the version after both.
        let mut edited = artifacts(true);
        edited[0]["payload"]["objectType"]["properties"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "note", "type": {"kind": "text"}, "default": "none"}));
        let both =
            save(&edited, &[question_def(&amount, "2026-11-15T00:00:00Z")]).expect("installed");
        assert_eq!(both.version, active());
        assert_eq!(
            both.version,
            asked.version + 2,
            "the set, then the question"
        );
    }
}
