//! The public three-systems demo cell (Drop 9), provisioned idempotently.
//!
//! The definitions are the retained catalog Studio already renders
//! (`fixtures/demo/explorer/three-systems.definitions.json`). They are admitted
//! by the shared checker at boot, so the admission receipt and its digest are
//! computed, not copied; an identical provision on a restarted Durable Object
//! reopens the first revision without writing (`provision_definitions`).

use std::fmt;

use norn_api::scope::CellScope;
use norn_cell::{CellStore, InstalledDefinitionInput, Model, engine::SqlEngine};
use norn_ir::ArtifactEnvelope;

const DEFINITIONS: &str =
    include_str!("../../../fixtures/demo/explorer/three-systems.definitions.json");

/// Logical install time of the demo revision; fixed so every boot is identical.
const INSTALLED_AT: u64 = 1;

/// A bootstrap failure. The cell is not served; the text is for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedError(pub String);

impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SeedError {}

fn fail(context: &str) -> impl Fn(&dyn fmt::Display) -> SeedError + '_ {
    move |error| SeedError(format!("{context}: {error}"))
}

/// The demo cell's physical scope.
#[must_use]
pub fn demo_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

/// Admit the demo definitions and provision (or reopen) them on `engine`.
///
/// # Errors
/// The retained set no longer admits, or the engine holds a different cell.
pub fn provision<E: SqlEngine>(engine: E, scope: &CellScope) -> Result<CellStore<E>, SeedError> {
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(DEFINITIONS).map_err(|e| fail("definitions")(&e))?;
    provision_set(engine, scope, &artifacts)
}

/// Admit `artifacts` with the shared checker and provision, or reopen, them
/// on a fresh `engine` as the cell's first governed definitions version.
///
/// # Errors
/// The set does not admit, or the engine holds a different cell.
pub(crate) fn provision_set<E: SqlEngine>(
    engine: E,
    scope: &CellScope,
    artifacts: &[ArtifactEnvelope],
) -> Result<CellStore<E>, SeedError> {
    let (model, input) = admit_set(artifacts)?;
    CellStore::provision_definitions(engine, model, &input, &scope.key())
        .map_err(|e| fail("provision")(&e))
}

/// Admit `artifacts` with the shared checker (any `core.` or `std.` artifact
/// as the retained trusted base): the projected model and the install input
/// carrying the computed receipt, installed at the fixed logical time.
///
/// # Errors
/// The set does not admit.
pub(crate) fn admit_set(
    artifacts: &[ArtifactEnvelope],
) -> Result<(Model, InstalledDefinitionInput), SeedError> {
    let admitted =
        norn_api::kernel_set::admit_kernel_set(artifacts).map_err(|refusal| match refusal {
            norn_api::kernel_set::KernelRefusal::Refused(diagnostics) => {
                SeedError(format!("demo definitions refused: {diagnostics:?}"))
            }
            norn_api::kernel_set::KernelRefusal::Invalid(detail) => SeedError(detail),
        })?;
    let input = InstalledDefinitionInput {
        canonical_set: admitted.canonical_set,
        registry: vec![],
        receipt_sha256: admitted.receipt_sha256,
        admission_receipt: Some(admitted.receipt),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: INSTALLED_AT,
        layout: None,
    };
    Ok((admitted.model, input))
}
