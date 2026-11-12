//! Admission of a compiled artifact set by the native checker (the same code
//! as the WASM checker), and the cell model it installs.
//!
//! DEV-ONE-AUTHORITY-1: the compile worker's artifacts are untrusted input.
//! Only what this checker admits reaches [`norn_api::host::DevHost`].
use std::collections::BTreeSet;

use norn_cell::{InstalledDefinitionInput, Model};
use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
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

/// Admit `artifacts` with the shared checker. Any `core.` or `std.` artifact is
/// the retained trusted base, as the cell host treats it.
pub(crate) fn admit(artifacts: &[Value], installed_at: u64) -> Result<Admitted, Refused> {
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
    let model = model_of(&envelopes).map_err(|message| Refused::one("norn.dev.model", message))?;
    Ok(Admitted { model, input })
}

/// The cell model of an admitted set: its entity roots and governing laws,
/// with the kernel's action receipt and owned-row types. A reopened cell
/// rebuilds the same model from its retained set (HOST-REOPEN-1).
pub(crate) fn model_of(artifacts: &[ArtifactEnvelope]) -> Result<Model, String> {
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
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                return Err(format!(
                    "{} uses an entity form the dev host does not serve yet (markings, extensions or EntityIR v1)",
                    artifact.artifact_id
                ));
            }
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .and_then(|model| model.with_governing_laws(artifacts))
        .map_err(|error| error.to_string())?;
    norn_api::action_admission::prepare_action_model(&model).map_err(|refusal| refusal.message)
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
