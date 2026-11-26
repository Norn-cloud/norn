//! PRINCIPAL-INPUT-1 (#1091): a request never supplies the principal.
//!
//! A decision reads the acting principal only through the inputs it declares,
//! and the host resolves those from the verified principal and the PDP. The
//! decide input member that carries them is `principal`, so a body that
//! submits a member of that name (at its top level, or at the top level of
//! an action's `input`) is refused with a 400 at the member's JSON pointer,
//! before anything is decoded or read. A deeper business field of the same
//! name is not the reserved member and is left alone.

use norn_ir::rule::DECIDE_PRINCIPAL_MEMBER;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::refusal::{Refusal, RefusalDiagnostic, codes};

/// The refusal of a submitted `principal` member at `pointer`.
#[must_use]
pub fn principal_member_refused(pointer: &str) -> Refusal {
    let message = "the principal is resolved by the host and is never accepted from a request";
    let mut refusal = Refusal::invalid(message);
    refusal.diagnostics = vec![RefusalDiagnostic::error(
        codes::INVALID_REQUEST,
        message,
        pointer,
    )];
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
    let mut deserializer = serde_json::Deserializer::from_str(body);
    let Ok((top_found, nested_found)) = TopPrincipalSeed { nested }.deserialize(&mut deserializer)
    else {
        return Ok(());
    };
    if deserializer.end().is_err() {
        return Ok(());
    }
    if top_found {
        return Err(principal_member_refused(&format!(
            "/{DECIDE_PRINCIPAL_MEMBER}"
        )));
    }
    if nested_found && let Some(nested) = nested {
        return Err(principal_member_refused(&format!(
            "/{nested}/{DECIDE_PRINCIPAL_MEMBER}"
        )));
    }
    Ok(())
}

struct TopPrincipalSeed<'a> {
    nested: Option<&'a str>,
}

impl<'de> DeserializeSeed<'de> for TopPrincipalSeed<'_> {
    type Value = (bool, bool);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct TopVisitor<'a> {
            nested: Option<&'a str>,
        }
        impl<'de> Visitor<'de> for TopVisitor<'_> {
            type Value = (bool, bool);
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut top_found = false;
                let mut nested_found = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == DECIDE_PRINCIPAL_MEMBER {
                        map.next_value::<IgnoredAny>()?;
                        top_found = true;
                    } else if self.nested == Some(key.as_str()) {
                        if map.next_value_seed(NestedPrincipalSeed)? {
                            nested_found = true;
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok((top_found, nested_found))
            }
        }
        deserializer.deserialize_map(TopVisitor {
            nested: self.nested,
        })
    }
}

struct NestedPrincipalSeed;

impl<'de> DeserializeSeed<'de> for NestedPrincipalSeed {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct NestedVisitor;
        impl<'de> Visitor<'de> for NestedVisitor {
            type Value = bool;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("any JSON value")
            }
            fn visit_bool<E>(self, _v: bool) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_i64<E>(self, _v: i64) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_u64<E>(self, _v: u64) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_f64<E>(self, _v: f64) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_str<E>(self, _v: &str) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_unit<E>(self) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(false)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
                let mut found = false;
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    if key == DECIDE_PRINCIPAL_MEMBER {
                        found = true;
                    }
                }
                Ok(found)
            }
        }
        deserializer.deserialize_any(NestedVisitor)
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
            (r#"{"principal":1} trailing"#, Some("input")),
            (r#"{"input":[1, true, null, "principal"]}"#, Some("input")),
            (r#"{"input":[{"principal":1}]}"#, Some("input")),
            ("not json", Some("input")),
            (r#"["principal"]"#, None),
        ] {
            assert!(refuse_principal_member(body, nested).is_ok(), "{body}");
        }
    }
}
