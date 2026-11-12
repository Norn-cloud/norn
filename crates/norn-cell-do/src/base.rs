//! The trusted base this host compiles in: the artifacts of
//! `registry/core/artifacts` in a reserved namespace (`core.`, `std.`).
//!
//! A tenant never submits these. Provisioning prepends them to the tenant's
//! artifacts and admits the tenant's as candidates against them
//! (`norn_api::provision`), so what a cell trusts is decided by this build,
//! never by a name or bytes in a request. A submission names the base it was
//! authored against by digest ([`TrustedBase::sha256`]).

use norn_api::provision::TrustedBase;

use crate::seed::SeedError;

/// Every reserved-namespace artifact of `registry/core/artifacts`, one
/// envelope each. `tests/provision.rs` checks the list against the directory.
pub const SOURCES: [&str; 22] = [
    include_str!("../../../registry/core/artifacts/core.Agent.interface.json"),
    include_str!("../../../registry/core/artifacts/core.AgentRole.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Claim.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Commitment.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Conversion.interface.json"),
    include_str!("../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Entitlement.entity.json"),
    include_str!("../../../registry/core/artifacts/core.Exchange.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Hold.interface.json"),
    include_str!("../../../registry/core/artifacts/core.LedgerEffect.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Order.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Resource.interface.json"),
    include_str!("../../../registry/core/artifacts/core.analytics.Restatement.entity.json"),
    include_str!(
        "../../../registry/core/artifacts/core.analytics.RestatementEvidence.interface.json"
    ),
    include_str!("../../../registry/core/artifacts/core.analytics.restatement.access.policy.json"),
    include_str!(
        "../../../registry/core/artifacts/core.analytics.restatement.activate.action.json"
    ),
    include_str!("../../../registry/core/artifacts/core.analytics.restatement.approve.action.json"),
    include_str!("../../../registry/core/artifacts/core.analytics.restatement_policy.law.json"),
    include_str!("../../../registry/core/artifacts/core.units.json"),
    include_str!("../../../registry/core/artifacts/core.value_types.entity.json"),
    include_str!("../../../registry/core/artifacts/std.value_types.entity.json"),
];

/// The host's trusted base.
///
/// # Errors
/// A compiled-in source is not a reserved-namespace artifact envelope.
pub fn trusted_base() -> Result<TrustedBase, SeedError> {
    TrustedBase::from_sources(&SOURCES).map_err(|error| SeedError(format!("trusted base: {error}")))
}
