//! Deterministic, conservative lowering of normative declarations (#3563/#3320).
//! This is not signature verification, admission, a proof or an evaluator.
use norn_ir::deontic::{DeonticModality, DeonticRule, RuleSourceSpan};
use norn_ir::{LawIR, LawModality, LawResponse, LawRoot};

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
    if !valid_source(&rule.source) {
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

fn valid_source(source: &RuleSourceSpan) -> bool {
    !source.artifact_id.trim().is_empty()
        && valid_pointer(&source.pointer)
        && source.bytes.as_ref().is_none_or(|r| r.start <= r.end)
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

/// Stable structured-authoring refusal. Success is structural, not admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementRefusal {
    /// A progress or deadline declaration is outside the closed fragment.
    UnsupportedTiming,
    /// Action and transition occurrence need semantics outside state safety.
    UnsupportedResponse,
    /// The temporal wrapper contradicts the normative grade.
    ModalityMismatch,
    /// A required identity, source span or scope is malformed.
    InvalidBinding,
    /// A predicate is not Boolean in the supplied environment.
    PredicateType,
    /// An English or Arabic reviewed template is missing.
    MissingTemplate,
}

impl RequirementRefusal {
    /// Stable locale-independent diagnostic identity.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedTiming => "law.validation.unsupported_temporal",
            Self::UnsupportedResponse | Self::ModalityMismatch | Self::MissingTemplate => {
                "law.validation.invalid_document"
            }
            Self::InvalidBinding => "law.model.unknown_binding",
            Self::PredicateType => "law.model.predicate_type",
        }
    }
}

/// Check pointwise safety/commit slots in the subject's typed environment.
/// Exceptions/preferences remain declarations for `norm_precedence` grounding;
/// they are never erased or converted into unconditional executable laws.
///
/// # Errors
/// Refuses progress, deadlines, inconsistent grades and non-Boolean predicates.
pub fn check_requirement(
    requirement: &norn_ir::deontic::StructuredRequirement,
    env: &norn_expr::ZenTypeEnv,
) -> Result<(), RequirementRefusal> {
    use RequirementRefusal as R;
    use norn_expr::{Expression, Type, check_expression};
    let rule = &requirement.norm;
    let law = &rule.law;
    let root = match &law.scope.root {
        LawRoot::Company(id) | LawRoot::App(id) | LawRoot::Cell(id) | LawRoot::Entity(id) => id,
    };
    if law.timing.is_some()
        || !matches!(law.modality, LawModality::Always {} | LawModality::Never {})
    {
        return Err(R::UnsupportedTiming);
    }
    if matches!(rule.modality, DeonticModality::Prohibition)
        != matches!(law.modality, LawModality::Never {})
    {
        return Err(R::ModalityMismatch);
    }
    let mut exceptions = std::collections::BTreeSet::new();
    let mut ids = [&law.id, &law.owner, &law.subject, root]
        .into_iter()
        .chain([&rule.authority_id, &rule.authority_version])
        .chain(&law.scope.object_types)
        .chain(&law.scope.commitments)
        .chain(&rule.defeated_by);
    if ids.any(|s| s.trim().is_empty())
        || law.scope.object_types.is_empty()
        || !valid_source(&rule.source)
        || rule.exceptions.iter().any(|e| {
            e.id.trim().is_empty() || !valid_source(&e.source) || !exceptions.insert(&e.id)
        })
        || law.scope.lifecycle_states.iter().any(|s| {
            s.object_type.trim().is_empty()
                || s.property.trim().is_empty()
                || s.states.is_empty()
                || s.states.iter().any(|id| id.trim().is_empty())
        })
    {
        return Err(R::InvalidBinding);
    }
    let w = &requirement.wording;
    if [&w.scope, &w.activation, &w.component, &w.response]
        .iter()
        .any(|w| w.en.trim().is_empty() || w.ar.trim().is_empty())
    {
        return Err(R::MissingTemplate);
    }
    let response = match &law.response {
        LawResponse::Predicate { expression } => Some(expression),
        LawResponse::Invariant {
            object_type,
            invariant_id,
        } => {
            if object_type.trim().is_empty() || invariant_id.trim().is_empty() {
                return Err(R::InvalidBinding);
            }
            None
        }
        LawResponse::Action { .. } | LawResponse::Transition { .. } => {
            return Err(R::UnsupportedResponse);
        }
    };
    for predicate in law
        .condition
        .iter()
        .chain(response)
        .chain(rule.exceptions.iter().map(|e| &e.condition))
    {
        let expression = Expression::compile(&predicate.0).map_err(|_error| R::PredicateType)?;
        check_expression(&expression, env, &Type::Bool).map_err(|_error| R::PredicateType)?;
    }
    Ok(())
}
