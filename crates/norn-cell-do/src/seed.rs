//! The public three-systems demo cell (Drop 9), provisioned idempotently.
//!
//! The definitions are the retained catalog Studio already renders
//! (`fixtures/demo/explorer/three-systems.definitions.json`). They are admitted
//! by the shared checker at boot, so the admission receipt and its digest are
//! computed, not copied; an identical provision on a restarted Durable Object
//! reopens the first revision without writing (`provision_definitions`).

use std::collections::BTreeSet;
use std::fmt;

use norn_api::scope::CellScope;
use norn_cell::{CellStore, InstalledDefinitionInput, Model, engine::SqlEngine};
use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};

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
    let value = |artifact: &ArtifactEnvelope| {
        serde_json::to_value(artifact)
            .map_err(|e| fail("artifact wire")(&e))
            .and_then(|wire| RuntimeValue::from_json(wire).map_err(|e| fail("artifact value")(&e)))
    };
    // The packs are checked over the core layer as a trusted base, which the
    // cell retains in the same set (`baseArtifactIds` on the definitions read).
    let (base, candidates): (Vec<_>, Vec<_>) = artifacts.iter().cloned().partition(|a| {
        a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std.")
    });
    let candidate_values = candidates.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let base_values = base.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let checked = check_artifact_set_with_receipt(
        &ReceiptSources::new(&candidate_values, &base_values)
            .map_err(|e| fail("receipt sources")(&e))?,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| fail("admission")(&e))?;
    if !checked.receipt.admitted {
        return Err(SeedError(format!(
            "demo definitions refused: {:?}",
            checked.diagnostics
        )));
    }
    let receipt = checked
        .receipt
        .canonical_json()
        .map_err(|e| fail("receipt")(&e))?;
    let sources = artifacts.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let canonical_set = RuntimeValue::Array(sources)
        .canonical_json()
        .map_err(|e| fail("canonical set")(&e))?;
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(canonical_set).map_err(|e| fail("set")(&e))?,
        registry: vec![],
        receipt_sha256: checked
            .receipt
            .content_sha256()
            .map_err(|e| fail("receipt digest")(&e))?,
        admission_receipt: Some(String::from_utf8(receipt).map_err(|e| fail("receipt")(&e))?),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: INSTALLED_AT,
        layout: None,
    };
    CellStore::provision_definitions(engine, model(&artifacts)?, &input, &scope.key())
        .map_err(|e| fail("provision")(&e))
}

/// The normalized model of the set's entity roots and their governing laws.
fn model(artifacts: &[ArtifactEnvelope]) -> Result<Model, SeedError> {
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts.iter().filter(|a| a.kind() == ArtifactKind::Entity) {
        let root: EntityRoot = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|e| fail(&artifact.artifact_id)(&e))?;
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                return Err(SeedError(format!(
                    "{} is outside the three-systems profile",
                    artifact.artifact_id
                )));
            }
        }
    }
    Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .map_err(|e| fail("model")(&e))?
        .with_governing_laws(artifacts)
        .map_err(|e| fail("governing laws")(&e))
}
