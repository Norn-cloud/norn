//! Loading an admitted `RuleIR` v2 decision and its sub-decision closure into
//! the engine.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use norn_expr::Value;
use norn_ir::jdm::{
    JdmDecisionTableContent, JdmEdge, JdmExecutionMode, JdmExpressionContent, JdmGraph,
    JdmHitPolicy, JdmNode, JdmSchemaContent, JdmSubDecisionContent, JdmSwitchContent,
};
use norn_ir::{
    DECIDE_INPUT_MEMBERS, DECIDE_OUTPUT_MEMBERS, JsonValue, RULE_IR_V2_VERSION,
    ResourceLimitProfile, RuleContractV2, RuleRoot, RuntimeObject, RuntimeValue,
};
use zen_engine::Decision;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{
    DecisionEdge, DecisionNode, DecisionNodeContent, DecisionNodeKind, DecisionTableContent,
    DecisionTableHitPolicy, DecisionTableInputField, DecisionTableOutputField, Expression,
    ExpressionNodeContent, GraphContent, InputNodeContent, OutputNodeContent, SwitchNodeContent,
    SwitchStatement, SwitchStatementHitPolicy, TransformAttributes, TransformExecutionMode,
};

use crate::convert::{value_from_ir, value_from_runtime};
use crate::schema::Schema;
use crate::{AdmissionError, DecisionError, TypedEvent, ValidatedSlice};

/// The `Norn-cloud/zen` tag the vendored engine (`vendor/zen`) was imported
/// from: part of every decision's engine identity. Equal to `fork.tag` in
/// `crates/norn-skeleton-guard/zen-pin.json` (tested).
pub const ENGINE_TAG: &str = "norn-v2.0.1-12";

/// The deepest sub-decision nesting the Norn profile admits (a graph that
/// calls a sub-decision that calls another is depth 2). Mirrors
/// `norn_checker::jdm_profile::MAX_SUB_DECISION_DEPTH`, which owns the bound.
pub const MAX_SUB_DECISION_DEPTH: usize = 3;

/// The most distinct sub-decision graphs a closure may hold (the root
/// excluded). Mirrors `norn_checker::jdm_profile::MAX_SUB_DECISION_CLOSURE`.
pub const MAX_SUB_DECISION_CLOSURE: usize = 16;

/// The engine's sub-decision depth limit: the root runs at iteration 0, so
/// the deepest admitted sub-decision runs at [`MAX_SUB_DECISION_DEPTH`].
pub(crate) const ENGINE_MAX_DEPTH: u8 = 4;

/// The admission context a decision is bound to. Both are opaque identities
/// computed by admission (`norn-checker`), carried so a decision result can
/// be attributed to exactly the profile and environment it ran under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecisionBinding {
    /// The expression/JDM profile identity (`norn_expr::check::CHECKER_PROFILE`,
    /// for example `norn-zen/6`).
    pub profile_id: String,
    /// Content hash of the type environment the decision was admitted in.
    pub environment_hash: String,
}

/// A `RuleIR` v2 `decide` graph with its exact sub-decision closure, loaded
/// into the engine and ready to evaluate.
///
/// Built by [`AdmittedDecision::admit`] from rules `norn-checker` has
/// admitted. The evaluator re-checks what it relies on (fail closed): the
/// graph is `RuleIR` v2 under the Norn profile (no `functionNode` or
/// `customNode`, no imports, no node-level schemas), the root implements the
/// `decide` contract and every other graph `sub_decision`, the closure is
/// exactly the reachable sub-decisions within the profile bounds, and the
/// schemas are the closed dialect. Sub-decisions resolve through an
/// in-memory, immediately-ready loader; no custom-node adapter or HTTP
/// handler is installed.
#[derive(Clone, Debug)]
pub struct AdmittedDecision {
    rule_id: String,
    binding: DecisionBinding,
    decision: Decision,
    slice_schema: Arc<Schema>,
    event_schema: Arc<Schema>,
    output_schema: Schema,
    defaults: BTreeMap<String, Value>,
}

