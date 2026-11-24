//! Load pinned action writers only from the shared verified retained set, and
//! register the kernel's own action types in a cell model before provisioning.
use crate::{refusal::Refusal, scope::CellScope};
use norn_cell::InstalledDefinitionSet;
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, RuleRoot, RuntimeValue};
use norn_kernel::{DecisionBinding, SynchronousRule};
use std::collections::{BTreeMap, BTreeSet};

/// Add kernel-owned receipt and owned-row schemas before provisioning the store.
/// Preserves business definitions, interfaces, laws and binding indexes.
/// # Errors
/// Tenant declarations using SQLite-equivalent reserved names or invalid models refuse.
pub fn prepare_action_model(model: &norn_cell::Model) -> Result<norn_cell::Model, Refusal> {
    use norn_ir::{ObjectTypeDef, ObjectTypeName, Property, PropertyId, PropertyType};
    let mut snapshot = model.definitions_snapshot();
    for (name, text_fields, json_fields) in [
        (
            norn_kernel::ACTION_CALL_TYPE,
            &[
                "actionId",
                "actionArtifactId",
                "actionVersion",
                "effectKind",
                "actor",
                "turn",
                "transition",
                "from",
                "to",
                "ruleBinding",
            ][..],
            &[
                "object",
                "input",
                "effectiveInput",
                "derivedValues",
                "preconditionOutcomes",
                "parameterPins",
                "rowOperations",
                "trace",
            ][..],
        ),
        (
            norn_kernel::OWNED_ROWS_TYPE,
            &["parentType", "parentKey", "collectionId"][..],
            &["rowIds"][..],
        ),
    ] {
        if snapshot
            .object_types
            .iter()
            .any(|ty| ty.name.as_str().eq_ignore_ascii_case(name))
            || snapshot
                .interfaces
                .iter()
                .any(|ty| ty.name.as_str().eq_ignore_ascii_case(name))
        {
            return Err(Refusal::invalid("reserved kernel object type"));
        }
        let properties = text_fields
            .iter()
            .map(|id| (*id, PropertyType::Text { format: None }))
            .chain(
                json_fields
                    .iter()
                    .map(|id| (*id, PropertyType::JsonExt { schema: None })),
            )
            .map(|(id, property_type)| Property {
                protected: false,
                id: PropertyId(id.into()),
                property_type,
                description: None,
                constraints: norn_ir::PropertyConstraints {
                    nullable: true,
                    ..Default::default()
                },
                default: None,
                markings: vec![],
                computed: None,
            })
            .collect();
        snapshot.object_types.push(ObjectTypeDef {
            immutable: false,
            name: ObjectTypeName(name.into()),
            role: None,
            description: None,
            status: norn_ir::ResourceStatus::default(),
            extends: None,
            implements: vec![],
            properties,
            links: vec![],
            owned_collections: vec![],
            identity: vec![],
            invariants: vec![],
            lifecycles: vec![],
            markings: vec![],
            accounting_leg: None,
        });
    }
    norn_cell::Model::from_snapshot(&snapshot).map_err(Refusal::host)
}

