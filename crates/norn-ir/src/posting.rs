//! Exact posting-leg contracts used by generic cell admission.
//! This module contains no accounting calendar, journal state or inventory authority.

use crate::PropertyId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Exact authority pins repeated by every account reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountingAccountRef {
    pub scope_key: String,
    pub book_id: String,
    pub graph_id: String,
    pub graph_version: u64,
    pub account_id: String,
}

/// One exact signed accounting leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountingLegIr {
    pub leg_id: String,
    pub account: AccountingAccountRef,
    pub unit_id: String,
    pub unit_precision: u8,
    pub layer_id: String,
    /// Canonical signed decimal `i128` at `unit_precision`.
    pub amount_unscaled: String,
}

/// How an object type declares a stored exact GL leg and its journal identity.
///
/// `journal_id_property` groups each committed leg into the journal identified
/// by its journal identity; `leg_property` stores the complete
/// exact [`AccountingLegIr`] value. The type-level declaration makes every
/// write to that object type discoverable by the shared kernel commit check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountingLegDeclaration {
    /// Property whose non-empty text value is the posting's journal id.
    pub journal_id_property: PropertyId,
    /// Property whose JSON value is a serialized accounting leg.
    pub leg_property: PropertyId,
}

/// Closed error vocabulary for the bounded pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountingError {
    InvalidState,
    UnsupportedIrVersion,
    InvalidIdentity,
    ScopeMismatch,
    BookMismatch,
    GraphMismatch,
    GraphVersionMismatch,
    CalendarMismatch,
    CalendarVersionMismatch,
    StaleOwnerVersion,
    OwnerVersionOverflow,
    InvalidDate,
    PeriodNotFound,
    PeriodClosed,
    PeriodExceptionMissing,
    PeriodExceptionScopeMismatch,
    PeriodExceptionDateMismatch,
    PeriodExceptionVersionMismatch,
    PeriodExceptionExpired,
    PeriodExceptionConsumed,
    DuplicateUnit,
    InvalidOntologyBinding,
    DuplicateLayer,
    DuplicateAccount,
    UnknownParentAccount,
    AccountGraphCycle,
    UnknownAccount,
    AccountReferenceMismatch,
    UnknownUnit,
    AccountUnitMismatch,
    UnitPrecisionMismatch,
    UnknownLayer,
    DuplicateLeg,
    TooFewLegs,
    InvalidLegRecord,
    LegIdentityMismatch,
    InvalidAmount,
    AmountNotReversible,
    ArithmeticOverflow,
    JournalUnbalanced,
    JournalUnbalancedByUnit,
    JournalUnbalancedByLayer,
    JournalAlreadyExists,
    JournalNotFound,
    JournalVersionMismatch,
    PostedJournalImmutable,
    JournalDeleteForbidden,
    ReversalNotExact,
    JournalAlreadyReversed,
    CannotReverseReversal,
    IdempotencyConflict,
    QuantityCheckerRejected,
    SerializationFailed,
}

impl AccountingError {
    /// Stable machine-readable result code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidState => "accounting.invalid_state",
            Self::UnsupportedIrVersion => "accounting.unsupported_ir_version",
            Self::InvalidIdentity => "accounting.invalid_identity",
            Self::ScopeMismatch => "accounting.scope_mismatch",
            Self::BookMismatch => "accounting.book_mismatch",
            Self::GraphMismatch => "accounting.graph_mismatch",
            Self::GraphVersionMismatch => "accounting.graph_version_mismatch",
            Self::CalendarMismatch => "accounting.calendar_mismatch",
            Self::CalendarVersionMismatch => "period.calendar_version_mismatch",
            Self::StaleOwnerVersion => "accounting.stale_owner_version",
            Self::OwnerVersionOverflow => "accounting.owner_version_overflow",
            Self::InvalidDate => "period.invalid_date",
            Self::PeriodNotFound => "period.not_found",
            Self::PeriodClosed => "period.closed",
            Self::PeriodExceptionMissing => "period.exception_missing",
            Self::PeriodExceptionScopeMismatch => "period.exception_scope_mismatch",
            Self::PeriodExceptionDateMismatch => "period.exception_date_mismatch",
            Self::PeriodExceptionVersionMismatch => "period.exception_version_mismatch",
            Self::PeriodExceptionExpired => "period.exception_expired",
            Self::PeriodExceptionConsumed => "period.exception_consumed",
            Self::DuplicateUnit => "account_graph.duplicate_unit",
            Self::InvalidOntologyBinding => "account_graph.invalid_ontology_binding",
            Self::DuplicateLayer => "account_graph.duplicate_layer",
            Self::DuplicateAccount => "account_graph.duplicate_account",
            Self::UnknownParentAccount => "account_graph.unknown_parent",
            Self::AccountGraphCycle => "account_graph.cycle",
            Self::UnknownAccount => "journal.unknown_account",
            Self::AccountReferenceMismatch => "journal.account_reference_mismatch",
            Self::UnknownUnit => "journal.unknown_unit",
            Self::AccountUnitMismatch => "journal.account_unit_mismatch",
            Self::UnitPrecisionMismatch => "journal.unit_precision_mismatch",
            Self::UnknownLayer => "journal.unknown_layer",
            Self::DuplicateLeg => "journal.duplicate_leg",
            Self::TooFewLegs => "journal.too_few_legs",
            Self::InvalidLegRecord => "journal.invalid_leg_record",
            Self::LegIdentityMismatch => "journal.leg_identity_mismatch",
            Self::InvalidAmount => "journal.invalid_amount",
            Self::AmountNotReversible => "journal.amount_not_reversible",
            Self::ArithmeticOverflow => "journal.arithmetic_overflow",
            Self::JournalUnbalanced => "journal.unbalanced",
            Self::JournalUnbalancedByUnit => "journal.unbalanced_by_unit",
            Self::JournalUnbalancedByLayer => "journal.unbalanced_by_layer",
            Self::JournalAlreadyExists => "journal.already_exists",
            Self::JournalNotFound => "journal.not_found",
            Self::JournalVersionMismatch => "journal.version_mismatch",
            Self::PostedJournalImmutable => "journal.posted_immutable",
            Self::JournalDeleteForbidden => "journal.delete_forbidden",
            Self::ReversalNotExact => "journal.reversal_not_exact",
            Self::JournalAlreadyReversed => "journal.already_reversed",
            Self::CannotReverseReversal => "journal.cannot_reverse_reversal",
            Self::IdempotencyConflict => "transition.identity_payload_mismatch",
            Self::QuantityCheckerRejected => "accounting.quantity_checker_rejected",
            Self::SerializationFailed => "accounting.serialization_failed",
        }
    }
}
