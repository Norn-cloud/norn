//! Admission checks for checked `links` items on `DecisionOutput` (#1586).
//!
//! A decide graph's `outputSchema` may declare an optional `links` array of
//! closed [`norn_ir::LinkChange`] records alongside `facts`, `commands`, and
//! `timers`. This module checks:
//!
//! - **T2** (`validate_link_change_schema`): closed record members, required
//!   fields, string `enum`s for `op`, `sourceType`, `link`, and `targetType`,
//!   string keys, and optional `values` object only when `add` is emitted.
//! - **T3** ([`check_decision_links_set`]): every `sourceType` and `targetType`
//!   resolves in scope, every declared `sourceType`, `link`, and `targetType`
//!   participates in a valid `(source, link, target)` triple, and every
//!   declared `values` property matches a non-computed property of an admitted
//!   emitted link.
//! - **Action writer T3** (`synchronous_writer_link_findings`): a
//!   `synchronousRule` action's link sources must be covered by the action's
//!   declared writes (outgoing links share source write authority), targets
//!   must be written or captured in the slice, and links may not target
//!   `norn.*` system types, owned-child types, or owned-collection links.

use std::collections::BTreeSet;

use norn_ir::{
    ActionRoot, ArtifactEnvelope, ArtifactKind, DiagnosticRemediation, DiagnosticSeverity,
    JsonValue, LINK_CHANGE_MEMBERS, LINK_CHANGE_REQUIRED_MEMBERS, LinkChangeOp, PropertyType,
    WriteChange,
};

use crate::action_set::{Model, codes as action_codes};
use crate::entity_artifacts::EntityArtifactSetContext;
use crate::entity_instance::{EffectiveKind, EffectiveProperty};
use crate::entity_scopes::ScopedViews;
use crate::envelope::invalid_diagnostic;
use crate::jdm_profile::codes as jdm_codes;
use crate::lifecycle_writes::{
    FactSchema as LinkSchema, codes as lifecycle_codes, member, string_enum, type_of,
};
use crate::{Diagnostic, DiagnosticCode, Span};

const ITEMS: &str = "payload.outputSchema.properties.links.items";
const ITEMS_POINTER: &str = "/payload/outputSchema/properties/links/items";

fn link_schema(output_schema: &JsonValue) -> Option<LinkSchema<'_>> {
    LinkSchema::of_array_member(output_schema, "links")
}

/// T2: check a decide graph's optional `links` item schema against the
/// canonical [`norn_ir::LinkChange`] record.
pub(crate) fn validate_link_change_schema(
    output_schema: Option<&JsonValue>,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(schema) = output_schema.and_then(link_schema) else {
        return;
    };
    let mut push = |message: String, path: String| {
        diagnostics.push(invalid_diagnostic(
            jdm_codes::INVALID_DECISION_CONTRACT,
            message,
            path,
            artifact_id,
        ));
    };
    for name in schema.properties.keys() {
        if !LINK_CHANGE_MEMBERS.contains(&name.as_str()) {
            push(
                format!(
                    "'{name}' is not a link change member; a link change carries {LINK_CHANGE_MEMBERS:?}"
                ),
                format!("{ITEMS}.properties.{name}"),
            );
        }
    }
    for name in LINK_CHANGE_REQUIRED_MEMBERS {
        if schema.get(name).is_none() {
            push(
                format!("a link change declares '{name}'"),
                format!("{ITEMS}.properties"),
            );
        } else if !schema.required.contains(&name) {
            push(
                format!("every link change carries '{name}': it must be required"),
                format!("{ITEMS}.required"),
            );
        }
    }
    for name in ["sourceKey", "targetKey"] {
        if schema.get(name).is_some() && type_of(schema.get(name)) != Some("string") {
            push(
                format!("a link change's '{name}' is a string"),
                format!("{ITEMS}.properties.{name}"),
            );
        }
    }
    for (name, what) in [
        ("op", "the link ops the decision emits (add or remove)"),
        ("sourceType", "the source object types of emitted links"),
        ("link", "the link ids the decision emits"),
        ("targetType", "the target object types of emitted links"),
    ] {
        let Some(declared) = schema.get(name) else {
            continue;
        };
        if string_enum(Some(declared)).is_none() {
            push(
                format!("a link change's '{name}' is a non-empty string enum of {what}"),
                format!("{ITEMS}.properties.{name}"),
            );
        }
    }
    let ops = schema.values("op").unwrap_or_default();
    let mut admits_add = false;
    for (index, value) in ops.iter().enumerate() {
        match LinkChangeOp::parse(value) {
            Some(LinkChangeOp::Add) => admits_add = true,
            Some(LinkChangeOp::Remove) => {}
            None => push(
                format!("'{value}' is not a link change op: add or remove"),
                format!("{ITEMS}.properties.op.enum[{index}]"),
            ),
        }
    }
    if let Some(values) = schema.get("values") {
        if type_of(Some(values)) != Some("object") {
            push(
                "a link change's 'values' is a closed record of link property values".to_owned(),
                format!("{ITEMS}.properties.values"),
            );
        } else if !admits_add {
            push(
                "only an 'add' link change carries 'values'".to_owned(),
                format!("{ITEMS}.properties.values"),
            );
        }
    }
}

