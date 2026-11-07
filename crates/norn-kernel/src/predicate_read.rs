//! Predicate reads: a complete, bounded read of every object of a type that
//! matches an equality filter and an optional window, guarded against
//! phantoms at commit (#1105; reused by #1104's property-selected slices).
//!
//! A turn that folds or lists the objects matching a predicate records the
//! predicate and every member it saw ([`PredicateReadExpectation`]). Inside
//! the commit transaction [`crate::check_commit`] asks the host for the
//! current members, bounded at one more than the recorded set, and refuses
//! with [`crate::CommitError::PredicateReadConflict`] unless the canonical
//! member set and every member's version are unchanged. An insert, delete, or
//! update that moves an object into or out of the predicate, or changes a
//! member, therefore conflicts, with no single-writer lane.
//!
//! Matching is defined once, here ([`PredicateQuery::matches`]); hosts
//! enumerate candidates and apply it, so every host agrees on membership.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::cell_host::{FactKey, FactVersion, JsonObject, JsonValue, ObjectTypeName};
use crate::{CommitError, CommitModel, CommitView};

/// Most members one predicate read may record, matching the slice profile.
pub const MAX_PREDICATE_MEMBERS: usize = 1_000;
/// Most aggregate member-key bytes one predicate read may record.
pub const MAX_PREDICATE_MEMBER_BYTES: usize = 262_144;
/// Most equality terms one predicate may carry.
pub const MAX_PREDICATE_TERMS: usize = 32;
/// Most bytes of one property name or scalar text operand.
pub const MAX_PREDICATE_OPERAND_BYTES: usize = 1_024;

/// How a window's edges and values are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateWindowKind {
    /// ISO 8601 calendar dates (`YYYY-MM-DD`), ordered by day.
    Date,
    /// RFC 3339 timestamps, ordered by their exact UTC instant.
    Timestamp,
}

/// A window over one date or timestamp property. Both edges and their
/// inclusivity are explicit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredicateWindow {
    /// The property the window ranges over.
    pub property: String,
    /// How edges and values are ordered.
    pub kind: PredicateWindowKind,
    /// The start edge.
    pub start: String,
    /// The end edge.
    pub end: String,
    /// Whether a value equal to `start` matches.
    pub start_inclusive: bool,
    /// Whether a value equal to `end` matches.
    pub end_inclusive: bool,
}

/// Which objects a predicate read selects: every live object whose concrete
/// type is `object_type` (or a subtype or implementer) whose properties equal
/// `filter` and, when present, fall inside `window`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredicateQuery {
    /// The type whose objects are selected.
    pub object_type: ObjectTypeName,
    /// Property equality terms. A term matches only a present, non-null,
    /// equal scalar; decimals compare by value, not spelling.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filter: BTreeMap<String, JsonValue>,
    /// The optional window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<PredicateWindow>,
}

/// One member observed by a predicate read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredicateReadMember {
    /// The member's identity.
    pub key: FactKey,
    /// The version observed.
    pub version: FactVersion,
}

/// One matching object a host read for a turn: the member plus its values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredicateObject {
    /// The member's identity, with its concrete type.
    pub key: FactKey,
    /// The version read.
    pub version: FactVersion,
    /// The property values read.
    pub values: JsonObject,
}

impl PredicateObject {
    /// The member this object contributes to a predicate read.
    #[must_use]
    pub fn member(&self) -> PredicateReadMember {
        PredicateReadMember {
            key: self.key.clone(),
            version: self.version,
        }
    }
}

/// A complete bounded predicate read, guarded against phantom matches at
/// commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredicateReadExpectation {
    /// What was selected.
    pub predicate: PredicateQuery,
    /// Every member observed, in canonical key order.
    pub members: Vec<PredicateReadMember>,
}

impl PredicateQuery {
    /// Whether an object's property values satisfy the predicate. Type
    /// membership is the host's: it only offers objects of the queried type.
    #[must_use]
    pub fn matches(&self, values: &JsonObject) -> bool {
        let terms = self.filter.iter().all(|(property, expected)| {
            values
                .get(property.as_str())
                .is_some_and(|actual| !actual.is_null() && actual == expected)
        });
        terms
            && self.window.as_ref().is_none_or(|window| {
                values
                    .get(window.property.as_str())
                    .and_then(JsonValue::as_str)
                    .is_some_and(|value| window.contains(value))
            })
    }

