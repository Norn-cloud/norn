//! Structural required core-link binding checks for `create_only` actions
//! targeting `core.*`-specializing concepts.
//!
//! Both `createLinks` bindings and `synchronousRule` outputs fail closed:
//! - a `createLinks` entry counts only when its source is the implicit created
//!   object (`source: None`) or an explicit `ActionLinkSource` matching BOTH
//!   `object_kind` and `object_id_field`;
//! - a `synchronousRule` counts for a specific `(object_kind, object_id_field,
//!   link_id)` only when its `outputSchema` requires `links` admitting `op:
//!   "add"`, `sourceType: object_kind`, and `link: link_id`, AND its JDM graph
//!   is an unconditional straight-line pipeline whose final `links` array
//!   literal statically emits `{ op: "add", sourceType: <object_kind>,
//!   sourceKey: event.input.<object_id_field>, link: <link_id>, ... }`.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ActionCreateLink, ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind,
    InterfaceDef, InterfaceName, JdmNode, JsonValue, UpsertWriteMode,
};

use super::{codes, decode_action, finding, interface_lineage, is_core_name};
use crate::entity_scopes::ScopedViews;
use crate::{Diagnostic, DiagnosticWitness, EffectiveObjectType};

/// Verify that every candidate `create_only` action targeting a concept
/// specializing a `core.*` interface explicitly binds every `required: true`
/// link declared on those `core.*` interfaces.
pub(super) fn check_required_core_link_bindings(
    artifacts: &[ArtifactEnvelope],
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    for artifact in artifacts {
        let Some(root) = decode_action(artifact) else {
            continue;
        };
        let Some(ActionEffectProgram::UpsertInputObject {
            object_id_field,
            object_kind,
            write_mode: UpsertWriteMode::CreateOnly,
            ..
        }) = &root.effect_program
        else {
            continue;
        };
        let view = views.of(artifact);
        let Some(Ok(effective)) = view.catalog().object_type_named(object_kind) else {
            continue;
        };
        let required_core_links = required_core_interface_links(effective, interfaces);
        if required_core_links.is_empty() {
            continue;
        }
        let bound_links = bound_create_links(&root, view.pool(), object_kind, object_id_field);
        for (link_id, (iface_name, target_name)) in required_core_links {
            if bound_links.contains(link_id.as_str()) {
                continue;
            }
            let expected = format!(
                "createLinks binding (or guaranteed synchronousRule add) for required link {link_id} -> {target_name} sourced from created {object_kind} (idField={object_id_field})"
            );
            let actual = format!("unbound in create_only action {}", root.action_id);
            findings.push(finding(
                codes::MISSING_REQUIRED_LINK_BINDING,
                artifact,
                "/payload/createLinks",
                format!(
                    "create_only action {:?} creates {:?} (implementing {:?}) without binding required core link {:?} -> {:?}",
                    root.action_id, object_kind, iface_name, link_id, target_name
                ),
                Some(DiagnosticWitness::core_link(
                    object_kind.as_str(),
                    iface_name,
                    link_id,
                    expected,
                    actual,
                )),
            ));
        }
    }
}

/// Returns whether a `createLinks` entry binds the newly created object of a
/// `create_only` upsert action (implicit source, or explicit source matching
/// both `object_kind` and `object_id_field`).
fn create_link_binds_created_object(
    binding: &ActionCreateLink,
    object_kind: &str,
    object_id_field: &str,
) -> bool {
    binding
        .source
        .as_ref()
        .is_none_or(|src| src.object_type == object_kind && src.id_field == object_id_field)
}

/// Collect the exact link IDs statically guaranteed to be bound on the newly
/// created `(object_kind, object_id_field)` object by either `createLinks` or
/// an unconditional `synchronousRule` `links` output.
pub(super) fn bound_create_links(
    root: &ActionRoot,
    pool: &[ArtifactEnvelope],
    object_kind: &str,
    object_id_field: &str,
) -> BTreeSet<String> {
    let mut bound: BTreeSet<String> = root
        .create_links
        .iter()
        .filter(|binding| create_link_binds_created_object(binding, object_kind, object_id_field))
        .map(|binding| binding.link.clone())
        .collect();
    if let Some(rule_ref) = &root.synchronous_rule {
        bound.extend(synchronous_rule_bound_links(
            pool,
            &rule_ref.artifact_id,
            &rule_ref.version,
            object_kind,
            object_id_field,
        ));
    }
    bound
}

