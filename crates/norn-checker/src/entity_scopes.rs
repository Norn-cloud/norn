//! Consumer-scoped entity resolution and cross-scope entity reference checks (#1376).
//!
//! Every `EntityIR` v2 definition (object type, supertype, interface,
//! extension, shared property type catalog) carries its enclosing
//! [`ArtifactEnvelope::scope`]. A consumer in scope `S` may observe only
//! definitions whose scope satisfies [`scope_visible`] — i.e.
//! [`crate::artifact_set::scope_conflict`] returns `None` across every scope
//! dimension (`orgId`, `tenantId`, `workspaceId`, `environmentId`,
//! `businessUnitId`). Admission resolves names only through a [`ScopedView`];
//! the whole-set catalog and environment builders are crate-private
//! inventory and historical receipt verification.

mod refs;
mod view;

use std::collections::{BTreeMap, BTreeSet};

use norn_canonical::CanonicalJsonError;
use norn_expr::{EnvError, TypeEnvRef};
use norn_ir::{
    ArtifactEnvelope, EntityRoot, JsonValue, PropertyTypeName, ResourceLimitProfile, ScopeRef,
};

use crate::artifact_set::{scope_conflict, scope_finding};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::entity_instance::{EffectiveCatalog, EffectiveObjectType};
use crate::{Diagnostic, Span};

pub(crate) use refs::check_entity_reference_scopes;
pub use refs::check_entity_reference_scopes_all;
pub(crate) use view::definitions_env;
pub use view::{ScopedView, ScopedViews};

pub use norn_expr::env::{SCOPED_TYPE_ENV_ID, SCOPED_TYPE_ENV_VERSION};

pub(crate) type ScopeKey = [Option<String>; 5];

pub(crate) fn scope_key(scope: &ScopeRef) -> ScopeKey {
    [
        scope.org_id.clone(),
        scope.tenant_id.clone(),
        scope.workspace_id.clone(),
        scope.environment_id.clone(),
        scope.business_unit_id.clone(),
    ]
}

/// Whether a consumer in `consumer` may observe an artifact in `target`
/// across all five scope dimensions.
#[must_use]
pub fn scope_visible(consumer: &ScopeRef, target: &ScopeRef) -> bool {
    scope_conflict(consumer, target).is_none()
}

impl EffectiveCatalog {
    /// The inventory catalog of `artifacts`: each object type resolved
    /// against what its own declaring scope sees.
    #[must_use]
    pub(crate) fn from_artifacts(artifacts: &[ArtifactEnvelope]) -> Self {
        Self::from_artifacts_with_registry(artifacts, &BTreeSet::new())
    }

