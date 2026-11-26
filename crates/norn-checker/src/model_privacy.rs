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

use std::collections::{BTreeMap, BTreeSet};

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
    value.and_then(Value::as_array).map_or_default(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
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
    /// Target object type name → extension JSONs visible to the model.
    extensions: BTreeMap<String, Vec<Value>>,
    /// Interface name → interface JSON.
    interfaces: BTreeMap<String, Value>,
    /// Shared property type name → its markings.
    property_types: BTreeMap<String, Vec<String>>,
    /// Mapping artifact id → payload.
    mappings: Vec<(String, Value)>,
    /// (dataset, field) → [(source schema id, markings)].
    source_fields: BTreeMap<SourceField, Vec<(String, Vec<String>)>>,
}

/// A source dataset and one of its fields.
type SourceField = (String, String);

impl Lineage {
    fn of(artifacts: &[&ArtifactEnvelope]) -> Self {
        let mut lineage = Self {
            types: BTreeMap::new(),
            extensions: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            property_types: BTreeMap::new(),
            mappings: Vec::new(),
            source_fields: BTreeMap::new(),
        };
        for artifact in artifacts {
            match artifact.kind() {
                ArtifactKind::Entity => {
                    let body = payload(artifact);
                    if let Some(object) = body.get("objectType")
                        && let Some(name) = object.get("name").and_then(Value::as_str)
                    {
                        lineage
                            .types
                            .entry(name.to_owned())
                            .or_insert_with(|| (artifact.artifact_id.clone(), object.clone()));
                    }
                    if let Some(ext) = body.get("objectTypeExtension")
                        && let Some(target) = ext.get("target").and_then(Value::as_str)
                    {
                        lineage
                            .extensions
                            .entry(target.to_owned())
                            .or_default()
                            .push(ext.clone());
                    }
                    if let Some(interface) = body.get("interface")
                        && let Some(name) = interface.get("name").and_then(Value::as_str)
                    {
                        lineage
                            .interfaces
                            .entry(name.to_owned())
                            .or_insert_with(|| interface.clone());
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
                    lineage
                        .mappings
                        .push((artifact.artifact_id.clone(), payload(artifact)));
                }
                ArtifactKind::SourceSchema => {
                    let body = payload(artifact);
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
                ArtifactKind::Relation
                | ArtifactKind::Rule
                | ArtifactKind::Action
                | ArtifactKind::Connector
                | ArtifactKind::Widget
                | ArtifactKind::Policy
                | ArtifactKind::Customization
                | ArtifactKind::Setting
                | ArtifactKind::BusinessPack
                | ArtifactKind::Derived
                | ArtifactKind::Law
                | ArtifactKind::DefinedConcept
                | ArtifactKind::Labels
                | ArtifactKind::Model => {}
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
        if !self.types.contains_key(object_type) {
            return carried;
        }
        let member = format!("{object_type}.{property}");
        let declared = |holder: &Value| {
            holder
                .get("properties")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(property))
                .cloned()
        };
        let mut visited_types = BTreeSet::new();
        let mut pending_interfaces = Vec::new();
        let mut current = Some(object_type.to_owned());
        while let Some(type_name) = current {
            if !visited_types.insert(type_name.clone()) {
                break;
            }
            let Some((entity, object)) = self.types.get(&type_name) else {
                break;
            };
            let is_self = type_name == object_type;
            let type_path = if is_self {
                format!("{object_type} (every property of the type)")
            } else {
                format!("{member} ← supertype {type_name} (every property of the type)")
            };
            push(strings(object.get("markings")), type_path);
            pending_interfaces.extend(strings(object.get("implements")));
            for holder in
                std::iter::once(object).chain(self.extensions.get(&type_name).into_iter().flatten())
            {
                if let Some(own) = declared(holder) {
                    let prop_path = if is_self {
                        member.clone()
                    } else {
                        format!("{member} ← supertype {type_name}.{property}")
                    };
                    push(strings(own.get("markings")), prop_path);
                    if let Some(shared) = shared_property_type(&own)
                        && let Some(markings) = self.property_types.get(shared)
                    {
                        push(
                            markings.clone(),
                            format!("{member} ← property type {shared}"),
                        );
                    }
                }
            }
            for (markings, path) in self.mapped(entity, &member, property) {
                push(markings, path);
            }
            current = object
                .get("extends")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        self.carried_from_interfaces(&member, property, pending_interfaces, &mut carried);
        carried.sort();
        carried.dedup();
        carried
    }

    fn carried_from_interfaces(
        &self,
        member: &str,
        property: &str,
        mut pending_interfaces: Vec<String>,
        carried: &mut Vec<Carried>,
    ) {
        let mut push = |markings: Vec<String>, path: String| {
            carried.extend(markings.into_iter().map(|marking| Carried {
                marking,
                path: path.clone(),
            }));
        };
        let mut visited_interfaces = BTreeSet::new();
        while let Some(interface) = pending_interfaces.pop() {
            if !visited_interfaces.insert(interface.clone()) {
                continue;
            }
            let Some(declared_by) = self.interfaces.get(&interface) else {
                continue;
            };
            push(
                strings(declared_by.get("markings")),
                format!("{member} ← interface {interface}"),
            );
            if let Some(own) = declared_by
                .get("properties")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(property))
            {
                push(
                    strings(own.get("markings")),
                    format!("{member} ← interface {interface}.{property}"),
                );
                if let Some(shared) = shared_property_type(own)
                    && let Some(markings) = self.property_types.get(shared)
                {
                    push(
                        markings.clone(),
                        format!(
                            "{member} ← interface {interface}.{property} ← property type {shared}"
                        ),
                    );
                }
            }
            pending_interfaces.extend(strings(declared_by.get("extends")));
            if let Some(parent) = declared_by.get("extends").and_then(Value::as_str) {
                pending_interfaces.push(parent.to_owned());
            }
        }
    }

    /// The markings mappings carry into `property` from classified source
    /// fields, each with its path.
    fn mapped(&self, entity: &str, member: &str, property: &str) -> Vec<(Vec<String>, String)> {
        let mut paths = Vec::new();
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
                if target != Some(entity) {
                    continue;
                }
                let Some(dataset) = map
                    .get("source")
                    .and_then(|source| source.get("dataset"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let when_read = map
                    .get("when")
                    .filter(|expr| !expr.is_null())
                    .map(|expr| expr.as_str().map_or_else(whole_root_reads, input_reads));
                for written in map
                    .get("properties")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|written| {
                        written.get("property").and_then(Value::as_str) == Some(property)
                    })
                {
                    if let Some(when_read) = &when_read {
                        self.push_dataset_reads(dataset, when_read, mapping, member, &mut paths);
                    }
                    let value_read = written
                        .get("value")
                        .and_then(Value::as_str)
                        .map_or_else(whole_root_reads, input_reads);
                    if let Some(nested) = written.get("nested").filter(|value| !value.is_null()) {
                        let parent_read = nested.get("field").and_then(Value::as_str).map_or_else(
                            whole_root_reads,
                            |field| norn_expr::ExpressionReads {
                                fields: BTreeSet::from([field.to_owned()]),
                                whole_root: false,
                            },
                        );
                        self.push_dataset_reads(dataset, &parent_read, mapping, member, &mut paths);
                        let where_read = nested
                            .get("where")
                            .and_then(Value::as_str)
                            .map_or_else(whole_root_reads, input_reads);
                        let mut child_read = value_read;
                        child_read.whole_root |= where_read.whole_root;
                        child_read.fields.extend(where_read.fields);
                        if let Some(child_dataset) = nested.get("dataset").and_then(Value::as_str) {
                            self.push_dataset_reads(
                                child_dataset,
                                &child_read,
                                mapping,
                                member,
                                &mut paths,
                            );
                        } else {
                            self.push_dataset_reads(
                                dataset,
                                &whole_root_reads(),
                                mapping,
                                member,
                                &mut paths,
                            );
                        }
                    } else {
                        self.push_dataset_reads(dataset, &value_read, mapping, member, &mut paths);
                    }
                }
            }
        }
        paths
    }

    fn push_dataset_reads(
        &self,
        dataset: &str,
        read: &norn_expr::ExpressionReads,
        mapping: &str,
        member: &str,
        paths: &mut Vec<(Vec<String>, String)>,
    ) {
        if read.whole_root {
            for ((source_dataset, field), schemas) in &self.source_fields {
                if source_dataset != dataset {
                    continue;
                }
                for (schema, markings) in schemas {
                    paths.push((
                        markings.clone(),
                        format!("{schema}: {dataset}.{field} → mapping {mapping} → {member}"),
                    ));
                }
            }
        } else {
            for field in &read.fields {
                for (schema, markings) in self
                    .source_fields
                    .get(&(dataset.to_owned(), field.clone()))
                    .into_iter()
                    .flatten()
                {
                    paths.push((
                        markings.clone(),
                        format!("{schema}: {dataset}.{field} → mapping {mapping} → {member}"),
                    ));
                }
            }
        }
    }
}

fn whole_root_reads() -> norn_expr::ExpressionReads {
    norn_expr::ExpressionReads {
        fields: BTreeSet::new(),
        whole_root: true,
    }
}

fn shared_property_type(property: &Value) -> Option<&str> {
    property
        .get("type")
        .filter(|kind| kind.get("kind").and_then(Value::as_str) == Some("ref"))
        .and_then(|kind| kind.get("name"))
        .and_then(Value::as_str)
}

/// The source fields an expression reads from `input`, failing closed to a
/// whole-root read when the expression cannot be parsed or reads `input`
/// dynamically (`input[k]`, `$root`, pass-through).
fn input_reads(expression: &str) -> norn_expr::ExpressionReads {
    norn_expr::expression_reads(expression, "input").unwrap_or(norn_expr::ExpressionReads {
        fields: BTreeSet::new(),
        whole_root: true,
    })
}

/// Refuse each candidate model that trains on unpseudonymized personal data.
pub(crate) fn check(
    models: &[(&ArtifactEnvelope, ModelRoot)],
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    finding: impl Fn(&str, String, String, &ArtifactEnvelope) -> Diagnostic,
) -> Vec<Diagnostic> {
    let mut views = crate::entity_scopes::ScopedViews::new(artifacts, base, &BTreeSet::new());
    let mut scope_cache = BTreeMap::new();
    let mut findings = Vec::new();
    for (artifact, root) in models {
        let key = crate::entity_scopes::scope_key(&artifact.scope);
        let (lineage, catalog) = &*scope_cache.entry(key).or_insert_with(|| {
            let pool = views.of(artifact).pool();
            let lineage = Lineage::of(&pool.iter().collect::<Vec<_>>());
            let catalog: BTreeMap<String, MarkingIr> = crate::marking_catalog_from_artifacts(pool)
                .into_iter()
                .map(|(id, marking)| (id.0, marking))
                .collect();
            (lineage, catalog)
        });
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
                .filter(|carried| trains_personal_data(catalog, &carried.marking))
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

/// Conservatively resolve the schema columns read by a transform expression.
/// An empty dependency path (whole-row or unresolved alias read) maps to every
/// column in `schema` so static and runtime marking guards never miss a read.
pub(crate) fn expression_columns<'a>(
    schema: &'a crate::transform::NodeSchema,
    source: &str,
) -> Result<Vec<&'a crate::transform::Column>, String> {
    let paths = norn_expr::expression_read_paths(source, crate::transform::ROW_ROOT)
        .map_err(|error| error.to_string())?;
    if paths.iter().any(Vec::is_empty) {
        return Ok(schema.columns.iter().collect());
    }
    Ok(paths
        .iter()
        .filter_map(|path| path.first())
        .filter_map(|name| schema.column(name))
        .collect())
}

impl crate::transform::TransformPlan {
    fn push_column_origins(
        &self,
        out: &mut Vec<(String, String, String)>,
        pointer: &str,
        column: &crate::transform::Column,
    ) {
        for origin in &column.origins {
            if let Some(scan) = self.inputs.get(&origin.alias) {
                out.push((
                    pointer.to_owned(),
                    scan.object_type.clone(),
                    origin.property.clone(),
                ));
            }
        }
    }

    fn push_named_origin(
        &self,
        out: &mut Vec<(String, String, String)>,
        pointer: &str,
        schema: &crate::transform::NodeSchema,
        col_name: &str,
    ) {
        if let Some(column) = schema.column(col_name) {
            self.push_column_origins(out, pointer, column);
        }
    }

    /// Every `(pointer, object_type, property)` origin read by a `Filter`
    /// predicate or `Window`/`Dedupe` `partitionBy`/`key`/`orderBy` key, used
    /// at runtime to refuse dynamic `_norn_markings` on filter, partition,
    /// dedupe, or order columns.
    #[must_use]
    pub fn filter_and_order_origins(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (node_id, node) in &self.nodes {
            match &node.step {
                crate::transform::Step::Filter { input, predicate } => {
                    if let Some(input_node) = self.nodes.get(input) {
                        let pointer = format!("/nodes/{node_id}/predicate");
                        let cols =
                            expression_columns(&input_node.schema, predicate.expression.source())
                                .unwrap_or_else(|_| input_node.schema.columns.iter().collect());
                        for col in cols {
                            self.push_column_origins(&mut out, &pointer, col);
                        }
                    }
                }
                crate::transform::Step::Window {
                    input,
                    partition_by,
                    order_by,
                    ..
                } => {
                    if let Some(input_node) = self.nodes.get(input) {
                        for (index, col_name) in partition_by.iter().enumerate() {
                            self.push_named_origin(
                                &mut out,
                                &format!("/nodes/{node_id}/partitionBy/{index}"),
                                &input_node.schema,
                                col_name,
                            );
                        }
                        for (index, (col_name, _, _)) in order_by.iter().enumerate() {
                            self.push_named_origin(
                                &mut out,
                                &format!("/nodes/{node_id}/orderBy/{index}"),
                                &input_node.schema,
                                col_name,
                            );
                        }
                    }
                }
                crate::transform::Step::Dedupe {
                    input,
                    key,
                    order_by,
                    ..
                } => {
                    if let Some(input_node) = self.nodes.get(input) {
                        for (index, col_name) in key.iter().enumerate() {
                            self.push_named_origin(
                                &mut out,
                                &format!("/nodes/{node_id}/key/{index}"),
                                &input_node.schema,
                                col_name,
                            );
                        }
                        for (index, (col_name, _, _)) in order_by.iter().enumerate() {
                            self.push_named_origin(
                                &mut out,
                                &format!("/nodes/{node_id}/orderBy/{index}"),
                                &input_node.schema,
                                col_name,
                            );
                        }
                    }
                }
                crate::transform::Step::Scan { .. }
                | crate::transform::Step::Project { .. }
                | crate::transform::Step::Join { .. }
                | crate::transform::Step::Aggregate { .. }
                | crate::transform::Step::Union { .. } => {}
            }
        }
        out
    }
}

/// The column type of a computed scalar in `ViewIR` transforms.
pub(crate) fn transform_column_type(inferred: &norn_expr::InferredType) -> Option<norn_expr::Type> {
    use norn_expr::{InferredType, Type};
    if let InferredType::Optional(inner) = inferred {
        return Some(Type::Optional(Box::new(transform_column_type(inner)?)));
    }
    if let InferredType::Enum(values) = inferred {
        return Some(Type::enumeration(values.iter().cloned()));
    }
    if matches!(inferred, InferredType::Bool) {
        Some(Type::Bool)
    } else if matches!(inferred, InferredType::String | InferredType::Const(_)) {
        Some(Type::text())
    } else if matches!(inferred, InferredType::Date) {
        Some(Type::Date)
    } else if matches!(inferred, InferredType::Timestamp) {
        Some(Type::Timestamp {
            timezone: norn_ir::TimezonePolicy::Utc,
        })
    } else if matches!(inferred, InferredType::Number) {
        Some(Type::Number)
    } else {
        // Null, lists and records are not column values.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::input_reads;
    use std::collections::BTreeSet;

    #[test]
    fn input_reads_covers_dot_bracket_dynamic_and_invalid_expressions() {
        let simple = input_reads("input.email");
        assert!(!simple.whole_root);
        assert_eq!(simple.fields, BTreeSet::from(["email".to_owned()]));

        let combined = input_reads("lower(input.first_name) + ' ' + input['last_name']");
        assert!(!combined.whole_root);
        assert_eq!(
            combined.fields,
            BTreeSet::from(["first_name".to_owned(), "last_name".to_owned()])
        );

        let unrelated = input_reads("myinput.email");
        assert!(!unrelated.whole_root);
        assert!(unrelated.fields.is_empty());

        for fail_closed in ["input[k]", "$root.email", "input", "input.email +"] {
            assert!(
                input_reads(fail_closed).whole_root,
                "expected whole_root fail-closed for {fail_closed:?}"
            );
        }
    }
}
