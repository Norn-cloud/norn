//! The `approval` retained-definition kind (APPROVAL-1): an organization's
//! record that calling one admitted action waits for a person's decision.
//!
//! Payload `{action, decider: {roles, law?}, question, risk, expiresInMs?,
//! when?}`.
//! While an approval for an action is active, every call of that action, by
//! any surface, is decided for its caller first and then parked as an open
//! decision instead of committing; a holder of one of `decider.roles` who is
//! not the caller approves or rejects it, and an approved call commits under
//! the caller's own authority, decided again at that commit. Installing or
//! revising one changes who may complete an action, so it is installed only by
//! signing the server's words for it, as a flow is.
//!
//! APPROVAL-WHEN-1: `when` (`{property, op, value}`) narrows the approval to
//! the calls whose written record satisfies it: the server compares the
//! record the call would commit (its proposed values) with `value`, and only
//! then parks the call; any other call commits at once. Numbers compare as
//! exact decimals (a decimal string is a number); text and booleans compare
//! with `eq`/`ne` only. A record without the property, or a comparison the
//! values do not support, parks the call (fail closed).

use std::collections::BTreeSet;

use norn_cell::engine::SqlEngine;
use norn_cell::retained::RetainedDefinition;
use serde_json::Value;

use crate::host::DevHost;
use crate::refusal::{Refusal, RefusalDiagnostic};

/// The payload's members; any other is a finding.
const MEMBERS: [&str; 6] = [
    "action",
    "decider",
    "question",
    "risk",
    "expiresInMs",
    "when",
];

