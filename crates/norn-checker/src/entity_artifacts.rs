//! The `EntityIR` v2 artifact-set check (T1-ONT, #2844): the whole-model
//! check [`crate::check_entity_model`] over a set of `entity` artifacts.
//!
//! An `entity` artifact in the `EntityIR` v2 form carries exactly one
//! definition (see [`norn_ir::EntityRoot`]): an object type (`objectType`), an
//! interface (`interface`), an extension of another layer's object type
//! (`objectTypeExtension`), or a catalog of shared property types
//! (`propertyTypes`). The per-artifact (T2) check decodes each one and runs
//! its per-definition rules. Rules that resolve across definitions —
//! supertypes, interfaces, link targets and inverse roles, `ref`s into the
//! property type catalog, and extension targets — run here, over one
//! **layer** of artifacts checked against its installed **base layer**.
//!
//! # Layers
//!
//! The caller names the layer being checked. The base layer is the registry
//! packages (Norn Core, domain packages) the layer builds on: they are
//! read-only, already checked, and only resolve names here. A tenant layer is
//! checked as a [`TenantLayer`]: the tenant's id and its
//! [`ExtensionNamespace`] (allocated by the registry, T8-REG-4). An artifact
//! states which layer it belongs to through the envelope's existing
//! `scope.tenantId`: every artifact of a tenant layer is scoped to exactly
//! that layer's tenant id, and no artifact of a registry layer is tenant
//! scoped. A disagreement is [`codes::LAYER_SCOPE_MISMATCH`].
//!
//! # Findings
//!
//! Every finding carries the owning artifact's id and a JSON pointer into
//! that artifact's envelope (`/payload/objectType/links/0/target`,
//! `/payload/propertyTypes/2/type/name`).
//!
//! [`check_artifact_set_entity_model`] is fail-closed on its own: it reports
//! every finding of the whole-model check, including per-definition ones the
//! per-artifact check also reports, and it always reports an `EntityIR` v2
//! artifact whose payload does not decode (for example one with no
//! definition member) as `entity.validation.invalid_payload` at `/payload`;
//! such an artifact takes no part in the model. v1 `entity` artifacts and
//! other kinds are not part of the v2 model and are skipped; consumers read v1
//! through [`crate::entity_lowering`] (T1-ONT-4, #2517), and making lowered v1
//! types resolvable here is #2848.
//!
//! **Recommended API:** [`check_entity_artifacts`] runs the per-artifact
//! check ([`crate::check_artifact`]) of every artifact in the layer plus the
//! set check, and removes findings identical in `(code, artifactId,
//! pointer)`, so each finding is reported once.
//!
//! This is the entity-model slice of artifact-set checking. Resolving the set
//! itself — dependency and version closure, duplicate artifact identities,
//! kind compatibility of references — is [`crate::artifact_set`] (T1-CHK-5,
//! #2387); [`crate::check_artifact_set`] resolves a set and then runs
//! [`check_entity_artifacts`] over it.

use std::collections::BTreeSet;

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, DiagnosticRemediation, DiagnosticSeverity, EntityRoot,
    ExtensionNamespace, InterfaceDef, ObjectTypeDef, ObjectTypeExtension, PropertyTypeDef,
    PropertyTypeName,
};

use crate::entity_model::{EntityContext, EntityModel, check_entity_model};
use crate::{Diagnostic, DiagnosticCode, Span};

/// Diagnostic codes emitted by [`check_artifact_set_entity_model`].
pub mod codes {
    /// An artifact's `scope.tenantId` disagrees with the layer it is checked
    /// in: a tenant layer's artifact is not scoped to the layer's one tenant,
    /// or a registry layer's artifact is tenant scoped.
    pub const LAYER_SCOPE_MISMATCH: &str = "entity.validation.layer_scope_mismatch";
}

const INVALID_PAYLOAD: &str = "entity.validation.invalid_payload";

/// What an entity artifact set is checked against.
#[derive(Debug, Clone, Copy)]
pub struct EntityArtifactSetContext<'a> {
    /// Registry (Norn Core) value type names a `ref` may name.
    pub registry: &'a BTreeSet<PropertyTypeName>,
    /// The installed, read-only base layer: its `EntityIR` v2 artifacts
    /// resolve names but are not re-checked (an undecodable one is ignored).
    pub base: &'a [ArtifactEnvelope],
    /// `Some` when the checked artifacts are a tenant layer; `None` for a
    /// registry (base) layer.
    pub tenant: Option<TenantLayer<'a>>,
}

