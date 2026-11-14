//! Conservative lowering and finite admission of normative declarations.
//! Structural authority bindings grant no permissions or proof status.
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
    /// A LAW-WITHIN-1 trigger, retrigger and cancel policy has no `LawIR` form.
    ActivationTiming,
    /// The rule and its embedded law both declare exceptions; one home only.
    ExceptionHome,
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
/// Existing `LawIR` conditions and deadlines are preserved verbatim. A
/// `StructuredRequirement` must go through [`lower_requirement`] instead, which
/// refuses `within` rather than drop its trigger, retrigger and cancel policy.
/// The result must still pass ordinary `LawIR` admission; no assurance label
/// is issued here.
///
/// # Errors
/// Returns a typed refusal rather than silently weakening unsupported semantics.
pub fn lower_deontic_law(rule: &DeonticRule) -> Result<LoweredDeonticLaw, LawLoweringRefusal> {
    if !valid_source(&rule.source) {
        return Err(LawLoweringRefusal::InvalidSource);
    }
    if !rule.exceptions.is_empty() && !rule.law.exceptions.is_empty() {
        return Err(LawLoweringRefusal::ExceptionHome);
    }
    if !rule.exceptions.is_empty()
        || !rule.law.exceptions.is_empty()
        || !rule.defeated_by.is_empty()
    {
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

/// Lower a structured requirement. A `DeonticRule` carries no trigger,
/// retrigger or cancel policy, so this is the only lowering entry for a
/// `StructuredRequirement`: a `within` requirement refuses rather than lose
/// its activation semantics in plain `LawIR`.
///
/// # Errors
/// [`LawLoweringRefusal::ActivationTiming`] for `within`, else as
/// [`lower_deontic_law`].
pub fn lower_requirement(
    requirement: &norn_ir::deontic::StructuredRequirement,
) -> Result<LoweredDeonticLaw, LawLoweringRefusal> {
    if matches!(
        requirement.timing,
        norn_ir::deontic::RequirementTiming::Within(_)
    ) {
        return Err(LawLoweringRefusal::ActivationTiming);
    }
    lower_deontic_law(&requirement.norm)
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

pub(crate) fn valid_source(source: &RuleSourceSpan) -> bool {
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

/// `LawTiming::Within` text rule: positive canonical u64 decimal (no sign,
/// leading zero or overflow). The one parser for every admission path.
#[must_use]
pub fn canonical_positive_millis(text: &str) -> Option<u64> {
    text.parse::<u64>()
        .ok()
        .filter(|value| *value > 0 && value.to_string() == text)
}

/// Largest admitted `within` window: 2^63 ms. The cell's committed clock is a
/// nonnegative SQLite INTEGER, so `t ≤ 2^63 − 1` and `t + millis ≤ u64::MAX`:
/// opening an activation can never overflow, so no deadline law can refuse a
/// business commit.
pub const MAX_WITHIN_MILLIS: u64 = 1 << 63;

/// Why a structured requirement's timing has no admitted shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingShapeError {
    /// Any temporal pairing other than the safety fragment or LAW-WITHIN-1.
    Unsupported,
    /// The normative grade contradicts its temporal wrapper.
    GradeMismatch,
    /// `law.timing.millis` is not positive canonical u64 decimal text, or it
    /// exceeds [`MAX_WITHIN_MILLIS`].
    InvalidDuration,
}

impl TimingShapeError {
    /// Stable locale-independent diagnostic identity.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "law.validation.unsupported_temporal",
            Self::GradeMismatch | Self::InvalidDuration => "law.validation.invalid_document",
        }
    }
}

/// LAW-WITHIN-1 shape. `within` pairs only with an obligation to eventually
/// respond under `law.timing = within {millis}`, returning the window in
/// milliseconds; every other requirement timing is the untimed always/never
/// fragment whose grade matches its wrapper.
///
/// # Errors
/// Any other temporal pairing, a contradicting grade, or a zero,
/// non-canonical, over-cap or overflowing duration.
pub fn within_shape(
    requirement: &norn_ir::deontic::StructuredRequirement,
) -> Result<Option<(&norn_ir::deontic::WithinTiming, u64)>, TimingShapeError> {
    use TimingShapeError as E;
    use norn_ir::LawTiming as T;
    use norn_ir::deontic::RequirementTiming as RT;
    let law = &requirement.norm.law;
    match (&requirement.timing, &law.modality, &law.timing) {
        (RT::Within(within), LawModality::Eventually {}, Some(T::Within { millis })) => {
            if requirement.norm.modality != DeonticModality::Obligation {
                return Err(E::GradeMismatch);
            }
            // The ordinary LawIR text rule, plus the no-overflow cap.
            let value = canonical_positive_millis(millis)
                .filter(|v| *v <= MAX_WITHIN_MILLIS)
                .ok_or(E::InvalidDuration)?;
            Ok(Some((within, value)))
        }
        (RT::Within(_), _, _)
        | (_, LawModality::Eventually {} | LawModality::Until { .. }, _)
        | (_, _, Some(_)) => Err(E::Unsupported),
        (_, modality, None) => {
            if (requirement.norm.modality == DeonticModality::Prohibition)
                != matches!(modality, LawModality::Never {})
            {
                return Err(E::GradeMismatch);
            }
            Ok(None)
        }
    }
}

/// LAW-WORDING-LOCALE-1 for one requirement: every locale is complete,
/// including the optional terms its root (`pack`) and timing (`within`) render.
///
/// # Errors
/// The wording's own refusal, or `law.wording.incomplete_locale`.
pub fn check_wording(
    requirement: &norn_ir::deontic::StructuredRequirement,
) -> Result<(), &'static str> {
    requirement.wording.validate()?;
    let pack = matches!(requirement.norm.law.scope.root, LawRoot::Pack(_));
    let within = matches!(
        requirement.timing,
        norn_ir::deontic::RequirementTiming::Within(_)
    );
    if requirement
        .wording
        .0
        .values()
        .any(|w| (pack && w.terms.pack.is_none()) || (within && w.terms.within.is_none()))
    {
        return Err("law.wording.incomplete_locale");
    }
    Ok(())
}

