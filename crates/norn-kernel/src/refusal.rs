//! The runtime refusal taxonomy (T1-L4, #2420).
//!
//! Every kernel refusal has:
//! - a stable `code` (an API contract; the existing `code()` strings, never
//!   renamed);
//! - a `category()` ([`RefusalCategory`]) that a shell maps to a transport
//!   status (HTTP, MCP tool error);
//! - a safe form, [`Refusable::refusal`]: `{code, category, message}` with a
//!   message a caller may see.
//!
//! The categories follow the Operational Ontology's three-way split of
//! visibility / permission / precondition, plus conflict, invalid, limit,
//! and internal.
//!
//! # Existence never leaks
//!
//! - An invisible object reads exactly like an absent one: the
//!   [`RefusalCategory::Visibility`] category with the fixed message
//!   [`NOT_FOUND_MESSAGE`].
//! - An action call on an absent or invisible target is
//!   `action.target_not_found`, whatever the authorizer says about an absent
//!   target ([`crate::action_commit`]).
//! - A permission refusal is reported only for a target the actor can see.
//! - [`RefusalCategory::Internal`] refusals carry the fixed message
//!   [`INTERNAL_MESSAGE`], never a host error or engine diagnostic.
//!
//! Traversal from a hidden origin (an empty result) is a read-path rule.
//! There is no kernel traversal API yet, so it is not covered here.

use std::{collections::BTreeMap, fmt};

use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};

use crate::action::{ActionCommandRejection, ActionRejection, AuthorizedRejection};
use crate::action_commit::ActionRefusal;
use crate::cell_host::{CommitError, FactKey};
use crate::decider::StepError;
use crate::decision_commit::DecisionRefusal;
use crate::limits::IngressLimitExceeded;

/// The category of a refusal: what the caller can do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCategory {
    /// The target does not exist for this actor (absent or invisible; the two
    /// are indistinguishable by design).
    Visibility,
    /// The actor may see the target but may not do this.
    Permission,
    /// The request is well-formed, but the current state does not allow it
    /// (a lifecycle `from` state, a guard, an action precondition).
    Precondition,
    /// State changed concurrently, or a retry does not match the original;
    /// re-read and decide again.
    Conflict,
    /// The request or decision output is malformed or names something the
    /// definitions do not declare.
    Invalid,
    /// A resource limit of the limit profile was exceeded.
    Limit,
    /// The host failed; nothing was committed. Never carries details.
    Internal,
}

impl RefusalCategory {
    /// The wire name of the category.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Visibility => "visibility",
            Self::Permission => "permission",
            Self::Precondition => "precondition",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
            Self::Limit => "limit",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for RefusalCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The fixed message of every visibility refusal.
pub const NOT_FOUND_MESSAGE: &str = "not found";
/// The fixed message of every internal refusal.
pub const INTERNAL_MESSAGE: &str = "the cell could not complete the request; nothing was committed";

/// The safe, stable form of one refusal: `{code, category, message}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    /// Stable machine-readable code.
    pub code: String,
    /// What the caller can do about it.
    pub category: RefusalCategory,
    /// A message safe to show the caller.
    pub message: String,
    /// Definition-derived law and operands, when the refusal identifies them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<RefusalReason>,
    /// Only engine-defined stale preparation/concurrency paths permit retry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retryable: bool,
}

pub use norn_ir::RefusalReason;

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if passes a field reference"
)]
fn is_false(value: &bool) -> bool {
    !value
}

impl Refusal {
    /// A refusal with `message`; visibility and internal refusals always get
    /// their fixed message instead.
    #[must_use]
    pub fn new(code: &str, category: RefusalCategory, message: impl fmt::Display) -> Self {
        let message = match category {
            RefusalCategory::Visibility => NOT_FOUND_MESSAGE.to_owned(),
            RefusalCategory::Internal => INTERNAL_MESSAGE.to_owned(),
            RefusalCategory::Permission
            | RefusalCategory::Precondition
            | RefusalCategory::Conflict
            | RefusalCategory::Invalid
            | RefusalCategory::Limit => message.to_string(),
        };
        Self {
            code: code.to_owned(),
            category,
            message,
            reason: None,
            retryable: false,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.category, self.message)
    }
}

/// A kernel refusal type that maps into the taxonomy.
pub trait Refusable {
    /// The refusal's safe, stable form.
    fn refusal(&self) -> Refusal;
}

