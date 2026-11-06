//! RECEIPT-AUTH-BIND-1: action input names evidence; only the host resolves it.
//! Binding does not consume a receipt. The action commit must fence its acceptance
//! reads and record consumption atomically; this is not publication authority.

use crate::result_reconciliation::Acceptance;
use norn_ir::{
    RuntimeValue,
    function::{Digest, FunctionOutcome, FunctionReceipt, RunRef},
};
use serde::de::DeserializeOwned;

/// A receipt reference admitted from action JSON; no receipt body is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptReference(Digest);

/// Invalid action-side receipt argument.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceError {
    /// Receipt bodies and other structured values are never authority.
    #[error("action.receipt_body_forbidden")]
    BodyForbidden,
    /// A reference must be a content digest, never a mutable identifier.
    #[error("action.receipt_reference_invalid")]
    InvalidDigest,
}

impl ReceiptReference {
    /// Admit only a digest string, refusing a supplied body even if it is valid.
    ///
    /// # Errors
    /// A structured body, non-string value, or malformed content digest.
    pub fn parse(value: &RuntimeValue) -> Result<Self, ReferenceError> {
        let RuntimeValue::String(text) = value else {
            return Err(ReferenceError::BodyForbidden);
        };
        if text.len() != 71 {
            return Err(ReferenceError::InvalidDigest);
        }
        let digest = Digest(text.clone());
        if !digest.is_well_formed() {
            return Err(ReferenceError::InvalidDigest);
        }
        Ok(Self(digest))
    }

    /// Immutable receipt identity the host must resolve.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.0
    }
}

/// A host-scoped lookup of already authenticated, reconciled receipt evidence.
pub enum ReceiptResolution<'a> {
    /// No accepted receipt in the current tenant/world/authority context.
    Unknown,
    /// A prior action already consumed this receipt; exact action retries are
    /// handled by normal commit idempotency before this binding is evaluated.
    Replayed,
    /// Authentic receipt and its persisted reconciliation acceptance.
    Accepted {
        /// Acceptance produced by the existing result reconciliation path.
        acceptance: &'a Acceptance,
        /// Receipt authenticated and retained by the host, never action JSON.
        receipt: &'a FunctionReceipt,
    },
}

/// The existing host receipt store supplies authentication and scope checks.
/// Implementations must resolve persisted acceptance and consumption state;
/// they must never manufacture acceptance from the action's input payload.
pub trait AcceptedReceiptStore {
    /// Host storage failure.
    type Error;
    /// Resolve within current tenant, world and authority; inaccessible evidence
    /// is `Unknown`. The accepting action must fence this read at commit.
    ///
    /// # Errors
    /// Host storage or authentication failure.
    fn resolve(&self, reference: &Digest) -> Result<ReceiptResolution<'_>, Self::Error>;
}

/// A refused binding cannot supply a typed result or acceptance to an action.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptBindingError<E> {
    /// Reference is absent or inaccessible.
    #[error("action.receipt_unknown")]
    Unknown,
    /// Receipt was consumed by an earlier action.
    #[error("action.receipt_replayed")]
    Replayed,
    /// Stored acceptance and requested/actual receipt identities disagree.
    #[error("action.receipt_binding_mismatch")]
    Mismatch,
    /// Only accepted successful results can become checked action inputs.
    #[error("action.receipt_not_accepted")]
    NotAccepted,
    /// The accepted summary does not decode to the action's declared result type.
    #[error("action.receipt_result_type")]
    ResultType,
    /// Canonical identity cannot be computed.
    #[error("receipt identity: {0}")]
    Canonical(#[from] norn_ir::canonical::CanonicalJsonError),
    /// Host refused the lookup.
    #[error("receipt store: {0}")]
    Store(E),
}

/// RECEIPT-EFFECT-1: why an action call cannot consume its receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiptRefusal {
    /// The input supplied a body, not a reference.
    BodyForbidden,
    /// The reference is not a content digest.
    ReferenceInvalid,
    /// The host does not support receipt effects, or the acceptance needs
    /// fences this path has not qualified.
    EffectUnsupported,
    /// No accepted receipt is visible under this reference.
    Unknown,
    /// An earlier action consumed the receipt.
    Replayed,
    /// The receipt's pins disagree with its acceptance or declaration.
    BindingMismatch,
    /// The receipt is not an accepted success.
    NotAccepted,
    /// The accepted summary does not fit the declared bindings.
    ResultType,
    /// The receipt belongs to another run or generation.
    RunMismatch,
    /// The receipt's delivery is no longer pending at commit.
    NotPending,
}

impl ReceiptRefusal {
    /// Stable `action.receipt_*` code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BodyForbidden => "action.receipt_body_forbidden",
            Self::ReferenceInvalid => "action.receipt_reference_invalid",
            Self::EffectUnsupported => "action.receipt_effect_unsupported",
            Self::Unknown => "action.receipt_unknown",
            Self::Replayed => "action.receipt_replayed",
            Self::BindingMismatch => "action.receipt_binding_mismatch",
            Self::NotAccepted => "action.receipt_not_accepted",
            Self::ResultType => "action.receipt_result_type",
            Self::RunMismatch => "action.receipt_run_mismatch",
            Self::NotPending => "action.receipt_not_pending",
        }
    }

    /// The refusal's category: an unknown receipt is indistinguishable from
    /// an invisible one; consumption races are conflicts.
    #[must_use]
    pub const fn category(self) -> crate::refusal::RefusalCategory {
        use crate::refusal::RefusalCategory;
        match self {
            Self::Unknown => RefusalCategory::Visibility,
            Self::Replayed | Self::NotPending => RefusalCategory::Conflict,
            Self::BodyForbidden
            | Self::ReferenceInvalid
            | Self::EffectUnsupported
            | Self::BindingMismatch
            | Self::NotAccepted
            | Self::ResultType
            | Self::RunMismatch => RefusalCategory::Invalid,
        }
    }
}

