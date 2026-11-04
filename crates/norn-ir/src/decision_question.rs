//! Decision-question wire records (#932), separate from analytical `QuestionIR`.
//!
//! These are data, never authorization, admission or closure evidence by themselves.
//! The server must reconstruct pins, options and consequences from live admission,
//! authorize transitions through Cedar, and retain them with the declaration change.
use crate::{ArtifactEnvelope, IrVersion01, function::Digest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A versioned decision awaiting an ordinary declaration change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionQuestion {
    pub question_version: IrVersion01,
    pub family: DecisionFamily,
    pub source: DecisionSource,
    pub requirement: QuestionRequirement,
    pub options: Vec<DecisionOption>,
}

impl DecisionQuestion {
    /// Identity of the undecided obligation, independent of proposals and actors.
    /// The source obligation must already bind exact source/base/profile bytes,
    /// obligation identity and source locations. This helper verifies only digest
    /// syntax; it neither reconstructs nor authenticates that obligation.
    #[must_use]
    pub fn id(&self) -> Option<Digest> {
        if !self.source.obligation.is_well_formed() {
            return None;
        }
        norn_canonical::domain_hash_hex(
            b"norn.decision-question/1\0",
            &(self.family, &self.source.obligation),
        )
        .ok()
        .map(|hex| Digest::from_hex(&hex))
    }
}

/// Closed first family; new families require a contract revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionFamily {
    RequiredPathBinding,
}

/// Exact authority inputs retained even when ordinary admission refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionSource {
    pub obligation: Digest,
    pub candidate: Digest,
    pub base: Digest,
    pub profile: Digest,
    /// Exact artifact identity and JSON pointer of the refused declaration.
    pub artifact_id: String,
    pub pointer: String,
}

/// Chosen by the generating admission obligation, never by the answerer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionRequirement {
    Required,
    Informational,
}

/// One computed repair and the result of ordinary admission after applying it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionOption {
    pub id: String,
    pub changes: Vec<DeclarationReplacement>,
    pub consequence: AdmissionConsequence,
}

/// Replace one exact version with an ordinary declared artifact; not a patch DSL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclarationReplacement {
    pub previous: Digest,
    pub replacement: ArtifactEnvelope,
}

/// A preview is an actual admission result, never a predicted prose consequence.
/// Decoding it does not authenticate its receipt or diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdmissionConsequence {
    Admitted {
        candidate: Digest,
        receipt: Digest,
    },
    Refused {
        candidate: Digest,
        /// Digest of the complete, source-located admission diagnostics.
        diagnostics: Digest,
    },
}

/// Versioned, attributed proposal. Identity comes from the authenticated server
/// session; a client-supplied actor string is never a trusted principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionProposal {
    pub question_id: Digest,
    /// Exact question body, including its options and consequences.
    pub question_content: Digest,
    pub revision: u64,
    pub option_id: String,
    pub actor: String,
    /// Exact candidate after applying this option to the retained source.
    pub candidate: Digest,
}

/// Approval binds the complete proposal, not merely the selected option id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionApproval {
    pub proposal: DecisionProposal,
    pub actor: String,
}

/// Server-retained lifecycle record; deserialization cannot perform a transition.
/// Required questions block until fresh re-admission closes them. Withdrawal
/// invalidates a question but does not discharge its source obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionQuestionState {
    Open,
    Proposed {
        proposal: DecisionProposal,
    },
    Approved {
        approval: DecisionApproval,
    },
    Closed {
        approval: DecisionApproval,
        /// Fresh admission receipt for the approved candidate, not its preview.
        receipt: Digest,
        /// Closure evidence binds question id/content, proposal and fresh receipt.
        closure: Digest,
    },
    Withdrawn {
        actor: String,
        reason: String,
    },
}