const fn law_category(reason: &crate::governing_laws::LawEvaluationError) -> RefusalCategory {
    use crate::governing_laws::LawEvaluationError as Law;
    match reason {
        Law::Violated(_) => RefusalCategory::Precondition,
        Law::BudgetExceeded => RefusalCategory::Limit,
        Law::UnsupportedUntilB2(_) | Law::Unsupported(_) | Law::Invalid(_) => {
            RefusalCategory::Invalid
        }
    }
}

impl<X> CommitError<X> {
    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::Law { reason, .. } => law_category(reason),
            Self::AtomicBoundary { .. }
            | Self::AtomicDeliveryBoundary { .. }
            | Self::DuplicateKey { .. }
            | Self::DuplicateLink { .. }
            | Self::LinkTarget { .. }
            | Self::InvariantModel { .. }
            | Self::EffectiveValues { .. }
            | Self::OwnedRowsProtected { .. }
            | Self::ObservationAuthorization
            | Self::UnsupportedLinkRead { .. }
            | Self::DuplicateLinkRead { .. }
            | Self::InvalidLinkRead { .. }
            | Self::UnsupportedPredicateRead { .. }
            | Self::InvalidPredicateRead { .. } => RefusalCategory::Invalid,
            Self::NotPending { .. }
            | Self::Conflict { .. }
            | Self::LinkConflict { .. }
            | Self::LinkReadConflict { .. }
            | Self::PredicateReadConflict { .. } => RefusalCategory::Conflict,
            Self::IdentityReuse
            | Self::IdentityCollision { .. }
            | Self::ImmutableObject { .. }
            | Self::DanglingLink { .. }
            | Self::LinkCardinality { .. }
            | Self::LinkRequired { .. }
            | Self::TypeMismatch { .. }
            | Self::InvariantViolation { .. } => RefusalCategory::Precondition,
            Self::AccountingPosting { reason, .. } => match reason {
                norn_ir::posting::AccountingError::PostedJournalImmutable
                | norn_ir::posting::AccountingError::JournalDeleteForbidden => {
                    RefusalCategory::Precondition
                }
                norn_ir::posting::AccountingError::ArithmeticOverflow => RefusalCategory::Limit,
                norn_ir::posting::AccountingError::InvalidState
                | norn_ir::posting::AccountingError::UnsupportedIrVersion
                | norn_ir::posting::AccountingError::InvalidIdentity
                | norn_ir::posting::AccountingError::ScopeMismatch
                | norn_ir::posting::AccountingError::BookMismatch
                | norn_ir::posting::AccountingError::GraphMismatch
                | norn_ir::posting::AccountingError::GraphVersionMismatch
                | norn_ir::posting::AccountingError::CalendarMismatch
                | norn_ir::posting::AccountingError::CalendarVersionMismatch
                | norn_ir::posting::AccountingError::StaleOwnerVersion
                | norn_ir::posting::AccountingError::OwnerVersionOverflow
                | norn_ir::posting::AccountingError::InvalidDate
                | norn_ir::posting::AccountingError::PeriodNotFound
                | norn_ir::posting::AccountingError::PeriodClosed
                | norn_ir::posting::AccountingError::PeriodExceptionMissing
                | norn_ir::posting::AccountingError::PeriodExceptionScopeMismatch
                | norn_ir::posting::AccountingError::PeriodExceptionDateMismatch
                | norn_ir::posting::AccountingError::PeriodExceptionVersionMismatch
                | norn_ir::posting::AccountingError::PeriodExceptionExpired
                | norn_ir::posting::AccountingError::PeriodExceptionConsumed
                | norn_ir::posting::AccountingError::DuplicateUnit
                | norn_ir::posting::AccountingError::InvalidOntologyBinding
                | norn_ir::posting::AccountingError::DuplicateLayer
                | norn_ir::posting::AccountingError::DuplicateAccount
                | norn_ir::posting::AccountingError::UnknownParentAccount
                | norn_ir::posting::AccountingError::AccountGraphCycle
                | norn_ir::posting::AccountingError::UnknownAccount
                | norn_ir::posting::AccountingError::AccountReferenceMismatch
                | norn_ir::posting::AccountingError::UnknownUnit
                | norn_ir::posting::AccountingError::AccountUnitMismatch
                | norn_ir::posting::AccountingError::UnitPrecisionMismatch
                | norn_ir::posting::AccountingError::UnknownLayer
                | norn_ir::posting::AccountingError::DuplicateLeg
                | norn_ir::posting::AccountingError::TooFewLegs
                | norn_ir::posting::AccountingError::InvalidLegRecord
                | norn_ir::posting::AccountingError::LegIdentityMismatch
                | norn_ir::posting::AccountingError::InvalidAmount
                | norn_ir::posting::AccountingError::AmountNotReversible
                | norn_ir::posting::AccountingError::JournalUnbalanced
                | norn_ir::posting::AccountingError::JournalUnbalancedByUnit
                | norn_ir::posting::AccountingError::JournalUnbalancedByLayer
                | norn_ir::posting::AccountingError::JournalAlreadyExists
                | norn_ir::posting::AccountingError::JournalNotFound
                | norn_ir::posting::AccountingError::JournalVersionMismatch
                | norn_ir::posting::AccountingError::ReversalNotExact
                | norn_ir::posting::AccountingError::JournalAlreadyReversed
                | norn_ir::posting::AccountingError::CannotReverseReversal
                | norn_ir::posting::AccountingError::IdempotencyConflict
                | norn_ir::posting::AccountingError::QuantityCheckerRejected
                | norn_ir::posting::AccountingError::SerializationFailed => {
                    RefusalCategory::Invalid
                }
            },
            Self::LinkReadTooLarge { .. }
            | Self::TraversalLimitExceeded { .. }
            | Self::InvariantBudgetExceeded { .. }
            | Self::EventLimitExceeded { .. }
            | Self::IdentifierTooLong { .. }
            | Self::Overflow => RefusalCategory::Limit,
            Self::Host(_) => RefusalCategory::Internal,
        }
    }
}

