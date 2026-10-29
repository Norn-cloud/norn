//! Literal values of a decide graph's fact changes (T3-LIFE-2, #2673).
//!
//! The output schema's enums bound every value a decision can emit, and the
//! evaluator validates output against them. A literal value in an expression
//! site that feeds the facts is also checked here, directly and per record
//! ([`super::literals`] reads the object literals):
//!
//! - every literal `op`, `objectType`, `property`, `lifecycle`, and
//!   `transition` of a fact-change record (an object literal with an `op` or
//!   `objectType` member), and every whole-literal cell of a decision-table
//!   output column with one of those names, is admitted by that member's
//!   enum ([`codes::FACT_LITERAL_OUTSIDE_SCHEMA`]);
//! - a literal `transition` is declared by the lifecycle the record names:
//!   its literal `lifecycle` (or every `lifecycle` enum value when that is
//!   computed) of its literal `objectType`, or, when that is computed, of
//!   every resolved `objectType` enum value that declares the lifecycle, at
//!   least one of which must (#3080)
//!   ([`codes::UNDECLARED_TRANSITION_LITERAL`]);
//! - a literal state in a `create` record's `values` for a lifecycle
//!   property is admitted by that value's enum and is an initial state
//!   ([`crate::check_lifecycle_writes`]).
//!
//! Sites that build `commands` or `timers` are not scanned.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, DiagnosticRemediation, FactChangeOp, JdmNode, JsonValue, PropertyType,
    RuntimeValue,
};

use super::literals::{self, Record, Value};
use super::{FactSchema, ITEMS_POINTER, codes, finding, member, string_enum};
use crate::entity_instance::{EffectiveKind, EffectiveLifecycle, EffectiveObjectType};
use crate::{Diagnostic, InstanceError, LifecycleWrite, Span, check_lifecycle_writes};

/// The fact-change members whose schema is a string enum.
const ENUM_MEMBERS: [&str; 5] = ["op", "objectType", "property", "lifecycle", "transition"];

/// A resolved object type the decision may write, with its effective
/// lifecycle and stored-property contracts.
pub(super) struct Resolved<'a> {
    pub(super) kind: &'a str,
    pub(super) lifecycles: &'a [EffectiveLifecycle],
    pub(super) effective_object_type: &'a EffectiveObjectType,
}

/// One expression site that feeds the facts.
enum Site {
    /// The object literals of an expression.
    Records(Vec<Record>),
    /// One table row's columns assembled for stored-property checks.
    StoredRecord(Record),
    /// A whole-literal cell of a table output column named like a
    /// fact-change member.
    Cell { member: String, literal: String },
}

/// The sites of a decide graph that feed its facts, with the JSON pointer of
/// each.
fn sites(artifact: &ArtifactEnvelope) -> Vec<(String, Site)> {
    let Some(graph) = crate::jdm_profile::decode_graph(artifact) else {
        return Vec::new();
    };
    let feeds_facts = |path: &str| {
        let head = path.split(['.', '[']).next().unwrap_or_default();
        head != "commands" && head != "timers"
    };
    let mut out = Vec::new();
    for (node_index, node) in graph.nodes.iter().enumerate() {
        match node {
            JdmNode::ExpressionNode { content, .. } => {
                for (index, expression) in content.expressions.iter().enumerate() {
                    if feeds_facts(&expression.key) {
                        out.push((
                            format!(
                                "/payload/graph/nodes/{node_index}/content/expressions/{index}/value"
                            ),
                            Site::Records(literals::records(expression.value.as_str())),
                        ));
                    }
                }
            }
            JdmNode::DecisionTableNode { content, .. } => {
                for (row_index, row) in content.rules.iter().enumerate() {
                    let mut records = BTreeMap::<String, Record>::new();
                    for output in &content.outputs {
                        let Some(cell) = row.cells.get(&output.id) else {
                            continue;
                        };
                        let field = content.output_path.as_ref().map_or_else(
                            || output.field.clone(),
                            |path| format!("{path}.{}", output.field),
                        );
                        table_member(&mut records, &field, cell.as_str());
                    }
                    for record in records.into_values() {
                        out.push((
                            format!("/payload/graph/nodes/{node_index}/content/rules/{row_index}"),
                            Site::StoredRecord(record),
                        ));
                    }
                }
                for output in content
                    .outputs
                    .iter()
                    .filter(|output| feeds_facts(&output.field))
                {
                    let column = output
                        .field
                        .rsplit(['.', '['])
                        .next()
                        .filter(|name| ENUM_MEMBERS.contains(name));
                    for (row_index, row) in content.rules.iter().enumerate() {
                        let Some(cell) = row.cells.get(&output.id) else {
                            continue;
                        };
                        let pointer = Span::from_segments([
                            "payload".to_owned(),
                            "graph".to_owned(),
                            "nodes".to_owned(),
                            node_index.to_string(),
                            "content".to_owned(),
                            "rules".to_owned(),
                            row_index.to_string(),
                            output.id.clone(),
                        ])
                        .pointer;
                        if let (Some(member), Some(literal)) =
                            (column, literals::whole_literal(cell.as_str()))
                        {
                            out.push((
                                pointer.clone(),
                                Site::Cell {
                                    member: member.to_owned(),
                                    literal,
                                },
                            ));
                        }
                        out.push((pointer, Site::Records(literals::records(cell.as_str()))));
                    }
                }
            }
            JdmNode::InputNode { .. }
            | JdmNode::OutputNode { .. }
            | JdmNode::SwitchNode { .. }
            | JdmNode::DecisionNode { .. }
            | JdmNode::FunctionNode { .. }
            | JdmNode::CustomNode { .. } => {}
        }
    }
    out
}