fn required_core_interface_links(
    effective: &EffectiveObjectType,
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
) -> BTreeMap<String, (String, String)> {
    let mut required = BTreeMap::new();
    for iface_name in &effective.interfaces {
        if !is_core_name(iface_name.as_str()) {
            continue;
        }
        for def in interface_lineage(iface_name, interfaces) {
            if !is_core_name(def.name.as_str()) {
                continue;
            }
            for link in &def.links {
                if link.cardinality.required {
                    required
                        .entry(link.id.as_str().to_owned())
                        .or_insert_with(|| {
                            (
                                def.name.as_str().to_owned(),
                                link.target.as_str().to_owned(),
                            )
                        });
                }
            }
        }
    }
    required
}

/// Return the link IDs that `synchronousRule` `(artifact_id, version)` is
/// statically guaranteed to emit as `op: "add"` for the newly created
/// `(object_kind, object_id_field)`. Fails closed (returns empty) whenever the
/// output schema does not require `links`, the graph branches/has non-linear
/// nodes, or the `links` expression is not a top-level array of literal link
/// objects bound to `event.input.<object_id_field>`.
fn synchronous_rule_bound_links(
    pool: &[ArtifactEnvelope],
    artifact_id: &str,
    version: &str,
    object_kind: &str,
    object_id_field: &str,
) -> BTreeSet<String> {
    let Some(rule) = pool.iter().find(|artifact| {
        artifact.kind() == ArtifactKind::Rule
            && artifact.artifact_id == artifact_id
            && artifact.version == version
    }) else {
        return BTreeSet::new();
    };
    let Some(admitted_links) = schema_admitted_add_links(rule, object_kind) else {
        return BTreeSet::new();
    };
    let Some(links_expr) = unconditional_links_expression(rule) else {
        return BTreeSet::new();
    };
    let expected_source_key = format!("event.input.{object_id_field}");
    extract_guaranteed_add_links(
        &links_expr,
        object_kind,
        &expected_source_key,
        &admitted_links,
    )
}

fn schema_admitted_add_links(
    rule: &ArtifactEnvelope,
    object_kind: &str,
) -> Option<BTreeSet<String>> {
    let payload = rule.payload.to_json_value();
    let output_schema = payload.as_object()?.get("outputSchema")?.as_object()?;
    let required = output_schema.get("required")?.as_array()?;
    if !required
        .iter()
        .any(|entry| entry.as_string() == Some("links"))
    {
        return None;
    }
    let items_props = output_schema
        .get("properties")?
        .as_object()?
        .get("links")?
        .as_object()?
        .get("items")?
        .as_object()?
        .get("properties")?
        .as_object()?;
    if !enum_contains(items_props.get("op")?, "add") {
        return None;
    }
    if !enum_contains(items_props.get("sourceType")?, object_kind) {
        return None;
    }
    let link_enum = items_props
        .get("link")?
        .as_object()?
        .get("enum")?
        .as_array()?;
    let links: BTreeSet<String> = link_enum
        .iter()
        .filter_map(JsonValue::as_string)
        .map(str::to_owned)
        .collect();
    (!links.is_empty()).then_some(links)
}

fn enum_contains(schema: &JsonValue, expected: &str) -> bool {
    schema
        .as_object()
        .and_then(|obj| obj.get("enum"))
        .and_then(JsonValue::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_string() == Some(expected)))
}

