//! Checker admission and cell-model projection of exact artifact sources.

use std::collections::BTreeSet;

use http::StatusCode;
use norn_cell::Model;
use norn_checker::{
    EntityArtifactSetContext, check_artifact_set_with_receipt, prepare_artifact_source,
};
use norn_ir::{EntityRoot, ResourceLimitProfile, RuntimeValue};

use crate::{refusal::Refusal, scope::CellScope};

/// A checked set; construction always runs the shared checker.
pub struct Admitted {
    pub(crate) model: Model,
    pub(crate) diagnostics: serde_json::Value,
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
        let mut exact = Vec::new();
        let mut artifacts = Vec::new();
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
            artifacts.push(prepared.artifact);
            layout.artifacts.extend(prepared.layout.artifacts);
            exact.push(prepared.source);
        }
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let sources = norn_checker::ReceiptSources::new(&exact, &[]).map_err(refused)?;
        let checked = check_artifact_set_with_receipt(
            &sources,
            &context,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .map_err(refused)?;
        if !checked.receipt.admitted {
            return Err(refused("artifact set has checker errors"));
        }
        for internal in action_system_types() {
            if objects.iter().any(|ty| {
                ty.name
                    .as_str()
                    .eq_ignore_ascii_case(internal.name.as_str())
            }) {
                return Err(refused("reserved kernel object type"));
            }
            objects.push(internal);
        }
        Ok(Self {
            diagnostics: norn_checker::diagnostics_to_json(&checked.diagnostics),
            layout: String::from_utf8(
                norn_ir::runtime::encode(&layout)
                    .map_err(refused)?
                    .canonical_json()
                    .map_err(refused)?,
            )
            .map_err(refused)?,
            model: Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
                .map_err(refused)?
                .with_governing_laws(&artifacts)
                .map_err(refused)?,
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

fn action_system_types() -> Vec<norn_ir::ObjectTypeDef> {
    use norn_ir::{ObjectTypeDef, ObjectTypeName, Property, PropertyId, PropertyType};
    let text = PropertyType::Text { format: None };
    let json = PropertyType::JsonExt { schema: None };
    let mut types = Vec::new();
    for (name, fields) in [
        (
            norn_kernel::ACTION_CALL_TYPE,
            vec![
                ("actionId", text.clone()),
                ("actionArtifactId", text.clone()),
                ("actionVersion", text.clone()),
                ("effectKind", text.clone()),
                ("object", json.clone()),
                ("input", json.clone()),
                ("effectiveInput", json.clone()),
                ("derivedValues", json.clone()),
                ("preconditionOutcomes", json.clone()),
                ("actor", text.clone()),
                ("turn", text.clone()),
                ("transition", text.clone()),
                ("from", text.clone()),
                ("to", text.clone()),
                ("rowOperations", json.clone()),
            ],
        ),
        (
            norn_kernel::OWNED_ROWS_TYPE,
            vec![
                ("parentType", text.clone()),
                ("parentKey", text.clone()),
                ("collectionId", text),
                ("rowIds", json),
            ],
        ),
    ] {
        types.push(ObjectTypeDef {
            role: None,
            name: ObjectTypeName(name.into()),
            description: None,
            status: norn_ir::ResourceStatus::default(),
            extends: None,
            implements: vec![],
            properties: fields
                .into_iter()
                .map(|(id, property_type)| Property {
                    id: PropertyId(id.into()),
                    description: None,
                    property_type,
                    constraints: norn_ir::PropertyConstraints {
                        nullable: true,
                        ..Default::default()
                    },
                    default: None,
                    markings: vec![],
                    computed: None,
                    protected: true,
                })
                .collect(),
            links: vec![],
            owned_collections: vec![],
            identity: vec![],
            invariants: vec![],
            lifecycles: vec![],
            markings: vec![],
            accounting_leg: None,
            immutable: false,
            controls: None,
        });
    }
    types
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "reserved schema conformance")]
mod tests {
    use super::*;

    #[test]
    fn reserved_kernel_types_refuse_sqlite_equivalent_tenant_names() {
        let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_str(include_str!(
            "../../../fixtures/engine-alpha/sales-order.definitions.json"
        ))
        .expect("authored declarations");
        let scope = CellScope {
            org_id: "org_alpha".into(),
            environment_id: "dev".into(),
        };
        let original = serde_json::to_string(&artifacts[..1]).expect("original source");
        assert!(Admitted::from_source(&original, &scope).is_ok());
        for name in ["norn.ActionCall", "norn.Actioncall", "norn.OWNEDROWS"] {
            let mut artifact = artifacts.first().expect("customer").clone();
            let mut root: norn_ir::EntityRootV2 = serde_json::from_value(
                serde_json::to_value(artifact.payload.to_json_value()).expect("entity"),
            )
            .expect("typed entity");
            root.object_type.name = norn_ir::ObjectTypeName(name.into());
            artifact.payload = norn_ir::ArtifactPayload::Entity(
                serde_json::from_value(serde_json::to_value(root).expect("entity"))
                    .expect("payload"),
            );
            let source = serde_json::to_string(&vec![artifact]).expect("typed source");
            let refusal = Admitted::from_source(&source, &scope)
                .err()
                .expect("reserved name");
            assert_eq!(refusal.code, "defs.not_admitted");
            assert_eq!(refusal.explanation, "reserved kernel object type");
        }
    }
}