fn table_member(records: &mut BTreeMap<String, Record>, field: &str, source: &str) {
    if field.split(['.', '[']).next() != Some("facts") {
        return;
    }
    if let Some((fact, property)) = field.split_once(".patch.") {
        let record = records.entry(fact.to_owned()).or_default();
        let mut patch = match record.get("patch") {
            Some(Value::Object(patch)) => patch.clone(),
            _ => Record::default(),
        };
        patch.insert_path(property, literals::whole_value(source));
        record.insert("patch", Value::Object(patch));
    } else if let Some((fact, name)) = field.rsplit_once('.')
        && matches!(name, "op" | "objectType" | "property" | "value" | "patch")
    {
        records
            .entry(fact.to_owned())
            .or_default()
            .insert(name, literals::whole_value(source));
    }
}

/// Check every literal fact-change value of `artifact` against `schema` and
/// the lifecycles of the `resolved` object types. See the module docs.
pub(super) fn literal_findings(
    artifact: &ArtifactEnvelope,
    schema: &FactSchema<'_>,
    resolved: &[Resolved<'_>],
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let mut covered_ops = BTreeSet::new();
    for (pointer, site) in sites(artifact) {
        match site {
            Site::StoredRecord(record) => {
                if record.get("patch").is_some() || record.get("value").is_some() {
                    let ops = record.literal("op").map_or_else(
                        || schema.ops(),
                        |op| FactChangeOp::parse(op).into_iter().collect(),
                    );
                    covered_ops.extend(ops.iter().copied());
                    let types = resolved
                        .iter()
                        .filter(|target| {
                            record
                                .literal("objectType")
                                .is_none_or(|kind| target.kind == kind)
                        })
                        .collect::<Vec<_>>();
                    findings.extend(stored_property_findings(
                        artifact, schema, &types, &record, &pointer, &ops,
                    ));
                }
            }
            Site::Cell { member, literal } => {
                findings.extend(outside(artifact, schema, &member, &literal, &pointer));
            }
            Site::Records(records) => {
                for record in records.iter().filter(|record| {
                    record.get("op").is_some() || record.get("objectType").is_some()
                }) {
                    covered_ops.extend(record.literal("op").map_or_else(
                        || schema.ops(),
                        |op| FactChangeOp::parse(op).into_iter().collect(),
                    ));
                    findings.extend(record_findings(
                        artifact, schema, resolved, record, &pointer,
                    ));
                }
            }
        }
    }
    let uncovered_ops: Vec<FactChangeOp> = schema
        .ops()
        .into_iter()
        .filter(|op| !covered_ops.contains(op))
        .collect();
    findings.extend(schema_property_findings(
        artifact,
        schema,
        resolved,
        &uncovered_ops,
    ));
    findings
}

/// A literal `member` value its enum does not admit.
fn outside(
    artifact: &ArtifactEnvelope,
    schema: &FactSchema<'_>,
    member: &str,
    literal: &str,
    pointer: &str,
) -> Option<Diagnostic> {
    let admitted = schema.values(member).unwrap_or_default();
    (!admitted.contains(&literal)).then(|| {
        finding(
            codes::FACT_LITERAL_OUTSIDE_SCHEMA,
            format!(
                "the literal {member} {literal:?} is not admitted by the facts item schema's {member} enum {admitted:?}"
            ),
            artifact,
            pointer,
        )
    })
}

fn record_findings(
    artifact: &ArtifactEnvelope,
    schema: &FactSchema<'_>,
    resolved: &[Resolved<'_>],
    record: &Record,
    pointer: &str,
) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = ENUM_MEMBERS
        .iter()
        .filter_map(|name| {
            let literal = record.literal(name)?;
            outside(artifact, schema, name, literal, pointer)
        })
        .collect();
    // The ops, and the object types, the record may be.
    let ops: Vec<FactChangeOp> = match record.get("op") {
        Some(Value::Str(op)) => FactChangeOp::parse(op).into_iter().collect(),
        _ => schema.ops(),
    };
    let types: Vec<&Resolved<'_>> = match record.literal("objectType") {
        Some(kind) => resolved
            .iter()
            .filter(|target| target.kind == kind)
            .collect(),
        None => resolved.iter().collect(),
    };

    if ops.contains(&FactChangeOp::Transition)
        && let Some(id) = record.literal("transition")
    {
        let lifecycles: Vec<&str> = match record.literal("lifecycle") {
            Some(lifecycle) => vec![lifecycle],
            None => schema.values("lifecycle").unwrap_or_default(),
        };
        let literal_type = record.literal("objectType").is_some();
        // An unresolved written type already failed closed and may be the one
        // that declares the lifecycle.
        let all_resolved = resolved.len() == schema.values("objectType").unwrap_or_default().len();
        let reason = undeclared_transition(&types, &lifecycles, id, literal_type, all_resolved);
        if let Some(reason) = reason {
            out.push(finding(
                codes::UNDECLARED_TRANSITION_LITERAL,
                format!("the expression fires transition {id:?}, but {reason}"),
                artifact,
                pointer,
            ));
        }
    }

    if ops.contains(&FactChangeOp::Create)
        && let Some(Value::Object(values)) = record.get("values")
    {
        let value_schemas =
            member(schema.get("values"), "properties").and_then(JsonValue::as_object);
        for target in &types {
            for machine in target.lifecycles {
                let kind = target.kind;
                let property = machine.lifecycle.property.as_str();
                let Some(state) = values.literal(property) else {
                    continue;
                };
                if let Some(states) =
                    string_enum(value_schemas.and_then(|schemas| schemas.get(property)))
                    && !states.contains(&state)
                {
                    out.push(finding(
                        codes::FACT_LITERAL_OUTSIDE_SCHEMA,
                        format!(
                            "the literal {property} {state:?} of a created {kind:?} is not admitted by the facts item schema's values enum {states:?}"
                        ),
                        artifact,
                        pointer,
                    ));
                }
                let write = LifecycleWrite {
                    from: None,
                    to: state,
                    at: Span::pointer(pointer),
                };
                out.extend(
                    check_lifecycle_writes(&machine.lifecycle, &[write])
                        .into_iter()
                        .map(|mut diagnostic| {
                            diagnostic.artifact_id = Some(artifact.artifact_id.clone());
                            diagnostic.remediation = Some(DiagnosticRemediation::FixDefinition);
                            diagnostic
                        }),
                );
            }
        }
    }
    out.extend(stored_property_findings(
        artifact, schema, &types, record, pointer, &ops,
    ));
    out
}

