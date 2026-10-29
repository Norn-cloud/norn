//! A `decide` rule's trigger and slice declarations (T3-DEC-3, #2671).
//!
//! `RuleRoot.triggers` names the committed ledger events a host runs the
//! decision on; `RuleRoot.slice` names the objects it loads, in order, into
//! the `slice` input record. The checker verifies them in two tiers:
//!
//! - **T2, one artifact** (`validate_rule_bindings`,
//!   [`codes::INVALID_BINDING`]): only a `decide` graph declares them; slice
//!   names are unique and non-empty; a `property` key names an *earlier*
//!   entry; a `triggerObject` key needs object-specific triggers of its type,
//!   and `triggerBefore`, `triggerAfter`, and `triggerChanges` need only
//!   `modified` triggers of their declared type. The declared names are
//!   exactly the members of the `slice` input record, so the decision reads
//!   only what it declares.
//! - **T3, the artifact set** (`check_rule_binding_set`,
//!   [`codes::UNKNOWN_BINDING`]): every trigger and slice object type is an
//!   object type of the set or the base, and a `transitioned` trigger names a
//!   transition a lifecycle of that type declares.
//!
//! Typing every expression site against the declared slice (and its read
//! depth) is the rest of T3-DEC-3.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, JsonValue, MAX_RULE_CAUSAL_DEPTH, PropertyType, RuleTrigger,
    SliceDeclaration, SliceKey,
};

use crate::action_set::Model;
use crate::entity_artifacts::EntityArtifactSetContext;
use crate::entity_instance::{EffectiveKind, EffectiveObjectType, EffectiveProperty};
use crate::envelope::invalid_diagnostic;
use crate::lifecycle_writes::finding;

/// Stable codes of rule bindings.
pub mod codes {
    /// A `decide` rule's trigger or slice declaration is malformed.
    pub const INVALID_BINDING: &str = "rule.validation.invalid_binding";
    /// A trigger or slice names an object type, or a transition, the
    /// artifact set and the base do not define.
    pub const UNKNOWN_BINDING: &str = "artifact_set.decision.unknown_binding";
}

fn decode_triggers(value: Option<&JsonValue>) -> Option<Vec<RuleTrigger>> {
    serde_json::from_value(serde_json::to_value(value?).ok()?).ok()
}

fn decode_slice(value: Option<&JsonValue>) -> Option<Vec<SliceDeclaration>> {
    serde_json::from_value(serde_json::to_value(value?).ok()?).ok()
}

