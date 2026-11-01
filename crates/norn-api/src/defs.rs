//! Checker admission and cell-model projection of exact artifact sources.

use std::collections::BTreeSet;

use http::StatusCode;
use norn_cell::Model;
use norn_checker::{
    EntityArtifactSetContext, check_artifact_set_with_receipt, prepare_artifact_source,
};
use norn_ir::{ArtifactEnvelope, EntityRoot, ResourceLimitProfile, RuntimeValue};

use crate::{refusal::Refusal, scope::CellScope};

/// A checked set; construction always runs the shared checker.
pub struct Admitted {
    pub(crate) model: Model,
    pub(crate) layout: String,
    pub(crate) canonical_set: String,
    pub(crate) receipt: String,
    pub(crate) receipt_sha256: String,
}

fn refused(detail: impl std::fmt::Display) -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "defs.not_admitted",
        "the checker refused the candidate definitions",
        detail.to_string(),
    )
}

impl Admitted {
    /// Admit a full artifact array without a floating-point JSON intermediary.
    ///
    /// # Errors
    /// Malformed, out-of-scope, unchecked or unprojectable definitions refuse.
    pub fn from_source(source: &str, scope: &CellScope) -> Result<Self, Refusal> {
        scope.admit(Some(&scope.org_id), Some(&scope.environment_id))?;
        let RuntimeValue::Array(sources) = RuntimeValue::from_json_text(source).map_err(refused)?
        else {
            return Err(refused("definitions must be an artifact array"));
        };
        let mut artifacts: Vec<ArtifactEnvelope> = Vec::new();
        let mut exact = Vec::new();
        let mut layout = norn_ir::LayoutSidecar::default();
        let mut objects = Vec::new();
        let mut properties = Vec::new();
        let mut interfaces = Vec::new();
        for source in sources {
            let prepared = prepare_artifact_source(source)
                .map_err(|diagnostics| refused(norn_checker::diagnostics_to_json(&diagnostics)))?;
            if norn_checker::scope_conflict(&scope.as_scope_ref(), &prepared.artifact.scope)
                .is_some()
            {
                return Err(refused("artifact is outside the cell scope"));
            }
            if prepared.artifact.kind() == norn_ir::ArtifactKind::Entity {
                let payload = prepared
                    .source
                    .get("payload")
                    .ok_or_else(|| refused("missing payload"))?;
                let root: EntityRoot = norn_ir::runtime::decode(payload).map_err(refused)?;
                match root {
                    EntityRoot::V2(root) => objects.push(root.object_type),
                    EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
                    EntityRoot::V2Interface(root) => interfaces.push(root.interface),
                    EntityRoot::V2Extension(_) | EntityRoot::V2Markings(_) => {}
                    EntityRoot::V1(root) => {
                        objects.push(norn_checker::lower_entity_v1(&root).object_type);
                    }
                }
            }
            layout.artifacts.extend(prepared.layout.artifacts);
            artifacts.push(prepared.artifact);
            exact.push(prepared.source);
        }
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let checked = check_artifact_set_with_receipt(
            &artifacts,
            &context,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .map_err(refused)?;
        if !checked.receipt.admitted {
            return Err(refused("artifact set has checker errors"));
        }
        Ok(Self {
            layout: String::from_utf8(
                norn_ir::runtime::encode(&layout)
                    .map_err(refused)?
                    .canonical_json()
                    .map_err(refused)?,
            )
            .map_err(refused)?,
            model: Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
                .map_err(refused)?
                .with_governing_laws(&artifacts).map_err(refused)?,
            canonical_set: String::from_utf8(
                RuntimeValue::Array(exact)
                    .canonical_json()
                    .map_err(refused)?,
            )
            .map_err(refused)?,
            receipt: String::from_utf8(checked.receipt.canonical_json().map_err(refused)?)
                .map_err(refused)?,
            receipt_sha256: checked.receipt.content_sha256().map_err(refused)?,
        })
    }
}