impl<X> Refusable for CommitError<X> {
    fn refusal(&self) -> Refusal {
        let mut refusal = Refusal::new(self.code(), self.category(), self);
        if let Self::InvariantViolation {
            key,
            owner,
            version,
            id,
            outcome,
            ..
        } = self
        {
            refusal.reason = Some(invariant_reason(key, owner, version, id, outcome));
        }
        refusal.retryable = matches!(
            self,
            Self::Conflict { .. }
                | Self::LinkConflict { .. }
                | Self::LinkReadConflict { .. }
                | Self::PredicateReadConflict { .. }
        );
        refusal
    }
}

impl<X> DecisionRefusal<X> {
    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::Law { reason, .. } => law_category(reason),
            Self::IdentityReuse
            | Self::IdentityCollision { .. }
            | Self::GuardFalse { .. }
            | Self::GuardError { .. }
            | Self::NotFromState { .. }
            | Self::ObjectMissing { .. }
            | Self::ObjectExists { .. }
            | Self::ImmutableStateWrite { .. }
            | Self::SourceOwnedTarget { .. }
            | Self::InvariantViolation { .. } => RefusalCategory::Precondition,
            Self::Stale { .. } | Self::NotPending { .. } => RefusalCategory::Conflict,
            Self::AccountingPosting { reason, .. } => match reason {
                norn_ir::posting::AccountingError::PostedJournalImmutable
                | norn_ir::posting::AccountingError::JournalDeleteForbidden => {
                    RefusalCategory::Precondition
                }
                norn_ir::posting::AccountingError::ArithmeticOverflow => RefusalCategory::Limit,
                norn_ir::posting::AccountingError::InvalidState
                | norn_ir::posting::AccountingError::UnsupportedIrVersion
                | norn_ir::posting::AccountingError::InvalidIdentity
                | norn_ir::posting::AccountingError::ScopeMismatch
                | norn_ir::posting::AccountingError::BookMismatch
                | norn_ir::posting::AccountingError::GraphMismatch
                | norn_ir::posting::AccountingError::GraphVersionMismatch
                | norn_ir::posting::AccountingError::CalendarMismatch
                | norn_ir::posting::AccountingError::CalendarVersionMismatch
                | norn_ir::posting::AccountingError::StaleOwnerVersion
                | norn_ir::posting::AccountingError::OwnerVersionOverflow
                | norn_ir::posting::AccountingError::InvalidDate
                | norn_ir::posting::AccountingError::PeriodNotFound
                | norn_ir::posting::AccountingError::PeriodClosed
                | norn_ir::posting::AccountingError::PeriodExceptionMissing
                | norn_ir::posting::AccountingError::PeriodExceptionScopeMismatch
                | norn_ir::posting::AccountingError::PeriodExceptionDateMismatch
                | norn_ir::posting::AccountingError::PeriodExceptionVersionMismatch
                | norn_ir::posting::AccountingError::PeriodExceptionExpired
                | norn_ir::posting::AccountingError::PeriodExceptionConsumed
                | norn_ir::posting::AccountingError::DuplicateUnit
                | norn_ir::posting::AccountingError::InvalidOntologyBinding
                | norn_ir::posting::AccountingError::DuplicateLayer
                | norn_ir::posting::AccountingError::DuplicateAccount
                | norn_ir::posting::AccountingError::UnknownParentAccount
                | norn_ir::posting::AccountingError::AccountGraphCycle
                | norn_ir::posting::AccountingError::UnknownAccount
                | norn_ir::posting::AccountingError::AccountReferenceMismatch
                | norn_ir::posting::AccountingError::UnknownUnit
                | norn_ir::posting::AccountingError::AccountUnitMismatch
                | norn_ir::posting::AccountingError::UnitPrecisionMismatch
                | norn_ir::posting::AccountingError::UnknownLayer
                | norn_ir::posting::AccountingError::DuplicateLeg
                | norn_ir::posting::AccountingError::TooFewLegs
                | norn_ir::posting::AccountingError::InvalidLegRecord
                | norn_ir::posting::AccountingError::LegIdentityMismatch
                | norn_ir::posting::AccountingError::InvalidAmount
                | norn_ir::posting::AccountingError::AmountNotReversible
                | norn_ir::posting::AccountingError::JournalUnbalanced
                | norn_ir::posting::AccountingError::JournalUnbalancedByUnit
                | norn_ir::posting::AccountingError::JournalUnbalancedByLayer
                | norn_ir::posting::AccountingError::JournalAlreadyExists
                | norn_ir::posting::AccountingError::JournalNotFound
                | norn_ir::posting::AccountingError::JournalVersionMismatch
                | norn_ir::posting::AccountingError::ReversalNotExact
                | norn_ir::posting::AccountingError::JournalAlreadyReversed
                | norn_ir::posting::AccountingError::CannotReverseReversal
                | norn_ir::posting::AccountingError::IdempotencyConflict
                | norn_ir::posting::AccountingError::QuantityCheckerRejected
                | norn_ir::posting::AccountingError::SerializationFailed => {
                    RefusalCategory::Invalid
                }
            },
            Self::InvariantBudgetExceeded { .. }
            | Self::TraversalLimitExceeded { .. }
            | Self::IdentifierTooLong { .. }
            | Self::Overflow
            | Self::EnvelopeExceeded { .. }
            | Self::TurnEnvelopeExceeded { .. } => RefusalCategory::Limit,
            Self::Host(_) => RefusalCategory::Internal,
            Self::AtomicBoundary { .. }
            | Self::AtomicDeliveryBoundary { .. }
            | Self::ReadOnlyWorld
            | Self::PerformOutsideMain { .. }
            | Self::UnknownType { .. }
            | Self::NotInSlice { .. }
            | Self::LifecyclePropertySet { .. }
            | Self::InvalidInitialState { .. }
            | Self::UnknownLifecycle { .. }
            | Self::UnresolvedLifecycle { .. }
            | Self::UnknownTransition { .. }
            | Self::InvalidOutput { .. } => RefusalCategory::Invalid,
        }
    }
}