/// T2 checks of `payload`'s `triggers` and `slice` (see the module docs).
/// Members that do not decode are left to the typed root.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the declarations, in the order the module docs list the rules"
)]
pub(crate) fn validate_rule_bindings(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    let declared = payload.contains_key("triggers") || payload.contains_key("slice");
    if !declared {
        return;
    }
    let mut invalid = |message: String, path: &str| {
        diagnostics.push(invalid_diagnostic(
            codes::INVALID_BINDING,
            message,
            path,
            artifact_id,
        ));
    };
    if payload.get("contract").and_then(JsonValue::as_string) != Some("decide") {
        invalid(
            "triggers and slice are only allowed on a decide graph".to_owned(),
            "payload.contract",
        );
        return;
    }
    // A member that is present but does not decode is itself a finding: a
    // malformed binding never silently drops (or un-hosts) a rule.
    let triggers: Vec<RuleTrigger> = match payload
        .get("triggers")
        .map(|value| decode_triggers(Some(value)))
    {
        None => Vec::new(),
        Some(Some(triggers)) => triggers,
        Some(None) => {
            invalid(
                "triggers must be a list of supported object or source trigger records".to_owned(),
                "payload.triggers",
            );
            return;
        }
    };
    let slice: Vec<SliceDeclaration> =
        match payload.get("slice").map(|value| decode_slice(Some(value))) {
            None => Vec::new(),
            Some(Some(slice)) => slice,
            Some(None) => {
                invalid(
                    "slice must be a list of {name, objectType, key} records".to_owned(),
                    "payload.slice",
                );
                return;
            }
        };
    let mut names = BTreeSet::new();
    for (index, entry) in slice.iter().enumerate() {
        let path = format!("payload.slice[{index}]");
        if entry.name.trim().is_empty() || !names.insert(entry.name.as_str()) {
            invalid(
                format!("slice name {:?} is empty or repeated", entry.name),
                &format!("{path}.name"),
            );
        }
        match &entry.key {
            SliceKey::TriggerObject => {
                // Every trigger must name an object of exactly this type, so
                // the key is never applied to an unrelated type.
                let same_type = !triggers.is_empty()
                    && triggers
                        .iter()
                        .all(|trigger| trigger.object_type() == Some(&entry.object_type));
                if !same_type {
                    invalid(
                        format!(
                            "a triggerObject key needs triggers that all name an object of {}",
                            entry.object_type.as_str()
                        ),
                        &format!("{path}.key"),
                    );
                }
            }
            SliceKey::TriggerBefore | SliceKey::TriggerAfter | SliceKey::TriggerChanges => {
                let same_type = !triggers.is_empty()
                    && triggers.iter().all(|trigger| {
                        matches!(trigger, RuleTrigger::Modified { object_type, .. }
                            if *object_type == entry.object_type)
                    });
                if !same_type {
                    invalid(
                        format!(
                            "a {:?} key needs only modified triggers of {}",
                            entry.key,
                            entry.object_type.as_str()
                        ),
                        &format!("{path}.key"),
                    );
                }
            }
            SliceKey::Property { from, .. } | SliceKey::Link { from, .. } => {
                // The key comes from one earlier, single-object entry: a list
                // entry has no single value to read a key from.
                match slice.iter().take(index).find(|other| other.name == *from) {
                    None => invalid(
                        format!("slice key names {from:?}, which is not an earlier slice entry"),
                        &format!("{path}.key"),
                    ),
                    Some(earlier)
                        if matches!(
                            &earlier.key,
                            SliceKey::TriggerBefore
                                | SliceKey::TriggerAfter
                                | SliceKey::TriggerChanges
                        ) =>
                    {
                        invalid(
                            format!(
                                "slice key names {from:?}, a trigger snapshot input that cannot key another entry"
                            ),
                            &format!("{path}.key"),
                        );
                    }
                    Some(earlier) if earlier.is_list() => invalid(
                        format!(
                            "slice key names {from:?}, a list entry; a key is read from a single-object entry"
                        ),
                        &format!("{path}.key"),
                    ),
                    Some(_) => {}
                }
            }
        }
        if entry.optional && !matches!(entry.key, SliceKey::Property { .. }) {
            invalid(
                format!(
                    "slice entry {:?} is optional, but only a property-keyed entry can be: a trigger object always resolves, and a list entry is empty rather than missing",
                    entry.name
                ),
                &format!("{path}.optional"),
            );
        }
    }
    for (index, trigger) in triggers.iter().enumerate() {
        let path = format!("payload.triggers[{index}]");
        if trigger
            .max_causal_depth()
            .is_some_and(|depth| depth == 0 || depth > MAX_RULE_CAUSAL_DEPTH)
        {
            invalid(
                format!("maxCausalDepth must be between 1 and {MAX_RULE_CAUSAL_DEPTH}"),
                &format!("{path}.maxCausalDepth"),
            );
        }
        if let RuleTrigger::Modified {
            changed_properties: Some(properties),
            ..
        } = trigger
        {
            let mut seen = BTreeSet::new();
            if properties.is_empty()
                || properties
                    .iter()
                    .any(|property| !seen.insert(property.as_str()))
            {
                invalid(
                    "changedProperties must be non-empty and contain no duplicate ids".to_owned(),
                    &format!("{path}.changedProperties"),
                );
            }
        }
    }
    let slice_record = payload
        .get("inputSchema")
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(JsonValue::as_object)
        .and_then(|properties| properties.get("slice"))
        .and_then(JsonValue::as_object);
    let slice_properties = slice_record
        .and_then(|slice| slice.get("properties"))
        .and_then(JsonValue::as_object);
    let members: Option<BTreeSet<&str>> =
        slice_properties.map(|properties| properties.keys().map(String::as_str).collect());
    // A list entry fills a list member; a single-object entry a record
    // member. An optional entry may be absent, so its member is not required.
    let required: BTreeSet<&str> = slice_record
        .and_then(|slice| slice.get("required"))
        .and_then(JsonValue::as_array)
        .map(|required| required.iter().filter_map(JsonValue::as_string).collect())
        .unwrap_or_default();
    for (index, entry) in slice.iter().enumerate() {
        let Some(member) = slice_properties.and_then(|properties| properties.get(&entry.name))
        else {
            continue;
        };
        let kind = member
            .as_object()
            .and_then(|member| member.get("type"))
            .and_then(JsonValue::as_string);
        let expected = match &entry.key {
            SliceKey::TriggerChanges | SliceKey::Link { .. } => "array",
            SliceKey::TriggerObject
            | SliceKey::TriggerBefore
            | SliceKey::TriggerAfter
            | SliceKey::Property { .. } => "object",
        };
        // A link list holds object records; triggerChanges holds strings.
        let items = member
            .as_object()
            .and_then(|member| member.get("items"))
            .and_then(JsonValue::as_object)
            .and_then(|items| items.get("type"))
            .and_then(JsonValue::as_string);
        let expected_items = match &entry.key {
            SliceKey::Link { .. } => Some("object"),
            SliceKey::TriggerChanges => Some("string"),
            SliceKey::TriggerObject
            | SliceKey::TriggerBefore
            | SliceKey::TriggerAfter
            | SliceKey::Property { .. } => None,
        };
        if kind != Some(expected) || expected_items.is_some_and(|expected| items != Some(expected))
        {
            invalid(
                format!(
                    "slice entry {:?} fills an {expected} member of the slice input record (with items {expected_items:?}), not {kind:?}",
                    entry.name
                ),
                &format!("payload.slice[{index}]"),
            );
        }
        if matches!(&entry.key, SliceKey::TriggerBefore | SliceKey::TriggerAfter)
            && let Some(reason) = snapshot_schema_error(member)
        {
            invalid(
                format!(
                    "slice entry {:?} has an invalid snapshot schema: {reason}",
                    entry.name
                ),
                &format!("payload.slice[{index}]"),
            );
        }
        if entry.optional && required.contains(entry.name.as_str()) {
            invalid(
                format!(
                    "slice entry {:?} is optional, so the slice input record must not require it",
                    entry.name
                ),
                &format!("payload.slice[{index}].optional"),
            );
        }
    }
    // A hosted rule (one with triggers) reads exactly what it declares: the
    // declared names, omitted meaning none, are the slice input record.
    let hosted = !triggers.is_empty() || !slice.is_empty();
    if let Some(members) = members
        && hosted
        && members != names
    {
        invalid(
            format!(
                "the declared slice {names:?} must be exactly the members of the slice input record {members:?}"
            ),
            "payload.slice",
        );
    }
}

