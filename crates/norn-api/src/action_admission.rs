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
    let mut snapshot = model.definitions_snapshot();
    for record in norn_cell::kernel_record_types() {
        let name = record.name.as_str();
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
        snapshot.object_types.push(record);
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
        crate::routes::admit_retained(set, scope)?;
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
    use super::RetainedActions;
    use crate::scope::CellScope;
    use norn_checker::entity_scopes::ScopedView;
    use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
    use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue};
    use serde_json::{Value, json};
    use std::collections::BTreeSet;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn envelope(
        id: &str,
        kind: &str,
        scope: &Value,
        payload: &Value,
    ) -> serde_json::Result<ArtifactEnvelope> {
        serde_json::from_value(json!({
            "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": "retail", "name": id,
            "version": "1.0.0", "lifecycle": "draft", "scope": scope, "dependencies": [],
            "provenance": { "source": "authoring_sdk" }, "payload": payload
        }))
    }

    /// A broader writer of a narrower extension's property, as a set admitted
    /// before scoped admission retains it.
    fn legacy_set() -> serde_json::Result<Vec<ArtifactEnvelope>> {
        let sku = json!({ "entityVersion": "2.0", "objectType": {
            "name": "retail.Sku",
            "properties": [{ "id": "sku_code", "type": { "kind": "text" } }],
            "identity": [{ "name": "primary", "properties": ["sku_code"] }]
        } });
        let extension = json!({ "entityVersion": "2.0", "objectTypeExtension": {
            "namespace": "acme",
            "target": "retail.Sku",
            "properties": [{ "id": "acme__secret_tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
        } });
        let writer = json!({
            "actionId": "retail.write_secret",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "properties": { "sku_code": { "type": "string" }, "acme__secret_tag": { "type": "string" } },
                "required": ["sku_code"],
                "additionalProperties": false
            },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectKind": "retail.Sku",
                "objectIdField": "sku_code",
                "entityArtifactId": "retail.Sku.entity",
                "entityArtifactVersion": "1.0.0"
            },
            "writes": [
                { "entityKind": "retail.Sku", "change": "created" },
                { "entityKind": "retail.Sku", "change": "modified" }
            ]
        });
        Ok(vec![
            envelope("retail.Sku.entity", "entity", &json!({}), &sku)?,
            envelope(
                "acme.SkuExt.entity",
                "entity",
                &json!({ "orgId": "org_demo", "environmentId": "dev" }),
                &extension,
            )?,
            envelope(
                "retail.write_secret",
                "action",
                &json!({ "orgId": "org_demo" }),
                &writer,
            )?,
        ])
    }

    #[test]
    fn a_retained_set_the_scoped_checks_refuse_supplies_no_actions() -> TestResult {
        let artifacts = legacy_set()?;
        let values = artifacts
            .iter()
            .map(|artifact| {
                RuntimeValue::from_json(serde_json::to_value(artifact)?).map_err(Into::into)
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let limits = ResourceLimitProfile::norn_limits_v1();
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let checked = check_artifact_set_with_receipt(
            &ReceiptSources::new(&values, &[])?,
            &context,
            &limits,
        )?;
        assert!(!checked.receipt.admitted, "{:#?}", checked.diagnostics);
        let narrow = artifacts.get(1).ok_or("extension")?;
        let mut legacy = checked.receipt;
        legacy.admitted = true;
        legacy.diagnostics.count = 0;
        legacy.diagnostics.errors = 0;
        let whole_set = ScopedView::of(narrow, &artifacts, &[], &registry)
            .receipt_env(&limits)
            .map_err(|errors| format!("{errors:?}"))?;
        legacy.type_env = Some(whole_set.reference()?);
        let installed = norn_cell::InstalledDefinitionSet {
            version: 1,
            receipt_sha256: legacy.content_sha256()?,
            admission_receipt: Some(String::from_utf8(legacy.canonical_json()?)?),
            canonical_set: String::from_utf8(norn_ir::canonical::canonical_json_of(&artifacts)?)?,
            registry: Vec::new(),
            model_hash: String::new(),
            ddl_hash: String::new(),
            installed_at: 1,
            commit_seq: 1,
            layout: None,
            layout_revision: 0,
        };
        let scope = CellScope {
            org_id: "org_demo".to_owned(),
            environment_id: "dev".to_owned(),
        };
        crate::routes::verify_retained(&installed, &scope)
            .map_err(|refusal| format!("{refusal:?}"))?;
        let refused = format!(
            "{:?}",
            RetainedActions::load(&installed, &scope)
                .err()
                .ok_or("actions loaded")?
        );
        assert!(
            refused.contains("no longer pass scoped admission"),
            "{refused}"
        );
        Ok(())
    }
}