    fn check_shape<X>(&self) -> Result<(), CommitError<X>> {
        let invalid = |reason: &'static str| CommitError::InvalidPredicateRead {
            object_type: self.object_type.clone(),
            reason,
        };
        if self.filter.len() > MAX_PREDICATE_TERMS {
            return Err(invalid("too many equality terms"));
        }
        for (property, value) in &self.filter {
            if property.is_empty() || property.len() > MAX_PREDICATE_OPERAND_BYTES {
                return Err(invalid("a filter property name is empty or too long"));
            }
            let scalar = match value {
                JsonValue::Bool(_) | JsonValue::Number(_) => true,
                JsonValue::String(text) => text.len() <= MAX_PREDICATE_OPERAND_BYTES,
                JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => false,
            };
            if !scalar {
                return Err(invalid("a filter operand is not a bounded non-null scalar"));
            }
        }
        if let Some(window) = &self.window {
            if window.property.is_empty() || window.property.len() > MAX_PREDICATE_OPERAND_BYTES {
                return Err(invalid("the window property name is empty or too long"));
            }
            let (Some(start), Some(end)) = (window.key(&window.start), window.key(&window.end))
            else {
                return Err(invalid("a window edge is not a valid date or timestamp"));
            };
            if start > end {
                return Err(invalid("the window starts after it ends"));
            }
        }
        Ok(())
    }
}

/// An ordering key for one date or timestamp value.
type WindowKey = (i64, u32);

impl PredicateWindow {
    fn key(&self, value: &str) -> Option<WindowKey> {
        match self.kind {
            // A valid `YYYY-MM-DD` spelling orders lexically by day.
            PredicateWindowKind::Date => {
                norn_expr::temporal::validate_date(value)
                    .ok()
                    .and_then(|()| {
                        let digits: String = value.chars().filter(char::is_ascii_digit).collect();
                        digits.parse::<i64>().ok().map(|day| (day, 0))
                    })
            }
            PredicateWindowKind::Timestamp => {
                norn_expr::temporal::timestamp_ordering_key(value).ok()
            }
        }
    }

    /// Whether `value` lies inside the window; an unreadable value never does.
    #[must_use]
    pub fn contains(&self, value: &str) -> bool {
        let (Some(value), Some(start), Some(end)) =
            (self.key(value), self.key(&self.start), self.key(&self.end))
        else {
            return false;
        };
        let after_start = if self.start_inclusive {
            value >= start
        } else {
            value > start
        };
        let before_end = if self.end_inclusive {
            value <= end
        } else {
            value < end
        };
        after_start && before_end
    }
}

/// Check predicate-read shapes before host access: bounded operands, valid
/// edges, a known type, canonical strictly ordered members of that type, and
/// no predicate read twice.
pub(crate) fn check_predicate_read_shapes<X>(
    reads: &[PredicateReadExpectation],
    model: CommitModel<'_, '_>,
) -> Result<(), CommitError<X>> {
    let mut seen: Vec<&PredicateQuery> = Vec::with_capacity(reads.len());
    for read in reads {
        let predicate = &read.predicate;
        let invalid = |reason: &'static str| CommitError::InvalidPredicateRead {
            object_type: predicate.object_type.clone(),
            reason,
        };
        predicate.check_shape()?;
        if seen.contains(&predicate) {
            return Err(invalid("the same predicate is read twice"));
        }
        seen.push(predicate);
        if read.members.len() > MAX_PREDICATE_MEMBERS {
            return Err(invalid("too many members"));
        }
        let bytes = read.members.iter().fold(0_usize, |total, member| {
            total.saturating_add(member.key.key.len())
        });
        if bytes > MAX_PREDICATE_MEMBER_BYTES {
            return Err(invalid("member keys exceed the byte bound"));
        }
        let mut previous: Option<FactKey> = None;
        let mut keys = BTreeSet::new();
        for member in &read.members {
            crate::commit_check::check_fact_key_identity(&member.key)?;
            if !model.is_a(&member.key.object_type, &predicate.object_type) {
                return Err(invalid("a member is not of the predicate's type"));
            }
            let canonical = model.canonical(&member.key);
            if previous.as_ref().is_some_and(|prior| prior >= &canonical)
                || !keys.insert(canonical.clone())
            {
                return Err(invalid("members are not in strict canonical key order"));
            }
            previous = Some(canonical);
        }
    }
    Ok(())
}

