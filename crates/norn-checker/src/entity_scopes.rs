//! Consumer-scoped entity resolution and cross-scope entity reference checks (#1376).
//!
//! Every `EntityIR` v2 definition (object type, supertype, interface,
//! extension, shared property type catalog) carries its enclosing
//! [`ArtifactEnvelope::scope`]. A consumer in scope `S` may observe only
//! definitions whose scope satisfies [`scope_visible`] — i.e.
//! [`crate::artifact_set::scope_conflict`] returns `None` across every scope
//! dimension (`orgId`, `tenantId`, `workspaceId`, `environmentId`,
//! `businessUnitId`).

mod refs;

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EntityDefinitions, EnvOptions, TypeEnvRef, ZenTypeEnv};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityRoot, JsonValue, PropertyTypeDef, PropertyTypeName,
    ResourceLimitProfile, ScopeRef,
};

use crate::artifact_set::{scope_conflict, scope_finding};
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::entity_instance::{EffectiveCatalog, EffectiveObjectType};
use crate::entity_lowering::entity_object_type;
use crate::{Diagnostic, Span};

pub(crate) use refs::check_entity_reference_scopes;
pub use refs::check_entity_reference_scopes_all;

/// Deterministic map key for caching per-scope entity projections.
pub type ScopeKey = [Option<String>; 5];

/// Build the canonical cache key for `scope`.
#[must_use]
pub fn scope_key(scope: &ScopeRef) -> ScopeKey {
    [
        scope.org_id.clone(),
        scope.tenant_id.clone(),
        scope.workspace_id.clone(),
        scope.environment_id.clone(),
        scope.business_unit_id.clone(),
    ]
}

/// Whether a consumer in `consumer` may observe an artifact in `target`
/// across all five scope dimensions (`orgId`, `tenantId`, `workspaceId`,
/// `environmentId`, `businessUnitId`).
#[must_use]
pub fn scope_visible(consumer: &ScopeRef, target: &ScopeRef) -> bool {
    scope_conflict(consumer, target).is_none()
}

/// Return only the artifacts of `pool` visible to a consumer in `consumer`.
#[must_use]
pub fn visible_artifacts(pool: &[ArtifactEnvelope], consumer: &ScopeRef) -> Vec<ArtifactEnvelope> {
    pool.iter()
        .filter(|artifact| scope_visible(consumer, &artifact.scope))
        .cloned()
        .collect()
}

impl EffectiveCatalog {
    /// Build the catalog of `artifacts` (see the type docs). Narrower
    /// extensions are never merged into broader object types.
    #[must_use]
    pub fn from_artifacts(artifacts: &[ArtifactEnvelope]) -> Self {
        Self::from_artifacts_with_registry(artifacts, &BTreeSet::new())
    }

