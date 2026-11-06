//! Principal resolution: the single seam from a transport credential to a
//! verified Norn principal.
//!
//! A resolver records the verified principal into the policy's
//! [`CallerSlot`], where the declared grant table turns its roles into
//! grants. Verified host tokens (#817) implement this seam next;
//! [`FixturePrincipals`] serves tests and local smoke builds; a Durable Object
//! selects it only with the `fixtures` feature, and otherwise nobody is
//! verified ([`NoPrincipals`]).

use std::sync::Arc;

use crate::policy::{Caller, CallerSlot};

/// Resolves the request's `Authorization` header to a trusted principal at
/// `now` (Unix seconds), recording it for the policy.
pub trait PrincipalResolver {
    /// The verified subject, or `None`; every read then denies by default.
    fn resolve(&self, authorization: Option<&str>, now: u64) -> Option<String>;

    /// Forget the request's principal. Required: the dispatch calls it when
    /// a request ends, on every exit path ([`RequestScope`]).
    fn end_request(&self);
}

/// The lifetime of one request's verified principal: it ends when this
/// guard drops, on every return path and during unwinding.
pub struct RequestScope<'a>(&'a dyn PrincipalResolver);

impl<'a> RequestScope<'a> {
    /// Begin a request on `principals`.
    #[must_use]
    pub const fn new(principals: &'a dyn PrincipalResolver) -> Self {
        Self(principals)
    }
}

impl Drop for RequestScope<'_> {
    fn drop(&mut self) {
        self.0.end_request();
    }
}

/// A deployable cell without a configured resolver: nobody is verified.
pub struct NoPrincipals(pub Arc<CallerSlot>);

impl PrincipalResolver for NoPrincipals {
    fn resolve(&self, _authorization: Option<&str>, _now: u64) -> Option<String> {
        self.0.record(None);
        None
    }

    fn end_request(&self) {
        self.0.record(None);
    }
}

/// Synthetic principals for tests and local smoke builds, named as
/// `Authorization: Bearer fixture:<name>`, each with its Norn roles.
pub struct FixturePrincipals {
    names: &'static [(&'static str, &'static [&'static str])],
    slot: Arc<CallerSlot>,
}

/// The fixture principals: `viewer@demo` holds `admin`, `guest@demo` `viewer`.
pub const DEMO_FIXTURES: &[(&str, &[&str])] =
    &[("viewer@demo", &["admin"]), ("guest@demo", &["viewer"])];

impl FixturePrincipals {
    /// Fixture principals recording into `slot`.
    #[must_use]
    pub const fn new(
        names: &'static [(&'static str, &'static [&'static str])],
        slot: Arc<CallerSlot>,
    ) -> Self {
        Self { names, slot }
    }
}

impl PrincipalResolver for FixturePrincipals {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        let caller = authorization
            .and_then(|value| value.strip_prefix("Bearer fixture:"))
            .and_then(|name| self.names.iter().find(|(known, _)| *known == name))
            .map(|(name, roles)| Caller {
                subject_id: (*name).to_owned(),
                roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            });
        let subject = caller.as_ref().map(|caller| caller.subject_id.clone());
        self.slot.record(caller);
        subject
    }

    fn end_request(&self) {
        self.slot.record(None);
    }
}
