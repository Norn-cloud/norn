//! The Norn profile of `RuleIR` v2 decision graphs (T3-DEC-1, #2669).
//!
//! A `RuleIR` v2 rule is a `GoRules` JDM graph ([`norn_ir::JdmGraph`]) evaluated
//! literally by `zen-engine` under the Norn profile (ruling 2026-09-25 §3, as
//! narrowed by the 2026-09-27 compute-boundary ruling and D33). This module is
//! the profile validator: everything a graph must satisfy before
//! `norn-decisions` (T3-DEC-2) may load it. Each finding has its own stable
//! code ([`codes`]); `fixtures/rule-ir-v2/invalid/` holds one canonical
//! rejection fixture per T2 code, and `fixtures/ir/artifact-set/` the T3
//! cases.
//!
//! # T2: one graph (`validate_jdm_graph`, run by [`crate::typed_roots::validate_rule_ir_v2_payload`])
//!
//! 1. **Admission**, on each node's raw `type` so a rejected node is reported
//!    as such even when its content would not decode: `functionNode`
//!    ([`codes::JDM_FUNCTION_NODE_REJECTED`]; JavaScript is forbidden in the
//!    deterministic core), `customNode` including its `zen-tmpl` template
//!    fields, which are never decoded ([`codes::JDM_CUSTOM_NODE_REJECTED`];
//!    D33 v1), any other `type` ([`codes::JDM_UNKNOWN_NODE_TYPE`]), and a
//!    decision-table or switch `hitPolicy` other than `first` and `collect`
//!    ([`codes::JDM_HIT_POLICY_REJECTED`]; other DMN policies translate or are
//!    rejected at DMN ingress, T3-DEC-4).
//! 2. **Decode** into the closed wire types; a member that does not decode is
//!    `rule.validation.invalid_payload` at its path.
//! 3. **Imports**: `zen-engine` imports name policy documents (dictionaries of
//!    output-column types). Norn has no such artifact — value types come from
//!    `EntityIR` v2 — so no import resolves ([`codes::JDM_UNRESOLVED_IMPORT`]).
//! 4. **Well-formedness** (only when 1–2 are clean, so one defect is not
//!    reported many times): unique node, edge, switch-statement, table-column,
//!    table-row, and expression ids ([`codes::JDM_DUPLICATE_ID`]); exactly one
//!    input and one output node ([`codes::JDM_IO_NODE_COUNT`]); every edge
//!    names existing nodes ([`codes::JDM_DANGLING_EDGE`]), never enters the
//!    input node or leaves the output node, and carries a `sourceHandle`
//!    exactly when it leaves a switch, naming one of its statements
//!    ([`codes::JDM_INVALID_EDGE`]); the graph is acyclic
//!    ([`codes::JDM_CYCLE`]); every node is on a path from the input to the
//!    output ([`codes::JDM_UNREACHABLE_NODE`]).
//! 5. **Switch branches.** `zen-engine` takes a statement with an empty
//!    `condition` unconditionally, so the empty statement is the default
//!    branch. Every switch has exactly one default; under `first` it is the
//!    last statement (anything after it is dead); `isDefault` agrees with the
//!    empty condition ([`codes::JDM_SWITCH_DEFAULT`]). A default only
//!    guarantees that a branch is selected; totality and output-schema
//!    conformance come from T3-DEC-3 typing and T3-DEC-2 output validation.
//!    Every statement has an outgoing edge ([`codes::JDM_SWITCH_UNCONNECTED_BRANCH`]).
//! 6. **Cells.** Every decision-table cell is keyed by a declared column id
//!    ([`codes::JDM_UNKNOWN_COLUMN`]). Every expression site compiles as ZEN
//!    Expression ([`codes::JDM_INVALID_EXPRESSION`]): expression-node values
//!    (a blank key or value is rejected too, because the engine silently
//!    skips it), switch conditions, column `field`s, `inputField`s, output
//!    cells, input cells of a column without a `field` (standard
//!    expressions), and input cells of a column with one (unary tests,
//!    [`norn_expr::check_unary_syntax`]). A blank cell matches anything.
//! 7. **Sub-decisions.** A `decisionNode` `key` is the `artifactId` of a rule
//!    the envelope declares as a required `rule` dependency
//!    ([`codes::JDM_UNRESOLVED_SUB_DECISION`]), never the rule itself
//!    ([`codes::JDM_RECURSIVE_SUB_DECISION`]).
//!
//! # The decision contract (`validate_rule_contract`)
//!
//! A v2 rule declares the contract it implements
//! ([`norn_ir::RuleContractV2`]). Its `inputSchema` and `outputSchema` are
//! closed records — closed schemas whose root is `type: object`
//! ([`codes::INVALID_SCHEMA`]). A `decide` graph is
//! `decide(slice, event) -> { facts, commands, timers }`: its input record has
//! exactly the members [`norn_ir::DECIDE_INPUT_MEMBERS`], each a closed
//! record, and its output record exactly [`norn_ir::DECIDE_OUTPUT_MEMBERS`],
//! each a list of closed records ([`codes::INVALID_DECISION_CONTRACT`]).
//! The `facts` items are the canonical fact change ([`norn_ir::FactChange`],
//! checked by `crate::lifecycle_writes`); the `commands` and `timers` item
//! records are the decision's own. `RuleIR` v2 schemas may give a `string`
//! an `enum` (a non-empty list of distinct strings), which the fact change
//! uses to declare what the decision writes.
//!
//! # T3: the sub-decision closure ([`check_sub_decision_closures`])
//!
//! Over a resolved artifact set, each sub-decision resolves (through its
//! pinned dependency) to a `RuleIR` v2 graph ([`codes::SUB_DECISION_TARGET`]),
//! and each candidate graph's closure is bounded: at most
//! [`MAX_SUB_DECISION_DEPTH`] nested levels and [`MAX_SUB_DECISION_CLOSURE`]
//! distinct graphs ([`codes::SUB_DECISION_BOUND`]). Recursion through other
//! rules is a dependency cycle, which the resolver already rejects.
//!
//! # Expression typing
//!
//! Expression sites are syntax-checked here; type-checking them against the
//! declared slice with `norn_expr::check_expression` is T3-DEC-3 (#2671) and
//! T1-EXPR-5 (#2523). `zen-engine` evaluates every node's expressions with the
//! node's input as the **root scope** (a decide graph's input node reads
//! `slice.…` and `event.…`, and downstream nodes read what upstream nodes
//! wrote), so those checks need a per-node context type, not a single `input`
//! variable.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::jdm::{
    JdmDecisionTableContent, JdmEdge, JdmExpressionContent, JdmGraph, JdmHitPolicy, JdmNode,
    JdmNodeType, JdmSwitchContent,
};
use norn_ir::{
    ArtifactDependency, ArtifactEnvelope, ArtifactKind, DECIDE_INPUT_MEMBERS,
    DECIDE_OUTPUT_MEMBERS, DiagnosticRemediation, DiagnosticSeverity, JsonValue, RuleContractV2,
};

use crate::envelope::{
    ClosedSchemaProfile, ClosedSchemaUnions, invalid_diagnostic, validate_closed_schema,
};
use crate::graph::{cycle_groups, reachable, topological_order};
use crate::{Diagnostic, DiagnosticCode, Span};

/// Stable diagnostic codes of the Norn profile.
pub mod codes {
    /// A JDM `functionNode`: arbitrary JavaScript is outside the Norn profile
    /// (ruling 2026-09-25 §3).
    pub const JDM_FUNCTION_NODE_REJECTED: &str = "rule.validation.jdm_function_node_rejected";
    /// A JDM `customNode` (and its template fields): inline custom nodes are
    /// rejected (ruling 2026-09-27 compute boundary; D33 decided v1).
    pub const JDM_CUSTOM_NODE_REJECTED: &str = "rule.validation.jdm_custom_node_rejected";
    /// A node `type` that is not a JDM node type.
    pub const JDM_UNKNOWN_NODE_TYPE: &str = "rule.validation.jdm_unknown_node_type";
    /// A decision-table hit policy other than `first`/`collect`/`unique`, or a
    /// switch hit policy other than `first`/`collect`.
    pub const JDM_HIT_POLICY_REJECTED: &str = "rule.validation.jdm_hit_policy_rejected";
    /// TABLE-UNIQUE-1: two rows of a `unique` table match one input (with the witness).
    pub const JDM_UNIQUE_OVERLAP: &str = "rule.validation.jdm_unique_overlap";
    /// TABLE-UNIQUE-1: the checker cannot prove a `unique` table's rows disjoint; Unknown is
    /// refused, never admitted.
    pub const JDM_UNIQUE_UNPROVED: &str = "rule.validation.jdm_unique_unproved";
    /// Two nodes, edges, statements, columns, rows, or expressions share an id.
    pub const JDM_DUPLICATE_ID: &str = "rule.validation.jdm_duplicate_id";
    /// Not exactly one input node and one output node.
    pub const JDM_IO_NODE_COUNT: &str = "rule.validation.jdm_io_node_count";
    /// An edge names a node the graph does not have.
    pub const JDM_DANGLING_EDGE: &str = "rule.validation.jdm_dangling_edge";
    /// An edge enters the input node, leaves the output node, or misuses
    /// `sourceHandle`.
    pub const JDM_INVALID_EDGE: &str = "rule.validation.jdm_invalid_edge";
    /// The graph has a cycle.
    pub const JDM_CYCLE: &str = "rule.validation.jdm_cycle";
    /// A node is not on a path from the input node to the output node.
    pub const JDM_UNREACHABLE_NODE: &str = "rule.validation.jdm_unreachable_node";
    /// A switch without exactly one default branch, a default that is not
    /// last under `first`, or an `isDefault` flag that disagrees with it.
    pub const JDM_SWITCH_DEFAULT: &str = "rule.validation.jdm_switch_default";
    /// A switch statement with no outgoing edge.
    pub const JDM_SWITCH_UNCONNECTED_BRANCH: &str = "rule.validation.jdm_switch_unconnected_branch";
    /// A decision-table cell keyed by an undeclared column id.
    pub const JDM_UNKNOWN_COLUMN: &str = "rule.validation.jdm_unknown_column";
    /// An expression site that is blank where the engine would skip it, or
    /// that does not compile as ZEN Expression.
    pub const JDM_INVALID_EXPRESSION: &str = "rule.validation.jdm_invalid_expression";
    /// A graph import: none resolves under the Norn profile.
    pub const JDM_UNRESOLVED_IMPORT: &str = "rule.validation.jdm_unresolved_import";
    /// A sub-decision key that is not a required `rule` dependency.
    pub const JDM_UNRESOLVED_SUB_DECISION: &str = "rule.validation.jdm_unresolved_sub_decision";
    /// A sub-decision that names its own rule.
    pub const JDM_RECURSIVE_SUB_DECISION: &str = "rule.validation.jdm_recursive_sub_decision";
    /// A decide graph whose schemas are not the decision contract.
    pub const INVALID_DECISION_CONTRACT: &str = "rule.validation.invalid_decision_contract";
    /// A `RuleIR` v2 input or output schema that is not a closed schema.
    pub const INVALID_SCHEMA: &str = "rule.validation.invalid_schema";
    /// A sub-decision that resolves to an artifact that is not a `RuleIR` v2
    /// graph.
    pub const SUB_DECISION_TARGET: &str = "artifact_set.decision.sub_decision_target";
    /// A fold declaration outside the supported structural fragment.
    pub const ORDERED_FOLD_INVALID: &str = "rule.validation.ordered_fold_invalid";
    /// Ordered fold execution is not yet supported by this profile.
    pub const ORDERED_FOLD_UNSUPPORTED: &str = "rule.validation.ordered_fold_unsupported";
    /// A sub-decision closure deeper or larger than the profile admits.
    pub const SUB_DECISION_BOUND: &str = "artifact_set.decision.sub_decision_bound";
}

