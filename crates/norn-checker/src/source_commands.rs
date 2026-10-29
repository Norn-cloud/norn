//! Admission of declared, version-bound proposals to observed source documents.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EnvOptions, Expression, Type, ZenTypeEnv, check_expression_with_root_bounds};
use norn_ir::mapping::MappingIr;
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::{
    ActionCommandOperation, ArtifactEnvelope, ArtifactKind, ArtifactPayload, ConnectorRoot,
    Diagnostic, JsonValue, ResourceLimitProfile, RuleContractV2, RuleRoot, RuleSourceCommand,
    SliceKey,
};

use crate::entity_artifacts::EntityArtifactSetContext;
use crate::envelope::invalid_diagnostic;
use crate::lifecycle_writes::finding;

const CODE: &str = "rule.validation.invalid_source_command";
const SET_CODE: &str = "artifact_set.decision.invalid_source_command";

macro_rules! decode {
    ($ty:ty, $value:expr) => {
        serde_json::to_value($value)
            .ok()
            .and_then(|value| serde_json::from_value::<$ty>(value).ok())
    };
}

/// Check optional connection qualification and child-table capabilities.
pub(crate) fn validate_connector_profile(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CONNECTOR_CODE: &str = "connector.validation.invalid_payload";
    let Some(root) = decode!(ConnectorRoot, &JsonValue::Object(payload.clone())) else {
        return;
    };
    if let Some(versions) = &root.supported_versions {
        let apps: BTreeSet<_> = versions
            .components
            .iter()
            .map(|component| component.app.as_str())
            .collect();
        if versions.endpoint.trim().is_empty()
            || !versions.endpoint.starts_with('/')
            || versions.endpoint.starts_with("//")
            || versions.endpoint.contains("://")
            || versions.endpoint.contains(['?', '#', '{', '}'])
            || crate::connector_path::ConnectorValuePath::parse(&versions.response_path).is_err()
            || versions.response_path.is_empty()
            || versions.response_path == "$"
            || apps.is_empty()
            || apps.len() != versions.components.len()
            || apps.iter().any(|app| app.trim().is_empty())
            || versions.components.iter().any(|component| {
                component.majors.is_empty()
                    || component.majors.contains(&0)
                    || component.majors.iter().collect::<BTreeSet<_>>().len()
                        != component.majors.len()
            })
        {
            diagnostics.push(invalid_diagnostic(CONNECTOR_CODE, "supportedVersions requires a relative member endpoint, response member path, and unique apps with positive major allowlists", "payload.supportedVersions", artifact_id));
        }
    }
    for (index, command) in root
        .commands
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let mut tables = BTreeSet::new();
        for (merge_index, merge) in command.merge_profiles.iter().enumerate() {
            let names = [&merge.field, &merge.ownership_field, &merge.row_key_field];
            let fields: BTreeSet<_> = merge.conflict_key_fields.iter().collect();
            let reserved = [
                "name",
                "parent",
                "parenttype",
                "parentfield",
                "idx",
                "doctype",
            ];
            let guarded = command.lifecycle.as_ref().is_some_and(|lifecycle| {
                lifecycle.version_field == "modified"
                    && merge.field != lifecycle.version_field
                    && merge.field != lifecycle.status_field
            }) && command.idempotency.as_ref().is_some_and(|idempotency| {
                merge.field != idempotency.key_field
                    && merge.field != idempotency.receipt.table_field
            });
            if command.operation != ActionCommandOperation::Update
                || !guarded
                || names.iter().any(|name| name.trim().is_empty())
                || merge.ownership_field == merge.row_key_field
                || reserved.contains(&merge.row_key_field.as_str())
                || reserved.contains(&merge.ownership_field.as_str())
                || !tables.insert(&merge.field)
                || fields.len() != merge.conflict_key_fields.len()
                || fields.iter().any(|field| field.trim().is_empty())
                || !command
                    .body_fields
                    .iter()
                    .any(|field| field == "*" || field == &merge.field)
                || command.fixed_body.contains_key(&merge.field)
            {
                diagnostics.push(invalid_diagnostic(CONNECTOR_CODE, "merge profiles require a guarded update, distinct nonempty owner/stable-key fields, unique tables, and an admitted body field", format!("payload.commands[{index}].mergeProfiles[{merge_index}]"), artifact_id));
            }
        }
    }
}

