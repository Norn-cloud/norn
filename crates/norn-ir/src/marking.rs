//! Data markings attached to definitions and propagated through lineage.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Grammar of a marking identifier (for example `pii.contact`).
pub const MARKING_ID_PATTERN: &str = r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$";

/// Stable identity of a data marking.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct MarkingId(#[schemars(regex(pattern = MARKING_ID_PATTERN))] pub String);

impl MarkingId {
    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Relative sensitivity of a marking.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum MarkingSeverity {
    /// Lower-risk internal classification.
    Low,
    /// Sensitive data that needs routine protection.
    Moderate,
    /// High-impact data that should be tightly restricted.
    High,
    /// The strongest sensitivity level.
    Critical,
}

/// The projection applied when a principal lacks `reveal` for a marking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MaskPolicy {
    /// Replace the value with a redaction marker.
    Redacted {},
    /// Keep only the final `count` characters.
    LastN {
        /// Number of trailing characters to retain; must be greater than zero.
        #[serde(deserialize_with = "crate::members::deserialize_u32")]
        count: u32,
    },
    /// Replace the value with JSON null.
    Null {},
    /// Replace the value with a stable pseudonym such as `Customer #K3V9MQ7T2A`.
    /// It is a keyed hash of the cell's secret, `subject` and the value, so
    /// equal values keep equal pseudonyms (joins and counts still hold) and
    /// it cannot be reversed without the cell.
    Pseudonym {
        /// What the value identifies (for example `Customer`); fields that
        /// share a subject share pseudonyms.
        subject: String,
    },
    /// Leave the property out of the document altogether.
    Refuse {},
}

impl MaskPolicy {
    /// How much this projection hides: a projection may only be replaced by
    /// one of at least the same strength (`last-n` also by no larger count).
    /// `pseudonym` keeps equality only, `redacted` and `null` keep nothing,
    /// `refuse` hides even that the property exists.
    #[must_use]
    pub const fn strength(&self) -> u8 {
        match self {
            Self::LastN { .. } => 1,
            Self::Pseudonym { .. } => 2,
            Self::Redacted {} | Self::Null {} => 3,
            Self::Refuse {} => 4,
        }
    }

    /// Whether replacing `self` with `candidate` would show more of a value.
    #[must_use]
    pub const fn weakened_by(&self, candidate: &Self) -> bool {
        match (self, candidate) {
            (Self::LastN { count: old }, Self::LastN { count: new }) => *new > *old,
            _ => candidate.strength() < self.strength(),
        }
    }
}

/// A declared classification and its read projection policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkingIr {
    /// Stable id used by definitions and `reveal` grants.
    pub id: MarkingId,
    /// Human-readable name.
    pub label: String,
    /// Relative sensitivity of the data.
    pub severity: MarkingSeverity,
    /// Tenant-defined category path (for example `pii` or `confidential`).
    #[schemars(regex(pattern = MARKING_ID_PATTERN))]
    pub category: String,
    /// Read projection used when the principal cannot reveal this marking.
    pub mask: MaskPolicy,
}

#[cfg(test)]
mod tests {
    use super::MaskPolicy;

    #[test]
    fn last_n_accepts_exact_ir_integers_and_rejects_fractional_or_out_of_range_counts() {
        assert_eq!(
            serde_json::from_str::<MaskPolicy>(r#"{"kind":"last-n","count":4.0}"#).ok(),
            Some(MaskPolicy::LastN { count: 4 })
        );
        assert_eq!(
            serde_json::from_str::<MaskPolicy>(r#"{"kind":"pseudonym","subject":"Customer"}"#).ok(),
            Some(MaskPolicy::Pseudonym {
                subject: "Customer".to_owned()
            })
        );
        assert_eq!(
            serde_json::from_str::<MaskPolicy>(r#"{"kind":"refuse"}"#).ok(),
            Some(MaskPolicy::Refuse {})
        );
        for count in ["-1", "0.5", "4294967296"] {
            let json = format!(r#"{{"kind":"last-n","count":{count}}}"#);
            assert!(serde_json::from_str::<MaskPolicy>(&json).is_err(), "{json}");
        }
    }
}