/// T3: check every candidate decide rule declaring `outputSchema.properties.links`
/// against the resolved object-type and link model.
#[must_use]
pub fn check_decision_links_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let decisions: Vec<(&ArtifactEnvelope, JsonValue)> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Rule)
        .filter_map(|artifact| {
            let payload = artifact.payload.to_json_value();
            let object = payload.as_object()?;
            if object.get("contract").and_then(JsonValue::as_string) != Some("decide") {
                return None;
            }
            let output = object.get("outputSchema")?.clone();
            link_schema(&output)?;
            Some((artifact, output))
        })
        .collect();
    if decisions.is_empty() {
        return Vec::new();
    }
    let mut views = ScopedViews::new(artifacts, context.base, context.registry);
    let mut findings = Vec::new();
    for (artifact, output) in &decisions {
        if let Some(schema) = link_schema(output) {
            let model = views.of(artifact).model();
            findings.extend(decision_links_findings(artifact, &schema, model));
        }
    }
    findings
}

fn decision_links_findings(
    artifact: &ArtifactEnvelope,
    schema: &LinkSchema<'_>,
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let resolved_sources = resolve_endpoint_types(
        artifact,
        model,
        &schema.values("sourceType").unwrap_or_default(),
        "sourceType",
        "source",
        &mut findings,
    );
    let resolved_targets = resolve_endpoint_types(
        artifact,
        model,
        &schema.values("targetType").unwrap_or_default(),
        "targetType",
        "target",
        &mut findings,
    );
    let links = schema.values("link").unwrap_or_default();
    let valid_triples = valid_link_triples(model, &resolved_sources, &links, &resolved_targets);

    for (index, link) in links.iter().enumerate() {
        let pointer = format!("{ITEMS_POINTER}/properties/link/enum/{index}");
        let declared_on_source = resolved_sources
            .iter()
            .any(|source| model.link_target(source, link).is_some());
        if !declared_on_source {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link {link:?} is not declared on any of sourceType {resolved_sources:?}"
                ),
                artifact,
                &pointer,
            ));
            continue;
        }
        if !valid_triples
            .iter()
            .any(|(_, candidate, _)| candidate == link)
        {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link {link:?} targets none of declared targetType {resolved_targets:?}"
                ),
                artifact,
                &pointer,
            ));
        }
    }
    if !links.is_empty() && !resolved_sources.is_empty() && !resolved_targets.is_empty() {
        for (s_index, source) in resolved_sources.iter().enumerate() {
            if !valid_triples
                .iter()
                .any(|(candidate, _, _)| candidate == source)
            {
                findings.push(finding(
                    action_codes::CREATE_LINK,
                    format!(
                        "decision link sourceType {source:?} matches none of declared links {links:?} to targetType {resolved_targets:?}"
                    ),
                    artifact,
                    &format!("{ITEMS_POINTER}/properties/sourceType/enum/{s_index}"),
                ));
            }
        }
        for (t_index, target) in resolved_targets.iter().enumerate() {
            if !valid_triples
                .iter()
                .any(|(_, _, candidate)| candidate == target)
            {
                findings.push(finding(
                    action_codes::CREATE_LINK,
                    format!(
                        "decision link targetType {target:?} is targeted by none of declared links {links:?} from sourceType {resolved_sources:?}"
                    ),
                    artifact,
                    &format!("{ITEMS_POINTER}/properties/targetType/enum/{t_index}"),
                ));
            }
        }
    }
    for (source, link, _) in &valid_triples {
        if is_owned_collection_link(model, source, link) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link {link:?} on {source:?} is an owned-collection link and cannot be mutated directly"
                ),
                artifact,
                &format!("{ITEMS_POINTER}/properties/link"),
            ));
        }
    }
    check_link_values_schema(artifact, schema, model, &valid_triples, &mut findings);
    findings
}

