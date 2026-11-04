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
    /// A locale map is malformed or incomplete.
    Wording(&'static str),
}

impl RequirementRefusal {
    /// Stable locale-independent diagnostic identity.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedTiming => "law.validation.unsupported_temporal",
            Self::UnsupportedResponse | Self::ModalityMismatch => "law.validation.invalid_document",
            Self::Wording(code) => code,
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
        LawRoot::Pack(_) => return Err(R::InvalidBinding),
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
        || !valid_norm_identity(rule)
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
    requirement.wording.validate().map_err(R::Wording)?;
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

/// Exhaustive finite grounding for one same-state response and scope. Priority
/// links remain explicit edges; this value is not an admission or proof result.
pub struct LoweredNormOrder {
    /// Source-preserving exhaustive applicability and own-exception masks.
    pub norms: Vec<crate::norm_precedence::GroundedNorm>,
    /// Exact original declaration locations in catalog order.
    pub origins: Vec<crate::consistency::RuleOrigin>,
    /// Winner, loser indices, derived solely from `defeated_by` identities.
    pub edges: Vec<(usize, usize)>,
    /// Number of assignments in the explicitly supplied closed schema.
    pub contexts: usize,
}

/// Lower native declarations without erasing their exceptions or priorities.
/// The caller must supply the actual closed schema and then check trusted layer
/// and protection bindings with `diagnose_declared_order`. No implicit authority,
/// date or specificity ordering is introduced. Only a common scope and identical
/// predicate response are supported; other compositions return unknown.
///
/// # Errors
/// Malformed identities, missing defeat targets, unsupported scope/timing and
/// exhausted whole-grounding evaluation budget refuse rather than weaken laws.
pub fn lower_declared_order(
    rules: &[DeonticRule],
    schema: &norn_expr::schema::Schema,
) -> Result<LoweredNormOrder, crate::table_analysis::Unknown> {
    use crate::table_analysis::Unknown;
    use std::collections::{BTreeMap, BTreeSet};
    let invalid = || Unknown::Expression("invalid normative source or binding".into());
    if rules.is_empty() || rules.len() > crate::consistency::diagnosis::MAX_DECLARATIONS {
        return Err(Unknown::Limit);
    }
    let first = rules.first().ok_or_else(invalid)?;
    let LawResponse::Predicate {
        expression: response,
    } = &first.law.response
    else {
        return Err(invalid());
    };
    let mut ids = BTreeMap::new();
    let mut origins = Vec::new();
    let mut edges = BTreeSet::new();
    for (index, rule) in rules.iter().enumerate() {
        if !normative_binding(rule, first) || ids.insert(rule.law.id.as_str(), index).is_some() {
            return Err(invalid());
        }
        origins.push(crate::consistency::RuleOrigin {
            artifact_id: Some(rule.source.artifact_id.clone()),
            pointer: rule.source.pointer.clone(),
        });
    }
    if origins.iter().collect::<BTreeSet<_>>().len() != origins.len() {
        return Err(invalid());
    }
    for (loser, rule) in rules.iter().enumerate() {
        for winner in &rule.defeated_by {
            let high = *ids.get(winner.as_str()).ok_or_else(invalid)?;
            if edges.len() >= 4096 || !edges.insert((high, loser)) {
                return Err(invalid());
            }
        }
    }
    let domain = crate::finite::domain(schema)?;
    let predicates = rules.iter().try_fold(1usize, |count, rule| {
        count
            .checked_add(usize::from(rule.law.condition.is_some()))
            .and_then(|n| n.checked_add(rule.exceptions.len()))
            .ok_or(Unknown::Limit)
    })?;
    if domain.values.is_empty()
        || domain.values.len().saturating_mul(predicates) > crate::table_analysis::MAX_EVALUATIONS
    {
        return Err(Unknown::Limit);
    }
    let contexts = domain.values.len();
    let mut norms: Vec<_> = rules
        .iter()
        .map(|rule| crate::norm_precedence::GroundedNorm {
            id: rule.law.id.clone(),
            modality: rule.modality,
            authority: (rule.authority_id.clone(), rule.authority_version.clone()),
            authority_level: None,
            enacted: 0,
            applicable: BTreeSet::new(),
            excepted: BTreeSet::new(),
        })
        .collect();
    for (context, input) in domain.values.into_iter().enumerate() {
        let runtime: norn_ir::RuntimeValue = serde_json::from_value(input)
            .map_err(|error| Unknown::Expression(error.to_string()))?;
        let value = norn_expr::Value::try_from(&runtime)
            .map_err(|error| Unknown::Expression(error.to_string()))?;
        // Evaluate the response even though it names the common proposition:
        // non-Boolean and unsupported expressions must never acquire meaning.
        normative_boolean(&response.0, &value)?;
        for (rule, norm) in rules.iter().zip(&mut norms) {
            let applicable = rule.law.condition.as_ref().map_or(Ok(true), |condition| {
                normative_boolean(&condition.0, &value)
            })?;
            let mut excepted = false;
            for exception in &rule.exceptions {
                excepted |= normative_boolean(&exception.condition.0, &value)?;
            }
            if applicable {
                norm.applicable.insert(context);
                if excepted {
                    norm.excepted.insert(context);
                }
            }
        }
    }
    Ok(LoweredNormOrder {
        norms,
        origins,
        edges: edges.into_iter().collect(),
        contexts,
    })
}

fn normative_boolean(
    expression: &str,
    context: &norn_expr::Value,
) -> Result<bool, crate::table_analysis::Unknown> {
    match crate::finite::expression(expression, context)? {
        norn_expr::Value::Bool(value) => Ok(value),
        norn_expr::Value::Null
        | norn_expr::Value::Number(_)
        | norn_expr::Value::String(_)
        | norn_expr::Value::Array(_)
        | norn_expr::Value::Object(_) => Err(crate::table_analysis::Unknown::Expression(
            "normative predicate is not Boolean".into(),
        )),
    }
}

fn normative_binding(rule: &DeonticRule, first: &DeonticRule) -> bool {
    use std::collections::BTreeSet;
    let invalid = rule.law.id.len() > 256
        || !valid_norm_identity(rule)
        || rule.authority_id.trim().is_empty()
        || rule.authority_version.trim().is_empty()
        || !valid_source(&rule.source)
        || !rule.law.scope.lifecycle_states.is_empty()
        || !rule.law.scope.commitments.is_empty()
        || rule.law.scope != first.law.scope
        || rule.law.subject != first.law.subject
        || rule.law.response != first.law.response
        || rule.law.timing.is_some()
        || !matches!(
            rule.law.modality,
            LawModality::Always {} | LawModality::Never {}
        )
        || (matches!(rule.modality, DeonticModality::Prohibition)
            != matches!(rule.law.modality, LawModality::Never {}));
    let mut exception_ids = BTreeSet::new();
    !invalid
        && !rule.exceptions.iter().any(|exception| {
            exception.id.trim().is_empty()
                || !exception_ids.insert(&exception.id)
                || !valid_source(&exception.source)
        })
}

fn valid_norm_identity(rule: &DeonticRule) -> bool {
    let law = &rule.law;
    let root = match &law.scope.root {
        LawRoot::Company(id) | LawRoot::App(id) | LawRoot::Cell(id) | LawRoot::Entity(id) => id,
    };
    !law.scope.object_types.is_empty()
        && [&law.id, &law.owner, &law.subject, root]
            .into_iter()
            .chain(&law.scope.object_types)
            .all(|id| !id.trim().is_empty())
}