/// DIAG-REASON-1: final-value refusals retain the exact evaluated declaration.
fn invariant_reason(
    key: &crate::FactKey,
    owner: &norn_ir::InvariantOwner,
    version: &str,
    id: &norn_ir::InvariantId,
    outcome: &norn_ir::DataQualityOutcome,
) -> RefusalReason {
    let outcome = match outcome {
        norn_ir::DataQualityOutcome::NotSatisfied => "not_satisfied",
        norn_ir::DataQualityOutcome::NonBoolean { .. } => "non_boolean",
        norn_ir::DataQualityOutcome::EvaluationFailed { .. } => "evaluation_failed",
        // Non-runtime outcomes carry no unproven observed values into a refusal.
        norn_ir::DataQualityOutcome::ObservedOutOfBand { .. }
        | norn_ir::DataQualityOutcome::InvalidTemporalObservation { .. }
        | norn_ir::DataQualityOutcome::IdentityProjectionIsolation { .. } => "unsupported_outcome",
    };
    RefusalReason {
        law: id.as_str().to_owned(),
        subject: key.to_string(),
        values: refusal_values([("outcome", outcome.into())]),
        pin: Some(norn_ir::DataQualityRuleRef::Invariant {
            owner: owner.clone(),
            invariant_id: id.clone(),
            version: version.to_owned(),
        }),
    }
}

