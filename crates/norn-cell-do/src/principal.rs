//! Principal resolution and the fixture server policy.
//!
//! [`PrincipalResolver`] is the single place a transport credential becomes a
//! Norn principal; Better Auth (#817) replaces [`FixturePrincipals`] behind it.
//! Authority is decided only by the server's [`Authorizer`]; the client never
//! supplies roles, grants or scope.

use norn_api::authz::{AccessRequest, Authorizer, DenyAll, Operation};
use norn_api::refusal::Refusal;

/// Resolves the request's `Authorization` header to a trusted principal.
pub trait PrincipalResolver {
    /// `None` is an anonymous request; every read then denies by default.
    fn resolve(&self, authorization: Option<&str>) -> Option<String>;
}

/// Public demo principals, named as `Authorization: Bearer fixture:<name>`.
/// They carry no secret: they select a synthetic identity for the public
/// demo cell and are replaced by #817 sessions.
#[derive(Debug, Clone, Copy)]
pub struct FixturePrincipals(pub &'static [&'static str]);

/// The demo's fixture principals.
pub const DEMO_PRINCIPALS: FixturePrincipals = FixturePrincipals(&["viewer@demo"]);

impl PrincipalResolver for FixturePrincipals {
    fn resolve(&self, authorization: Option<&str>) -> Option<String> {
        let name = authorization?.strip_prefix("Bearer fixture:")?;
        self.0.contains(&name).then(|| name.to_owned())
    }
}

/// Server policy for fixture principals: definition reads only. Data
/// capabilities (Population, Values, Sources) stay denied until their routes
/// land with HOST-READ-1 grants.
#[derive(Debug, Clone, Copy)]
pub struct FixturePolicy(pub FixturePrincipals);

impl Authorizer for FixturePolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::ReadDefinitions, Some(actor)) if self.0.0.contains(&actor) => Ok(()),
            _ => DenyAll.authorize(request),
        }
    }
}