fn check_link_values_schema(
    artifact: &ArtifactEnvelope,
    schema: &LinkSchema<'_>,
    model: &Model<'_>,
    valid_triples: &[(&str, &str, &str)],
    findings: &mut Vec<Diagnostic>,
) {
    let Some(values_props) =
        member(schema.get("values"), "properties").and_then(JsonValue::as_object)
    else {
        return;
    };
    if values_props.is_empty() {
        return;
    }
    let valid_pairs: BTreeSet<(&str, &str)> = valid_triples
        .iter()
        .map(|(source, link, _)| (*source, *link))
        .collect();
    if valid_pairs.is_empty() {
        return;
    }
    for (prop_name, prop_schema) in values_props {
        let pointer = format!("{ITEMS_POINTER}/properties/values/properties/{prop_name}");
        let matching_props: Vec<&EffectiveProperty> = valid_pairs
            .iter()
            .filter_map(|(source, link)| model.effective_link(source, link))
            .filter_map(|effective| {
                effective
                    .properties
                    .iter()
                    .find(|property| property.id.as_str() == prop_name)
            })
            .collect();
        if matching_props.is_empty() {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link values property {prop_name:?} is not declared on any emitted link"
                ),
                artifact,
                &pointer,
            ));
            continue;
        }
        let writable: Vec<&EffectiveProperty> = matching_props
            .into_iter()
            .filter(|property| !property.is_computed())
            .collect();
        if writable.is_empty() {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link values property {prop_name:?} is computed and cannot be written"
                ),
                artifact,
                &pointer,
            ));
            continue;
        }
        if !writable
            .iter()
            .any(|property| link_property_schema_matches(prop_schema, property))
        {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link values property {prop_name:?} schema type does not match the declared link property type"
                ),
                artifact,
                &pointer,
            ));
        }
    }
}