/// The deepest sub-decision nesting a graph may have: a graph that calls a
/// sub-decision that calls another is depth 2.
pub const MAX_SUB_DECISION_DEPTH: usize = 3;

/// The most distinct graphs a graph's sub-decision closure may hold, the graph
/// itself excluded.
pub const MAX_SUB_DECISION_CLOSURE: usize = 16;

const RULE_CODE: &str = "rule.validation.invalid_payload";
const GRAPH: &str = "payload.graph";

/// The single Norn-profile admission predicate (ruling 2026-09-25 §3; D33
/// decided v1): every JDM node type except `functionNode` and `customNode`.
#[must_use]
pub const fn is_norn_profile_admitted(node_type: JdmNodeType) -> bool {
    match node_type {
        JdmNodeType::InputNode
        | JdmNodeType::OutputNode
        | JdmNodeType::ExpressionNode
        | JdmNodeType::DecisionTableNode
        | JdmNodeType::SwitchNode
        | JdmNodeType::DecisionNode => true,
        JdmNodeType::FunctionNode | JdmNodeType::CustomNode => false,
    }
}

/// Stable code and message for a node type outside the Norn profile.
const fn profile_rejection(node_type: JdmNodeType) -> (&'static str, &'static str) {
    match node_type {
        JdmNodeType::CustomNode => (
            codes::JDM_CUSTOM_NODE_REJECTED,
            "customNode is not admitted by the Norn profile: inline custom nodes are rejected (D33); use a pool Function",
        ),
        JdmNodeType::InputNode
        | JdmNodeType::OutputNode
        | JdmNodeType::ExpressionNode
        | JdmNodeType::DecisionTableNode
        | JdmNodeType::SwitchNode
        | JdmNodeType::DecisionNode
        | JdmNodeType::FunctionNode => (
            codes::JDM_FUNCTION_NODE_REJECTED,
            "functionNode is not admitted by the Norn profile: JavaScript is forbidden in the deterministic core",
        ),
    }
}

/// Where one T2 finding goes.
struct Sink<'a> {
    diagnostics: &'a mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&'a str>,
}

impl Sink<'_> {
    fn push(&mut self, code: &str, message: impl Into<String>, path: impl Into<String>) {
        self.diagnostics
            .push(invalid_diagnostic(code, message, path, self.artifact_id));
    }
}

fn node_path(index: usize) -> String {
    format!("{GRAPH}.nodes[{index}]")
}

fn edge_path(index: usize) -> String {
    format!("{GRAPH}.edges[{index}]")
}

/// Check a `RuleIR` v2 `graph` member under the Norn profile (T2). See the
/// module docs. `dependencies` are the envelope's declared dependencies and
/// `artifact_id` the rule's own id.
pub(crate) fn validate_jdm_graph(
    graph: &JsonValue,
    dependencies: &[ArtifactDependency],
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    let mut sink = Sink {
        diagnostics,
        artifact_id,
    };
    let raw_nodes = graph
        .as_object()
        .and_then(|object| object.get("nodes"))
        .and_then(JsonValue::as_array);
    let excluded = screen_nodes(raw_nodes.unwrap_or_default(), &mut sink);

    // Decode the graph without the screened-out nodes, so their content is
    // not reported again.
    let mut admissible = graph.clone();
    if let JsonValue::Object(object) = &mut admissible
        && let Some(JsonValue::Array(nodes)) = object.get_mut("nodes")
    {
        let kept = std::mem::take(nodes)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !excluded.contains(index))
            .map(|(_, node)| node)
            .collect();
        *nodes = kept;
    }
    match serde_json::to_value(&admissible).and_then(serde_json::from_value::<JdmGraph>) {
        Ok(decoded) => {
            check_imports(&decoded, &mut sink);
            if excluded.is_empty() {
                check_graph(&decoded, dependencies, &mut sink);
            }
        }
        Err(_) => locate_decode_failure(graph, &admissible, raw_nodes, &excluded, &mut sink),
    }
}

/// Admission on the raw node `type` and `content.hitPolicy`. Returns the
/// indices of nodes that are reported here and left out of the decode.
fn screen_nodes(raw_nodes: &[JsonValue], sink: &mut Sink<'_>) -> BTreeSet<usize> {
    let mut excluded = BTreeSet::new();
    for (index, node) in raw_nodes.iter().enumerate() {
        let object = node.as_object();
        let Some(raw_type) = object
            .and_then(|object| object.get("type"))
            .and_then(JsonValue::as_string)
        else {
            // A missing or non-string type is a decode finding.
            continue;
        };
        let path = format!("{}.type", node_path(index));
        let Some(node_type) = JdmNodeType::from_wire(raw_type) else {
            excluded.insert(index);
            sink.push(
                codes::JDM_UNKNOWN_NODE_TYPE,
                format!(
                    "node type '{raw_type}' is not a JDM node type; the Norn profile admits inputNode, outputNode, expressionNode, decisionTableNode, switchNode, and decisionNode"
                ),
                path,
            );
            continue;
        };
        if !is_norn_profile_admitted(node_type) {
            excluded.insert(index);
            let (code, message) = profile_rejection(node_type);
            sink.push(code, message, path);
            continue;
        }
        if matches!(
            node_type,
            JdmNodeType::DecisionTableNode | JdmNodeType::SwitchNode
        ) && let Some(policy) = object
            .and_then(|object| object.get("content"))
            .and_then(JsonValue::as_object)
            .and_then(|content| content.get("hitPolicy"))
            .and_then(JsonValue::as_string)
            && !matches!(policy, "first" | "collect")
            && !(policy == "unique" && node_type == JdmNodeType::DecisionTableNode)
        {
            excluded.insert(index);
            sink.push(
                codes::JDM_HIT_POLICY_REJECTED,
                format!(
                    "hit policy '{policy}' is not admitted by the Norn profile: decision tables admit first, collect and unique, switches first and collect; other DMN hit policies are translated or rejected at DMN ingress (T3-DEC-4)"
                ),
                format!("{}.content.hitPolicy", node_path(index)),
            );
        }
    }
    excluded
}

/// TABLE-UNIQUE-1: every `unique` decision table is proved disjoint over its input, or refused.
///
/// The proof is the exhaustive finite analysis of [`crate::table_analysis::analyze_table`] over
/// the rule's closed `inputSchema`, so it applies to a table whose only incoming edge comes from
/// the input node. An overlap is refused with its witness input and matching rows. A table whose
/// context is not the rule input, or whose domain, budget or cells the analysis cannot decide, is
/// Unknown and refused: a unique table is never admitted on trust, so evaluation never sees two
/// matching rows.
pub(crate) fn check_unique_tables(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    use crate::table_analysis::{Unknown, analyze_table};
    let Some(graph) = payload
        .get("graph")
        .and_then(|graph| serde_json::to_value(graph).ok())
        .and_then(|graph| serde_json::from_value::<JdmGraph>(graph).ok())
    else {
        return;
    };
    let mut sink = Sink {
        diagnostics,
        artifact_id,
    };
    let schema = payload
        .get("inputSchema")
        .map(|schema| norn_expr::schema::Schema::parse(schema, "payload.inputSchema"));
    for (index, node) in graph.nodes.iter().enumerate() {
        let JdmNode::DecisionTableNode { id, content, .. } = node else {
            continue;
        };
        if content.hit_policy != JdmHitPolicy::Unique {
            continue;
        }
        let path = format!("{}.content.hitPolicy", node_path(index));
        let incoming: Vec<&JdmEdge> = graph.edges.iter().filter(|e| &e.target_id == id).collect();
        let from_input = matches!(incoming.as_slice(), [edge] if graph.nodes.iter().any(|n| {
            matches!(n, JdmNode::InputNode { .. }) && n.id() == edge.source_id
        }));
        let unproved = |sink: &mut Sink<'_>, reason: String| {
            sink.push(
                codes::JDM_UNIQUE_UNPROVED,
                format!(
                    "unique table '{id}' is Unknown, so it is refused: {reason}; use first for a classification table, or give the table a finite closed input it reads directly"
                ),
                path.clone(),
            );
        };
        let Some(Ok(schema)) = schema.as_ref().filter(|_| from_input) else {
            unproved(
                &mut sink,
                "its input is not the rule's closed input record, so no finite proof covers it"
                    .to_owned(),
            );
            continue;
        };
        match analyze_table(content, schema) {
            Ok(analysis) => {
                if let Some(overlap) = analysis.overlaps.first() {
                    let input = norn_canonical::canonical_json_exact_of(&overlap.input)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_default();
                    sink.push(
                        codes::JDM_UNIQUE_OVERLAP,
                        format!(
                            "unique table '{id}' has rows {:?} matching the same input {input} ({} overlapping inputs of {}); make the rows disjoint, or use first or collect",
                            overlap.matching_rows,
                            analysis.overlaps.len(),
                            analysis.inputs_checked
                        ),
                        path,
                    );
                }
            }
            Err(unknown) => {
                let reason = match unknown {
                    Unknown::UnboundedDomain => "the input has an unbounded member".to_owned(),
                    Unknown::Limit => {
                        "the input domain or row work exceeds the analysis budget".to_owned()
                    }
                    Unknown::LoopExecution => "loop execution reads another input model".to_owned(),
                    Unknown::ArrayOutput => "an array output changes selection".to_owned(),
                    Unknown::SharedSubject => "cells share the engine's mutable subject".to_owned(),
                    Unknown::TemporalIntermediate => {
                        "a temporal column cannot be projected losslessly".to_owned()
                    }
                    Unknown::NonCanonicalIntermediate => {
                        "an intermediate value would be normalized".to_owned()
                    }
                    Unknown::Expression(detail) => {
                        format!("a cell does not evaluate to a Boolean: {detail}")
                    }
                };
                unproved(&mut sink, reason);
            }
        }
    }
}