/// Checked rendering: the admitted timing shape and LAW-WORDING-LOCALE-1 over
/// every locale, then norn-ir's render-only projection. Refusal codes for
/// pre-existing cases are unchanged.
///
/// # Errors
/// The first timing, wording or rendering refusal.
pub fn render_requirement(
    view: &norn_ir::requirement_display::RequirementView,
) -> Result<Vec<norn_ir::requirement_display::RequirementSlot>, &'static str> {
    within_shape(&view.requirement).map_err(TimingShapeError::code)?;
    let slots = view.slots()?;
    check_wording(&view.requirement)?;
    Ok(slots)
}

/// Stable structured-authoring refusal. Success is structural, not admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementRefusal {
    /// A progress or deadline declaration is outside the closed fragment.
    UnsupportedTiming,
    /// A `within` window is not positive canonical u64 milliseconds.
    InvalidDuration,
    /// A `within` deadline names a business calendar; none is admitted.
    UnsupportedCalendar,
    /// A `within` activation is gated, scoped or defeasible; not yet admitted.
    UnsupportedActivation,
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
    /// The rule and its embedded law both declare exceptions; one home only.
    ExceptionHome,
    /// The declared exception tree is malformed.
    ExceptionTree(crate::law_exceptions::ExceptionTreeRefusal),
    /// An invariant identity cannot be lifted by an exception.
    ExceptionResponse,
}

impl RequirementRefusal {
    /// Stable locale-independent diagnostic identity.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedTiming | Self::UnsupportedCalendar | Self::UnsupportedActivation => {
                "law.validation.unsupported_temporal"
            }
            Self::UnsupportedResponse | Self::ModalityMismatch | Self::InvalidDuration => {
                "law.validation.invalid_document"
            }
            Self::Wording(code) => code,
            Self::InvalidBinding => "law.model.unknown_binding",
            Self::PredicateType => "law.model.predicate_type",
            Self::ExceptionHome => crate::law_exceptions::codes::TWO_HOMES,
            Self::ExceptionTree(refusal) => refusal.code(),
            Self::ExceptionResponse => crate::law_exceptions::codes::UNSUPPORTED_RESPONSE,
        }
    }
}

/// Check pointwise safety/commit slots, or a LAW-WITHIN-1 deadline, in the
/// subject's typed environment. Structural only: runtime law evaluation still
/// refuses timed laws until a cell activation monitor is bound.
/// Exceptions/preferences remain declarations for `norm_precedence` grounding;
/// they are never erased or converted into unconditional executable laws.
///
/// # Errors
/// Refuses unbounded progress, unsupported deadlines, inconsistent grades and
/// non-Boolean predicates.
pub fn check_requirement(
    requirement: &norn_ir::deontic::StructuredRequirement,
    env: &norn_expr::ZenTypeEnv,
) -> Result<(), RequirementRefusal> {
    check(requirement, env, false)
}