/// The identity of a tenant layer: every artifact of the layer is scoped to
/// `tenant_id`, and every member it adds is named in `namespace`.
#[derive(Debug, Clone, Copy)]
pub struct TenantLayer<'a> {
    /// The tenant every artifact's `scope.tenantId` must equal.
    pub tenant_id: &'a str,
    /// The tenant's extension namespace.
    pub namespace: &'a ExtensionNamespace,
}

/// Check one layer of `EntityIR` v2 `entity` artifacts as a whole model
/// against `context`. Returns every finding of the whole-model check, located
/// in its artifact; fail-closed on its own (an undecodable v2 artifact is
/// always reported). Prefer [`check_entity_artifacts`], which adds the
/// per-artifact checks and reports each finding once. See the module docs.
#[must_use]
pub fn check_artifact_set_entity_model(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let mut out = check_layer_scopes(artifacts, context.tenant.map(|tenant| tenant.tenant_id));

    let mut local = Layer::default();
    let mut findings = Vec::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        match decode_v2(artifact) {
            Decoded::NotV2 => {}
            Decoded::Invalid => {
                findings.push(finding(
                INVALID_PAYLOAD,
                "EntityIR v2 payload does not decode into one definition, so it takes no part in the artifact-set check; see its per-artifact findings"
                    .to_owned(),
                artifact,
                "/payload",
                ));
            }
            Decoded::Root(root) => local.push(index, *root),
        }
    }
    let mut base = Layer::default();
    for (index, artifact) in context.base.iter().enumerate() {
        if let Decoded::Root(root) = decode_v2(artifact) {
            base.push(index, *root);
        }
    }

    // The catalog: base definitions first, so a local definition that
    // repeats a base name is the one reported.
    let catalog: Vec<PropertyTypeDef> = base
        .catalog
        .iter()
        .chain(&local.catalog)
        .map(|(_, def)| def.clone())
        .collect();
    let entity_context = EntityContext {
        catalog: &catalog,
        registry: context.registry,
        base: base.as_model(),
        tenant: context.tenant.map(|tenant| tenant.namespace),
    };
    findings.extend(catalog_findings(
        &catalog, context, &base, &local, artifacts,
    ));
    findings.extend(model_findings(&entity_context, &local, artifacts));

    out.extend(findings);
    out
}

/// Check one layer of artifacts completely: the per-artifact check
/// ([`crate::check_artifact`]) of every artifact in `artifacts`, then
/// [`check_artifact_set_entity_model`] over the layer. Findings identical in
/// `(code, artifactId, pointer)` are reported once (the first occurrence,
/// per-artifact findings first). An empty result means every artifact and
/// the layer as a whole are valid. This is the recommended entry point; the
/// base layer in `context` is not re-checked.
#[must_use]
pub fn check_entity_artifacts(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    dedupe_findings(
        artifacts
            .iter()
            .flat_map(crate::check_artifact)
            .chain(check_artifact_set_entity_model(artifacts, context))
            .chain(crate::markings::check_references(artifacts, context.base)),
    )
}

/// Keep the first of the findings identical in `(code, artifactId,
/// pointer)`, preserving order. UI-law findings also include their message
/// in the identity, because distinct effects and composed render paths may
/// share the same real JSON pointer.
pub(crate) fn dedupe_findings(findings: impl IntoIterator<Item = Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = BTreeSet::new();
    findings
        .into_iter()
        .filter(|diagnostic| {
            seen.insert((
                diagnostic.artifact_id.clone(),
                diagnostic.code.as_str().to_owned(),
                diagnostic
                    .span
                    .as_ref()
                    .map(|span| span.pointer.clone())
                    .unwrap_or_default(),
                (diagnostic.code.as_str() == "widget.validation.invalid_payload"
                    && diagnostic.message.starts_with("UI law:"))
                .then(|| diagnostic.message.clone()),
            ))
        })
        .collect()
}