/// Locate a graph decode failure: per node and per edge, then the graph
/// (without the screened-out nodes) as a whole.
fn locate_decode_failure(
    graph: &JsonValue,
    admissible: &JsonValue,
    raw_nodes: Option<&[JsonValue]>,
    excluded: &BTreeSet<usize>,
    sink: &mut Sink<'_>,
) {
    // `serde` is not a dependency of this crate, so the decode is a macro
    // rather than a function generic over `DeserializeOwned`.
    macro_rules! decode {
        ($ty:ty, $value:expr, $path:expr, $sink:expr) => {{
            let path: String = $path;
            if let Err(error) = serde_json::to_value($value).and_then(serde_json::from_value::<$ty>)
            {
                $sink.push(RULE_CODE, format!("{path} is invalid: {error}"), path);
            }
        }};
    }
    let before = sink.diagnostics.len();
    for (index, node) in raw_nodes.unwrap_or_default().iter().enumerate() {
        if !excluded.contains(&index) {
            decode!(JdmNode, node, node_path(index), sink);
        }
    }
    let edges = graph
        .as_object()
        .and_then(|object| object.get("edges"))
        .and_then(JsonValue::as_array);
    for (index, edge) in edges.unwrap_or_default().iter().enumerate() {
        decode!(JdmEdge, edge, edge_path(index), sink);
    }
    if sink.diagnostics.len() == before {
        decode!(JdmGraph, admissible, GRAPH.to_owned(), sink);
    }
}

fn check_imports(graph: &JdmGraph, sink: &mut Sink<'_>) {
    for (index, import) in graph.imports.iter().enumerate() {
        sink.push(
            codes::JDM_UNRESOLVED_IMPORT,
            format!(
                "import '{import}' does not resolve: the Norn profile has no importable policy documents; declare value types in EntityIR v2"
            ),
            format!("{GRAPH}.imports[{index}]"),
        );
    }
}

/// Well-formedness, switch, cell, and sub-decision checks over a decoded
/// graph whose every node is admitted.
fn check_graph(graph: &JdmGraph, dependencies: &[ArtifactDependency], sink: &mut Sink<'_>) {
    let before = sink.diagnostics.len();
    let mut by_id: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        // The first occurrence of an id is the node its edges resolve to.
        if by_id.contains_key(node.id()) {
            sink.push(
                codes::JDM_DUPLICATE_ID,
                format!("node id '{}' is not unique in the graph", node.id()),
                format!("{}.id", node_path(index)),
            );
        } else {
            by_id.insert(node.id(), index);
        }
    }
    let mut edge_ids = BTreeSet::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        if !edge_ids.insert(edge.id.as_str()) {
            sink.push(
                codes::JDM_DUPLICATE_ID,
                format!("edge id '{}' is not unique in the graph", edge.id),
                format!("{}.id", edge_path(index)),
            );
        }
    }
    let ids_unique = sink.diagnostics.len() == before;

    for (index, node) in graph.nodes.iter().enumerate() {
        check_node(index, node, graph, dependencies, sink);
    }
    let Some(endpoints) = check_io_and_edges(graph, &by_id, sink) else {
        return;
    };
    if ids_unique {
        check_flow(graph, &endpoints, sink);
    }
}

/// The input and output node indices and the graph's successor lists.
struct Endpoints {
    input: usize,
    output: usize,
    successors: BTreeMap<usize, BTreeSet<usize>>,
}

/// Input/output node counts and per-edge checks. Returns the endpoints when
/// the graph has one input, one output, and no dangling or invalid edge.
fn check_io_and_edges(
    graph: &JdmGraph,
    by_id: &BTreeMap<&str, usize>,
    sink: &mut Sink<'_>,
) -> Option<Endpoints> {
    let mut valid = true;
    let io = |node_type: JdmNodeType, label: &str, sink: &mut Sink<'_>| {
        let found: Vec<usize> = graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.node_type() == node_type)
            .map(|(index, _)| index)
            .collect();
        match found.as_slice() {
            [single] => Some(*single),
            [] => {
                sink.push(
                    codes::JDM_IO_NODE_COUNT,
                    format!("the graph has no {label} node; a decision graph has exactly one"),
                    format!("{GRAPH}.nodes"),
                );
                None
            }
            [_, second, ..] => {
                sink.push(
                    codes::JDM_IO_NODE_COUNT,
                    format!(
                        "the graph has {} {label} nodes; a decision graph has exactly one",
                        found.len()
                    ),
                    format!("{}.type", node_path(*second)),
                );
                None
            }
        }
    };
    let input = io(JdmNodeType::InputNode, "input", sink);
    let output = io(JdmNodeType::OutputNode, "output", sink);

    let mut successors: BTreeMap<usize, BTreeSet<usize>> = (0..graph.nodes.len())
        .map(|index| (index, BTreeSet::new()))
        .collect();
    for (index, edge) in graph.edges.iter().enumerate() {
        let source = by_id.get(edge.source_id.as_str()).copied();
        let target = by_id.get(edge.target_id.as_str()).copied();
        for (end, id, member) in [
            (source, &edge.source_id, "sourceId"),
            (target, &edge.target_id, "targetId"),
        ] {
            if end.is_none() {
                valid = false;
                sink.push(
                    codes::JDM_DANGLING_EDGE,
                    format!(
                        "edge '{}' names node '{id}', which the graph does not have",
                        edge.id
                    ),
                    format!("{}.{member}", edge_path(index)),
                );
            }
        }
        let (Some(source), Some(target)) = (source, target) else {
            continue;
        };
        if !check_edge(index, edge, graph, source, target, sink) {
            valid = false;
        }
        if let Some(targets) = successors.get_mut(&source) {
            targets.insert(target);
        }
    }
    match (input, output, valid) {
        (Some(input), Some(output), true) => Some(Endpoints {
            input,
            output,
            successors,
        }),
        _ => None,
    }
}

/// Direction and `sourceHandle` rules of one edge between existing nodes.
fn check_edge(
    index: usize,
    edge: &JdmEdge,
    graph: &JdmGraph,
    source: usize,
    target: usize,
    sink: &mut Sink<'_>,
) -> bool {
    let mut valid = true;
    let source_node = graph.nodes.get(source);
    let target_node = graph.nodes.get(target);
    if source_node.is_some_and(|node| node.node_type() == JdmNodeType::OutputNode) {
        valid = false;
        sink.push(
            codes::JDM_INVALID_EDGE,
            format!("edge '{}' leaves the output node", edge.id),
            format!("{}.sourceId", edge_path(index)),
        );
    }
    if target_node.is_some_and(|node| node.node_type() == JdmNodeType::InputNode) {
        valid = false;
        sink.push(
            codes::JDM_INVALID_EDGE,
            format!("edge '{}' enters the input node", edge.id),
            format!("{}.targetId", edge_path(index)),
        );
    }
    let handle = edge
        .source_handle
        .as_deref()
        .filter(|handle| !handle.is_empty());
    let handle_path = format!("{}.sourceHandle", edge_path(index));
    match source_node {
        Some(JdmNode::SwitchNode { content, .. }) => {
            let known = handle.is_some_and(|handle| {
                content
                    .statements
                    .iter()
                    .any(|statement| statement.id == handle)
            });
            if !known {
                valid = false;
                sink.push(
                    codes::JDM_INVALID_EDGE,
                    format!(
                        "edge '{}' leaves a switch node, so its sourceHandle must name one of the switch's statements",
                        edge.id
                    ),
                    handle_path,
                );
            }
        }
        _ if handle.is_some() => {
            valid = false;
            sink.push(
                codes::JDM_INVALID_EDGE,
                format!(
                    "edge '{}' carries a sourceHandle, but only edges leaving a switch node select a branch",
                    edge.id
                ),
                handle_path,
            );
        }
        _ => {}
    }
    valid
}