/// Recheck every predicate read against the commit's view: the canonical
/// member set and each member's version must be unchanged.
pub(crate) fn recheck_predicate_reads<V: CommitView + ?Sized>(
    reads: &[PredicateReadExpectation],
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    for read in reads {
        let canonical = |members: &[PredicateReadMember]| -> Vec<PredicateReadMember> {
            let mut members: Vec<_> = members
                .iter()
                .map(|member| PredicateReadMember {
                    key: model.canonical(&member.key),
                    version: member.version,
                })
                .collect();
            members.sort();
            members
        };
        // One more than recorded is enough to see any phantom addition.
        let limit = read.members.len().saturating_add(1);
        let Some(current) = view
            .predicate_members(&read.predicate, limit)
            .map_err(CommitError::Host)?
        else {
            return Err(CommitError::UnsupportedPredicateRead {
                object_type: read.predicate.object_type.clone(),
            });
        };
        if canonical(&current) != canonical(&read.members) {
            return Err(CommitError::PredicateReadConflict {
                object_type: read.predicate.object_type.clone(),
            });
        }
    }
    Ok(())
}

/// Select the members of `predicate` among `candidates` (objects of the
/// queried type, any order): matching, canonically ordered, at most `limit`.
/// Hosts that enumerate candidates in memory share this helper.
#[must_use]
pub fn select_members<'a, I>(
    predicate: &PredicateQuery,
    candidates: I,
    limit: usize,
) -> Vec<PredicateObject>
where
    I: IntoIterator<Item = (&'a FactKey, FactVersion, &'a JsonObject)>,
{
    let mut members: Vec<PredicateObject> = candidates
        .into_iter()
        .filter(|(_, _, values)| predicate.matches(values))
        .map(|(key, version, values)| PredicateObject {
            key: key.clone(),
            version,
            values: values.clone(),
        })
        .collect();
    members.sort_by(|left, right| left.key.key.cmp(&right.key.key));
    members.truncate(limit);
    members
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "kernel matcher assertions")]
    use super::*;

    fn values(json: serde_json::Value) -> JsonObject {
        serde_json::from_value(json).expect("values")
    }

    fn query(json: serde_json::Value) -> PredicateQuery {
        serde_json::from_value(json).expect("predicate")
    }

    #[test]
    fn equality_compares_decimals_by_value_and_never_matches_absent_or_null() {
        let predicate = query(serde_json::json!({
            "objectType": "books.Entry",
            "filter": { "amount": 1.5, "account": "A" }
        }));
        assert!(predicate.matches(&values(
            serde_json::json!({ "amount": 1.50, "account": "A" })
        )));
        assert!(!predicate.matches(&values(serde_json::json!({ "amount": 1.5 }))));
        assert!(!predicate.matches(&values(
            serde_json::json!({ "amount": 1.5, "account": null })
        )));
        assert!(!predicate.matches(&values(serde_json::json!({ "amount": 2, "account": "A" }))));
    }

    #[test]
    fn window_edges_follow_their_declared_inclusivity() {
        let window = |start_inclusive, end_inclusive| PredicateWindow {
            property: "on".to_owned(),
            kind: PredicateWindowKind::Date,
            start: "2026-11-01".to_owned(),
            end: "2026-11-30".to_owned(),
            start_inclusive,
            end_inclusive,
        };
        for (start, end, at_start, at_end) in [
            (true, true, true, true),
            (true, false, true, false),
            (false, true, false, true),
            (false, false, false, false),
        ] {
            let window = window(start, end);
            assert_eq!(window.contains("2026-11-01"), at_start);
            assert_eq!(window.contains("2026-11-30"), at_end);
            assert!(window.contains("2026-11-15"));
            assert!(!window.contains("2026-12-01"));
            assert!(!window.contains("not a date"));
        }
    }
}
