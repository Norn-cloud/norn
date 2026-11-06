//! The public three-systems demo cell (Drop 9), provisioned idempotently.
//!
//! The definitions are the retained catalog Studio already renders
//! (`fixtures/demo/explorer/three-systems.definitions.json`). They are admitted
//! by the shared checker at boot, so the admission receipt and its digest are
//! computed, not copied. A restarted Durable Object reopens with the model
//! of its active set and writes nothing when the seed is unchanged; a changed
//! seed is installed in place as the next governed version, keeping every
//! artifact installed since (such as flows), never wiping the cell
//! (`open_governed`).

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

/// Admit the demo definitions and provision, reopen or upgrade them on
/// `engine` (see `open_governed`).
///
/// # Errors
/// The retained set no longer admits, or the engine holds a different cell.
pub fn provision<E: SqlEngine>(engine: E, scope: &CellScope) -> Result<CellStore<E>, SeedError> {
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(DEFINITIONS).map_err(|e| fail("definitions")(&e))?;
    open_governed(engine, scope, &artifacts, INSTALLED_AT)
}

/// Open a cell whose definitions are seeded from `seed`, never wiping it:
///
/// - the store is opened with the model of its *active* definitions set
///   when it has one (so artifacts installed since, such as flows and their
///   Run types, keep the schema they were installed with), else with the
///   seed's model;
/// - a cell with no definitions version (fresh, or opened before it had
///   one) gets the seed set installed as its first governed version;
/// - a cell whose seeded artifacts differ from `seed` is upgraded in place:
///   the seed artifacts and every active artifact the seed does not own are
///   admitted together and installed as the next governed version, in one
///   commit. A cell already matching opens without writing.
///
/// # Errors
/// A set that does not admit, or an engine holding a different cell.
pub(crate) fn open_governed<E: SqlEngine>(
    engine: E,
    scope: &CellScope,
    seed: &[ArtifactEnvelope],
    installed_at: u64,
) -> Result<CellStore<E>, SeedError> {
    let active =
        CellStore::read_active_definitions(&engine).map_err(|e| fail("definitions")(&e))?;
    let (seed_model, seed_input) = admit_set(seed)?;
    let Some(active) = active else {
        let mut store =
            CellStore::from_engine(engine, seed_model.clone()).map_err(|e| fail("open")(&e))?;
        store
            .bind("scope", &scope.key())
            .map_err(|e| fail("scope")(&e))?;
        let input = InstalledDefinitionInput {
            installed_at: installed_at.max(clock(&store)?),
            ..seed_input
        };
        store
            .install_definitions(seed_model, input, Some(0), &scope.key())
            .map_err(|e| fail("definitions v1")(&e))?;
        return Ok(store);
    };
    let active_artifacts = norn_api::kernel_set::retained_artifacts(&active.canonical_set)
        .map_err(|e| fail("active definitions")(&e))?;
    let active_model =
        norn_api::kernel_set::kernel_model(&active_artifacts).map_err(|refusal| match refusal {
            norn_api::kernel_set::KernelRefusal::Refused(diagnostics) => {
                SeedError(format!("active definitions refused: {diagnostics:?}"))
            }
            norn_api::kernel_set::KernelRefusal::Invalid(detail) => SeedError(detail),
        })?;
    let mut store = CellStore::from_engine(engine, active_model).map_err(|e| fail("open")(&e))?;
    store
        .bind("scope", &scope.key())
        .map_err(|e| fail("scope")(&e))?;
    let seeded: Vec<&str> = seed
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    let mut merged: Vec<ArtifactEnvelope> = seed.to_vec();
    merged.extend(
        active_artifacts
            .into_iter()
            .filter(|artifact| !seeded.contains(&artifact.artifact_id.as_str())),
    );
    let (model, input) = admit_set(&merged)?;
    if input.canonical_set == active.canonical_set {
        return Ok(store);
    }
    let input = InstalledDefinitionInput {
        installed_at: installed_at.max(clock(&store)?),
        ..input
    };
    store
        .install_definitions(model, input, Some(active.version), &scope.key())
        .map_err(|e| fail("definitions upgrade")(&e))?;
    Ok(store)
}

/// The logical time of the cell's last commit (0 when it has none).
fn clock<E: SqlEngine>(store: &CellStore<E>) -> Result<u64, SeedError> {
    let commits = store
        .verified_head()
        .map_err(|e| fail("ledger")(&e))?
        .commits;
    Ok(store
        .read()
        .receipt_at(commits)
        .map_err(|e| fail("receipt")(&e))?
        .map_or(0, |receipt| receipt.body.logical_time))
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
