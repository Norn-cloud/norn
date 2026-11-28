//! Flow AST definitions, source-map graph projection, and flow resource limits.
//!
//! Ports `tools/norn-ts/src/builder/flow.ts`, `flow-source-map.ts`, and
//! `flow-limits.ts`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::expr::Expr;
use super::value_schema::{ObjectSchema, ValueSchema};

/// Wire format identifier for a finalized flow source map.
pub const FLOW_SOURCE_MAP_FORMAT: &str = "norn-flow-source-map/1";
/// Wire format identifier for a compiler-emitted draft flow source map.
pub const FLOW_SOURCE_MAP_DRAFT_FORMAT: &str = "norn-flow-source-map-draft/1";

/// Static flow lowering bounds (`FLOW_LIMITS` in `builder/lower-flow.ts`).
pub const FLOW_MAX_RECURS: u32 = 10;
/// Maximum single timer duration in milliseconds (7 days).
pub const FLOW_MAX_TIMER_MILLIS: u64 = 604_800_000;
/// Maximum durable steps (`perform` + `sleep`) per flow.
pub const FLOW_MAX_STEPS: usize = 64;
/// Maximum generated decision-table rows per flow.
pub const FLOW_MAX_ROWS: usize = 1_024;

/// 1-based source location recorded on builder definitions and source maps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Relative path to the authoring file.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// Optional 1-based UTF-16 column number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl SourceLocation {
    /// Serializes this location to JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("file".to_owned(), Value::String(self.file.clone()));
        map.insert("line".to_owned(), Value::Number(self.line.into()));
        if let Some(col) = self.column {
            map.insert("column".to_owned(), Value::Number(col.into()));
        }
        Value::Object(map)
    }
}

/// Authoring flow source-map step classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowSourceMapStepKind {
    /// `Norn.perform`
    Perform,
    /// `Effect.catchTag`
    CatchTag,
    /// `if` branch
    If,
    /// `switch` branch
    Switch,
    /// `Effect.retry`
    Retry,
    /// `Effect.sleep`
    Sleep,
    /// `Effect.all`
    All,
    /// Terminal `return` / `Effect.succeed`
    Succeed,
    /// `Effect.fail`
    Fail,
    /// Scoped `Compose` block
    Compose,
}

impl FlowSourceMapStepKind {
    /// Returns the wire `camelCase` spelling.
    #[must_use]
    pub const fn wire_str(self) -> &'static str {
        match self {
            Self::Perform => "perform",
            Self::CatchTag => "catchTag",
            Self::If => "if",
            Self::Switch => "switch",
            Self::Retry => "retry",
            Self::Sleep => "sleep",
            Self::All => "all",
            Self::Succeed => "succeed",
            Self::Fail => "fail",
            Self::Compose => "compose",
        }
    }
}

/// Compiler-only source-map annotation carried on a flow [`Step`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSourceMapAnnotation {
    /// Stable authoring step identifier (`step_<kind>_...`).
    pub id: String,
    /// Step kind.
    pub kind: FlowSourceMapStepKind,
    /// Source-root-relative 1-based location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<SourceLocation>,
    /// Branch labels for `Route` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_labels: Option<Vec<String>>,
    /// Label for a `Route`'s `otherwise` branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otherwise_label: Option<String>,
}

/// Artifact reference entry in a finalized [`FlowSourceMapFlow`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSourceMapArtifactRef {
    /// Artifact id.
    pub artifact_id: String,
    /// Artifact version.
    pub version: String,
    /// Artifact Norn1 SHA-256 hash.
    pub sha256: String,
}

/// Projected step node in a flow source map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSourceMapStep {
    /// Stable step identifier.
    pub id: String,
    /// Step kind.
    pub kind: FlowSourceMapStepKind,
    /// Optional source location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<SourceLocation>,
    /// Lowered artifact ids referencing this step.
    pub artifact_ids: Vec<String>,
    /// Lowered lifecycle state ids owned by this step.
    pub state_ids: Vec<String>,
    /// Lowered decision-table row ids owned by this step.
    pub decision_row_ids: Vec<String>,
}

/// Edge classification in a flow source map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowSourceMapEdgeKind {
    /// Sequential fallthrough edge.
    Next,
    /// Branch entry edge.
    Branch,
    /// Error handler catch edge.
    Catch,
    /// Retry link edge.
    Retry,
    /// Container membership edge.
    Contains,
}

