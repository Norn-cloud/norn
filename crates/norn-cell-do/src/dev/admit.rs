//! Admission of a compiled artifact set by the native checker (the same code
//! as the WASM checker), and the cell model it installs.
//!
//! DEV-ONE-AUTHORITY-1: the compile worker's artifacts are untrusted input.
//! Only what this checker admits reaches [`norn_api::host::DevHost`].
use std::collections::BTreeSet;

use norn_cell::{InstalledDefinitionInput, Model};
use norn_checker::{EntityArtifactSetContext, ReceiptSources};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use norn_ui_checker::check_artifact_set_with_receipt;
use serde_json::{Value, json};

/// A candidate the checker admitted: the model and install input for the
/// next revision.
pub(crate) struct Admitted {
    /// The projected cell model, prepared for governed actions.
    pub(crate) model: Model,
    /// The retained set, receipt and its digest.
    pub(crate) input: InstalledDefinitionInput,
}

/// Why a candidate was not admitted: one diagnostic object per finding,
/// as the checker reports them.
pub(crate) struct Refused {
    pub(crate) diagnostics: Vec<Value>,
}

impl Refused {
    fn one(code: &str, message: impl Into<String>) -> Self {
        Self {
            diagnostics: vec![
                json!({ "code": code, "severity": "error", "message": message.into() }),
            ],
        }
    }
}

/// MAPPING-SOURCE-REF-1: the `sha256` the compiler writes for a mapping
/// bound by reference to a source schema of the same project
/// (`tools/norn-ts` `PENDING_SOURCE_SHA256`).
pub(crate) const PENDING_SOURCE_SHA256: &str = "pending:same-project-source-schema";

/// MAPPING-SOURCE-REF-1: each mapping whose source binding is pending gets
/// the checker's own hash ([`norn_checker::ArtifactRef`]) of the source
/// schema of the set it names. A binding whose schema is not in the set
/// stays pending, and the checker refuses it with the hash it expected.
pub(crate) fn bind_source_schemas(artifacts: &[Value]) -> Vec<Value> {
    let hash_of = |schema_id: &str, version: &str| {
        artifacts
            .iter()
            .filter(|artifact| {
                artifact.get("kind").and_then(Value::as_str) == Some("source_schema")
                    && artifact.get("artifactId").and_then(Value::as_str) == Some(schema_id)
                    && artifact.get("version").and_then(Value::as_str) == Some(version)
            })
            .find_map(|artifact| {
                let envelope: ArtifactEnvelope = serde_json::from_value(artifact.clone()).ok()?;
                norn_checker::ArtifactRef::of(&envelope)
                    .ok()
                    .map(|reference| reference.sha256)
            })
    };
    artifacts
        .iter()
        .map(|artifact| {
            let mut artifact = artifact.clone();
            let pending = artifact.get("kind").and_then(Value::as_str) == Some("mapping")
                && artifact
                    .pointer("/payload/source/sha256")
                    .and_then(Value::as_str)
                    == Some(PENDING_SOURCE_SHA256);
            if pending {
                let text = |pointer: &str| {
                    artifact
                        .pointer(pointer)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let found = hash_of(
                    &text("/payload/source/schemaId"),
                    &text("/payload/source/version"),
                );
                if let (Some(sha256), Some(slot)) =
                    (found, artifact.pointer_mut("/payload/source/sha256"))
                {
                    *slot = Value::String(sha256);
                }
            }
            artifact
        })
        .collect()
}

/// Admit `artifacts` with the shared checker. Any `core.` or `std.` artifact is
/// the retained trusted base, as the cell host treats it.
///
/// `keeps` is whether the live cell already records the call record's
/// `principalInputs` column ([`keeps`]): the candidate model keeps it then,
/// so a set that no longer declares principal inputs still installs.
pub(crate) fn admit(
    artifacts: &[Value],
    installed_at: u64,
    keeps: bool,
) -> Result<Admitted, Refused> {
    let artifacts = &bind_source_schemas(artifacts)[..];
    let envelopes: Vec<ArtifactEnvelope> = serde_json::from_value(Value::Array(artifacts.to_vec()))
        .map_err(|error| {
            Refused::one(
                "norn.dev.artifact_shape",
                format!("the compiled artifacts are not artifact envelopes: {error}"),
            )
        })?;
    let values = artifacts
        .iter()
        .cloned()
        .map(RuntimeValue::from_json)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            Refused::one(
                "norn.dev.artifact_shape",
                format!("an artifact is not exact JSON: {error}"),
            )
        })?;
    let trusted = |envelope: &ArtifactEnvelope| {
        envelope.artifact_id.starts_with("core.") || envelope.artifact_id.starts_with("std.")
    };
    let (mut base, mut candidates, mut base_values, mut candidate_values) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (envelope, value) in envelopes.iter().zip(values.iter()) {
        if trusted(envelope) {
            base.push(envelope.clone());
            base_values.push(value.clone());
        } else {
            candidates.push(envelope.clone());
            candidate_values.push(value.clone());
        }
    }
    let sources = ReceiptSources::new(&candidate_values, &base_values)
        .map_err(|error| Refused::one("norn.dev.receipt", error.to_string()))?;
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|error| Refused::one("norn.dev.receipt", error.to_string()))?;
    if !checked.receipt.admitted {
        return Err(Refused {
            diagnostics: checked
                .diagnostics
                .iter()
                .map(norn_checker::Diagnostic::to_json)
                .collect(),
        });
    }
    let definitions = checked_definitions(&envelopes)?;
    let text = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .map_err(|error| Refused::one("norn.dev.receipt", error.to_string()))
    };
    let receipt = text(
        checked
            .receipt
            .canonical_json()
            .map_err(|error| Refused::one("norn.dev.receipt", error.to_string()))?,
    )?;
    let canonical_set = text(
        RuntimeValue::Array(values)
            .canonical_json()
            .map_err(|error| Refused::one("norn.dev.receipt", error.to_string()))?,
    )?;
    let input = InstalledDefinitionInput {
        canonical_set,
        registry: vec![],
        receipt_sha256: checked
            .receipt
            .content_sha256()
            .map_err(|error| Refused::one("norn.dev.receipt", error.to_string()))?,
        admission_receipt: Some(receipt),
        readmission: None,
        diff: json!({}),
        installed_at,
        layout: None,
    };
    let model = serving(&definitions, &envelopes, keeps)
        .map_err(|message| Refused::one("norn.dev.model", message))?;
    Ok(Admitted { model, input })
}