fn stored_property_findings(
    artifact: &ArtifactEnvelope,
    schema: &FactSchema<'_>,
    types: &[&Resolved<'_>],
    record: &Record,
    record_pointer: &str,
    ops: &[FactChangeOp],
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    if ops.contains(&FactChangeOp::Patch) {
        let patch_schemas =
            member(schema.get("patch"), "properties").and_then(JsonValue::as_object);
        let literal_patch = match record.get("patch") {
            Some(Value::Object(patch)) => Some(patch),
            _ => None,
        };
        let mut fields: BTreeMap<&str, Option<&Value>> =
            literal_patch.map_or_else(BTreeMap::new, |patch| {
                patch
                    .entries()
                    .map(|(name, value)| (name, Some(value)))
                    .collect()
            });
        if let Some(schemas) = patch_schemas {
            for name in schemas.keys() {
                fields.entry(name).or_insert(None);
            }
        }
        for target in types {
            for (property, literal) in &fields {
                if target.effective_object_type.lifecycle(property).is_some() {
                    continue;
                }
                let schema_value = patch_schemas.and_then(|members| members.get(*property));
                let schema_path = format!("{ITEMS_POINTER}/properties/patch/properties/{property}");
                findings.extend(check_stored_member(
                    artifact,
                    target,
                    property,
                    literal.and_then(Value::runtime_value).as_ref(),
                    record_pointer,
                    &schema_path,
                ));
                findings.extend(check_schema_values(
                    artifact,
                    target,
                    property,
                    schema_value,
                    &schema_path,
                ));
            }
        }
    }
    if ops.contains(&FactChangeOp::Set) {
        let properties: Vec<String> = record.literal("property").map_or_else(
            || {
                schema
                    .values("property")
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            },
            |property| vec![property.to_owned()],
        );
        let value = record.get("value").and_then(Value::runtime_value);
        let schema_value = schema.get("value");
        let schema_path = format!("{ITEMS_POINTER}/properties/value");
        for target in types {
            for property in &properties {
                if target.effective_object_type.lifecycle(property).is_some() {
                    continue;
                }
                let property_path = format!("{ITEMS_POINTER}/properties/property");
                findings.extend(check_stored_member(
                    artifact,
                    target,
                    property,
                    value.as_ref(),
                    record_pointer,
                    &property_path,
                ));
                findings.extend(check_schema_values(
                    artifact,
                    target,
                    property,
                    schema_value,
                    &schema_path,
                ));
            }
        }
    }
    findings
}

fn schema_property_findings(
    artifact: &ArtifactEnvelope,
    schema: &FactSchema<'_>,
    resolved: &[Resolved<'_>],
    ops: &[FactChangeOp],
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let patch_schemas = member(schema.get("patch"), "properties").and_then(JsonValue::as_object);
    let set_properties = schema.values("property").unwrap_or_default();
    for target in resolved {
        if ops.contains(&FactChangeOp::Patch) {
            for (property, value_schema) in patch_schemas.into_iter().flatten() {
                if !schema_target(target, resolved, property) {
                    continue;
                }
                if target.effective_object_type.lifecycle(property).is_some() {
                    continue;
                }
                let path = format!("{ITEMS_POINTER}/properties/patch/properties/{property}");
                findings.extend(check_stored_member(
                    artifact, target, property, None, &path, &path,
                ));
                findings.extend(check_schema_values(
                    artifact,
                    target,
                    property,
                    Some(value_schema),
                    &path,
                ));
            }
        }
        if ops.contains(&FactChangeOp::Set) {
            for property in &set_properties {
                if !schema_target(target, resolved, property) {
                    continue;
                }
                if target.effective_object_type.lifecycle(property).is_some() {
                    continue;
                }
                let path = format!("{ITEMS_POINTER}/properties/property");
                findings.extend(check_stored_member(
                    artifact, target, property, None, &path, &path,
                ));
                findings.extend(check_schema_values(
                    artifact,
                    target,
                    property,
                    schema.get("value"),
                    &format!("{ITEMS_POINTER}/properties/value"),
                ));
            }
        }
    }
    findings
}

/// An uncorrelated schema member may belong to any written type. Check
/// candidates declaring it; if none do, retain the unknown-member refusal.
/// Literal records and runtime values still check their actual target type.
fn schema_target(target: &Resolved<'_>, resolved: &[Resolved<'_>], property: &str) -> bool {
    let declares = |candidate: &Resolved<'_>| {
        candidate
            .effective_object_type
            .property(property)
            .is_some_and(|effective| !effective.computed)
    };
    declares(target) || !resolved.iter().any(declares)
}