/// Validate the fixed, closed input envelope for a trigger snapshot. The
/// entity-specific `values` properties are checked in the artifact-set pass.
fn snapshot_schema_error(schema: &JsonValue) -> Option<String> {
    let schema = schema.as_object()?;
    if schema.get("type").and_then(JsonValue::as_string) != Some("object")
        || !matches!(
            schema.get("additionalProperties"),
            Some(JsonValue::Bool(false))
        )
    {
        return Some("the snapshot must be a closed object".to_owned());
    }
    let properties = schema.get("properties").and_then(JsonValue::as_object)?;
    let names: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
    if names != BTreeSet::from(["version", "values"]) {
        return Some("the snapshot must declare exactly version and values".to_owned());
    }
    let required: BTreeSet<&str> = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_string)
        .collect();
    if required != BTreeSet::from(["version", "values"]) {
        return Some("version and values must both be required".to_owned());
    }
    let version = properties.get("version")?.as_object()?;
    if version.get("type").and_then(JsonValue::as_string) != Some("integer") {
        return Some("version must have integer type".to_owned());
    }
    let values = properties.get("values")?.as_object()?;
    if values.get("type").and_then(JsonValue::as_string) != Some("object")
        || !matches!(
            values.get("additionalProperties"),
            Some(JsonValue::Bool(false))
        )
        || values
            .get("properties")
            .and_then(JsonValue::as_object)
            .is_none()
    {
        return Some("values must be a closed object with declared properties".to_owned());
    }
    None
}

