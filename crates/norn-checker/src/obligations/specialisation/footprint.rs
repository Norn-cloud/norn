//! Static type-footprint and writer-closure analysis for projected-safety
//! specialisation (`specialisation-projected/1`).
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::expression_reads;
use norn_ir::mapping::MappingDocument;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, EntityRoot, LawIR,
    LawResponse, PropertyTypeName,
};

use crate::entity_artifacts::{Decoded, decode_v2};
use crate::lifecycle_writes::FactSchema;
use crate::verification::LawWriter;
use crate::{EffectiveCatalog, EffectiveLink, EffectiveObjectType, EffectiveProperty};

/// Deterministic work budget for projected-safety catalog indexing and
/// transitive footprint analysis; exceeding it fails closed (no reuse).
const MAX_PROJECTED_WORK: usize = 262_144;

/// Static write footprint of one newly introduced writer in the extension.
pub(super) enum WriterFootprint {
    /// Provably closed set of touched object/interface type names (expanded
    /// across supertypes, implemented interfaces, and subtypes/implementers).
    Closed(BTreeSet<String>),
    /// Writer whose write set cannot be statically closed; fails closed.
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LawFootprint {
    /// Concrete object types governed or read by the law.
    pub(super) concrete_types: BTreeSet<String>,
    /// All governed/read type names plus their supertypes and interfaces.
    pub(super) subject_and_ancestor_names: BTreeSet<String>,
}

/// Precomputed catalog lookup tables so footprint and link-closure queries stay
/// linear in the catalog size under a bounded work budget.
pub(super) struct CatalogIndex {
    pub(super) catalog: EffectiveCatalog,
    target_types: BTreeMap<String, BTreeSet<String>>,
    forward_links: BTreeMap<String, BTreeSet<String>>,
    inverse_links: BTreeMap<String, BTreeSet<String>>,
}

impl CatalogIndex {
    pub(super) fn build(
        pool: &[ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
        work: &mut usize,
    ) -> Option<Self> {
        let catalog = EffectiveCatalog::from_artifacts_with_registry(pool, registry);
        let mut target_types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in catalog.object_type_names() {
            charge_work(work, 1)?;
            let obj = catalog.object_type_named(name)?.ok()?;
            for ancestor in &obj.lineage {
                charge_work(work, 1)?;
                target_types
                    .entry(ancestor.as_str().to_owned())
                    .or_default()
                    .insert(name.to_owned());
            }
            for iface in &obj.interfaces {
                charge_work(work, 1)?;
                target_types
                    .entry(iface.as_str().to_owned())
                    .or_default()
                    .insert(name.to_owned());
            }
        }
        // Ensure declared interfaces with zero implementers still resolve to `{}`.
        for artifact in pool {
            if let Decoded::Root(root) = decode_v2(artifact)
                && let EntityRoot::V2Interface(iface) = *root
            {
                charge_work(work, 1)?;
                target_types
                    .entry(iface.interface.name.as_str().to_owned())
                    .or_default();
            }
        }

        let mut forward_links: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut inverse_links: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in catalog.object_type_names() {
            let Some(links) = catalog.links_for_object_type(name) else {
                continue;
            };
            for link in links {
                let targets = target_types.get(link.target.as_str())?;
                for target in targets {
                    charge_work(work, 1)?;
                    forward_links
                        .entry(name.to_owned())
                        .or_default()
                        .insert(target.clone());
                    inverse_links
                        .entry(target.clone())
                        .or_default()
                        .insert(name.to_owned());
                }
            }
        }
        Some(Self {
            catalog,
            target_types,
            forward_links,
            inverse_links,
        })
    }

    fn target_types(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.target_types.get(name)
    }
}

pub(super) fn charge_work(work: &mut usize, cost: usize) -> Option<()> {
    *work = work.saturating_add(cost);
    (*work <= MAX_PROJECTED_WORK).then_some(())
}

fn effective_property_unchanged(left: &EffectiveProperty, right: &EffectiveProperty) -> bool {
    left.id == right.id
        && left.kind == right.kind
        && left.constraints == right.constraints
        && left.nullable == right.nullable
        && left.default == right.default
        && left.protected() == right.protected()
        && left.computed == right.computed
        && left.computed_inputs == right.computed_inputs
        && left.computed_reads_all == right.computed_reads_all
        && left.markings == right.markings
        && left.record_members().len() == right.record_members().len()
        && left
            .record_members()
            .iter()
            .zip(right.record_members())
            .all(|(a, b)| effective_property_unchanged(a, b))
}

pub(super) fn effective_contract_unchanged(
    base_obj: &EffectiveObjectType,
    target_obj: &EffectiveObjectType,
    base_links: Option<&[EffectiveLink]>,
    target_links: Option<&[EffectiveLink]>,
) -> bool {
    if base_obj.name != target_obj.name
        || base_obj.lineage != target_obj.lineage
        || base_obj.interfaces != target_obj.interfaces
        || base_obj.lifecycles != target_obj.lifecycles
        || base_obj.owned_collections != target_obj.owned_collections
        || base_obj.has_links != target_obj.has_links
        || base_obj.has_invariants != target_obj.has_invariants
        || base_obj.markings != target_obj.markings
        || base_obj.referenced_property_types != target_obj.referenced_property_types
        || base_obj.properties.len() != target_obj.properties.len()
        || base_obj.keys.len() != target_obj.keys.len()
    {
        return false;
    }
    if !base_obj
        .properties
        .iter()
        .zip(&target_obj.properties)
        .all(|(left, right)| effective_property_unchanged(left, right))
    {
        return false;
    }
    for (left, right) in base_obj.keys.iter().zip(&target_obj.keys) {
        if left.name != right.name
            || left.declared_by != right.declared_by
            || left.components != right.components
            || left.normalize != right.normalize
            || left.match_policy != right.match_policy
            || left.naming_series != right.naming_series
        {
            return false;
        }
    }
    match (base_links, target_links) {
        (Some(left), Some(right)) => {
            left.len() == right.len()
                && left.iter().zip(right).all(|(a, b)| {
                    a.id == b.id
                        && a.target == b.target
                        && a.cardinality == b.cardinality
                        && a.acyclic == b.acyclic
                        && a.inverse == b.inverse
                        && a.properties.len() == b.properties.len()
                        && a.properties
                            .iter()
                            .zip(&b.properties)
                            .all(|(lp, rp)| effective_property_unchanged(lp, rp))
                })
        }
        (None, None) => true,
        _ => false,
    }
}

/// Expands a type name (object type or interface) across its complete type
/// hierarchy: every concrete subtype/implementer (`target_types`), plus every
/// ancestor (`lineage`) and implemented interface (`interfaces`) of those types.
pub(super) fn expand_type_hierarchy(
    type_name: &str,
    index: &CatalogIndex,
    out: &mut BTreeSet<String>,
    work: &mut usize,
) -> bool {
    if charge_work(work, 1).is_none() {
        return false;
    }
    let Some(targets) = index.target_types(type_name) else {
        return false;
    };
    out.insert(type_name.to_owned());
    for concrete in targets {
        if charge_work(work, 1).is_none() {
            return false;
        }
        let Some(Ok(obj)) = index.catalog.object_type_named(concrete) else {
            return false;
        };
        out.insert(concrete.clone());
        for ancestor in &obj.lineage {
            out.insert(ancestor.as_str().to_owned());
        }
        for iface in &obj.interfaces {
            out.insert(iface.as_str().to_owned());
        }
    }
    true
}

pub(super) fn writer_footprint(
    writer: &LawWriter,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    match writer {
        LawWriter::Transition { object_type, .. } => {
            let mut touched = BTreeSet::new();
            if expand_type_hierarchy(object_type, index, &mut touched, work) {
                WriterFootprint::Closed(touched)
            } else {
                WriterFootprint::Opaque
            }
        }
        LawWriter::Artifact { artifact_id } => {
            let matching: Vec<&ArtifactEnvelope> = pool
                .iter()
                .filter(|artifact| artifact.artifact_id == *artifact_id)
                .collect();
            let [artifact] = matching.as_slice() else {
                return WriterFootprint::Opaque;
            };
            artifact_writer_footprint(artifact, pool, index, work)
        }
    }
}

fn artifact_writer_footprint(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    match artifact.kind() {
        ArtifactKind::Action => action_writer_footprint(artifact, pool, index, work),
        ArtifactKind::Rule => rule_writer_footprint(artifact, index, work),
        ArtifactKind::Mapping => mapping_writer_footprint(artifact, pool, index, work),
        ArtifactKind::Connector
        | ArtifactKind::Entity
        | ArtifactKind::Relation
        | ArtifactKind::Widget
        | ArtifactKind::Customization
        | ArtifactKind::Setting
        | ArtifactKind::BusinessPack
        | ArtifactKind::Policy
        | ArtifactKind::Derived
        | ArtifactKind::SourceSchema
        | ArtifactKind::Law
        | ArtifactKind::DefinedConcept
        | ArtifactKind::Labels
        | ArtifactKind::Model
        | ArtifactKind::Theme => WriterFootprint::Opaque,
    }
}

fn action_writer_footprint(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    let Ok(payload) = serde_json::to_value(artifact.payload.to_json_value()) else {
        return WriterFootprint::Opaque;
    };
    let Ok(action) = serde_json::from_value::<ActionRoot>(payload) else {
        return WriterFootprint::Opaque;
    };
    // Outbound commands and quantity-ledger effects reach outside the closed
    // EntityIR write footprint; fail closed to Opaque.
    if !action.commands.is_empty() || action.quantity_effect.is_some() {
        return WriterFootprint::Opaque;
    }
    let Some(program) = &action.effect_program else {
        return WriterFootprint::Opaque;
    };

    let mut raw_types = BTreeSet::new();
    let target_kind = program.target().object_kind;
    raw_types.insert(target_kind.to_owned());

    if matches!(program, ActionEffectProgram::MutateParentWithRows { .. }) {
        let Some(targets) = index.target_types(target_kind) else {
            return WriterFootprint::Opaque;
        };
        for concrete in targets {
            let Some(Ok(parent)) = index.catalog.object_type_named(concrete) else {
                return WriterFootprint::Opaque;
            };
            for col in &parent.owned_collections {
                raw_types.insert(col.child_type.as_str().to_owned());
            }
        }
    }
    for write in &action.writes {
        raw_types.insert(write.entity_kind.clone());
    }
    for link in &action.create_links {
        raw_types.insert(link.target_type.clone());
        if let Some(source) = &link.source {
            raw_types.insert(source.object_type.clone());
        }
    }

    let mut touched = BTreeSet::new();
    for name in &raw_types {
        if !expand_type_hierarchy(name, index, &mut touched, work) {
            return WriterFootprint::Opaque;
        }
    }

    if let Some(pin) = &action.synchronous_rule {
        let matched: Vec<&ArtifactEnvelope> = pool
            .iter()
            .filter(|candidate| {
                candidate.artifact_id == pin.artifact_id
                    && candidate.version == pin.version
                    && candidate.kind() == ArtifactKind::Rule
            })
            .collect();
        let [rule_artifact] = matched.as_slice() else {
            return WriterFootprint::Opaque;
        };
        match rule_writer_footprint(rule_artifact, index, work) {
            WriterFootprint::Closed(rule_types) => touched.extend(rule_types),
            WriterFootprint::Opaque => return WriterFootprint::Opaque,
        }
    }

    WriterFootprint::Closed(touched)
}

/// Whether `output_schema.properties.<field>` is guaranteed to be empty
/// (`maxItems == 0`, or omitted under `additionalProperties: false`).
fn schema_array_must_be_empty(output_schema: &norn_ir::JsonValue, field: &str) -> bool {
    let Some(root) = output_schema.as_object() else {
        return false;
    };
    let Some(properties) = root
        .get("properties")
        .and_then(norn_ir::JsonValue::as_object)
    else {
        return false;
    };
    let Some(entry) = properties.get(field) else {
        return matches!(
            root.get("additionalProperties"),
            Some(norn_ir::JsonValue::Bool(false))
        );
    };
    let Some(entry_obj) = entry.as_object() else {
        return false;
    };
    entry_obj
        .get("type")
        .and_then(norn_ir::JsonValue::as_string)
        == Some("array")
        && matches!(
            entry_obj.get("maxItems"),
            Some(norn_ir::JsonValue::Number(n)) if n.is_finite() && n.abs().to_bits() == 0
        )
}

fn schema_string_enum<'a>(schema: &'a norn_ir::JsonValue, field: &str) -> Option<Vec<&'a str>> {
    let prop = schema
        .as_object()?
        .get("properties")?
        .as_object()?
        .get(field)?
        .as_object()?;
    if prop.get("type")?.as_string()? != "string" {
        return None;
    }
    let values = prop.get("enum")?.as_array()?;
    let strings: Vec<&str> = values
        .iter()
        .filter_map(norn_ir::JsonValue::as_string)
        .collect();
    (!strings.is_empty() && strings.len() == values.len()).then_some(strings)
}