/// Validate activation schemas, exact target bindings and pure expressions.
pub(crate) fn validate(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(value) = payload.get("sourceCommands") else {
        return;
    };
    let Some(commands) = decode!(Vec<RuleSourceCommand>, value) else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "sourceCommands must be typed declarations",
            "payload.sourceCommands",
            artifact_id,
        ));
        return;
    };
    if commands.is_empty() {
        return;
    }
    let root = decode!(RuleRoot, &JsonValue::Object(payload.clone()));
    let Some(root) = root else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "sourceCommands require a well-formed RuleIR v2 root",
            "payload.sourceCommands",
            artifact_id,
        ));
        return;
    };
    if root.contract != Some(RuleContractV2::Decide)
        || root.triggers.is_empty()
        || root.triggers.iter().any(|trigger| {
            !matches!(
                trigger,
                norn_ir::RuleTrigger::Created { .. } | norn_ir::RuleTrigger::Modified { .. }
            )
        })
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "source commands require a hosted decide rule triggered by creation or modification",
            "payload.contract",
            artifact_id,
        ));
    }
    let ids: BTreeSet<_> = commands.iter().map(|command| command.id.as_str()).collect();
    if ids.len() != commands.len() || ids.iter().any(|id| id.trim().is_empty()) {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "source command ids must be nonempty and unique",
            "payload.sourceCommands",
            artifact_id,
        ));
    }
    if commands.len()
        > usize::try_from(ResourceLimitProfile::norn_limits_v1().envelope.max_commands)
            .unwrap_or(usize::MAX)
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "source commands exceed the command envelope",
            "payload.sourceCommands",
            artifact_id,
        ));
    }
    validate_activation_schema(&root, &ids, diagnostics, artifact_id);
    let environment = environment(&JsonValue::Object(root.input_schema.clone()));
    if environment.is_none() {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "source command expressions require a closed typed input environment",
            "payload.inputSchema",
            artifact_id,
        ));
    }
    for (index, command) in commands.iter().enumerate() {
        validate_declaration(
            &root,
            command,
            index,
            environment.as_ref(),
            diagnostics,
            artifact_id,
        );
    }
}

fn validate_declaration(
    root: &RuleRoot,
    command: &RuleSourceCommand,
    index: usize,
    environment: Option<&ZenTypeEnv>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let path = format!("payload.sourceCommands[{index}]");
    let entry = root.slice.iter().find(|entry| entry.name == command.slice);
    if entry.is_none_or(|entry| entry.optional || entry.key != SliceKey::TriggerObject) {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "source commands target a required triggerObject slice entry",
            format!("{path}.slice"),
            artifact_id,
        ));
    }
    for (member, value) in [
        ("connector", &command.connector),
        ("mapping", &command.mapping),
        ("map", &command.map),
        ("doctype", &command.doctype),
        ("modifiedProperty", &command.modified_property),
    ] {
        if value.trim().is_empty() {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{member} must be nonempty"),
                format!("{path}.{member}"),
                artifact_id,
            ));
        }
    }
    if let Some(env) = environment {
        for (field, expression) in &command.fields {
            if field.trim().is_empty() {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "field names must be nonempty",
                    format!("{path}.fields"),
                    artifact_id,
                ));
            }
            check_expression(
                expression.as_str(),
                &Type::Json,
                env,
                &format!("{path}.fields.{field}"),
                diagnostics,
                artifact_id,
            );
        }
        for (merge_index, merge) in command.child_merges.iter().enumerate() {
            let merge_path = format!("{path}.childMerges[{merge_index}]");
            check_expression(
                merge.ownership_value.as_str(),
                &Type::text(),
                env,
                &format!("{merge_path}.ownershipValue"),
                diagnostics,
                artifact_id,
            );
            check_expression(
                merge.rows.as_str(),
                &Type::Json.list(),
                env,
                &format!("{merge_path}.rows"),
                diagnostics,
                artifact_id,
            );
        }
    }
}