/// Acyclicity, then reachability from the input and to the output.
fn check_flow(graph: &JdmGraph, endpoints: &Endpoints, sink: &mut Sink<'_>) {
    let mut predecessors: BTreeMap<usize, BTreeSet<usize>> = endpoints
        .successors
        .keys()
        .map(|&index| (index, BTreeSet::new()))
        .collect();
    for (&source, targets) in &endpoints.successors {
        for target in targets {
            if let Some(sources) = predecessors.get_mut(target) {
                sources.insert(source);
            }
        }
    }
    let (_, blocked) = topological_order(&predecessors, |index| index);
    let cycles = cycle_groups(&endpoints.successors, &blocked);
    for group in &cycles {
        let names: Vec<&str> = group
            .iter()
            .filter_map(|&index| graph.nodes.get(index).map(JdmNode::id))
            .collect();
        if let Some(&anchor) = group.first() {
            sink.push(
                codes::JDM_CYCLE,
                format!("nodes {names:?} form a cycle; a decision graph is acyclic"),
                format!("{}.id", node_path(anchor)),
            );
        }
    }
    if !cycles.is_empty() {
        return;
    }
    let all: BTreeSet<usize> = endpoints.successors.keys().copied().collect();
    let mut from_input = reachable(endpoints.input, &endpoints.successors, &all);
    from_input.insert(endpoints.input);
    let mut to_output = reachable(endpoints.output, &predecessors, &all);
    to_output.insert(endpoints.output);
    for (index, node) in graph.nodes.iter().enumerate() {
        let problem = match (from_input.contains(&index), to_output.contains(&index)) {
            (true, true) => continue,
            (false, _) => "is not reachable from the input node",
            (true, false) => "does not reach the output node",
        };
        sink.push(
            codes::JDM_UNREACHABLE_NODE,
            format!(
                "node '{}' {problem}; every node of a decision graph lies on a path from input to output",
                node.id()
            ),
            format!("{}.id", node_path(index)),
        );
    }
}

/// Per-node content checks.
fn check_node(
    index: usize,
    node: &JdmNode,
    graph: &JdmGraph,
    dependencies: &[ArtifactDependency],
    sink: &mut Sink<'_>,
) {
    let content = format!("{}.content", node_path(index));
    match node {
        JdmNode::ExpressionNode { content: body, .. } => {
            check_expression_node(&content, body, sink);
        }
        JdmNode::DecisionTableNode { content: body, .. } => {
            check_table(&content, body, sink);
        }
        JdmNode::SwitchNode {
            id, content: body, ..
        } => {
            check_switch(id, &content, body, graph, sink);
        }
        JdmNode::DecisionNode { content: body, .. } => {
            check_optional_expression(
                body.input_field.as_deref(),
                &format!("{content}.inputField"),
                sink,
            );
            if let Some(fold) = &body.ordered_fold {
                if body
                    .input_field
                    .as_deref()
                    .is_none_or(|field| field.trim().is_empty())
                {
                    sink.push(
                        codes::ORDERED_FOLD_INVALID,
                        "a fold requires an explicit collection selector",
                        format!("{content}.orderedFold"),
                    );
                }
                if body.execution_mode == Some(norn_ir::jdm::JdmExecutionMode::Loop) {
                    sink.push(
                        codes::ORDERED_FOLD_INVALID,
                        "an ordered fold cannot also use independent JDM loop execution",
                        format!("{content}.orderedFold"),
                    );
                }
                check_ordered_fold(fold, &content, sink);
            }
            check_sub_decision_key(&body.key, &format!("{content}.key"), dependencies, sink);
        }
        JdmNode::InputNode { .. }
        | JdmNode::OutputNode { .. }
        | JdmNode::FunctionNode { .. }
        | JdmNode::CustomNode { .. } => {}
    }
}

fn check_ordered_fold(fold: &norn_ir::jdm::OrderedFold, content: &str, sink: &mut Sink<'_>) {
    use norn_expr::schema::Schema;
    use norn_ir::jdm::FoldOrderKind;
    let path = format!("{content}.orderedFold");
    let invalid = |sink: &mut Sink<'_>, reason: &str| {
        sink.push(codes::ORDERED_FOLD_INVALID, reason, &path);
    };
    if fold.semantics_version != "1.0" || fold.arithmetic.scale > 18 {
        invalid(
            sink,
            "ordered folds require semantics 1.0 and an exact scale of 0..18",
        );
    }
    if fold.max_items == 0
        || fold.max_items > 4096
        || fold.max_accumulator_bytes == 0
        || fold.max_accumulator_bytes > 1_048_576
    {
        invalid(
            sink,
            "fold bounds require 1..4096 items and 1..1048576 accumulator bytes",
        );
    }
    let item = Schema::parse(&JsonValue::Object(fold.item_schema.clone()), &path);
    let state = Schema::parse(&JsonValue::Object(fold.accumulator_schema.clone()), &path);
    if !matches!(state, Ok(Schema::Object { .. })) {
        invalid(sink, "the accumulator must have a closed record schema");
    }
    let Ok(Schema::Object {
        properties,
        required,
    }) = item
    else {
        invalid(sink, "each item must have a closed record schema");
        return;
    };
    let mut keys = BTreeSet::new();
    for key in &fold.order {
        let typed = matches!(
            (key.kind, properties.get(&key.property)),
            (FoldOrderKind::String, Some(Schema::String(_)))
                | (FoldOrderKind::Integer, Some(Schema::Integer))
        );
        if !typed || !required.contains(&key.property) || !keys.insert(&key.property) {
            invalid(
                sink,
                "order keys must be distinct required non-null properties of the declared scalar type",
            );
        }
    }
    if fold.identity.trim().is_empty()
        || !fold
            .order
            .last()
            .is_some_and(|key| key.property == fold.identity && key.kind == FoldOrderKind::String)
    {
        invalid(
            sink,
            "the final order key must be the stable string identity",
        );
    }
    check_standard(fold.initial.as_str(), format!("{path}.initial"), sink);
}

fn check_standard(source: &str, path: String, sink: &mut Sink<'_>) {
    if let Err(error) = norn_expr::Expression::compile(source.trim()) {
        sink.push(
            codes::JDM_INVALID_EXPRESSION,
            format!("'{source}' is not a ZEN expression: {error}"),
            path,
        );
    }
}

fn check_unary(source: &str, path: String, sink: &mut Sink<'_>) {
    if let Err(error) = norn_expr::check_unary_syntax(source.trim()) {
        sink.push(
            codes::JDM_INVALID_EXPRESSION,
            format!("'{source}' is not a ZEN unary test: {error}"),
            path,
        );
    }
}

/// An optional expression member (`inputField`, a column `field`): absent or
/// blank is fine, otherwise it must compile.
fn check_optional_expression(source: Option<&str>, path: &str, sink: &mut Sink<'_>) {
    if let Some(source) = source.filter(|source| !source.trim().is_empty()) {
        check_standard(source, path.to_owned(), sink);
    }
}

fn check_unique<'a>(
    ids: impl IntoIterator<Item = (usize, &'a str)>,
    what: &str,
    path: impl Fn(usize) -> String,
    sink: &mut Sink<'_>,
) {
    let mut seen = BTreeSet::new();
    for (index, id) in ids {
        if !seen.insert(id) {
            sink.push(
                codes::JDM_DUPLICATE_ID,
                format!("{what} id '{id}' is not unique in its node"),
                path(index),
            );
        }
    }
}

fn check_expression_node(content: &str, body: &JdmExpressionContent, sink: &mut Sink<'_>) {
    check_optional_expression(
        body.input_field.as_deref(),
        &format!("{content}.inputField"),
        sink,
    );
    check_unique(
        body.expressions
            .iter()
            .enumerate()
            .map(|(index, expression)| (index, expression.id.as_str())),
        "expression",
        |index| format!("{content}.expressions[{index}].id"),
        sink,
    );
    for (index, expression) in body.expressions.iter().enumerate() {
        let path = format!("{content}.expressions[{index}]");
        if expression.key.trim().is_empty() {
            sink.push(
                codes::JDM_INVALID_EXPRESSION,
                "an expression with a blank key is silently skipped by the engine; give it a key",
                format!("{path}.key"),
            );
        }
        if expression.value.as_str().trim().is_empty() {
            sink.push(
                codes::JDM_INVALID_EXPRESSION,
                "an expression with a blank value is silently skipped by the engine; give it a value",
                format!("{path}.value"),
            );
        } else {
            check_standard(expression.value.as_str(), format!("{path}.value"), sink);
        }
    }
}

fn check_table(content: &str, body: &JdmDecisionTableContent, sink: &mut Sink<'_>) {
    check_optional_expression(
        body.input_field.as_deref(),
        &format!("{content}.inputField"),
        sink,
    );
    // Cells are keyed by column id across inputs and outputs, so the ids are
    // unique across both.
    let mut seen = BTreeSet::new();
    let columns =
        body.inputs
            .iter()
            .enumerate()
            .map(|(index, column)| (format!("{content}.inputs[{index}].id"), column.id.as_str()))
            .chain(body.outputs.iter().enumerate().map(|(index, column)| {
                (format!("{content}.outputs[{index}].id"), column.id.as_str())
            }));
    for (path, id) in columns {
        if !seen.insert(id) {
            sink.push(
                codes::JDM_DUPLICATE_ID,
                format!("column id '{id}' is not unique in its decision table"),
                path,
            );
        }
    }
    for (index, column) in body.inputs.iter().enumerate() {
        check_optional_expression(
            column.field.as_deref(),
            &format!("{content}.inputs[{index}].field"),
            sink,
        );
    }
    check_unique(
        body.rules
            .iter()
            .enumerate()
            .map(|(index, row)| (index, row.id.as_str())),
        "row",
        |index| format!("{content}.rules[{index}]._id"),
        sink,
    );
    // Column id → whether its cells are unary tests (an input column with a
    // field), standard boolean expressions (an input column without one), or
    // output expressions.
    let unary: BTreeMap<&str, bool> = body
        .inputs
        .iter()
        .map(|column| {
            let has_field = column
                .field
                .as_deref()
                .is_some_and(|field| !field.trim().is_empty());
            (column.id.as_str(), has_field)
        })
        .chain(
            body.outputs
                .iter()
                .map(|column| (column.id.as_str(), false)),
        )
        .collect();
    for (row_index, row) in body.rules.iter().enumerate() {
        for (column, cell) in &row.cells {
            let path = format!("{content}.rules[{row_index}].{column}");
            let Some(&is_unary) = unary.get(column.as_str()) else {
                sink.push(
                    codes::JDM_UNKNOWN_COLUMN,
                    format!(
                        "row '{}' has a cell for '{column}', which is not a column of the table",
                        row.id
                    ),
                    path,
                );
                continue;
            };
            if cell.as_str().trim().is_empty() {
                continue;
            }
            if is_unary {
                check_unary(cell.as_str(), path, sink);
            } else {
                check_standard(cell.as_str(), path, sink);
            }
        }
    }
}

