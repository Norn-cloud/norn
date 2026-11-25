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
    prepare(model, false)
}

/// The property of the kernel's call record that keeps resolved principal
/// inputs (PRINCIPAL-INPUT-1, #1091).
const PRINCIPAL_INPUTS: &str = "principalInputs";

/// What a cell's stored model serves, which the model of every later
/// revision keeps (DEFS-REOPEN-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Serving {
    /// The kernel's action receipt and owned-row types.
    pub actions: bool,
    /// The call record's `principalInputs` column. Once a revision records
    /// it, later revisions keep it whether or not they declare principal
    /// inputs, so a rollback never orphans stored values.
    pub principal_inputs: bool,
}

impl Serving {
    /// A cell that serves actions and has recorded no principal inputs.
    pub const ACTIONS: Self = Self {
        actions: true,
        principal_inputs: false,
    };

    /// What `model` serves.
    #[must_use]
    pub fn of(model: &norn_cell::Model) -> Self {
        let call_record = model
            .object_types()
            .find(|declared| declared.name.as_str() == norn_kernel::ACTION_CALL_TYPE);
        Self {
            actions: call_record.is_some(),
            principal_inputs: call_record.is_some_and(|record| {
                record
                    .properties
                    .iter()
                    .any(|property| property.id.as_str() == PRINCIPAL_INPUTS)
            }),
        }
    }
}

impl<E: norn_cell::engine::SqlEngine> crate::host::DevHost<E> {
    /// What this host's open cell serves. A host that installs a revision
    /// builds its candidate model from this, so the column a cell already
    /// records is kept (DEFS-REOPEN-1).
    /// # Errors
    /// The cell is fenced or its lock is poisoned.
    pub fn serving(&self) -> Result<Serving, Refusal> {
        Ok(Serving::of(self.cell()?.store().model()))
    }
}

/// [`prepare_action_model`] for the definitions `artifacts`. When a decide
/// rule of the set declares principal inputs (PRINCIPAL-INPUT-1, #1091) the
/// call record also keeps `principalInputs`. A set that declares none is
/// served exactly the model [`prepare_action_model`] gives it, so its ledger
/// does not change.
/// # Errors
/// As [`prepare_action_model`].
pub fn prepare_action_model_for(
    model: &norn_cell::Model,
    artifacts: &[ArtifactEnvelope],
) -> Result<norn_cell::Model, Refusal> {
    prepare_action_model_keeping(model, artifacts, false)
}

/// [`prepare_action_model_for`], keeping the call record's `principalInputs`
/// when `keep` says the cell already records it.
/// # Errors
/// As [`prepare_action_model`].
pub fn prepare_action_model_keeping(
    model: &norn_cell::Model,
    artifacts: &[ArtifactEnvelope],
    keep: bool,
) -> Result<norn_cell::Model, Refusal> {
    let declared = artifacts.iter().any(|artifact| {
        artifact.kind() == ArtifactKind::Rule
            && matches!(
                artifact.payload.to_json_value().as_object().and_then(|payload| payload.get("principalInputs")),
                Some(norn_ir::JsonValue::Array(inputs)) if !inputs.is_empty()
            )
    });
    prepare(model, declared || keep)
}