/// Why [`check_defeasible_requirement`] refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefeasibleWithinRefusal {
    /// Any refusal of [`check_requirement`]; an unresolved trigger is `InvalidBinding`.
    Requirement(RequirementRefusal),
    /// LIFE-SOUND-9: an exception could cancel the deadline in a state from which no
    /// declared good or compensated ending is proved reachable.
    CancellationDeadEnd(crate::lifecycle_soundness::CancellationDeadEnd),
}

impl DefeasibleWithinRefusal {
    /// Stable locale-independent diagnostic identity.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Requirement(refusal) => refusal.code(),
            Self::CancellationDeadEnd(_) => {
                crate::lifecycle_soundness::codes::CANCELLATION_DEAD_END
            }
        }
    }
}

/// LAW-WITHIN-1 with the LIFE-SOUND-9 relaxation (ruling on #1313, "Reading A gated by a
/// proof"): a `within` obligation whose only extra is an exception tree (no condition, scope
/// filter or defeat edge) is admitted when [`crate::lifecycle_soundness::cancellation_gate`]
/// proves that in every live state where an applying exception could cancel the activation,
/// the subject's `lifecycle` still reaches a declared good or compensated ending. Otherwise it
/// is refused with the failing states and the smallest blocking set. Every other requirement is
/// exactly [`check_requirement`]; without this proof a defeasible `within` stays refused.
///
/// # Errors
/// [`DefeasibleWithinRefusal`].
pub fn check_defeasible_requirement(
    requirement: &norn_ir::deontic::StructuredRequirement,
    env: &norn_expr::ZenTypeEnv,
    lifecycle: &norn_ir::Lifecycle,
    states: &[String],
) -> Result<(), DefeasibleWithinRefusal> {
    use crate::lifecycle_soundness::{CancellationRefusal, cancellation_gate};
    let rule = &requirement.norm;
    let defeasible = !rule.exceptions.is_empty() || !rule.law.exceptions.is_empty();
    let within = within_shape(requirement).ok().flatten();
    let Some((within, _millis)) = within.filter(|_| defeasible) else {
        return check_requirement(requirement, env).map_err(DefeasibleWithinRefusal::Requirement);
    };
    match cancellation_gate(lifecycle, states, &within.trigger) {
        Ok(()) => {}
        Err(CancellationRefusal::UnknownTrigger) => {
            return Err(DefeasibleWithinRefusal::Requirement(
                RequirementRefusal::InvalidBinding,
            ));
        }
        Err(CancellationRefusal::DeadEnd(failure)) => {
            return Err(DefeasibleWithinRefusal::CancellationDeadEnd(failure));
        }
    }
    check(requirement, env, true).map_err(DefeasibleWithinRefusal::Requirement)
}