    /// Build the catalog of `artifacts` against the registry (Norn Core)
    /// value type names `registry`, so a type whose `ref` names a registry
    /// type is available. Narrower extensions are never merged into broader
    /// object types; each object type resolves against artifacts visible from
    /// its own declaring scope.
    #[must_use]
    pub fn from_artifacts_with_registry(
        artifacts: &[ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        if has_narrower_extension(artifacts) {
            return catalog_by_declaring_scope(artifacts, registry);
        }
        Self::build_single_layer(artifacts, registry)
    }

    /// Build the catalog of `artifacts` visible to a consumer in `consumer`
    /// against `registry`.
    #[must_use]
    pub fn for_scope(
        artifacts: &[ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
        consumer: &ScopeRef,
    ) -> Self {
        let visible = visible_artifacts(artifacts, consumer);
        Self::build_single_layer(&visible, registry)
    }
}

/// Build an [`EffectiveCatalog`] visible to a consumer in `consumer`.
#[must_use]
pub fn effective_catalog_for_scope(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    consumer: &ScopeRef,
) -> EffectiveCatalog {
    EffectiveCatalog::for_scope(pool, registry, consumer)
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
        let scoped = EffectiveCatalog::for_scope(artifacts, registry, &scope);
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

fn bind_exact_schemas(
    mut env: ZenTypeEnv,
    artifacts: &[ArtifactEnvelope],
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    for artifact in artifacts {
        if matches!(
            &artifact.payload,
            ArtifactPayload::Rule(_) | ArtifactPayload::Action(_)
        ) {
            let payload_value = artifact.payload.to_json_value();
            let Some(payload) = payload_value.as_object() else {
                continue;
            };
            for member in ["inputSchema", "outputSchema"] {
                if let Some(schema) = payload.get(member) {
                    let name = format!(
                        "exact.{}.{}.{}",
                        artifact.artifact_id, artifact.version, member
                    );
                    let bound = if matches!(&artifact.payload, ArtifactPayload::Action(_))
                        && member == "inputSchema"
                    {
                        env.bind_exact_action_schema(&name, schema)
                    } else {
                        env.bind_exact_schema(&name, schema)
                    };
                    env = bound.map_err(|reason| {
                        vec![norn_expr::EnvError::Unresolved {
                            site: name,
                            name: reason,
                        }]
                    })?;
                }
            }
        }
    }
    Ok(env)
}

/// Derive the ZEN Expression type environment of `base` and `artifacts` without
/// merging narrower extensions into broader object types.
///
/// # Errors
/// Every [`norn_expr::EnvError`] of the derivation.
pub(crate) fn derive_unscoped_type_env(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    let combined: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
    let target_scopes = v2_object_type_scopes(&combined);
    let mut layer = Layer::default();
    for (index, artifact) in combined.iter().enumerate() {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        if let EntityRoot::V2Extension(ext) = root.as_ref()
            && target_scopes
                .get(ext.object_type_extension.target.as_str())
                .is_some_and(|target_scope| !scope_visible(target_scope, &artifact.scope))
        {
            continue;
        }
        layer.push(index, *root);
    }
    let catalog: Vec<PropertyTypeDef> = layer.catalog.iter().map(|(_, def)| def.clone()).collect();
    let definitions = EntityDefinitions {
        property_types: &catalog,
        interfaces: &layer.interfaces,
        object_types: &layer.object_types,
        extensions: &layer.extensions,
    };
    let env = ZenTypeEnv::from_artifact_set(&definitions, EnvOptions::from_limit_profile(limits))?;
    bind_exact_schemas(env, &combined)
}

/// Derive the ZEN Expression type environment visible to a consumer in `consumer`
/// (including exact rule/action schemas visible to `consumer`).
///
/// # Errors
/// Every [`norn_expr::EnvError`] of the derivation.
pub fn derive_receipt_type_env_for_scope(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
    consumer: &ScopeRef,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    let visible: Vec<ArtifactEnvelope> = base
        .iter()
        .chain(artifacts)
        .filter(|artifact| scope_visible(consumer, &artifact.scope))
        .cloned()
        .collect();
    let mut layer = Layer::default();
    for (index, artifact) in visible.iter().enumerate() {
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
    }
    let catalog: Vec<PropertyTypeDef> = layer.catalog.iter().map(|(_, def)| def.clone()).collect();
    let definitions = EntityDefinitions {
        property_types: &catalog,
        interfaces: &layer.interfaces,
        object_types: &layer.object_types,
        extensions: &layer.extensions,
    };
    let env = ZenTypeEnv::from_artifact_set(&definitions, EnvOptions::from_limit_profile(limits))?;
    bind_exact_schemas(env, &visible)
}

/// Derive the scoped ZEN Expression type environment for every distinct scope
/// present in `base` and `artifacts`.
///
/// # Errors
/// Every [`norn_expr::EnvError`] from the first consumer scope that cannot derive.
pub fn derive_scoped_type_envs(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<BTreeMap<ScopeKey, ZenTypeEnv>, Vec<norn_expr::EnvError>> {
    let mut scopes: BTreeMap<ScopeKey, ScopeRef> = BTreeMap::new();
    for artifact in base.iter().chain(artifacts) {
        scopes
            .entry(scope_key(&artifact.scope))
            .or_insert_with(|| artifact.scope.clone());
    }
    if scopes.is_empty() {
        let global = ScopeRef::default();
        scopes.insert(scope_key(&global), global);
    }
    let mut out = BTreeMap::new();
    for (key, scope) in scopes {
        let env = derive_receipt_type_env_for_scope(artifacts, base, limits, &scope)?;
        out.insert(key, env);
    }
    Ok(out)
}

/// Derive the scope-bound [`TypeEnvRef`] pinned by an admission receipt at
/// `version`.
///
/// Validates that every distinct consumer scope in `base` and `artifacts` has a
/// derivable [`ZenTypeEnv`]. When no extension is narrower than its target type,
/// returns the canonical single-environment reference verbatim; when narrower
/// extensions cause scopes to observe distinct environments, hashes the sorted
/// `(scope, typeEnv)` map together with the unscoped base reference under Norn1.
///
/// # Errors
/// Derivation errors (`Err(Ok(errors))`) or canonical hashing errors
/// (`Err(Err(error))`).
pub fn derive_receipt_type_env_ref_at_version(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
    version: norn_expr::env::TypeEnvVersion,
) -> Result<TypeEnvRef, Result<Vec<norn_expr::EnvError>, norn_canonical::CanonicalJsonError>> {
    let scoped = derive_scoped_type_envs(artifacts, base, limits).map_err(Ok)?;
    let base_env = derive_unscoped_type_env(artifacts, base, limits).map_err(Ok)?;
    let base_ref = base_env.reference_at_version(version).map_err(Err)?;
    let combined: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
    if !has_narrower_extension(&combined) {
        return Ok(base_ref);
    }
    let mut scopes_map: BTreeMap<ScopeKey, ScopeRef> = BTreeMap::new();
    for artifact in &combined {
        scopes_map
            .entry(scope_key(&artifact.scope))
            .or_insert_with(|| artifact.scope.clone());
    }
    let mut entries = Vec::new();
    for (key, env) in &scoped {
        let Some(scope) = scopes_map.get(key) else {
            continue;
        };
        let env_ref = env.reference_at_version(version).map_err(Err)?;
        entries.push(serde_json::json!({
            "scope": scope,
            "typeEnv": {
                "id": env_ref.id,
                "version": env_ref.version,
                "sha256": env_ref.sha256,
            }
        }));
    }
    let payload = serde_json::json!({
        "base": {
            "id": base_ref.id,
            "version": base_ref.version,
            "sha256": base_ref.sha256,
        },
        "scopes": entries,
    });
    let sha256 = crate::receipt::hash_serde_json(&payload).map_err(Err)?;
    Ok(TypeEnvRef {
        id: base_ref.id,
        version: base_ref.version,
        sha256,
    })
}

/// Build a [`ZenTypeEnv`] visible to a consumer in `consumer`.
///
/// # Errors
/// Returns expression environment derivation errors from [`ZenTypeEnv`].
pub fn derive_type_env_for_scope(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    options: EnvOptions,
    consumer: &ScopeRef,
    for_checking: bool,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    derive_type_env_from_parts(pool, &[], registry, options, Some(consumer), for_checking)
}

/// Build a [`ZenTypeEnv`] over `base` and `artifacts`, optionally restricted
/// to artifacts visible to `consumer`.
///
/// # Errors
/// Returns expression environment derivation errors from [`ZenTypeEnv`].
pub(crate) fn derive_type_env_from_parts(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    options: EnvOptions,
    consumer: Option<&ScopeRef>,
    for_checking: bool,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    let mut layer = Layer::default();
    let mut object_types = Vec::new();
    for (index, artifact) in base.iter().chain(artifacts).enumerate() {
        if consumer.is_some_and(|scope| !scope_visible(scope, &artifact.scope)) {
            continue;
        }
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
        if for_checking
            && let ArtifactPayload::Entity(payload) = &artifact.payload
            && let Ok(Some(object)) = entity_object_type(payload)
        {
            object_types.push(object.root.object_type);
        }
    }
    let property_types: Vec<_> = layer.catalog.iter().map(|(_, def)| def.clone()).collect();
    let definitions = EntityDefinitions {
        property_types: &property_types,
        interfaces: &layer.interfaces,
        object_types: if for_checking {
            &object_types
        } else {
            &layer.object_types
        },
        extensions: &layer.extensions,
    };
    if for_checking {
        ZenTypeEnv::from_artifact_set_for_checking(&definitions, registry, options)
    } else {
        ZenTypeEnv::from_artifact_set_with_registry(&definitions, registry, options)
    }
}

/// Consumer-scoped catalog, v2 entity layer (indexed into `pool`), and ZEN
/// type environment for law admission.
pub(crate) struct LawScopeEnv {
    pub(crate) catalog: EffectiveCatalog,
    pub(crate) layer: Layer,
    pub(crate) env: Option<ZenTypeEnv>,
}

/// Build a consumer-scoped [`LawScopeEnv`] over `pool` for `consumer`.
pub(crate) fn law_scope_env(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    limits: &ResourceLimitProfile,
    consumer: &ScopeRef,
) -> LawScopeEnv {
    let catalog = effective_catalog_for_scope(pool, registry, consumer);
    let mut layer = Layer::default();
    for (index, artifact) in pool.iter().enumerate() {
        if !scope_visible(consumer, &artifact.scope) {
            continue;
        }
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
    }
    let env = derive_type_env_for_scope(
        pool,
        registry,
        EnvOptions::from_limit_profile(limits),
        consumer,
        false,
    )
    .ok();
    LawScopeEnv {
        catalog,
        layer,
        env,
    }
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
    let complete: Vec<_> = base.iter().chain(artifacts).cloned().collect();
    let mut findings = Vec::new();
    for (scope, owned_ids) in groups.into_values() {
        let visible = visible_artifacts(&complete, scope);
        if let Err(group_findings) = crate::consistency::lower_path_equations(&visible, registry) {
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

/// Build the scoped effective catalog and expression environment for relational transform admission.
#[must_use]
pub(crate) fn transform_scope_parts(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    limits: &norn_ir::ResourceLimitProfile,
    scope: &ScopeRef,
) -> (EffectiveCatalog, Option<ZenTypeEnv>) {
    let catalog = effective_catalog_for_scope(pool, registry, scope);
    let opts = EnvOptions::from_limit_profile(limits);
    let env = derive_type_env_for_scope(pool, registry, opts, scope, false).ok();
    (catalog, env)
}

/// Build the effective catalog and expression environment for `action_set::Model`.
pub(crate) fn action_model_parts(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    effective: bool,
    consumer: Option<&ScopeRef>,
) -> (
    Option<EffectiveCatalog>,
    Result<ZenTypeEnv, Vec<norn_expr::EnvError>>,
) {
    let catalog = effective.then(|| {
        let combined: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
        consumer.map_or_else(
            || EffectiveCatalog::from_artifacts_with_registry(&combined, registry),
            |scope| effective_catalog_for_scope(&combined, registry, scope),
        )
    });
    let env = derive_type_env_from_parts(
        artifacts,
        base,
        registry,
        EnvOptions::default(),
        consumer,
        true,
    );
    (catalog, env)
}

/// Build the type environment and effective catalog for `MappingModel`.
///
/// # Errors
/// Environment derivation failures as `ENVIRONMENT_UNDERIVABLE` diagnostics.
pub(crate) fn mapping_model_parts(
    artifacts: &[ArtifactEnvelope],
    consumer: Option<&ScopeRef>,
) -> Result<(ZenTypeEnv, EffectiveCatalog), Vec<Diagnostic>> {
    let limits = ResourceLimitProfile::norn_limits_v1();
    let env_res = match consumer {
        Some(scope) => derive_receipt_type_env_for_scope(artifacts, &[], &limits, scope),
        None => derive_unscoped_type_env(artifacts, &[], &limits),
    };
    let env = env_res.map_err(|errors| {
        errors
            .into_iter()
            .map(|reason| Diagnostic {
                witness: None,
                code: crate::DiagnosticCode::new(crate::receipt::codes::ENVIRONMENT_UNDERIVABLE),
                severity: norn_ir::DiagnosticSeverity::Error,
                message: format!(
                    "the mapping model's type environment cannot be derived: {reason}"
                ),
                span: Some(Span::pointer("")),
                artifact_id: None,
                remediation: Some(norn_ir::DiagnosticRemediation::FixDefinition),
            })
            .collect::<Vec<_>>()
    })?;
    let catalog = match consumer {
        Some(scope) => EffectiveCatalog::for_scope(artifacts, &BTreeSet::new(), scope),
        None => EffectiveCatalog::from_artifacts(artifacts),
    };
    Ok((env, catalog))
}
