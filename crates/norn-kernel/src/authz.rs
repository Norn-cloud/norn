//! Authorization inside the kernel (T5-AZ-3, #2578).
//!
//! The kernel repeats authorization on every call; a gateway pre-check is a
//! convenience, never the trust boundary (ruling 2026-08-27 §7, 2026-09-25
//! §16). A shell adapts the [`Pdp`] to action calls with
//! [`PdpActionAuthorizer`], supplying a trusted request mapper and visibility
//! decision. The PDP is `norn-authz`'s Cedar PDP under the AND rule (capability
//! ∧ Biscuit slot ∧ `AccessIR` Cedar); every error denies.

pub use crate::authz_action::PdpActionAuthorizer;
pub use norn_authz::{
    AccessRequest, ActorKind, AttrValue, CapabilityGrant, CommitBinding, Delegation, DenyReason,
    EntityRef, Pdp, PdpDecision, PrincipalInput, ResourceInput, ScopeKey,
};

/// A denied authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[error("authorization denied: {}", .reason.code())]
pub struct AuthzDenied {
    /// The first failing conjunct.
    pub reason: DenyReason,
}

/// Authorize one request; `Ok` only when every conjunct of the AND rule holds.
///
/// # Errors
///
/// [`AuthzDenied`] with the first failing conjunct.
pub fn authorize(pdp: &Pdp, request: &AccessRequest) -> Result<(), AuthzDenied> {
    match pdp.decide(request) {
        PdpDecision::Allow => Ok(()),
        PdpDecision::Deny(reason) => Err(AuthzDenied { reason }),
    }
}