fn check(
    requirement: &norn_ir::deontic::StructuredRequirement,
    env: &norn_expr::ZenTypeEnv,
    cancellation_proved: bool,
) -> Result<(), RequirementRefusal> {
    use RequirementRefusal as R;
    use norn_expr::{Expression, Type, check_expression};
    let rule = &requirement.norm;
    let law = &rule.law;
    let root = match &law.scope.root {
        LawRoot::Company(id) | LawRoot::App(id) | LawRoot::Cell(id) | LawRoot::Entity(id) => id,
        LawRoot::Pack(_) => return Err(R::InvalidBinding),
    };
    let within = within_shape(requirement).map_err(|error| match error {
        TimingShapeError::Unsupported => R::UnsupportedTiming,
        TimingShapeError::GradeMismatch => R::ModalityMismatch,
        TimingShapeError::InvalidDuration => R::InvalidDuration,
    })?;
    if let Some((within, _millis)) = within {
        if within.calendar.is_some() {
            return Err(R::UnsupportedCalendar);
        }
        // A pointwise condition, lifecycle/commitment scope filter or defeat
        // edge on an activation has no admitted semantics yet; an exception
        // only once LIFE-SOUND-9 proved its cancellations end well.
        if law.condition.is_some()
            || !law.scope.lifecycle_states.is_empty()
            || !law.scope.commitments.is_empty()
            || (!cancellation_proved && (!rule.exceptions.is_empty() || !law.exceptions.is_empty()))
            || !rule.defeated_by.is_empty()
        {
            return Err(R::UnsupportedActivation);
        }
        if !matches!(law.response, LawResponse::Predicate { .. }) {
            return Err(R::UnsupportedResponse);
        }
        if within.trigger.trim().is_empty() || within.accountable.trim().is_empty() {
            return Err(R::InvalidBinding);
        }
    }
    let declared = exception_home(rule).ok_or(R::ExceptionHome)?;
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
        || declared.iter().any(|e| {
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
    crate::law_exceptions::check_tree(&law.id, declared).map_err(R::ExceptionTree)?;
    check_wording(requirement).map_err(R::Wording)?;
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
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            return Err(R::UnsupportedResponse);
        }
    };
    // As in law admission: an invariant identity is unconditional.
    if !declared.is_empty() && matches!(law.response, LawResponse::Invariant { .. }) {
        return Err(R::ExceptionResponse);
    }
    let cancel = within.and_then(|(w, _millis)| w.cancel.as_ref());
    for predicate in law
        .condition
        .iter()
        .chain(response)
        .chain(cancel)
        .chain(declared.iter().map(|e| &e.condition))
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
            .and_then(|n| n.checked_add(exception_home(rule).map_or(0, <[_]>::len)))
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
            // Evaluate every declared condition (a failure anywhere refuses),
            // then fold the tree: siblings OR together, a matching child
            // removes only its own parent.
            let declared = exception_home(rule).ok_or_else(invalid)?;
            let matched = declared
                .iter()
                .map(|exception| normative_boolean(&exception.condition.0, &value))
                .collect::<Result<Vec<_>, _>>()?;
            let (excepted, _applied) =
                crate::law_exceptions::fold(&rule.law.id, declared, &matched);
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
    let Some(declared) = exception_home(rule) else {
        return false;
    };
    !invalid
        && !declared.iter().any(|exception| {
            exception.id.trim().is_empty()
                || !exception_ids.insert(&exception.id)
                || !valid_source(&exception.source)
        })
        && crate::law_exceptions::check_tree(&rule.law.id, declared).is_ok()
}

/// A rule's exceptions live on the rule or on its embedded law, never both.
/// `None` when both lists are nonempty; readers never merge them.
#[must_use]
pub fn exception_home(rule: &DeonticRule) -> Option<&[norn_ir::LawException]> {
    match (rule.exceptions.is_empty(), rule.law.exceptions.is_empty()) {
        (false, false) => None,
        (true, false) => Some(&rule.law.exceptions),
        (_, true) => Some(&rule.exceptions),
    }
}

/// Rationale given to an exception stored in the flat pre-tree shape.
pub const LEGACY_EXCEPTION_RATIONALE: &str =
    "Declared before exception trees; lifts its rule directly.";