fn link_property_schema_matches(prop_schema: &JsonValue, property: &EffectiveProperty) -> bool {
    let Some(type_val) = member(Some(prop_schema), "type") else {
        return false;
    };
    let (schema_type, admits_null, is_json_union) = match type_val {
        JsonValue::String(name) => (Some(name.as_str()), name == "null", false),
        JsonValue::Array(items) => {
            let names: Vec<&str> = items.iter().filter_map(JsonValue::as_string).collect();
            if names.len() != items.len() || names.is_empty() {
                return false;
            }
            let unique: BTreeSet<&str> = names.iter().copied().collect();
            let json_domain: BTreeSet<&str> =
                ["null", "boolean", "number", "string", "array", "object"]
                    .into_iter()
                    .collect();
            if unique == json_domain {
                (None, true, true)
            } else if unique.len() == 2 && unique.contains("null") {
                let non_null = unique.iter().copied().find(|name| *name != "null");
                (non_null, true, false)
            } else {
                return false;
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::Object(_) => {
            return false;
        }
    };
    if admits_null
        && !property.nullable
        && !matches!(
            &property.kind,
            EffectiveKind::Base(PropertyType::JsonExt { .. })
        )
    {
        return false;
    }
    match &property.kind {
        EffectiveKind::Base(PropertyType::JsonExt { .. }) => is_json_union || schema_type.is_some(),
        EffectiveKind::Base(
            PropertyType::Text { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Enum { .. },
        ) => schema_type == Some("string"),
        EffectiveKind::Base(PropertyType::Integer { .. }) => schema_type == Some("integer"),
        EffectiveKind::Base(PropertyType::Decimal { .. } | PropertyType::DecimalBinding { .. }) => {
            matches!(schema_type, Some("number" | "integer"))
        }
        EffectiveKind::Base(PropertyType::Boolean {}) => schema_type == Some("boolean"),
        EffectiveKind::Base(PropertyType::Record { .. }) => schema_type == Some("object"),
        EffectiveKind::Base(PropertyType::Ref { .. })
        | EffectiveKind::Registry(_)
        | EffectiveKind::Unresolved => false,
    }
}

fn valid_link_triples<'a>(
    model: &Model<'_>,
    sources: &[&'a str],
    links: &[&'a str],
    targets: &[&'a str],
) -> Vec<(&'a str, &'a str, &'a str)> {
    let mut triples = Vec::new();
    for source in sources {
        for link in links {
            let Some(declared_target) = model.link_target(source, link) else {
                continue;
            };
            for target in targets {
                if target_matches_declared(model, target, declared_target) {
                    triples.push((*source, *link, *target));
                }
            }
        }
    }
    triples
}

fn is_owned_collection_link(model: &Model<'_>, source: &str, link: &str) -> bool {
    model
        .effective_object_type_named(source)
        .is_some_and(|effective| {
            effective
                .owned_collections
                .iter()
                .any(|collection| collection.id.as_str() == link)
        })
}

fn resolve_endpoint_types<'a>(
    artifact: &ArtifactEnvelope,
    model: &Model<'_>,
    kinds: &[&'a str],
    field: &str,
    role: &str,
    findings: &mut Vec<Diagnostic>,
) -> Vec<&'a str> {
    let mut resolved = Vec::new();
    for (index, kind) in kinds.iter().enumerate() {
        let pointer = format!("{ITEMS_POINTER}/properties/{field}/enum/{index}");
        let unknown = || {
            finding(
                lifecycle_codes::UNKNOWN_FACT_OBJECT_TYPE,
                format!(
                    "the decision links {role} type {kind:?}, which is not an object type of the artifact set or the installed base"
                ),
                artifact,
                &pointer,
            )
        };
        match model.resolve(artifact, kind, &pointer, unknown) {
            Ok(_) => resolved.push(*kind),
            Err(refusal) => findings.push(*refusal),
        }
    }
    resolved
}

/// T3: check that a `synchronousRule` action's declared `links` output schema
/// only names sources covered by declared writes (outgoing links share source
/// write authority), targets written or captured in the action slice, valid
/// `(source, link, target)` triples, and never targets `norn.*` system types,
/// owned-child types, or owned-collection links.
pub(crate) fn synchronous_writer_link_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    writer: &serde_json::Value,
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    let Some(links_schema) = writer.pointer("/outputSchema/properties/links") else {
        return Vec::new();
    };
    let pointer = "/payload/synchronousRule";
    let Some(properties) = links_schema.pointer("/items/properties") else {
        return vec![finding(
            action_codes::CREATE_LINK,
            "synchronousRule links output requires a closed LinkChange item schema".to_owned(),
            artifact,
            pointer,
        )];
    };
    let extract_enum = |field: &str| -> Option<Vec<&str>> {
        let arr = properties.get(field)?.get("enum")?.as_array()?;
        let strs: Vec<&str> = arr
            .iter()
            .filter_map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
            .collect();
        (!strs.is_empty() && strs.len() == arr.len()).then_some(strs)
    };
    let (Some(ops), Some(sources), Some(links), Some(targets)) = (
        extract_enum("op"),
        extract_enum("sourceType"),
        extract_enum("link"),
        extract_enum("targetType"),
    ) else {
        return vec![finding(
            action_codes::CREATE_LINK,
            "synchronousRule links requires finite op, sourceType, link, and targetType enums"
                .to_owned(),
            artifact,
            pointer,
        )];
    };
    let mut findings = Vec::new();
    if ops.iter().any(|op| LinkChangeOp::parse(op).is_none()) {
        findings.push(finding(
            action_codes::CREATE_LINK,
            "synchronousRule links op enum may contain only add or remove".to_owned(),
            artifact,
            pointer,
        ));
    }
    check_synchronous_writer_endpoints(
        artifact,
        root,
        pointer,
        model,
        &sources,
        &targets,
        &mut findings,
    );
    check_synchronous_writer_triples(
        artifact,
        pointer,
        model,
        &sources,
        &links,
        &targets,
        &mut findings,
    );
    findings
}

fn check_synchronous_writer_endpoints(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    pointer: &str,
    model: &Model<'_>,
    sources: &[&str],
    targets: &[&str],
    findings: &mut Vec<Diagnostic>,
) {
    let primary_kind = root
        .effect_program
        .as_ref()
        .map(|program| program.target().object_kind);
    let source_written = |kind: &str| {
        primary_kind == Some(kind)
            || root.writes.iter().any(|write| {
                write.entity_kind == kind
                    && matches!(write.change, WriteChange::Created | WriteChange::Modified)
            })
    };
    let target_available = |kind: &str| {
        source_written(kind)
            || root
                .slice
                .iter()
                .any(|entry| entry.object_type.as_str() == kind)
    };
    for kind in sources {
        if kind.starts_with("norn.") || model.is_owned_child_target(kind) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "synchronousRule link source {kind:?} cannot target system or owned-child types"
                ),
                artifact,
                pointer,
            ));
        } else if !source_written(kind) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link source {kind:?} must be covered by the action's declared writes"),
                artifact,
                pointer,
            ));
        }
    }
    for kind in targets {
        if kind.starts_with("norn.") || model.is_owned_child_target(kind) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "synchronousRule link target {kind:?} cannot target system or owned-child types"
                ),
                artifact,
                pointer,
            ));
        } else if !target_available(kind) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link target {kind:?} must be written or captured in the action slice"),
                artifact,
                pointer,
            ));
        }
    }
}

