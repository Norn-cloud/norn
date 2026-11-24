//! Decision answer validation and refusal disclosure helpers (DIAG-DISCLOSE-1).

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::decision_ledger::MAX_TEXT_CHARS;
use norn_cell::engine::SqlEngine;

use crate::authz::RefusalDetailRequest;
use crate::decisions::{DecideRequest, MAX_IDEMPOTENCY_KEY_BYTES, OpenDecision, decision_codes};
use crate::host::{ActionCell, DevHost};
use crate::refusal::{Refusal, RefusalDiagnostic, RefusalSpan, codes};

/// Governed action id passed to the refusal-detail PDP when a flow's
/// `norn.decision@1` step refuses on a decider law.
pub const DECISION_REFUSAL_ACTION: &str = "norn.decision.decide";

/// The single governed action resource bound to a decision's decider-law
/// refusal disclosure check (DIAG-DISCLOSE-1): the parked call's action for
/// an approval decision, or [`DECISION_REFUSAL_ACTION`] for a flow's
/// `norn.decision@1` step. Never an OR across multiple resources and never
/// chosen from client-supplied choice fields.
pub(crate) fn decider_law_action(decision: &OpenDecision) -> &str {
    decision
        .call
        .as_ref()
        .map_or(DECISION_REFUSAL_ACTION, |held| held.call.action.as_str())
}

/// Whether `actor` holds explicit refusal-detail disclosure authority for
/// `decision`'s decider-law refusal (DIAG-DISCLOSE-1), checked against the
/// single resource [`decider_law_action`] binds.
pub(crate) fn disclose_decider_law_refusal<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    decision: &OpenDecision,
) -> bool {
    host.authorizer
        .disclose_refusal_detail(&RefusalDetailRequest {
            scope: host.scope(),
            actor,
            action_id: decider_law_action(decision),
        })
}

/// The answer's own refusals, in the contract's order: an unevaluable
/// decider law, an unknown choice, a long note, a bad idempotency key.
pub(crate) fn check_answer<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    cell: &ActionCell<E>,
    decision: &OpenDecision,
    body: &DecideRequest,
) -> Result<(), Refusal> {
    if let Some(law_id) = &decision.request.decider.law {
        let revealable = disclose_decider_law_refusal(host, actor, decision);
        return Err(law_not_satisfied_refusal(
            cell,
            &decision.id,
            law_id,
            revealable,
        ));
    }
    if !decision
        .request
        .options
        .iter()
        .any(|option| option.id == body.choice)
    {
        return Err(at(
            decision_codes::UNKNOWN_CHOICE,
            "/choice",
            "choice names no option of this decision",
        ));
    }
    if body
        .note
        .as_deref()
        .is_some_and(|note| note.chars().count() > MAX_TEXT_CHARS)
    {
        return Err(at(
            decision_codes::NOTE_TOO_LONG,
            "/note",
            "the note is longer than 500 characters",
        ));
    }
    if body.idempotency_key.trim().is_empty()
        || body.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
    {
        return Err(at(
            codes::INVALID_REQUEST,
            "/idempotencyKey",
            "idempotencyKey must be 1-128 bytes",
        ));
    }
    Ok(())
}

fn at(code: &str, pointer: &str, message: &str) -> Refusal {
    let mut refusal = Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        code,
        message,
        "correct the field and decide again",
    );
    refusal.diagnostics = vec![RefusalDiagnostic {
        code: code.to_owned(),
        severity: "error".to_owned(),
        message: message.to_owned(),
        span: Some(RefusalSpan {
            pointer: pointer.to_owned(),
        }),
        law: None,
    }];
    refusal
}