/// Findings of the property type catalog check on the layer's own
/// definitions (`catalog` is the base layer's definitions, then `local`'s),
/// located in their artifacts.
fn catalog_findings(
    catalog: &[PropertyTypeDef],
    context: &EntityArtifactSetContext<'_>,
    base: &Layer,
    local: &Layer,
    artifacts: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let base_catalog = base.catalog.len();
    let mut findings = Vec::new();
    for mut diagnostic in crate::check_property_type_defs(catalog, context.registry) {
        let Some((position, rest)) = diagnostic.span.as_ref().and_then(|span| {
            split_index(&span.pointer).map(|(position, rest)| (position, rest.to_owned()))
        }) else {
            continue;
        };
        // Base definitions are already checked.
        let Some((artifact, member, def)) = position
            .checked_sub(base_catalog)
            .and_then(|position| local.catalog.get(position))
            .and_then(|((artifact, member), def)| Some((artifacts.get(*artifact)?, member, def)))
        else {
            continue;
        };
        // The catalog check names the first definition by its index in the
        // combined catalog; name its artifact instead.
        if diagnostic.code == crate::property_types::codes::DUPLICATE_NAME
            && let Some(first) = catalog.iter().position(|other| other.name == def.name)
            && first < position
        {
            let owner = match first.checked_sub(base_catalog) {
                None => base
                    .catalog
                    .get(first)
                    .and_then(|((artifact, _), _)| context.base.get(*artifact)),
                Some(first) => local
                    .catalog
                    .get(first)
                    .and_then(|((artifact, _), _)| artifacts.get(*artifact)),
            };
            if let Some(owner) = owner {
                diagnostic.message = format!(
                    "property type {:?} is already defined by artifact {:?}",
                    def.name.as_str(),
                    owner.artifact_id
                );
            }
        }
        relocate(
            &mut diagnostic,
            artifact,
            &format!("/payload/propertyTypes/{member}{rest}"),
        );
        findings.push(diagnostic);
    }

    findings
}

/// Findings of the model check, located in the artifacts that own them.
fn model_findings(
    entity_context: &EntityContext<'_>,
    local: &Layer,
    artifacts: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for mut diagnostic in check_entity_model(&local.as_model(), entity_context) {
        let target = diagnostic.span.as_ref().and_then(|span| {
            let (root, rest) = span.pointer.strip_prefix('/')?.split_once('/')?;
            let (position, rest) = split_index(&format!("/{rest}"))
                .map(|(position, rest)| (position, rest.to_owned()))?;
            let (owners, member) = match root {
                "objectTypes" => (&local.object_type_owners, "objectType"),
                "interfaces" => (&local.interface_owners, "interface"),
                "extensions" => (&local.extension_owners, "objectTypeExtension"),
                _ => return None,
            };
            let artifact = artifacts.get(*owners.get(position)?)?;
            Some((artifact, format!("/payload/{member}{rest}")))
        });
        if let Some((artifact, pointer)) = target {
            relocate(&mut diagnostic, artifact, &pointer);
        }
        findings.push(diagnostic);
    }

    findings
}

/// Check a catalog of shared property types on its own: the per-definition
/// (T2) subset of [`crate::check_property_type_defs`] for an `EntityIR` v2
/// `propertyTypes` entity artifact (#2844). A `ref` to a property type of
/// another catalog, or to a registry value type, resolves only in the
/// artifact-set check, so `unknown_ref` is dropped here. Spans are JSON
/// pointers into the catalog array (`/0/type/name`).
#[must_use]
pub fn check_property_type_catalog_local(defs: &[PropertyTypeDef]) -> Vec<Diagnostic> {
    crate::check_property_type_defs(defs, &BTreeSet::new())
        .into_iter()
        .filter(|finding| finding.code != crate::property_types::codes::UNKNOWN_REF)
        .collect()
}

/// `scope.tenantId` of every `EntityIR` v2 artifact against the layer's
/// tenant id (`None` for a registry layer).
fn check_layer_scopes(
    artifacts: &[ArtifactEnvelope],
    layer_tenant: Option<&str>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for artifact in artifacts.iter().filter(|artifact| is_v2(artifact)) {
        let tenant_id = artifact.scope.tenant_id.as_deref();
        let problem = match (layer_tenant, tenant_id) {
            (None, Some(tenant_id)) => Some(format!(
                "artifact is scoped to tenant {tenant_id:?} but is checked in a registry layer; registry definitions are not tenant scoped"
            )),
            (Some(expected), None) => Some(format!(
                "artifact has no scope.tenantId but is checked in the tenant layer of {expected:?}; every tenant-layer definition is scoped to the tenant"
            )),
            (Some(expected), Some(tenant_id)) if expected != tenant_id => Some(format!(
                "artifact is scoped to tenant {tenant_id:?} but is checked in the tenant layer of {expected:?}"
            )),
            (Some(_), Some(_)) | (None, None) => None,
        };
        if let Some(message) = problem {
            let pointer = if tenant_id.is_some() {
                "/scope/tenantId"
            } else {
                "/scope"
            };
            out.push(finding(
                codes::LAYER_SCOPE_MISMATCH,
                message,
                artifact,
                pointer,
            ));
        }
    }
    out
}

/// The definitions of one layer, each with the index of its artifact.
#[derive(Default)]
pub(crate) struct Layer {
    pub(crate) object_types: Vec<ObjectTypeDef>,
    pub(crate) object_type_owners: Vec<usize>,
    pub(crate) interfaces: Vec<InterfaceDef>,
    pub(crate) interface_owners: Vec<usize>,
    pub(crate) extensions: Vec<ObjectTypeExtension>,
    pub(crate) extension_owners: Vec<usize>,
    /// `((artifact, position in its propertyTypes), definition)`.
    pub(crate) catalog: Vec<((usize, usize), PropertyTypeDef)>,
}