impl AdmittedDecision {
    /// Loads `root` with the sub-decisions it reaches. `sub_decisions` is
    /// keyed by the `decisionNode` key (the sub-decision rule's artifact id)
    /// and must be exactly the reachable closure.
    ///
    /// # Errors
    ///
    /// An [`AdmissionError`] when any check above fails.
    pub fn admit(
        root: &RuleRoot,
        sub_decisions: &BTreeMap<String, RuleRoot>,
        binding: DecisionBinding,
    ) -> Result<Self, AdmissionError> {
        Self::admit_with_defaults(root, sub_decisions, binding, None)
    }

    /// [`Self::admit`], with the rule's `defaults` member as exact runtime
    /// values parsed from the artifact's source text (#2770). The decoded
    /// IR defaults carry numbers as `f64`, which cannot recover an authored
    /// decimal, so a numeric default is admitted only from `exact_defaults`;
    /// without it a numeric default is refused, as by [`Self::admit`].
    ///
    /// # Errors
    ///
    /// As [`Self::admit`]; [`AdmissionError::InvalidDefault`] when a numeric
    /// default has no exact source value, or the exact value is not the
    /// decoded one.
    pub fn admit_with_defaults(
        root: &RuleRoot,
        sub_decisions: &BTreeMap<String, RuleRoot>,
        binding: DecisionBinding,
        exact_defaults: Option<&RuntimeObject>,
    ) -> Result<Self, AdmissionError> {
        let root_graph = decision_graph(root, RuleContractV2::Decide)?;
        let loader = MemoryLoader::default();
        let mut loaded_keys = BTreeSet::new();
        // Deepest depth each key has been expanded at: a key is re-expanded
        // only on a deeper path (which may exceed the bound), so the walk is
        // at most MAX_SUB_DECISION_DEPTH expansions per key.
        let mut expanded: BTreeMap<&str, usize> = BTreeMap::new();
        let mut stack = vec![(root.rule_id.as_str(), root_graph, 0_usize)];
        while let Some((rule_id, graph, depth)) = stack.pop() {
            for key in sub_decision_keys(graph) {
                let child_depth = depth.saturating_add(1);
                if expanded.get(key).is_some_and(|seen| *seen >= child_depth) {
                    continue;
                }
                expanded.insert(key, child_depth);
                if child_depth > MAX_SUB_DECISION_DEPTH {
                    return Err(AdmissionError::SubDecisionBound {
                        depth: child_depth,
                        size: loaded_keys.len(),
                    });
                }
                let sub = sub_decisions.get(key).ok_or_else(|| {
                    AdmissionError::UnresolvedSubDecision {
                        rule_id: rule_id.to_owned(),
                        key: key.to_owned(),
                    }
                })?;
                let sub_graph = decision_graph(sub, RuleContractV2::SubDecision)?;
                if loaded_keys.insert(key) {
                    closed_record(sub, "inputSchema")?;
                    closed_record(sub, "outputSchema")?;
                    if loaded_keys.len() > MAX_SUB_DECISION_CLOSURE {
                        return Err(AdmissionError::SubDecisionBound {
                            depth: child_depth,
                            size: loaded_keys.len(),
                        });
                    }
                    loader.add(key, graph_content(&sub.rule_id, sub_graph)?);
                }
                stack.push((sub.rule_id.as_str(), sub_graph, child_depth));
            }
        }
        if let Some(unused) = sub_decisions
            .keys()
            .find(|key| !loaded_keys.contains(key.as_str()))
        {
            return Err(AdmissionError::UnusedSubDecision {
                key: unused.clone(),
            });
        }

        let content = graph_content(&root.rule_id, root_graph)?;
        let (slice_schema, event_schema) = decide_input(root)?;
        let output_schema = decide_output(root)?;
        let defaults = defaults(root, &output_schema, exact_defaults)?;
        Ok(Self {
            rule_id: root.rule_id.clone(),
            binding,
            decision: Decision::from(Arc::new(content)).with_loader(Arc::new(loader)),
            slice_schema: Arc::new(slice_schema),
            event_schema: Arc::new(event_schema),
            output_schema,
            defaults,
        })
    }