fn builtin_reason(
    law: &str,
    key: &crate::FactKey,
    values: BTreeMap<String, RuntimeValue>,
) -> RefusalReason {
    RefusalReason {
        law: law.to_owned(),
        subject: key.to_string(),
        values,
        pin: Some(norn_ir::DataQualityRuleRef::Builtin {
            id: law.to_owned(),
            version: "1".to_owned(),
        }),
    }
}

fn refusal_values<const N: usize>(
    values: [(&str, RuntimeValue); N],
) -> BTreeMap<String, RuntimeValue> {
    values
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

fn lifecycle_states(
    lifecycle: &str,
    current: Option<&str>,
    allowed: &[String],
) -> BTreeMap<String, RuntimeValue> {
    refusal_values([
        ("lifecycle", lifecycle.into()),
        (
            "current",
            current.map_or(RuntimeValue::Null, RuntimeValue::from),
        ),
        (
            "allowed",
            RuntimeValue::Array(allowed.iter().cloned().map(RuntimeValue::String).collect()),
        ),
    ])
}

fn lifecycle_guard(
    transition: &str,
    guard: &str,
    error: Option<&str>,
    underlying_code: Option<&str>,
) -> BTreeMap<String, RuntimeValue> {
    let mut values = refusal_values([("transition", transition.into()), ("guard", guard.into())]);
    let (name, value) = error.map_or(("result", RuntimeValue::Bool(false)), |error| {
        ("error", error.into())
    });
    values.insert(name.to_owned(), value);
    if let Some(code) = underlying_code {
        values.insert("underlyingCode".to_owned(), code.into());
    }
    values
}

impl<X> DecisionRefusal<X> {
    fn lifecycle_reason_parts(&self) -> Option<(&str, &FactKey, BTreeMap<String, RuntimeValue>)> {
        match self {
            Self::NotFromState {
                lifecycle,
                key,
                transition,
                current,
                from,
            } => Some(("lifecycle.transition_source", key.as_ref(), {
                let mut values = lifecycle_states(lifecycle, current.as_deref(), from);
                values.insert("transition".to_owned(), transition.clone().into());
                values
            })),
            Self::InvalidInitialState {
                key,
                lifecycle,
                state,
                initial,
            } => Some((
                "lifecycle.initial_state",
                key,
                lifecycle_states(lifecycle, state.as_deref(), initial),
            )),
            Self::ImmutableStateWrite {
                key,
                lifecycle,
                state,
            } => Some((
                "lifecycle.immutable_state",
                key,
                refusal_values([
                    ("lifecycle", RuntimeValue::String(lifecycle.clone())),
                    ("current", RuntimeValue::String(state.clone())),
                ]),
            )),
            Self::LifecyclePropertySet {
                key,
                property: lifecycle,
            }
            | Self::UnknownLifecycle { key, lifecycle }
            | Self::UnresolvedLifecycle { key, lifecycle } => Some((
                self.code(),
                key,
                refusal_values([("lifecycle", RuntimeValue::String(lifecycle.clone()))]),
            )),
            Self::UnknownTransition {
                key,
                lifecycle,
                transition,
            } => Some((
                "lifecycle.declared_transition",
                key,
                refusal_values([
                    ("lifecycle", RuntimeValue::String(lifecycle.clone())),
                    ("transition", RuntimeValue::String(transition.clone())),
                ]),
            )),
            Self::GuardFalse {
                key,
                transition,
                guard,
                diagnostic,
            } => Some((
                "lifecycle.guard",
                key,
                lifecycle_guard(
                    transition,
                    guard,
                    None,
                    diagnostic.map(|_| "decision.guard_false"),
                ),
            )),
            Self::GuardError {
                key,
                transition,
                guard,
                reason,
                diagnostic,
            } => Some((
                "lifecycle.guard_evaluable",
                key.as_ref(),
                lifecycle_guard(
                    transition,
                    guard,
                    Some(reason),
                    diagnostic.map(|_| "decision.guard_error"),
                ),
            )),
            Self::IdentityReuse
            | Self::IdentityCollision { .. }
            | Self::AtomicBoundary { .. }
            | Self::AtomicDeliveryBoundary { .. }
            | Self::ReadOnlyWorld
            | Self::PerformOutsideMain { .. }
            | Self::EnvelopeExceeded { .. }
            | Self::IdentifierTooLong { .. }
            | Self::TurnEnvelopeExceeded { .. }
            | Self::UnknownType { .. }
            | Self::NotInSlice { .. }
            | Self::SourceOwnedTarget { .. }
            | Self::ObjectMissing { .. }
            | Self::ObjectExists { .. }
            | Self::InvalidOutput { .. }
            | Self::Stale { .. }
            | Self::NotPending { .. }
            | Self::AccountingPosting { .. }
            | Self::Law { .. }
            | Self::InvariantViolation { .. }
            | Self::InvariantBudgetExceeded { .. }
            | Self::TraversalLimitExceeded { .. }
            | Self::Overflow
            | Self::Host(_) => None,
        }
    }

    fn lifecycle_reason(&self) -> Option<RefusalReason> {
        self.lifecycle_reason_parts()
            .map(|(law, key, values)| builtin_reason(law, key, values))
    }
}

impl<X> Refusable for DecisionRefusal<X> {
    fn refusal(&self) -> Refusal {
        let mut refusal = Refusal::new(self.code(), self.category(), self);
        refusal.reason = if let Self::InvariantViolation {
            key,
            owner,
            version,
            id,
            outcome,
            ..
        } = self
        {
            Some(invariant_reason(key, owner, version, id, outcome))
        } else {
            self.lifecycle_reason()
        };
        refusal.retryable = matches!(self, Self::Stale { .. });
        refusal
    }
}

impl<X> ActionRefusal<X> {
    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::Decision(refusal) => refusal.category(),
            Self::TargetNotFound | Self::SliceObjectMissing { .. } => RefusalCategory::Visibility,
            Self::PermissionDenied { .. } => RefusalCategory::Permission,
            Self::PreconditionFailed { .. } | Self::WriteModeFailed { .. } => {
                RefusalCategory::Precondition
            }
            Self::IdempotencyConflict { .. } | Self::StalePreconditionRead { .. } => {
                RefusalCategory::Conflict
            }
            Self::ReceiptRefused { reason } => reason.category(),
            Self::InputBoundExceeded { .. } | Self::ExpressionBudgetExceeded { .. } => {
                RefusalCategory::Limit
            }
            Self::Command(rejection) => rejection.category(),
            Self::MissingScope { .. }
            | Self::UnsupportedEffect { .. }
            | Self::UncheckedWriter { .. }
            | Self::InvalidInput { .. }
            | Self::DerivationFailed { .. }
            | Self::SliceUnavailable { .. }
            | Self::KindMismatch { .. } => RefusalCategory::Invalid,
        }
    }
}