/// Check the `values` envelope against the complete effective stored shape.
/// Kernel snapshots materialize every stored property, including nulls, and
/// omit computed properties.
fn snapshot_values_schema_error(
    schema: &JsonValue,
    effective: &EffectiveObjectType,
) -> Option<String> {
    let values = schema
        .as_object()?
        .get("properties")?
        .as_object()?
        .get("values")?
        .as_object()?;
    let values_properties = values.get("properties")?.as_object()?;
    let expected: BTreeSet<&str> = effective
        .properties
        .iter()
        .filter(|property| !property.computed)
        .map(|property| property.id.as_str())
        .collect();
    let declared: BTreeSet<&str> = values_properties.keys().map(String::as_str).collect();
    if expected != declared {
        return Some(format!(
            "values must declare exactly the stored properties {expected:?}"
        ));
    }
    let required: BTreeSet<&str> = values
        .get("required")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_string)
        .collect();
    if required != expected {
        return Some(
            "values must require every stored property in its complete snapshot".to_owned(),
        );
    }
    for property in effective
        .properties
        .iter()
        .filter(|property| !property.computed)
    {
        let Some(member) = values_properties.get(property.id.as_str()) else {
            return Some(format!(
                "values omits stored property {}",
                property.id.as_str()
            ));
        };
        if let Some(reason) = snapshot_property_schema_error(member, property) {
            return Some(reason);
        }
    }
    None
}

