//! Decision answer validation and refusal disclosure helpers (DIAG-DISCLOSE-1).

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::decision_ledger::MAX_TEXT_CHARS;
use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeValue;

use crate::authz::RefusalDetailRequest;
use crate::decisions::{DecideRequest, MAX_IDEMPOTENCY_KEY_BYTES, OpenDecision, decision_codes};
use crate::host::{ActionCell, DevHost};
use crate::refusal::{Refusal, RefusalDiagnostic, RefusalSpan, codes};

/// Governed action id passed to the refusal-detail PDP when a decision refusal
/// has no callable governed action of its own (a binding choice or a decider
/// law).
pub const DECISION_REFUSAL_ACTION: &str = "norn.decision.decide";

/// Whether `actor` holds explicit refusal-detail disclosure authority for a
/// decision (DIAG-DISCLOSE-1): asks the host authorizer for the chosen
/// option's governed action (or the parked call's action), falling back to
/// [`DECISION_REFUSAL_ACTION`] when the choice names no callable action.
pub(crate) fn disclose_decision_refusal<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    decision: &OpenDecision,
    body: &DecideRequest,
) -> bool {
    let check = |action_id: &str| {
        host.authorizer
            .disclose_refusal_detail(&RefusalDetailRequest {
                scope: host.scope(),
                actor,
                action_id,
            })
    };
    let option_action = decision
        .request
        .options
        .iter()
        .find(|option| option.id == body.choice)
        .and_then(|option| option.effect.action.as_deref());
    let held_action = decision.call.as_ref().map(|held| held.call.action.as_str());
    option_action.is_some_and(check)
        || held_action.is_some_and(check)
        || check(DECISION_REFUSAL_ACTION)
}

/// The answer's own refusals, in the contract's order: an unevaluable
/// decider law, an unknown choice, a long note, a bad idempotency key.
pub(crate) fn check_answer<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision: &OpenDecision,
    body: &DecideRequest,
    revealable: bool,
) -> Result<(), Refusal> {
    if let Some(law_id) = &decision.request.decider.law {
        return Err(law_not_satisfied_refusal(
            cell,
            &decision.id,
            law_id,
            body,
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
/// law reason projected under the caller's disclosure authority.
fn law_not_satisfied_refusal<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
    law_id: &str,
    body: &DecideRequest,
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
        values: BTreeMap::from([(
            "choice".to_owned(),
            RuntimeValue::String(body.choice.clone()),
        )]),
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