/// Build a `403 decision.law_not_satisfied` refusal carrying the structured
/// law reason projected under the caller's disclosure authority. Because the
/// cell cannot evaluate the decider law, `values` stays empty rather than
/// echoing client-supplied request fields as evaluated law operands.
fn law_not_satisfied_refusal<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
    law_id: &str,
    revealable: bool,
) -> Refusal {
    let mut refusal = Refusal::new(
        StatusCode::FORBIDDEN,
        decision_codes::LAW_NOT_SATISFIED,
        "the decider law is not satisfied",
        "this decision names a law the cell cannot evaluate for the caller",
    );
    refusal.reason = Some(Box::new(norn_ir::RefusalReason {
        law: law_id.to_owned(),
        subject: format!("decision/{decision_id}"),
        values: BTreeMap::new(),
        pin: law_pin(cell, law_id),
    }));
    refusal.project_detail(revealable)
}

/// Pin `law_id` to its installed law artifact when present in the cell's model.
fn law_pin<E: SqlEngine>(
    cell: &ActionCell<E>,
    law_id: &str,
) -> Option<norn_ir::DataQualityRuleRef> {
    cell.store()
        .model()
        .law_artifacts()
        .iter()
        .find_map(|artifact| {
            let norn_ir::ArtifactPayload::Law(law) = &artifact.payload else {
                return None;
            };
            let id = law.id.as_value().and_then(norn_ir::JsonValue::as_string)?;
            (id == law_id).then(|| norn_ir::DataQualityRuleRef::Law {
                artifact_id: artifact.artifact_id.clone(),
                law_id: id.to_owned(),
                version: artifact.version.clone(),
            })
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "DIAG-DISCLOSE-1 unit checks")]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use norn_cell::decision_ledger::DecisionRequest;
    use norn_cell::engine::NativeEngine;
    use norn_cell::flow_run::RunStarter;
    use norn_cell::{CellStore, Model};
    use norn_ir::RuntimeValue;
    use norn_kernel::{CommandId, Generation};

    use super::*;
    use crate::approvals::{HeldCall, ParkedCall};
    use crate::authz::{AccessRequest, Authorizer, DenyAll};
    use crate::scope::CellScope;

    struct RecordingAuthorizer {
        allowed: BTreeSet<String>,
        queried: Arc<Mutex<Vec<String>>>,
    }

    impl Authorizer for RecordingAuthorizer {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
            DenyAll.authorize(request)
        }

        fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
            self.queried
                .lock()
                .unwrap()
                .push(request.action_id.to_owned());
            self.allowed.contains(request.action_id)
        }
    }

    fn host_with(allowed: &[&str]) -> (DevHost<NativeEngine>, Arc<Mutex<Vec<String>>>) {
        let model = Model::from_definitions(&[], &[]).unwrap();
        let store = CellStore::open_in_memory(model).unwrap();
        let queried = Arc::new(Mutex::new(Vec::new()));
        let authorizer = RecordingAuthorizer {
            allowed: allowed.iter().map(|s| (*s).to_owned()).collect(),
            queried: Arc::clone(&queried),
        };
        let host = DevHost::from_store(
            store,
            CellScope {
                org_id: "org_demo".into(),
                environment_id: "dev".into(),
            },
            |command| command.payload.clone(),
        )
        .unwrap()
        .with_authorizer(authorizer);
        (host, queried)
    }

    fn sample_decision(held_action: Option<&str>) -> OpenDecision {
        let request: DecisionRequest = serde_json::from_value(serde_json::json!({
            "question": "Approve refund?",
            "risk": "high",
            "options": [
                {
                    "id": "approve",
                    "label": "Approve",
                    "scope": "Issue refund",
                    "effect": { "action": "ops.refund.approve" }
                }
            ],
            "decider": {
                "roles": ["admin"],
                "law": "ops.refund.finance_limit"
            },
            "expiresInMs": 60_000
        }))
        .unwrap();
        OpenDecision {
            id: "dec_test".into(),
            command: CommandId("cmd_1".into()),
            generation: Generation(1),
            run_id: "run_1".into(),
            step: "review".into(),
            attempt: RuntimeValue::Number(1_i64.into()),
            request,
            requested_at_ms: 1_000,
            expires_at_ms: 61_000,
            call: held_action.map(|action| HeldCall {
                definition: "ops.melt_approval".into(),
                revision: 1,
                call: ParkedCall {
                    action: action.into(),
                    input: BTreeMap::new(),
                    idempotency_key: "k-parked".into(),
                    requester: RunStarter {
                        subject: "user:requester".into(),
                        actor_kind: "user".into(),
                        roles: vec!["member".into()],
                        role_table: "default".into(),
                    },
                    action_digest: "digest".into(),
                    definitions_version: None,
                    attempt: None,
                    trace: None,
                },
            }),
        }
    }

    fn sample_body() -> DecideRequest {
        DecideRequest {
            choice: "approve".into(),
            note: None,
            idempotency_key: "k-1".into(),
        }
    }

    /// DIAG-DISCLOSE-1: an approval decision binds solely to its parked call's
    /// action. Denying that action redacts the refusal even when the caller
    /// holds grants on `norn.decision.decide` and on the chosen option's action.
    #[test]
    fn approval_decision_binds_solely_to_held_call_action() {
        let decision = sample_decision(Some("constitution.melt.convert"));
        assert_eq!(decider_law_action(&decision), "constitution.melt.convert");

        let (host, queried) = host_with(&[DECISION_REFUSAL_ACTION, "ops.refund.approve"]);
        let cell = host.cell().unwrap();
        let refused = check_answer(&host, "user:admin", &cell, &decision, &sample_body())
            .expect_err("unevaluable decider law must refuse");
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(refused.code, decision_codes::LAW_NOT_SATISFIED);
        assert!(refused.reason.is_none());
        assert_eq!(refused.explanation, Refusal::UNPROVEN_DETAIL);
        assert_eq!(*queried.lock().unwrap(), vec!["constitution.melt.convert"]);

        let (host, queried) = host_with(&["constitution.melt.convert"]);
        let cell = host.cell().unwrap();
        let disclosed = check_answer(&host, "user:admin", &cell, &decision, &sample_body())
            .expect_err("unevaluable decider law must refuse");
        assert_eq!(*queried.lock().unwrap(), vec!["constitution.melt.convert"]);
        let reason = disclosed.reason.expect("explicit grant discloses reason");
        assert_eq!(reason.law, "ops.refund.finance_limit");
        assert_eq!(reason.subject, "decision/dec_test");
        assert!(
            reason.values.is_empty(),
            "unevaluated decider law must not copy choice into evaluated values"
        );
    }

    /// DIAG-DISCLOSE-1: a flow decision binds solely to `DECISION_REFUSAL_ACTION`.
    /// Denying `norn.decision.decide` redacts the refusal even when the caller
    /// holds a grant on the chosen option's `effect.action`.
    #[test]
    fn flow_decision_binds_solely_to_decision_refusal_action() {
        let decision = sample_decision(None);
        assert_eq!(decider_law_action(&decision), DECISION_REFUSAL_ACTION);

        let (host, queried) = host_with(&["ops.refund.approve"]);
        let cell = host.cell().unwrap();
        let refused = check_answer(&host, "user:admin", &cell, &decision, &sample_body())
            .expect_err("unevaluable decider law must refuse");
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(refused.code, decision_codes::LAW_NOT_SATISFIED);
        assert!(refused.reason.is_none());
        assert_eq!(refused.explanation, Refusal::UNPROVEN_DETAIL);
        assert_eq!(*queried.lock().unwrap(), vec![DECISION_REFUSAL_ACTION]);

        let (host, queried) = host_with(&[DECISION_REFUSAL_ACTION]);
        let cell = host.cell().unwrap();
        let disclosed = check_answer(&host, "user:admin", &cell, &decision, &sample_body())
            .expect_err("unevaluable decider law must refuse");
        assert_eq!(*queried.lock().unwrap(), vec![DECISION_REFUSAL_ACTION]);
        let reason = disclosed.reason.expect("explicit grant discloses reason");
        assert_eq!(reason.law, "ops.refund.finance_limit");
        assert!(reason.values.is_empty());
    }
}
