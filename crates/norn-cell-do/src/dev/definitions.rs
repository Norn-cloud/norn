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
use std::collections::BTreeSet;

use norn_api::cell_definitions::source_approvals;
use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_cell::InstalledDefinitionSet;
use norn_cell::engine::SqlEngine;
use norn_cell::retained::{RetainedDefinitionChange, RetainedLifecycle};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::admit::Admitted;
use super::policy::{self, DevActions};

/// The retained kinds a project's sources may declare.
pub(crate) const SOURCE_KINDS: [&str; 1] = ["approval"];

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

/// One declared definition `{ kind, id, payload }`, as `(id, payload)`.
fn declared(value: &Value) -> Result<(String, Box<RawValue>), Value> {
    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    let kind = value.get("kind").and_then(Value::as_str);
    let payload = value.get("payload").filter(|payload| payload.is_object());
    match (kind, payload) {
        (Some(kind), Some(payload)) if SOURCE_KINDS.contains(&kind) && !id.is_empty() => {
            let raw = RawValue::from_string(payload.to_string())
                .map_err(|error| finding(id, "norn.dev.definition_shape", error.to_string()))?;
            Ok((id.to_owned(), raw))
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
    let mut refused = Vec::new();
    let mut seen = BTreeSet::new();
    let mut declarations: Vec<(String, Box<RawValue>)> = Vec::new();
    for value in definitions {
        match declared(value) {
            Ok((id, _)) if !seen.insert(id.clone()) => {
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
    if !refused.is_empty() {
        return Err(refused);
    }
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
            dev_kinds(),
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
            dev_kinds(),
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
        let refused = prepare(&host, ACTOR, dev_kinds(), &[gate.clone(), gate], "[]", 1)
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
