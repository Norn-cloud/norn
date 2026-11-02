//! Automatic identities for admitted flow steps, without an execution runtime.

use norn_ir::canonical::{CanonicalJsonError, HashProfile, canonical_json_of};
use serde::Serialize;

use crate::IdempotencyKey;

/// Admitted, canonical identities pinned for the lifetime of a flow instance.
///
/// These fields identify an authoring site, never an object's memory address.
/// The compiler must still refuse reuse of one durable step object at two sites.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FlowStepIdentity<'a> {
    /// Cell authority scope (including tenant and environment).
    pub scope: &'a str,
    /// Execution world; fork and main identities must differ.
    pub world: &'a str,
    /// Pinned flow artifact identity.
    pub flow: &'a str,
    /// Pinned definition version or content digest.
    pub version: &'a str,
    /// Durable flow instance identity.
    pub instance: &'a str,
    /// Unique admitted durable step site.
    pub step: &'a str,
}

impl FlowStepIdentity<'_> {
    /// FLOW-KEY-1: exact redelivery retains its key; distinct attempts are separate.
    ///
    /// Hashes the canonical, structurally framed tuple, avoiding delimiter aliases.
    /// This key is for a durable attempt, not a provider business-operation key.
    ///
    /// # Errors
    /// Returns a canonical encoding error rather than falling back to a weaker key.
    pub fn attempt_key(&self, attempt: u64) -> Result<IdempotencyKey, CanonicalJsonError> {
        let bytes = canonical_json_of(&("norn.flow.attempt/1", self, attempt))?;
        Ok(IdempotencyKey(format!(
            "flow-attempt:{}",
            HashProfile::Norn1.hash_canonical_bytes(&bytes)
        )))
    }

    /// EFFECT-IDENTITY-1: provider retries retain the same business operation key.
    ///
    /// A provider must explicitly support deduplication/lookup for this key;
    /// otherwise uncertain outcomes require reconciliation, never blind retry.
    ///
    /// # Errors
    /// Returns a canonical encoding error.
    pub fn operation_key(&self) -> Result<IdempotencyKey, CanonicalJsonError> {
        let bytes = canonical_json_of(&("norn.flow.operation/1", self))?;
        Ok(IdempotencyKey(format!(
            "flow-operation:{}",
            HashProfile::Norn1.hash_canonical_bytes(&bytes)
        )))
    }
}