/// The comparison operators of `when`.
pub const WHEN_OPS: [&str; 6] = ["eq", "ne", "lt", "le", "gt", "ge"];

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
        .map_or_default(|roles| roles.iter().filter_map(Value::as_str).collect());
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
    if let Some(when) = object.get("when") {
        check_when(when, &mut findings);
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

fn check_when(when: &Value, findings: &mut Vec<RefusalDiagnostic>) {
    let shaped = when.as_object().is_some_and(|when| {
        when.keys()
            .all(|key| matches!(key.as_str(), "property" | "op" | "value"))
            && when
                .get("property")
                .and_then(Value::as_str)
                .is_some_and(|property| {
                    property
                        .chars()
                        .next()
                        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                        && property
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            && when
                .get("op")
                .and_then(Value::as_str)
                .is_some_and(|op| WHEN_OPS.contains(&op))
            && when.get("value").is_some_and(|value| {
                // #1469 S10: a JSON number is exact only as an integer; a
                // fractional threshold is decimal text, so it is compared
                // and restated exactly as written.
                value.is_string() || value.is_i64() || value.is_u64() || value.is_boolean()
            })
    });
    if !shaped {
        findings.push(finding(
            codes::INVALID,
            "when is {property: a property id, op: eq|ne|lt|le|gt|ge, value: text, an integer or boolean; a fractional value is decimal text such as \"1000.10\"}",
            "/when",
        ));
    }
}

/// `when` in words: `amount > "1000"` (text) or `amount > 1000` (a number).
fn when_words(when: &Value) -> Option<String> {
    let property = when.get("property")?.as_str()?;
    let op = match when.get("op")?.as_str()? {
        "eq" => "=",
        "ne" => "≠",
        "lt" => "<",
        "le" => "≤",
        "gt" => ">",
        "ge" => "≥",
        _ => return None,
    };
    // #1469 S9: the operand is restated as JSON, so text is quoted and
    // escaped (`"1"`) and a number or boolean is bare (`1`). Against a text
    // property the two compare differently, so the signed words must differ.
    let value = when.get("value")?.to_string();
    Some(format!("{property} {op} {value}"))
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
        .map_or_default(|roles| roles.iter().filter_map(Value::as_str).collect());
    let property = text("/when/property");
    let first = match (value.get("when").and_then(when_words), property) {
        // APPROVAL-WHEN-1 fails closed: a condition it cannot evaluate parks.
        (Some(when), Some(property)) => format!(
            "A call of {action} that writes a record with {when} waits for a person to approve it, and so does one whose record's {property} is missing or cannot be compared; any other call commits at once."
        ),
        _ => format!("Every call of {action} waits for a person to approve it."),
    };
    let mut lines = vec![
        first,
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

/// The admitted action ids of the active definitions, and the actions an
/// active approval other than `id` already gates.
pub(crate) fn approval_context<E: SqlEngine>(
    host: &DevHost<E>,
    id: &str,
) -> Result<(BTreeSet<String>, BTreeSet<String>), Refusal> {
    let actions = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        reader
            .installed_definitions(None)
            .map_err(Refusal::host)?
            .map(|active| serde_json::from_str::<Vec<Value>>(&active.canonical_set))
            .transpose()
            .map_err(Refusal::host)?
            .unwrap_or_default()
            .iter()
            .filter(|artifact| artifact.get("kind").and_then(Value::as_str) == Some("action"))
            .filter_map(|artifact| {
                artifact
                    .pointer("/payload/actionId")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    };
    Ok((actions, gated_except(host, &|other| other == id)?))
}

pub(crate) fn gated_except<E: SqlEngine>(
    host: &DevHost<E>,
    skip: &dyn Fn(&str) -> bool,
) -> Result<BTreeSet<String>, Refusal> {
    let mut gated = BTreeSet::new();
    for record in active_approvals(host)? {
        if skip(&record.id) {
            continue;
        }
        let payload: Value = serde_json::from_str(&record.payload).map_err(Refusal::host)?;
        if let Some(action) = payload.get("action").and_then(Value::as_str) {
            gated.insert(action.to_owned());
        }
    }
    Ok(gated)
}

/// Every active approval revision, ordered by id.
pub(crate) fn active_approvals<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<Vec<RetainedDefinition>, Refusal> {
    let limit = crate::definition_routes::MAX_LIST_LIMIT;
    let cell = host.cell()?;
    let reader = cell.store().read();
    let mut approvals = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = reader
            .retained_definitions("approval", false, after.as_deref(), limit)
            .map_err(Refusal::host)?;
        let full = page.len() >= usize::try_from(limit).unwrap_or(usize::MAX);
        after = page.last().map(|record| record.definition.id.clone());
        approvals.extend(page.into_iter().map(|record| record.definition));
        if !full {
            return Ok(approvals);
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "fixture findings")]
mod tests {
    //! APPROVAL-WHEN-1: a narrowed approval is checked and restated.
    use super::*;
    use serde_json::json;

    fn approval(when: &Value) -> Value {
        json!({ "action": "purchasing.request.submit", "decider": { "roles": ["admin"] },
                "question": "Approve this purchase request?", "risk": "medium", "when": when })
    }

    fn findings(payload: &Value) -> Vec<RefusalDiagnostic> {
        check(
            payload,
            &BTreeSet::from(["purchasing.request.submit".to_owned()]),
            &["admin"],
            &BTreeSet::new(),
        )
    }

    #[test]
    fn a_threshold_admits_and_is_restated_in_words() {
        let payload = approval(&json!({ "property": "amount", "op": "gt", "value": 1000 }));
        assert_eq!(findings(&payload), []);
        let words = restate(&payload.to_string()).unwrap_or_default();
        assert!(
            words.starts_with(
                "A call of purchasing.request.submit that writes a record with amount > 1000 waits for a person to approve it, and so does one whose record's amount is missing or cannot be compared; any other call commits at once."
            ),
            "{words}"
        );
    }

    /// #1469 S10: a decimal threshold is admitted and restated exactly as
    /// written; a fractional JSON number, which a double would round, is
    /// refused.
    #[test]
    fn a_decimal_threshold_stays_exact() {
        let payload = approval(&json!({ "property": "amount", "op": "gt", "value": "1000.10" }));
        assert_eq!(findings(&payload), []);
        let words = restate(&payload.to_string()).unwrap_or_default();
        assert!(
            words.contains(r#"with amount > "1000.10" waits"#),
            "{words}"
        );
        let integer = approval(&json!({ "property": "amount", "op": "gt", "value": 1000 }));
        assert_eq!(findings(&integer), []);
        let fractional = approval(&json!({ "property": "amount", "op": "gt", "value": 1000.1 }));
        let found = findings(&fractional);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, codes::INVALID);
    }

    /// #1469 S9: text and a number of the same digits compare differently,
    /// so they restate differently: text is quoted and escaped.
    #[test]
    fn a_text_operand_restates_apart_from_a_number() {
        let words = |value: Value| {
            restate(
                &approval(&json!({ "property": "code", "op": "eq", "value": value })).to_string(),
            )
            .unwrap_or_default()
        };
        let text = words(json!("1"));
        let number = words(json!(1));
        assert!(text.contains(r#"with code = "1" waits"#), "{text}");
        assert!(number.contains("with code = 1 waits"), "{number}");
        assert_ne!(text, number);
        let quoted = words(json!("1\" waits"));
        assert!(
            quoted.contains(r#"with code = "1\" waits" waits"#),
            "{quoted}"
        );
        assert_ne!(words(json!("true")), words(json!(true)));
    }

    #[test]
    fn a_malformed_condition_is_refused_at_when() {
        for when in [
            json!({ "property": "amount", "op": "between", "value": "1" }),
            json!({ "property": "self.amount", "op": "gt", "value": "1" }),
            json!({ "property": "amount", "op": "gt", "value": ["1"] }),
            json!({ "property": "amount", "op": "gt", "value": "1", "unit": "EGP" }),
            json!("amount > 1000"),
        ] {
            let found = findings(&approval(&when));
            assert_eq!(found.len(), 1, "{when}");
            assert_eq!(found[0].code, codes::INVALID, "{when}");
        }
    }
}