fn check_stored_member(
    artifact: &ArtifactEnvelope,
    target: &Resolved<'_>,
    property: &str,
    value: Option<&RuntimeValue>,
    record_pointer: &str,
    schema_pointer: &str,
) -> Vec<Diagnostic> {
    let result = match value {
        Some(value) => target
            .effective_object_type
            .check_stored_property_value(property, value),
        None => target
            .effective_object_type
            .property(property)
            .filter(|effective| !effective.computed)
            .map_or_else(
                || {
                    Err(InstanceError::ConstraintViolation(format!(
                        "{property:?} is not an effective stored property"
                    )))
                },
                |_| Ok(()),
            ),
    };
    match result {
        Ok(()) => Vec::new(),
        Err(error) => vec![finding(
            codes::INVALID_STORED_PROPERTY_WRITE,
            format!(
                "the decision writes {property:?} of {:?}, which violates its effective stored-property contract: {error:?}",
                target.kind
            ),
            artifact,
            if value.is_some() {
                record_pointer
            } else {
                schema_pointer
            },
        )],
    }
}

fn check_schema_values(
    artifact: &ArtifactEnvelope,
    target: &Resolved<'_>,
    property: &str,
    value_schema: Option<&JsonValue>,
    schema_pointer: &str,
) -> Vec<Diagnostic> {
    let Some(value_schema) = value_schema else {
        return Vec::new();
    };
    let Some(effective) = target.effective_object_type.property(property) else {
        return Vec::new();
    };
    if effective.computed {
        return Vec::new();
    }
    let mut findings = Vec::new();
    if let Some(schema_type) = member(Some(value_schema), "type").and_then(JsonValue::as_string) {
        let compatible = match &effective.kind {
            EffectiveKind::Base(PropertyType::Text { .. } | PropertyType::Enum { .. }) => {
                schema_type == "string"
            }
            EffectiveKind::Base(PropertyType::Integer {} | PropertyType::Decimal { .. }) => {
                matches!(schema_type, "integer" | "number")
            }
            EffectiveKind::Base(PropertyType::Boolean {}) => schema_type == "boolean",
            // json-ext is checked against its actual schema by the runtime
            // property contract for literal values and before commit for
            // computed values.
            EffectiveKind::Base(_) | EffectiveKind::Registry(_) | EffectiveKind::Unresolved => true,
        };
        if !compatible {
            findings.push(finding(
                codes::INVALID_STORED_PROPERTY_WRITE,
                format!(
                    "the decision's {property:?} schema type {schema_type:?} is incompatible with the effective stored-property type of {:?}",
                    target.kind
                ),
                artifact,
                schema_pointer,
            ));
        }
    }
    let mut values = Vec::new();
    if let Some(items) = member(Some(value_schema), "enum").and_then(JsonValue::as_array) {
        values.extend(
            items
                .iter()
                .enumerate()
                .map(|(index, value)| (value, format!("{schema_pointer}/enum/{index}"))),
        );
    }
    if let Some(value) = member(Some(value_schema), "const") {
        values.push((value, format!("{schema_pointer}/const")));
    }
    findings.extend(values
        .into_iter()
        .filter_map(|(value, pointer)| {
            let runtime = RuntimeValue::from_ir(value).ok()?;
            target
                .effective_object_type
                .check_stored_property_value(property, &runtime)
                .err()
                .map(|error| {
                    finding(
                        codes::INVALID_STORED_PROPERTY_WRITE,
                        format!(
                            "the decision's {property:?} schema value violates the effective stored-property contract of {:?}: {error:?}",
                            target.kind
                        ),
                        artifact,
                        &pointer,
                    )
                })
        }));
    findings
}

