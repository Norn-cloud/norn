//! The `approval` retained-definition kind (APPROVAL-1): an organization's
//! record that calling one admitted action waits for a person's decision.
//!
//! Payload `{action, decider: {roles, law?}, question, risk, expiresInMs?}`.
//! While an approval for an action is active, every call of that action, by
//! any surface, is decided for its caller first and then parked as an open
//! decision instead of committing; a holder of one of `decider.roles` who is
//! not the caller approves or rejects it, and an approved call commits under
//! the caller's own authority, decided again at that commit. Installing or
//! revising one changes who may complete an action, so it is installed only by
//! signing the server's words for it, as a flow is.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::refusal::RefusalDiagnostic;

/// The payload's members; any other is a finding.
const MEMBERS: [&str; 5] = ["action", "decider", "question", "risk", "expiresInMs"];

/// The longest question, in characters.
pub const MAX_QUESTION_CHARS: usize = 500;

/// The longest an approval may wait: seven days.
pub const MAX_EXPIRES_IN_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Stable `definition.approval_*` codes.
pub mod codes {
    /// The payload is not the approval shape.
    pub const INVALID: &str = "definition.approval_invalid";
    /// The action is not admitted in the cell's active definitions.
    pub const ACTION_UNKNOWN: &str = "definition.approval_action_unknown";
    /// A decider role is not a role of the cell's declared role table.
    pub const ROLE_UNKNOWN: &str = "definition.approval_role_unknown";
    /// Another active approval already gates the action.
    pub const DUPLICATE: &str = "definition.approval_duplicate";
}

fn finding(code: &str, message: impl Into<String>, pointer: &str) -> RefusalDiagnostic {
    RefusalDiagnostic::error(code, message, pointer)
}

/// The findings for an approval payload. `actions` are the admitted action
/// ids; `roles` the cell's declared roles; `gated` the actions another active
/// approval already gates.
#[must_use]
pub fn check(
    payload: &Value,
    actions: &BTreeSet<String>,
    roles: &[&str],
    gated: &BTreeSet<String>,
) -> Vec<RefusalDiagnostic> {
    let mut findings = Vec::new();
    let Some(object) = payload.as_object() else {
        return vec![finding(codes::INVALID, "an approval is an object", "")];
    };
    for key in object.keys().filter(|key| !MEMBERS.contains(&key.as_str())) {
        findings.push(finding(
            codes::INVALID,
            format!("an approval has no member {key:?}"),
            &format!("/{key}"),
        ));
    }
    match object.get("action").and_then(Value::as_str) {
        Some(action) if actions.contains(action) => {
            if gated.contains(action) {
                findings.push(finding(
                    codes::DUPLICATE,
                    "another active approval already gates this action",
                    "/action",
                ));
            }
        }
        Some(_) => findings.push(finding(
            codes::ACTION_UNKNOWN,
            "the action is not admitted in this cell",
            "/action",
        )),
        None => findings.push(finding(
            codes::INVALID,
            "action is the admitted action id",
            "/action",
        )),
    }
    let decider = object.get("decider").and_then(Value::as_object);
    let named: Vec<&str> = decider
        .and_then(|decider| decider.get("roles"))
        .and_then(Value::as_array)
        .map(|roles| roles.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let shaped = decider.is_some_and(|decider| {
        decider.keys().all(|key| key == "roles" || key == "law")
            && decider.get("law").is_none_or(Value::is_string)
    });
    if !shaped || named.is_empty() {
        findings.push(finding(
            codes::INVALID,
            "decider is {roles: [one or more roles], law?}",
            "/decider",
        ));
    }
    for (index, role) in named.iter().enumerate() {
        if !roles.contains(role) {
            findings.push(finding(
                codes::ROLE_UNKNOWN,
                format!("{role:?} is not a role of this cell's role table"),
                &format!("/decider/roles/{index}"),
            ));
        }
    }
    let question = object.get("question").and_then(Value::as_str);
    if question.is_none_or(|question| {
        question.trim().is_empty() || question.chars().count() > MAX_QUESTION_CHARS
    }) {
        findings.push(finding(
            codes::INVALID,
            format!("question is 1 to {MAX_QUESTION_CHARS} characters"),
            "/question",
        ));
    }
    if !matches!(
        object.get("risk").and_then(Value::as_str),
        Some("low" | "medium" | "high")
    ) {
        findings.push(finding(
            codes::INVALID,
            "risk is low, medium or high",
            "/risk",
        ));
    }
    if object.get("expiresInMs").is_some_and(|expires| {
        expires
            .as_u64()
            .is_none_or(|ms| ms == 0 || ms > MAX_EXPIRES_IN_MS)
    }) {
        findings.push(finding(
            codes::INVALID,
            "expiresInMs is 1 ms to seven days",
            "/expiresInMs",
        ));
    }
    findings
}

/// The plain-words restatement a signer signs (D59). Deterministic.
///
/// # Errors
/// The payload is not an approval.
pub fn restate(canonical: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(canonical).map_err(|error| error.to_string())?;
    let text = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);
    let action = text("/action").ok_or("the approval names no action")?;
    let question = text("/question").ok_or("the approval asks no question")?;
    let roles: Vec<&str> = value
        .pointer("/decider/roles")
        .and_then(Value::as_array)
        .map(|roles| roles.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut lines = vec![
        format!("Every call of {action} waits for a person to approve it."),
        format!("It asks: {question}"),
        format!(
            "A holder of {} decides, and never the caller.",
            roles.join(" or ")
        ),
    ];
    if let Some(law) = text("/decider/law") {
        lines.push(format!("The decider must satisfy {law}."));
    }
    lines.push(format!(
        "An approved call commits under the caller's own authority, checked again then. Risk: {}.",
        text("/risk").unwrap_or("medium")
    ));
    Ok(lines.join("\n"))
}