fn check_switch(
    id: &str,
    content: &str,
    body: &JdmSwitchContent,
    graph: &JdmGraph,
    sink: &mut Sink<'_>,
) {
    check_unique(
        body.statements
            .iter()
            .enumerate()
            .map(|(index, statement)| (index, statement.id.as_str())),
        "statement",
        |index| format!("{content}.statements[{index}].id"),
        sink,
    );
    let first = !matches!(body.hit_policy, Some(norn_ir::jdm::JdmHitPolicy::Collect));
    let defaults: Vec<usize> = body
        .statements
        .iter()
        .enumerate()
        .filter(|(_, statement)| statement.condition.as_str().is_empty())
        .map(|(index, _)| index)
        .collect();
    match defaults.as_slice() {
        [] => sink.push(
            codes::JDM_SWITCH_DEFAULT,
            format!(
                "switch '{id}' has no default branch (a statement with an empty condition); without one an event can take no branch and the decision is not total"
            ),
            format!("{content}.statements"),
        ),
        [only] if first && Some(*only) != body.statements.len().checked_sub(1) => sink.push(
            codes::JDM_SWITCH_DEFAULT,
            format!(
                "switch '{id}' uses the first hit policy, so its default branch must be the last statement; the statements after it can never be taken"
            ),
            format!("{content}.statements[{only}].condition"),
        ),
        [_] => {}
        [_, second, ..] => sink.push(
            codes::JDM_SWITCH_DEFAULT,
            format!("switch '{id}' has more than one default branch (empty condition)"),
            format!("{content}.statements[{second}].condition"),
        ),
    }
    for (position, statement) in body.statements.iter().enumerate() {
        let path = format!("{content}.statements[{position}]");
        let is_default = statement.condition.as_str().is_empty();
        if let Some(flag) = statement.is_default
            && flag != is_default
        {
            sink.push(
                codes::JDM_SWITCH_DEFAULT,
                "isDefault must agree with the condition: the engine takes a statement with an empty condition as the default branch, and only that one",
                format!("{path}.isDefault"),
            );
        }
        if !is_default {
            check_standard(
                statement.condition.as_str(),
                format!("{path}.condition"),
                sink,
            );
        }
        let connected = graph.edges.iter().any(|edge| {
            edge.source_id == id && edge.source_handle.as_deref() == Some(statement.id.as_str())
        });
        if !connected {
            sink.push(
                codes::JDM_SWITCH_UNCONNECTED_BRANCH,
                format!(
                    "branch '{}' of switch '{id}' has no outgoing edge, so an event that takes it reaches no node",
                    statement.id
                ),
                format!("{path}.id"),
            );
        }
    }
}

fn check_sub_decision_key(
    key: &str,
    path: &str,
    dependencies: &[ArtifactDependency],
    sink: &mut Sink<'_>,
) {
    if sink.artifact_id == Some(key) {
        sink.push(
            codes::JDM_RECURSIVE_SUB_DECISION,
            format!("sub-decision '{key}' names this rule; a decision graph cannot call itself"),
            path.to_owned(),
        );
        return;
    }
    let declared = dependencies.iter().any(|dependency| {
        dependency.artifact_id == key
            && dependency.kind == ArtifactKind::Rule
            && dependency.required
    });
    if !declared {
        sink.push(
            codes::JDM_UNRESOLVED_SUB_DECISION,
            format!(
                "sub-decision '{key}' is not a required rule dependency of this artifact; the key is the artifactId of a RuleIR v2 rule declared in dependencies"
            ),
            path.to_owned(),
        );
    }
}

/// Check the `RuleIR` v2 contract members: `contract` itself, closed input
/// and output schemas, and, for a decide graph, the decision contract. See
/// the module docs.
pub(crate) fn validate_rule_contract(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    check_unique_tables(payload, diagnostics, artifact_id);
    for (member, path) in [
        ("inputSchema", "payload.inputSchema"),
        ("outputSchema", "payload.outputSchema"),
    ] {
        if let Some(schema @ JsonValue::Object(_)) = payload.get(member) {
            validate_closed_schema(
                Some(schema),
                path,
                0,
                diagnostics,
                artifact_id,
                ClosedSchemaProfile {
                    code: codes::INVALID_SCHEMA,
                    subject: "RuleIR",
                    string_enums: true,
                    unions: ClosedSchemaUnions::Nullable,
                },
            );
            // Both contracts exchange records: the root is a closed object,
            // never a scalar or list (a missing or unsupported type is
            // reported by the closed-schema check above).
            if let Some(root_type) = schema
                .as_object()
                .and_then(|object| object.get("type"))
                .and_then(JsonValue::as_string)
                .filter(|root_type| *root_type != "object")
            {
                diagnostics.push(invalid_diagnostic(
                    codes::INVALID_SCHEMA,
                    format!("{path} must be a closed record (type object), not type '{root_type}'"),
                    format!("{path}.type"),
                    artifact_id,
                ));
            }
        }
    }
    let mut sink = Sink {
        diagnostics,
        artifact_id,
    };
    let contract = match payload.get("contract") {
        None => {
            sink.push(
                RULE_CODE,
                "contract is required with a RuleIR v2 graph: decide or sub_decision",
                "payload.contract",
            );
            return;
        }
        Some(value) => {
            match serde_json::to_value(value).and_then(serde_json::from_value::<RuleContractV2>) {
                Ok(contract) => contract,
                Err(error) => {
                    sink.push(
                        RULE_CODE,
                        format!("payload.contract is invalid: {error}"),
                        "payload.contract",
                    );
                    return;
                }
            }
        }
    };
    if contract == RuleContractV2::Decide {
        check_decide_record(
            payload.get("inputSchema"),
            "payload.inputSchema",
            &DECIDE_INPUT_MEMBERS,
            false,
            &mut sink,
        );
        check_decide_record(
            payload.get("outputSchema"),
            "payload.outputSchema",
            &DECIDE_OUTPUT_MEMBERS,
            true,
            &mut sink,
        );
        crate::lifecycle_writes::validate_fact_change_schema(
            payload.get("outputSchema"),
            sink.diagnostics,
            artifact_id,
        );
    }
}

/// A decide graph's input or output record: exactly `members`, all required,
/// each a closed record (`lists`: a list of closed records).
fn check_decide_record(
    schema: Option<&JsonValue>,
    path: &str,
    members: &[&str],
    lists: bool,
    sink: &mut Sink<'_>,
) {
    let Some(properties) = schema
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(JsonValue::as_object)
    else {
        // Not a closed record: reported by the closed-schema check.
        return;
    };
    let shape = if lists {
        "a list of closed records"
    } else {
        "a closed record"
    };
    for member in members {
        if !properties.contains_key(*member) {
            sink.push(
                codes::INVALID_DECISION_CONTRACT,
                format!(
                    "a decide graph's {path} declares exactly {members:?}; '{member}' is missing"
                ),
                format!("{path}.properties"),
            );
        }
    }
    for (member, member_schema) in properties {
        let member_path = format!("{path}.properties.{member}");
        if !members.contains(&member.as_str()) {
            sink.push(
                codes::INVALID_DECISION_CONTRACT,
                format!(
                    "a decide graph's {path} declares exactly {members:?}; '{member}' is not part of the decision contract"
                ),
                member_path,
            );
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
            sink.push(
                codes::INVALID_DECISION_CONTRACT,
                format!("decision contract member '{member}' must be {shape}"),
                member_path,
            );
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
    // A missing member is reported above; a declared one must be required.
    if let Some(member) = members
        .iter()
        .find(|member| properties.contains_key(**member) && !required.contains(**member))
    {
        sink.push(
            codes::INVALID_DECISION_CONTRACT,
            format!(
                "a decide graph's {path} requires every decision contract member; '{member}' is not required"
            ),
            format!("{path}.required"),
        );
    }
}

/// One sub-decision reference of a candidate graph.
struct SubDecisionRef<'a> {
    node: usize,
    key: &'a str,
}

pub(crate) fn decode_graph(artifact: &ArtifactEnvelope) -> Option<JdmGraph> {
    if artifact.kind() != ArtifactKind::Rule {
        return None;
    }
    let payload = artifact.payload.to_json_value();
    let graph = payload.as_object()?.get("graph")?;
    serde_json::to_value(graph)
        .and_then(serde_json::from_value::<JdmGraph>)
        .ok()
}

fn sub_decisions(graph: &JdmGraph) -> Vec<SubDecisionRef<'_>> {
    graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(node, candidate)| match candidate {
            JdmNode::DecisionNode { content, .. } => Some(SubDecisionRef {
                node,
                key: content.key.as_str(),
            }),
            JdmNode::InputNode { .. }
            | JdmNode::OutputNode { .. }
            | JdmNode::ExpressionNode { .. }
            | JdmNode::DecisionTableNode { .. }
            | JdmNode::SwitchNode { .. }
            | JdmNode::FunctionNode { .. }
            | JdmNode::CustomNode { .. } => None,
        })
        .collect()
}

/// The artifacts of a resolved set, candidates first, by `(artifactId,
/// version)`.
struct Pool<'a> {
    by_identity: BTreeMap<(&'a str, &'a str), &'a ArtifactEnvelope>,
}

impl<'a> Pool<'a> {
    fn new(artifacts: &'a [ArtifactEnvelope], base: &'a [ArtifactEnvelope]) -> Self {
        let mut by_identity = BTreeMap::new();
        for artifact in base.iter().chain(artifacts) {
            by_identity.insert(
                (artifact.artifact_id.as_str(), artifact.version.as_str()),
                artifact,
            );
        }
        Self { by_identity }
    }

    /// The artifact a sub-decision key of `owner` resolves to: the pinned
    /// version of its declared `rule` dependency.
    fn target(&self, owner: &ArtifactEnvelope, key: &str) -> Option<&'a ArtifactEnvelope> {
        let dependency = owner.dependencies.iter().find(|dependency| {
            dependency.artifact_id == key && dependency.kind == ArtifactKind::Rule
        })?;
        let version = dependency.version.as_deref()?;
        self.by_identity.get(&(key, version)).copied()
    }
}

