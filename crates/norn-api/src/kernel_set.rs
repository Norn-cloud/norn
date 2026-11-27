//! Admission of a cell's full kernel artifact set: the shared checker over
//! the set (any `core.` or `std.` artifact as the retained trusted base),
//! its canonical text and receipt, and the projected model. Cell
//! provisioning and flow installs run this one path.

use std::collections::BTreeSet;

use norn_cell::Model;
use norn_checker::{EntityArtifactSetContext, ReceiptSources};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use norn_ui_checker::{check_artifact_set, check_artifact_set_with_receipt};

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
/// Trusted input only (a compiled-in seed, the local dev host): an artifact
/// named `core.` or `std.` is taken as base, unchecked as a candidate. A
/// request's artifacts never come here; network provisioning supplies the
/// host's own base ([`crate::provision::admit_submission`]) and refuses a
/// reserved id in the submission.
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

/// Refuse any JSON number in `raw_json` that loses precision when stored in
/// Norn IR (`JsonValue::Number(f64)`), so >53-bit decimals are never silently
/// rounded at admission or split between `canonical_set` and the checker
/// receipt. Exact >53-bit decimals in definitions must be encoded as strings
/// (such as `initial.idDecimal` in `norn.pull/1`).
///
/// # Errors
/// A JSON number inside `raw_json` that does not round-trip losslessly through
/// IEEE-754 binary64.
pub fn reject_lossy_ir_numbers(raw_json: &str) -> Result<(), String> {
    fn check(value: &RuntimeValue) -> Result<(), String> {
        match value {
            RuntimeValue::Number(decimal) if !crate::pull_feed::is_binary64_exact(*decimal) => {
                Err(format!(
                    "artifact JSON number {decimal} exceeds IEEE-754 binary64 IR precision; encode exact decimals as strings (e.g. initial.idDecimal)"
                ))
            }
            RuntimeValue::Array(items) => {
                for item in items {
                    check(item)?;
                }
                Ok(())
            }
            RuntimeValue::Object(entries) => {
                for item in entries.values() {
                    check(item)?;
                }
                Ok(())
            }
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::String(_) => Ok(()),
        }
    }
    if let Ok(value) = RuntimeValue::from_json_text(raw_json) {
        check(&value)?;
    }
    Ok(())
}

/// Admit the full set `artifacts` from raw JSON text, refusing any JSON number
/// that would lose precision when decoded into Norn IR [`ArtifactEnvelope`].
///
/// # Errors
/// Invalid JSON, a lossy >53-bit JSON number, the checker's findings, or a set
/// that cannot be encoded or projected.
pub fn admit_kernel_set_json(artifacts_json: &str) -> Result<AdmittedSet, KernelRefusal> {
    reject_lossy_ir_numbers(artifacts_json).map_err(KernelRefusal::Invalid)?;
    let RuntimeValue::Array(values) = RuntimeValue::from_json_text(artifacts_json)
        .map_err(|e| invalid("artifact set json")(&e))?
    else {
        return Err(KernelRefusal::Invalid(
            "artifact set json must be an array".to_owned(),
        ));
    };
    let artifacts: Vec<ArtifactEnvelope> = values
        .iter()
        .map(|rv| norn_ir::runtime::decode(rv).map_err(KernelRefusal::Invalid))
        .collect::<Result<_, _>>()?;
    admit_kernel_set(&artifacts)
}

fn encode_artifact_value(artifact: &ArtifactEnvelope) -> Result<RuntimeValue, KernelRefusal> {
    serde_json::to_string(artifact)
        .map_err(|e| invalid("artifact wire")(&e))
        .and_then(|wire| {
            RuntimeValue::from_json_text(&wire).map_err(|e| invalid("artifact value")(&e))
        })
}