/// The admitted actions of a verified retained set and their pinned writers.
pub(crate) struct RetainedActions {
    pub(crate) actions: Vec<(ArtifactEnvelope, ActionRoot)>,
    pub(crate) rules: Vec<SynchronousRule>,
    /// APPROVAL-REVISION-1: each action's definition digest, by action id.
    digests: BTreeMap<String, String>,
}
impl RetainedActions {
    pub(crate) fn load(set: &InstalledDefinitionSet, scope: &CellScope) -> Result<Self, Refusal> {
        crate::routes::verify_retained(set, scope)?;
        let RuntimeValue::Array(sources) =
            RuntimeValue::from_json_text(&set.canonical_set).map_err(Refusal::host)?
        else {
            return Err(Refusal::host("retained set is not an array"));
        };
        let artifacts = sources
            .iter()
            .map(norn_ir::runtime::decode)
            .collect::<Result<Vec<ArtifactEnvelope>, _>>()
            .map_err(Refusal::host)?;
        let mut actions = Vec::new();
        let mut action_sources = BTreeMap::new();
        let mut roots = BTreeMap::new();
        let mut identities = BTreeSet::new();
        for (artifact, source) in artifacts.iter().zip(&sources) {
            let source = source
                .get("payload")
                .ok_or_else(|| Refusal::host("missing payload"))?;
            match artifact.kind() {
                ArtifactKind::Action => {
                    let action: ActionRoot =
                        norn_ir::runtime::decode(source).map_err(Refusal::host)?;
                    if !identities.insert(action.action_id.clone()) {
                        return Err(Refusal::host("ambiguous retained action identity"));
                    }
                    action_sources.insert(action.action_id.clone(), source);
                    actions.push((artifact.clone(), action));
                }
                ArtifactKind::Rule => {
                    let root: RuleRoot = norn_ir::runtime::decode(source).map_err(Refusal::host)?;
                    if roots.insert(artifact.artifact_id.clone(), root).is_some() {
                        return Err(Refusal::host("ambiguous retained writer identity"));
                    }
                }
                ArtifactKind::Entity
                | ArtifactKind::Relation
                | ArtifactKind::Connector
                | ArtifactKind::Widget
                | ArtifactKind::Customization
                | ArtifactKind::Setting
                | ArtifactKind::BusinessPack
                | ArtifactKind::Policy
                | ArtifactKind::Derived
                | ArtifactKind::SourceSchema
                | ArtifactKind::Mapping
                | ArtifactKind::Law
                | ArtifactKind::DefinedConcept
                | ArtifactKind::Labels
                | ArtifactKind::Model => {}
            }
        }
        let receipt: serde_json::Value = serde_json::from_str(
            set.admission_receipt
                .as_deref()
                .ok_or_else(|| Refusal::host("missing retained receipt"))?,
        )
        .map_err(Refusal::host)?;
        let binding = DecisionBinding {
            profile_id: receipt
                .get("exprProfile")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| Refusal::host("missing expression profile"))?
                .into(),
            environment_hash: receipt
                .pointer("/typeEnv/sha256")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| Refusal::host("missing type environment"))?
                .into(),
        };
        let mut rules = Vec::new();
        let digests = action_digests(&actions, &action_sources, &artifacts, &sources, &roots)?;
        let mut loaded = BTreeSet::new();
        for (_, action) in &actions {
            let Some(pin) = &action.synchronous_rule else {
                continue;
            };
            if !loaded.insert((pin.artifact_id.clone(), pin.version.clone())) {
                continue;
            }
            let writer = artifacts
                .iter()
                .find(|artifact| {
                    artifact.artifact_id == pin.artifact_id && artifact.version == pin.version
                })
                .ok_or_else(|| Refusal::host("missing pinned writer"))?;
            let root = roots
                .get(&writer.artifact_id)
                .ok_or_else(|| Refusal::host("missing writer graph"))?;
            let helpers = helper_closure(writer, &artifacts, &roots)?;
            rules.push(
                SynchronousRule::admit(pin.clone(), root, &helpers, binding.clone())
                    .map_err(Refusal::host)?,
            );
        }
        Ok(Self {
            actions,
            rules,
            digests,
        })
    }

    /// APPROVAL-REVISION-1: the digest of `action_id`'s definition as this
    /// revision retains it: the action's payload, and its pinned writer's
    /// with the writer's helper closure. An approval binds to it, so an
    /// approval of a call never executes under a changed action.
    pub(crate) fn digest(&self, action_id: &str) -> Option<&str> {
        self.digests.get(action_id).map(String::as_str)
    }
}
/// APPROVAL-REVISION-1: each retained action's definition digest, by action
/// id: its payload, and its pinned writer's payload with the writer's helper
/// closure.
fn action_digests(
    actions: &[(ArtifactEnvelope, ActionRoot)],
    action_sources: &BTreeMap<String, &RuntimeValue>,
    artifacts: &[ArtifactEnvelope],
    sources: &[RuntimeValue],
    roots: &BTreeMap<String, RuleRoot>,
) -> Result<BTreeMap<String, String>, Refusal> {
    let mut digests = BTreeMap::new();
    for (_, action) in actions {
        let writer = match &action.synchronous_rule {
            Some(pin) => {
                let (index, writer) = artifacts
                    .iter()
                    .enumerate()
                    .find(|(_, artifact)| {
                        artifact.artifact_id == pin.artifact_id && artifact.version == pin.version
                    })
                    .ok_or_else(|| Refusal::host("missing pinned writer"))?;
                let source = sources
                    .get(index)
                    .and_then(|source| source.get("payload"))
                    .ok_or_else(|| Refusal::host("missing payload"))?;
                Some((source, helper_closure(writer, artifacts, roots)?))
            }
            None => None,
        };
        let digest = norn_ir::canonical::domain_hash_hex(
            b"norn.approval.action/1\0",
            &(action_sources.get(&action.action_id), &writer),
        )
        .map_err(Refusal::host)?;
        digests.insert(action.action_id.clone(), digest);
    }
    Ok(digests)
}
fn helper_closure(
    writer: &ArtifactEnvelope,
    artifacts: &[ArtifactEnvelope],
    roots: &BTreeMap<String, RuleRoot>,
) -> Result<BTreeMap<String, RuleRoot>, Refusal> {
    let mut helpers = BTreeMap::new();
    let mut pending = vec![writer];
    while let Some(current) = pending.pop() {
        for pin in current
            .dependencies
            .iter()
            .filter(|pin| pin.kind == ArtifactKind::Rule)
        {
            let artifact = artifacts
                .iter()
                .find(|artifact| {
                    artifact.artifact_id == pin.artifact_id
                        && pin.version.as_deref() == Some(artifact.version.as_str())
                })
                .ok_or_else(|| Refusal::host("missing exact helper dependency"))?;
            if !helpers.contains_key(&pin.artifact_id) {
                let root = roots
                    .get(&pin.artifact_id)
                    .ok_or_else(|| Refusal::host("missing helper graph"))?;
                helpers.insert(pin.artifact_id.clone(), root.clone());
                pending.push(artifact);
            }
        }
    }
    Ok(helpers)
}

/// Target version as of `receipt` (written row by type lineage, else `version_as_of`, S5b).
pub(crate) fn target_version<E: norn_cell::engine::SqlEngine>(
    store: &norn_cell::CellStore<E>,
    receipt: &norn_cell::Receipt,
    record: &norn_kernel::ActionCallRecord,
) -> Result<u64, Refusal> {
    let ty = record.object.object_type.as_str();
    let model = store.model();
    if let Some(row) = receipt.body.written.iter().find(|r| {
        r.key == record.object.key
            && (r.object_type == ty
                || model.is_a(&r.object_type, ty)
                || model.is_a(ty, &r.object_type))
    }) {
        return Ok(row.version);
    }
    store
        .read()
        .version_as_of(
            norn_cell::MAIN_WORLD,
            ty,
            &record.object.key,
            receipt.body.commit_seq,
        )
        .map_err(Refusal::host)
}