fn validate_activation_schema(
    root: &RuleRoot,
    ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let schema = serde_json::to_value(&root.output_schema).unwrap_or_default();
    let item = schema.pointer("/properties/commands/items");
    let declared: BTreeSet<_> = item
        .and_then(|item| item.get("properties"))
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.keys().map(String::as_str))
        .collect();
    let required: BTreeSet<_> = item
        .and_then(|item| item.get("required"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let values: BTreeSet<_> = item
        .and_then(|item| item.pointer("/properties/sourceCommandId/enum"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    if declared != BTreeSet::from(["sourceCommandId"])
        || required != declared
        || &values != ids
        || item.and_then(|item| item.get("additionalProperties"))
            != Some(&serde_json::Value::Bool(false))
    {
        diagnostics.push(invalid_diagnostic(CODE, "command items must be closed activations with only required sourceCommandId, enumerating the declared ids", "payload.outputSchema.properties.commands.items", artifact_id));
    }
}

fn check_expression(
    source: &str,
    expected: &Type,
    environment: &ZenTypeEnv,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let result = Expression::compile(source)
        .map_err(|error| error.to_string())
        .and_then(|expression| {
            check_expression_with_root_bounds(
                &expression,
                environment,
                expected,
                &ResourceLimitProfile::norn_limits_v1(),
                &crate::typed_roots::action_expression_root_bounds(environment),
            )
            .map(|_| ())
            .map_err(|findings| {
                findings
                    .into_iter()
                    .map(|finding| format!("{} ({})", finding.message, finding.code))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        });
    if let Err(error) = result {
        diagnostics.push(invalid_diagnostic(
            CODE,
            format!("source command expression is invalid: {error}"),
            path,
            artifact_id,
        ));
    }
}

/// Reuse the expression engine's explicit nested-record input environment.
fn environment(schema: &JsonValue) -> Option<ZenTypeEnv> {
    let mut records = BTreeMap::new();
    let root_type = schema_type(schema, "source_command_input", &mut records, 0)?;
    let Type::Record(root_name) = root_type else {
        return None;
    };
    let fields = records.remove(&root_name)?;
    let mut environment =
        ZenTypeEnv::input_only_with_records(records, fields.clone(), EnvOptions::default()).ok()?;
    for (name, ty) in fields {
        environment = environment.bind_operand(&name, ty).ok()?;
    }
    Some(environment)
}

fn schema_type(
    schema: &JsonValue,
    name: &str,
    records: &mut BTreeMap<String, BTreeMap<String, Type>>,
    depth: usize,
) -> Option<Type> {
    if depth > 32 {
        return None;
    }
    let object = schema.as_object()?;
    let type_value = object.get("type")?;
    if let Some(kinds) = type_value.as_array() {
        let kinds: BTreeSet<_> = kinds
            .iter()
            .map(JsonValue::as_string)
            .collect::<Option<_>>()?;
        if kinds.len() != 2 || !kinds.contains("null") {
            return None;
        }
        let kind = kinds.iter().find(|kind| **kind != "null")?;
        let mut scalar = object.clone();
        scalar.insert("type".to_owned(), JsonValue::String((*kind).to_owned()));
        return schema_type(
            &JsonValue::Object(scalar),
            name,
            records,
            depth.saturating_add(1),
        )
        .map(Type::optional);
    }
    match type_value.as_string()? {
        "string" => Some(Type::text()),
        "integer" => Some(Type::Integer),
        "number" => Some(Type::Number),
        "boolean" => Some(Type::Bool),
        "null" => Some(Type::Json),
        "array" => schema_type(
            object.get("items")?,
            &format!("{name}_item"),
            records,
            depth.saturating_add(1),
        )
        .map(Type::list),
        "object" => {
            if object.get("additionalProperties") != Some(&JsonValue::Bool(false)) {
                return None;
            }
            let properties = object.get("properties")?.as_object()?;
            let required: BTreeSet<_> = object
                .get("required")
                .and_then(JsonValue::as_array)
                .into_iter()
                .flatten()
                .filter_map(JsonValue::as_string)
                .collect();
            let mut fields = BTreeMap::new();
            for (index, (field, schema)) in properties.iter().enumerate() {
                let ty = schema_type(
                    schema,
                    &format!("{name}_{index}"),
                    records,
                    depth.saturating_add(1),
                )?;
                fields.insert(
                    field.clone(),
                    if required.contains(field.as_str()) {
                        ty
                    } else {
                        ty.optional()
                    },
                );
            }
            records.insert(name.to_owned(), fields);
            Some(Type::Record(name.to_owned()))
        }
        _ => None,
    }
}

/// Resolve admitted source commands against exact connector/mapping contracts.
pub(crate) fn check_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<crate::Diagnostic> {
    let pool: Vec<_> = artifacts.iter().chain(context.base).collect();
    let mut findings = Vec::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Rule)
    {
        let Some(root) = decode!(RuleRoot, &artifact.payload.to_json_value()) else {
            continue;
        };
        for (index, command) in root.source_commands.iter().enumerate() {
            let pointer = format!("/payload/sourceCommands/{index}");
            if let Err(message) = check_binding(artifact, &root, command, &pool) {
                findings.push(finding(SET_CODE, message, artifact, &pointer));
            }
        }
    }
    findings
}

fn resolve<'a>(
    owner: &ArtifactEnvelope,
    id: &str,
    kind: &ArtifactKind,
    pool: &[&'a ArtifactEnvelope],
) -> Result<&'a ArtifactEnvelope, String> {
    let target = pool
        .iter()
        .copied()
        .find(|artifact| artifact.artifact_id == id && artifact.kind() == *kind)
        .ok_or_else(|| format!("source command cannot resolve {id}"))?;
    if !owner.dependencies.iter().any(|dependency| {
        dependency.artifact_id == id
            && dependency.kind == *kind
            && dependency.required
            && dependency.version.as_deref() == Some(target.version.as_str())
    }) {
        return Err(format!(
            "source command requires an exact required dependency on {id}@{}",
            target.version
        ));
    }
    if crate::artifact_set::scope_conflict(&owner.scope, &target.scope).is_some() {
        return Err(format!("source command cannot cross the scope of {id}"));
    }
    Ok(target)
}

#[allow(
    clippy::too_many_lines,
    reason = "one trust-boundary check binds source identity, raw version, and guarded capability together"
)]
fn check_binding(
    owner: &ArtifactEnvelope,
    rule: &RuleRoot,
    command: &RuleSourceCommand,
    pool: &[&ArtifactEnvelope],
) -> Result<(), String> {
    let connector_artifact = resolve(owner, &command.connector, &ArtifactKind::Connector, pool)?;
    let connector = decode!(ConnectorRoot, &connector_artifact.payload.to_json_value())
        .ok_or("source connector does not decode")?;
    let mapping_artifact = resolve(owner, &command.mapping, &ArtifactKind::Mapping, pool)?;
    let mapping = decode!(MappingIr, &mapping_artifact.payload.to_json_value())
        .ok_or("source mapping does not decode")?;
    let map = mapping
        .maps
        .iter()
        .find(|map| map.id == command.map)
        .ok_or("source command names an unknown map")?;
    let entry = rule
        .slice
        .iter()
        .find(|entry| entry.name == command.slice)
        .ok_or("source command names an unknown slice")?;
    if entry.optional
        || entry.key != SliceKey::TriggerObject
        || rule.triggers.is_empty()
        || rule
            .triggers
            .iter()
            .any(|trigger| trigger.object_type() != Some(&entry.object_type))
    {
        return Err(
            "source command target must be the exact required triggering object".to_owned(),
        );
    }
    let target = pool
        .iter()
        .copied()
        .find(|artifact| artifact.artifact_id == map.target.entity)
        .ok_or("source map target is unresolved")?;
    let ArtifactPayload::Entity(entity) = &target.payload else {
        return Err("source map target is not an entity".to_owned());
    };
    let target = crate::entity_object_type(entity)?.ok_or("source target is not an object type")?;
    if target.root.object_type.name != entry.object_type
        || map.source.nested_in.is_some()
        || map.subject.as_str().trim() != "input.name"
        || map.source_record.as_str().trim() != "input.name"
    {
        return Err("source target must preserve the parent source document identity".to_owned());
    }
    let schema_artifact = pool
        .iter()
        .copied()
        .find(|artifact| {
            artifact.artifact_id == mapping.source.schema_id
                && artifact.version == mapping.source.version
        })
        .ok_or("source schema is unresolved")?;
    let schema = decode!(SourceSchemaIr, &schema_artifact.payload.to_json_value())
        .ok_or("source schema does not decode")?;
    let dataset = schema
        .datasets
        .iter()
        .find(|dataset| dataset.id == map.source.dataset)
        .ok_or("source dataset is unresolved")?;
    if dataset.native_name != command.doctype
        || connector_artifact.namespace != mapping_artifact.namespace
        || schema.source.system != connector_artifact.namespace
    {
        return Err(
            "source command document type or connector source does not match its mapping"
                .to_owned(),
        );
    }
    let version = map
        .properties
        .iter()
        .find(|property| property.property.as_str() == command.modified_property)
        .ok_or("source modified property is not mapped")?;
    if version.nested.is_some()
        || version.value.as_str().trim() != "input.modified"
        || !schema
            .fields
            .iter()
            .any(|field| field.dataset == dataset.id && field.name == "modified")
    {
        return Err("source version must map the raw parent input.modified directly".to_owned());
    }
    let profiles: Vec<_> = connector
        .commands
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|profile| {
            profile.operation == ActionCommandOperation::Update
                && profile.action_id.is_none()
                && profile
                    .doctypes
                    .iter()
                    .any(|doctype| doctype == &command.doctype)
        })
        .collect();
    let [profile] = profiles.as_slice() else {
        return Err(
            "source command requires one explicit generic update capability for its doctype"
                .to_owned(),
        );
    };
    let idempotency = profile
        .idempotency
        .as_ref()
        .ok_or("source update requires receipts and idempotency")?;
    let lifecycle = profile
        .lifecycle
        .as_ref()
        .ok_or("source update requires source CAS")?;
    if lifecycle.version_field != "modified" {
        return Err("source update must guard the declared modified version".to_owned());
    }
    let reserved = [
        &idempotency.key_field,
        &idempotency.receipt.table_field,
        &lifecycle.version_field,
        &lifecycle.status_field,
    ];
    let allowed = |field: &str| {
        !field.starts_with('_')
            && field != "flags"
            && ![
                "name",
                "doctype",
                "parent",
                "parenttype",
                "parentfield",
                "idx",
            ]
            .contains(&field)
            && !reserved.iter().any(|reserved| reserved.as_str() == field)
            && !profile.fixed_body.contains_key(field)
            && profile
                .body_fields
                .iter()
                .any(|allowed| allowed == "*" || allowed == field)
    };
    if command.fields.keys().any(|field| !allowed(field)) {
        return Err("source update contains an undeclared or reserved field".to_owned());
    }
    if profile.fixed_body.keys().any(|field| {
        field.starts_with('_')
            || field == "flags"
            || reserved.iter().any(|reserved| reserved.as_str() == field)
            || [
                "name",
                "doctype",
                "parent",
                "parenttype",
                "parentfield",
                "idx",
            ]
            .contains(&field.as_str())
    }) {
        return Err(
            "source update defaults cannot change identity, lifecycle, version, or receipt fields"
                .to_owned(),
        );
    }
    let mut tables = BTreeSet::new();
    for merge in &command.child_merges {
        if !tables.insert(&merge.field)
            || command.fields.contains_key(&merge.field)
            || !allowed(&merge.field)
            || !profile.merge_profiles.iter().any(|profile| {
                profile.field == merge.field
                    && profile.ownership_field == merge.ownership_field
                    && profile.row_key_field == merge.row_key_field
            })
        {
            return Err("source child merge must match a unique admitted table profile".to_owned());
        }
    }
    let output = serde_json::to_value(&rule.output_schema).map_err(|error| error.to_string())?;
    if output
        .pointer("/properties/facts/items/properties/objectType/enum")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|types| {
            types
                .iter()
                .any(|ty| ty.as_str() == Some(entry.object_type.as_str()))
        })
    {
        return Err(
            "source-owned target facts cannot be written directly by the source-command rule"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn source_rule() -> Value {
        let mut fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json"
        ))
        .expect("recorded rule");
        let root = &mut fixture["payload"];
        root["sourceCommands"] = json!([{
            "id": "derive", "connector": "erpnext.connector", "mapping": "erpnext.mapping",
            "map": "item", "doctype": "Item", "slice": "order", "modifiedProperty": "modified",
            "fields": {"style_code": "slice.order.poNumber"}
        }]);
        root["slice"] =
            json!([{"name":"order", "objectType":"erpnext.Item", "key":"triggerObject"}]);
        root["triggers"] = json!([{"on":"modified", "objectType":"erpnext.Item"}]);
        root["outputSchema"]["properties"]["commands"]["items"] = json!({
            "type":"object", "properties":{"sourceCommandId":{"type":"string", "enum":["derive"]}},
            "required":["sourceCommandId"], "additionalProperties":false
        });
        root.clone()
    }

    fn diagnostics(value: Value) -> Vec<Diagnostic> {
        let payload = serde_json::from_value(value).expect("IR payload");
        let mut diagnostics = Vec::new();
        validate(&payload, &mut diagnostics, Some("rule"));
        diagnostics
    }

    #[test]
    fn declared_source_activation_and_typed_slice_expression_admit() {
        assert_eq!(diagnostics(source_rule()), Vec::new());
    }

    #[test]
    fn source_activations_reject_counterfeit_origin_and_undeclared_ids() {
        let mut root = source_rule();
        root["outputSchema"]["properties"]["commands"]["items"]["properties"]["actionId"] =
            json!({"type":"string"});
        assert!(diagnostics(root).iter().any(|finding| finding.code == CODE));
        let mut root = source_rule();
        root["outputSchema"]["properties"]["commands"]["items"]["properties"]["sourceCommandId"]
            ["enum"] = json!(["other"]);
        assert!(diagnostics(root).iter().any(|finding| finding.code == CODE));
    }

    #[test]
    fn source_expressions_reject_unknown_reads_and_optional_targets() {
        let mut root = source_rule();
        root["sourceCommands"][0]["fields"]["style_code"] = json!("slice.order.not_declared");
        assert!(diagnostics(root).iter().any(|finding| finding.code == CODE));
        let mut root = source_rule();
        root["slice"][0]["optional"] = json!(true);
        assert!(diagnostics(root).iter().any(|finding| finding.code == CODE));
    }

    #[test]
    fn connector_qualification_rejects_root_paths_and_duplicate_apps() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/connectors/frappe.connector.json"
        ))
        .expect("connector fixture");
        for invalid in [
            json!({"endpoint":"https://other.example/versions", "responsePath":"/message", "components":[{"app":"frappe", "majors":[16]}], "requireMatchingMajors":true}),
            json!({"endpoint":"/versions", "responsePath":"$", "components":[{"app":"frappe", "majors":[16]}], "requireMatchingMajors":true}),
            json!({"endpoint":"/versions", "responsePath":"/message", "components":[{"app":"frappe", "majors":[16]}, {"app":"frappe", "majors":[16]}], "requireMatchingMajors":true}),
        ] {
            let mut payload = fixture["payload"].clone();
            payload["supportedVersions"] = invalid;
            let payload = serde_json::from_value(payload).expect("payload");
            let mut findings = Vec::new();
            validate_connector_profile(&payload, &mut findings, None);
            assert!(!findings.is_empty());
        }
    }
}
