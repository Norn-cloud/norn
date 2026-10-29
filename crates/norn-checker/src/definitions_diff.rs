//! Pure comparison of an installed definition set with its candidate.
//!
//! This is the shared diff contract for definition planning and installation.
//! It performs no admission, persistence, or host work; callers admit the
//! candidate with [`crate::artifact_set::check_artifact_set`] first.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ConstraintClass, InterfaceName, InvariantId, InvariantOwner,
    ObjectTypeName,
};
use serde_json::{Value, json};

use crate::entity_instance::{EffectiveCatalog, EffectiveKey, EffectiveLink};

/// The maximum number of IDs included for each ledger diff section.
pub const LEDGER_DIFF_ID_LIMIT: usize = 100;
/// The maximum number of detailed narrowing rows included in one ledger event.
pub const LEDGER_DIFF_NARROWING_LIMIT: usize = 20;
/// The maximum total number of member IDs included across narrowing rows.
pub const LEDGER_DIFF_NARROWING_MEMBER_ID_LIMIT: usize = 64;

fn owned_collection_bound_widened(old: &Value, new: &Value) -> bool {
    let (Ok(mut old), Ok(new)) = (
        serde_json::from_value::<norn_ir::OwnedChildCollection>(old.clone()),
        serde_json::from_value::<norn_ir::OwnedChildCollection>(new.clone()),
    ) else {
        return false;
    };
    let old_max = old.max_rows.0;
    old.max_rows = new.max_rows;
    old == new && new.max_rows.0 >= old_max
}

/// One artifact identified by its stable id and kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DefinitionId {
    /// The artifact envelope id.
    pub artifact_id: String,
    /// The snake-case artifact kind.
    pub kind: String,
}

/// One server-computed narrowing. `ids` contains removed or tightened member
/// identifiers; lifecycle state removals place state names there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Narrowing {
    /// Stable snake-case category, such as `property_removed` or
    /// `lifecycle_state_removed`.
    pub kind: String,
    /// The artifact that owns the changed definition.
    pub artifact_id: String,
    /// The affected object type, when the narrowing concerns an entity model.
    pub object_type: Option<String>,
    /// The affected property, when the narrowing concerns a property lifecycle
    /// or property member.
    pub property: Option<String>,
    /// The affected member, state, transition, or link identifiers.
    pub ids: Vec<String>,
    /// Short human-readable explanation for refusal responses.
    pub detail: String,
}

impl Narrowing {
    /// The full refusal representation (the ledger uses the bounded summary
    /// returned by [`DefinitionDiff::to_json`] instead).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "artifactId": self.artifact_id,
            "objectType": self.object_type,
            "property": self.property,
            "ids": self.ids,
            "detail": self.detail,
        })
    }
}

/// A deterministic diff from the installed set to the candidate set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefinitionDiff {
    /// Artifacts present only in the candidate.
    pub added: Vec<DefinitionId>,
    /// Artifacts present only in the installed set.
    pub removed: Vec<DefinitionId>,
    /// Semantically changed artifacts (provenance-only changes are excluded).
    pub changed: Vec<DefinitionId>,
    /// Every removed structure or tighter lifecycle rule.
    pub narrowing: Vec<Narrowing>,
}