    /// The root rule's id.
    #[must_use]
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }

    /// The admission context this decision is bound to.
    #[must_use]
    pub const fn binding(&self) -> &DecisionBinding {
        &self.binding
    }

    /// The engine tag ([`ENGINE_TAG`]).
    #[must_use]
    pub const fn engine_tag(&self) -> &'static str {
        ENGINE_TAG
    }

    /// Validates `slice` against the decide input schema's `slice` record.
    ///
    /// # Errors
    ///
    /// [`DecisionError::InputSchema`] when it does not conform.
    pub fn validate_slice(&self, slice: Value) -> Result<ValidatedSlice, DecisionError> {
        check_input(&self.slice_schema, "slice", &slice)?;
        Ok(ValidatedSlice {
            rule_id: self.rule_id.clone(),
            schema: Arc::clone(&self.slice_schema),
            value: slice,
        })
    }

    /// Validates `event` against the decide input schema's `event` record.
    ///
    /// # Errors
    ///
    /// [`DecisionError::InputSchema`] when it does not conform.
    pub fn type_event(&self, event: Value) -> Result<TypedEvent, DecisionError> {
        check_input(&self.event_schema, "event", &event)?;
        Ok(TypedEvent {
            rule_id: self.rule_id.clone(),
            schema: Arc::clone(&self.event_schema),
            value: event,
        })
    }

    /// Whether `slice` and `event` were validated for this decision: the same
    /// rule and identical input schemas (an input validated under another
    /// version of the rule is not accepted).
    pub(crate) fn accepts(&self, slice: &ValidatedSlice, event: &TypedEvent) -> bool {
        slice.rule_id == self.rule_id
            && event.rule_id == self.rule_id
            && (Arc::ptr_eq(&slice.schema, &self.slice_schema) || slice.schema == self.slice_schema)
            && (Arc::ptr_eq(&event.schema, &self.event_schema) || event.schema == self.event_schema)
    }

    pub(crate) const fn decision(&self) -> &Decision {
        &self.decision
    }

    pub(crate) const fn output_schema(&self) -> &Schema {
        &self.output_schema
    }

    pub(crate) const fn defaults(&self) -> &BTreeMap<String, Value> {
        &self.defaults
    }
}

fn check_input(schema: &Schema, member: &'static str, value: &Value) -> Result<(), DecisionError> {
    schema
        .check(value, member)
        .map_err(|violation| DecisionError::InputSchema {
            member,
            path: violation.path,
            reason: violation.reason,
        })
}

/// The JDM graph of a `RuleIR` v2 rule that implements `contract`.
fn decision_graph(rule: &RuleRoot, contract: RuleContractV2) -> Result<&JdmGraph, AdmissionError> {
    let graph = rule
        .graph
        .as_ref()
        .filter(|_| rule.rule_version == RULE_IR_V2_VERSION)
        .ok_or_else(|| AdmissionError::NotDecisionGraph {
            rule_id: rule.rule_id.clone(),
            rule_version: rule.rule_version.clone(),
        })?;
    if rule.contract != Some(contract) {
        return Err(AdmissionError::WrongContract {
            rule_id: rule.rule_id.clone(),
            expected: match contract {
                RuleContractV2::Decide => "decide",
                RuleContractV2::SubDecision => "sub_decision",
            },
        });
    }
    Ok(graph)
}

fn sub_decision_keys(graph: &JdmGraph) -> impl Iterator<Item = &str> {
    graph.nodes.iter().filter_map(|node| match node {
        JdmNode::DecisionNode { content, .. } => Some(content.key.as_str()),
        JdmNode::InputNode { .. }
        | JdmNode::OutputNode { .. }
        | JdmNode::ExpressionNode { .. }
        | JdmNode::DecisionTableNode { .. }
        | JdmNode::SwitchNode { .. }
        | JdmNode::FunctionNode { .. }
        | JdmNode::CustomNode { .. } => None,
    })
}

/// The engine form of an admitted graph. A typed, exhaustive mapping (rather
/// than a JSON round trip) so an IR change is a compile error here, and so no
/// number or schema passes through serde.
fn graph_content(rule_id: &str, graph: &JdmGraph) -> Result<GraphContent, AdmissionError> {
    if !graph.imports.is_empty() {
        return Err(AdmissionError::Import {
            rule_id: rule_id.to_owned(),
        });
    }
    validate_graph_expressions(rule_id, graph)?;
    let mut content = GraphContent {
        nodes: graph
            .nodes
            .iter()
            .map(|node| decision_node(rule_id, node).map(Arc::new))
            .collect::<Result<_, _>>()?,
        edges: graph
            .edges
            .iter()
            .map(decision_edge)
            .map(Arc::new)
            .collect(),
        ..GraphContent::default()
    };
    content.compile();
    Ok(content)
}

