//! AUTH-ATTENUATE-1: delegation restricts existing authority, never replaces it.
use std::collections::{BTreeMap, BTreeSet};

use crate::request::is_exact_value;
use crate::{AccessRequest, Delegation, PrincipalInput, ScopeKey};

/// Immutable grant identity plus its issuance revocation generation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrantPin {
    /// Authoritative store identity; edits require a new identity.
    pub id: String,
    /// Generation at issuance; must match the current snapshot generation.
    pub generation: u64,
}

/// Authenticated store row, never decoded from request JSON or token claims.
#[derive(Debug, Clone)]
pub struct DelegationGrant {
    /// Exact immutable row identity.
    pub pin: GrantPin,
    /// Parent identity, absent only for a root.
    pub parent: Option<GrantPin>,
    /// Authenticated issuer; child issuer must be its parent's recipient.
    pub issuer: PrincipalInput,
    /// Recipient identity; roles and attributes confer no authority here.
    pub recipient: PrincipalInput,
    /// Authenticated accountable person, independent of acting sessions.
    pub accountable_person: String,
    /// Exact scope, with no wildcard dimensions.
    pub scope: ScopeKey,
    /// Explicit capability subset.
    pub capabilities: BTreeSet<String>,
    /// Explicit action subset.
    pub actions: BTreeSet<String>,
    /// Explicit (object type, resource id) subset.
    pub resources: BTreeSet<(String, String)>,
    /// Exclusive expiry in the host's logical clock.
    pub expires_at: u64,
    /// Remaining budget, read atomically with authorization and consumption.
    pub remaining_budget: u64,
}

/// Trusted host snapshot for one commit/dispatch attempt. The host authenticates
/// rows and accountable identities against its grant store, not request claims.
/// Rebuild after revocation or consumption; retaining this snapshot across
/// attempts does not reserve budget or establish current authority.
#[derive(Debug, Clone, Default)]
pub struct DelegationSnapshot {
    /// Immutable grants indexed by exact identity.
    pub grants: BTreeMap<String, DelegationGrant>,
    /// Current revocation generations, including every ancestor.
    pub generations: BTreeMap<String, u64>,
    /// Current trusted logical time, in the same units as expiry.
    pub now: u64,
    /// Trusted cost of this attempt; cannot be supplied by the request.
    pub cost: u64,
}

fn same_actor(a: &PrincipalInput, b: &PrincipalInput) -> bool {
    a.subject_id == b.subject_id
        && a.actor_kind == b.actor_kind
        && a.agent_session_id == b.agent_session_id
}

fn exact_actor(a: &PrincipalInput) -> bool {
    is_exact_value(&a.subject_id) && a.agent_session_id.as_deref().is_none_or(is_exact_value)
}

impl DelegationSnapshot {
    /// AUTH-ATTENUATE-1 / AUTH-REVOKE: every hop is current and attenuated.
    pub(crate) fn allows(&self, request: &AccessRequest) -> bool {
        let Delegation::Chain(pins) = &request.delegation else {
            return request.delegation == Delegation::Direct;
        };
        if pins.is_empty() || pins.len() > 32 || self.cost == 0 {
            return false;
        }
        let mut seen = BTreeSet::new();
        let mut actors = BTreeSet::new();
        let mut parent: Option<&DelegationGrant> = None;
        for pin in pins {
            let Some(grant) = self.grants.get(&pin.id) else {
                return false;
            };
            if !seen.insert(&pin.id)
                || grant.pin != *pin
                || self.generations.get(&pin.id) != Some(&pin.generation)
                || !is_exact_value(&pin.id)
                || !exact_actor(&grant.issuer)
                || !exact_actor(&grant.recipient)
                || !is_exact_value(&grant.accountable_person)
                || !grant.scope.is_exact()
                || request.scope.as_ref() != Some(&grant.scope)
                || self.now >= grant.expires_at
                || self.cost > grant.remaining_budget
                || !grant.capabilities.contains(&request.capability)
                || !grant.actions.contains(&request.action_id)
                || !grant.resources.contains(&(
                    request.resource.object_type.clone(),
                    request.resource.id.clone(),
                ))
            {
                return false;
            }
            if let Some(p) = parent {
                if grant.parent.as_ref() != Some(&p.pin)
                    || !same_actor(&grant.issuer, &p.recipient)
                    || grant.accountable_person != p.accountable_person
                    || !grant.capabilities.is_subset(&p.capabilities)
                    || !grant.actions.is_subset(&p.actions)
                    || !grant.resources.is_subset(&p.resources)
                    || grant.expires_at > p.expires_at
                    || grant.remaining_budget > p.remaining_budget
                {
                    return false;
                }
            } else if grant.parent.is_some() {
                return false;
            } else {
                actors.insert((
                    &grant.issuer.subject_id,
                    grant.issuer.agent_session_id.as_ref(),
                ));
            }
            if !actors.insert((
                &grant.recipient.subject_id,
                grant.recipient.agent_session_id.as_ref(),
            )) {
                return false;
            }
            parent = Some(grant);
        }
        parent.is_some_and(|leaf| same_actor(&leaf.recipient, &request.principal))
    }

    /// Capability authority is the authenticated root issuer, while Cedar
    /// still evaluates the acting leaf. No independent leaf grant is minted.
    pub(crate) fn capability_principal<'a>(
        &'a self,
        request: &'a AccessRequest,
    ) -> Option<&'a PrincipalInput> {
        match &request.delegation {
            Delegation::Direct => Some(&request.principal),
            Delegation::Chain(pins) if self.allows(request) => self
                .grants
                .get(&pins.first()?.id)
                .map(|grant| &grant.issuer),
            _ => None,
        }
    }

    /// Accountable identity only after the complete chain passes the PDP.
    pub(crate) fn accountable_person<'a>(&'a self, request: &AccessRequest) -> Option<&'a str> {
        let Delegation::Chain(pins) = &request.delegation else {
            return None;
        };
        self.grants
            .get(&pins.last()?.id)
            .map(|g| g.accountable_person.as_str())
    }
}
