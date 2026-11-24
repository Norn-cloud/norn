//! Consumer-scoped entity resolution and cross-scope entity reference checks (#1376).
//!
//! Every `EntityIR` v2 definition (object type, supertype, interface,
//! extension, shared property type catalog) carries its enclosing
//! [`ArtifactEnvelope::scope`]. A consumer in scope `S` may observe only
//! definitions whose scope satisfies [`scope_visible(S, target)`] — i.e.
//! [`crate::artifact_set::scope_conflict`] returns `None` across every scope
//! dimension (`orgId`, `tenantId`, `workspaceId`, `environmentId`,
//! `businessUnitId`).

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EntityDefinitions, EnvOptions, ZenTypeEnv};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityRoot, InterfaceDef, JsonValue, ObjectTypeDef,
    PropertyType, PropertyTypeDef, PropertyTypeName, ResourceLimitProfile, ResourceStatus,
    ScopeRef,
};

use crate::artifact_set::{scope_conflict, scope_finding};
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::entity_instance::{EffectiveCatalog, EffectiveObjectType};
use crate::entity_lowering::entity_object_type;
use crate::{Diagnostic, Span};

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

/// Build an [`EffectiveCatalog`] visible to a consumer in `consumer`.
#[must_use]
pub fn effective_catalog_for_scope(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    consumer: &ScopeRef,
) -> EffectiveCatalog {
    let visible = visible_artifacts(pool, consumer);
    EffectiveCatalog::from_artifacts_with_registry(&visible, registry)
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

/// Indexed `EntityIR` v2 definitions across `base` and `artifacts`, retaining
/// the defining [`ArtifactEnvelope`] (and its [`ScopeRef`]) for each name.
struct EntityIndex<'a> {
    object_types: BTreeMap<String, (&'a ArtifactEnvelope, ObjectTypeDef)>,
    interfaces: BTreeMap<String, (&'a ArtifactEnvelope, InterfaceDef)>,
    property_types: BTreeMap<String, (&'a ArtifactEnvelope, PropertyTypeDef)>,
}

impl<'a> EntityIndex<'a> {
    fn build(artifacts: &'a [ArtifactEnvelope], base: &'a [ArtifactEnvelope]) -> Self {
        let mut index = Self {
            object_types: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            property_types: BTreeMap::new(),
        };
        for artifact in base.iter().chain(artifacts) {
            let Decoded::Root(root) = decode_v2(artifact) else {
                continue;
            };
            match *root {
                EntityRoot::V2(root) => {
                    index
                        .object_types
                        .entry(root.object_type.name.0.clone())
                        .or_insert((artifact, root.object_type));
                }
                EntityRoot::V2Interface(root) => {
                    index
                        .interfaces
                        .entry(root.interface.name.0.clone())
                        .or_insert((artifact, root.interface));
                }
                EntityRoot::V2PropertyTypes(root) => {
                    for def in root.property_types {
                        index
                            .property_types
                            .entry(def.name.0.clone())
                            .or_insert((artifact, def));
                    }
                }
                EntityRoot::V2Extension(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
            }
        }
        index
    }

    fn check_object_chain(
        &self,
        owner: &ArtifactEnvelope,
        start: &str,
        pointer: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let mut visited_types = BTreeSet::new();
        let mut visited_ifaces = BTreeSet::new();
        let mut next = Some(start.to_owned());
        while let Some(name) = next.take() {
            if !visited_types.insert(name.clone()) {
                break;
            }
            let Some((target, def)) = self.object_types.get(&name) else {
                break;
            };
            if let Some(diagnostic) = scope_finding(owner, target, pointer) {
                out.push(diagnostic);
                return;
            }
            for iface in &def.implements {
                if self.check_interface_chain_inner(
                    owner,
                    iface.as_str(),
                    pointer,
                    &mut visited_ifaces,
                    out,
                ) {
                    return;
                }
            }
            next = def
                .extends
                .as_ref()
                .map(|parent| parent.as_str().to_owned());
        }
    }

    fn check_interface_chain(
        &self,
        owner: &ArtifactEnvelope,
        start: &str,
        pointer: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let mut visited = BTreeSet::new();
        let _ = self.check_interface_chain_inner(owner, start, pointer, &mut visited, out);
    }

    fn check_interface_chain_inner(
        &self,
        owner: &ArtifactEnvelope,
        start: &str,
        pointer: &str,
        visited: &mut BTreeSet<String>,
        out: &mut Vec<Diagnostic>,
    ) -> bool {
        let mut pending = vec![start.to_owned()];
        while let Some(name) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let Some((target, def)) = self.interfaces.get(&name) else {
                continue;
            };
            if let Some(diagnostic) = scope_finding(owner, target, pointer) {
                out.push(diagnostic);
                return true;
            }
            pending.extend(def.extends.iter().map(|parent| parent.as_str().to_owned()));
        }
        false
    }

    fn check_named_object(
        &self,
        owner: &ArtifactEnvelope,
        name: &str,
        pointer: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        if let Some((target, _)) = self.object_types.get(name) {
            out.extend(scope_finding(owner, target, pointer));
        }
    }

    fn check_replacement<N>(
        &self,
        owner: &ArtifactEnvelope,
        status: &ResourceStatus<N>,
        as_str: impl Fn(&N) -> &str,
        pointer: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let ResourceStatus::Deprecated {
            replaced_by: Some(name),
            ..
        } = status
        else {
            return;
        };
        let name = as_str(name);
        if let Some((target, _)) = self.object_types.get(name) {
            out.extend(scope_finding(owner, target, pointer));
        } else if let Some((target, _)) = self.interfaces.get(name) {
            out.extend(scope_finding(owner, target, pointer));
        }
    }

    fn check_property_type(
        &self,
        owner: &ArtifactEnvelope,
        property_type: &PropertyType,
        pointer: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        match property_type {
            PropertyType::Ref { name, .. } => {
                let name_pointer = format!("{pointer}/name");
                let mut visited = BTreeSet::new();
                let mut current = Some(name.as_str().to_owned());
                while let Some(ref_name) = current.take() {
                    if !visited.insert(ref_name.clone()) {
                        break;
                    }
                    let Some((target, def)) = self.property_types.get(&ref_name) else {
                        break;
                    };
                    if let Some(diagnostic) = scope_finding(owner, target, &name_pointer) {
                        out.push(diagnostic);
                        break;
                    }
                    current = if let PropertyType::Ref { name, .. } = &def.property_type {
                        Some(name.as_str().to_owned())
                    } else {
                        None
                    };
                }
            }
            PropertyType::Record { members } => {
                for (index, member) in members.iter().enumerate() {
                    self.check_property_type(
                        owner,
                        &member.property_type,
                        &format!("{pointer}/members/{index}/type"),
                        out,
                    );
                }
            }
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Decimal { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Boolean {}
            | PropertyType::Timestamp { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Enum { .. }
            | PropertyType::JsonExt { .. } => {}
        }
    }

    fn check_links(
        &self,
        owner: &ArtifactEnvelope,
        prefix: &str,
        links: &[norn_ir::Link],
        out: &mut Vec<Diagnostic>,
    ) {
        for (index, link) in links.iter().enumerate() {
            self.check_named_object(
                owner,
                link.target.as_str(),
                &format!("{prefix}/links/{index}/target"),
                out,
            );
            for (prop_index, prop) in link.properties.iter().enumerate() {
                self.check_property_type(
                    owner,
                    &prop.property_type,
                    &format!("{prefix}/links/{index}/properties/{prop_index}/type"),
                    out,
                );
            }
        }
    }
    fn check_object_type(
        &self,
        artifact: &ArtifactEnvelope,
        def: &ObjectTypeDef,
        out: &mut Vec<Diagnostic>,
    ) {
        if let Some(parent) = &def.extends {
            self.check_object_chain(
                artifact,
                parent.as_str(),
                "/payload/objectType/extends",
                out,
            );
        }
        for (i, iface) in def.implements.iter().enumerate() {
            self.check_interface_chain(
                artifact,
                iface.as_str(),
                &format!("/payload/objectType/implements/{i}"),
                out,
            );
        }
        self.check_links(artifact, "/payload/objectType", &def.links, out);
        for (i, coll) in def.owned_collections.iter().enumerate() {
            self.check_named_object(
                artifact,
                coll.child_type.as_str(),
                &format!("/payload/objectType/ownedCollections/{i}/childType"),
                out,
            );
        }
        self.check_replacement(
            artifact,
            &def.status,
            norn_ir::ObjectTypeName::as_str,
            "/payload/objectType/status/replacedBy",
            out,
        );
        for (i, prop) in def.properties.iter().enumerate() {
            self.check_property_type(
                artifact,
                &prop.property_type,
                &format!("/payload/objectType/properties/{i}/type"),
                out,
            );
        }
    }

    fn check_interface(
        &self,
        artifact: &ArtifactEnvelope,
        def: &InterfaceDef,
        out: &mut Vec<Diagnostic>,
    ) {
        for (i, parent) in def.extends.iter().enumerate() {
            self.check_interface_chain(
                artifact,
                parent.as_str(),
                &format!("/payload/interface/extends/{i}"),
                out,
            );
        }
        for (i, link) in def.links.iter().enumerate() {
            self.check_named_object(
                artifact,
                link.target.as_str(),
                &format!("/payload/interface/links/{i}/target"),
                out,
            );
        }
        self.check_replacement(
            artifact,
            &def.status,
            norn_ir::InterfaceName::as_str,
            "/payload/interface/status/replacedBy",
            out,
        );
        for (i, prop) in def.properties.iter().enumerate() {
            self.check_property_type(
                artifact,
                &prop.property_type,
                &format!("/payload/interface/properties/{i}/type"),
                out,
            );
        }
    }

    fn check_extension(
        &self,
        artifact: &ArtifactEnvelope,
        ext: &norn_ir::ObjectTypeExtension,
        out: &mut Vec<Diagnostic>,
    ) {
        self.check_object_chain(
            artifact,
            ext.target.as_str(),
            "/payload/objectTypeExtension/target",
            out,
        );
        self.check_links(artifact, "/payload/objectTypeExtension", &ext.links, out);
        for (i, prop) in ext.properties.iter().enumerate() {
            self.check_property_type(
                artifact,
                &prop.property_type,
                &format!("/payload/objectTypeExtension/properties/{i}/type"),
                out,
            );
        }
    }
}

/// Check every candidate `EntityIR` v2 artifact's entity-to-entity references
/// (supertypes, interfaces, extension targets, link targets, owned collection
/// child types, replacements, and `ref` property types) against both
/// `artifacts` and installed `base` across all scope dimensions.
pub(crate) fn check_entity_reference_scopes(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let index = EntityIndex::build(artifacts, base);
    let mut out = Vec::new();
    for artifact in artifacts {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        match *root {
            EntityRoot::V2(root) => index.check_object_type(artifact, &root.object_type, &mut out),
            EntityRoot::V2Interface(root) => {
                index.check_interface(artifact, &root.interface, &mut out);
            }
            EntityRoot::V2Extension(root) => {
                index.check_extension(artifact, &root.object_type_extension, &mut out);
            }
            EntityRoot::V2PropertyTypes(root) => {
                for (i, def) in root.property_types.iter().enumerate() {
                    index.check_property_type(
                        artifact,
                        &def.property_type,
                        &format!("/payload/propertyTypes/{i}/type"),
                        &mut out,
                    );
                }
            }
            EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }
    out
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