fn admit_with(
    artifacts: &[ArtifactEnvelope],
    is_base: impl Fn(&ArtifactEnvelope) -> bool,
) -> Result<AdmittedSet, KernelRefusal> {
    let mut base = Vec::new();
    let mut candidates = Vec::new();
    let mut base_values = Vec::new();
    let mut candidate_values = Vec::new();
    let mut all_values = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let rv = encode_artifact_value(artifact)?;
        if is_base(artifact) {
            base.push(artifact.clone());
            base_values.push(rv.clone());
        } else {
            candidates.push(artifact.clone());
            candidate_values.push(rv.clone());
        }
        all_values.push(rv);
    }
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &base,
        tenant: None,
    };
    // Check the decoded envelopes BEFORE re-serializing to RuntimeValue for receipt signing:
    // closed payload roots record unknown fields and invalid extensions during deserialization
    // but omit them when serialized back to JSON.
    let preflight = check_artifact_set(&candidates, &context);
    if preflight.iter().any(norn_checker::Diagnostic::is_error) {
        return Err(KernelRefusal::Refused(preflight));
    }
    let checked = check_artifact_set_with_receipt(
        &ReceiptSources::new(&candidate_values, &base_values)
            .map_err(|e| invalid("receipt sources")(&e))?,
        &context,
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| invalid("admission")(&e))?;
    if !checked.receipt.admitted {
        return Err(KernelRefusal::Refused(checked.diagnostics));
    }
    let model = kernel_model(artifacts)?;
    check_principal_catalog(artifacts, &model)?;
    let receipt = checked
        .receipt
        .canonical_json()
        .map_err(|e| invalid("receipt")(&e))?;
    let canonical_set = RuntimeValue::Array(all_values)
        .canonical_json()
        .map_err(|e| invalid("canonical set")(&e))?;
    Ok(AdmittedSet {
        model,
        canonical_set: String::from_utf8(canonical_set).map_err(|e| invalid("set")(&e))?,
        receipt_sha256: checked
            .receipt
            .content_sha256()
            .map_err(|e| invalid("receipt digest")(&e))?,
        receipt: String::from_utf8(receipt).map_err(|e| invalid("receipt")(&e))?,
    })
}

/// PRINCIPAL-INPUT-1 (#1091): a set that declares a principal attribute
/// catalog is admitted only when the catalog agrees with what the set's own
/// access rules use each principal attribute at
/// ([`norn_kernel::authz::admit_principal_catalog`]). A host compiles its
/// action policy from the installed set; a catalog that cannot compile there
/// would leave the cell serving no action, so it is refused here, before
/// anything is installed. A set that declares no catalog is not checked.
/// `model` is the set's model before the kernel's own types are added.
///
/// # Errors
/// The catalog disagrees with the access rules, as one
/// `artifact_set.policy.principal_catalog_conflict` finding.
pub fn check_principal_catalog(
    artifacts: &[ArtifactEnvelope],
    model: &Model,
) -> Result<(), KernelRefusal> {
    fn payload<T: serde::de::DeserializeOwned>(artifact: &ArtifactEnvelope) -> Option<T> {
        serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .ok()
    }
    let mut policies = Vec::new();
    let mut declaring = None;
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Policy)
    {
        if let Some(norn_ir::access::PolicyRoot::Access(access)) = payload(artifact) {
            if declaring.is_none() && !access.principal_catalog.is_empty() {
                declaring = Some(artifact.artifact_id.clone());
            }
            policies.push(access);
        }
    }
    let Some(artifact_id) = declaring else {
        return Ok(());
    };
    let actions: Vec<norn_ir::ActionRoot> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Action)
        .filter_map(payload)
        .collect();
    let object_types: Vec<_> = model.object_types().cloned().collect();
    let interfaces: Vec<_> = model.interfaces().cloned().collect();
    norn_kernel::authz::admit_principal_catalog(&policies, &object_types, &interfaces, &actions)
        .map(|_catalog| ())
        .map_err(|error| {
            KernelRefusal::Refused(vec![norn_checker::Diagnostic {
                code: norn_checker::DiagnosticCode::new(
                    norn_checker::principal_inputs::codes::PRINCIPAL_CATALOG_CONFLICT,
                ),
                severity: norn_ir::DiagnosticSeverity::Error,
                message: format!(
                    "the principal catalog does not agree with the set's access rules: {error}"
                ),
                span: Some(norn_checker::Span::pointer("/payload/principalCatalog")),
                artifact_id: Some(artifact_id),
                remediation: Some(norn_ir::DiagnosticRemediation::FixDefinition),
                witness: None,
            }])
        })
}

