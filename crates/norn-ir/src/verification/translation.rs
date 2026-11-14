//! TRANSLATION-PRESERVES-1: what a translation protects and what is known about it.
use crate::ZenExpression;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One named observation, read on each side of a translation and compared exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranslationObservation {
    /// Stable observation identity.
    pub id: String,
    /// ZEN over the source record.
    pub source: ZenExpression,
    /// ZEN over the translated target record.
    pub target: ZenExpression,
}

/// The state of one goal. Only a checked solver answer can leave `Unsupported`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranslationStatus {
    /// No admitted valid source breaks the goal, and at least one such source exists.
    ProvedUnderAssumptions {
        /// The modelling assumptions the proof depends on.
        assumptions: Vec<String>,
        /// Solver model of an admitted valid source.
        witness: String,
    },
    /// Checked over a finite profile only, never generalized.
    Bounded {
        /// The exact finite profile.
        profile: String,
    },
    /// Outside the fragment, vacuous or unanswered; this remains unknown.
    Unsupported {
        /// The concrete boundary.
        reason: String,
    },
    /// An admitted valid source breaks the goal.
    Refuted {
        /// Solver model of the counterexample.
        counterexample: String,
    },
}