/// Match a declared snapshot member to its effective stored property.
fn snapshot_property_schema_error(
    member: &JsonValue,
    property: &EffectiveProperty,
) -> Option<String> {
    if let EffectiveKind::Base(PropertyType::JsonExt { schema }) = &property.kind {
        return snapshot_json_schema_error(member, property, schema.as_ref());
    }
    let Some(schema_type) = member.as_object().and_then(|member| member.get("type")) else {
        return Some(format!(
            "{} must declare its scalar type",
            property.id.as_str()
        ));
    };
    let allows_null = schema_type
        .as_array()
        .is_some_and(|types| types.iter().any(|value| value.as_string() == Some("null")));
    let scalar_type = schema_type.as_string().or_else(|| {
        schema_type.as_array().and_then(|types| {
            types
                .iter()
                .find_map(|value| value.as_string().filter(|kind| *kind != "null"))
        })
    });
    let expected_type = match &property.kind {
        EffectiveKind::Base(
            PropertyType::Text { .. }
            | PropertyType::Enum { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {},
        ) => Some("string"),
        EffectiveKind::Base(PropertyType::Integer {}) => Some("integer"),
        EffectiveKind::Base(PropertyType::Decimal { .. }) => Some("number"),
        EffectiveKind::Base(PropertyType::Boolean {}) => Some("boolean"),
        EffectiveKind::Base(PropertyType::Record { .. }) => Some("object"),
        EffectiveKind::Base(PropertyType::JsonExt { .. }) => None,
        EffectiveKind::Base(PropertyType::Ref { .. })
        | EffectiveKind::Registry(_)
        | EffectiveKind::Unresolved => {
            return Some(format!(
                "{} has an unresolved scalar type",
                property.id.as_str()
            ));
        }
    };
    if expected_type.is_some_and(|expected| scalar_type != Some(expected)) {
        return Some(format!(
            "{} must have {} type",
            property.id.as_str(),
            expected_type.unwrap_or("a supported")
        ));
    }
    if property.nullable != allows_null {
        return Some(format!(
            "{} must {} null",
            property.id.as_str(),
            if property.nullable { "allow" } else { "reject" }
        ));
    }
    if matches!(
        property.kind,
        EffectiveKind::Base(PropertyType::Record { .. })
    ) {
        return snapshot_record_schema_error(member, property);
    }
    if let Some(reason) = snapshot_scalar_enum_error(member, property) {
        return Some(reason);
    }
    None
}

/// A complete record snapshot preserves every declared member recursively.
fn snapshot_record_schema_error(
    member: &JsonValue,
    property: &EffectiveProperty,
) -> Option<String> {
    let object = member.as_object()?;
    let expected: BTreeSet<&str> = property
        .record_members()
        .iter()
        .map(|member| member.id.as_str())
        .collect();
    let declared = object.get("properties").and_then(JsonValue::as_object);
    let required: BTreeSet<&str> = object
        .get("required")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_string)
        .collect();
    if !matches!(
        object.get("additionalProperties"),
        Some(JsonValue::Bool(false))
    ) || declared.is_none_or(|members| {
        members.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected
    }) || required != expected
    {
        return Some(format!(
            "{} must declare and require exactly its closed record members {expected:?}",
            property.id.as_str()
        ));
    }
    for child in property.record_members() {
        let schema = declared.and_then(|members| members.get(child.id.as_str()))?;
        if let Some(reason) = snapshot_property_schema_error(schema, child) {
            return Some(format!("{}: {reason}", property.id.as_str()));
        }
    }
    None
}

/// Ensure a snapshot schema's enum restriction accepts every legal effective
/// value. The closed snapshot dialect supports string enums only, and nullable
/// string unions do not support enum restrictions.
fn snapshot_scalar_enum_error(member: &JsonValue, property: &EffectiveProperty) -> Option<String> {
    let declared = member.as_object().and_then(|member| member.get("enum"))?;
    let Some(declared) = declared.as_array() else {
        return Some(format!(
            "{} snapshot enum must be a list",
            property.id.as_str()
        ));
    };
    let EffectiveKind::Base(PropertyType::Enum { values }) = &property.kind else {
        return Some(format!(
            "{} cannot use a snapshot enum to narrow its effective scalar values",
            property.id.as_str()
        ));
    };
    if property.nullable {
        return Some(format!(
            "{} nullable enum snapshots must not declare an enum restriction",
            property.id.as_str()
        ));
    }
    if values
        .iter()
        .any(|value| !declared.contains(&JsonValue::String(value.value.clone())))
    {
        return Some(format!(
            "{} snapshot enum must include every effective value",
            property.id.as_str()
        ));
    }
    None
}

