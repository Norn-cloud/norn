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
    JdmDecisionTableContent, JdmEdge, JdmExpressionContent, JdmGraph, JdmNode, JdmNodeType,
    JdmSwitchContent,
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
    /// A decision-table or switch hit policy other than `first`/`collect`.
    pub const JDM_HIT_POLICY_REJECTED: &str = "rule.validation.jdm_hit_policy_rejected";
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
        {
            excluded.insert(index);
            sink.push(
                codes::JDM_HIT_POLICY_REJECTED,
                format!(
                    "hit policy '{policy}' is not admitted by the Norn profile: JDM admits first and collect; other DMN hit policies are translated or rejected at DMN ingress (T3-DEC-4)"
                ),
                format!("{}.content.hitPolicy", node_path(index)),
            );
        }
    }
    excluded
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
            check_sub_decision_key(&body.key, &format!("{content}.key"), dependencies, sink);
        }
        JdmNode::InputNode { .. }
        | JdmNode::OutputNode { .. }
        | JdmNode::FunctionNode { .. }
        | JdmNode::CustomNode { .. } => {}
    }
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
}
