//! Cross-artifact checks for classification marking declarations and references.
//!
//! Effective markings are unions of string IDs in the entity model and do not
//! need mask-policy resolution. This module validates those IDs against the
//! admitted base and candidate marking catalogs, plus `AccessIR` reveal clauses.

use std::collections::BTreeSet;

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, DiagnosticSeverity, EntityRoot, MarkingId, MarkingIr,
};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, Span};

/// Stable cross-artifact marking diagnostics.
pub(crate) mod codes {
    /// A candidate marking id is declared more than once or shadows the base.
    pub(crate) const DUPLICATE: &str = "artifact_set.marking.duplicate";
    /// A marking reference has no declaration in the base or candidate set.
    pub(crate) const UNKNOWN: &str = "artifact_set.marking.unknown";
}

/// Check candidate marking declarations and every string reference against
/// the installed base and candidate catalogs. Malformed catalog roots are
/// handled by the ordinary typed-root checks.
pub(crate) fn check_references(
    candidates: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let mut known = BTreeSet::new();
    for artifact in base {
        for (id, _position) in catalog_ids(artifact) {
            known.insert(id);
        }
    }

    let mut out = Vec::new();
    for artifact in candidates {
        for (id, position) in catalog_ids(artifact) {
            if !known.insert(id.clone()) {
                out.push(finding(
                    codes::DUPLICATE,
                    format!("marking id {id:?} is already declared in the base or candidate set"),
                    artifact,
                    &[
                        "payload".to_owned(),
                        "markings".to_owned(),
                        position.to_string(),
                        "id".to_owned(),
                    ],
                ));
            }
        }
    }

    for artifact in candidates {
        let mut seen = BTreeSet::new();
        for reference in collect_references(artifact) {
            let parent = reference
                .path
                .get(..reference.path.len().saturating_sub(1))
                .unwrap_or_default();
            if !seen.insert((parent.to_vec(), reference.id.clone())) {
                out.push(finding(
                    codes::DUPLICATE,
                    format!(
                        "marking reference {:?} is listed more than once",
                        reference.id
                    ),
                    artifact,
                    &reference.path,
                ));
            }
            if !known.contains(&reference.id) {
                out.push(finding(
                    codes::UNKNOWN,
                    format!(
                        "marking reference {:?} has no declaration in the base or candidate set",
                        reference.id
                    ),
                    artifact,
                    &reference.path,
                ));
            }
        }
    }
    out
}

/// Build the complete marking policy catalog from already-admitted entity
/// artifacts. The returned map is keyed by the typed marking id; malformed
/// roots are ignored because admission must reject them before read
/// projection.
#[must_use]
pub fn marking_catalog_from_artifacts(
    artifacts: &[ArtifactEnvelope],
) -> std::collections::BTreeMap<MarkingId, MarkingIr> {
    let mut catalog = std::collections::BTreeMap::new();
    for artifact in artifacts {
        let Some(EntityRoot::V2Markings(root)) = entity_root(artifact) else {
            continue;
        };
        for marking in root.markings {
            catalog.entry(marking.id.clone()).or_insert(marking);
        }
    }
    catalog
}