fn rule_writer_footprint(
    artifact: &ArtifactEnvelope,
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    let payload = artifact.payload.to_json_value();
    let Some(object) = payload.as_object() else {
        return WriterFootprint::Opaque;
    };
    let is_decide = object
        .get("contract")
        .and_then(norn_ir::JsonValue::as_string)
        == Some("decide")
        && object.contains_key("graph");
    if !is_decide {
        return WriterFootprint::Opaque;
    }
    // External write-back commands on a rule leave the closed entity footprint.
    if object
        .get("sourceCommands")
        .is_some_and(|cmds| !cmds.as_array().is_some_and(<[_]>::is_empty))
    {
        return WriterFootprint::Opaque;
    }
    let Some(output_schema) = object.get("outputSchema") else {
        return WriterFootprint::Opaque;
    };
    if !schema_array_must_be_empty(output_schema, "commands")
        || !schema_array_must_be_empty(output_schema, "timers")
    {
        return WriterFootprint::Opaque;
    }
    let Some(schema) = FactSchema::of(output_schema) else {
        return WriterFootprint::Opaque;
    };
    let Some(object_types) = schema.values("objectType") else {
        return WriterFootprint::Opaque;
    };
    if object_types.is_empty() {
        return WriterFootprint::Opaque;
    }
    let mut touched = BTreeSet::new();
    for type_name in object_types {
        if !expand_type_hierarchy(type_name, index, &mut touched, work) {
            return WriterFootprint::Opaque;
        }
    }
    // If `links` is declared and not guaranteed empty, include every sourceType
    // and targetType in its closed enum (fail closed to Opaque otherwise).
    if !schema_array_must_be_empty(output_schema, "links") {
        let Some(links_obj) = output_schema
            .as_object()
            .and_then(|root| root.get("properties"))
            .and_then(norn_ir::JsonValue::as_object)
            .and_then(|props| props.get("links"))
            .and_then(norn_ir::JsonValue::as_object)
        else {
            return WriterFootprint::Opaque;
        };
        if links_obj
            .get("type")
            .and_then(norn_ir::JsonValue::as_string)
            != Some("array")
        {
            return WriterFootprint::Opaque;
        }
        let Some(items) = links_obj.get("items") else {
            return WriterFootprint::Opaque;
        };
        let (Some(sources), Some(targets)) = (
            schema_string_enum(items, "sourceType"),
            schema_string_enum(items, "targetType"),
        ) else {
            return WriterFootprint::Opaque;
        };
        for type_name in sources.into_iter().chain(targets) {
            if !expand_type_hierarchy(type_name, index, &mut touched, work) {
                return WriterFootprint::Opaque;
            }
        }
    }
    WriterFootprint::Closed(touched)
}