fn validate_graph_expressions(rule_id: &str, graph: &JdmGraph) -> Result<(), AdmissionError> {
    let limits = ResourceLimitProfile::norn_limits_v1();
    for (index, node) in graph.nodes.iter().enumerate() {
        let base = format!("graph.nodes[{index}].content");
        match node {
            JdmNode::ExpressionNode { content, .. } => {
                validate_optional_source(
                    rule_id,
                    &format!("{base}.inputField"),
                    content.input_field.as_deref(),
                    false,
                    &limits,
                )?;
                for (expression_index, expression) in content.expressions.iter().enumerate() {
                    validate_source(
                        rule_id,
                        &format!("{base}.expressions[{expression_index}].value"),
                        expression.value.as_str(),
                        false,
                        &limits,
                    )?;
                }
            }
            JdmNode::DecisionTableNode { content, .. } => {
                validate_optional_source(
                    rule_id,
                    &format!("{base}.inputField"),
                    content.input_field.as_deref(),
                    false,
                    &limits,
                )?;
                let mut unary_columns = BTreeMap::new();
                for (column_index, column) in content.inputs.iter().enumerate() {
                    validate_optional_source(
                        rule_id,
                        &format!("{base}.inputs[{column_index}].field"),
                        column.field.as_deref(),
                        false,
                        &limits,
                    )?;
                    unary_columns.insert(
                        column.id.as_str(),
                        column
                            .field
                            .as_deref()
                            .is_some_and(|field| !field.trim().is_empty()),
                    );
                }
                for output in &content.outputs {
                    unary_columns.insert(output.id.as_str(), false);
                }
                for (row_index, row) in content.rules.iter().enumerate() {
                    for (column, cell) in &row.cells {
                        if cell.as_str().trim().is_empty() {
                            continue;
                        }
                        let Some(is_unary) = unary_columns.get(column.as_str()) else {
                            continue;
                        };
                        validate_source(
                            rule_id,
                            &format!("{base}.rules[{row_index}].{column}"),
                            cell.as_str(),
                            *is_unary,
                            &limits,
                        )?;
                    }
                }
            }
            JdmNode::SwitchNode { content, .. } => {
                for (statement_index, statement) in content.statements.iter().enumerate() {
                    if statement.condition.as_str().is_empty() {
                        continue;
                    }
                    validate_source(
                        rule_id,
                        &format!("{base}.statements[{statement_index}].condition"),
                        statement.condition.as_str(),
                        false,
                        &limits,
                    )?;
                }
            }
            JdmNode::DecisionNode { content, .. } => validate_optional_source(
                rule_id,
                &format!("{base}.inputField"),
                content.input_field.as_deref(),
                false,
                &limits,
            )?,
            JdmNode::InputNode { .. }
            | JdmNode::OutputNode { .. }
            | JdmNode::FunctionNode { .. }
            | JdmNode::CustomNode { .. } => {}
        }
    }
    Ok(())
}

fn validate_optional_source(
    rule_id: &str,
    path: &str,
    source: Option<&str>,
    unary: bool,
    limits: &ResourceLimitProfile,
) -> Result<(), AdmissionError> {
    if let Some(source) = source.filter(|source| !source.trim().is_empty()) {
        validate_source(rule_id, path, source, unary, limits)?;
    }
    Ok(())
}

fn validate_source(
    rule_id: &str,
    path: &str,
    source: &str,
    unary: bool,
    limits: &ResourceLimitProfile,
) -> Result<(), AdmissionError> {
    let result = if unary {
        norn_expr::validate_unary_evaluation_source(source, limits)
    } else {
        norn_expr::validate_evaluation_source(source, limits)
    };
    result.map_err(|diagnostics| {
        let diagnostic = diagnostics.first();
        AdmissionError::InvalidExpression {
            rule_id: rule_id.to_owned(),
            path: path.to_owned(),
            code: diagnostic
                .map_or_else(|| "expr.syntax.invalid".to_owned(), |d| d.code.to_owned()),
            message: diagnostic.map_or_else(
                || "expression source is not admitted".to_owned(),
                |d| d.message.clone(),
            ),
        }
    })
}