/// JSON extension snapshots preserve the entity's exact closed JSON contract.
fn snapshot_json_schema_error(
    member: &JsonValue,
    property: &EffectiveProperty,
    declared: Option<&BTreeMap<String, JsonValue>>,
) -> Option<String> {
    use norn_expr::schema::Schema;

    let Some(declared) = declared else {
        return Some(format!(
            "{} needs a closed JSON schema to bind a complete snapshot",
            property.id.as_str()
        ));
    };
    let expected = match Schema::parse(&JsonValue::Object(declared.clone()), "property") {
        Ok(Schema::Nullable(inner)) => *inner,
        Ok(schema) => schema,
        Err(error) => {
            return Some(format!(
                "{} has an unsupported JSON schema: {}",
                property.id.as_str(),
                error.reason
            ));
        }
    };
    if expected == Schema::Null && !property.nullable {
        return Some(format!(
            "{} rejects null but declares a null-only JSON schema",
            property.id.as_str()
        ));
    }
    let expected = if property.nullable && expected != Schema::Null {
        Schema::Nullable(Box::new(expected))
    } else {
        expected
    };
    match Schema::parse(member, "snapshot") {
        Ok(actual) if actual == expected => None,
        _ => Some(format!(
            "{} must match its declared JSON schema and nullability",
            property.id.as_str()
        )),
    }
}

/// T3 checks of every rule's bindings against the set's object types (see
/// the module docs).
pub(crate) fn check_rule_binding_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<crate::Diagnostic> {
    let bound: Vec<(&ArtifactEnvelope, Vec<RuleTrigger>, Vec<SliceDeclaration>)> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Rule)
        .filter_map(|artifact| {
            let payload = artifact.payload.to_json_value();
            let object = payload.as_object()?;
            let triggers: Vec<RuleTrigger> =
                decode_triggers(object.get("triggers")).unwrap_or_default();
            let slice: Vec<SliceDeclaration> =
                decode_slice(object.get("slice")).unwrap_or_default();
            (!triggers.is_empty() || !slice.is_empty()).then_some((artifact, triggers, slice))
        })
        .collect();
    if bound.is_empty() {
        return Vec::new();
    }
    let model = Model::new(artifacts, context.base, context.registry, true);
    let mut findings = Vec::new();
    for (artifact, triggers, slice) in bound {
        check_trigger_binding_set(&model, artifact, &triggers, &mut findings);
        check_slice_binding_set(&model, artifact, &slice, &mut findings);
    }
    findings
}

/// Check trigger object types, transitions, and modified-property filters.
fn check_trigger_binding_set(
    model: &Model<'_>,
    artifact: &ArtifactEnvelope,
    triggers: &[RuleTrigger],
    findings: &mut Vec<crate::Diagnostic>,
) {
    for (index, trigger) in triggers.iter().enumerate() {
        let pointer = format!("/payload/triggers/{index}");
        let (object_type, transition) = match trigger {
            RuleTrigger::Transitioned {
                object_type,
                transition,
                ..
            } => (object_type, Some(transition)),
            RuleTrigger::Created { object_type, .. }
            | RuleTrigger::Deleted { object_type, .. }
            | RuleTrigger::Modified { object_type, .. } => (object_type, None),
            RuleTrigger::Observed { .. } => continue,
        };
        let Some(defined) = model.object_type(object_type.as_str()) else {
            findings.push(finding(
                codes::UNKNOWN_BINDING,
                format!(
                    "the trigger names {}, which is not an object type of the artifact set or the installed base",
                    object_type.as_str()
                ),
                artifact,
                &pointer,
            ));
            continue;
        };
        if let Some(transition) = transition {
            let declared = defined.lifecycles.as_ref().is_ok_and(|lifecycles| {
                lifecycles.iter().any(|effective| {
                    effective
                        .lifecycle
                        .transitions
                        .iter()
                        .any(|declared| declared.id == *transition)
                })
            });
            if !declared {
                findings.push(finding(
                    codes::UNKNOWN_BINDING,
                    format!(
                        "the trigger names transition {} of {}, which no lifecycle of the type declares",
                        transition.0,
                        object_type.as_str()
                    ),
                    artifact,
                    &format!("{pointer}/transition"),
                ));
            }
        }
        if let RuleTrigger::Modified {
            changed_properties: Some(properties),
            ..
        } = trigger
        {
            let effective = defined
                .effective_object_type
                .as_ref()
                .and_then(|effective| effective.as_ref().ok());
            for (property_index, property) in properties.iter().enumerate() {
                let declared = effective.is_some_and(|effective| {
                    effective
                        .properties
                        .iter()
                        .any(|candidate| candidate.id == *property && !candidate.computed)
                });
                if !declared {
                    findings.push(finding(
                        codes::UNKNOWN_BINDING,
                        format!(
                            "changedProperties names {}, which is not a stored property of {}",
                            property.as_str(),
                            object_type.as_str()
                        ),
                        artifact,
                        &format!("{pointer}/changedProperties/{property_index}"),
                    ));
                }
            }
        }
    }
}