/// The sub-decision closure of a graph: its depth and its distinct members.
#[derive(Default, Clone)]
struct Closure<'a> {
    depth: usize,
    members: BTreeSet<(&'a str, &'a str)>,
}

/// Check every candidate's sub-decision references and closure bound over a
/// resolved artifact set (T3). See the module docs.
#[must_use]
pub fn check_sub_decision_closures(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let pool = Pool::new(artifacts, base);
    let mut memo: BTreeMap<(&str, &str), Closure<'_>> = BTreeMap::new();
    let mut fold_graphs = BTreeMap::new();
    let mut findings = Vec::new();
    for artifact in artifacts {
        let Some(graph) = decode_graph(artifact) else {
            continue;
        };
        let mut closure = Closure::default();
        for reference in sub_decisions(&graph) {
            let pointer = format!("/payload/graph/nodes/{}/content/key", reference.node);
            let Some(target) = pool.target(artifact, reference.key) else {
                // Undeclared (T2) or unresolved (the resolver) elsewhere.
                continue;
            };
            if let Some(JdmNode::DecisionNode { content, .. }) = graph.nodes.get(reference.node)
                && let Some(fold) = &content.ordered_fold
                && !fold_helper_matches(fold, target)
            {
                findings.push(set_finding(
                    codes::ORDERED_FOLD_INVALID,
                    "fold helper schemas must equal {accumulator, item, index} -> accumulator"
                        .to_owned(),
                    artifact,
                    &pointer,
                    DiagnosticRemediation::FixDefinition,
                ));
            }
            let Some(nested) = closure_of(target, &pool, &mut memo, &mut BTreeSet::new()) else {
                findings.push(set_finding(
                    codes::SUB_DECISION_TARGET,
                    format!(
                        "sub-decision '{}' resolves to {:?} version {:?}, which is not a RuleIR v2 decision graph",
                        reference.key, target.artifact_id, target.version
                    ),
                    artifact,
                    &pointer,
                    DiagnosticRemediation::ResolveDependency,
                ));
                continue;
            };
            if let Some(JdmNode::DecisionNode { content, .. }) = graph.nodes.get(reference.node)
                && content.ordered_fold.is_some()
                && !std::iter::once(target)
                    .chain(
                        nested
                            .members
                            .iter()
                            .filter_map(|identity| pool.by_identity.get(identity).copied()),
                    )
                    .all(|helper| {
                        *fold_graphs
                            .entry((helper.artifact_id.as_str(), helper.version.as_str()))
                            .or_insert_with(|| {
                                decode_graph(helper).is_some_and(|graph| {
                                    norn_expr::schema::fold_step_graph_supported(&graph)
                                })
                            })
                    })
            {
                findings.push(set_finding(
                    codes::ORDERED_FOLD_INVALID,
                    "nested fold or loop helper is unsupported".to_owned(),
                    artifact,
                    &pointer,
                    DiagnosticRemediation::FixDefinition,
                ));
            }
            let depth = nested.depth.saturating_add(1);
            if depth > MAX_SUB_DECISION_DEPTH {
                findings.push(set_finding(
                    codes::SUB_DECISION_BOUND,
                    format!(
                        "sub-decision '{}' nests {depth} levels deep; the Norn profile admits at most {MAX_SUB_DECISION_DEPTH}",
                        reference.key
                    ),
                    artifact,
                    &pointer,
                    DiagnosticRemediation::FixDefinition,
                ));
            }
            closure.depth = closure.depth.max(depth);
            closure
                .members
                .insert((target.artifact_id.as_str(), target.version.as_str()));
            closure.members.extend(nested.members.iter().copied());
        }
        if closure.members.len() > MAX_SUB_DECISION_CLOSURE {
            findings.push(set_finding(
                codes::SUB_DECISION_BOUND,
                format!(
                    "the sub-decision closure holds {} graphs; the Norn profile admits at most {MAX_SUB_DECISION_CLOSURE}",
                    closure.members.len()
                ),
                artifact,
                "/payload/graph",
                DiagnosticRemediation::FixDefinition,
            ));
        }
    }
    findings
}

/// The closure of `target`, or `None` when it is not a `RuleIR` v2 graph. A
/// reference back onto the current path (impossible in a resolved set, whose
/// candidates are acyclic and whose base is installed) counts as unbounded.
fn closure_of<'a>(
    target: &'a ArtifactEnvelope,
    pool: &Pool<'a>,
    memo: &mut BTreeMap<(&'a str, &'a str), Closure<'a>>,
    path: &mut BTreeSet<(&'a str, &'a str)>,
) -> Option<Closure<'a>> {
    let identity = (target.artifact_id.as_str(), target.version.as_str());
    if let Some(known) = memo.get(&identity) {
        return Some(known.clone());
    }
    let graph = decode_graph(target)?;
    if !path.insert(identity) {
        return Some(Closure {
            depth: usize::MAX,
            members: BTreeSet::new(),
        });
    }
    let mut closure = Closure::default();
    for reference in sub_decisions(&graph) {
        let Some(next) = pool.target(target, reference.key) else {
            continue;
        };
        let nested = closure_of(next, pool, memo, path).unwrap_or_default();
        closure.depth = closure.depth.max(nested.depth.saturating_add(1));
        closure
            .members
            .insert((next.artifact_id.as_str(), next.version.as_str()));
        closure.members.extend(nested.members);
    }
    path.remove(&identity);
    memo.insert(identity, closure.clone());
    Some(closure)
}

/// The sub-decision closure of one rule in an admitted set: every graph its
/// `decisionNode`s reach, keyed as the evaluator resolves them.
#[derive(Debug, Clone)]
pub struct RuleClosure<'a> {
    /// The rule itself.
    pub root: &'a ArtifactEnvelope,
    /// Every reachable sub-decision graph, keyed by its `decisionNode` key
    /// (the dependency's `artifactId`).
    pub sub_decisions: BTreeMap<&'a str, &'a ArtifactEnvelope>,
    /// Norn1 hash of the canonical `{root, members}` record, where `root` and
    /// each member are `{artifactId, version, sha256}` and members are sorted:
    /// the identity of exactly the rule set an evaluation ran.
    pub digest: String,
}

/// Why a rule's sub-decision closure cannot be evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleClosureError {
    /// No rule, or more than one version of it, has this `artifactId`.
    Root {
        /// The requested `artifactId`.
        artifact_id: String,
        /// How many rule artifacts carry it.
        found: usize,
    },
    /// A `decisionNode` key with no pinned `rule` dependency in the set.
    Unresolved {
        /// The rule whose graph names the key.
        owner: String,
        /// The unresolved key.
        key: String,
    },
    /// A key that resolves to an artifact that is not a `RuleIR` v2 graph.
    NotAGraph {
        /// The key.
        key: String,
    },
    /// Two rules in the closure pin different versions of one key, which a
    /// single evaluation cannot hold.
    VersionSkew {
        /// The key.
        key: String,
        /// The pinned versions, sorted.
        versions: Vec<String>,
    },
    /// A key reached again on its own path.
    Cycle {
        /// The key.
        key: String,
    },
    /// Deeper than [`MAX_SUB_DECISION_DEPTH`] or larger than
    /// [`MAX_SUB_DECISION_CLOSURE`].
    Bound {
        /// What exceeded.
        message: String,
    },
    /// An artifact could not be hashed.
    Digest {
        /// The failure.
        message: String,
    },
}

impl std::fmt::Display for RuleClosureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root { artifact_id, found } => write!(
                f,
                "rule '{artifact_id}': expected exactly one rule artifact, found {found}"
            ),
            Self::Unresolved { owner, key } => write!(
                f,
                "rule '{owner}': sub-decision '{key}' has no pinned rule dependency in the set"
            ),
            Self::NotAGraph { key } => {
                write!(f, "sub-decision '{key}' is not a RuleIR v2 decision graph")
            }
            Self::VersionSkew { key, versions } => write!(
                f,
                "sub-decision '{key}' is pinned at versions {versions:?}; one evaluation holds one"
            ),
            Self::Cycle { key } => write!(f, "sub-decision '{key}' recurses"),
            Self::Bound { message } | Self::Digest { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for RuleClosureError {}

/// Resolve the sub-decision closure of the rule `artifact_id` over an
/// admitted set (`artifacts` over its installed `base`), by the same pinned
/// dependency resolution and bounds as [`check_sub_decision_closures`], so
/// admission, offline evaluation and the cell agree on one closure.
///
/// # Errors
/// A [`RuleClosureError`] when the closure is absent, ambiguous, cyclic or
/// over the profile bounds.
pub fn rule_closure<'a>(
    artifacts: &'a [ArtifactEnvelope],
    base: &'a [ArtifactEnvelope],
    artifact_id: &str,
) -> Result<RuleClosure<'a>, RuleClosureError> {
    let roots: Vec<&ArtifactEnvelope> = artifacts
        .iter()
        .filter(|artifact| {
            artifact.artifact_id == artifact_id && artifact.kind() == ArtifactKind::Rule
        })
        .collect();
    let [root] = roots.as_slice() else {
        return Err(RuleClosureError::Root {
            artifact_id: artifact_id.to_owned(),
            found: roots.len(),
        });
    };
    let pool = Pool::new(artifacts, base);
    let mut members: BTreeMap<&'a str, &'a ArtifactEnvelope> = BTreeMap::new();
    // The deepest depth each key has been scheduled at. A key is expanded
    // again only on a strictly deeper path, which the depth bound may
    // refuse, so each of at most MAX_SUB_DECISION_CLOSURE + 1 graphs is
    // decoded at most MAX_SUB_DECISION_DEPTH + 1 times, however many
    // references share it. Depths rise strictly along any path, so a cycle
    // is still walked back to its start (Cycle) or past the bound (Bound).
    let mut scheduled: BTreeMap<&'a str, usize> = BTreeMap::new();
    // (rule, depth of rule, keys on the path to it)
    let mut stack: Vec<(&'a ArtifactEnvelope, usize, Vec<&'a str>)> =
        vec![(root, 0, vec![root.artifact_id.as_str()])];
    while let Some((owner, depth, path)) = stack.pop() {
        let graph = decode_graph(owner).ok_or_else(|| RuleClosureError::NotAGraph {
            key: owner.artifact_id.clone(),
        })?;
        for reference in sub_decisions(&graph) {
            let target =
                pool.target(owner, reference.key)
                    .ok_or_else(|| RuleClosureError::Unresolved {
                        owner: owner.artifact_id.clone(),
                        key: reference.key.to_owned(),
                    })?;
            let key = target.artifact_id.as_str();
            if path.contains(&key) {
                return Err(RuleClosureError::Cycle {
                    key: key.to_owned(),
                });
            }
            let child_depth = depth.saturating_add(1);
            if child_depth > MAX_SUB_DECISION_DEPTH {
                return Err(RuleClosureError::Bound {
                    message: format!(
                        "sub-decision '{key}' nests {child_depth} levels deep; the Norn profile admits at most {MAX_SUB_DECISION_DEPTH}"
                    ),
                });
            }
            match members.get(key) {
                Some(known) if known.version != target.version => {
                    let mut versions = vec![known.version.clone(), target.version.clone()];
                    versions.sort();
                    return Err(RuleClosureError::VersionSkew {
                        key: key.to_owned(),
                        versions,
                    });
                }
                Some(_) => {}
                None => {
                    members.insert(key, target);
                    if members.len() > MAX_SUB_DECISION_CLOSURE {
                        return Err(RuleClosureError::Bound {
                            message: format!(
                                "the sub-decision closure holds more than {MAX_SUB_DECISION_CLOSURE} graphs"
                            ),
                        });
                    }
                }
            }
            if scheduled
                .get(key)
                .is_some_and(|&deepest| deepest >= child_depth)
            {
                continue;
            }
            scheduled.insert(key, child_depth);
            let mut next = path.clone();
            next.push(key);
            stack.push((target, child_depth, next));
        }
    }
    let digest = closure_digest(root, &members)?;
    Ok(RuleClosure {
        root,
        sub_decisions: members,
        digest,
    })
}

