//! `norn-cell-rivet`: one tenant cell per Rivet actor (D131, #1130).
//!
//! Ring 1 host perimeter, the third host behind the `SqlEngine` + `CellHost`
//! seam next to the native host and the Durable Object host
//! (`norn-cell-do`). The cell's `SQLite` is the actor's own, on Rivet's VFS in
//! this process (`RivetKit` 3.1.1 `sqlite-local`); the request layer is
//! `norn-cell-do`'s `Tenant` and `server` dispatch, unchanged.
//!
//! - [`engine`]: `RivetEngine`, the synchronous seam over the actor's `SQLite`.
//! - [`thread`]: the actor's cell thread, the cell's writer lock.
//! - [`core`]: one opened cell, the native counterpart of `NornCell`.
//! - [`actor`]: the cell actor (`Cell` and `wake.mjs` of the Worker).
//! - [`alarm`]: the actor's durable alarm and kept scope.
//! - [`live`]: live subscriptions (`subscribe.mjs`).
//! - [`router`]: the public router (the Worker's default export).
//! - [`config`]: the pinned configuration.
//! - `conformance` (feature): the host conformance seams and probe actor.

pub mod actor;
pub mod alarm;
pub mod config;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod core;
pub mod egress;
pub mod engine;
pub mod live;
pub mod router;
pub mod thread;

use norn_api::scope::CellScope;

/// The conformance suite's synthetic scope (`org_demo/analytics`).
#[must_use]
pub fn synthetic_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".to_owned(),
        environment_id: "analytics".to_owned(),
    }
}

/// Whether `scope` is the synthetic scope.
#[must_use]
pub fn conformance_scope(scope: &CellScope) -> bool {
    *scope == synthetic_scope()
}

/// The probe actor's name (served only by conformance builds).
#[must_use]
pub const fn conformance_probe_actor() -> &'static str {
    "probe"
}

/// The registry this host serves: the cell actor, and the probe actor in
/// conformance builds.
#[must_use]
pub fn registry() -> rivetkit::Registry {
    let mut registry = rivetkit::Registry::new();
    registry.register_actor::<actor::CellActor>(actor::CELL_ACTOR);
    #[cfg(feature = "conformance")]
    registry.register_actor::<conformance::ProbeActor>(conformance::PROBE_ACTOR);
    registry
}
