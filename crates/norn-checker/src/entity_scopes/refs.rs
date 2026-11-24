//! Cross-scope `EntityIR` v2 reference checks across candidates and installed base (#1376).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, EntityRoot, InterfaceDef, ObjectTypeDef, PropertyType, PropertyTypeDef,
    ResourceStatus,
};

use crate::Diagnostic;
use crate::artifact_set::scope_finding;
use crate::entity_artifacts::{Decoded, decode_v2};

/// Indexed `EntityIR` v2 definitions across `base` and `artifacts`, retaining
/// the defining [`ArtifactEnvelope`] (and its [`norn_ir::ScopeRef`]) for each name.
struct EntityIndex<'a> {
    object_types: BTreeMap<String, (&'a ArtifactEnvelope, ObjectTypeDef)>,
    interfaces: BTreeMap<String, (&'a ArtifactEnvelope, InterfaceDef)>,
    property_types: BTreeMap<String, (&'a ArtifactEnvelope, PropertyTypeDef)>,
}

fn inherited_edge_finding(
    owner: &ArtifactEnvelope,
    declarer: &ArtifactEnvelope,
    target: &ArtifactEnvelope,
    pointer: &str,
) -> Option<Diagnostic> {
    if let Some(diagnostic) = scope_finding(owner, target, pointer) {
        return Some(diagnostic);
    }
    let mut edge = scope_finding(declarer, target, pointer)?;
    edge.artifact_id = Some(owner.artifact_id.clone());
    edge.message = format!(
        "inherited reference via {:?} violates scope: {}",
        declarer.artifact_id, edge.message
    );
    Some(edge)
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
        let mut declarer = owner;
        let mut next = Some(start.to_owned());
        while let Some(name) = next.take() {
            if !visited_types.insert(name.clone()) {
                break;
            }
            let Some((target, def)) = self.object_types.get(&name) else {
                break;
            };
            if let Some(diagnostic) = inherited_edge_finding(owner, declarer, target, pointer) {
                out.push(diagnostic);
                return;
            }
            for iface in &def.implements {
                if self.check_interface_chain_inner(
                    owner,
                    target,
                    iface.as_str(),
                    pointer,
                    &mut visited_ifaces,
                    out,
                ) {
                    return;
                }
            }
            declarer = target;
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
        let _ = self.check_interface_chain_inner(owner, owner, start, pointer, &mut visited, out);
    }

    fn check_interface_chain_inner(
        &self,
        owner: &ArtifactEnvelope,
        initial_declarer: &'a ArtifactEnvelope,
        start: &str,
        pointer: &str,
        visited: &mut BTreeSet<String>,
        out: &mut Vec<Diagnostic>,
    ) -> bool {
        let mut pending = vec![(initial_declarer, start.to_owned())];
        while let Some((declarer, name)) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let Some((target, def)) = self.interfaces.get(&name) else {
                continue;
            };
            if let Some(diagnostic) = inherited_edge_finding(owner, declarer, target, pointer) {
                out.push(diagnostic);
                return true;
            }
            pending.extend(
                def.extends
                    .iter()
                    .map(|parent| (*target, parent.as_str().to_owned())),
            );
        }
        false
    }

    /// The definer of `name` in the one name space object types and
    /// interfaces share (link targets, replacements, child types).
    fn check_named(
        &self,
        owner: &ArtifactEnvelope,
        name: &str,
        pointer: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let target = self
            .object_types
            .get(name)
            .map(|(target, _)| target)
            .or_else(|| self.interfaces.get(name).map(|(target, _)| target));
        if let Some(target) = target {
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
        self.check_named(owner, as_str(name), pointer, out);
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
                let mut declarer = owner;
                let mut current = Some(name.as_str().to_owned());
                while let Some(ref_name) = current.take() {
                    if !visited.insert(ref_name.clone()) {
                        break;
                    }
                    let Some((target, def)) = self.property_types.get(&ref_name) else {
                        break;
                    };
                    if let Some(diagnostic) =
                        inherited_edge_finding(owner, declarer, target, &name_pointer)
                    {
                        out.push(diagnostic);
                        break;
                    }
                    declarer = target;
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
            self.check_named(
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
            self.check_named(
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
            self.check_named(
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

    fn check_artifact(&self, artifact: &ArtifactEnvelope, out: &mut Vec<Diagnostic>) {
        let Decoded::Root(root) = decode_v2(artifact) else {
            return;
        };
        match *root {
            EntityRoot::V2(root) => self.check_object_type(artifact, &root.object_type, out),
            EntityRoot::V2Interface(root) => self.check_interface(artifact, &root.interface, out),
            EntityRoot::V2Extension(root) => {
                self.check_extension(artifact, &root.object_type_extension, out);
            }
            EntityRoot::V2PropertyTypes(root) => {
                for (i, def) in root.property_types.iter().enumerate() {
                    self.check_property_type(
                        artifact,
                        &def.property_type,
                        &format!("/payload/propertyTypes/{i}/type"),
                        out,
                    );
                }
            }
            EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }
}

/// Check every candidate and installed-base `EntityIR` v2 artifact's
/// entity-to-entity references (supertypes, interfaces, extension targets,
/// link targets, owned collection child types, replacements, and `ref`
/// property types) across all scope dimensions.
pub(crate) fn check_entity_reference_scopes(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let index = EntityIndex::build(artifacts, base);
    let mut out = Vec::new();
    for artifact in artifacts.iter().chain(base) {
        index.check_artifact(artifact, &mut out);
    }
    out
}

/// Check every `EntityIR` v2 artifact in `artifacts` for cross-scope entity
/// references (supertypes, interfaces, extensions, links, owned collections,
/// replacements, and `ref` property types).
#[must_use]
pub fn check_entity_reference_scopes_all(artifacts: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    check_entity_reference_scopes(artifacts, &[])
}