impl<X> Refusable for ActionRefusal<X> {
    fn refusal(&self) -> Refusal {
        if let Self::Decision(refusal) = self {
            return refusal.refusal();
        }
        let mut refusal = Refusal::new(self.code(), self.category(), self);
        refusal.retryable = matches!(self, Self::StalePreconditionRead { .. });
        refusal
    }
}

impl ActionCommandRejection {
    /// Stable machine-readable code (the same as the action path's).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingOrgScope => "action.missing_scope",
            Self::BlankField => "action.blank_field",
            Self::ActionIdTooLong { .. } => "action.id_too_long",
            Self::IdempotencyKeyTooLong { .. } => "action.idempotency_key_too_long",
        }
    }

    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::MissingOrgScope | Self::BlankField => RefusalCategory::Invalid,
            Self::ActionIdTooLong { .. } | Self::IdempotencyKeyTooLong { .. } => {
                RefusalCategory::Limit
            }
        }
    }
}

impl Refusable for ActionCommandRejection {
    fn refusal(&self) -> Refusal {
        Refusal::new(self.code(), self.category(), self)
    }
}

impl ActionRejection {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidTransitionIdentity => "action.invalid_transition_identity",
            Self::TransitionUsedByOtherProtocol { .. } => "action.transition_protocol_conflict",
            Self::TransitionBindingMismatch { .. } => "action.transition_binding_mismatch",
            Self::DuplicatePrecondition => "action.duplicate_precondition",
            Self::MissingPrecondition => "action.missing_precondition",
            Self::OwnerVersionConflict { .. } => "action.owner_version_conflict",
            Self::IdempotencyWithoutReceipt => "action.idempotency_without_receipt",
            Self::OwnerVersionOverflow => "action.owner_version_overflow",
        }
    }

    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::InvalidTransitionIdentity
            | Self::DuplicatePrecondition
            | Self::MissingPrecondition => RefusalCategory::Invalid,
            Self::TransitionUsedByOtherProtocol { .. }
            | Self::TransitionBindingMismatch { .. }
            | Self::OwnerVersionConflict { .. }
            | Self::IdempotencyWithoutReceipt => RefusalCategory::Conflict,
            Self::OwnerVersionOverflow => RefusalCategory::Limit,
        }
    }
}