/// The normalized model of the set's entity roots and their governing laws.
///
/// # Errors
/// An entity root outside the supported profile, or a model that does not
/// project.
pub fn kernel_model(artifacts: &[ArtifactEnvelope]) -> Result<Model, KernelRefusal> {
    let scope_findings = norn_checker::entity_scopes::check_entity_reference_scopes_all(artifacts);
    if !scope_findings.is_empty() {
        return Err(KernelRefusal::Refused(scope_findings));
    }
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
            // A markings catalog declares no object type. The read path takes
            // it from the retained canonical set (`privacy::catalog_of`), as
            // the checker admitted it, so the model has nothing to hold.
            EntityRoot::V2Markings(_) => {}
            EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
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

/// HOST-REOPEN-1: the model a host serves `artifacts` under, as provisioning
/// installs it and a restarted host rebuilds it from the retained set: the
/// kernel model with the kernel's action receipt and owned-row types. Every
/// definitions install uses it (DEFS-REOPEN-1), so no install commits a
/// layout the next open would refuse.
///
/// # Errors
/// The set's model does not project, or declares a kernel-reserved name.
pub fn serving_model(artifacts: &[ArtifactEnvelope]) -> Result<Model, String> {
    serving_model_keeping(artifacts, false)
}

/// [`serving_model`], keeping the call record's `principalInputs` column when
/// the cell already records it ([`crate::action_admission::Serving`]).
///
/// # Errors
/// As [`serving_model`].
pub fn serving_model_keeping(
    artifacts: &[ArtifactEnvelope],
    keep_principal_inputs: bool,
) -> Result<Model, String> {
    let model = kernel_model(artifacts).map_err(|refusal| match refusal {
        KernelRefusal::Refused(findings) => findings
            .iter()
            .map(|f| format!("{}: {}", f.code, f.message))
            .collect::<Vec<_>>()
            .join("; "),
        KernelRefusal::Invalid(detail) => detail,
    })?;
    crate::action_admission::prepare_action_model_keeping(&model, artifacts, keep_principal_inputs)
        .map_err(|refusal| refusal.message)
}

/// HOST-REOPEN-1 for a restarted host: the serving model of the installed
/// revision whose recorded model hash is `model_hash`. That is
/// [`serving_model`], unless the revision was installed keeping the call
/// record's `principalInputs` column: the recorded hash says which.
///
/// # Errors
/// As [`serving_model`].
pub fn serving_model_installed(
    artifacts: &[ArtifactEnvelope],
    model_hash: &str,
) -> Result<Model, String> {
    let model = serving_model(artifacts)?;
    if model.model_hash() == model_hash {
        return Ok(model);
    }
    let kept = serving_model_keeping(artifacts, true)?;
    Ok(if kept.model_hash() == model_hash {
        kept
    } else {
        model
    })
}

/// Admit `added` over an installed set, as installing it would: the retained
/// artifacts, any of the same id replaced by its new version, re-admitted
/// whole on the base the retained receipt records.
///
/// # Errors
/// The checker's findings, or a retained set or receipt that cannot be read.
pub fn admit_over(
    canonical_set: &str,
    receipt: Option<&str>,
    added: &[ArtifactEnvelope],
) -> Result<AdmittedSet, KernelRefusal> {
    let base = base_ids(receipt).map_err(KernelRefusal::Invalid)?;
    let replaced: BTreeSet<&str> = added.iter().map(|a| a.artifact_id.as_str()).collect();
    let mut artifacts: Vec<ArtifactEnvelope> = retained_artifacts(canonical_set)
        .map_err(KernelRefusal::Invalid)?
        .into_iter()
        .filter(|artifact| !replaced.contains(artifact.artifact_id.as_str()))
        .collect();
    artifacts.extend(added.iter().cloned());
    admit_kernel_set_with_base(&artifacts, &base)
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
