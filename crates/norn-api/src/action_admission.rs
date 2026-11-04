//! Load pinned action writers only from the shared verified retained set.
use crate::{refusal::Refusal, scope::CellScope};
use norn_cell::InstalledDefinitionSet;
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, RuleRoot, RuntimeValue};
use norn_kernel::{DecisionBinding, SynchronousRule};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct RetainedActions {
    pub(crate) actions: Vec<(ArtifactEnvelope, ActionRoot)>,
    pub(crate) rules: Vec<SynchronousRule>,
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
                | ArtifactKind::DefinedConcept => {}
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
        Ok(Self { actions, rules })
    }
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
