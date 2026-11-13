//! PRIVACY-3, the law "personal data never trains a model".
//!
//! A candidate model is refused at admission when one of its training inputs
//! (a feature or the target) carries a personal-data marking that is not
//! pseudonymized. A marking is personal data when its declared category is
//! `pii` or under it (`pii.contact`); an undeclared marking is judged by its
//! id the same way, so an unknown classification never passes as public. It is
//! pseudonymized when the cell's catalog declares its projection as
//! `pseudonym`: the model then trains on stable keyed pseudonyms, never on
//! the values.
//!
//! The markings of an input are found through the same lineage the cell
//! propagates them along: the input type's own markings, the property's,
//! the shared property type it refers to, the interfaces the type implements,
//! and every mapping that writes the property from a classified source field.
//! The refusal names the input, each path that carries the marking, and what
//! would pass: pseudonymizing the marking or dropping the input.

use std::collections::BTreeMap;

use norn_ir::{ArtifactEnvelope, ArtifactKind, MarkingIr, MaskPolicy, ModelRoot};
use serde_json::Value;

use crate::Diagnostic;

/// The refusal code of the law.
pub(crate) const PERSONAL_DATA_TRAINING: &str = "model.law.personal_data_never_trains";

/// The category that classifies personal data, and its sub-categories.
pub(crate) const PERSONAL_DATA_CATEGORY: &str = "pii";

fn personal(path: &str) -> bool {
    path == PERSONAL_DATA_CATEGORY
        || path
            .strip_prefix(PERSONAL_DATA_CATEGORY)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `marking` classifies personal data that has not been pseudonymized.
fn trains_personal_data(catalog: &BTreeMap<String, MarkingIr>, marking: &str) -> bool {
    match catalog.get(marking) {
        Some(declared) => {
            personal(&declared.category) && !matches!(declared.mask, MaskPolicy::Pseudonym { .. })
        }
        None => personal(marking),
    }
}

fn payload(artifact: &ArtifactEnvelope) -> Value {
    serde_json::to_value(artifact.payload.to_json_value()).unwrap_or(Value::Null)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// One way a marking reaches an input: the marking and the path, in words.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Carried {
    pub(crate) marking: String,
    pub(crate) path: String,
}

/// The definitions the law reads, from the candidate set and the base.
struct Lineage {
    /// Object type name → (entity artifact id, objectType JSON).
    types: BTreeMap<String, (String, Value)>,
    /// Interface name → interface JSON.
    interfaces: BTreeMap<String, Value>,
    /// Shared property type name → its markings.
    property_types: BTreeMap<String, Vec<String>>,
    /// Mapping artifact id → payload.
    mappings: Vec<(String, Value)>,
    /// (dataset, field) → [(source schema id, markings)].
    source_fields: BTreeMap<(String, String), Vec<(String, Vec<String>)>>,
}

impl Lineage {
    fn of(artifacts: &[&ArtifactEnvelope]) -> Self {
        let mut lineage = Self {
            types: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            property_types: BTreeMap::new(),
            mappings: Vec::new(),
            source_fields: BTreeMap::new(),
        };
        for artifact in artifacts {
            let body = payload(artifact);
            match artifact.kind() {
                ArtifactKind::Entity => {
                    if let Some(object) = body.get("objectType") {
                        if let Some(name) = object.get("name").and_then(Value::as_str) {
                            lineage
                                .types
                                .entry(name.to_owned())
                                .or_insert((artifact.artifact_id.clone(), object.clone()));
                        }
                    }
                    if let Some(interface) = body.get("interface") {
                        if let Some(name) = interface.get("name").and_then(Value::as_str) {
                            lineage
                                .interfaces
                                .entry(name.to_owned())
                                .or_insert(interface.clone());
                        }
                    }
                    for shared in body
                        .get("propertyTypes")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(name) = shared.get("name").and_then(Value::as_str) {
                            lineage
                                .property_types
                                .entry(name.to_owned())
                                .or_insert_with(|| strings(shared.get("markings")));
                        }
                    }
                }
                ArtifactKind::Mapping => {
                    lineage.mappings.push((artifact.artifact_id.clone(), body));
                }
                ArtifactKind::SourceSchema => {
                    for field in body
                        .get("fields")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let (Some(dataset), Some(name)) = (
                            field.get("dataset").and_then(Value::as_str),
                            field.get("name").and_then(Value::as_str),
                        ) else {
                            continue;
                        };
                        let markings = strings(field.get("markings"));
                        if !markings.is_empty() {
                            lineage
                                .source_fields
                                .entry((dataset.to_owned(), name.to_owned()))
                                .or_default()
                                .push((artifact.artifact_id.clone(), markings));
                        }
                    }
                }
                _ => {}
            }
        }
        lineage
    }

    /// Every marking that reaches `property` of `object_type`, with its path.
    fn carried(&self, object_type: &str, property: &str) -> Vec<Carried> {
        let mut carried = Vec::new();
        let mut push = |markings: Vec<String>, path: String| {
            carried.extend(markings.into_iter().map(|marking| Carried {
                marking,
                path: path.clone(),
            }));
        };
        let Some((entity, object)) = self.types.get(object_type) else {
            return carried;
        };
        let member = format!("{object_type}.{property}");
        push(
            strings(object.get("markings")),
            format!("{object_type} (every property of the type)"),
        );
        let declared = |holder: &Value| {
            holder
                .get("properties")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(property))
                .cloned()
        };
        if let Some(own) = declared(object) {
            push(strings(own.get("markings")), member.clone());
            if let Some(shared) = own
                .get("type")
                .filter(|kind| kind.get("kind").and_then(Value::as_str) == Some("ref"))
                .and_then(|kind| kind.get("name"))
                .and_then(Value::as_str)
            {
                if let Some(markings) = self.property_types.get(shared) {
                    push(
                        markings.clone(),
                        format!("{member} ← property type {shared}"),
                    );
                }
            }
        }
        for interface in strings(object.get("implements")) {
            if let Some(declared_by) = self.interfaces.get(&interface) {
                push(
                    strings(declared_by.get("markings")),
                    format!("{member} ← interface {interface}"),
                );
                if let Some(own) = declared(declared_by) {
                    push(
                        strings(own.get("markings")),
                        format!("{member} ← interface {interface}.{property}"),
                    );
                }
            }
        }
        for (mapping, body) in &self.mappings {
            for map in body
                .get("maps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let target = map
                    .get("target")
                    .and_then(|target| target.get("entity"))
                    .and_then(Value::as_str);
                if target != Some(entity.as_str()) {
                    continue;
                }
                let Some(dataset) = map
                    .get("source")
                    .and_then(|source| source.get("dataset"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                for written in map
                    .get("properties")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|written| {
                        written.get("property").and_then(Value::as_str) == Some(property)
                    })
                {
                    let expression = written.get("value").and_then(Value::as_str).unwrap_or("");
                    for field in input_fields(expression) {
                        for (schema, markings) in self
                            .source_fields
                            .get(&(dataset.to_owned(), field.clone()))
                            .into_iter()
                            .flatten()
                        {
                            push(
                                markings.clone(),
                                format!(
                                    "{schema}: {dataset}.{field} → mapping {mapping} → {member}"
                                ),
                            );
                        }
                    }
                }
            }
        }
        carried.sort();
        carried.dedup();
        carried
    }
}

