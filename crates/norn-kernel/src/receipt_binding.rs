//! RECEIPT-AUTH-BIND-1: action input names evidence; only the host resolves it.
//! Binding does not consume a receipt. The action commit must fence its acceptance
//! reads and record consumption atomically; this is not publication authority.

use crate::result_reconciliation::Acceptance;
use norn_ir::{
    RuntimeValue,
    function::{Digest, FunctionOutcome, FunctionReceipt},
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
    /// Authenticated cell/world for qualified evidence. `None` selects local-only
    /// legacy lookup; `Some` refuses records without qualified provenance.
    fn origin_context(&self) -> Option<(&norn_ir::cell_id::CellId, &str)> {
        None
    }
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

/// A checked digest/result pair, retaining reconciliation reads for the commit.
#[derive(Debug)]
pub struct ReceiptBinding<T> {
    digest: Digest,
    result: T,
    acceptance: Acceptance,
}

impl<T> ReceiptBinding<T> {
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
    if !crate::cell_provenance::local_origin(record, store.origin_context()) {
        return Err(ReceiptBindingError::Mismatch);
    }
    if record.classification.is_some() || record.reason.is_some() {
        return Err(ReceiptBindingError::NotAccepted);
    }
    let digest = receipt.content_digest()?;
    let result_identity = receipt.result_identity()?;
    let lookup = crate::cell_provenance::receipt_reference(record)?
        .unwrap_or_else(|| record.receipt.clone());
    if lookup != reference.0
        || digest != record.receipt
        || result_identity != record.result_identity
        || record.command.0 != receipt.command_id
        || record.generation.0 != receipt.run.generation
        || record.attempt != receipt.attempt
        || acceptance.state.generation != record.generation
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
    })
}