fn catalog_ids(artifact: &ArtifactEnvelope) -> Vec<(String, usize)> {
    let Some(EntityRoot::V2Markings(root)) = entity_root(artifact) else {
        return Vec::new();
    };
    root.markings
        .iter()
        .enumerate()
        .map(|(position, marking)| (marking.id.as_str().to_owned(), position))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct MarkingReference {
    id: String,
    path: Vec<String>,
}

fn entity_root(artifact: &ArtifactEnvelope) -> Option<EntityRoot> {
    let ArtifactPayload::Entity(payload) = &artifact.payload else {
        return None;
    };
    let value = serde_json::to_value(payload.to_json_value()).ok()?;
    serde_json::from_value(value).ok()
}

/// Visit only typed marking reference locations. Extension slots carry opaque
/// JSON, so recursively treating every member named `markings` as a reference
/// would reject otherwise valid extension data.
fn collect_references(artifact: &ArtifactEnvelope) -> Vec<MarkingReference> {
    let mut references = Vec::new();
    match &artifact.payload {
        ArtifactPayload::Entity(_) => {
            if let Some(root) = entity_root(artifact) {
                collect_entity_references(&root, &mut references);
            }
        }
        ArtifactPayload::SourceSchema(payload) => {
            if let Some(fields) = payload.fields.as_value()
                && let Ok(fields) = serde_json::to_value(fields)
                    .and_then(serde_json::from_value::<Vec<norn_ir::source_schema::SourceField>>)
            {
                for (index, field) in fields.iter().enumerate() {
                    let mut path = vec![
                        "payload".to_owned(),
                        "fields".to_owned(),
                        index.to_string(),
                        "markings".to_owned(),
                    ];
                    collect_markings(&field.markings, &mut path, &mut references);
                }
            }
        }
        ArtifactPayload::Policy(payload) if !payload.access_version.is_absent() => {
            if let Some(rules) = payload.rules.as_value()
                && let Ok(rules) = serde_json::to_value(rules)
                    .and_then(serde_json::from_value::<Vec<norn_ir::access::AccessRule>>)
            {
                for (rule_index, rule) in rules.iter().enumerate() {
                    for (condition_index, condition) in rule.all_of.iter().enumerate() {
                        let norn_ir::access::AccessCondition::Marking(condition) = condition else {
                            continue;
                        };
                        let mut path = vec![
                            "payload".to_owned(),
                            "rules".to_owned(),
                            rule_index.to_string(),
                            "allOf".to_owned(),
                            condition_index.to_string(),
                            "marking".to_owned(),
                            "anyOf".to_owned(),
                        ];
                        collect_markings(&condition.any_of, &mut path, &mut references);
                    }
                }
            }
        }
        ArtifactPayload::Relation(_)
        | ArtifactPayload::Action(_)
        | ArtifactPayload::Rule(_)
        | ArtifactPayload::Connector(_)
        | ArtifactPayload::Widget(_)
        | ArtifactPayload::Customization(_)
        | ArtifactPayload::Setting(_)
        | ArtifactPayload::BusinessPack(_)
        | ArtifactPayload::Policy(_)
        | ArtifactPayload::Derived(_)
        | ArtifactPayload::Mapping(_)
        | ArtifactPayload::Law(_)
        | ArtifactPayload::Claim(_)
        | ArtifactPayload::Ruling(_) => {}
    }
    references
}

fn collect_entity_references(root: &EntityRoot, references: &mut Vec<MarkingReference>) {
    match root {
        EntityRoot::V1(root) => {
            collect_markings(
                &root.markings,
                &mut vec!["payload".to_owned(), "markings".to_owned()],
                references,
            );
            for (name, field) in &root.fields {
                collect_markings(
                    field.markings(),
                    &mut vec![
                        "payload".to_owned(),
                        "fields".to_owned(),
                        name.clone(),
                        "markings".to_owned(),
                    ],
                    references,
                );
            }
        }
        EntityRoot::V2(root) => {
            let mut path = vec![
                "payload".to_owned(),
                "objectType".to_owned(),
                "markings".to_owned(),
            ];
            collect_markings(&root.object_type.markings, &mut path, references);
            path.pop();
            for (index, property) in root.object_type.properties.iter().enumerate() {
                path.push("properties".to_owned());
                path.push(index.to_string());
                path.push("markings".to_owned());
                collect_markings(&property.markings, &mut path, references);
                path.truncate(2);
            }
            path.truncate(2);
            collect_link_property_references(&root.object_type.links, &mut path, references);
        }
        EntityRoot::V2Interface(root) => {
            let mut path = vec![
                "payload".to_owned(),
                "interface".to_owned(),
                "markings".to_owned(),
            ];
            collect_markings(&root.interface.markings, &mut path, references);
            path.pop();
            for (index, property) in root.interface.properties.iter().enumerate() {
                path.push("properties".to_owned());
                path.push(index.to_string());
                path.push("markings".to_owned());
                collect_markings(&property.markings, &mut path, references);
                path.truncate(2);
            }
        }
        EntityRoot::V2Extension(root) => {
            for (index, property) in root.object_type_extension.properties.iter().enumerate() {
                collect_markings(
                    &property.markings,
                    &mut vec![
                        "payload".to_owned(),
                        "objectTypeExtension".to_owned(),
                        "properties".to_owned(),
                        index.to_string(),
                        "markings".to_owned(),
                    ],
                    references,
                );
            }
            collect_link_property_references(
                &root.object_type_extension.links,
                &mut vec!["payload".to_owned(), "objectTypeExtension".to_owned()],
                references,
            );
        }
        EntityRoot::V2PropertyTypes(root) => {
            for (index, property_type) in root.property_types.iter().enumerate() {
                collect_markings(
                    &property_type.markings,
                    &mut vec![
                        "payload".to_owned(),
                        "propertyTypes".to_owned(),
                        index.to_string(),
                        "markings".to_owned(),
                    ],
                    references,
                );
            }
        }
        EntityRoot::V2Markings(_) => {}
    }
}

fn collect_link_property_references(
    links: &[norn_ir::entity_v2::link::Link],
    path: &mut Vec<String>,
    references: &mut Vec<MarkingReference>,
) {
    path.push("links".to_owned());
    for (link_index, link) in links.iter().enumerate() {
        path.push(link_index.to_string());
        path.push("properties".to_owned());
        for (property_index, property) in link.properties.iter().enumerate() {
            path.push(property_index.to_string());
            path.push("markings".to_owned());
            collect_markings(&property.markings, path, references);
            path.pop();
            path.pop();
        }
        path.pop();
        path.pop();
    }
    path.pop();
}

fn collect_markings(
    markings: &[MarkingId],
    path: &mut Vec<String>,
    references: &mut Vec<MarkingReference>,
) {
    for (index, marking) in markings.iter().enumerate() {
        path.push(index.to_string());
        references.push(MarkingReference {
            id: marking.as_str().to_owned(),
            path: path.clone(),
        });
        path.pop();
    }
}

fn finding(
    code: &str,
    message: String,
    artifact: &ArtifactEnvelope,
    path: &[String],
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::from_segments(path)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::{check_references, codes, marking_catalog_from_artifacts};
    use norn_ir::{ArtifactEnvelope, MarkingId, MaskPolicy};

    fn artifact(artifact_id: &str, kind: &str, payload: &serde_json::Value) -> ArtifactEnvelope {
        serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": artifact_id,
            "kind": kind,
            "namespace": "test",
            "name": artifact_id,
            "version": "0.1.0",
            "lifecycle": "active",
            "scope": {},
            "dependencies": [],
            "provenance": {"source": "system"},
            "payload": payload,
        }))
        .expect("artifact decodes")
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "cover each typed marking reference location with one expectation"
    )]
    fn collects_entity_source_and_access_marking_references() {
        let artifacts = [
            artifact(
                "test.object",
                "entity",
                &json!({
                    "entityVersion": "2.0",
                    "objectType": {
                        "name": "test.Record",
                        "markings": ["typed.object"],
                        "properties": [{
                            "id": "id",
                            "type": {"kind": "text"},
                            "markings": ["typed.property"]
                        }],
                        "links": [{
                            "id": "aliases",
                            "target": "test.Record",
                            "cardinality": {"to": "many", "from": "many"},
                            "properties": [{
                                "id": "source",
                                "type": {"kind": "text"},
                                "markings": ["typed.link_property"]
                            }]
                        }]
                    }
                }),
            ),
            artifact(
                "test.interface",
                "entity",
                &json!({
                    "entityVersion": "2.0",
                    "interface": {
                        "name": "test.RecordView",
                        "markings": ["typed.interface"],
                        "properties": [{
                            "id": "email",
                            "type": {"kind": "text"},
                            "markings": ["typed.interface_property"]
                        }]
                    }
                }),
            ),
            artifact(
                "test.extension",
                "entity",
                &json!({
                    "entityVersion": "2.0",
                    "objectTypeExtension": {
                        "namespace": "acme",
                        "target": "test.Record",
                        "properties": [{
                            "id": "acme__email",
                            "type": {"kind": "text"},
                            "markings": ["typed.extension_property"]
                        }],
                        "links": [{
                            "id": "acme__aliases",
                            "target": "test.Record",
                            "cardinality": {"to": "many", "from": "many"},
                            "properties": [{
                                "id": "reason",
                                "type": {"kind": "text"},
                                "markings": ["typed.extension_link_property"]
                            }]
                        }]
                    }
                }),
            ),
            artifact(
                "test.property-types",
                "entity",
                &json!({
                    "entityVersion": "2.0",
                    "propertyTypes": [{
                        "name": "test.email",
                        "type": {"kind": "text"},
                        "markings": ["typed.shared_type"]
                    }]
                }),
            ),
            artifact(
                "test.legacy",
                "entity",
                &json!({
                    "objectKind": "test.Legacy",
                    "markings": ["typed.legacy_object"],
                    "fields": {
                        "email": {"type": "text", "markings": ["typed.legacy_field"]}
                    }
                }),
            ),
            artifact(
                "test.source-schema",
                "source_schema",
                &json!({
                    "fields": [{
                        "dataset": "customers",
                        "name": "email",
                        "ordinal": 0,
                        "nativeType": "varchar",
                        "typeCandidate": {"type": "string", "origin": "declared"},
                        "markings": ["typed.source_field"]
                    }]
                }),
            ),
            artifact(
                "test.policy",
                "policy",
                &json!({
                    "policyId": "test.reveal",
                    "accessVersion": "0.1",
                    "rules": [{
                        "ruleId": "reveal",
                        "actions": ["norn.reveal"],
                        "resourceType": "norn.Marking",
                        "scope": {"dimensions": ["org"]},
                        "allOf": [{"marking": {"anyOf": ["typed.access"]}}]
                    }]
                }),
            ),
        ];
        let mut ids: Vec<_> = artifacts
            .iter()
            .flat_map(super::collect_references)
            .map(|reference| reference.id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "typed.access",
                "typed.extension_link_property",
                "typed.extension_property",
                "typed.interface",
                "typed.interface_property",
                "typed.legacy_field",
                "typed.legacy_object",
                "typed.link_property",
                "typed.object",
                "typed.property",
                "typed.shared_type",
                "typed.source_field",
            ]
        );
    }

    #[test]
    fn arbitrary_extension_members_named_markings_are_not_references() {
        let artifact = artifact(
            "test.person",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "test.Person",
                    "properties": [{"id": "id", "type": {"kind": "text"}}],
                    "identity": [{"name": "primary", "properties": ["id"]}]
                },
                "extensions": {
                    "acme/example@1": {
                        "markings": ["draft"],
                        "marking": {"anyOf": ["draft"]}
                    }
                }
            }),
        );
        let registry = BTreeSet::new();
        let findings = crate::check_entity_artifacts(
            &[artifact],
            &crate::EntityArtifactSetContext {
                registry: &registry,
                base: &[],
                tenant: None,
            },
        );
        assert!(
            findings.is_empty(),
            "valid extension was rejected: {findings:?}"
        );
    }

    #[test]
    fn extracts_typed_marking_policies_from_entity_artifacts() {
        let artifact: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "core.markings.entity",
            "kind": "entity",
            "namespace": "core",
            "name": "Markings",
            "version": "0.1.0",
            "lifecycle": "active",
            "scope": {},
            "dependencies": [],
            "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "markings": [{
                    "id": "pii.financial",
                    "label": "Financial",
                    "severity": "critical",
                    "category": "pii.financial",
                    "mask": {"kind": "last-n", "count": 4}
                }]
            }
        }))
        .expect("marking artifact decodes");
        let catalog = marking_catalog_from_artifacts(&[artifact]);
        assert_eq!(catalog.len(), 1);
        let marking = catalog
            .get(&MarkingId("pii.financial".to_owned()))
            .expect("catalog contains the typed marking id");
        assert_eq!(marking.mask, MaskPolicy::LastN { count: 4 });
    }

    #[test]
    fn unknown_references_fail_with_payload_relative_locations() {
        let artifact = artifact(
            "test.person",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "test.Person",
                    "properties": [{
                        "id": "email",
                        "type": {"kind": "text"},
                        "markings": ["pii.contact"]
                    }]
                }
            }),
        );
        let findings = check_references(&[artifact], &[]);
        let unknown = findings
            .iter()
            .find(|finding| finding.code.as_str() == codes::UNKNOWN)
            .expect("undeclared marking should be reported");
        assert_eq!(
            unknown.span.as_ref().map(|span| span.pointer.as_str()),
            Some("/payload/objectType/properties/0/markings/0")
        );
    }

    #[test]
    fn link_property_references_report_exact_paths_and_duplicates() {
        let candidate = artifact(
            "test.links",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "test.Person",
                    "links": [{
                        "id": "aliases",
                        "target": "test.Person",
                        "cardinality": {"to": "many", "from": "many"},
                        "properties": [{
                            "id": "source",
                            "type": {"kind": "text"},
                            "markings": ["pii.contact", "pii.contact"]
                        }]
                    }]
                }
            }),
        );
        let extension = artifact(
            "test.link-extension",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "objectTypeExtension": {
                    "namespace": "acme",
                    "target": "test.Person",
                    "links": [{
                        "id": "acme__aliases",
                        "target": "test.Person",
                        "cardinality": {"to": "many", "from": "many"},
                        "properties": [{
                            "id": "reason",
                            "type": {"kind": "text"},
                            "markings": ["pii.unknown"]
                        }]
                    }]
                }
            }),
        );
        let catalog = artifact(
            "test.markings",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "markings": [{
                    "id": "pii.contact",
                    "label": "Contact",
                    "severity": "high",
                    "category": "pii.contact",
                    "mask": {"kind": "redacted"}
                }]
            }),
        );
        let findings = check_references(&[candidate, extension], &[catalog]);

        assert!(findings.iter().any(|finding| {
            finding.code.as_str() == codes::DUPLICATE
                && finding.span.as_ref().is_some_and(|span| {
                    span.pointer == "/payload/objectType/links/0/properties/0/markings/1"
                })
        }));
        assert!(findings.iter().any(|finding| {
            finding.code.as_str() == codes::UNKNOWN
                && finding.span.as_ref().is_some_and(|span| {
                    span.pointer == "/payload/objectTypeExtension/links/0/properties/0/markings/0"
                })
        }));
    }

    #[test]
    fn duplicate_declared_and_referenced_markings_remain_invalid() {
        let duplicate_catalog = artifact(
            "test.markings",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "markings": [
                    {"id": "pii.contact", "label": "Contact", "severity": "high", "category": "pii.contact", "mask": {"kind": "redacted"}},
                    {"id": "pii.contact", "label": "Contact duplicate", "severity": "high", "category": "pii.contact", "mask": {"kind": "redacted"}}
                ]
            }),
        );
        let duplicate_reference = artifact(
            "test.person",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "test.Person",
                    "properties": [{
                        "id": "email",
                        "type": {"kind": "text"},
                        "markings": ["pii.contact", "pii.contact"]
                    }]
                }
            }),
        );
        let declarations = check_references(&[duplicate_catalog], &[]);
        assert!(declarations.iter().any(|finding| {
            finding.code.as_str() == codes::DUPLICATE
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/markings/1/id")
        }));

        let catalog = artifact(
            "test.markings",
            "entity",
            &json!({
                "entityVersion": "2.0",
                "markings": [{
                    "id": "pii.contact",
                    "label": "Contact",
                    "severity": "high",
                    "category": "pii.contact",
                    "mask": {"kind": "redacted"}
                }]
            }),
        );
        let references = check_references(&[duplicate_reference], &[catalog]);
        assert!(references.iter().any(|finding| {
            finding.code.as_str() == codes::DUPLICATE
                && finding.span.as_ref().is_some_and(|span| {
                    span.pointer == "/payload/objectType/properties/0/markings/1"
                })
        }));
        assert!(
            references
                .iter()
                .all(|finding| finding.code.as_str() != codes::UNKNOWN)
        );
    }

    #[test]
    fn naming_series_identity_inherits_effective_scope_markings() {
        use crate::{EntityContext, EntityModel, effective_object_types};
        use norn_ir::{InterfaceDef, PropertyTypeDef};

        let object_types = naming_series_object_types();
        let property_types: Vec<PropertyTypeDef> = serde_json::from_value(json!([
            {
                "name": "marking.ScopeText",
                "type": {"kind": "text"},
                "markings": ["pii.shared_scope"]
            }
        ]))
        .expect("shared property types decode");
        let interfaces: Vec<InterfaceDef> = serde_json::from_value(json!([
            {
                "name": "marking.Scoped",
                "markings": ["pii.interface_scope"],
                "properties": [{
                    "id": "tenant",
                    "type": {"kind": "text"},
                    "markings": ["pii.interface_property"]
                }]
            }
        ]))
        .expect("interfaces decode");
        let registry = BTreeSet::new();
        let model = EntityModel {
            interfaces: &interfaces,
            object_types: &object_types,
            extensions: &[],
        };
        let context = EntityContext {
            catalog: &property_types,
            registry: &registry,
            base: EntityModel::EMPTY,
            tenant: None,
        };
        let effective = effective_object_types(&model, &context);

        let base = effective
            .get(&0)
            .expect("base invoice has an effective view")
            .0
            .property("number")
            .expect("generated identity property exists");
        let base_markings: BTreeSet<_> = base.markings.iter().map(MarkingId::as_str).collect();
        assert_eq!(
            base_markings,
            BTreeSet::from([
                "pii.invoice",
                "pii.invoice_scope",
                "pii.interface_property",
                "pii.interface_scope",
                "pii.shared_scope",
            ])
        );
        assert!(!base.computed, "a generated key stays a stored property");
        assert!(base.computed_inputs.is_empty(), "no fake computed metadata");

        let child = effective
            .get(&1)
            .expect("special invoice has an effective view")
            .0
            .property("number")
            .expect("inherited generated identity property exists");
        let child_markings: BTreeSet<_> = child.markings.iter().map(MarkingId::as_str).collect();
        assert!(child_markings.contains("pii.special_invoice"));
        assert!(child_markings.contains("pii.special_scope"));

        let public = effective
            .get(&2)
            .expect("public invoice has an effective view")
            .0
            .property("number")
            .expect("public generated identity property exists");
        assert!(
            public.markings.is_empty(),
            "an unclassified key stays public"
        );
    }

    fn naming_series_object_types() -> Vec<norn_ir::ObjectTypeDef> {
        serde_json::from_value(json!([
            {
                "name": "marking.Invoice",
                "implements": ["marking.Scoped"],
                "markings": ["pii.invoice"],
                "properties": [
                    {"id": "number", "type": {"kind": "text"}},
                    {
                        "id": "tenant",
                        "type": {"kind": "ref", "name": "marking.ScopeText"},
                        "markings": ["pii.invoice_scope"]
                    }
                ],
                "identity": [{
                    "name": "business",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "INV-{tenant}-{#####}",
                        "timezone": "Etc/UTC",
                        "scopeFields": ["tenant"],
                        "resetPolicy": "never",
                        "gapPolicy": "gapless_within_commit"
                    }
                }]
            },
            {
                "name": "marking.SpecialInvoice",
                "extends": "marking.Invoice",
                "markings": ["pii.special_invoice"],
                "properties": [{
                    "id": "tenant",
                    "type": {"kind": "ref", "name": "marking.ScopeText"},
                    "markings": ["pii.special_scope"]
                }]
            },
            {
                "name": "marking.PublicInvoice",
                "properties": [
                    {"id": "number", "type": {"kind": "text"}},
                    {"id": "tenant", "type": {"kind": "text"}}
                ],
                "identity": [{
                    "name": "business",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "PUB-{tenant}-{#####}",
                        "timezone": "Etc/UTC",
                        "scopeFields": ["tenant"],
                        "resetPolicy": "never",
                        "gapPolicy": "gapless_within_commit"
                    }
                }]
            }
        ]))
        .expect("object types decode")
    }
}