fn check_synchronous_writer_triples(
    artifact: &ArtifactEnvelope,
    pointer: &str,
    model: &Model<'_>,
    sources: &[&str],
    links: &[&str],
    targets: &[&str],
    findings: &mut Vec<Diagnostic>,
) {
    let valid_triples = valid_link_triples(model, sources, links, targets);
    for link in links {
        if !valid_triples
            .iter()
            .any(|(_, candidate, _)| candidate == link)
        {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link {link:?} does not match a declared link between sourceType and targetType"),
                artifact,
                pointer,
            ));
        }
    }
    for source in sources {
        if !valid_triples
            .iter()
            .any(|(candidate, _, _)| candidate == source)
        {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link sourceType {source:?} does not match any declared link to targetType"),
                artifact,
                pointer,
            ));
        }
    }
    for target in targets {
        if !valid_triples
            .iter()
            .any(|(_, _, candidate)| candidate == target)
        {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link targetType {target:?} is not targeted by any declared link from sourceType"),
                artifact,
                pointer,
            ));
        }
    }
    for (source, link, _) in &valid_triples {
        if is_owned_collection_link(model, source, link) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link {link:?} on {source:?} is an owned-collection link and cannot be mutated directly"),
                artifact,
                pointer,
            ));
        }
    }
}

fn target_matches_declared(model: &Model<'_>, target: &str, declared_target: &str) -> bool {
    if target == declared_target {
        return true;
    }
    model
        .effective_object_type_named(target)
        .is_some_and(|effective| {
            effective
                .lineage
                .iter()
                .any(|ancestor| ancestor.as_str() == declared_target)
                || effective
                    .interfaces
                    .iter()
                    .any(|iface| iface.as_str() == declared_target)
        })
}

fn finding(code: &str, message: String, artifact: &ArtifactEnvelope, pointer: &str) -> Diagnostic {
    Diagnostic {
        witness: None,
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// A decide graph's input or output record: every `members` (plus any
/// declared `optional` members), all required, each a closed record (`lists`:
/// a list of closed records).
pub(crate) fn check_decide_record(
    schema: Option<&JsonValue>,
    path: &str,
    members: &[&str],
    optional: &[&str],
    lists: bool,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(properties) = schema
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(JsonValue::as_object)
    else {
        return;
    };
    let shape = if lists {
        "a list of closed records"
    } else {
        "a closed record"
    };
    for member in members {
        if !properties.contains_key(*member) {
            diagnostics.push(invalid_diagnostic(
                jdm_codes::INVALID_DECISION_CONTRACT,
                format!(
                    "a decide graph's {path} declares exactly {members:?}; '{member}' is missing"
                ),
                format!("{path}.properties"),
                artifact_id,
            ));
        }
    }
    for (member, member_schema) in properties {
        let member_path = format!("{path}.properties.{member}");
        if !members.contains(&member.as_str()) && !optional.contains(&member.as_str()) {
            diagnostics.push(invalid_diagnostic(
                jdm_codes::INVALID_DECISION_CONTRACT,
                format!(
                    "a decide graph's {path} declares exactly {members:?}; '{member}' is not part of the decision contract"
                ),
                member_path,
                artifact_id,
            ));
            continue;
        }
        let type_of = |value: Option<&JsonValue>| {
            value
                .and_then(JsonValue::as_object)
                .and_then(|schema| schema.get("type"))
                .and_then(JsonValue::as_string)
                .map(str::to_owned)
        };
        let record = if lists {
            (type_of(Some(member_schema)).as_deref() == Some("array"))
                .then(|| {
                    member_schema
                        .as_object()
                        .and_then(|schema| schema.get("items"))
                })
                .flatten()
        } else {
            Some(member_schema)
        };
        if type_of(record).as_deref() != Some("object") {
            diagnostics.push(invalid_diagnostic(
                jdm_codes::INVALID_DECISION_CONTRACT,
                format!("decision contract member '{member}' must be {shape}"),
                member_path,
                artifact_id,
            ));
        }
    }
    let required: BTreeSet<&str> = schema
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("required"))
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_string)
        .collect();
    if let Some(member) = members
        .iter()
        .chain(optional.iter())
        .find(|member| properties.contains_key(**member) && !required.contains(**member))
    {
        diagnostics.push(invalid_diagnostic(
            jdm_codes::INVALID_DECISION_CONTRACT,
            format!(
                "a decide graph's {path} requires every decision contract member; '{member}' is not required"
            ),
            format!("{path}.required"),
            artifact_id,
        ));
    }
}
