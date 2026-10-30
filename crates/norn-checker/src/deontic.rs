//! Deterministic, conservative lowering of normative declarations (#3563/#3320).
//! This is not signature verification, admission, a proof or an evaluator.
use norn_ir::deontic::{DeonticModality, DeonticRule, RuleSourceSpan};
use norn_ir::{LawIR, LawModality};

/// Why a declaration cannot be represented faithfully as an ordinary law.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LawLoweringRefusal {
    /// Permissions do not assert that an action must occur.
    Permission,
    /// Defeasible applicability requires the Datalog profile, not Boolean erasure.
    Defeasible,
    /// The law template contradicts its normative grade.
    ModalityMismatch,
    /// Temporal constraints on prohibition have no admitted interpretation here.
    ProhibitionTiming,
    /// Invalid or missing source identity/range.
    InvalidSource,
    /// L1 v1 supports only untimed always/never, never progress claims.
    UnsupportedProofFragment,
}

/// Exact law plus the source link consumed by a later proof bridge.
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredDeonticLaw {
    /// Exact ordinary law; still requires `LawIR` admission.
    pub law: LawIR,
    /// Original structured declaration location.
    pub source: RuleSourceSpan,
    /// Declared source identity, not verified authority.
    pub authority_id: String,
    /// Exact declared source version.
    pub authority_version: String,
}

/// Lower only unconditional-in-the-defeasibility-sense obligations/prohibitions.
/// Existing `LawIR` conditions and deadlines are preserved verbatim. The result
/// must still pass ordinary `LawIR` admission; no assurance label is issued here.
///
/// # Errors
/// Returns a typed refusal rather than silently weakening unsupported semantics.
pub fn lower_deontic_law(rule: &DeonticRule) -> Result<LoweredDeonticLaw, LawLoweringRefusal> {
    if rule.source.artifact_id.trim().is_empty()
        || !valid_pointer(&rule.source.pointer)
        || rule
            .source
            .bytes
            .as_ref()
            .is_some_and(|range| range.start > range.end)
    {
        return Err(LawLoweringRefusal::InvalidSource);
    }
    if !rule.exceptions.is_empty() || !rule.defeated_by.is_empty() {
        return Err(LawLoweringRefusal::Defeasible);
    }
    match rule.modality {
        DeonticModality::Permission => return Err(LawLoweringRefusal::Permission),
        DeonticModality::Obligation => {
            if !matches!(
                rule.law.modality,
                LawModality::Always {} | LawModality::Eventually {}
            ) {
                return Err(LawLoweringRefusal::ModalityMismatch);
            }
        }
        DeonticModality::Prohibition => {
            if !matches!(rule.law.modality, LawModality::Never {}) {
                return Err(LawLoweringRefusal::ModalityMismatch);
            }
            if rule.law.timing.is_some() {
                return Err(LawLoweringRefusal::ProhibitionTiming);
            }
        }
    }
    Ok(LoweredDeonticLaw {
        law: rule.law.clone(),
        source: rule.source.clone(),
        authority_id: rule.authority_id.clone(),
        authority_version: rule.authority_version.clone(),
    })
}

/// Select a candidate for L1's `norn.theorem-input/0.1` closed fragment.
/// This does not produce a theorem input: L1 must still bind the hashed state
/// model, validate/typecheck ZEN, bind sources and authorities, and hash `LawIR`.
/// An accepted candidate carries no proof status or evidence label.
///
/// # Errors
/// Refuses all temporal progress/timing and unsupported normative constructs.
pub fn lower_theorem_candidate(
    rule: &DeonticRule,
) -> Result<LoweredDeonticLaw, LawLoweringRefusal> {
    let candidate = lower_deontic_law(rule)?;
    if candidate.law.timing.is_some()
        || !matches!(
            candidate.law.modality,
            LawModality::Always {} | LawModality::Never {}
        )
    {
        return Err(LawLoweringRefusal::UnsupportedProofFragment);
    }
    Ok(candidate)
}

fn valid_pointer(pointer: &str) -> bool {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}