impl FlowSourceMapEdgeKind {
    /// Returns the wire spelling.
    #[must_use]
    pub const fn wire_str(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Branch => "branch",
            Self::Catch => "catch",
            Self::Retry => "retry",
            Self::Contains => "contains",
        }
    }
}

/// Directed edge between two [`FlowSourceMapStep`] nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSourceMapEdge {
    /// Source step id.
    pub from: String,
    /// Target step id.
    pub to: String,
    /// Edge kind.
    pub kind: FlowSourceMapEdgeKind,
    /// Optional edge label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Source-map entry for one compiled flow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSourceMapFlow {
    /// Flow id.
    pub flow_id: String,
    /// Flow version.
    pub flow_version: String,
    /// Source file path.
    pub source_path: String,
    /// Artifact references (empty in draft maps).
    pub artifact_refs: Vec<FlowSourceMapArtifactRef>,
    /// Projected step nodes.
    pub steps: Vec<FlowSourceMapStep>,
    /// Projected directed edges.
    pub edges: Vec<FlowSourceMapEdge>,
}

/// Draft flow source map emitted by the compiler before Rust supplies Norn1 hashes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSourceMapDraft {
    /// Always [`FLOW_SOURCE_MAP_DRAFT_FORMAT`].
    pub format: String,
    /// Projected flows.
    pub flows: Vec<FlowSourceMapFlow>,
}

/// Intermediate graph of steps and edges built from annotated [`Step`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSourceMapGraph {
    /// Steps in traversal order.
    pub steps: Vec<FlowSourceMapStep>,
    /// Deduplicated edges in traversal order.
    pub edges: Vec<FlowSourceMapEdge>,
}

/// Registered tagged error declaration (`class X extends Data.TaggedError("X")<{ ... }> {}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaggedErrorDecl {
    /// `PascalCase` error tag.
    pub tag: String,
    /// Closed record schema for error fields.
    pub fields: ObjectSchema,
    /// Declaration source location.
    pub at: Option<SourceLocation>,
}

/// Versioned Function handle (`Norn.fn<In, Out, Err>({ id, version })`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnRef {
    /// Function identifier.
    pub id: String,
    /// Function version.
    pub version: String,
    /// Closed input record schema.
    pub input: ObjectSchema,
    /// Closed output schema.
    pub output: ValueSchema,
    /// Declared tagged failure classes.
    pub errors: Vec<TaggedErrorDecl>,
    /// Declaration source location.
    pub at: Option<SourceLocation>,
}

/// Retry delay policy (`Schedule.spaced`, `Schedule.exponential`, `Schedule.max`).
#[derive(Clone, Debug, PartialEq)]
pub enum RetryDelay {
    /// Constant spacing between attempts.
    Spaced {
        /// Milliseconds between attempts.
        millis: f64,
    },
    /// Exponential backoff.
    Exponential {
        /// Initial delay in milliseconds.
        base_millis: f64,
        /// Multiplicative growth factor.
        factor: f64,
    },
    /// Pointwise maximum of inner delays.
    Max(Vec<Self>),
}

/// Finite retry policy (`Schedule.recurs(n)` + optional [`RetryDelay`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetryPolicy {
    /// Retry count (total attempts = `recurs + 1`).
    pub recurs: Option<u32>,
    /// Optional delay schedule between retries.
    pub delay: Option<RetryDelay>,
}

/// Single `Norn.perform` step.
#[derive(Clone, Debug, PartialEq)]
pub struct PerformStep {
    /// Unique `snake_case` step identifier.
    pub step: String,
    /// Target Function handle.
    pub func: FnRef,
    /// Input record expression.
    pub input: Expr,
    /// Optional result binding name.
    pub bind: Option<String>,
    /// Optional display label.
    pub label: Option<String>,
    /// Source location.
    pub at: Option<SourceLocation>,
    /// Optional flow source-map annotation.
    pub source_map: Option<FlowSourceMapAnnotation>,
}

/// One conditional branch of a [`Step::Route`].
#[derive(Clone, Debug, PartialEq)]
pub struct RouteBranch {
    /// Boolean guard expression.
    pub when: Expr,
    /// Steps executed when `when` holds.
    pub then: Vec<Step>,
}