/// The source fields an expression reads (`input.<field>`).
fn input_fields(expression: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut rest = expression;
    while let Some(at) = rest.find("input.") {
        let before = rest.get(..at).and_then(|text| text.chars().last());
        let after = rest.get(at + "input.".len()..).unwrap_or("");
        let field: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') && !field.is_empty() {
            fields.push(field.clone());
        }
        rest = after.get(field.len()..).unwrap_or("");
    }
    fields
}

/// Refuse each candidate model that trains on unpseudonymized personal data.
pub(crate) fn check(
    models: &[(&ArtifactEnvelope, ModelRoot)],
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    finding: impl Fn(&str, String, String, &ArtifactEnvelope) -> Diagnostic,
) -> Vec<Diagnostic> {
    let all: Vec<&ArtifactEnvelope> = artifacts.iter().chain(base).collect();
    let lineage = Lineage::of(&all);
    let owned: Vec<ArtifactEnvelope> = all.iter().map(|artifact| (*artifact).clone()).collect();
    let catalog: BTreeMap<String, MarkingIr> = crate::marking_catalog_from_artifacts(&owned)
        .into_iter()
        .map(|(id, marking)| (id.0, marking))
        .collect();
    let mut findings = Vec::new();
    for (artifact, root) in models {
        let input = root.input.object_type().as_str();
        let uses = root
            .features
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), format!("/payload/features/{index}"), "feature"))
            .chain(
                root.target
                    .iter()
                    .map(|id| (id.as_str(), "/payload/target".to_owned(), "target")),
            );
        for (property, pointer, role) in uses {
            let carried: Vec<Carried> = lineage
                .carried(input, property)
                .into_iter()
                .filter(|carried| trains_personal_data(&catalog, &carried.marking))
                .collect();
            if carried.is_empty() {
                continue;
            }
            let mut markings: Vec<&str> = carried
                .iter()
                .map(|carried| carried.marking.as_str())
                .collect();
            markings.dedup();
            let paths = carried
                .iter()
                .map(|carried| format!("`{}` via {}", carried.marking, carried.path))
                .collect::<Vec<_>>()
                .join("; ");
            findings.push(finding(
                PERSONAL_DATA_TRAINING,
                format!(
                    "personal data never trains a model: the {role} {input}.{property} carries {paths}. \
                     It would pass if {} declared a pseudonym projection (mask {{\"kind\": \"pseudonym\"}}), \
                     or if {property} were dropped from the model's {role}s",
                    markings
                        .iter()
                        .map(|marking| format!("`{marking}`"))
                        .collect::<Vec<_>>()
                        .join(" and "),
                ),
                pointer,
                artifact,
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::input_fields;

    #[test]
    fn input_fields_are_the_fields_an_expression_reads() {
        assert_eq!(input_fields("input.email"), ["email"]);
        assert_eq!(
            input_fields("lower(input.first_name) + ' ' + input.last_name"),
            ["first_name", "last_name"]
        );
        assert!(input_fields("myinput.email").is_empty());
    }
}