/// Extract the final expression writing `key == "links"` when the JDM graph is
/// an unconditional straight-line chain `InputNode -> ExpressionNode* -> OutputNode`.
fn unconditional_links_expression(rule: &ArtifactEnvelope) -> Option<String> {
    let graph = crate::jdm_profile::decode_graph(rule)?;
    let mut nodes: BTreeMap<&str, &JdmNode> = BTreeMap::new();
    let mut input_id: Option<&str> = None;
    let mut output_id: Option<&str> = None;

    for node in &graph.nodes {
        match node {
            JdmNode::InputNode { id, .. } => {
                if input_id.replace(id.as_str()).is_some() {
                    return None;
                }
                nodes.insert(id.as_str(), node);
            }
            JdmNode::OutputNode { id, .. } => {
                if output_id.replace(id.as_str()).is_some() {
                    return None;
                }
                nodes.insert(id.as_str(), node);
            }
            JdmNode::ExpressionNode { id, .. } => {
                nodes.insert(id.as_str(), node);
            }
            JdmNode::DecisionTableNode { .. }
            | JdmNode::SwitchNode { .. }
            | JdmNode::DecisionNode { .. }
            | JdmNode::FunctionNode { .. }
            | JdmNode::CustomNode { .. } => return None,
        }
    }
    let start = input_id?;
    let end = output_id?;

    let mut next_by_source: BTreeMap<&str, &str> = BTreeMap::new();
    for edge in &graph.edges {
        if next_by_source
            .insert(edge.source_id.as_str(), edge.target_id.as_str())
            .is_some()
        {
            return None;
        }
    }

    let mut current = start;
    let mut seen = BTreeSet::new();
    let mut last_links_expr: Option<String> = None;
    loop {
        if !seen.insert(current) {
            return None;
        }
        if let Some(JdmNode::ExpressionNode { content, .. }) = nodes.get(current) {
            for expr in &content.expressions {
                if expr.key == "links" {
                    last_links_expr = Some(expr.value.clone());
                }
            }
        }
        if current == end {
            return last_links_expr;
        }
        current = *next_by_source.get(current)?;
    }
}

fn extract_guaranteed_add_links(
    source: &str,
    object_kind: &str,
    expected_source_key: &str,
    admitted_links: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut bound = BTreeSet::new();
    let Some(inner) = source
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return bound;
    };
    for element in split_top_level(inner, ',') {
        let Some(fields) = parse_object_literal_fields(element.trim()) else {
            continue;
        };
        let op_ok = fields
            .get("op")
            .and_then(|raw| parse_string_literal(raw))
            .is_some_and(|op| op == "add");
        let source_type_ok = fields
            .get("sourceType")
            .and_then(|raw| parse_string_literal(raw))
            .is_some_and(|st| st == object_kind);
        let source_key_ok = fields
            .get("sourceKey")
            .is_some_and(|raw| normalize_expr_path(raw) == expected_source_key);
        let link_id = fields
            .get("link")
            .and_then(|raw| parse_string_literal(raw))
            .filter(|id| admitted_links.contains(id));
        if op_ok
            && source_type_ok
            && source_key_ok
            && let Some(link_id) = link_id
        {
            bound.insert(link_id);
        }
    }
    bound
}

fn parse_object_literal_fields(raw: &str) -> Option<BTreeMap<String, String>> {
    let body = raw.strip_prefix('{')?.strip_suffix('}')?;
    let mut fields = BTreeMap::new();
    for entry in split_top_level(body, ',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (key_raw, value_raw) = split_once_top_level(trimmed, ':')?;
        let key_trimmed = key_raw.trim();
        let key = parse_string_literal(key_trimmed).unwrap_or_else(|| key_trimmed.to_owned());
        if key.is_empty() {
            return None;
        }
        fields.insert(key, value_raw.trim().to_owned());
    }
    Some(fields)
}

fn parse_string_literal(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    for quote in ['\'', '"'] {
        if let Some(inner) = trimmed
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
            && !inner.contains(quote)
        {
            return Some(inner.to_owned());
        }
    }
    None
}

fn normalize_expr_path(raw: &str) -> String {
    raw.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut depth: usize = 0;

    for ch in input.chars() {
        if let Some(active) = quote {
            current.push(ch);
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' | '[' | '{' => {
                depth = depth.saturating_add(1);
                current.push(ch);
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            c if c == delimiter && depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

fn split_once_top_level(input: &str, delimiter: char) -> Option<(String, String)> {
    let mut left = String::new();
    let mut right = String::new();
    let mut found = false;
    let mut quote: Option<char> = None;
    let mut depth: usize = 0;

    for ch in input.chars() {
        if found {
            right.push(ch);
            continue;
        }
        if let Some(active) = quote {
            left.push(ch);
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => {
                quote = Some(ch);
                left.push(ch);
            }
            '(' | '[' | '{' => {
                depth = depth.saturating_add(1);
                left.push(ch);
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                left.push(ch);
            }
            c if c == delimiter && depth == 0 => {
                found = true;
            }
            _ => left.push(ch),
        }
    }
    found.then_some((left, right))
}
