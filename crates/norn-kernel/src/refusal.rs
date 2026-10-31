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

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::action::{ActionCommandRejection, ActionRejection, AuthorizedRejection};
use crate::action_commit::ActionRefusal;
use crate::cell_host::CommitError;
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

impl<X> CommitError<X> {
    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::DuplicateKey { .. }
            | Self::DuplicateLink { .. }
            | Self::LinkTarget { .. }
            | Self::InvariantModel { .. }
            | Self::EffectiveValues { .. }
            | Self::OwnedRowsProtected { .. }
            | Self::ObservationAuthorization
            | Self::UnsupportedLinkRead { .. }
            | Self::DuplicateLinkRead { .. }
            | Self::InvalidLinkRead { .. } => RefusalCategory::Invalid,
            Self::NotPending { .. }
            | Self::Conflict { .. }
            | Self::LinkConflict { .. }
            | Self::LinkReadConflict { .. } => RefusalCategory::Conflict,
            Self::ImmutableObject { .. }
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
        Refusal::new(self.code(), self.category(), self)
    }
}

impl<X> DecisionRefusal<X> {
    /// The refusal's category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        match self {
            Self::GuardFalse { .. }
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
            Self::ReadOnlyWorld
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

impl<X> Refusable for DecisionRefusal<X> {
    fn refusal(&self) -> Refusal {
        Refusal::new(self.code(), self.category(), self)
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
            Self::InputBoundExceeded { .. } | Self::ExpressionBudgetExceeded { .. } => {
                RefusalCategory::Limit
            }
            Self::Command(rejection) => rejection.category(),
            Self::MissingScope { .. }
            | Self::UnsupportedEffect { .. }
            | Self::InvalidInput { .. }
            | Self::DerivationFailed { .. }
            | Self::SliceUnavailable { .. }
            | Self::KindMismatch { .. } => RefusalCategory::Invalid,
        }
    }
}

impl<X> Refusable for ActionRefusal<X> {
    fn refusal(&self) -> Refusal {
        Refusal::new(self.code(), self.category(), self)
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
