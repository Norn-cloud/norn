//! Admission checks for checked `links` items on `DecisionOutput` (#1586).
//!
//! A decide graph's `outputSchema` may declare an optional `links` array of
//! closed [`norn_ir::LinkChange`] records alongside `facts`, `commands`, and
//! `timers`. This module checks:
//!
//! - **T2** ([`validate_link_change_schema`]): closed record members, required
//!   fields, string `enum`s for `op`, `sourceType`, `link`, and `targetType`,
//!   string keys, and optional `values` object only when `add` is emitted.
//! - **T3** ([`check_decision_links_set`]): every `sourceType` and `targetType`
//!   resolves in scope, and every declared `link` is declared on a matching
//!   source type whose target lineage admits a declared target type.
//! - **Action writer T3** ([`synchronous_writer_link_findings`]): a
//!   `synchronousRule` action's link endpoints must be written or captured in
//!   the action contract and may not target `norn.*` system types or bypass
//!   owned-child authority.

use std::collections::BTreeMap;

use norn_ir::{
    ActionRoot, ArtifactEnvelope, ArtifactKind, DiagnosticRemediation, DiagnosticSeverity,
    JsonValue, LINK_CHANGE_MEMBERS, LINK_CHANGE_REQUIRED_MEMBERS, LinkChangeOp, WriteChange,
};

use crate::action_set::{Model, codes as action_codes};
use crate::artifact_set::scope_finding;
use crate::entity_artifacts::EntityArtifactSetContext;
use crate::envelope::invalid_diagnostic;
use crate::jdm_profile::codes as jdm_codes;
use crate::lifecycle_writes::codes as lifecycle_codes;
use crate::{Diagnostic, DiagnosticCode, Span};

const ITEMS: &str = "payload.outputSchema.properties.links.items";
const ITEMS_POINTER: &str = "/payload/outputSchema/properties/links/items";

struct LinkSchema<'s> {
    properties: &'s BTreeMap<String, JsonValue>,
    required: Vec<&'s str>,
}

impl<'s> LinkSchema<'s> {
    fn of(output_schema: &'s JsonValue) -> Option<Self> {
        let links = member(Some(output_schema), "properties")
            .and_then(|properties| member(Some(properties), "links"))?;
        if type_of(Some(links)) != Some("array") {
            return None;
        }
        let items = member(Some(links), "items")?;
        if type_of(Some(items)) != Some("object") {
            return None;
        }
        Some(Self {
            properties: member(Some(items), "properties")?.as_object()?,
            required: member(Some(items), "required")
                .and_then(JsonValue::as_array)
                .unwrap_or_default()
                .iter()
                .filter_map(JsonValue::as_string)
                .collect(),
        })
    }

    fn get(&self, name: &str) -> Option<&'s JsonValue> {
        self.properties.get(name)
    }

    fn values(&self, name: &str) -> Option<Vec<&'s str>> {
        string_enum(self.get(name))
    }
}

fn member<'s>(schema: Option<&'s JsonValue>, name: &str) -> Option<&'s JsonValue> {
    schema?.as_object()?.get(name)
}

fn type_of(schema: Option<&JsonValue>) -> Option<&str> {
    member(schema, "type")?.as_string()
}

fn string_enum(schema: Option<&JsonValue>) -> Option<Vec<&str>> {
    if type_of(schema) != Some("string") {
        return None;
    }
    let values = member(schema, "enum")?.as_array()?;
    let strings: Vec<&str> = values.iter().filter_map(JsonValue::as_string).collect();
    (!strings.is_empty() && strings.len() == values.len()).then_some(strings)
}

/// T2: check a decide graph's optional `links` item schema against the
/// canonical [`norn_ir::LinkChange`] record.
pub(crate) fn validate_link_change_schema(
    output_schema: Option<&JsonValue>,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(schema) = output_schema.and_then(LinkSchema::of) else {
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
            LinkSchema::of(&output)?;
            Some((artifact, output))
        })
        .collect();
    if decisions.is_empty() {
        return Vec::new();
    }
    let model = Model::new(artifacts, context.base, context.registry, true);
    let mut findings = Vec::new();
    for (artifact, output) in &decisions {
        if let Some(schema) = LinkSchema::of(output) {
            findings.extend(decision_links_findings(artifact, &schema, &model));
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
    for (index, link) in links.iter().enumerate() {
        let pointer = format!("{ITEMS_POINTER}/properties/link/enum/{index}");
        let declared_matches: Vec<(&str, &str)> = resolved_sources
            .iter()
            .filter_map(|source| {
                model
                    .link_target(source, link)
                    .map(|declared_target| (*source, declared_target))
            })
            .collect();
        if declared_matches.is_empty() {
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
        let any_target_matches = declared_matches.iter().any(|(_, declared_target)| {
            resolved_targets
                .iter()
                .any(|target| target_matches_declared(model, target, declared_target))
        });
        if !any_target_matches {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!(
                    "decision link {link:?} targets none of declared targetType {resolved_targets:?}"
                ),
                artifact,
                &pointer,
            ));
        }
        if resolved_sources.len() == 1
            && links.len() == 1
            && let Some((_, declared_target)) = declared_matches.first()
        {
            for (t_index, target) in resolved_targets.iter().enumerate() {
                if !target_matches_declared(model, target, declared_target) {
                    findings.push(finding(
                        action_codes::CREATE_LINK,
                        format!(
                            "decision link {link:?} targets {declared_target:?}, not {target:?}"
                        ),
                        artifact,
                        &format!("{ITEMS_POINTER}/properties/targetType/enum/{t_index}"),
                    ));
                }
            }
        }
    }
    findings
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
        match model.object_type(kind) {
            None => findings.push(finding(
                lifecycle_codes::UNKNOWN_FACT_OBJECT_TYPE,
                format!(
                    "the decision links {role} type {kind:?}, which is not an object type of the artifact set or the installed base"
                ),
                artifact,
                &pointer,
            )),
            Some(defined) => {
                findings.extend(scope_finding(artifact, defined.artifact, &pointer));
                resolved.push(*kind);
            }
        }
    }
    resolved
}

/// T3: check that a `synchronousRule` action's declared `links` output schema
/// only names endpoints that are written or captured in the action contract
/// and never targets `norn.*` system types or owned-child types.
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
    let primary_kind = root
        .effect_program
        .as_ref()
        .map(|program| program.target().object_kind);
    let endpoint_available = |kind: &str| {
        primary_kind == Some(kind)
            || root.writes.iter().any(|write| {
                write.entity_kind == kind
                    && matches!(write.change, WriteChange::Created | WriteChange::Modified)
            })
            || root
                .slice
                .iter()
                .any(|entry| entry.object_type.as_str() == kind)
    };
    for kind in sources.iter().chain(targets.iter()) {
        if kind.starts_with("norn.") || model.is_owned_child_target(kind) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link endpoint {kind:?} cannot target system or owned-child types"),
                artifact,
                pointer,
            ));
        } else if !endpoint_available(kind) {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link endpoint {kind:?} must be written or captured in the action slice"),
                artifact,
                pointer,
            ));
        }
    }
    for link in &links {
        let valid = sources.iter().any(|source| {
            model.link_target(source, link).is_some_and(|declared| {
                targets
                    .iter()
                    .any(|target| target_matches_declared(model, target, declared))
            })
        });
        if !valid {
            findings.push(finding(
                action_codes::CREATE_LINK,
                format!("synchronousRule link {link:?} does not match a declared link between sourceType and targetType"),
                artifact,
                pointer,
            ));
        }
    }
    findings
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