/// The model of the admitted set's own definitions. As the hosted admission
/// does, a principal catalog the action policy could not compile is refused
/// here, before anything is installed.
fn checked_definitions(envelopes: &[ArtifactEnvelope]) -> Result<Model, Refused> {
    use norn_api::kernel_set::{KernelRefusal, check_principal_catalog};
    let definitions =
        definitions_model(envelopes).map_err(|message| Refused::one("norn.dev.model", message))?;
    check_principal_catalog(envelopes, &definitions).map_err(|refusal| match refusal {
        KernelRefusal::Refused(findings) => Refused {
            diagnostics: findings
                .iter()
                .map(norn_checker::Diagnostic::to_json)
                .collect(),
        },
        KernelRefusal::Invalid(detail) => Refused::one("norn.dev.model", detail),
    })?;
    Ok(definitions)
}

/// Whether the cell `serving` holds already records the call record's
/// `principalInputs` column. Every later revision keeps it (PRINCIPAL-INPUT-1).
pub(crate) fn keeps(serving: &super::http::Serving) -> bool {
    serving.read().is_ok_and(|guard| {
        guard
            .as_ref()
            .and_then(|host| host.serving().ok())
            .is_some_and(|served| served.principal_inputs)
    })
}

/// HOST-REOPEN-1: the model a restarted dev host reopens `active` under: the
/// one its recorded model hash names, with or without the kept
/// `principalInputs` column.
pub(crate) fn installed_model(active: &norn_cell::InstalledDefinitionSet) -> Result<Model, String> {
    let artifacts = retained_envelopes(&active.canonical_set)?;
    let definitions = definitions_model(&artifacts)?;
    let model = serving(&definitions, &artifacts, false)?;
    if model.model_hash() == active.model_hash {
        return Ok(model);
    }
    let kept = serving(&definitions, &artifacts, true)?;
    Ok(if kept.model_hash() == active.model_hash {
        kept
    } else {
        model
    })
}

/// `definitions` with the kernel's action receipt and owned-row types, and
/// the call record's `principalInputs` column when the set declares principal
/// inputs or the cell already `keeps` it.
fn serving(
    definitions: &Model,
    artifacts: &[ArtifactEnvelope],
    keeps: bool,
) -> Result<Model, String> {
    norn_api::action_admission::prepare_action_model_keeping(definitions, artifacts, keeps)
        .map_err(|refusal| refusal.message)
}

/// The cell model of an admitted set's entity roots and governing laws,
/// before the kernel's own types are added.
fn definitions_model(artifacts: &[ArtifactEnvelope]) -> Result<Model, String> {
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|error| format!("{}: {error}", artifact.artifact_id))?;
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            // A markings catalog declares no object type; reads take it from
            // the retained set, as the hosted path does.
            EntityRoot::V2Markings(_) => {}
            EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                return Err(format!(
                    "{} uses an entity form the dev host does not serve yet (extensions or EntityIR v1)",
                    artifact.artifact_id
                ));
            }
        }
    }
    Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .and_then(|model| model.with_governing_laws(artifacts))
        .map_err(|error| error.to_string())
}

/// The envelopes of a retained canonical set.
pub(crate) fn retained_envelopes(canonical_set: &str) -> Result<Vec<ArtifactEnvelope>, String> {
    serde_json::from_str(canonical_set)
        .map_err(|error| format!("the retained set does not decode: {error}"))
}

/// Attach `source` (project-relative file and line) to each checker
/// diagnostic whose artifact and JSON pointer the compile's source map names:
/// the longest matching pointer wins.
pub(crate) fn source_mapped(diagnostics: Vec<Value>, source_map: &[Value]) -> Vec<Value> {
    diagnostics
        .into_iter()
        .map(|mut diagnostic| {
            let artifact = diagnostic
                .get("artifactId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let pointer = diagnostic
                .get("span")
                .and_then(|span| span.get("pointer"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let found = source_map
                .iter()
                .filter(|entry| {
                    entry.get("artifactId").and_then(Value::as_str) == Some(artifact.as_str())
                })
                .filter_map(|entry| {
                    let at = entry.get("pointer").and_then(Value::as_str)?;
                    let matches = pointer == at
                        || pointer
                            .strip_prefix(at)
                            .is_some_and(|rest| rest.starts_with('/'));
                    matches.then_some((at.len(), entry.get("at")?))
                })
                .max_by_key(|(length, _)| *length);
            if let (Some((_, at)), Some(object)) = (found, diagnostic.as_object_mut()) {
                object.insert("source".to_owned(), at.clone());
            }
            diagnostic
        })
        .collect()
}

#[cfg(test)]
#[path = "principal_inputs_tests.rs"]
mod principal_inputs_tests;