impl DefinitionDiff {
    /// A bounded JSON summary for append-only ledger events.
    ///
    /// The count fields always report full counts. Artifact lists, narrowing
    /// rows, and member IDs have fixed limits, and every identifier is capped
    /// at 128 UTF-8 bytes, so a large definition set cannot inflate a ledger
    /// event without bound.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let ids = |items: &[DefinitionId]| {
            items
                .iter()
                .take(LEDGER_DIFF_ID_LIMIT)
                .map(|item| {
                    json!({
                        "artifactId": bounded(&item.artifact_id, 128),
                        "kind": bounded(&item.kind, 64),
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut member_ids_remaining = LEDGER_DIFF_NARROWING_MEMBER_ID_LIMIT;
        let narrowing = self
            .narrowing
            .iter()
            .take(LEDGER_DIFF_NARROWING_LIMIT)
            .map(|item| {
                let member_id_count = item.ids.len().min(8).min(member_ids_remaining);
                member_ids_remaining = member_ids_remaining.saturating_sub(member_id_count);
                json!({
                    "kind": bounded(&item.kind, 64),
                    "artifactId": bounded(&item.artifact_id, 128),
                    "objectType": item.object_type.as_deref().map(|value| bounded(value, 128)),
                    "property": item.property.as_deref().map(|value| bounded(value, 128)),
                    "ids": item.ids.iter().take(member_id_count)
                        .map(|value| bounded(value, 128)).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "addedCount": self.added.len(),
            "added": ids(&self.added),
            "removedCount": self.removed.len(),
            "removed": ids(&self.removed),
            "changedCount": self.changed.len(),
            "changed": ids(&self.changed),
            "narrowingCount": self.narrowing.len(),
            "narrowing": narrowing,
        })
    }

    /// Full narrowing entries for an install refusal response.
    #[must_use]
    pub fn narrowing_json(&self) -> Vec<Value> {
        self.narrowing.iter().map(Narrowing::to_json).collect()
    }
}

fn bounded(value: &str, max_bytes: usize) -> String {
    let end = value
        .char_indices()
        .map(|(index, character)| index.saturating_add(character.len_utf8()))
        .take_while(|end| *end <= max_bytes)
        .last()
        .unwrap_or(0);
    value.get(..end).unwrap_or_default().to_owned()
}

/// Compare the old admitted set with the candidate set.
///
/// The result is independent of input order. Artifact envelope provenance is
/// presentation metadata and does not make an artifact semantically changed.
#[must_use]
pub fn diff_definition_sets(
    previous: &[ArtifactEnvelope],
    candidate: &[ArtifactEnvelope],
) -> DefinitionDiff {
    let old = artifact_index(previous);
    let new = artifact_index(candidate);
    let mut diff = DefinitionDiff::default();

    for (artifact_id, artifact) in &new {
        match old.get(artifact_id) {
            None => diff.added.push(definition_id(artifact)),
            Some(before) if !same_definition(before, artifact) => {
                diff.changed.push(definition_id(artifact));
                diff_changed_artifact(before, artifact, &mut diff.narrowing);
            }
            Some(_) => {}
        }
    }
    for (artifact_id, artifact) in &old {
        if !new.contains_key(artifact_id) {
            diff.removed.push(definition_id(artifact));
            artifact_removal(artifact, &mut diff.narrowing);
        }
    }

    let previous_model = EntityModelSnapshot::from_artifacts(previous);
    let candidate_model = EntityModelSnapshot::from_artifacts(candidate);
    diff_entity_members(&previous_model, &candidate_model, &mut diff.narrowing);
    diff_effective_contracts(
        previous,
        candidate,
        &previous_model,
        &candidate_model,
        &mut diff.narrowing,
    );
    diff_effective_lifecycles(&previous_model, &candidate_model, &mut diff.narrowing);
    diff_effective_invariants(&previous_model, &candidate_model, &mut diff.narrowing);

    diff.added.sort();
    diff.removed.sort();
    diff.changed.sort();
    diff.narrowing.sort_by(|left, right| {
        (
            left.artifact_id.as_str(),
            left.object_type.as_deref().unwrap_or_default(),
            left.property.as_deref().unwrap_or_default(),
            left.kind.as_str(),
            left.ids.as_slice(),
        )
            .cmp(&(
                right.artifact_id.as_str(),
                right.object_type.as_deref().unwrap_or_default(),
                right.property.as_deref().unwrap_or_default(),
                right.kind.as_str(),
                right.ids.as_slice(),
            ))
    });
    diff.narrowing.dedup();
    diff
}

#[allow(
    clippy::too_many_lines,
    reason = "the compatibility check reports all effective contract changes in one traversal"
)]
fn diff_effective_contracts(
    previous_artifacts: &[ArtifactEnvelope],
    candidate_artifacts: &[ArtifactEnvelope],
    previous: &EntityModelSnapshot,
    candidate: &EntityModelSnapshot,
    out: &mut Vec<Narrowing>,
) {
    let previous_catalog = EffectiveCatalog::from_artifacts(previous_artifacts);
    let candidate_catalog = EffectiveCatalog::from_artifacts(candidate_artifacts);
    for name in previous.object_types.keys() {
        let Some(new_definition) = candidate.object_types.get(name) else {
            continue;
        };
        let Some(Ok(old)) = previous_catalog.object_type_named(name) else {
            continue;
        };
        let Some(Ok(new)) = candidate_catalog.object_type_named(name) else {
            continue;
        };
        let artifact_id = &new_definition.artifact_id;
        let old_properties = old
            .properties
            .iter()
            .map(|property| (property.id.as_str(), property))
            .collect::<BTreeMap<_, _>>();
        for property in &new.properties {
            let Some(previous_property) = old_properties.get(property.id.as_str()) else {
                continue;
            };
            let reasons = property.narrowing_reasons_from(previous_property);
            if !reasons.is_empty() {
                push_narrowing(
                    out,
                    "property_contract_narrowed",
                    artifact_id,
                    Some(name),
                    Some(property.id.as_str()),
                    vec![property.id.as_str().to_owned()],
                    format!(
                        "effective property {name}.{} narrowed: {}",
                        property.id.as_str(),
                        reasons.join(", ")
                    ),
                );
            }
        }

        diff_effective_naming_series(name, artifact_id, &old.keys, &new.keys, out);

        let Some(old_links) = previous_catalog.links_for_object_type(name) else {
            continue;
        };
        let Some(new_links) = candidate_catalog.links_for_object_type(name) else {
            continue;
        };
        let old_collections = previous.effective_owned_collections(name, &mut BTreeSet::new());
        let new_collections = candidate.effective_owned_collections(name, &mut BTreeSet::new());
        for (id, old) in &old_collections {
            match new_collections.get(id) {
                Some(new) if new == old || owned_collection_bound_widened(old, new) => {}
                Some(_) => push_narrowing(
                    out,
                    "owned_collection_contract_narrowed",
                    artifact_id,
                    Some(name),
                    None,
                    vec![id.clone()],
                    format!(
                        "owned collection {name}.{id} changed its row identity, ordering, bound, or cascade contract"
                    ),
                ),
                None => push_narrowing(
                    out,
                    "owned_collection_removed",
                    artifact_id,
                    Some(name),
                    None,
                    vec![id.clone()],
                    format!("owned collection {name}.{id} was removed"),
                ),
            }
        }
        for id in new_collections
            .keys()
            .filter(|id| !old_collections.contains_key(*id))
        {
            push_narrowing(
                out,
                "owned_collection_added",
                artifact_id,
                Some(name),
                None,
                vec![id.clone()],
                format!("owned collection {name}.{id} was added"),
            );
        }
        let old_links = old_links
            .iter()
            .map(|link| (link.id.as_str(), link))
            .collect::<BTreeMap<_, _>>();
        for link in new_links {
            let Some(previous_link) = old_links.get(link.id.as_str()) else {
                if link.cardinality.required {
                    push_narrowing(
                        out,
                        "link_contract_narrowed",
                        artifact_id,
                        Some(name),
                        None,
                        vec![link.id.as_str().to_owned()],
                        format!(
                            "new required link {name}.{} requires existing objects to have a link",
                            link.id.as_str()
                        ),
                    );
                }
                continue;
            };
            diff_effective_link(
                name,
                artifact_id,
                previous_link,
                link,
                &previous_catalog,
                &candidate_catalog,
                out,
            );
        }
    }
}

fn diff_effective_naming_series(
    object_type: &str,
    artifact_id: &str,
    previous: &[EffectiveKey],
    candidate: &[EffectiveKey],
    out: &mut Vec<Narrowing>,
) {
    // There is at most one effective naming series per type. Compare that
    // effective key, including its declaration owner, so reparenting a child
    // between series-bearing parents cannot bypass the migration gate.
    let old = previous.iter().find(|key| key.naming_series.is_some());
    let new = candidate.iter().find(|key| key.naming_series.is_some());
    let old_contract = old.map(|key| {
        (
            &key.declared_by,
            &key.name,
            &key.components,
            &key.naming_series,
        )
    });
    let new_contract = new.map(|key| {
        (
            &key.declared_by,
            &key.name,
            &key.components,
            &key.naming_series,
        )
    });
    if old_contract == new_contract {
        return;
    }

    let property = new
        .or(old)
        .and_then(|key| key.components.first())
        .map(norn_ir::PropertyId::as_str);
    let mut key_names = BTreeSet::new();
    if let Some(old) = old {
        key_names.insert(old.name.as_str().to_owned());
    }
    if let Some(new) = new {
        key_names.insert(new.name.as_str().to_owned());
    }
    let owners = format!(
        "from {:?} to {:?}",
        old.map(|key| format!("{}.{}", key.declared_by.as_str(), key.name.as_str())),
        new.map(|key| format!("{}.{}", key.declared_by.as_str(), key.name.as_str()))
    );
    push_narrowing(
        out,
        "naming_series_contract_changed",
        artifact_id,
        Some(object_type),
        property,
        key_names.into_iter().collect(),
        format!(
            "effective naming series for {object_type} changed {owners}; existing numbers and counter facts require a versioned migration"
        ),
    );
}

fn diff_effective_link(
    object_type: &str,
    artifact_id: &str,
    previous: &EffectiveLink,
    candidate: &EffectiveLink,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
    out: &mut Vec<Narrowing>,
) {
    let link_id = candidate.id.as_str();
    if link_target_narrows(
        candidate.target.as_str(),
        previous.target.as_str(),
        previous_catalog,
        candidate_catalog,
    ) != Some(false)
    {
        push_narrowing(
            out,
            "link_target_narrowed",
            artifact_id,
            Some(object_type),
            None,
            vec![link_id.to_owned()],
            format!("link {object_type}.{link_id} changed target"),
        );
    }
    if !previous.acyclic && candidate.acyclic {
        push_narrowing(
            out,
            "link_acyclic_added",
            artifact_id,
            Some(object_type),
            None,
            vec![link_id.to_owned()],
            format!("link {object_type}.{link_id} now requires an acyclic graph"),
        );
    }
    let old = previous.cardinality;
    let new = candidate.cardinality;
    if (old.to == norn_ir::Multiplicity::Many && new.to == norn_ir::Multiplicity::One)
        || (old.from == norn_ir::Multiplicity::Many && new.from == norn_ir::Multiplicity::One)
        || (!old.required && new.required)
    {
        push_narrowing(
            out,
            "link_cardinality_narrowed",
            artifact_id,
            Some(object_type),
            None,
            vec![link_id.to_owned()],
            format!("link {object_type}.{link_id} cardinality narrowed"),
        );
    }
    if previous.inverse.is_some() && previous.inverse != candidate.inverse {
        push_narrowing(
            out,
            "link_inverse_changed",
            artifact_id,
            Some(object_type),
            None,
            vec![link_id.to_owned()],
            format!("link {object_type}.{link_id} changed its inverse role"),
        );
    }
    diff_effective_link_properties(object_type, artifact_id, previous, candidate, out);
}

/// Narrowed or removed properties carried on an effective link.
fn diff_effective_link_properties(
    object_type: &str,
    artifact_id: &str,
    previous: &EffectiveLink,
    candidate: &EffectiveLink,
    out: &mut Vec<Narrowing>,
) {
    let link_id = candidate.id.as_str();
    let old_properties = previous
        .properties
        .iter()
        .map(|property| (property.id.as_str(), property))
        .collect::<BTreeMap<_, _>>();
    for property in &candidate.properties {
        let Some(previous_property) = old_properties.get(property.id.as_str()) else {
            continue;
        };
        let reasons = property.narrowing_reasons_from(previous_property);
        if !reasons.is_empty() {
            push_narrowing(
                out,
                "link_property_contract_narrowed",
                artifact_id,
                Some(object_type),
                Some(property.id.as_str()),
                vec![link_id.to_owned(), property.id.as_str().to_owned()],
                format!(
                    "effective link property {object_type}.{link_id}.{} narrowed: {}",
                    property.id.as_str(),
                    reasons.join(", ")
                ),
            );
        }
    }
    for property in &previous.properties {
        if !candidate
            .properties
            .iter()
            .any(|current| current.id == property.id)
        {
            push_narrowing(
                out,
                "link_property_removed",
                artifact_id,
                Some(object_type),
                Some(property.id.as_str()),
                vec![link_id.to_owned()],
                format!(
                    "link property {object_type}.{link_id}.{} removed",
                    property.id.as_str()
                ),
            );
        }
    }
}

fn link_target_narrows(
    candidate: &str,
    previous: &str,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Option<bool> {
    match (
        previous_catalog.target_types(previous),
        candidate_catalog.target_types(candidate),
    ) {
        (Some(previous_types), Some(candidate_types)) => {
            Some(!previous_types.is_subset(&candidate_types))
        }
        // Registry-owned target types are outside the tenant artifact set.
        // An unchanged external target remains stable; a changed target we
        // cannot resolve is conservatively treated as unknown by the caller.
        _ if candidate == previous => Some(false),
        _ => None,
    }
}

fn artifact_index(artifacts: &[ArtifactEnvelope]) -> BTreeMap<&str, &ArtifactEnvelope> {
    artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect()
}

fn same_definition(left: &ArtifactEnvelope, right: &ArtifactEnvelope) -> bool {
    left.ir_version == right.ir_version
        && left.artifact_id == right.artifact_id
        && left.namespace == right.namespace
        && left.name == right.name
        && left.version == right.version
        && left.lifecycle == right.lifecycle
        && left.scope == right.scope
        && left.dependencies == right.dependencies
        && left.payload == right.payload
}

fn kind_name(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Entity => "entity",
        ArtifactKind::Relation => "relation",
        ArtifactKind::Action => "action",
        ArtifactKind::Rule => "rule",
        ArtifactKind::Connector => "connector",
        ArtifactKind::Widget => "widget",
        ArtifactKind::Customization => "customization",
        ArtifactKind::Setting => "setting",
        ArtifactKind::BusinessPack => "business_pack",
        ArtifactKind::Policy => "policy",
        ArtifactKind::Derived => "derived",
        ArtifactKind::SourceSchema => "source_schema",
        ArtifactKind::Mapping => "mapping",
        ArtifactKind::Law => "law",
    }
}

fn definition_id(artifact: &ArtifactEnvelope) -> DefinitionId {
    DefinitionId {
        artifact_id: artifact.artifact_id.clone(),
        kind: kind_name(&artifact.kind()).to_owned(),
    }
}

fn push_narrowing(
    out: &mut Vec<Narrowing>,
    kind: &str,
    artifact_id: &str,
    object_type: Option<&str>,
    property: Option<&str>,
    ids: Vec<String>,
    detail: String,
) {
    out.push(Narrowing {
        kind: kind.to_owned(),
        artifact_id: artifact_id.to_owned(),
        object_type: object_type.map(str::to_owned),
        property: property.map(str::to_owned),
        ids,
        detail,
    });
}

fn artifact_removal(artifact: &ArtifactEnvelope, out: &mut Vec<Narrowing>) {
    let kind = kind_name(&artifact.kind());
    push_narrowing(
        out,
        "artifact_removed",
        &artifact.artifact_id,
        None,
        None,
        vec![artifact.artifact_id.clone()],
        format!("{kind} artifact removed"),
    );
    let payload = payload_value(artifact);
    match artifact.kind() {
        ArtifactKind::Action => {
            let action_id = payload
                .get("actionId")
                .and_then(Value::as_str)
                .unwrap_or(&artifact.artifact_id);
            push_narrowing(
                out,
                "action_removed",
                &artifact.artifact_id,
                None,
                None,
                vec![action_id.to_owned()],
                format!("action {action_id:?} removed"),
            );
        }
        ArtifactKind::Mapping => {
            let map_ids = payload
                .get("maps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|map| map.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            push_narrowing(
                out,
                "mapping_removed",
                &artifact.artifact_id,
                None,
                None,
                map_ids,
                "mapping artifact removed".to_owned(),
            );
        }
        ArtifactKind::Relation => {
            let source = payload.get("from").and_then(Value::as_str);
            push_narrowing(
                out,
                "link_removed",
                &artifact.artifact_id,
                source,
                None,
                vec![artifact.artifact_id.clone()],
                "RelationIR link artifact removed".to_owned(),
            );
        }
        ArtifactKind::Entity
        | ArtifactKind::Rule
        | ArtifactKind::Connector
        | ArtifactKind::Widget
        | ArtifactKind::Customization
        | ArtifactKind::Setting
        | ArtifactKind::BusinessPack
        | ArtifactKind::Policy
        | ArtifactKind::Derived
        | ArtifactKind::SourceSchema
        | ArtifactKind::Law => {}
    }
}

fn diff_changed_artifact(
    previous: &ArtifactEnvelope,
    candidate: &ArtifactEnvelope,
    out: &mut Vec<Narrowing>,
) {
    let old_payload = payload_value(previous);
    let new_payload = payload_value(candidate);
    match (previous.kind(), candidate.kind()) {
        (ArtifactKind::Action, ArtifactKind::Action) => {
            let old_id = old_payload
                .get("actionId")
                .and_then(Value::as_str)
                .unwrap_or(&previous.artifact_id);
            let new_id = new_payload
                .get("actionId")
                .and_then(Value::as_str)
                .unwrap_or(&candidate.artifact_id);
            if old_id != new_id {
                push_narrowing(
                    out,
                    "action_removed",
                    &previous.artifact_id,
                    None,
                    None,
                    vec![old_id.to_owned()],
                    format!("action {old_id:?} replaced by {new_id:?}"),
                );
            }
        }
        (ArtifactKind::Mapping, ArtifactKind::Mapping) => {
            let old_maps = named_array(old_payload.get("maps"), "id");
            let new_maps = named_array(new_payload.get("maps"), "id");
            for map_id in old_maps.keys() {
                if !new_maps.contains_key(map_id) {
                    push_narrowing(
                        out,
                        "mapping_removed",
                        &previous.artifact_id,
                        None,
                        None,
                        vec![map_id.clone()],
                        format!("mapping object map {map_id:?} removed"),
                    );
                }
            }
        }
        (ArtifactKind::Law, ArtifactKind::Law) => {
            if old_payload != new_payload {
                push_narrowing(out, "law_contract_changed", &previous.artifact_id, None, None, Vec::new(), "protected law contract changed; retain inherited laws and add conjunctive identities for tightening".to_owned());
            }
        }
        (ArtifactKind::Action | ArtifactKind::Mapping | ArtifactKind::Law, _) => {
            artifact_removal(previous, out);
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Default)]
struct ObjectTypeSnapshot {
    artifact_id: String,
    extends: Option<String>,
    implements: Vec<String>,
    properties: BTreeMap<String, Value>,
    links: BTreeMap<String, Value>,
    owned_collections: BTreeMap<String, Value>,
    lifecycles: BTreeMap<String, Value>,
    invariants: BTreeMap<String, InvariantSnapshot>,
}

#[derive(Debug, Clone, Default)]
struct InterfaceSnapshot {
    artifact_id: String,
    invariants: BTreeMap<String, InvariantSnapshot>,
}

#[derive(Debug, Clone)]
struct InvariantSnapshot {
    classification: ConstraintClass,
    expression: Value,
}

#[derive(Debug, Clone)]
struct EffectiveInvariantSnapshot {
    artifact_id: String,
    invariant: InvariantSnapshot,
}

#[derive(Debug, Clone, Default)]
struct ExtensionSnapshot {
    properties: BTreeMap<String, Value>,
    links: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
struct EntityModelSnapshot {
    object_types: BTreeMap<String, ObjectTypeSnapshot>,
    interfaces: BTreeMap<String, InterfaceSnapshot>,
    extensions: BTreeMap<String, Vec<ExtensionSnapshot>>,
}

impl EntityModelSnapshot {
    fn from_artifacts(artifacts: &[ArtifactEnvelope]) -> Self {
        let mut ordered = artifacts.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        let mut model = Self::default();
        for artifact in ordered {
            if !matches!(artifact.kind(), ArtifactKind::Entity) {
                continue;
            }
            let payload = payload_value(artifact);
            if let Some(object_type) = payload.get("objectType").and_then(Value::as_object) {
                let Some(name) = object_type.get("name").and_then(Value::as_str) else {
                    continue;
                };
                model.object_types.insert(
                    name.to_owned(),
                    ObjectTypeSnapshot {
                        artifact_id: artifact.artifact_id.clone(),
                        extends: object_type
                            .get("extends")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        implements: strings(object_type.get("implements")),
                        properties: named_array(object_type.get("properties"), "id"),
                        links: named_array(object_type.get("links"), "id"),
                        owned_collections: named_array(object_type.get("ownedCollections"), "id"),
                        lifecycles: named_array(object_type.get("lifecycles"), "property"),
                        invariants: invariant_array(object_type.get("invariants")),
                    },
                );
            } else if let Some(interface) = payload.get("interface").and_then(Value::as_object) {
                let Some(name) = interface.get("name").and_then(Value::as_str) else {
                    continue;
                };
                model.interfaces.insert(
                    name.to_owned(),
                    InterfaceSnapshot {
                        artifact_id: artifact.artifact_id.clone(),
                        invariants: invariant_array(interface.get("invariants")),
                    },
                );
            } else if let Some(object_kind) = payload.get("objectKind").and_then(Value::as_str) {
                model.object_types.insert(
                    object_kind.to_owned(),
                    ObjectTypeSnapshot {
                        artifact_id: artifact.artifact_id.clone(),
                        properties: payload
                            .get("fields")
                            .and_then(Value::as_object)
                            .map(|fields| {
                                fields
                                    .iter()
                                    .map(|(name, value)| (name.clone(), value.clone()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        ..ObjectTypeSnapshot::default()
                    },
                );
            } else if let Some(extension) = payload
                .get("objectTypeExtension")
                .and_then(Value::as_object)
            {
                let Some(target) = extension.get("target").and_then(Value::as_str) else {
                    continue;
                };
                model
                    .extensions
                    .entry(target.to_owned())
                    .or_default()
                    .push(ExtensionSnapshot {
                        properties: named_array(extension.get("properties"), "id"),
                        links: named_array(extension.get("links"), "id"),
                    });
            }
        }
        model
    }

    fn effective_properties(
        &self,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> BTreeMap<String, Value> {
        if !seen.insert(name.to_owned()) {
            return BTreeMap::new();
        }
        let Some(definition) = self.object_types.get(name) else {
            seen.remove(name);
            return BTreeMap::new();
        };
        let mut members = definition
            .extends
            .as_deref()
            .map(|parent| self.effective_properties(parent, seen))
            .unwrap_or_default();
        members.extend(definition.properties.clone());
        if let Some(extensions) = self.extensions.get(name) {
            for extension in extensions {
                members.extend(extension.properties.clone());
            }
        }
        seen.remove(name);
        members
    }

    fn effective_links(&self, name: &str, seen: &mut BTreeSet<String>) -> BTreeMap<String, Value> {
        if !seen.insert(name.to_owned()) {
            return BTreeMap::new();
        }
        let Some(definition) = self.object_types.get(name) else {
            seen.remove(name);
            return BTreeMap::new();
        };
        let mut members = definition
            .extends
            .as_deref()
            .map(|parent| self.effective_links(parent, seen))
            .unwrap_or_default();
        members.extend(definition.links.clone());
        if let Some(extensions) = self.extensions.get(name) {
            for extension in extensions {
                members.extend(extension.links.clone());
            }
        }
        seen.remove(name);
        members
    }

    fn effective_owned_collections(
        &self,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> BTreeMap<String, Value> {
        if !seen.insert(name.to_owned()) {
            return BTreeMap::new();
        }
        let Some(definition) = self.object_types.get(name) else {
            seen.remove(name);
            return BTreeMap::new();
        };
        let mut members = definition
            .extends
            .as_deref()
            .map(|parent| self.effective_owned_collections(parent, seen))
            .unwrap_or_default();
        members.extend(definition.owned_collections.clone());
        seen.remove(name);
        members
    }

    fn effective_lifecycles(
        &self,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> BTreeMap<String, LifecycleSnapshot> {
        if !seen.insert(name.to_owned()) {
            return BTreeMap::new();
        }
        let Some(definition) = self.object_types.get(name) else {
            seen.remove(name);
            return BTreeMap::new();
        };
        let properties = self.effective_properties(name, &mut BTreeSet::new());
        let states = properties
            .iter()
            .map(|(property, value)| (property.clone(), enum_values(value)))
            .collect::<BTreeMap<_, _>>();
        let mut machines = definition
            .extends
            .as_deref()
            .map(|parent| self.effective_lifecycles(parent, seen))
            .unwrap_or_default();
        machines.retain(|property, _machine| states.contains_key(property));
        for (property, machine) in &mut machines {
            if let Some(property_states) = states.get(property) {
                machine.restrict(property_states);
            }
        }
        for (property, value) in &definition.lifecycles {
            let property_states = states.get(property).cloned().unwrap_or_default();
            let mut declared = LifecycleSnapshot::from_value(value, &property_states);
            if let Some(inherited) = machines.get(property) {
                for (id, transition) in &mut declared.transitions {
                    if let Some(parent) = inherited.transitions.get(id) {
                        transition.guards.extend(parent.guards.iter().cloned());
                        transition.guards.sort();
                        transition.guards.dedup();
                    }
                }
            }
            machines.insert(property.clone(), declared);
        }
        seen.remove(name);
        machines
    }

    fn effective_invariants(
        &self,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> BTreeMap<(InvariantOwner, InvariantId), EffectiveInvariantSnapshot> {
        if !seen.insert(name.to_owned()) {
            return BTreeMap::new();
        }
        let Some(definition) = self.object_types.get(name) else {
            seen.remove(name);
            return BTreeMap::new();
        };
        let mut invariants = definition
            .extends
            .as_deref()
            .map(|parent| self.effective_invariants(parent, seen))
            .unwrap_or_default();
        for (id, invariant) in &definition.invariants {
            invariants.insert(
                (
                    InvariantOwner::Object(ObjectTypeName(name.to_owned())),
                    InvariantId(id.clone()),
                ),
                EffectiveInvariantSnapshot {
                    artifact_id: definition.artifact_id.clone(),
                    invariant: invariant.clone(),
                },
            );
        }
        for interface_name in &definition.implements {
            let Some(interface) = self.interfaces.get(interface_name) else {
                continue;
            };
            for (id, invariant) in &interface.invariants {
                invariants.insert(
                    (
                        InvariantOwner::Interface(InterfaceName(interface_name.clone())),
                        InvariantId(id.clone()),
                    ),
                    EffectiveInvariantSnapshot {
                        artifact_id: interface.artifact_id.clone(),
                        invariant: invariant.clone(),
                    },
                );
            }
        }
        seen.remove(name);
        invariants
    }
}

fn payload_value(artifact: &ArtifactEnvelope) -> Value {
    serde_json::to_value(artifact)
        .ok()
        .and_then(|envelope| envelope.get("payload").cloned())
        .unwrap_or(Value::Null)
}

fn named_array(value: Option<&Value>, key: &str) -> BTreeMap<String, Value> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|member| {
            let id = member.get(key)?.as_str()?.to_owned();
            Some((id, member.clone()))
        })
        .collect()
}

fn invariant_array(value: Option<&Value>) -> BTreeMap<String, InvariantSnapshot> {
    named_array(value, "id")
        .into_iter()
        .map(|(id, invariant)| {
            // Omitted classifications are the historical integrity default.
            // Treat malformed values conservatively: the caller admits the
            // candidate separately, and the diff must not hide a narrowing.
            let classification = match invariant.get("classification").and_then(Value::as_str) {
                Some("quality") => ConstraintClass::Quality,
                _ => ConstraintClass::Integrity,
            };
            let expression = invariant.get("expression").cloned().unwrap_or(Value::Null);
            (
                id,
                InvariantSnapshot {
                    classification,
                    expression,
                },
            )
        })
        .collect()
}

fn enum_values(property: &Value) -> BTreeSet<String> {
    let Some(ty) = property.get("type") else {
        return BTreeSet::new();
    };
    if ty.get("kind").and_then(Value::as_str) != Some("enum") {
        return BTreeSet::new();
    }
    ty.get("values")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.get("value")?.as_str().map(str::to_owned))
        .collect()
}

fn strings(values: Option<&Value>) -> Vec<String> {
    values
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn diff_entity_members(
    previous: &EntityModelSnapshot,
    candidate: &EntityModelSnapshot,
    out: &mut Vec<Narrowing>,
) {
    for (name, before) in &previous.object_types {
        let Some(after) = candidate.object_types.get(name) else {
            push_narrowing(
                out,
                "object_type_removed",
                &before.artifact_id,
                Some(name),
                None,
                vec![name.clone()],
                format!("object type {name:?} removed"),
            );
            continue;
        };
        let previous_properties = previous.effective_properties(name, &mut BTreeSet::new());
        let candidate_properties = candidate.effective_properties(name, &mut BTreeSet::new());
        for property in previous_properties.keys() {
            if !candidate_properties.contains_key(property) {
                push_narrowing(
                    out,
                    "property_removed",
                    &after.artifact_id,
                    Some(name),
                    Some(property),
                    vec![property.clone()],
                    format!("property {name}.{property} removed"),
                );
            }
        }
        let previous_links = previous.effective_links(name, &mut BTreeSet::new());
        let candidate_links = candidate.effective_links(name, &mut BTreeSet::new());
        for (link, old_link) in &previous_links {
            let Some(new_link) = candidate_links.get(link) else {
                push_narrowing(
                    out,
                    "link_removed",
                    &after.artifact_id,
                    Some(name),
                    None,
                    vec![link.clone()],
                    format!("link {name}.{link} removed"),
                );
                continue;
            };
            let old_properties = named_array(old_link.get("properties"), "id");
            let new_properties = named_array(new_link.get("properties"), "id");
            for property in old_properties.keys() {
                if !new_properties.contains_key(property) {
                    push_narrowing(
                        out,
                        "link_property_removed",
                        &after.artifact_id,
                        Some(name),
                        Some(property),
                        vec![link.clone()],
                        format!("link property {name}.{link}.{property} removed"),
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TransitionSnapshot {
    from: BTreeSet<String>,
    to: String,
    guards: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LifecycleSnapshot {
    states: BTreeSet<String>,
    initial: BTreeSet<String>,
    terminal: BTreeSet<String>,
    transitions: BTreeMap<String, TransitionSnapshot>,
}

impl LifecycleSnapshot {
    fn from_value(value: &Value, states: &BTreeSet<String>) -> Self {
        let transitions = value
            .get("transitions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|transition| {
                let id = transition.get("id")?.as_str()?.to_owned();
                Some((
                    id,
                    TransitionSnapshot {
                        from: strings(transition.get("from")).into_iter().collect(),
                        to: transition
                            .get("to")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        guards: strings(transition.get("guards")),
                    },
                ))
            })
            .collect();
        let mut machine = Self {
            states: states.clone(),
            initial: strings(value.get("initial")).into_iter().collect(),
            terminal: strings(value.get("terminal")).into_iter().collect(),
            transitions,
        };
        machine.restrict(states);
        machine
    }

    fn restrict(&mut self, states: &BTreeSet<String>) {
        self.states.clone_from(states);
        self.initial.retain(|state| states.contains(state));
        self.terminal.retain(|state| states.contains(state));
        self.transitions.retain(|_, transition| {
            transition.from.retain(|state| states.contains(state));
            states.contains(&transition.to) && !transition.from.is_empty()
        });
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one ordered pass compares every effective lifecycle restriction"
)]
fn diff_effective_lifecycles(
    previous: &EntityModelSnapshot,
    candidate: &EntityModelSnapshot,
    out: &mut Vec<Narrowing>,
) {
    for name in previous.object_types.keys() {
        let Some(after_type) = candidate.object_types.get(name) else {
            continue;
        };
        let before = previous.effective_lifecycles(name, &mut BTreeSet::new());
        let after = candidate.effective_lifecycles(name, &mut BTreeSet::new());
        for (property, next) in &after {
            let Some(old) = before.get(property) else {
                if previous
                    .effective_properties(name, &mut BTreeSet::new())
                    .contains_key(property)
                {
                    push_narrowing(
                        out,
                        "lifecycle_added",
                        &after_type.artifact_id,
                        Some(name),
                        Some(property),
                        vec![property.clone()],
                        format!("lifecycle added to existing type {name:?}.{property}"),
                    );
                }
                continue;
            };
            let removed_states = difference(&old.states, &next.states);
            if !removed_states.is_empty() {
                push_narrowing(
                    out,
                    "lifecycle_state_removed",
                    &after_type.artifact_id,
                    Some(name),
                    Some(property),
                    removed_states,
                    format!("lifecycle states removed from {name}.{property}"),
                );
            }
            push_set_removal(
                out,
                "lifecycle_initial_removed",
                "initial states removed",
                &after_type.artifact_id,
                name,
                property,
                &old.initial,
                &next.initial,
            );
            push_set_addition(
                out,
                "lifecycle_terminal_added",
                "terminal states added",
                &after_type.artifact_id,
                name,
                property,
                &old.terminal,
                &next.terminal,
            );
            for (transition, old_transition) in &old.transitions {
                let Some(new_transition) = next.transitions.get(transition) else {
                    push_narrowing(
                        out,
                        "lifecycle_transition_removed",
                        &after_type.artifact_id,
                        Some(name),
                        Some(property),
                        vec![transition.clone()],
                        format!("transition {transition:?} removed from {name}.{property}"),
                    );
                    continue;
                };
                let removed_sources = difference(&old_transition.from, &new_transition.from);
                if !removed_sources.is_empty() {
                    push_narrowing(
                        out,
                        "lifecycle_source_state_removed",
                        &after_type.artifact_id,
                        Some(name),
                        Some(property),
                        removed_sources,
                        format!("transition {transition:?} loses source states"),
                    );
                }
                if old_transition.to != new_transition.to {
                    push_narrowing(
                        out,
                        "lifecycle_transition_target_changed",
                        &after_type.artifact_id,
                        Some(name),
                        Some(property),
                        vec![transition.clone()],
                        format!(
                            "transition {transition:?} now targets {:?} instead of {:?}",
                            new_transition.to, old_transition.to
                        ),
                    );
                }
                let added_guards = new_transition
                    .guards
                    .iter()
                    .filter(|guard| !old_transition.guards.contains(guard))
                    .count();
                if added_guards > 0 {
                    push_narrowing(
                        out,
                        "lifecycle_guard_added",
                        &after_type.artifact_id,
                        Some(name),
                        Some(property),
                        vec![transition.clone()],
                        format!("transition {transition:?} adds {added_guards} guard(s)"),
                    );
                }
            }
        }
    }
}

fn diff_effective_invariants(
    previous: &EntityModelSnapshot,
    candidate: &EntityModelSnapshot,
    out: &mut Vec<Narrowing>,
) {
    for name in previous.object_types.keys() {
        if !candidate.object_types.contains_key(name) {
            continue;
        }
        let before = previous.effective_invariants(name, &mut BTreeSet::new());
        let after = candidate.effective_invariants(name, &mut BTreeSet::new());
        for ((owner, id), next) in after {
            if next.invariant.classification != ConstraintClass::Integrity {
                continue;
            }
            let reason = match before.get(&(owner.clone(), id.clone())) {
                None => Some("integrity invariant added"),
                Some(old) if old.invariant.classification == ConstraintClass::Quality => {
                    Some("quality invariant changed to integrity")
                }
                Some(old) if old.invariant.expression != next.invariant.expression => {
                    Some("integrity invariant expression changed")
                }
                Some(_) => None,
            };
            if let Some(reason) = reason {
                push_narrowing(
                    out,
                    "integrity_invariant_narrowed",
                    &next.artifact_id,
                    Some(name),
                    None,
                    vec![id.as_str().to_owned()],
                    format!(
                        "{reason}: {}.{} applies to {name}",
                        invariant_owner_name(&owner),
                        id.as_str()
                    ),
                );
            }
        }
    }
}

fn invariant_owner_name(owner: &InvariantOwner) -> &str {
    match owner {
        InvariantOwner::Object(name) => name.as_str(),
        InvariantOwner::Interface(name) => name.as_str(),
    }
}

fn difference(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Vec<String> {
    left.difference(right).cloned().collect()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the diagnostic carries its category, owner and compared state sets"
)]
fn push_set_removal(
    out: &mut Vec<Narrowing>,
    kind: &str,
    label: &str,
    artifact_id: &str,
    object_type: &str,
    property: &str,
    old: &BTreeSet<String>,
    new: &BTreeSet<String>,
) {
    let ids = difference(old, new);
    if !ids.is_empty() {
        push_narrowing(
            out,
            kind,
            artifact_id,
            Some(object_type),
            Some(property),
            ids,
            format!("{label} on {object_type}.{property}"),
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the diagnostic carries its category, owner and compared state sets"
)]
fn push_set_addition(
    out: &mut Vec<Narrowing>,
    kind: &str,
    label: &str,
    artifact_id: &str,
    object_type: &str,
    property: &str,
    old: &BTreeSet<String>,
    new: &BTreeSet<String>,
) {
    let ids = difference(new, old);
    if !ids.is_empty() {
        push_narrowing(
            out,
            kind,
            artifact_id,
            Some(object_type),
            Some(property),
            ids,
            format!("{label} on {object_type}.{property}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use norn_ir::{
        ArtifactEnvelope, ArtifactKind, ArtifactLifecycle, ArtifactPayload, ArtifactProvenance,
        JsonValue, ProvenanceSource, SUPPORTED_IR_VERSION, ScopeRef,
    };
    use serde_json::{Value, json};

    use super::*;

    fn entity_artifact(artifact_id: &str, payload: Value) -> ArtifactEnvelope {
        let payload: JsonValue = serde_json::from_value(payload).expect("payload JSON");
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_owned(),
            artifact_id: artifact_id.to_owned(),
            namespace: "commerce".to_owned(),
            name: artifact_id
                .rsplit('.')
                .next()
                .unwrap_or("definition")
                .to_owned(),
            version: "1.0.0".to_owned(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: ArtifactProvenance {
                source: ProvenanceSource::System,
                source_path: None,
                actor_id: None,
                created_at: None,
            },
            payload: ArtifactPayload::from_kind_json(&ArtifactKind::Entity, payload)
                .expect("entity payload object"),
        }
    }

    fn v2_root(definition: Value) -> Value {
        let Value::Object(mut root) = definition else {
            panic!("definition root is object");
        };
        root.insert("entityVersion".to_owned(), json!("2.0"));
        Value::Object(root)
    }

    fn entity_set_with_alias(minimum: &str) -> Vec<ArtifactEnvelope> {
        vec![
            entity_artifact(
                "commerce.entity.types",
                v2_root(json!({
                    "propertyTypes": [{
                        "name": "commerce.amount",
                        "type": { "kind": "integer" },
                        "constraints": { "min": minimum }
                    }]
                })),
            ),
            entity_artifact(
                "commerce.entity.base",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Base",
                        "properties": [{
                            "id": "amount",
                            "type": { "kind": "ref", "name": "commerce.amount" }
                        }]
                    }
                })),
            ),
            entity_artifact(
                "commerce.entity.child",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Child",
                        "extends": "commerce.Base"
                    }
                })),
            ),
        ]
    }

    fn party_interface_set(
        customer_implements_party: bool,
        partner_implements_party: bool,
    ) -> Vec<ArtifactEnvelope> {
        let customer = if customer_implements_party {
            json!({
                "name": "commerce.Customer",
                "implements": ["commerce.Party"]
            })
        } else {
            json!({ "name": "commerce.Customer" })
        };
        let partner = if partner_implements_party {
            json!({
                "name": "commerce.Partner",
                "implements": ["commerce.Party"]
            })
        } else {
            json!({ "name": "commerce.Partner" })
        };
        vec![
            entity_artifact(
                "commerce.entity.party",
                v2_root(json!({
                    "interface": { "name": "commerce.Party" }
                })),
            ),
            entity_artifact(
                "commerce.entity.customer",
                v2_root(json!({ "objectType": customer })),
            ),
            entity_artifact(
                "commerce.entity.customer-child",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.PremiumCustomer",
                        "extends": "commerce.Customer"
                    }
                })),
            ),
            entity_artifact(
                "commerce.entity.partner",
                v2_root(json!({ "objectType": partner })),
            ),
            entity_artifact(
                "commerce.entity.order",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Order",
                        "links": [{
                            "id": "customer",
                            "target": "commerce.Party",
                            "cardinality": { "to": "many", "from": "many" }
                        }]
                    }
                })),
            ),
        ]
    }

    fn party_object_set(customer_extends_party: bool) -> Vec<ArtifactEnvelope> {
        let customer = if customer_extends_party {
            json!({
                "name": "commerce.Customer",
                "extends": "commerce.Party"
            })
        } else {
            json!({ "name": "commerce.Customer" })
        };
        vec![
            entity_artifact(
                "commerce.entity.party",
                v2_root(json!({
                    "objectType": { "name": "commerce.Party" }
                })),
            ),
            entity_artifact(
                "commerce.entity.customer",
                v2_root(json!({ "objectType": customer })),
            ),
            entity_artifact(
                "commerce.entity.customer-child",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.PremiumCustomer",
                        "extends": "commerce.Customer"
                    }
                })),
            ),
            entity_artifact(
                "commerce.entity.order",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Order",
                        "links": [{
                            "id": "customer",
                            "target": "commerce.Party",
                            "cardinality": { "to": "many", "from": "many" }
                        }]
                    }
                })),
            ),
        ]
    }

    #[test]
    fn unchanged_interface_link_target_narrows_when_inherited_membership_is_removed() {
        let previous = party_interface_set(true, true);
        let candidate = party_interface_set(false, true);
        let previous_targets = EffectiveCatalog::from_artifacts(&previous)
            .target_types("commerce.Party")
            .expect("Party interface is in the previous catalog");
        let candidate_targets = EffectiveCatalog::from_artifacts(&candidate)
            .target_types("commerce.Party")
            .expect("Party interface remains in the candidate catalog");
        assert_eq!(
            previous_targets,
            BTreeSet::from([
                "commerce.Customer".to_owned(),
                "commerce.Partner".to_owned(),
                "commerce.PremiumCustomer".to_owned(),
            ])
        );
        assert_eq!(
            candidate_targets,
            BTreeSet::from(["commerce.Partner".to_owned()])
        );
        let empty_party = party_interface_set(false, false);
        assert_eq!(
            EffectiveCatalog::from_artifacts(&empty_party).target_types("commerce.Party"),
            Some(BTreeSet::new())
        );

        let diff = diff_definition_sets(&previous, &candidate);
        assert!(diff.narrowing.iter().any(|item| {
            item.kind == "link_target_narrowed"
                && item.object_type.as_deref() == Some("commerce.Order")
                && item.ids.len() == 1
                && item.ids.first().is_some_and(|id| id == "customer")
        }));

        let widening = diff_definition_sets(&candidate, &previous);
        assert!(
            !widening
                .narrowing
                .iter()
                .any(|item| item.kind == "link_target_narrowed")
        );
    }

    #[test]
    fn unchanged_object_link_target_tracks_transitive_subtypes_and_widening() {
        let previous = party_object_set(true);
        let candidate = party_object_set(false);
        let previous_targets = EffectiveCatalog::from_artifacts(&previous)
            .target_types("commerce.Party")
            .expect("Party type is in the previous catalog");
        let candidate_targets = EffectiveCatalog::from_artifacts(&candidate)
            .target_types("commerce.Party")
            .expect("Party type remains in the candidate catalog");
        assert_eq!(
            previous_targets,
            BTreeSet::from([
                "commerce.Customer".to_owned(),
                "commerce.Party".to_owned(),
                "commerce.PremiumCustomer".to_owned(),
            ])
        );
        assert_eq!(
            candidate_targets,
            BTreeSet::from(["commerce.Party".to_owned()])
        );
        let diff = diff_definition_sets(&previous, &candidate);
        assert!(diff.narrowing.iter().any(|item| {
            item.kind == "link_target_narrowed"
                && item.object_type.as_deref() == Some("commerce.Order")
                && item.ids.len() == 1
                && item.ids.first().is_some_and(|id| id == "customer")
        }));

        let widening = diff_definition_sets(&candidate, &previous);
        assert!(
            !widening
                .narrowing
                .iter()
                .any(|item| item.kind == "link_target_narrowed")
        );
    }

    #[test]
    fn alias_constraint_narrowing_reaches_inherited_effective_properties() {
        let previous = entity_set_with_alias("1");
        let candidate = entity_set_with_alias("2");
        let diff = diff_definition_sets(&previous, &candidate);

        assert!(diff.narrowing.iter().any(|item| {
            item.kind == "property_contract_narrowed"
                && item.object_type.as_deref() == Some("commerce.Base")
                && item.property.as_deref() == Some("amount")
        }));
        assert!(diff.narrowing.iter().any(|item| {
            item.kind == "property_contract_narrowed"
                && item.object_type.as_deref() == Some("commerce.Child")
                && item.property.as_deref() == Some("amount")
        }));
    }

    #[test]
    fn interface_constraints_are_part_of_implementors_effective_contracts() {
        let previous = vec![
            entity_artifact(
                "commerce.entity.named",
                v2_root(json!({
                    "interface": {
                        "name": "commerce.Named",
                        "properties": [{
                            "id": "name",
                            "type": { "kind": "text" },
                            "constraints": { "pattern": "^A" }
                        }]
                    }
                })),
            ),
            entity_artifact(
                "commerce.entity.item",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Item",
                        "implements": ["commerce.Named"],
                        "properties": [{ "id": "name", "type": { "kind": "text" } }]
                    }
                })),
            ),
        ];
        let candidate = vec![
            entity_artifact(
                "commerce.entity.named",
                v2_root(json!({
                    "interface": {
                        "name": "commerce.Named",
                        "properties": [{
                            "id": "name",
                            "type": { "kind": "text" },
                            "constraints": { "pattern": "^AB" }
                        }]
                    }
                })),
            ),
            previous[1].clone(),
        ];
        let diff = diff_definition_sets(&previous, &candidate);

        assert!(diff.narrowing.iter().any(|item| {
            item.kind == "property_contract_narrowed"
                && item.object_type.as_deref() == Some("commerce.Item")
                && item.property.as_deref() == Some("name")
        }));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the complete canonical fixtures exercise related effective link contracts"
    )]
    fn effective_links_report_target_cardinality_property_and_new_required_changes() {
        let previous = vec![
            entity_artifact(
                "commerce.entity.amount-types",
                v2_root(json!({
                    "propertyTypes": [{
                        "name": "commerce.amount",
                        "type": { "kind": "integer" },
                        "constraints": { "min": "1" }
                    }]
                })),
            ),
            entity_artifact(
                "commerce.entity.party",
                v2_root(json!({ "objectType": { "name": "commerce.Party" } })),
            ),
            entity_artifact(
                "commerce.entity.customer",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Customer",
                        "extends": "commerce.Party"
                    }
                })),
            ),
            entity_artifact(
                "commerce.entity.order",
                v2_root(json!({
                    "objectType": {
                        "name": "commerce.Order",
                        "links": [{
                            "id": "customer",
                            "target": "commerce.Party",
                            "cardinality": { "to": "many", "from": "many" },
                            "properties": [{
                                "id": "weight",
                                "type": { "kind": "ref", "name": "commerce.amount" }
                            }]
                        }]
                    }
                })),
            ),
        ];
        let mut candidate = previous.clone();
        candidate[0] = entity_artifact(
            "commerce.entity.amount-types",
            v2_root(json!({
                "propertyTypes": [{
                    "name": "commerce.amount",
                    "type": { "kind": "integer" },
                    "constraints": { "min": "2" }
                }]
            })),
        );
        candidate[3] = entity_artifact(
            "commerce.entity.order",
            v2_root(json!({
                "objectType": {
                    "name": "commerce.Order",
                    "links": [{
                        "id": "customer",
                        "target": "commerce.Customer",
                        "cardinality": { "to": "one", "from": "many", "required": true },
                        "properties": [{
                            "id": "weight",
                            "type": { "kind": "ref", "name": "commerce.amount" }
                        }]
                    }]
                }
            })),
        );
        let diff = diff_definition_sets(&previous, &candidate);

        for kind in [
            "link_target_narrowed",
            "link_cardinality_narrowed",
            "link_property_contract_narrowed",
        ] {
            assert!(
                diff.narrowing.iter().any(|item| item.kind == kind),
                "{kind}"
            );
        }

        let no_link = entity_artifact(
            "commerce.entity.order",
            v2_root(json!({ "objectType": { "name": "commerce.Order" } })),
        );
        let required_link = entity_artifact(
            "commerce.entity.order",
            v2_root(json!({
                "objectType": {
                    "name": "commerce.Order",
                    "links": [{
                        "id": "customer",
                        "target": "commerce.Customer",
                        "cardinality": { "to": "many", "from": "many", "required": true }
                    }]
                }
            })),
        );
        let previous_without_link = previous
            .iter()
            .filter(|artifact| artifact.artifact_id != "commerce.entity.order")
            .cloned()
            .chain([no_link])
            .collect::<Vec<_>>();
        let candidate_with_required_link = previous_without_link
            .iter()
            .map(|artifact| {
                if artifact.artifact_id == "commerce.entity.order" {
                    required_link.clone()
                } else {
                    artifact.clone()
                }
            })
            .collect::<Vec<_>>();
        let required_diff =
            diff_definition_sets(&previous_without_link, &candidate_with_required_link);
        assert!(
            required_diff
                .narrowing
                .iter()
                .any(|item| item.kind == "link_contract_narrowed")
        );
    }
}
