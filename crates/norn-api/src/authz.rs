//! Host authorization; absent configuration always denies.

use crate::{
    refusal::{Refusal, codes},
    scope::CellScope,
};

/// Operation admitted before reading definitions.
#[non_exhaustive]
pub enum Operation {
    /// Read the admitted definitions.
    ReadDefinitions,
}

/// A scoped host request. The actor is supplied by the trusted transport.
pub struct AccessRequest<'a> {
    /// Already admitted cell scope.
    pub scope: &'a CellScope,
    /// Authenticated principal, if available.
    pub actor: Option<&'a str>,
    /// Requested operation.
    pub operation: Operation,
}

/// Trusted server policy for host reads.
pub trait Authorizer: Send + Sync {
    /// Decide before any target is loaded.
    ///
    /// # Errors
    /// A denied request returns the original stable refusal.
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal>;
}

/// Default policy: every request refuses.
pub struct DenyAll;

impl Authorizer for DenyAll {
    fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> {
        Err(Refusal::new(
            http::StatusCode::FORBIDDEN,
            codes::AUTHZ_DENIED,
            "the request is not authorized",
            "the host denies access unless an explicit authorizer admits it",
        ))
    }
}
