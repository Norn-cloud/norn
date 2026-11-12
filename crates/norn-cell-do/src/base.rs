//! The trusted base this host compiles in.
//!
//! A tenant never submits these. Provisioning prepends them to the tenant's
//! artifacts and admits the tenant's as candidates against them
//! (`norn_api::provision`), so what a cell trusts is decided by this build,
//! never by a name or bytes in a request. A submission names the base it was
//! authored against by digest (`TrustedBase::sha256`).
//!
//! The base is the reserved-namespace (`core.`, `std.`) artifacts the host's
//! seeded demo cell admits with, read from `registry/core/artifacts`: an
//! admissible base this host already serves. Not every reserved registry
//! artifact is one on its own (`core.Order.interface` names value types and
//! interfaces that no reserved artifact defines), so the base is listed, and
//! `tests/provision.rs` checks the list against the seed and the registry.

use norn_api::provision::TrustedBase;

use crate::seed::SeedError;

/// The trusted base: one artifact envelope each.
pub const SOURCES: [&str; 20] = [
    include_str!("../../../registry/core/artifacts/core.Agent.interface.json"),
    include_str!("../../../registry/core/artifacts/core.AgentRole.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Claim.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Commitment.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Conversion.interface.json"),
    include_str!("../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Exchange.interface.json"),
    include_str!("../../../registry/core/artifacts/core.Hold.interface.json"),
    include_str!("../../../registry/core/artifacts/core.LedgerEffect.interface.json"),
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
    // Parsed and hashed once per process: every cell open needs it.
    static BASE: std::sync::OnceLock<Result<TrustedBase, String>> = std::sync::OnceLock::new();
    BASE.get_or_init(|| TrustedBase::from_sources(&SOURCES))
        .clone()
        .map_err(|error| SeedError(format!("trusted base: {error}")))
}