impl Refusable for ActionRejection {
    fn refusal(&self) -> Refusal {
        Refusal::new(self.code(), self.category(), self)
    }
}

impl<X: Refusable> AuthorizedRejection<X> {
    /// Stable machine-readable code, preserving a shell refusal's code.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Shell(error) => error.refusal().code,
            Self::BindingMismatch { .. } => "action.authorized_binding_mismatch".to_owned(),
            Self::UsedByAuthorityProtocol { .. } => {
                "action.transition_protocol_conflict".to_owned()
            }
            Self::LegacyVersionedReceipt => "action.legacy_receipt".to_owned(),
            Self::LogicalTimeRollback => "action.logical_time_rollback".to_owned(),
        }
    }

    /// The refusal's category, preserving the shell's distinction between
    /// an authorization denial and an internal persistence failure.
    #[must_use]
    pub fn category(&self) -> RefusalCategory {
        match self {
            Self::BindingMismatch { .. }
            | Self::UsedByAuthorityProtocol { .. }
            | Self::LegacyVersionedReceipt => RefusalCategory::Conflict,
            Self::LogicalTimeRollback => RefusalCategory::Invalid,
            Self::Shell(error) => error.refusal().category,
        }
    }
}

impl<X: Refusable> Refusable for AuthorizedRejection<X> {
    fn refusal(&self) -> Refusal {
        match self {
            Self::Shell(error) => error.refusal(),
            Self::BindingMismatch { .. }
            | Self::UsedByAuthorityProtocol { .. }
            | Self::LegacyVersionedReceipt
            | Self::LogicalTimeRollback => Refusal::new(&self.code(), self.category(), self),
        }
    }
}

impl<R: Refusable> StepError<R> {
    /// Stable machine-readable code, preserving a rejected domain's code.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Rejected(rejection) => rejection.refusal().code,
            Self::ReadOnlyWorld => "decision.read_only_world".to_owned(),
            Self::PerformOutsideMain { .. } => "decision.perform_outside_main".to_owned(),
            Self::EnvelopeExceeded { .. } => "decision.envelope_exceeded".to_owned(),
        }
    }

    /// The refusal's category, preserving a rejected domain's category.
    #[must_use]
    pub fn category(&self) -> RefusalCategory {
        match self {
            Self::Rejected(rejection) => rejection.refusal().category,
            Self::ReadOnlyWorld | Self::PerformOutsideMain { .. } => RefusalCategory::Invalid,
            Self::EnvelopeExceeded { .. } => RefusalCategory::Limit,
        }
    }
}

impl<R: Refusable + fmt::Display> Refusable for StepError<R> {
    fn refusal(&self) -> Refusal {
        match self {
            Self::Rejected(rejection) => rejection.refusal(),
            Self::ReadOnlyWorld
            | Self::PerformOutsideMain { .. }
            | Self::EnvelopeExceeded { .. } => Refusal::new(&self.code(), self.category(), self),
        }
    }
}

impl Refusable for IngressLimitExceeded {
    fn refusal(&self) -> Refusal {
        Refusal::new("ingress.limit_exceeded", RefusalCategory::Limit, self)
    }
}