    /// [`Self::from_artifacts`] against the registry value type names `registry`.
    #[must_use]
    pub(crate) fn from_artifacts_with_registry(
        artifacts: &[ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        if has_narrower_extension(artifacts) {
            return catalog_by_declaring_scope(artifacts, registry);
        }
        Self::build_single_layer(artifacts, registry)
    }
}

/// Build an [`EffectiveCatalog`] where each `EntityIR` v2 object type is
/// resolved strictly against artifacts visible from its own declaring scope.
pub(crate) fn catalog_by_declaring_scope(
    artifacts: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> EffectiveCatalog {
    let mut out = EffectiveCatalog::default();
    let mut by_scope: BTreeMap<ScopeKey, (ScopeRef, Vec<&str>)> = BTreeMap::new();
    for artifact in artifacts {
        if let Decoded::Root(root) = decode_v2(artifact) {
            match *root {
                EntityRoot::V2(_) => {
                    by_scope
                        .entry(scope_key(&artifact.scope))
                        .or_insert_with(|| (artifact.scope.clone(), Vec::new()))
                        .1
                        .push(artifact.artifact_id.as_str());
                }
                EntityRoot::V2Interface(iface) => {
                    out.interface_names
                        .insert(iface.interface.name.as_str().to_owned());
                }
                EntityRoot::V2Extension(_)
                | EntityRoot::V2PropertyTypes(_)
                | EntityRoot::V2Markings(_)
                | EntityRoot::V1(_) => {}
            }
        }
    }
    let cross_findings = check_cross_scope_duplicates(artifacts, &[]);
    for (_, (scope, ids)) in by_scope {
        let view = ScopedView::bound(&scope, artifacts, &[], registry);
        let scoped = view.catalog();
        for id in ids {
            let Some(entry) = scoped.by_artifact.get(id) else {
                continue;
            };
            let blocked = cross_findings
                .iter()
                .find(|finding| finding.artifact_id.as_deref() == Some(id));
            let result = match (blocked, entry) {
                (None, Ok(object_type)) => {
                    let name = object_type.name.as_str().to_owned();
                    out.by_name
                        .entry(name.clone())
                        .or_insert_with(|| id.to_owned());
                    out.lineages
                        .entry(name.clone())
                        .or_insert_with(|| object_type.lineage.clone());
                    if let Some(links) = scoped.links_by_name.get(&name) {
                        out.links_by_name
                            .entry(name)
                            .or_insert_with(|| links.clone());
                    }
                    Ok(object_type.clone())
                }
                (Some(finding), _) | (None, Err(finding)) => Err(finding.clone()),
            };
            out.by_artifact.entry(id.to_owned()).or_insert(result);
        }
    }
    out.add_v1_artifacts(artifacts);
    out
}

/// Whether `artifacts` contains any `EntityIR` v2 extension whose scope is
/// strictly narrower than (or not visible from) its target object type's scope.
#[must_use]
pub(crate) fn has_narrower_extension(artifacts: &[ArtifactEnvelope]) -> bool {
    let target_scopes = v2_object_type_scopes(artifacts);
    artifacts.iter().any(|artifact| {
        let Decoded::Root(root) = decode_v2(artifact) else {
            return false;
        };
        let EntityRoot::V2Extension(ext) = *root else {
            return false;
        };
        target_scopes
            .get(ext.object_type_extension.target.as_str())
            .is_some_and(|target_scope| !scope_visible(target_scope, &artifact.scope))
    })
}

fn v2_object_type_scopes(artifacts: &[ArtifactEnvelope]) -> BTreeMap<String, &ScopeRef> {
    let mut scopes = BTreeMap::new();
    for artifact in artifacts {
        if let Decoded::Root(root) = decode_v2(artifact)
            && let EntityRoot::V2(def) = *root
        {
            scopes
                .entry(def.object_type.name.0)
                .or_insert(&artifact.scope);
        }
    }
    scopes
}

/// The type-environment reference an admission receipt of `artifacts` over
/// `base` pins: the whole-set environment when no extension is narrower than
/// its target type (every consumer sees the same one), otherwise a
/// [`SCOPED_TYPE_ENV_ID`] commitment to each consumer scope's environment.
///
/// # Errors
/// Derivation errors (`Err(Ok(errors))`) or canonical hashing errors
/// (`Err(Err(error))`).
pub fn receipt_type_env(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<TypeEnvRef, Result<Vec<EnvError>, CanonicalJsonError>> {
    let pool: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
    if !has_narrower_extension(&pool) {
        let env = crate::receipt::derive_type_env(artifacts, base, limits).map_err(Ok)?;
        return env.reference().map_err(Err);
    }
    let scopes: BTreeMap<ScopeKey, &ScopeRef> = pool
        .iter()
        .map(|artifact| (scope_key(&artifact.scope), &artifact.scope))
        .collect();
    let mut entries = Vec::new();
    for scope in scopes.into_values() {
        let view = ScopedView::bound(scope, artifacts, base, &BTreeSet::new());
        let reference = view
            .receipt_env(limits)
            .map_err(Ok)?
            .reference()
            .map_err(Err)?;
        entries.push(serde_json::json!({
            "scope": scope,
            "typeEnv": {
                "id": reference.id,
                "version": reference.version,
                "sha256": reference.sha256,
            }
        }));
    }
    let commitment = serde_json::json!({ "scopes": entries });
    Ok(TypeEnvRef {
        id: SCOPED_TYPE_ENV_ID.to_owned(),
        version: SCOPED_TYPE_ENV_VERSION,
        sha256: crate::receipt::hash_serde_json(&commitment).map_err(Err)?,
    })
}

/// Lower path equations per candidate scope group so an entity in a broader or
/// sibling scope cannot traverse an out-of-scope extension link.
pub(crate) fn scoped_path_equation_findings(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Vec<Diagnostic> {
    let mut groups: BTreeMap<ScopeKey, (&ScopeRef, BTreeSet<&str>)> = BTreeMap::new();
    for artifact in artifacts {
        if matches!(decode_v2(artifact), Decoded::Root(_)) {
            let entry = groups
                .entry(scope_key(&artifact.scope))
                .or_insert_with(|| (&artifact.scope, BTreeSet::new()));
            entry.1.insert(artifact.artifact_id.as_str());
        }
    }
    if groups.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (scope, owned_ids) in groups.into_values() {
        let view = ScopedView::bound(scope, artifacts, base, registry);
        if let Err(group_findings) = crate::consistency::lower_path_equations(view.pool(), registry)
        {
            findings.extend(group_findings.into_iter().filter(|diagnostic| {
                diagnostic
                    .artifact_id
                    .as_deref()
                    .is_some_and(|id| owned_ids.contains(id))
            }));
        }
    }
    findings
}

/// Refuse `UpsertInputObject` input properties that target an extension
/// property declared in an out-of-scope extension on `kind` or its lineage.
pub(crate) fn upsert_extension_scope_findings(
    action: &ArtifactEnvelope,
    kind: &str,
    input_properties: &BTreeMap<String, JsonValue>,
    effective: Option<&EffectiveObjectType>,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let lineage: BTreeSet<&str> = effective.map_or_else(
        || BTreeSet::from([kind]),
        |view| {
            view.lineage
                .iter()
                .map(norn_ir::ObjectTypeName::as_str)
                .collect()
        },
    );
    let mut out_of_scope: BTreeMap<String, &ArtifactEnvelope> = BTreeMap::new();
    for candidate in base.iter().chain(artifacts) {
        if scope_visible(&action.scope, &candidate.scope) {
            continue;
        }
        let Decoded::Root(root) = decode_v2(candidate) else {
            continue;
        };
        let EntityRoot::V2Extension(ext) = *root else {
            continue;
        };
        if !lineage.contains(ext.object_type_extension.target.as_str()) {
            continue;
        }
        for property in ext.object_type_extension.properties {
            out_of_scope.entry(property.id.0).or_insert(candidate);
        }
    }
    if out_of_scope.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for field in input_properties.keys() {
        if effective.is_some_and(|view| view.property(field).is_some()) {
            continue;
        }
        if let Some(ext_artifact) = out_of_scope.get(field) {
            let pointer =
                Span::from_segments(["payload", "inputSchema", "properties", field]).pointer;
            findings.extend(scope_finding(action, ext_artifact, &pointer));
        }
    }
    findings
}

/// Ensure name uniqueness across scope boundaries when candidates are checked
/// in separate scope groups (preserving `DUPLICATE_NAME` / `SHADOWS_BASE`).
pub(crate) fn check_cross_scope_duplicates(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let mut base_types: BTreeMap<String, &ArtifactEnvelope> = BTreeMap::new();
    let mut base_props: BTreeMap<String, &ArtifactEnvelope> = BTreeMap::new();
    for artifact in base {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        match *root {
            EntityRoot::V2(root) => {
                base_types
                    .entry(root.object_type.name.0)
                    .or_insert(artifact);
            }
            EntityRoot::V2Interface(root) => {
                base_types.entry(root.interface.name.0).or_insert(artifact);
            }
            EntityRoot::V2PropertyTypes(root) => {
                for def in root.property_types {
                    base_props.entry(def.name.0).or_insert(artifact);
                }
            }
            EntityRoot::V2Extension(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }
    let mut types = DupTypeState {
        base: base_types,
        seen: BTreeMap::new(),
        out: Vec::new(),
    };
    let mut seen_props: BTreeMap<String, &ArtifactEnvelope> = BTreeMap::new();
    for artifact in artifacts {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        match *root {
            EntityRoot::V2(root) => {
                types.check(
                    artifact,
                    &root.object_type.name.0,
                    "/payload/objectType/name",
                    crate::object_types::codes::SHADOWS_BASE,
                    crate::object_types::codes::DUPLICATE_NAME,
                );
            }
            EntityRoot::V2Interface(root) => {
                types.check(
                    artifact,
                    &root.interface.name.0,
                    "/payload/interface/name",
                    crate::interfaces::codes::SHADOWS_BASE,
                    crate::interfaces::codes::DUPLICATE_NAME,
                );
            }
            EntityRoot::V2PropertyTypes(root) => {
                for (index, def) in root.property_types.iter().enumerate() {
                    let name = &def.name.0;
                    let pointer = format!("/payload/propertyTypes/{index}/name");
                    if let Some(owner) = base_props
                        .get(name)
                        .or_else(|| seen_props.get(name))
                        .filter(|owner| !scope_visible(&artifact.scope, &owner.scope))
                    {
                        types.out.push(dup_finding(
                            crate::property_types::codes::DUPLICATE_NAME,
                            format!(
                                "property type {name:?} is already defined by artifact {:?}",
                                owner.artifact_id
                            ),
                            artifact,
                            &pointer,
                        ));
                    } else {
                        seen_props.entry(name.clone()).or_insert(artifact);
                    }
                }
            }
            EntityRoot::V2Extension(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }
    types.out
}

struct DupTypeState<'a> {
    base: BTreeMap<String, &'a ArtifactEnvelope>,
    seen: BTreeMap<String, &'a ArtifactEnvelope>,
    out: Vec<Diagnostic>,
}

impl<'a> DupTypeState<'a> {
    fn check(
        &mut self,
        artifact: &'a ArtifactEnvelope,
        name: &str,
        pointer: &str,
        shadows_code: &str,
        dup_code: &str,
    ) {
        if let Some(owner) = self.base.get(name) {
            if !scope_visible(&artifact.scope, &owner.scope) {
                self.out.push(dup_finding(
                    shadows_code,
                    format!("type {name:?} shadows an installed base definition"),
                    artifact,
                    pointer,
                ));
            }
        } else if let Some(owner) = self.seen.get(name) {
            if !scope_visible(&artifact.scope, &owner.scope) {
                self.out.push(dup_finding(
                    dup_code,
                    format!(
                        "type {name:?} is already defined by artifact {:?}",
                        owner.artifact_id
                    ),
                    artifact,
                    pointer,
                ));
            }
        } else {
            self.seen.insert(name.to_owned(), artifact);
        }
    }
}

fn dup_finding(
    code: &str,
    message: String,
    artifact: &ArtifactEnvelope,
    pointer: &str,
) -> Diagnostic {
    Diagnostic {
        witness: None,
        code: crate::DiagnosticCode::new(code),
        severity: norn_ir::DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(norn_ir::DiagnosticRemediation::FixDefinition),
    }
}

/// Index every entity object type (v1 or v2) across `artifacts` and `base` by
/// its object type name to its defining [`ArtifactEnvelope`].
pub(crate) fn entity_object_type_owners<'a>(
    artifacts: &'a [ArtifactEnvelope],
    base: &'a [ArtifactEnvelope],
) -> BTreeMap<String, &'a ArtifactEnvelope> {
    let mut owners = BTreeMap::new();
    for artifact in artifacts.iter().chain(base) {
        if artifact.kind() == norn_ir::ArtifactKind::Entity
            && let Ok(value) = serde_json::to_value(artifact.payload.to_json_value())
            && let Ok(root) = serde_json::from_value::<EntityRoot>(value)
        {
            match root {
                EntityRoot::V2(def) => {
                    owners.entry(def.object_type.name.0).or_insert(artifact);
                }
                EntityRoot::V1(def) => {
                    owners.entry(def.object_kind).or_insert(artifact);
                }
                EntityRoot::V2Interface(_)
                | EntityRoot::V2Extension(_)
                | EntityRoot::V2PropertyTypes(_)
                | EntityRoot::V2Markings(_) => {}
            }
        }
    }
    owners
}

/// Whether `diagnostic` is a redundant `unknown_*` refusal at an exact
/// `(artifact_id, pointer)` already reported as `INCOMPATIBLE_SCOPE`.
pub(crate) fn is_redundant_unknown_after_scope(
    diagnostic: &Diagnostic,
    scope_sites: &BTreeSet<(String, String)>,
) -> bool {
    let code = diagnostic.code.as_str();
    let is_unknown = matches!(
        code,
        crate::object_types::codes::UNKNOWN_SUPERTYPE
            | crate::object_types::codes::UNKNOWN_INTERFACE
            | crate::object_types::codes::UNKNOWN_EXTENSION_TARGET
            | crate::object_types::codes::UNKNOWN_LINK_TARGET
            | crate::object_types::codes::UNKNOWN_REPLACEMENT
            | crate::object_types::codes::INVALID_OWNED_COLLECTION
            | crate::interfaces::codes::UNKNOWN_LINK_TARGET
            | crate::interfaces::codes::UNKNOWN_REPLACEMENT
            | crate::property_types::codes::UNKNOWN_REF
    );
    if !is_unknown {
        return false;
    }
    let (Some(artifact_id), Some(span)) = (&diagnostic.artifact_id, &diagnostic.span) else {
        return false;
    };
    scope_sites.contains(&(artifact_id.clone(), span.pointer.clone()))
}