/// Closure-free flow step AST (`Step` in `tools/norn-ts/src/builder/flow.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Durable Function invocation (`Norn.perform`).
    Perform(Box<PerformStep>),
    /// Finite retry around one [`PerformStep`].
    Retry {
        /// Wrapped perform step.
        perform: Box<PerformStep>,
        /// Finite retry policy.
        policy: RetryPolicy,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Durable timer wait (`Effect.sleep`).
    Sleep {
        /// Step identifier.
        step: String,
        /// Wait duration in milliseconds.
        millis: f64,
        /// Optional display label.
        label: Option<String>,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// First-hit conditional routing (`if` / `switch`).
    Route {
        /// Ordered guarded branches.
        branches: Vec<RouteBranch>,
        /// Fallback steps.
        otherwise: Vec<Step>,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Typed failure handler (`Effect.catchTag`).
    CatchTag {
        /// Protected body steps.
        body: Vec<Step>,
        /// Caught error tag.
        tag: String,
        /// Optional handler parameter binding name.
        as_name: Option<String>,
        /// Recovery handler steps.
        handler: Vec<Step>,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Explicit typed failure (`Effect.fail`).
    Fail {
        /// Tagged error declaration.
        error: TaggedErrorDecl,
        /// Failure fields record expression.
        fields: Expr,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Terminal flow completion (`return` / `Effect.succeed`).
    Succeed {
        /// Return expression.
        value: Expr,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Scoped inner sequence returning via [`Step::Value`].
    Compose {
        /// Binding name receiving the inner value.
        name: String,
        /// Closed output schema of the sequence.
        schema: ValueSchema,
        /// Inner steps.
        body: Vec<Step>,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Value return from the nearest [`Step::Compose`].
    Value {
        /// Returned expression.
        value: Expr,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
    /// Ordered children from a literal `Effect.all([...])`.
    All {
        /// Child steps in tuple order.
        steps: Vec<Step>,
        /// Source location.
        at: Option<SourceLocation>,
        /// Optional source-map annotation.
        source_map: Option<FlowSourceMapAnnotation>,
    },
}

impl Step {
    /// Returns the optional [`FlowSourceMapAnnotation`] carried by this step.
    #[must_use]
    pub const fn source_map(&self) -> Option<&FlowSourceMapAnnotation> {
        match self {
            Self::Perform(p) => p.source_map.as_ref(),
            Self::Retry { source_map, .. }
            | Self::Sleep { source_map, .. }
            | Self::Route { source_map, .. }
            | Self::CatchTag { source_map, .. }
            | Self::Fail { source_map, .. }
            | Self::Succeed { source_map, .. }
            | Self::Compose { source_map, .. }
            | Self::Value { source_map, .. }
            | Self::All { source_map, .. } => source_map.as_ref(),
        }
    }

    /// Sets the [`FlowSourceMapAnnotation`] on this step.
    pub fn set_source_map(&mut self, annotation: FlowSourceMapAnnotation) {
        match self {
            Self::Perform(p) => p.source_map = Some(annotation),
            Self::Retry { source_map, .. }
            | Self::Sleep { source_map, .. }
            | Self::Route { source_map, .. }
            | Self::CatchTag { source_map, .. }
            | Self::Fail { source_map, .. }
            | Self::Succeed { source_map, .. }
            | Self::Compose { source_map, .. }
            | Self::Value { source_map, .. }
            | Self::All { source_map, .. } => *source_map = Some(annotation),
        }
    }
}

/// Flow metadata (`id`, `version`, optional display `title`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowMeta {
    /// Dotted lower-case flow id (`namespace.name`).
    pub id: String,
    /// Flow version string.
    pub version: String,
    /// Optional display-only title.
    pub title: Option<String>,
}

/// Closure-free compiled flow definition (`FlowDef`).
#[derive(Clone, Debug, PartialEq)]
pub struct FlowDef {
    /// Flow metadata.
    pub meta: FlowMeta,
    /// Root step sequence.
    pub body: Vec<Step>,
    /// Source location of `Norn.flow(...)`.
    pub at: Option<SourceLocation>,
}

/// Computes the retry delay in milliseconds after failed attempt `attempt` (1-based).
#[must_use]
pub(crate) fn retry_delay(policy: &RetryPolicy, attempt: u32) -> f64 {
    match &policy.delay {
        None => 0.0,
        Some(delay) => retry_delay_inner(delay, attempt),
    }
}

fn retry_delay_inner(delay: &RetryDelay, attempt: u32) -> f64 {
    match delay {
        RetryDelay::Spaced { millis } => *millis,
        RetryDelay::Max(delays) => delays
            .iter()
            .map(|item| retry_delay_inner(item, attempt))
            .fold(0.0_f64, f64::max),
        RetryDelay::Exponential {
            base_millis,
            factor,
        } => {
            let exp = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
            base_millis * factor.powi(exp)
        }
    }
}

/// Builds the flow source-map graph from annotated steps (`flowSourceMapGraph`).
#[must_use]
pub(crate) fn flow_source_map_graph(steps: &[Step]) -> FlowSourceMapGraph {
    let mut builder = GraphBuilder::default();
    let _ = builder.visit_sequence(steps);
    FlowSourceMapGraph {
        steps: builder
            .order
            .into_iter()
            .filter_map(|id| builder.nodes.remove(&id))
            .collect(),
        edges: builder.edges,
    }
}

#[derive(Default)]
struct GraphBuilder {
    nodes: BTreeMap<String, FlowSourceMapStep>,
    order: Vec<String>,
    edges: Vec<FlowSourceMapEdge>,
    edge_keys: BTreeSet<String>,
}

struct Shape {
    first: Option<String>,
    tails: Vec<String>,
}

impl GraphBuilder {
    fn add_edge(&mut self, edge: FlowSourceMapEdge) {
        let key = format!(
            "{}\0{}\0{}\0{}",
            edge.from,
            edge.to,
            edge.kind.wire_str(),
            edge.label.as_deref().unwrap_or("")
        );
        if self.edge_keys.insert(key) {
            self.edges.push(edge);
        }
    }

    fn contain(&mut self, parent: Option<&str>, values: &[Step], label: &str) {
        let Some(from) = parent else {
            return;
        };
        for value in values {
            if let Some(child) = value.source_map() {
                self.add_edge(FlowSourceMapEdge {
                    from: from.to_owned(),
                    to: child.id.clone(),
                    kind: FlowSourceMapEdgeKind::Contains,
                    label: Some(label.to_owned()),
                });
            }
        }
    }

    fn route_branch(&mut self, parent: Option<&str>, values: &[Step], label: &str) -> Shape {
        let shape = self.visit_sequence(values);
        if let (Some(from), Some(first)) = (parent, shape.first.as_ref()) {
            self.add_edge(FlowSourceMapEdge {
                from: from.to_owned(),
                to: first.clone(),
                kind: FlowSourceMapEdgeKind::Branch,
                label: Some(label.to_owned()),
            });
        }
        self.contain(parent, values, label);
        shape
    }

    fn visit_sequence(&mut self, sequence: &[Step]) -> Shape {
        let mut first: Option<String> = None;
        let mut previous_tails: Vec<String> = Vec::new();
        for step in sequence {
            let id = step.source_map().map(|ann| ann.id.clone());
            if let Some(ann) = step.source_map() {
                self.record_node(ann, &mut first, &previous_tails);
            }
            previous_tails = self.visit_step(step, id, previous_tails);
        }
        Shape {
            first,
            tails: previous_tails,
        }
    }

    fn record_node(
        &mut self,
        ann: &FlowSourceMapAnnotation,
        first: &mut Option<String>,
        previous_tails: &[String],
    ) {
        if !self.nodes.contains_key(&ann.id) {
            self.order.push(ann.id.clone());
            self.nodes.insert(
                ann.id.clone(),
                FlowSourceMapStep {
                    id: ann.id.clone(),
                    kind: ann.kind,
                    at: ann.at.clone(),
                    artifact_ids: Vec::new(),
                    state_ids: Vec::new(),
                    decision_row_ids: Vec::new(),
                },
            );
        }
        if first.is_none() {
            *first = Some(ann.id.clone());
        }
        for from in previous_tails {
            self.add_edge(FlowSourceMapEdge {
                from: from.clone(),
                to: ann.id.clone(),
                kind: FlowSourceMapEdgeKind::Next,
                label: None,
            });
        }
    }

    fn visit_step(
        &mut self,
        step: &Step,
        id: Option<String>,
        previous_tails: Vec<String>,
    ) -> Vec<String> {
        let id_ref = id.as_deref();
        match step {
            Step::All { steps: values, .. } => self.visit_container(id_ref, id, values, "steps"),
            Step::Compose { body: values, .. } => self.visit_container(id_ref, id, values, "body"),
            Step::Retry { perform, .. } => self.visit_retry(id_ref, id, perform),
            Step::Route {
                branches,
                otherwise,
                source_map,
                ..
            } => self.visit_route(id_ref, id, branches, otherwise, source_map.as_ref()),
            Step::CatchTag {
                body, tag, handler, ..
            } => self.visit_catch_tag(id_ref, id, body, tag, handler),
            Step::Succeed { .. } | Step::Fail { .. } => Vec::new(),
            Step::Perform(_) | Step::Sleep { .. } | Step::Value { .. } => match id {
                Some(own) => vec![own],
                None => previous_tails,
            },
        }
    }

    fn visit_container(
        &mut self,
        id_ref: Option<&str>,
        id: Option<String>,
        values: &[Step],
        label: &str,
    ) -> Vec<String> {
        let shape = self.visit_sequence(values);
        self.contain(id_ref, values, label);
        if shape.first.is_some() {
            shape.tails
        } else {
            id.into_iter().collect()
        }
    }

    fn visit_retry(
        &mut self,
        id_ref: Option<&str>,
        id: Option<String>,
        perform: &PerformStep,
    ) -> Vec<String> {
        let Some(perf_ann) = perform.source_map.as_ref() else {
            return id.into_iter().collect();
        };
        let perf_id = perf_ann.id.clone();
        let shape = self.visit_sequence(&[Step::Perform(Box::new(perform.clone()))]);
        if let Some(from) = id_ref {
            self.add_edge(FlowSourceMapEdge {
                from: from.to_owned(),
                to: perf_id.clone(),
                kind: FlowSourceMapEdgeKind::Contains,
                label: Some("perform".to_owned()),
            });
            self.add_edge(FlowSourceMapEdge {
                from: from.to_owned(),
                to: perf_id,
                kind: FlowSourceMapEdgeKind::Retry,
                label: None,
            });
        }
        shape.tails
    }

    fn visit_route(
        &mut self,
        id_ref: Option<&str>,
        id: Option<String>,
        branches: &[RouteBranch],
        otherwise: &[Step],
        source_map: Option<&FlowSourceMapAnnotation>,
    ) -> Vec<String> {
        let mut branch_shapes = Vec::with_capacity(branches.len().saturating_add(1));
        for (idx, branch) in branches.iter().enumerate() {
            let label = source_map
                .and_then(|sm| sm.branch_labels.as_ref())
                .and_then(|labels| labels.get(idx))
                .cloned()
                .unwrap_or_else(|| format!("branch-{}", idx.saturating_add(1)));
            branch_shapes.push(self.route_branch(id_ref, &branch.then, &label));
        }
        let otherwise_label = source_map
            .and_then(|sm| sm.otherwise_label.as_deref())
            .unwrap_or("otherwise");
        branch_shapes.push(self.route_branch(id_ref, otherwise, otherwise_label));
        let mut tails = Vec::new();
        for shape in branch_shapes {
            if shape.first.is_some() {
                tails.extend(shape.tails);
            } else if let Some(ref own) = id {
                tails.push(own.clone());
            }
        }
        tails
    }

    fn visit_catch_tag(
        &mut self,
        id_ref: Option<&str>,
        id: Option<String>,
        body: &[Step],
        tag: &str,
        handler: &[Step],
    ) -> Vec<String> {
        let body_shape = self.visit_sequence(body);
        let handler_shape = self.visit_sequence(handler);
        self.contain(id_ref, body, "body");
        self.contain(id_ref, handler, "handler");
        if let (Some(from), Some(h_first)) = (id_ref, handler_shape.first.as_ref()) {
            self.add_edge(FlowSourceMapEdge {
                from: from.to_owned(),
                to: h_first.clone(),
                kind: FlowSourceMapEdgeKind::Catch,
                label: Some(tag.to_owned()),
            });
        }
        let mut tails = if body_shape.first.is_some() {
            body_shape.tails
        } else {
            id.into_iter().collect()
        };
        if handler_shape.first.is_some() {
            tails.extend(handler_shape.tails);
        }
        tails
    }
}

pub(crate) use super::flow_limits::list_length;