fn prepare(model: &norn_cell::Model, principal_inputs: bool) -> Result<norn_cell::Model, Refusal> {
    use norn_ir::{ObjectTypeDef, ObjectTypeName, Property, PropertyId, PropertyType};
    // The call record's `principalInputs`, only for a set that declares them.
    let recorded_principal: &[&str] = if principal_inputs {
        &[PRINCIPAL_INPUTS]
    } else {
        &[]
    };
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
                    .chain(
                        recorded_principal
                            .iter()
                            .filter(|_| name == norn_kernel::ACTION_CALL_TYPE),
                    )
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

/// APPROVAL-REVISION-1: the digest an approval or automation authority binds
/// to: the action's retained definition (its payload, its pinned writer and
/// the writer's helpers) and its effective write contract over `model`: the
/// declaration of every object type it may write (with the types they extend,
/// so a changed default that changes the written facts changes the digest),
/// the interfaces and property types those declarations draw on, and the
/// governing laws of each written type. An action whose writes are not
/// statically known (a synchronous writer) binds every object type.
///
/// # Errors
/// A host failure: a retained action without a digest, an unreadable law.
pub(crate) fn effective_digest(
    admitted: &RetainedActions,
    action: &ActionRoot,
    model: &norn_cell::Model,
    catalog: &norn_kernel::TypeCatalog<'_>,
) -> Result<String, Refusal> {
    let definition = admitted
        .digest(&action.action_id)
        .ok_or_else(|| Refusal::host("a retained action has no digest"))?;
    let every = action.synchronous_rule.is_some()
        || (action.writes.is_empty() && action.effect_program.is_some());
    let mut written: BTreeSet<String> = action
        .writes
        .iter()
        .map(|write| write.entity_kind.clone())
        .chain(action.create_links.iter().flat_map(|link| {
            std::iter::once(link.target_type.clone()).chain(
                link.source
                    .as_ref()
                    .map(|source| source.object_type.clone()),
            )
        }))
        .collect();
    if every {
        written.extend(model.object_types().map(|t| t.name.as_str().to_owned()));
    }
    let mut pending: Vec<String> = written.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        if let Some(parent) = model
            .object_types()
            .find(|t| t.name.as_str() == name)
            .and_then(|t| t.extends.as_ref())
            && written.insert(parent.as_str().to_owned())
        {
            pending.push(parent.as_str().to_owned());
        }
    }
    let mut declarations = Vec::new();
    let mut laws = Vec::new();
    for name in &written {
        let declaration = model.object_types().find(|t| t.name.as_str() == name);
        declarations.push((name, declaration));
        if declaration.is_some() {
            let governing = catalog
                .governing_laws(&norn_ir::ObjectTypeName(name.clone()))
                .map_err(|error| Refusal::host(format!("{error:?}")))?;
            laws.push((name, governing));
        }
    }
    let interfaces: Vec<_> = model.interfaces().collect();
    norn_ir::canonical::domain_hash_hex(
        b"norn.approval.action/2\0",
        &(
            definition,
            declarations,
            interfaces,
            model.property_types(),
            laws,
        ),
    )
    .map_err(Refusal::host)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call_record_properties(model: &norn_cell::Model) -> Vec<String> {
        model
            .object_types()
            .find(|object_type| object_type.name.as_str() == norn_kernel::ACTION_CALL_TYPE)
            .map_or_default(|object_type| {
                object_type
                    .properties
                    .iter()
                    .map(|property| property.id.0.clone())
                    .collect()
            })
    }

    fn rule(payload: &serde_json::Value) -> ArtifactEnvelope {
        serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "document.review.rule",
            "kind": "rule",
            "namespace": "document",
            "name": "Review",
            "version": "1.0.0",
            "lifecycle": "draft",
            "scope": {"orgId": "org_demo", "environmentId": "dev"},
            "dependencies": [],
            "provenance": {"source": "authoring_sdk", "sourcePath": "test.ts"},
            "payload": payload
        }))
        .expect("rule artifact")
    }

    /// PRINCIPAL-INPUT-1: the call record gains `principalInputs` only for a
    /// set that declares principal inputs; every other set is served the
    /// model it was served before, so its ledger does not change.
    #[test]
    fn only_a_set_that_declares_principal_inputs_records_them() {
        let model = norn_cell::Model::from_definitions(&[], &[]).expect("model");
        let plain = call_record_properties(&prepare_action_model(&model).expect("served model"));
        assert!(!plain.iter().any(|id| id == "principalInputs"));
        let undeclared = rule(&json!({"ruleId": "document.review"}));
        let same = prepare_action_model_for(&model, &[undeclared]).expect("served model");
        assert_eq!(call_record_properties(&same), plain);
        let declared = rule(&json!({
            "ruleId": "document.review",
            "principalInputs": [
                {"name": "isReviewer", "kind": {"holdsRole": {"role": "reviewer", "at": "resource"}}}
            ]
        }));
        let recorded = prepare_action_model_for(&model, &[declared]).expect("served model");
        assert_eq!(
            Serving::of(&recorded),
            Serving {
                actions: true,
                principal_inputs: true
            }
        );
        let recorded = call_record_properties(&recorded);
        assert_eq!(recorded.last().map(String::as_str), Some("principalInputs"));
        assert_eq!(
            recorded.split_last().map(|(_, rest)| rest),
            Some(plain.as_slice())
        );
    }

    /// DEFS-REOPEN-1: a cell that recorded principal inputs keeps the column
    /// under a later set that declares none, so its stored values are never
    /// orphaned; a cell that never recorded any is unchanged.
    #[test]
    fn a_cell_that_records_principal_inputs_keeps_the_column_under_any_later_set() {
        let model = norn_cell::Model::from_definitions(&[], &[]).expect("model");
        let undeclared = [rule(&json!({"ruleId": "document.review"}))];
        let plain = prepare_action_model(&model).expect("served model");
        assert!(!Serving::of(&model).actions);
        assert_eq!(Serving::of(&plain), Serving::ACTIONS);
        let kept = prepare_action_model_keeping(&model, &undeclared, true).expect("served model");
        assert!(Serving::of(&kept).principal_inputs);
        let dropped =
            prepare_action_model_keeping(&model, &undeclared, false).expect("served model");
        assert_eq!(dropped.model_hash(), plain.model_hash());
        assert_ne!(kept.model_hash(), plain.model_hash());
    }
}