fn decision_edge(edge: &JdmEdge) -> DecisionEdge {
    DecisionEdge {
        id: Arc::from(edge.id.as_str()),
        source_id: Arc::from(edge.source_id.as_str()),
        target_id: Arc::from(edge.target_id.as_str()),
        source_handle: edge.source_handle.as_deref().map(Arc::from),
    }
}

fn decision_node(rule_id: &str, node: &JdmNode) -> Result<DecisionNode, AdmissionError> {
    let kind = match node {
        JdmNode::InputNode { content, .. } => {
            no_node_schema(rule_id, node.id(), content.as_ref())?;
            DecisionNodeKind::InputNode {
                content: InputNodeContent::default(),
            }
        }
        JdmNode::OutputNode { content, .. } => {
            no_node_schema(rule_id, node.id(), content.as_ref())?;
            DecisionNodeKind::OutputNode {
                content: OutputNodeContent::default(),
            }
        }
        JdmNode::ExpressionNode { content, .. } => DecisionNodeKind::ExpressionNode {
            content: expression_content(content),
        },
        JdmNode::DecisionTableNode { content, .. } => DecisionNodeKind::DecisionTableNode {
            content: table_content(content),
        },
        JdmNode::SwitchNode { content, .. } => DecisionNodeKind::SwitchNode {
            content: switch_content(content),
        },
        JdmNode::DecisionNode { content, .. } => DecisionNodeKind::DecisionNode {
            content: sub_decision_content(content),
        },
        JdmNode::FunctionNode { .. } | JdmNode::CustomNode { .. } => {
            return Err(AdmissionError::NodeNotAdmitted {
                rule_id: rule_id.to_owned(),
                node_id: node.id().to_owned(),
                node_type: node.node_type().as_str(),
            });
        }
    };
    Ok(DecisionNode {
        id: Arc::from(node.id()),
        name: Arc::from(node.name()),
        kind,
    })
}

fn no_node_schema(
    rule_id: &str,
    node_id: &str,
    content: Option<&JdmSchemaContent>,
) -> Result<(), AdmissionError> {
    match content.and_then(|content| content.schema.as_deref()) {
        Some(schema) if !schema.trim().is_empty() => Err(AdmissionError::NodeSchema {
            rule_id: rule_id.to_owned(),
            node_id: node_id.to_owned(),
        }),
        _ => Ok(()),
    }
}