/// Why literal transition `id` of `lifecycles` is not declared on `types`, if
/// it is not. A literal object type must declare the lifecycle. A computed
/// one ranges over the written types, and only those that declare the
/// lifecycle are transition targets (#3080); at least one must.
fn undeclared_transition(
    types: &[&Resolved<'_>],
    lifecycles: &[&str],
    id: &str,
    literal_type: bool,
    all_resolved: bool,
) -> Option<String> {
    if !literal_type
        && all_resolved
        && let Some(property) = lifecycles.iter().find(|property| {
            !types
                .iter()
                .any(|target| machine_of(target.lifecycles, property).is_some())
        })
    {
        return Some(format!(
            "no type the decision writes has a {property:?} lifecycle"
        ));
    }
    types.iter().find_map(|target| {
        lifecycles
            .iter()
            .find_map(|property| match machine_of(target.lifecycles, property) {
                None => {
                    literal_type.then(|| format!("{:?} has no {property:?} lifecycle", target.kind))
                }
                Some(lifecycle) => (!lifecycle
                    .transitions
                    .iter()
                    .any(|transition| transition.id.as_str() == id))
                .then(|| {
                    format!(
                        "the {property:?} lifecycle of {:?} does not declare it",
                        target.kind
                    )
                }),
            })
    })
}

/// The effective lifecycle of `property` among `machines`, if declared.
fn machine_of<'m>(
    machines: &'m [EffectiveLifecycle],
    property: &str,
) -> Option<&'m norn_ir::Lifecycle> {
    machines
        .iter()
        .find(|machine| machine.lifecycle.property.as_str() == property)
        .map(|machine| &machine.lifecycle)
}
