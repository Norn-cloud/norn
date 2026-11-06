//! Admission of a cell's full kernel artifact set: the shared checker over
//! the set (any `core.` or `std.` artifact as the retained trusted base),
//! its canonical text and receipt, and the projected model. Cell
//! provisioning and flow installs run this one path.

use std::collections::BTreeSet;

use norn_cell::Model;
use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};

/// An admitted kernel set, ready to install.
#[derive(Debug, Clone)]
pub struct AdmittedSet {
    /// The projected model.
    pub model: Model,
    /// Canonical JSON of the full artifact list.
    pub canonical_set: String,
    /// The canonical checker receipt.
    pub receipt: String,
    /// The receipt's content digest.
    pub receipt_sha256: String,
}

/// Why a kernel set was not admitted.
#[derive(Debug, Clone)]
pub enum KernelRefusal {
    /// The checker's findings.
    Refused(Vec<norn_checker::Diagnostic>),
    /// The set could not be checked or projected; the text is diagnostic.
    Invalid(String),
}

fn invalid(context: &str) -> impl Fn(&dyn std::fmt::Display) -> KernelRefusal + '_ {
    move |error| KernelRefusal::Invalid(format!("{context}: {error}"))
}

/// Whether `artifact_id` names a trusted-base artifact by its reserved
/// namespace. Only trusted provisioning code partitions a set by name; a
/// submitted id never makes an artifact trusted (see [`base_ids`]).
#[must_use]
pub fn is_reserved(artifact_id: &str) -> bool {
    artifact_id.starts_with("core.") || artifact_id.starts_with("std.")
}

/// The ids the retained admission receipt recorded as the trusted base: the
/// only source of base membership for a set built from requests.
///
/// # Errors
/// The receipt is not JSON.
pub fn base_ids(receipt: Option<&str>) -> Result<BTreeSet<String>, String> {
    let Some(receipt) = receipt else {
        return Ok(BTreeSet::new());
    };
    let value: serde_json::Value =
        serde_json::from_str(receipt).map_err(|error| error.to_string())?;
    Ok(value
        .pointer("/artifactSet/base")
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|entry| entry.get("artifactId").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect())
}

/// Admit the full set `artifacts` from trusted provisioning code, its base
/// partitioned by reserved namespace.
///
/// # Errors
/// The checker's findings, or a set that cannot be encoded or projected.
pub fn admit_kernel_set(artifacts: &[ArtifactEnvelope]) -> Result<AdmittedSet, KernelRefusal> {
    admit_with(artifacts, |artifact| is_reserved(&artifact.artifact_id))
}

/// Admit the full set `artifacts` with exactly `base` as the trusted base.
///
/// # Errors
/// The checker's findings, or a set that cannot be encoded or projected.
pub fn admit_kernel_set_with_base(
    artifacts: &[ArtifactEnvelope],
    base: &BTreeSet<String>,
) -> Result<AdmittedSet, KernelRefusal> {
    admit_with(artifacts, |artifact| base.contains(&artifact.artifact_id))
}

fn admit_with(
    artifacts: &[ArtifactEnvelope],
    is_base: impl Fn(&ArtifactEnvelope) -> bool,
) -> Result<AdmittedSet, KernelRefusal> {
    let value = |artifact: &ArtifactEnvelope| {
        serde_json::to_value(artifact)
            .map_err(|e| invalid("artifact wire")(&e))
            .and_then(|wire| {
                RuntimeValue::from_json(wire).map_err(|e| invalid("artifact value")(&e))
            })
    };
    let (base, candidates): (Vec<_>, Vec<_>) = artifacts.iter().cloned().partition(is_base);
    let candidate_values = candidates
        .iter()
        .map(value)
        .collect::<Result<Vec<_>, _>>()?;
    let base_values = base.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let checked = check_artifact_set_with_receipt(
        &ReceiptSources::new(&candidate_values, &base_values)
            .map_err(|e| invalid("receipt sources")(&e))?,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| invalid("admission")(&e))?;
    if !checked.receipt.admitted {
        return Err(KernelRefusal::Refused(checked.diagnostics));
    }
    let receipt = checked
        .receipt
        .canonical_json()
        .map_err(|e| invalid("receipt")(&e))?;
    let sources = artifacts.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let canonical_set = RuntimeValue::Array(sources)
        .canonical_json()
        .map_err(|e| invalid("canonical set")(&e))?;
    Ok(AdmittedSet {
        model: kernel_model(artifacts)?,
        canonical_set: String::from_utf8(canonical_set).map_err(|e| invalid("set")(&e))?,
        receipt_sha256: checked
            .receipt
            .content_sha256()
            .map_err(|e| invalid("receipt digest")(&e))?,
        receipt: String::from_utf8(receipt).map_err(|e| invalid("receipt")(&e))?,
    })
}

/// The normalized model of the set's entity roots and their governing laws.
///
/// # Errors
/// An entity root outside the supported profile, or a model that does not
/// project.
pub fn kernel_model(artifacts: &[ArtifactEnvelope]) -> Result<Model, KernelRefusal> {
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|e| invalid(&artifact.artifact_id)(&e))?;
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                return Err(KernelRefusal::Invalid(format!(
                    "{} is outside the supported entity profile",
                    artifact.artifact_id
                )));
            }
        }
    }
    Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .map_err(|e| invalid("model")(&e))?
        .with_governing_laws(artifacts)
        .map_err(|e| invalid("governing laws")(&e))
}

/// The artifact envelopes of an installed set's canonical text.
///
/// # Errors
/// The retained set is not an artifact array.
pub fn retained_artifacts(canonical_set: &str) -> Result<Vec<ArtifactEnvelope>, String> {
    let RuntimeValue::Array(sources) =
        RuntimeValue::from_json_text(canonical_set).map_err(|error| error.to_string())?
    else {
        return Err("retained definitions must be an artifact array".to_owned());
    };
    sources.iter().map(norn_ir::runtime::decode).collect()
}