/// `zen-engine` reads an empty or blank `inputField`/`outputPath` (and table
/// `field`/`type`) as absent.
fn non_blank(text: Option<&String>) -> Option<Arc<str>> {
    text.map(String::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(Arc::from)
}

fn transform_attributes(
    input_field: Option<&String>,
    output_path: Option<&String>,
    execution_mode: Option<JdmExecutionMode>,
    pass_through: Option<bool>,
) -> TransformAttributes {
    TransformAttributes {
        input_field: non_blank(input_field),
        output_path: non_blank(output_path),
        execution_mode: match execution_mode {
            Some(JdmExecutionMode::Loop) => TransformExecutionMode::Loop,
            Some(JdmExecutionMode::Single) | None => TransformExecutionMode::Single,
        },
        pass_through: pass_through.unwrap_or(false),
    }
}

fn expression_content(content: &JdmExpressionContent) -> ExpressionNodeContent {
    ExpressionNodeContent {
        expressions: Arc::new(
            content
                .expressions
                .iter()
                .map(|expression| Expression {
                    id: Arc::from(expression.id.as_str()),
                    key: Arc::from(expression.key.as_str()),
                    value: Arc::from(expression.value.as_str()),
                })
                .collect(),
        ),
        transform_attributes: transform_attributes(
            content.input_field.as_ref(),
            content.output_path.as_ref(),
            content.execution_mode,
            content.pass_through,
        ),
    }
}

fn table_content(content: &JdmDecisionTableContent) -> DecisionTableContent {
    DecisionTableContent {
        // As zen's own decoder does: every row member, values trimmed.
        rules: Arc::new(
            content
                .rules
                .iter()
                .map(|row| {
                    let mut cells: Vec<(Arc<str>, Arc<str>)> =
                        vec![(Arc::from("_id"), Arc::from(row.id.trim()))];
                    if let Some(description) = &row.description {
                        cells.push((Arc::from("_description"), Arc::from(description.trim())));
                    }
                    cells.extend(row.cells.iter().map(|(column, cell)| {
                        (Arc::from(column.as_str()), Arc::from(cell.as_str().trim()))
                    }));
                    cells.into_iter().collect()
                })
                .collect(),
        ),
        inputs: Arc::new(
            content
                .inputs
                .iter()
                .map(|input| DecisionTableInputField {
                    id: Arc::from(input.id.as_str()),
                    name: Arc::from(input.name.as_deref().unwrap_or_default()),
                    field: non_blank(input.field.as_ref()),
                })
                .collect(),
        ),
        outputs: Arc::new(
            content
                .outputs
                .iter()
                .map(|output| DecisionTableOutputField {
                    id: Arc::from(output.id.as_str()),
                    name: Arc::from(output.name.as_deref().unwrap_or_default()),
                    field: Arc::from(output.field.as_str()),
                    column_type: non_blank(output.column_type.as_ref()),
                })
                .collect(),
        ),
        hit_policy: match content.hit_policy {
            JdmHitPolicy::First => DecisionTableHitPolicy::First,
            JdmHitPolicy::Collect => DecisionTableHitPolicy::Collect,
        },
        transform_attributes: transform_attributes(
            content.input_field.as_ref(),
            content.output_path.as_ref(),
            content.execution_mode,
            content.pass_through,
        ),
    }
}

fn switch_content(content: &JdmSwitchContent) -> SwitchNodeContent {
    SwitchNodeContent {
        hit_policy: match content.hit_policy {
            Some(JdmHitPolicy::Collect) => SwitchStatementHitPolicy::Collect,
            Some(JdmHitPolicy::First) | None => SwitchStatementHitPolicy::First,
        },
        statements: Arc::new(
            content
                .statements
                .iter()
                .map(|statement| SwitchStatement {
                    id: Arc::from(statement.id.as_str()),
                    condition: Arc::from(statement.condition.as_str()),
                })
                .collect(),
        ),
    }
}

fn sub_decision_content(content: &JdmSubDecisionContent) -> DecisionNodeContent {
    DecisionNodeContent {
        key: Arc::from(content.key.as_str()),
        transform_attributes: transform_attributes(
            content.input_field.as_ref(),
            content.output_path.as_ref(),
            content.execution_mode,
            content.pass_through,
        ),
    }
}

fn invalid_schema(rule_id: &str, path: &str, reason: impl Into<String>) -> AdmissionError {
    AdmissionError::InvalidSchema {
        rule_id: rule_id.to_owned(),
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn parse_schema(rule: &RuleRoot, member: &str) -> Result<Schema, AdmissionError> {
    let document = norn_ir::JsonValue::Object(match member {
        "inputSchema" => rule.input_schema.clone(),
        _ => rule.output_schema.clone(),
    });
    Schema::parse(&document, member)
        .map_err(|violation| invalid_schema(&rule.rule_id, &violation.path, violation.reason))
}

/// A sub-decision's schema: the closed dialect, rooted at a record.
fn closed_record(rule: &RuleRoot, member: &str) -> Result<(), AdmissionError> {
    match parse_schema(rule, member)?.properties() {
        Some(_) => Ok(()),
        None => Err(invalid_schema(
            &rule.rule_id,
            member,
            "must be a closed record",
        )),
    }
}

/// The record members of a decide schema, which must be exactly `members`,
/// all required.
fn contract_members(
    rule: &RuleRoot,
    schema: &Schema,
    member: &str,
    members: &[&str],
) -> Result<BTreeMap<String, Schema>, AdmissionError> {
    let properties = schema
        .properties()
        .ok_or_else(|| invalid_schema(&rule.rule_id, member, "must be a closed record"))?;
    let declared: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = members.iter().copied().collect();
    let required: BTreeSet<&str> = schema
        .required()
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if declared != expected || required != expected {
        return Err(invalid_schema(
            &rule.rule_id,
            member,
            format!("the decide contract requires exactly the members {members:?}, all required"),
        ));
    }
    Ok(properties.clone())
}

fn decide_input(rule: &RuleRoot) -> Result<(Schema, Schema), AdmissionError> {
    let schema = parse_schema(rule, "inputSchema")?;
    let mut members = contract_members(rule, &schema, "inputSchema", &DECIDE_INPUT_MEMBERS)?;
    let mut record = |name: &str| {
        members
            .remove(name)
            .filter(|member| member.properties().is_some())
            .ok_or_else(|| {
                invalid_schema(
                    &rule.rule_id,
                    &format!("inputSchema.properties.{name}"),
                    "must be a closed record",
                )
            })
    };
    let slice = record("slice")?;
    let event = record("event")?;
    Ok((slice, event))
}

fn decide_output(rule: &RuleRoot) -> Result<Schema, AdmissionError> {
    let schema = parse_schema(rule, "outputSchema")?;
    let members = contract_members(rule, &schema, "outputSchema", &DECIDE_OUTPUT_MEMBERS)?;
    for (name, member) in &members {
        match member {
            Schema::Array(items) if items.properties().is_some() => {}
            Schema::Object { .. }
            | Schema::Array(_)
            | Schema::String(_)
            | Schema::Integer
            | Schema::Number
            | Schema::Decimal(_)
            | Schema::Boolean
            | Schema::Null
            | Schema::Json
            | Schema::Nullable(_) => {
                return Err(invalid_schema(
                    &rule.rule_id,
                    &format!("outputSchema.properties.{name}"),
                    "must be a list of closed records",
                ));
            }
        }
    }
    Ok(schema)
}

fn defaults(
    rule: &RuleRoot,
    output: &Schema,
    exact: Option<&RuntimeObject>,
) -> Result<BTreeMap<String, Value>, AdmissionError> {
    let mut defaults = BTreeMap::new();
    for (member, value) in rule.defaults.iter().flatten() {
        let invalid = |reason: String| AdmissionError::InvalidDefault {
            rule_id: rule.rule_id.clone(),
            member: member.clone(),
            reason,
        };
        let value = match (
            value_from_ir(value),
            exact.and_then(|exact| exact.get(member)),
        ) {
            (Ok(value), _) => value,
            // A numeric default, from its exact source text. The decoded
            // `f64` must be what that text decodes to, so the source is this
            // artifact's.
            (Err(_), Some(source)) => {
                if !same_up_to_f64(value, source) {
                    return Err(invalid(
                        "the exact default does not match the decoded artifact".to_owned(),
                    ));
                }
                value_from_runtime(source).map_err(|error| invalid(error.to_string()))?
            }
            (Err(error), None) => return Err(invalid(error.to_string())),
        };
        let schema = output
            .properties()
            .and_then(|properties| properties.get(member))
            .ok_or_else(|| invalid("is not a declared output member".to_owned()))?;
        schema
            .check(&value, member)
            .map_err(|violation| invalid(format!("{}: {}", violation.path, violation.reason)))?;
        defaults.insert(member.clone(), value);
    }
    Ok(defaults)
}

/// Whether the exact `source` decodes to the IR value `decoded`: the same
/// shape, and every number's `f64` (nearest, as JSON decoding gives) equal.
fn same_up_to_f64(decoded: &JsonValue, source: &RuntimeValue) -> bool {
    match (decoded, source) {
        (JsonValue::Null, RuntimeValue::Null) => true,
        (JsonValue::Bool(left), RuntimeValue::Bool(right)) => left == right,
        // IR decoding reads numbers with `serde_json`'s historical parser
        // (`norn_ir::JsonNumber`); a correctly rounded parse is also
        // accepted, so no spelling of this artifact's own number is refused.
        (JsonValue::Number(left), RuntimeValue::Number(right)) => {
            let text = right.canonical();
            let historical = norn_ir::JsonNumber::parse_literal(&text)
                .is_ok_and(|number| number.as_f64().to_bits() == left.to_bits());
            historical
                || text
                    .parse::<f64>()
                    .is_ok_and(|right| right.to_bits() == left.to_bits())
        }
        (JsonValue::String(left), RuntimeValue::String(right)) => left == right,
        (JsonValue::Array(left), RuntimeValue::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_up_to_f64(left, right))
        }
        (JsonValue::Object(left), RuntimeValue::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|((left_key, left), (right_key, right))| {
                        left_key == right_key && same_up_to_f64(left, right)
                    })
        }
        _ => false,
    }
}