fn closure_digest(
    root: &ArtifactEnvelope,
    members: &BTreeMap<&str, &ArtifactEnvelope>,
) -> Result<String, RuleClosureError> {
    let identity = |artifact: &ArtifactEnvelope| {
        crate::receipt::ArtifactRef::of(artifact)
            .map(|reference| {
                serde_json::json!({
                    "artifactId": reference.artifact_id,
                    "version": reference.version,
                    "sha256": reference.sha256,
                })
            })
            .map_err(|error| RuleClosureError::Digest {
                message: error.to_string(),
            })
    };
    let members = members
        .values()
        .map(|artifact| identity(artifact))
        .collect::<Result<Vec<_>, _>>()?;
    let record = serde_json::json!({ "root": identity(root)?, "members": members });
    crate::receipt::hash_serde_json(&record).map_err(|error| RuleClosureError::Digest {
        message: error.to_string(),
    })
}

fn set_finding(
    code: &str,
    message: String,
    artifact: &ArtifactEnvelope,
    pointer: &str,
    remediation: DiagnosticRemediation,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(remediation),
    }
}

fn fold_helper_matches(fold: &norn_ir::jdm::OrderedFold, target: &ArtifactEnvelope) -> bool {
    let Ok((input, output)) = norn_expr::schema::fold_step_schemas(fold) else {
        return false;
    };
    let payload = target.payload.to_json_value();
    let parse = |name| {
        payload
            .as_object()
            .and_then(|fields| fields.get(name))
            .and_then(|value| norn_expr::schema::Schema::parse(value, name).ok())
    };
    parse("inputSchema") == Some(input) && parse("outputSchema") == Some(output)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "unit tests")]

    use norn_ir::ArtifactEnvelope;
    use serde_json::{Value, json};

    use super::{MAX_SUB_DECISION_CLOSURE, check_sub_decision_closures, codes};
    use crate::envelope::validate_artifact;

    const DECIDE: &str =
        include_str!("../../../fixtures/rule-ir-v2/valid/reorder-decision.rule.json");
    const PRIORITY: &str =
        include_str!("../../../fixtures/rule-ir-v2/valid/reorder-priority.rule.json");
    /// Node indices of the decide fixture.
    const ROUTE: usize = 3;

    fn decide() -> Value {
        serde_json::from_str(DECIDE).expect("fixture should parse")
    }

    fn findings(envelope: Value) -> Vec<(String, String)> {
        let envelope: ArtifactEnvelope =
            serde_json::from_value(envelope).expect("envelope should decode");
        validate_artifact(&envelope)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.path.unwrap_or_default()))
            .collect()
    }

    fn only(found: &[(String, String)], code: &str, path: &str) {
        assert_eq!(found, [(code.to_owned(), path.to_owned())], "{found:?}");
    }

    /// TABLE-UNIQUE-1 planted cases over a closed `{tier: a|b|c, vip}` input.
    fn unique_payload(
        conditions: &[&str],
        schema: &Value,
    ) -> std::collections::BTreeMap<String, norn_ir::JsonValue> {
        let rules: Vec<Value> = conditions
            .iter()
            .enumerate()
            .map(|(index, condition)| json!({"_id": format!("row_{index}"), "condition": condition, "out_r": "1"}))
            .collect();
        serde_json::from_value(json!({
            "inputSchema": schema,
            "graph": {"nodes": [
                {"id": "in", "name": "input", "type": "inputNode"},
                {"id": "t", "name": "table", "type": "decisionTableNode", "content": {
                    "hitPolicy": "unique",
                    "inputs": [{"id": "condition", "name": "condition", "type": "expression"}],
                    "outputs": [{"id": "out_r", "name": "r", "field": "r"}],
                    "rules": rules}},
                {"id": "out", "name": "output", "type": "outputNode"}],
             "edges": [
                {"id": "e0", "sourceId": "in", "targetId": "t", "type": "edge"},
                {"id": "e1", "sourceId": "t", "targetId": "out", "type": "edge"}]}
        }))
        .expect("payload")
    }

    fn finite() -> Value {
        json!({"type": "object", "additionalProperties": false, "required": ["tier", "vip"],
            "properties": {"tier": {"type": "string", "enum": ["a", "b", "c"]}, "vip": {"type": "boolean"}}})
    }

    fn unique_codes(
        payload: &std::collections::BTreeMap<String, norn_ir::JsonValue>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        super::check_unique_tables(payload, &mut out, Some("t.rule"));
        out.into_iter().map(|d| d.code).collect()
    }

    #[test]
    fn a_unique_table_is_admitted_only_when_proved_disjoint() {
        // Disjoint and total: admitted.
        let disjoint = unique_payload(&["tier == 'a'", "tier == 'b' or tier == 'c'"], &finite());
        assert!(unique_codes(&disjoint).is_empty());
        // tier a and vip overlap at {tier: a, vip: true}: refused with the witness.
        let overlapping = unique_payload(&["tier == 'a'", "vip"], &finite());
        let mut found = Vec::new();
        super::check_unique_tables(&overlapping, &mut found, Some("t.rule"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, codes::JDM_UNIQUE_OVERLAP);
        assert!(
            found[0].message.contains("row_0") && found[0].message.contains("row_1"),
            "{}",
            found[0].message
        );
        assert!(
            found[0].message.contains("\"tier\":\"a\""),
            "{}",
            found[0].message
        );
        // An unbounded number input cannot be decided: Unknown, refused, never a pass.
        let unbounded = unique_payload(
            &["amount > 100", "amount <= 100"],
            &json!({"type": "object", "additionalProperties": false, "required": ["amount"],
                "properties": {"amount": {"type": "number"}}}),
        );
        assert_eq!(unique_codes(&unbounded), [codes::JDM_UNIQUE_UNPROVED]);
        // A table whose input is not the rule input is Unknown too.
        let mut detached = disjoint;
        if let Some(norn_ir::JsonValue::Object(graph)) = detached.get_mut("graph")
            && let Some(norn_ir::JsonValue::Array(edges)) = graph.get_mut("edges")
        {
            edges.clear();
        }
        assert_eq!(unique_codes(&detached), [codes::JDM_UNIQUE_UNPROVED]);
    }

    #[test]
    fn rule_ir_v2_switch_default_rules() {
        let statements = format!("payload.graph.nodes[{ROUTE}].content.statements");
        // The default is not last under `first`.
        let mut rule = decide();
        rule["payload"]["graph"]["nodes"][ROUTE]["content"]["statements"] = json!([
            { "id": "stocked", "condition": "" },
            { "id": "short", "condition": "short" }
        ]);
        only(
            &findings(rule.clone()),
            codes::JDM_SWITCH_DEFAULT,
            &format!("{statements}[0].condition"),
        );
        // Under `collect` the default may come first: it is always taken.
        rule["payload"]["graph"]["nodes"][ROUTE]["content"]["hitPolicy"] = json!("collect");
        assert_eq!(findings(rule), []);

        // Two defaults.
        let mut rule = decide();
        rule["payload"]["graph"]["nodes"][ROUTE]["content"]["statements"][0]["condition"] =
            json!("");
        let found = findings(rule);
        assert!(
            found.contains(&(
                codes::JDM_SWITCH_DEFAULT.to_owned(),
                format!("{statements}[1].condition")
            )),
            "{found:?}"
        );

        // `isDefault` on a conditional branch.
        let mut rule = decide();
        rule["payload"]["graph"]["nodes"][ROUTE]["content"]["statements"][0]["isDefault"] =
            json!(true);
        only(
            &findings(rule),
            codes::JDM_SWITCH_DEFAULT,
            &format!("{statements}[0].isDefault"),
        );
    }

    #[test]
    fn rule_ir_v2_edge_direction_and_handles() {
        // A switch edge without a handle.
        let mut rule = decide();
        rule["payload"]["graph"]["edges"][3]
            .as_object_mut()
            .expect("edge")
            .remove("sourceHandle");
        let found = findings(rule);
        assert!(
            found.contains(&(
                codes::JDM_INVALID_EDGE.to_owned(),
                "payload.graph.edges[3].sourceHandle".to_owned()
            )),
            "{found:?}"
        );

        // An edge into the input node and one out of the output node.
        let mut rule = decide();
        rule["payload"]["graph"]["edges"]
            .as_array_mut()
            .expect("edges")
            .push(json!({ "id": "e_back", "sourceId": "out", "targetId": "in" }));
        let found = findings(rule);
        for path in [
            "payload.graph.edges[7].sourceId",
            "payload.graph.edges[7].targetId",
        ] {
            assert!(
                found.contains(&(codes::JDM_INVALID_EDGE.to_owned(), path.to_owned())),
                "{path}: {found:?}"
            );
        }

        // No input node at all.
        let mut rule = decide();
        rule["payload"]["graph"]["nodes"][0]["type"] = json!("expressionNode");
        rule["payload"]["graph"]["nodes"][0]["content"] = json!({ "expressions": [] });
        let found = findings(rule);
        assert!(
            found.contains(&(
                codes::JDM_IO_NODE_COUNT.to_owned(),
                "payload.graph.nodes".to_owned()
            )),
            "{found:?}"
        );
    }

    #[test]
    fn rule_ir_v2_blank_expressions_and_contract_members() {
        let mut rule = decide();
        rule["payload"]["graph"]["nodes"][1]["content"]["expressions"][0]["key"] = json!(" ");
        only(
            &findings(rule),
            codes::JDM_INVALID_EXPRESSION,
            "payload.graph.nodes[1].content.expressions[0].key",
        );

        // A decide input record without the event member.
        let mut rule = decide();
        let input = &mut rule["payload"]["inputSchema"];
        input["properties"]
            .as_object_mut()
            .expect("properties")
            .remove("event");
        input["required"] = json!(["slice"]);
        only(
            &findings(rule),
            codes::INVALID_DECISION_CONTRACT,
            "payload.inputSchema.properties",
        );

        // A sub_decision graph has free records; `contract` is closed.
        let mut rule = decide();
        rule["payload"]["contract"] = json!("sub_decision");
        assert_eq!(findings(rule.clone()), []);
        rule["payload"]["contract"] = json!("process");
        only(
            &findings(rule),
            "rule.validation.invalid_payload",
            "payload.contract",
        );
    }

    #[test]
    fn rule_ir_v2_closure_size_is_bounded() {
        // One graph calling MAX + 1 distinct sub-decisions, each a leaf.
        let leaf: Value = serde_json::from_str(PRIORITY).expect("fixture should parse");
        let mut artifacts = Vec::new();
        let mut root = decide();
        let mut dependencies = Vec::new();
        let count = MAX_SUB_DECISION_CLOSURE + 1;
        for index in 0..count {
            let id = format!("retail.leaf_{index}.rule");
            let mut artifact = leaf.clone();
            artifact["artifactId"] = json!(id);
            artifacts.push(artifact);
            dependencies.push(
                json!({ "artifactId": id, "kind": "rule", "version": "0.1.0", "required": true }),
            );
        }
        root["dependencies"] = Value::Array(dependencies);
        let nodes = root["payload"]["graph"]["nodes"]
            .as_array_mut()
            .expect("nodes");
        let template = nodes[2].clone();
        for index in 0..count {
            let mut node = template.clone();
            node["id"] = json!(format!("leaf_{index}"));
            node["content"]["key"] = json!(format!("retail.leaf_{index}.rule"));
            nodes.push(node);
        }
        artifacts.push(root);
        let envelopes: Vec<ArtifactEnvelope> = artifacts
            .into_iter()
            .map(|value| serde_json::from_value(value).expect("envelope should decode"))
            .collect();
        let found: Vec<(String, Option<String>)> = check_sub_decision_closures(&envelopes, &[])
            .into_iter()
            .map(|finding| {
                (
                    finding.code.as_str().to_owned(),
                    finding.span.map(|span| span.pointer),
                )
            })
            .collect();
        assert_eq!(
            found,
            [(
                codes::SUB_DECISION_BOUND.to_owned(),
                Some("/payload/graph".to_owned())
            )]
        );
    }

    /// A helper graph named `id`@`version` that calls each `(key, version)`.
    fn helper(id: &str, version: &str, calls: &[(&str, &str)]) -> ArtifactEnvelope {
        serde_json::from_value(helper_json(id, version, calls)).expect("envelope should decode")
    }

    fn helper_json(id: &str, version: &str, calls: &[(&str, &str)]) -> Value {
        let mut rule: Value = serde_json::from_str(PRIORITY).expect("fixture should parse");
        rule["artifactId"] = json!(id);
        rule["version"] = json!(version);
        rule["payload"]["ruleId"] = json!(id);
        for (index, (key, pinned)) in calls.iter().enumerate() {
            rule["payload"]["graph"]["nodes"]
                .as_array_mut()
                .expect("nodes")
                .push(json!({
                    "id": format!("call_{index}_{key}"), "name": key, "type": "decisionNode",
                    "content": { "key": key, "outputPath": key, "passThrough": true }
                }));
            rule["dependencies"]
                .as_array_mut()
                .expect("dependencies")
                .push(json!({ "artifactId": key, "kind": "rule", "version": pinned, "required": true }));
        }
        rule
    }

    fn keys(closure: &super::RuleClosure<'_>) -> Vec<String> {
        closure
            .sub_decisions
            .iter()
            .map(|(key, artifact)| format!("{key}@{}", artifact.version))
            .collect()
    }

    #[test]
    fn a_rule_chain_resolves_its_whole_closure() {
        let set = [
            helper("demo.pricing", "1", &[("demo.discount", "1")]),
            helper("demo.discount", "1", &[("demo.tier", "1")]),
            helper("demo.tier", "1", &[]),
        ];
        let closure = super::rule_closure(&set, &[], "demo.pricing").expect("chain resolves");
        assert_eq!(keys(&closure), ["demo.discount@1", "demo.tier@1"]);
        // A standalone rule has an empty closure, and its own identity.
        let leaf = super::rule_closure(&set, &[], "demo.tier").expect("leaf resolves");
        assert!(leaf.sub_decisions.is_empty());
        assert_ne!(leaf.digest, closure.digest);
        // The digest is exactly the rule set: stable, and moved by any member.
        assert_eq!(
            super::rule_closure(&set, &[], "demo.pricing")
                .expect("again")
                .digest,
            closure.digest
        );
        let mut edited = set.clone();
        let mut tier = helper_json("demo.tier", "1", &[]);
        tier["name"] = json!("Renamed tier");
        edited[2] = serde_json::from_value(tier).expect("envelope should decode");
        assert_ne!(
            super::rule_closure(&edited, &[], "demo.pricing")
                .expect("edited")
                .digest,
            closure.digest
        );
        // Installed base rules resolve too.
        let base = super::rule_closure(&set[..1], &set[1..], "demo.pricing").expect("base");
        assert_eq!(keys(&base), keys(&closure));
    }

    #[test]
    fn a_diamond_shares_one_member_and_skewed_pins_are_refused() {
        let diamond = [
            helper("demo.top", "1", &[("demo.left", "1"), ("demo.right", "1")]),
            helper("demo.left", "1", &[("demo.leaf", "1")]),
            helper("demo.right", "1", &[("demo.leaf", "1")]),
            helper("demo.leaf", "1", &[]),
        ];
        let closure = super::rule_closure(&diamond, &[], "demo.top").expect("diamond");
        assert_eq!(
            keys(&closure),
            ["demo.leaf@1", "demo.left@1", "demo.right@1"]
        );
        let mut skewed = diamond.to_vec();
        skewed[2] = helper("demo.right", "1", &[("demo.leaf", "2")]);
        skewed.push(helper("demo.leaf", "2", &[]));
        assert_eq!(
            super::rule_closure(&skewed, &[], "demo.top").err(),
            Some(super::RuleClosureError::VersionSkew {
                key: "demo.leaf".to_owned(),
                versions: vec!["1".to_owned(), "2".to_owned()],
            })
        );
    }

    /// Shared references are expanded once per depth, not once per path:
    /// three levels of 100 references each to the next rule would otherwise
    /// decode graphs 1,010,101 times for a three-member closure.
    #[test]
    fn shared_references_are_expanded_once_per_depth() {
        let calls = |key: &'static str| vec![(key, "1"); 100];
        let set = [
            helper("demo.l0", "1", &calls("demo.l1")),
            helper("demo.l1", "1", &calls("demo.l2")),
            helper("demo.l2", "1", &calls("demo.l3")),
            helper("demo.l3", "1", &[]),
        ];
        let started = std::time::Instant::now();
        let closure = super::rule_closure(&set, &[], "demo.l0").expect("fan-out resolves");
        assert_eq!(keys(&closure), ["demo.l1@1", "demo.l2@1", "demo.l3@1"]);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn unresolvable_closures_are_typed_refusals() {
        use super::RuleClosureError as E;
        let missing = [helper("demo.top", "1", &[("demo.gone", "1")])];
        assert_eq!(
            super::rule_closure(&missing, &[], "demo.top").err(),
            Some(E::Unresolved {
                owner: "demo.top".to_owned(),
                key: "demo.gone".to_owned()
            })
        );
        assert_eq!(
            super::rule_closure(&missing, &[], "demo.absent").err(),
            Some(E::Root {
                artifact_id: "demo.absent".to_owned(),
                found: 0
            })
        );
        let cycle = [
            helper("demo.a", "1", &[("demo.b", "1")]),
            helper("demo.b", "1", &[("demo.a", "1")]),
        ];
        assert_eq!(
            super::rule_closure(&cycle, &[], "demo.a").err(),
            Some(E::Cycle {
                key: "demo.a".to_owned()
            })
        );
        let deep = [
            helper("demo.d0", "1", &[("demo.d1", "1")]),
            helper("demo.d1", "1", &[("demo.d2", "1")]),
            helper("demo.d2", "1", &[("demo.d3", "1")]),
            helper("demo.d3", "1", &[("demo.d4", "1")]),
            helper("demo.d4", "1", &[]),
        ];
        assert!(matches!(
            super::rule_closure(&deep, &[], "demo.d0"),
            Err(E::Bound { .. })
        ));
        assert!(super::rule_closure(&deep, &[], "demo.d1").is_ok());
    }
}