fn mapping_writer_footprint(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    let Ok(payload) = serde_json::to_value(artifact.payload.to_json_value()) else {
        return WriterFootprint::Opaque;
    };
    let Ok(MappingDocument::SourceRecord(mapping)) =
        serde_json::from_value::<MappingDocument>(payload)
    else {
        return WriterFootprint::Opaque;
    };
    if !mapping.entitlements.is_empty() || mapping.maps.is_empty() {
        return WriterFootprint::Opaque;
    }
    let mut touched = BTreeSet::new();
    for map in &mapping.maps {
        let pinned_version = artifact
            .dependencies
            .iter()
            .find(|dep| dep.required && dep.artifact_id == map.target.entity)
            .and_then(|dep| dep.version.as_deref());
        let matched: Vec<&ArtifactEnvelope> = pool
            .iter()
            .filter(|candidate| {
                candidate.artifact_id == map.target.entity
                    && pinned_version.is_none_or(|ver| candidate.version == ver)
            })
            .collect();
        let [entity_artifact] = matched.as_slice() else {
            return WriterFootprint::Opaque;
        };
        let Some(Ok(obj)) = index.catalog.object_type(&entity_artifact.artifact_id) else {
            return WriterFootprint::Opaque;
        };
        if !expand_type_hierarchy(obj.name.as_str(), index, &mut touched, work) {
            return WriterFootprint::Opaque;
        }
        if !map.links.is_empty() {
            let Some(declared_links) = index.catalog.links_for_object_type(obj.name.as_str())
            else {
                return WriterFootprint::Opaque;
            };
            for link_map in &map.links {
                let Some(link) = declared_links.iter().find(|l| l.id == link_map.link) else {
                    return WriterFootprint::Opaque;
                };
                if !expand_type_hierarchy(link.target.as_str(), index, &mut touched, work) {
                    return WriterFootprint::Opaque;
                }
            }
        }
    }
    WriterFootprint::Closed(touched)
}

