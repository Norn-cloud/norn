//! Declared invariants of object types and interfaces (T1-ONT-1d, #2514; ruling
//! 2026-09-09 §5 "declared invariants", expressed in ZEN Expression per
//! 2026-09-25 §3).
//!
//! An [`Invariant`] is a boolean ZEN Expression over an object's properties.
//! Integrity constraints veto violating commands; quality expectations do not.
//! Observations preserve external state and emit findings for either class.
//! Object types inherit invariants from supertypes;
//! implementing an interface also requires its invariants. A subtype may add
//! invariants but may never remove or replace one. The expression reads the
//! object through `self` (`self.total >= 0`); the checker parses and types it
//! against the owning object or interface contract, and enforcement at commit
//! is the kernel's.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::object::MEMBER_ID_PATTERN;
use crate::ZenExpression;
use crate::members::present_non_null;

/// How a declared invariant is applied at a write boundary.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintClass {
    /// A command that would violate the invariant is refused atomically.
    /// Observations still record the external state and emit a finding.
    #[default]
    Integrity,
    /// A violation does not refuse a command or observation. Observed
    /// violations produce expectation findings.
    Quality,
}

impl ConstraintClass {
    /// Whether this is the historical integrity default.
    #[must_use]
    pub const fn is_integrity(&self) -> bool {
        matches!(self, Self::Integrity)
    }
}

/// Id of an [`Invariant`], unique within its owner's effective invariant
/// contract. Same grammar as a property id ([`MEMBER_ID_PATTERN`]); invariants
/// have their own name space.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct InvariantId(#[schemars(regex(pattern = MEMBER_ID_PATTERN))] pub String);

impl InvariantId {
    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One declared invariant of an object type or interface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Invariant {
    /// Unique id among the type's invariants, inherited ones included.
    pub id: InvariantId,
    /// Whether violations reject commands (`integrity`) or remain findings
    /// (`quality`). Omitted wire values mean `integrity` for compatibility.
    #[serde(default, skip_serializing_if = "ConstraintClass::is_integrity")]
    pub classification: ConstraintClass,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// A boolean ZEN Expression over the object's or interface's properties.
    pub expression: ZenExpression,
    /// What a commit that violates the invariant reports; non-blank.
    pub message: String,
}