impl<E> ReceiptBindingError<E> {
    /// The receipt refusal, or the host failure, which is never a refusal.
    ///
    /// # Errors
    /// The host's own failure ([`ReceiptBindingError::Store`]).
    pub fn into_refusal(self) -> Result<ReceiptRefusal, E> {
        match self {
            Self::Unknown => Ok(ReceiptRefusal::Unknown),
            Self::Replayed => Ok(ReceiptRefusal::Replayed),
            Self::Mismatch | Self::Canonical(_) => Ok(ReceiptRefusal::BindingMismatch),
            Self::NotAccepted => Ok(ReceiptRefusal::NotAccepted),
            Self::ResultType => Ok(ReceiptRefusal::ResultType),
            Self::Store(error) => Err(error),
        }
    }
}

impl From<ReferenceError> for ReceiptRefusal {
    fn from(error: ReferenceError) -> Self {
        match error {
            ReferenceError::BodyForbidden => Self::BodyForbidden,
            ReferenceError::InvalidDigest => Self::ReferenceInvalid,
        }
    }
}

/// A checked digest/result pair, retaining reconciliation reads for the commit.
#[derive(Debug)]
pub struct ReceiptBinding<T> {
    digest: Digest,
    result: T,
    acceptance: Acceptance,
    run: RunRef,
    manifest: Digest,
}

impl<T> ReceiptBinding<T> {
    /// The authenticated receipt's `Run` and generation.
    #[must_use]
    pub const fn run(&self) -> &RunRef {
        &self.run
    }

    /// The Function manifest the authenticated receipt executed.
    #[must_use]
    pub const fn manifest(&self) -> &Digest {
        &self.manifest
    }

    /// Move the typed result and its exact receipt/acceptance pins into preparation.
    #[must_use]
    pub fn into_parts(self) -> (Digest, T, Acceptance) {
        (self.digest, self.result, self.acceptance)
    }
}

/// Resolve a reference through the host and bind only its authenticated result.
/// RECEIPT-AUTH-BIND-1 includes the full receipt digest, successful reconciliation,
/// command/generation/attempt and deterministic result identity. Ordinary action
/// JSON never supplies receipt evidence. Consumption still belongs to the commit.
///
/// # Errors
/// Unknown/replayed evidence, mismatched acceptance/digests, failure outcomes,
/// wrong result type, canonicalization or host storage failure.
pub fn bind_receipt<T: DeserializeOwned, S: AcceptedReceiptStore>(
    store: &S,
    reference: &ReceiptReference,
) -> Result<ReceiptBinding<T>, ReceiptBindingError<S::Error>> {
    let (acceptance, receipt) = match store
        .resolve(reference.digest())
        .map_err(ReceiptBindingError::Store)?
    {
        ReceiptResolution::Unknown => return Err(ReceiptBindingError::Unknown),
        ReceiptResolution::Replayed => return Err(ReceiptBindingError::Replayed),
        ReceiptResolution::Accepted {
            acceptance,
            receipt,
        } => (acceptance, receipt),
    };
    let record = &acceptance.record;
    if record.classification.is_some() || record.reason.is_some() {
        return Err(ReceiptBindingError::NotAccepted);
    }
    let digest = receipt.content_digest()?;
    let result_identity = receipt.result_identity()?;
    if digest != reference.0
        || digest != record.receipt
        || result_identity != record.result_identity
        || record.command.0 != receipt.command_id
        || record.generation.0 != receipt.run.generation
        || record.attempt != receipt.attempt
        || acceptance.state.generation != record.generation
        // The delivery it consumes is this command's result at this
        // generation, never another pending delivery (RECEIPT-CONSUME-1).
        || acceptance.consumed
            != (crate::cell_host::Consumed::Result {
                command: record.command.clone(),
                generation: record.generation,
            })
        || acceptance.state.cancelled
        || acceptance.state.accepted.as_ref().is_none_or(|accepted| {
            accepted.receipt != digest
                || accepted.result_identity != result_identity
                || accepted.generation != record.generation
        })
    {
        return Err(ReceiptBindingError::Mismatch);
    }
    let FunctionOutcome::Success(success) = &receipt.outcome else {
        return Err(ReceiptBindingError::NotAccepted);
    };
    let result = norn_ir::runtime::decode(&RuntimeValue::Object(success.summary.clone()))
        .map_err(|_invalid_summary| ReceiptBindingError::ResultType)?;
    Ok(ReceiptBinding {
        digest,
        result,
        acceptance: acceptance.clone(),
        run: receipt.run.clone(),
        manifest: receipt.manifest.clone(),
    })
}