/// Upgrade exceptions stored in the flat `norn.normative/1.0.0` shape
/// (`{ id, condition, source }`) to the tree shape. A flat exception lifted its
/// rule directly, so it becomes an exception that overrides the rule's law id:
/// the same meaning. Any other shape is left for the strict decoder.
fn upgrade_legacy_exceptions(slot: &mut serde_json::Value) {
    let Some(rules) = slot.as_array_mut() else {
        return;
    };
    for rule in rules {
        let Some(law_id) = rule
            .pointer("/law/id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(exceptions) = rule
            .get_mut("exceptions")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for exception in exceptions {
            let Some(object) = exception.as_object_mut() else {
                continue;
            };
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            if keys == ["condition", "id", "source"] {
                object.insert("overrides".into(), law_id.clone().into());
                object.insert("rationale".into(), LEGACY_EXCEPTION_RATIONALE.into());
            }
        }
    }
}

/// Decode one normative slot, upgrading flat legacy exceptions first.
fn decode_slot(slot: &serde_json::Value) -> Result<Vec<DeonticRule>, String> {
    let mut value = slot.clone();
    upgrade_legacy_exceptions(&mut value);
    serde_json::from_slice(&norn_canonical::canonical_json_of(&value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Customer-authored declarations in the existing `EntityIR` extension map.
pub const NORMATIVE_SLOT: &str = "norn.normative/1.0.0";
/// One whole-admission query ceiling; unknown always refuses admission.
const ADMISSION_CALLS: usize = 4096;

pub(crate) fn check_declared_order_set(
    artifacts: &[norn_ir::ArtifactEnvelope],
    base: &[norn_ir::ArtifactEnvelope],
) -> Vec<crate::Diagnostic> {
    let mut findings = match admission_order(artifacts, base) {
        Ok(Some(report)) => report.diagnostics,
        Ok(None) => Vec::new(),
        Err(error) => vec![crate::consistency::finding(
            "consistency.diagnosis.unknown",
            crate::DiagnosticSeverity::Error,
            format!(
                "Normative admission refused: {error}. Limits: 64 declarations, 256 contexts, 8192 evaluations, {ADMISSION_CALLS} diagnosis queries."
            ),
            None,
        )],
    };
    if !findings.is_empty() {
        use sha2::Digest;
        let source = norn_canonical::canonical_json_of(&artifacts);
        let installed = norn_canonical::canonical_json_of(&base);
        if let (Ok(source), Ok(installed)) = (source, installed) {
            let source_hash = format!("{:x}", sha2::Sha256::digest(source));
            let base_hash = format!("{:x}", sha2::Sha256::digest(installed));
            let profile = serde_json::json!([
                "norn.normative-admission/1",
                env!("CARGO_PKG_VERSION"),
                crate::receipt::EXPR_PROFILE,
                ADMISSION_CALLS,
                crate::consistency::diagnosis::MAX_DECLARATIONS,
                crate::consistency::finite::FiniteOracle::bounds(),
                "structural-base-protected/candidate-editable",
                source_hash,
                base_hash
            ]);
            let Ok(profile) = norn_canonical::canonical_json_of(&profile) else {
                return findings;
            };
            let pins = format!(
                " Canonical attempted/base SHA256: {source_hash}/{base_hash}; profile SHA256: {:x}. Profile norn.normative-admission/1; closed domains derived from the exact pinned subject definitions.",
                sha2::Sha256::digest(&profile)
            );
            for finding in &mut findings {
                finding.message.push_str(&pins);
            }
        }
    }
    findings
}

fn admission_order(
    artifacts: &[norn_ir::ArtifactEnvelope],
    base: &[norn_ir::ArtifactEnvelope],
) -> Result<Option<crate::norm_precedence::DeclaredOrderReport>, String> {
    use crate::norm_precedence::{ConstitutionLayer, diagnose_candidate_order};
    use std::collections::BTreeSet;
    let mut rules = Vec::new();
    let mut layers = Vec::new();
    let mut protected = BTreeSet::new();
    let mut schema = None;
    for (artifact, inherited) in base
        .iter()
        .map(|a| (a, true))
        .chain(artifacts.iter().map(|a| (a, false)))
    {
        let payload =
            serde_json::to_value(artifact.payload.to_json_value()).map_err(|e| e.to_string())?;
        let Some(extensions) = payload
            .get("extensions")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        if !extensions
            .keys()
            .any(|key| key.starts_with("norn.normative/"))
        {
            continue;
        }
        if extensions
            .keys()
            .any(|key| key.starts_with("norn.normative/") && key != NORMATIVE_SLOT)
        {
            return Err("unsupported normative extension version".into());
        }
        let crate::entity_artifacts::Decoded::Root(root) =
            crate::entity_artifacts::decode_v2(artifact)
        else {
            return Err("normative carrier requires an admitted EntityIR v2 object type".into());
        };
        if !matches!(*root, norn_ir::EntityRoot::V2(_)) {
            return Err("normative carrier kind is unsupported".into());
        }
        let declared = decode_slot(
            extensions
                .get(NORMATIVE_SLOT)
                .ok_or("missing normative declaration slot")?,
        )?;
        if rules.len().saturating_add(declared.len())
            > crate::consistency::diagnosis::MAX_DECLARATIONS
        {
            return Err("declaration limit".into());
        }
        for (index, rule) in declared.into_iter().enumerate() {
            let pointer = format!("/payload/extensions/norn.normative~11.0.0/{index}");
            if rule.source.artifact_id != artifact.artifact_id || rule.source.pointer != pointer {
                return Err(
                    "normative source/subject does not identify its containing artifact".into(),
                );
            }
            if rule.law.scope.object_types != [rule.law.subject.clone()] {
                return Err("normative scope must name exactly its grounded subject".into());
            }
            let derived = admission_schema(&rule.law.subject, artifacts, base, artifact)?;
            if schema.as_ref().is_some_and(|prior| prior != &derived) {
                return Err("different subject domains are unsupported".into());
            }
            schema = Some(derived);
            if inherited {
                protected.insert(rules.len());
            }
            layers.push(if inherited {
                ConstitutionLayer::Legal
            } else {
                ConstitutionLayer::Company
            });
            rules.push(rule);
        }
    }
    if rules.is_empty() {
        return Ok(None);
    }
    if artifacts
        .iter()
        .chain(base)
        .any(|a| a.kind() == norn_ir::ArtifactKind::Law)
    {
        return Err("mixed ordinary LawIR and normative declarations require unsupported joint grounding; no inherited law is ignored".into());
    }
    let lowered = lower_declared_order(&rules, &schema.ok_or("missing closed subject domain")?)
        .map_err(|e| format!("{e:?}"))?;
    diagnose_candidate_order(
        &lowered.norms,
        &lowered.origins,
        &layers,
        &lowered.edges,
        &protected,
        lowered.contexts,
        ADMISSION_CALLS,
    )
    .map(Some)
    .map_err(|e| format!("{e:?}"))
}

fn normative_schema(object: &norn_ir::ObjectTypeDef) -> Result<norn_expr::schema::Schema, String> {
    use norn_expr::schema::Schema;
    use std::collections::{BTreeMap, BTreeSet};
    if object.extends.is_some()
        || !object.implements.is_empty()
        || !object.links.is_empty()
        || !object.invariants.is_empty()
        || !object.lifecycles.is_empty()
    {
        return Err(
            "normative subject requires unsupported inherited/relational constraints".into(),
        );
    }
    let mut properties = BTreeMap::new();
    for property in &object.properties {
        // Enumerating an unconstrained superset is safe for refusal but cannot
        // establish fulfilment. This fragment accepts plain Boolean properties.
        if property.computed.is_some()
            || property.constraints != norn_ir::PropertyConstraints::default()
        {
            return Err("normative property constraints are unsupported".into());
        }
        let norn_ir::PropertyType::Boolean {} = property.property_type else {
            return Err("normative subject domain is not a closed Boolean domain".into());
        };
        properties.insert(property.id.0.clone(), Schema::Boolean);
    }
    let required = properties.keys().cloned().collect();
    let subject = Schema::Object {
        properties,
        required,
    };
    Ok(Schema::Object {
        properties: BTreeMap::from([("self".into(), subject)]),
        required: BTreeSet::from(["self".into()]),
    })
}

fn admission_schema(
    subject: &str,
    artifacts: &[norn_ir::ArtifactEnvelope],
    base: &[norn_ir::ArtifactEnvelope],
    owner: &norn_ir::ArtifactEnvelope,
) -> Result<norn_expr::schema::Schema, String> {
    for artifact in artifacts.iter().chain(base) {
        let crate::entity_artifacts::Decoded::Root(root) =
            crate::entity_artifacts::decode_v2(artifact)
        else {
            continue;
        };
        let norn_ir::EntityRoot::V2(root) = *root else {
            continue;
        };
        if root.object_type.name.0 != subject {
            continue;
        }
        if crate::scope_conflict(&owner.scope, &artifact.scope).is_some()
            || (artifact.artifact_id != owner.artifact_id
                && !owner.dependencies.iter().any(|d| {
                    d.artifact_id == artifact.artifact_id
                        && d.version.as_deref() == Some(artifact.version.as_str())
                        && d.required
                }))
        {
            return Err("normative subject requires a visible, exact pinned dependency".into());
        }
        return normative_schema(&root.object_type);
    }
    Err("normative subject is not installed or admitted".into())
}

fn valid_norm_identity(rule: &DeonticRule) -> bool {
    let law = &rule.law;
    let root = match &law.scope.root {
        LawRoot::Company(id) | LawRoot::App(id) | LawRoot::Cell(id) | LawRoot::Entity(id) => id,
        // Pack-rooted normative scopes are unsupported here, as in binding.
        LawRoot::Pack(_) => return false,
    };
    !law.scope.object_types.is_empty()
        && [&law.id, &law.owner, &law.subject, root]
            .into_iter()
            .chain(&law.scope.object_types)
            .all(|id| !id.trim().is_empty())
}

/// Existing base-to-base resolutions were already admitted. Preserve them;
/// only newly introduced incoming edges participate in the protection audit.
pub(crate) fn installed_protection(
    inherited: &std::collections::BTreeSet<usize>,
    edges: &[(usize, usize)],
) -> std::collections::BTreeSet<usize> {
    inherited
        .iter()
        .copied()
        .filter(|low| {
            let incoming: Vec<_> = edges.iter().filter(|(_, target)| target == low).collect();
            incoming.is_empty() || incoming.iter().any(|(high, _)| !inherited.contains(high))
        })
        .collect()
}