/// Check declared slice object types, immutable snapshots, and link bindings.
fn check_slice_binding_set(
    model: &Model<'_>,
    artifact: &ArtifactEnvelope,
    slice: &[SliceDeclaration],
    findings: &mut Vec<crate::Diagnostic>,
) {
    let rule_payload = artifact.payload.to_json_value();
    let input_properties = rule_payload
        .as_object()
        .and_then(|payload| payload.get("inputSchema"))
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(JsonValue::as_object)
        .and_then(|properties| properties.get("slice"))
        .and_then(JsonValue::as_object)
        .and_then(|slice| slice.get("properties"))
        .and_then(JsonValue::as_object);
    for (index, entry) in slice.iter().enumerate() {
        let defined = model.object_type(entry.object_type.as_str());
        if defined.is_none() {
            findings.push(finding(
                codes::UNKNOWN_BINDING,
                format!(
                    "slice entry {} names {}, which is not an object type of the artifact set or the installed base",
                    entry.name,
                    entry.object_type.as_str()
                ),
                artifact,
                &format!("/payload/slice/{index}/objectType"),
            ));
        }
        if matches!(&entry.key, SliceKey::TriggerBefore | SliceKey::TriggerAfter)
            && let Some(member) =
                input_properties.and_then(|properties| properties.get(&entry.name))
            && let Some(defined) = defined
            && let Some(Ok(effective)) = defined.effective_object_type.as_ref()
            && let Some(reason) = snapshot_values_schema_error(member, effective)
        {
            findings.push(finding(
                codes::UNKNOWN_BINDING,
                format!(
                    "slice snapshot {} does not match {}: {reason}",
                    entry.name,
                    entry.object_type.as_str()
                ),
                artifact,
                &format!(
                    "/payload/inputSchema/properties/slice/properties/{}",
                    entry.name
                ),
            ));
        }
        findings.extend(list_entry_finding(model, artifact, slice, index, entry));
    }
}

/// T3 check of one list entry (#3079): it reads link `link` of its source
/// entry's type, so the link must be declared there (or on a supertype), and
/// the entry names exactly its target type, so every listed object is loaded
/// as the type the link admits.
fn list_entry_finding(
    model: &Model<'_>,
    artifact: &ArtifactEnvelope,
    slice: &[SliceDeclaration],
    index: usize,
    entry: &SliceDeclaration,
) -> Option<crate::Diagnostic> {
    let SliceKey::Link { from, link } = &entry.key else {
        return None;
    };
    let source = slice.iter().find(|other| other.name == *from)?;
    match model.link_target(source.object_type.as_str(), link) {
        None => Some(finding(
            codes::UNKNOWN_BINDING,
            format!(
                "slice entry {} reads link {link:?} of {}, which neither the type nor a supertype declares",
                entry.name,
                source.object_type.as_str()
            ),
            artifact,
            &format!("/payload/slice/{index}/key"),
        )),
        Some(target) if target != entry.object_type.as_str() => Some(finding(
            codes::UNKNOWN_BINDING,
            format!(
                "slice entry {} names {}, but link {link:?} of {} targets {target}",
                entry.name,
                entry.object_type.as_str(),
                source.object_type.as_str()
            ),
            artifact,
            &format!("/payload/slice/{index}/objectType"),
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
#[path = "rule_bindings_tests.rs"]
mod tests;