impl Layer {
    pub(crate) fn push(&mut self, artifact: usize, root: EntityRoot) {
        match root {
            EntityRoot::V2(root) => {
                self.object_types.push(root.object_type);
                self.object_type_owners.push(artifact);
            }
            EntityRoot::V2Interface(root) => {
                self.interfaces.push(root.interface);
                self.interface_owners.push(artifact);
            }
            EntityRoot::V2Extension(root) => {
                self.extensions.push(root.object_type_extension);
                self.extension_owners.push(artifact);
            }
            EntityRoot::V2PropertyTypes(root) => {
                self.catalog.extend(
                    root.property_types
                        .into_iter()
                        .enumerate()
                        .map(|(position, def)| ((artifact, position), def)),
                );
            }
            EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }

    pub(crate) const fn as_model(&self) -> EntityModel<'_> {
        EntityModel {
            interfaces: self.interfaces.as_slice(),
            object_types: self.object_types.as_slice(),
            extensions: self.extensions.as_slice(),
        }
    }
}

pub(crate) enum Decoded {
    /// Not an `EntityIR` v2 `entity` artifact.
    NotV2,
    /// An `EntityIR` v2 payload that does not decode.
    Invalid,
    Root(Box<EntityRoot>),
}

fn is_v2(artifact: &ArtifactEnvelope) -> bool {
    matches!(&artifact.payload, ArtifactPayload::Entity(payload) if payload.is_entity_ir_v2())
}

pub(crate) fn decode_v2(artifact: &ArtifactEnvelope) -> Decoded {
    let ArtifactPayload::Entity(payload) = &artifact.payload else {
        return Decoded::NotV2;
    };
    if !payload.is_entity_ir_v2() {
        return Decoded::NotV2;
    }
    match serde_json::to_value(payload.to_json_value()).and_then(serde_json::from_value) {
        Ok(
            root @ (EntityRoot::V2(_)
            | EntityRoot::V2Interface(_)
            | EntityRoot::V2Extension(_)
            | EntityRoot::V2PropertyTypes(_)
            | EntityRoot::V2Markings(_)),
        ) => Decoded::Root(Box::new(root)),
        Ok(EntityRoot::V1(_)) | Err(_) => Decoded::Invalid,
    }
}

/// Split `/<index>/rest` into the index and `/rest` (possibly empty).
fn split_index(pointer: &str) -> Option<(usize, &str)> {
    let rest = pointer.strip_prefix('/')?;
    let index_len = rest.find('/').unwrap_or(rest.len());
    let (index, rest) = rest.split_at_checked(index_len)?;
    Some((index.parse().ok()?, rest))
}

fn relocate(diagnostic: &mut Diagnostic, artifact: &ArtifactEnvelope, pointer: &str) {
    diagnostic.artifact_id = Some(artifact.artifact_id.clone());
    diagnostic.span = Some(Span::pointer(pointer));
}

fn finding(code: &str, message: String, artifact: &ArtifactEnvelope, pointer: &str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[cfg(test)]
mod tests {
    use super::dedupe_findings;
    use crate::{Diagnostic, DiagnosticCode, DiagnosticSeverity, Span};

    fn diagnostic(code: &str, message: &str) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::new(code),
            severity: DiagnosticSeverity::Error,
            message: message.to_owned(),
            span: Some(Span::pointer("/payload/ux/pages/0/root")),
            artifact_id: Some("test.widget".to_owned()),
            remediation: None,
        }
    }

    #[test]
    fn ui_laws_preserve_distinct_messages_but_deduplicate_identical_findings() {
        let external = diagnostic(
            "widget.validation.invalid_payload",
            "UI law: action test.action has unhandled external at render path root",
        );
        let money = diagnostic(
            "widget.validation.invalid_payload",
            "UI law: action test.action has unhandled money(currency.USD) at render path root",
        );
        let found = dedupe_findings([external.clone(), money.clone(), external]);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|finding| finding.message == money.message));
    }

    #[test]
    fn other_findings_still_deduplicate_by_code_artifact_and_pointer() {
        let first = diagnostic("widget.validation.invalid_payload", "unknown placement");
        let second = diagnostic("widget.validation.invalid_payload", "unknown binding");
        let found = dedupe_findings([first.clone(), second]);
        assert_eq!(found.len(), 1);
        assert!(found.iter().all(|finding| finding.message == first.message));
    }
}
