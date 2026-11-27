//! Honest per-law enforcement and proof classification (`LAW-HONESTY-1`).
//!
//! Mirrors `norn-kernel/src/governing_laws.rs` commit-path rules 1:1 so
//! `norn check`, WASM consumers, and `CheckReceipt` projections never conflate
//! "structurally admitted" with "enforced at commit" or "formally proved".

use norn_ir::{ArtifactEnvelope, ArtifactKind, LawIR, LawModality, LawResponse, LawRoot};
use serde_json::{Map, Value, json};

/// Whether `norn-kernel` enforces this law on every commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LawEnforcementStatus {
    /// Checked on every native commit by `norn-kernel::governing_laws`.
    Enforced,
    /// Structurally admitted (or undecodable), but not enforced on commits yet
    /// (refused with `core.laws.unsupported_semantics` if installed as a governing law).
    DeclaredOnly,
}

impl LawEnforcementStatus {
    /// Wire kebab-case token for JSON projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::DeclaredOnly => "declared-only",
        }
    }
}

/// Honest verification/proof evidence level for one law (`AGENTS.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LawProofStatus {
    /// No static discharge or admission check has been recorded for this law.
    Unknown,
    /// Structurally admitted (well-typed predicate, valid bindings, closure).
    Structural,
    /// Discharged by `obligations::induction::transition_frame`.
    FrameProved,
    /// Discharged by exhaustive finite-state checking (`LIFE-SOUND` / `finite.rs`).
    ExhaustiveFinite,
    /// Discharged by bounded model checking (e.g. Quint / Apalache).
    BoundedChecked,
    /// Discharged by a checked proof (`CONS-LINEAR-1` / Aeneas / Lean).
    CheckedProof,
}

impl LawProofStatus {
    /// Wire kebab-case token for JSON projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Structural => "structural",
            Self::FrameProved => "frame-proved",
            Self::ExhaustiveFinite => "exhaustive-finite",
            Self::BoundedChecked => "bounded-checked",
            Self::CheckedProof => "checked-proof",
        }
    }
}

/// One law's honest enforcement and proof record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LawReceiptEntry {
    /// Artifact envelope id (e.g. `soliman.Payslip.zero_break_and_exact_net.law`).
    pub artifact_id: String,
    /// Stable law identity inside the artifact.
    pub law_id: String,
    /// Runtime commit-path enforcement status.
    pub enforcement: LawEnforcementStatus,
    /// Static proof/evidence level.
    pub proof: LawProofStatus,
    /// Optional content digest of the attached proof/certificate evidence.
    pub evidence_digest: Option<String>,
    /// Human-readable explanation when `DeclaredOnly` or partially discharged.
    pub reason: Option<String>,
}

impl LawReceiptEntry {
    /// Canonical `camelCase` JSON representation for CLI/WASM projections.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("artifactId".to_owned(), json!(self.artifact_id));
        map.insert("lawId".to_owned(), json!(self.law_id));
        map.insert("enforcement".to_owned(), json!(self.enforcement.as_str()));
        map.insert("proof".to_owned(), json!(self.proof.as_str()));
        if let Some(digest) = &self.evidence_digest {
            map.insert("evidenceDigest".to_owned(), json!(digest));
        }
        if let Some(reason) = &self.reason {
            map.insert("reason".to_owned(), json!(reason));
        }
        Value::Object(map)
    }
}

/// Classifies one decoded [`LawIR`] against the exact commit-path support rules
/// of `norn-kernel/src/governing_laws.rs` (both `supported` and single-entity
/// scope gating).
///
/// Leaves [`LawReceiptEntry::proof`] at [`LawProofStatus::Unknown`] because this
/// helper inspects the decoded law shape without running full artifact-set
/// admission; callers that have admitted the set may promote `proof`.
#[must_use]
pub fn classify_law(artifact_id: &str, law: &LawIR) -> LawReceiptEntry {
    let unsupported_reason = declared_only_reason(law);
    let enforcement = if unsupported_reason.is_none() {
        LawEnforcementStatus::Enforced
    } else {
        LawEnforcementStatus::DeclaredOnly
    };
    LawReceiptEntry {
        artifact_id: artifact_id.to_owned(),
        law_id: law.id.clone(),
        enforcement,
        proof: LawProofStatus::Unknown,
        evidence_digest: None,
        reason: unsupported_reason.map(str::to_owned),
    }
}

fn declared_only_reason(law: &LawIR) -> Option<&'static str> {
    if !matches!(&law.scope.root, LawRoot::Entity(name) if *name == law.subject) {
        return Some("only single-entity root laws are enforced on commit");
    }
    if law.scope.object_types != [law.subject.as_str()] {
        return Some("multi-type or non-subject scope.objectTypes are not enforced on commit");
    }
    if !law.exceptions.is_empty() {
        return Some("exception trees have no commit-path fold yet");
    }
    if law.timing.is_some() {
        return Some("timed/deadline laws are not enforced on commit");
    }
    if !law.scope.lifecycle_states.is_empty() {
        return Some("lifecycle-state scoped laws are not enforced on commit");
    }
    if !law.scope.commitments.is_empty() {
        return Some("commitment-scoped laws are not enforced on commit");
    }
    if !matches!(law.modality, LawModality::Always {} | LawModality::Never {}) {
        return Some("temporal modalities (eventually/until) are declared-only");
    }
    match &law.response {
        LawResponse::Predicate { .. } => None,
        LawResponse::Invariant { .. } => {
            if matches!(law.modality, LawModality::Always {}) && law.condition.is_none() {
                None
            } else {
                Some("conditional or non-always invariant responses are declared-only")
            }
        }
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            Some("action, transition, and translation law responses are declared-only")
        }
    }
}

/// Classifies every `Law` artifact envelope in `artifacts` in deterministic
/// order by `(artifact_id, law_id)`. Undecodable law envelopes are surfaced
/// explicitly as `DeclaredOnly` + `Unknown` rather than silently omitted.
#[must_use]
pub fn classify_law_honesty(artifacts: &[ArtifactEnvelope]) -> Vec<LawReceiptEntry> {
    let mut entries = Vec::new();
    for artifact in artifacts {
        if artifact.kind() != ArtifactKind::Law {
            continue;
        }
        let decoded = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value::<LawIR>);
        match decoded {
            Ok(law) => entries.push(classify_law(&artifact.artifact_id, &law)),
            Err(error) => entries.push(LawReceiptEntry {
                artifact_id: artifact.artifact_id.clone(),
                law_id: artifact.name.clone(),
                enforcement: LawEnforcementStatus::DeclaredOnly,
                proof: LawProofStatus::Unknown,
                evidence_digest: None,
                reason: Some(format!("law payload failed to decode: {error}")),
            }),
        }
    }
    entries.sort();
    entries
}
