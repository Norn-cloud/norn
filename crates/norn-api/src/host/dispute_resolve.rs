//! Governed host entrypoint for `mapping.dispute.resolve` (#1549).
//!
//! Enforces explicit cell scope admission, trusted host authorization for
//! [`MAPPING_DISPUTE_RESOLVE_ACTION`], and caller-to-actor binding before
//! delegating to [`norn_cell::SqliteCell::resolve_mapping_dispute`].

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::{DisputeResolutionInput, DisputeResolutionOutcome, MAPPING_DISPUTE_RESOLVE_ACTION};

use super::DevHost;
use crate::authz::{AccessRequest, Operation};
use crate::refusal::{Refusal, codes};
use crate::scope::ScopeQuery;

impl<E: SqlEngine> DevHost<E> {
    /// Resolve one pending `mapping-conflict:` or `mapping-identity:` dispute
    /// through the governed `mapping.dispute.resolve` action (#1549).
    ///
    /// # Errors
    /// Scope mismatch, authorization refusal, unauthenticated or mismatched
    /// actor, or cell refusal.
    pub fn resolve_mapping_dispute(
        &self,
        scope: &ScopeQuery,
        actor: Option<&str>,
        input: DisputeResolutionInput,
    ) -> Result<DisputeResolutionOutcome, Refusal> {
        self.scope
            .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
        self.authorizer.authorize(&AccessRequest {
            scope: &self.scope,
            actor,
            operation: Operation::CallAction(MAPPING_DISPUTE_RESOLVE_ACTION.to_owned()),
        })?;
        let Some(actor_id) = actor.filter(|a| !a.trim().is_empty()) else {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                codes::AUTHZ_DENIED,
                "dispute resolution requires an authenticated actor",
            ));
        };
        if input.actor != actor_id {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                codes::AUTHZ_DENIED,
                "dispute resolution actor must match authenticated caller",
            ));
        }
        self.observing(|cell| cell.resolve_mapping_dispute(input))
    }
}