/// Computes the governed and read type footprint of a safety law `L`.
///
/// Includes `L.subject`, every type named in `L.scope` and `L.response`, every
/// concrete subtype/implementer of those types, and—whenever any expression of
/// `L` reads beyond direct non-computed stored properties of `self`—the
/// transitive forward/inverse link closure of those concrete types. Returns
/// `None` (fail closed) on unresolved types, non-state responses, or work-budget
/// exhaustion.
pub(super) fn law_footprint(
    law: &LawIR,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> Option<LawFootprint> {
    let mut named_types = BTreeSet::new();
    named_types.insert(law.subject.clone());
    for ty in &law.scope.object_types {
        named_types.insert(ty.clone());
    }
    for ty in &law.scope.commitments {
        named_types.insert(ty.clone());
    }
    for state in &law.scope.lifecycle_states {
        named_types.insert(state.object_type.clone());
    }

    let mut expressions: Vec<String> = Vec::new();
    if let Some(cond) = &law.condition {
        expressions.push(cond.as_str().to_owned());
    }
    for exception in &law.exceptions {
        expressions.push(exception.condition.as_str().to_owned());
    }
    let mut conservative_links_required = false;
    match &law.response {
        LawResponse::Predicate { expression } => {
            expressions.push(expression.as_str().to_owned());
        }
        LawResponse::Invariant {
            object_type,
            invariant_id,
        } => {
            named_types.insert(object_type.clone());
            let found =
                collect_invariant_expressions(pool, &index.catalog, object_type, invariant_id);
            if found.is_empty() {
                conservative_links_required = true;
            } else {
                expressions.extend(found);
            }
        }
        LawResponse::Transition { object_type, .. } => {
            named_types.insert(object_type.clone());
            conservative_links_required = true;
        }
        LawResponse::TranslationPreserves { target_type, .. } => {
            named_types.insert(target_type.clone());
            conservative_links_required = true;
        }
        LawResponse::Action { .. } => return None,
    }

    let mut concrete_types = BTreeSet::new();
    for name in &named_types {
        charge_work(work, 1)?;
        concrete_types.extend(index.target_types(name)?.iter().cloned());
    }

    let linked_types = collect_linked_footprint_types(
        &concrete_types,
        &expressions,
        conservative_links_required,
        index,
        work,
    )?;
    concrete_types.extend(linked_types);

    let mut subject_and_ancestor_names = named_types;
    for concrete in &concrete_types {
        charge_work(work, 1)?;
        let obj = index.catalog.object_type_named(concrete)?.ok()?;
        subject_and_ancestor_names.insert(concrete.clone());
        for ancestor in &obj.lineage {
            subject_and_ancestor_names.insert(ancestor.as_str().to_owned());
        }
        for iface in &obj.interfaces {
            subject_and_ancestor_names.insert(iface.as_str().to_owned());
        }
    }

    Some(LawFootprint {
        concrete_types,
        subject_and_ancestor_names,
    })
}

fn collect_linked_footprint_types(
    concrete_types: &BTreeSet<String>,
    expressions: &[String],
    conservative_links_required: bool,
    index: &CatalogIndex,
    work: &mut usize,
) -> Option<BTreeSet<String>> {
    let mut visited = BTreeSet::new();
    let mut pending = Vec::new();
    for concrete in concrete_types {
        charge_work(work, 1)?;
        let obj = index.catalog.object_type_named(concrete)?.ok()?;
        let direct_only = !conservative_links_required
            && expressions
                .iter()
                .all(|expr| reads_only_direct_stored_properties(expr, obj));
        if !direct_only && visited.insert(concrete.clone()) {
            pending.push(concrete.clone());
        }
    }
    // Transitive fixed-point closure over precomputed forward and inverse links
    // so multi-hop reads (via computed properties, invariants, or conditions)
    // are covered in linear time under `MAX_PROJECTED_WORK`.
    let mut linked_types = BTreeSet::new();
    while let Some(current) = pending.pop() {
        for neighbors in [
            index.forward_links.get(&current),
            index.inverse_links.get(&current),
        ]
        .into_iter()
        .flatten()
        {
            for neighbor in neighbors {
                charge_work(work, 1)?;
                linked_types.insert(neighbor.clone());
                if visited.insert(neighbor.clone()) {
                    pending.push(neighbor.clone());
                }
            }
        }
    }
    Some(linked_types)
}

fn reads_only_direct_stored_properties(expression: &str, object: &EffectiveObjectType) -> bool {
    let Ok(reads) = expression_reads(expression, "self") else {
        return false;
    };
    if reads.whole_root {
        return false;
    }
    reads.fields.iter().all(|field| {
        object
            .property(field)
            .is_some_and(|prop| !prop.computed && prop.unsupported().is_none())
    })
}

fn collect_invariant_expressions(
    pool: &[ArtifactEnvelope],
    catalog: &EffectiveCatalog,
    object_type: &str,
    invariant_id: &str,
) -> Vec<String> {
    let mut owners = BTreeSet::from([object_type.to_owned()]);
    if let Some(Ok(obj)) = catalog.object_type_named(object_type) {
        for ancestor in &obj.lineage {
            owners.insert(ancestor.as_str().to_owned());
        }
        for iface in &obj.interfaces {
            owners.insert(iface.as_str().to_owned());
        }
    }
    let mut out = Vec::new();
    for artifact in pool {
        if let Decoded::Root(root) = decode_v2(artifact) {
            match *root {
                EntityRoot::V2(entity) if owners.contains(entity.object_type.name.as_str()) => {
                    for inv in &entity.object_type.invariants {
                        if inv.id.as_str() == invariant_id {
                            out.push(inv.expression.as_str().to_owned());
                        }
                    }
                }
                EntityRoot::V2Interface(iface)
                    if owners.contains(iface.interface.name.as_str()) =>
                {
                    for inv in &iface.interface.invariants {
                        if inv.id.as_str() == invariant_id {
                            out.push(inv.expression.as_str().to_owned());
                        }
                    }
                }
                EntityRoot::V2(_)
                | EntityRoot::V2Interface(_)
                | EntityRoot::V2Extension(_)
                | EntityRoot::V2PropertyTypes(_)
                | EntityRoot::V2Markings(_)
                | EntityRoot::V1(_) => {}
            }
        }
    }
    out
}
