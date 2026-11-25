//! PRINCIPAL-INPUT-1 (#1091): a request never supplies the principal.
//!
//! A decision reads the acting principal only through the inputs it declares,
//! and the host resolves those from the verified principal and the PDP. The
//! decide input member that carries them is `principal`, so a body that
//! submits a member of that name (at its top level, or at the top level of
//! an action's `input`) is refused with a 400 at the member's JSON pointer,
//! before anything is decoded or read. A deeper business field of the same
//! name is not the reserved member and is left alone.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::rule::DECIDE_PRINCIPAL_MEMBER;
use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};

use crate::refusal::{Refusal, RefusalDiagnostic, RefusalSpan, codes};

/// The refusal of a submitted `principal` member at `pointer`.
#[must_use]
pub fn principal_member_refused(pointer: &str) -> Refusal {
    let message = "the principal is resolved by the host and is never accepted from a request";
    let mut refusal = Refusal::invalid(message);
    refusal.diagnostics = vec![RefusalDiagnostic {
        code: codes::INVALID_REQUEST.to_owned(),
        severity: "error".to_owned(),
        message: message.to_owned(),
        span: Some(RefusalSpan {
            pointer: pointer.to_owned(),
        }),
        law: None,
    }];
    refusal
}

/// Refuse a JSON object `body` that submits `principal` at its top level
/// or, when `nested` names a member that is itself an object (an action's
/// `input`), at that member's top level. A body that is not a JSON object is
/// left to the caller's own decoder.
///
/// # Errors
///
/// `request.invalid` (400) with the member's pointer.
pub fn refuse_principal_member(body: &str, nested: Option<&str>) -> Result<(), Refusal> {
    let Ok(members) = serde_json::from_str::<BTreeMap<String, Member>>(body) else {
        return Ok(());
    };
    if members.contains_key(DECIDE_PRINCIPAL_MEMBER) {
        return Err(principal_member_refused(&format!(
            "/{DECIDE_PRINCIPAL_MEMBER}"
        )));
    }
    if let Some(nested) = nested
        && let Some(inner) = members.get(nested).and_then(Member::names)
        && inner.0.contains(DECIDE_PRINCIPAL_MEMBER)
    {
        return Err(principal_member_refused(&format!(
            "/{nested}/{DECIDE_PRINCIPAL_MEMBER}"
        )));
    }
    Ok(())
}

/// The member names of one JSON object.
struct Names(BTreeSet<String>);

impl<'de> Deserialize<'de> for Names {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NamesVisitor;
        impl<'de> Visitor<'de> for NamesVisitor {
            type Value = Names;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Names, A::Error> {
                let mut names = BTreeSet::new();
                while let Some(name) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    names.insert(name);
                }
                Ok(Names(names))
            }
        }
        deserializer.deserialize_map(NamesVisitor)
    }
}

/// One top-level member: its own member names when it is an object.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Member {
    Object(Names),
    Other(IgnoredAny),
}

impl Member {
    const fn names(&self) -> Option<&Names> {
        match self {
            Self::Object(names) => Some(names),
            Self::Other(IgnoredAny) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(refusal: &Refusal) -> Option<&str> {
        refusal
            .diagnostics
            .first()
            .and_then(|diagnostic| diagnostic.span.as_ref())
            .map(|span| span.pointer.as_str())
    }

    #[test]
    fn a_submitted_principal_member_refuses_400_at_its_pointer() {
        for (body, nested, expected) in [
            (
                r#"{"principal":{"isApprover":true},"input":{}}"#,
                Some("input"),
                "/principal",
            ),
            (
                r#"{"idempotencyKey":"k","input":{"amount":7,"principal":{"isApprover":true}}}"#,
                Some("input"),
                "/input/principal",
            ),
            (
                r#"{"choice":"approve","principal":"user:eve"}"#,
                None,
                "/principal",
            ),
        ] {
            let refusal = refuse_principal_member(body, nested).expect_err("refused");
            assert_eq!(refusal.status, 400);
            assert_eq!(refusal.code, codes::INVALID_REQUEST);
            assert_eq!(pointer(&refusal), Some(expected));
            // The refusal never repeats what was submitted.
            let document = serde_json::to_string(&refusal).expect("refusal serializes");
            assert!(!document.contains("isApprover") && !document.contains("user:eve"));
        }
    }

    #[test]
    fn other_members_and_deeper_business_fields_are_left_to_the_decoder() {
        for (body, nested) in [
            (
                r#"{"input":{"amount":"10.00"},"idempotencyKey":"k"}"#,
                Some("input"),
            ),
            (
                r#"{"input":{"loan":{"principal":"900.00"}}}"#,
                Some("input"),
            ),
            (r#"{"input":[{"principal":1}]}"#, Some("input")),
            ("not json", Some("input")),
            (r#"["principal"]"#, None),
        ] {
            assert!(refuse_principal_member(body, nested).is_ok(), "{body}");
        }
    }
}
