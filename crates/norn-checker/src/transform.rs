//! `ViewIR` v1 typed transforms (VIEW-TYPE-1 … VIEW-ORDER-1, Core 03).
//!
//! A `derived` artifact whose `calc` is `relational` is a closed operator
//! plan over declared inputs. Admission types every node against the
//! effective catalog and the ZEN type environment of the same artifact pool:
//! each node has a closed row schema (column → type, additivity, identity,
//! exact origin, lineage, markings, fan-out) and a row identity (grain).
//! Joins exist only along declared links or declared keys (VIEW-JOIN-1);
//! aggregates reuse the metric layer's additivity refusals (VIEW-AGG-1);
//! windows and dedupes need a total order (VIEW-ORDER-1). The admitted
//! [`TransformPlan`] is what every engine evaluates; [`eval`] is the exact
//! reference interpreter. Admission grants no read authority.

pub mod eval;

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EnvOptions, Expression, InferredType, Type, ZenTypeEnv};
use norn_ir::{
    AggregateFn, ArtifactEnvelope, ArtifactKind, ArtifactPayload, CalcExpr, DedupeKeep, JoinKind,
    JoinOn, Normalization, OrderKey, PropertyAdditivity, PropertyTypeName, ResourceLimitProfile,
    SortDirection, ViewInputKind, ViewIrPayload, ViewMaintenance, ViewNode, WindowFn,
};

use norn_ir::metric::TimeGrain;

use crate::entity_instance::{EffectiveCatalog, EffectiveKind};
use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

/// Stable codes of `ViewIR` v1 admission. The metric layer's additivity codes
/// are reused verbatim where the law is the same (VIEW-AGG-1).
pub mod codes {
    /// A reference, name, cycle or declaration the plan grammar does not admit.
    pub const INVALID_PLAN: &str = "derived.validation.invalid_plan";
    /// A column that the referenced node does not have.
    pub const UNKNOWN_COLUMN: &str = "derived.validation.unknown_column";
    /// An expression or column whose type the operator does not accept.
    pub const TYPE_MISMATCH: &str = "derived.validation.type_mismatch";
    /// An operator that would lose the input's row identity.
    pub const GRAIN_DROPPED: &str = "derived.validation.grain_dropped";
    /// A join that is neither along a declared link nor on a declared key.
    pub const JOIN_UNDECLARED: &str = "derived.validation.join_undeclared";
    /// Union inputs that are not the same shape.
    pub const UNION_SHAPE: &str = "derived.validation.union_shape";
    /// Two output columns with one name.
    pub const COLUMN_CONFLICT: &str = "derived.validation.column_conflict";
    /// A window or dedupe whose order is not total over the input grain.
    pub const ORDER_NOT_TOTAL: &str = "derived.validation.order_not_total";
    /// A relational view with a hand-written output schema.
    pub const OUTPUT_SCHEMA_MISMATCH: &str = "derived.validation.output_schema_mismatch";
    /// Summing a measure that a join repeats (QUERY-FANOUT).
    pub const AMBIGUOUS_GRAIN: &str = "core.compute.ambiguous_grain";
}

use crate::metric::codes as metric_codes;

/// The ZEN root every transform expression reads its row from: `input.<column>`.
pub const ROW_ROOT: &str = "input";

/// Rows any node of the exact engine may hold (VIEW-ENGINE-1: the exact
/// engine is the small-data reference; bulk work lowers to `DuckDB`).
pub const MAX_ROWS: usize = 100_000;

/// One stored property of one input: a column's origin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    /// The input alias.
    pub alias: String,
    /// The stored property id, or `id` for the object identity.
    pub property: String,
}

impl Origin {
    fn new(alias: &str, property: &str) -> Self {
        Self {
            alias: alias.to_owned(),
            property: property.to_owned(),
        }
    }
}

/// One typed column of a node's row (VIEW-TYPE-1).
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// The column name, read as `input.<name>`.
    pub name: String,
    /// The value type; [`Type::Optional`] when the value may be null.
    pub ty: Type,
    /// The aggregation contract (METRIC-ADDITIVITY-1).
    pub additivity: PropertyAdditivity,
    /// The object type whose ids this column holds.
    pub identity: Option<String>,
    /// The stored property this column is an unchanged copy of.
    pub exact: Option<Origin>,
    /// Every stored property the value derives from (column lineage).
    pub origins: BTreeSet<Origin>,
    /// A join repeats the value across rows: summing it would count it twice.
    pub fanned: bool,
    /// The stored property holding this money value's per-row currency.
    pub currency: Option<Origin>,
    /// Effective markings of every origin (VIEW-MARK-1).
    pub markings: BTreeSet<String>,
}

impl Column {
    fn derived(name: &str, ty: Type, additivity: PropertyAdditivity) -> Self {
        Self {
            name: name.to_owned(),
            ty,
            additivity,
            identity: None,
            exact: None,
            origins: BTreeSet::new(),
            fanned: false,
            currency: None,
            markings: BTreeSet::new(),
        }
    }

    fn renamed(&self, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..self.clone()
        }
    }

    fn nullable(mut self) -> Self {
        if !matches!(self.ty, Type::Optional(_)) {
            self.ty = Type::Optional(Box::new(self.ty));
        }
        self
    }
}

/// A node's closed row schema and row identity (VIEW-GRAIN-1).
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSchema {
    /// Columns, in output order.
    pub columns: Vec<Column>,
    /// The columns whose values identify one row; empty means at most one row.
    pub grain: Vec<String>,
}

impl NodeSchema {
    /// The column named `name`.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| column.name == name)
    }

    fn fields(&self) -> BTreeMap<String, Type> {
        self.columns
            .iter()
            .map(|column| (column.name.clone(), column.ty.clone()))
            .collect()
    }
}

/// One declared input that a scan reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanInput {
    /// The input alias.
    pub alias: String,
    /// The pinned artifact.
    pub artifact_id: String,
    /// Its exact version.
    pub version: String,
    /// The object type the artifact declares.
    pub object_type: String,
    /// Stored properties the scan exposes, in id order.
    pub properties: Vec<String>,
}

/// An admitted ZEN expression with the environment and expected type it was
/// checked under; evaluation reuses exactly both.
#[derive(Debug, Clone)]
pub struct TypedExpression {
    /// The compiled source.
    pub expression: Expression,
    /// The row environment (`input` bound to the node's columns).
    pub env: ZenTypeEnv,
    /// The expected result type at admission.
    pub expected: Type,
}

/// How one projected column is computed.
#[derive(Debug, Clone)]
pub enum Projection {
    /// An unchanged input column.
    Column(String),
    /// A computed value.
    Expression(TypedExpression),
}

/// The admitted form of one operator.
#[derive(Debug, Clone)]
pub enum Step {
    /// Rows of one input.
    Scan {
        /// The input alias.
        alias: String,
    },
    /// Rows whose predicate is `true`.
    Filter {
        /// The input node.
        input: String,
        /// The Boolean predicate.
        predicate: TypedExpression,
    },
    /// Exactly these columns.
    Project {
        /// The input node.
        input: String,
        /// Output column → how it is computed.
        columns: Vec<(String, Projection)>,
    },
    /// A declared join.
    Join {
        /// The left node.
        left: String,
        /// The right node.
        right: String,
        /// How rows pair.
        on: AdmittedJoin,
        /// Inner or left.
        kind: JoinKind,
        /// Right column → output name.
        renames: Vec<(String, String)>,
    },
    /// Groups and measures.
    Aggregate {
        /// The input node.
        input: String,
        /// Group column, optional calendar grain.
        group_by: Vec<(String, Option<TimeGrain>, Type)>,
        /// Output name, function, measured column and its type.
        measures: Vec<(String, AggregateFn, Option<String>, Option<Type>)>,
    },
    /// Window columns.
    Window {
        /// The input node.
        input: String,
        /// Partition columns.
        partition_by: Vec<String>,
        /// Total order keys with their column types.
        order_by: Vec<(String, SortDirection, Type)>,
        /// Output name, function, read column and offset.
        columns: Vec<(String, WindowFn, Option<String>, u32)>,
    },
    /// Branch-tagged concatenation.
    Union {
        /// The input nodes, in order.
        inputs: Vec<String>,
        /// The branch column.
        branch: String,
    },
    /// One row per key.
    Dedupe {
        /// The input node.
        input: String,
        /// Key columns.
        key: Vec<String>,
        /// Order keys with their column types.
        order_by: Vec<(String, SortDirection, Type)>,
        /// Which row is kept.
        keep: DedupeKeep,
    },
}

/// An admitted join relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmittedJoin {
    /// Along `link` of the left column's object type; the engine reads the
    /// link edges of the scan `alias` that owns the column.
    Link {
        /// The left column holding source ids.
        column: String,
        /// The input alias whose objects declare the link.
        alias: String,
        /// The link role.
        link: String,
        /// The right identity column.
        right: String,
    },
    /// Left columns equal the right columns under per-component normalization.
    Key {
        /// Left column, right column, normalization steps.
        pairs: Vec<(String, String, Vec<Normalization>)>,
    },
}

/// One admitted node: its operator and its schema.
#[derive(Debug, Clone)]
pub struct AdmittedNode {
    /// The operator.
    pub step: Step,
    /// The output schema.
    pub schema: NodeSchema,
}

/// A checked `ViewIR` v1 plan (VIEW-TYPE-1). Construction is through [`admit`];
/// it is not execution evidence or a read grant.
#[derive(Debug, Clone)]
pub struct TransformPlan {
    /// The view artifact.
    pub artifact_id: String,
    /// Its exact version.
    pub version: String,
    /// The view id.
    pub view_id: String,
    /// Inputs by alias.
    pub inputs: BTreeMap<String, ScanInput>,
    /// Node ids in evaluation order (every input before its consumers).
    pub order: Vec<String>,
    /// Admitted nodes by id.
    pub nodes: BTreeMap<String, AdmittedNode>,
    /// The output node.
    pub output: String,
    /// `(alias, link)` edges the engine must read for link joins.
    pub links: BTreeSet<(String, String)>,
    /// Fuel for one expression evaluation (its static cost fits it).
    pub fuel_units: u64,
    /// The output node's schema.
    schema: NodeSchema,
}

impl TransformPlan {
    /// The schema of the view's rows.
    #[must_use]
    pub const fn output_schema(&self) -> &NodeSchema {
        &self.schema
    }
}

fn finding(code: &str, message: impl Into<String>, pointer: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(pointer)),
        artifact_id: None,
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// Decode the strict `ViewIR` root of a `derived` artifact.
fn view_root(artifact: &ArtifactEnvelope) -> Option<ViewIrPayload> {
    let ArtifactPayload::Derived(payload) = &artifact.payload else {
        return None;
    };
    serde_json::to_value(payload.to_json_value())
        .and_then(serde_json::from_value::<ViewIrPayload>)
        .ok()
}

/// Whether the artifact is a `ViewIR` v1 relational view.
#[must_use]
pub fn is_relational(artifact: &ArtifactEnvelope) -> bool {
    view_root(artifact).is_some_and(|root| matches!(root.calc, CalcExpr::Relational(_)))
}

/// Whole-set admission of every relational view among `artifacts`, against
/// `base` and `artifacts` together.
#[must_use]
pub fn check_transform_set(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Vec<Diagnostic> {
    if !artifacts.iter().any(is_relational) {
        return Vec::new();
    }
    let pool: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
    let context = Context::new(&pool, registry);
    artifacts
        .iter()
        .filter(|artifact| is_relational(artifact))
        .flat_map(|artifact| match context.admit(artifact) {
            Ok(_plan) => Vec::new(),
            Err(findings) => findings
                .into_iter()
                .map(|mut finding| {
                    finding.artifact_id = Some(artifact.artifact_id.clone());
                    finding
                })
                .collect(),
        })
        .collect()
}

/// Admit one relational view against the artifact pool it is installed with.
///
/// # Errors
/// Every finding, with stable codes; an artifact that is not a relational
/// view refuses with [`codes::INVALID_PLAN`].
pub fn admit(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<TransformPlan, Vec<Diagnostic>> {
    Context::new(pool, registry).admit(artifact)
}

struct Context<'a> {
    pool: &'a [ArtifactEnvelope],
    catalog: EffectiveCatalog,
    env: Option<ZenTypeEnv>,
    limits: ResourceLimitProfile,
}

impl<'a> Context<'a> {
    fn new(pool: &'a [ArtifactEnvelope], registry: &BTreeSet<PropertyTypeName>) -> Self {
        let limits = ResourceLimitProfile::norn_limits_v1();
        Self {
            pool,
            catalog: EffectiveCatalog::from_artifacts_with_registry(pool, registry),
            env: crate::derive_type_env(&[], pool, &limits).ok(),
            limits,
        }
    }

    fn admit(&self, artifact: &ArtifactEnvelope) -> Result<TransformPlan, Vec<Diagnostic>> {
        let root = view_root(artifact).ok_or_else(|| {
            vec![finding(
                codes::INVALID_PLAN,
                "the view payload does not decode as ViewIR",
                "/payload",
            )]
        })?;
        let CalcExpr::Relational(plan) = &root.calc else {
            return Err(vec![finding(
                codes::INVALID_PLAN,
                "only a relational calc is a ViewIR v1 transform",
                "/payload/calc/kind",
            )]);
        };
        let mut out = Vec::new();
        if root.maintenance != ViewMaintenance::RecomputeOnRead {
            out.push(finding(
                codes::INVALID_PLAN,
                "a relational view is recomputed on read until materialization is admitted",
                "/payload/maintenance",
            ));
        }
        if root
            .temporal_scope
            .as_ref()
            .is_some_and(|scope| scope.mode != norn_ir::TemporalScopeMode::Current)
        {
            out.push(finding(
                codes::INVALID_PLAN,
                "a relational view reads the current world; as-of views are not admitted yet",
                "/payload/temporalScope",
            ));
        }
        if !root.output_schema.is_empty() {
            out.push(finding(
                codes::OUTPUT_SCHEMA_MISMATCH,
                "a relational view infers its output schema; `outputSchema` must be `{}`",
                "/payload/outputSchema",
            ));
        }
        let inputs = self.inputs(artifact, &root, &mut out);
        let Some(order) = topological(plan, &mut out) else {
            return Err(out);
        };
        let mut builder = Builder {
            context: self,
            inputs: &inputs,
            nodes: BTreeMap::new(),
            links: BTreeSet::new(),
            scanned: BTreeSet::new(),
            out: Vec::new(),
        };
        for id in &order {
            let Some(node) = plan.nodes.get(id) else {
                continue;
            };
            let pointer = format!("/payload/calc/nodes/{}", pointer_token(id));
            if let Some(admitted) = builder.node(node, &pointer) {
                builder.nodes.insert(id.clone(), admitted);
            } else {
                if builder.out.is_empty() {
                    builder.fail(codes::INVALID_PLAN, "the node cannot be typed", pointer);
                }
                // A node that fails leaves its consumers unchecked; their
                // findings would only repeat this one.
                break;
            }
        }
        let nodes = std::mem::take(&mut builder.nodes);
        let links = std::mem::take(&mut builder.links);
        out.append(&mut builder.out);
        let Some(schema) = nodes.get(&plan.output).map(|node| node.schema.clone()) else {
            return Err(out);
        };
        if !out.is_empty() {
            return Err(out);
        }
        Ok(TransformPlan {
            artifact_id: artifact.artifact_id.clone(),
            version: artifact.version.clone(),
            view_id: root.view_id,
            inputs,
            order,
            nodes,
            output: plan.output.clone(),
            links,
            fuel_units: self.limits.decide.fuel_units,
            schema,
        })
    }

    fn inputs(
        &self,
        artifact: &ArtifactEnvelope,
        root: &ViewIrPayload,
        out: &mut Vec<Diagnostic>,
    ) -> BTreeMap<String, ScanInput> {
        let mut inputs = BTreeMap::new();
        for (index, input) in root.inputs.iter().enumerate() {
            let pointer = format!("/payload/inputs/{index}");
            if input.kind != ViewInputKind::Entity {
                out.push(finding(
                    codes::INVALID_PLAN,
                    "a ViewIR v1 input reads an entity; relation and derived inputs are not admitted yet",
                    format!("{pointer}/kind"),
                ));
                continue;
            }
            let pinned = artifact.dependencies.iter().any(|pin| {
                pin.required
                    && pin.artifact_id == input.artifact_id
                    && pin.version.as_deref() == Some(input.version.as_str())
                    && pin.kind == ArtifactKind::Entity
            });
            let mut targets = self
                .pool
                .iter()
                .filter(|a| a.artifact_id == input.artifact_id && a.version == input.version);
            let target = targets.next();
            if !pinned || target.is_none() || targets.next().is_some() {
                out.push(finding(
                    codes::INVALID_PLAN,
                    "an input needs one exact required entity dependency in the set",
                    pointer,
                ));
                continue;
            }
            let Some(Ok(object)) = self.catalog.object_type(&input.artifact_id) else {
                out.push(finding(
                    codes::INVALID_PLAN,
                    "an input must pin the artifact that declares an available object type",
                    format!("{pointer}/artifactId"),
                ));
                continue;
            };
            let properties = object
                .properties
                .iter()
                .filter(|property| !property.computed && property.id.as_str() != "id")
                .map(|property| property.id.as_str().to_owned())
                .collect();
            inputs.insert(
                input.alias.clone(),
                ScanInput {
                    alias: input.alias.clone(),
                    artifact_id: input.artifact_id.clone(),
                    version: input.version.clone(),
                    object_type: object.name.as_str().to_owned(),
                    properties,
                },
            );
        }
        inputs
    }
}

fn pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// A column or node name: lower-case ASCII identifier, readable as `input.<name>`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.len() <= 64
}

fn node_inputs(node: &ViewNode) -> Vec<&str> {
    match node {
        ViewNode::Scan(_) => Vec::new(),
        ViewNode::Filter(n) => vec![&n.input],
        ViewNode::Project(n) => vec![&n.input],
        ViewNode::Join(n) => vec![&n.left, &n.right],
        ViewNode::Aggregate(n) => vec![&n.input],
        ViewNode::Window(n) => vec![&n.input],
        ViewNode::Union(n) => n.inputs.iter().map(String::as_str).collect(),
        ViewNode::Dedupe(n) => vec![&n.input],
    }
}

/// Evaluation order: every node after its inputs. Unknown references,
/// cycles, unreachable nodes and an unknown output refuse.
fn topological(plan: &norn_ir::RelationalPlan, out: &mut Vec<Diagnostic>) -> Option<Vec<String>> {
    let before = out.len();
    if plan.nodes.is_empty() || plan.nodes.len() > 256 {
        out.push(finding(
            codes::INVALID_PLAN,
            "a relational plan has 1 to 256 nodes",
            "/payload/calc/nodes",
        ));
        return None;
    }
    for (id, node) in &plan.nodes {
        if !valid_name(id) {
            out.push(finding(
                codes::INVALID_PLAN,
                format!("node id {id:?} must be a lower-case identifier"),
                format!("/payload/calc/nodes/{}", pointer_token(id)),
            ));
        }
        for input in node_inputs(node) {
            if !plan.nodes.contains_key(input) {
                out.push(finding(
                    codes::INVALID_PLAN,
                    format!("node {id:?} reads unknown node {input:?}"),
                    format!("/payload/calc/nodes/{}", pointer_token(id)),
                ));
            }
        }
    }
    if !plan.nodes.contains_key(&plan.output) {
        out.push(finding(
            codes::INVALID_PLAN,
            format!("output node {:?} is not declared", plan.output),
            "/payload/calc/output",
        ));
    }
    if out.len() > before {
        return None;
    }
    // Depth-first from the output: post-order is an evaluation order.
    let mut order = Vec::new();
    let mut state: BTreeMap<&str, bool> = BTreeMap::new(); // false = visiting, true = done
    let mut stack: Vec<(&str, bool)> = vec![(plan.output.as_str(), false)];
    while let Some((id, expanded)) = stack.pop() {
        if expanded {
            state.insert(id, true);
            order.push(id.to_owned());
            continue;
        }
        match state.get(id) {
            Some(true) => continue,
            Some(false) => {
                out.push(finding(
                    codes::INVALID_PLAN,
                    format!("node {id:?} is part of a cycle"),
                    format!("/payload/calc/nodes/{}", pointer_token(id)),
                ));
                return None;
            }
            None => {}
        }
        state.insert(id, false);
        stack.push((id, true));
        let inputs = plan.nodes.get(id).map(node_inputs).unwrap_or_default();
        for input in inputs.into_iter().rev() {
            match state.get(input) {
                Some(true) => {}
                Some(false) => {
                    out.push(finding(
                        codes::INVALID_PLAN,
                        format!("node {input:?} is part of a cycle"),
                        format!("/payload/calc/nodes/{}", pointer_token(input)),
                    ));
                    return None;
                }
                None => stack.push((input, false)),
            }
        }
    }
    for id in plan.nodes.keys() {
        if !state.contains_key(id.as_str()) {
            out.push(finding(
                codes::INVALID_PLAN,
                format!("node {id:?} does not contribute to the output"),
                format!("/payload/calc/nodes/{}", pointer_token(id)),
            ));
        }
    }
    (out.len() == before).then_some(order)
}

struct Builder<'c, 'a> {
    context: &'c Context<'a>,
    inputs: &'c BTreeMap<String, ScanInput>,
    nodes: BTreeMap<String, AdmittedNode>,
    links: BTreeSet<(String, String)>,
    scanned: BTreeSet<String>,
    out: Vec<Diagnostic>,
}

fn strip(ty: &Type) -> &Type {
    if let Type::Optional(inner) = ty {
        strip(inner)
    } else {
        ty
    }
}

fn numeric(ty: &Type) -> bool {
    matches!(
        strip(ty),
        Type::Integer | Type::Decimal { .. } | Type::Number | Type::DecimalBinding { .. }
    )
}

fn scalar(ty: &Type) -> bool {
    matches!(
        strip(ty),
        Type::Bool
            | Type::Text { .. }
            | Type::Integer
            | Type::Decimal { .. }
            | Type::Number
            | Type::Timestamp { .. }
            | Type::Date
            | Type::Duration
            | Type::Enum { .. }
    )
}

fn orderable(ty: &Type) -> bool {
    scalar(ty) && !matches!(strip(ty), Type::Duration)
}

impl Builder<'_, '_> {
    fn fail(&mut self, code: &str, message: impl Into<String>, pointer: impl Into<String>) {
        self.out.push(finding(code, message, pointer));
    }

    fn schema(&self, id: &str) -> Option<&NodeSchema> {
        self.nodes.get(id).map(|node| &node.schema)
    }

    fn node(&mut self, node: &ViewNode, at: &str) -> Option<AdmittedNode> {
        let before = self.out.len();
        let admitted = match node {
            ViewNode::Scan(scan) => self.scan(&scan.input, at),
            ViewNode::Filter(filter) => self.filter(&filter.input, &filter.predicate, at),
            ViewNode::Project(project) => self.project(project, at),
            ViewNode::Join(join) => self.join(join, at),
            ViewNode::Aggregate(aggregate) => self.aggregate(aggregate, at),
            ViewNode::Window(window) => self.window(window, at),
            ViewNode::Union(union) => self.union(union, at),
            ViewNode::Dedupe(dedupe) => self.dedupe(dedupe, at),
        };
        if self.out.len() > before {
            return None;
        }
        if let Some(admitted) = &admitted {
            let mut names = BTreeSet::new();
            for column in &admitted.schema.columns {
                if !names.insert(column.name.as_str()) {
                    self.fail(
                        codes::COLUMN_CONFLICT,
                        format!("two output columns are named {:?}", column.name),
                        at,
                    );
                    return None;
                }
            }
        }
        admitted
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one scan contract: every stored property's column, read top to bottom"
    )]
    fn scan(&mut self, alias: &str, at: &str) -> Option<AdmittedNode> {
        let Some(input) = self.inputs.get(alias) else {
            self.fail(
                codes::INVALID_PLAN,
                format!("scan reads undeclared input alias {alias:?}"),
                format!("{at}/input"),
            );
            return None;
        };
        if !self.scanned.insert(alias.to_owned()) {
            self.fail(
                codes::INVALID_PLAN,
                format!("input {alias:?} is scanned twice; declare a second alias"),
                format!("{at}/input"),
            );
            return None;
        }
        let Some(env) = &self.context.env else {
            self.fail(
                codes::INVALID_PLAN,
                "the input types have no supported expression environment",
                format!("{at}/input"),
            );
            return None;
        };
        let Some(record) = env.record(&input.object_type) else {
            self.fail(
                codes::INVALID_PLAN,
                format!(
                    "type {:?} is absent from the expression environment",
                    input.object_type
                ),
                format!("{at}/input"),
            );
            return None;
        };
        let Some(Ok(object)) = self.context.catalog.object_type_named(&input.object_type) else {
            self.fail(
                codes::INVALID_PLAN,
                format!("type {:?} is unavailable", input.object_type),
                format!("{at}/input"),
            );
            return None;
        };
        let mut columns = vec![Column {
            name: "id".to_owned(),
            ty: Type::text(),
            additivity: PropertyAdditivity::Undeclared {},
            identity: Some(input.object_type.clone()),
            exact: Some(Origin::new(alias, "id")),
            origins: BTreeSet::from([Origin::new(alias, "id")]),
            fanned: false,
            currency: None,
            markings: object
                .markings
                .iter()
                .map(|marking| marking.as_str().to_owned())
                .collect(),
        }];
        for id in &input.properties {
            let Some(property) = object.property(id) else {
                continue;
            };
            let Some(field) = record.fields.get(id) else {
                continue;
            };
            let additivity = match &property.kind {
                EffectiveKind::Base(kind) => kind.additivity().clone(),
                EffectiveKind::Registry(_) | EffectiveKind::Unresolved => {
                    PropertyAdditivity::Undeclared {}
                }
            };
            let currency = if let Type::Decimal {
                currency_source: Some(path),
                ..
            } = strip(&field.ty)
            {
                Some(if path.links.is_empty() {
                    Origin::new(alias, path.property.as_str())
                } else {
                    // A currency read through a link cannot be grouped by a
                    // column of this row: every sum of it refuses.
                    Origin::new(alias, "")
                })
            } else {
                None
            };
            let mut markings: BTreeSet<String> = object
                .markings
                .iter()
                .map(|marking| marking.as_str().to_owned())
                .collect();
            markings.extend(
                property
                    .markings
                    .iter()
                    .map(|marking| marking.as_str().to_owned()),
            );
            columns.push(Column {
                name: id.clone(),
                ty: field.ty.clone(),
                additivity,
                identity: None,
                exact: Some(Origin::new(alias, id)),
                origins: BTreeSet::from([Origin::new(alias, id)]),
                fanned: false,
                currency,
                markings,
            });
        }
        Some(AdmittedNode {
            step: Step::Scan {
                alias: alias.to_owned(),
            },
            schema: NodeSchema {
                columns,
                grain: vec!["id".to_owned()],
            },
        })
    }

    fn env_for(&mut self, schema: &NodeSchema, at: &str) -> Option<ZenTypeEnv> {
        let Some(base) = self.context.env.as_ref() else {
            self.fail(
                codes::INVALID_PLAN,
                "the input types have no supported expression environment",
                at,
            );
            return None;
        };
        let records = base
            .records()
            .iter()
            .filter(|(name, _)| name.as_str() != "input")
            .map(|(name, record)| {
                (
                    name.clone(),
                    record
                        .fields
                        .iter()
                        .map(|(field, value)| (field.clone(), value.ty.clone()))
                        .collect(),
                )
            })
            .collect();
        match ZenTypeEnv::input_only_with_records(
            records,
            schema.fields(),
            EnvOptions::from_limit_profile(&self.context.limits),
        ) {
            Ok(env) => Some(env),
            Err(error) => {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("the row has no expression environment: {error}"),
                    at,
                );
                None
            }
        }
    }

    fn typed(
        &mut self,
        schema: &NodeSchema,
        source: &str,
        expected: &Type,
        at: &str,
    ) -> Option<(TypedExpression, InferredType)> {
        let env = self.env_for(schema, at)?;
        let expression = match Expression::compile(source) {
            Ok(expression) => expression,
            Err(error) => {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("expression does not compile: {error}"),
                    at,
                );
                return None;
            }
        };
        match norn_expr::check_expression_with(&expression, &env, expected, &self.context.limits) {
            Ok(checked) => {
                let inferred = checked.into_inferred();
                Some((
                    TypedExpression {
                        expression,
                        env,
                        expected: expected.clone(),
                    },
                    inferred,
                ))
            }
            Err(findings) => {
                let message = findings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                self.fail(codes::TYPE_MISMATCH, message, at);
                None
            }
        }
    }

    fn read_columns(&mut self, schema: &NodeSchema, source: &str, at: &str) -> Vec<Column> {
        match norn_expr::expression_read_paths(source, ROW_ROOT) {
            Ok(paths) => paths
                .iter()
                .filter_map(|path| path.first())
                .filter_map(|name| schema.column(name).cloned())
                .collect(),
            Err(error) => {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("expression reads are unbounded: {error}"),
                    at,
                );
                Vec::new()
            }
        }
    }

    fn filter(&mut self, input: &str, predicate: &str, at: &str) -> Option<AdmittedNode> {
        let schema = self.schema(input)?.clone();
        let (predicate, _) =
            self.typed(&schema, predicate, &Type::Bool, &format!("{at}/predicate"))?;
        Some(AdmittedNode {
            step: Step::Filter {
                input: input.to_owned(),
                predicate,
            },
            schema,
        })
    }

    fn project(&mut self, project: &norn_ir::ProjectNode, at: &str) -> Option<AdmittedNode> {
        let schema = self.schema(&project.input)?.clone();
        if project.columns.is_empty() {
            self.fail(
                codes::INVALID_PLAN,
                "a projection lists at least one column",
                format!("{at}/columns"),
            );
            return None;
        }
        let mut columns = Vec::new();
        let mut steps = Vec::new();
        for (index, column) in project.columns.iter().enumerate() {
            let pointer = format!("{at}/columns/{index}");
            if !valid_name(&column.name) {
                self.fail(
                    codes::INVALID_PLAN,
                    format!(
                        "column name {:?} must be a lower-case identifier",
                        column.name
                    ),
                    format!("{pointer}/name"),
                );
                continue;
            }
            if let Some(source) = passthrough(&column.expression) {
                let Some(found) = schema.column(source) else {
                    self.fail(
                        codes::UNKNOWN_COLUMN,
                        format!("column {source:?} is not in the input row"),
                        format!("{pointer}/expression"),
                    );
                    continue;
                };
                columns.push(found.renamed(&column.name));
                steps.push((column.name.clone(), Projection::Column(source.to_owned())));
                continue;
            }
            let Some((typed, inferred)) = self.typed(
                &schema,
                &column.expression,
                &Type::Json,
                &format!("{pointer}/expression"),
            ) else {
                continue;
            };
            let Some(ty) = column_type(&inferred) else {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("a computed column is a scalar, not {inferred}"),
                    format!("{pointer}/expression"),
                );
                continue;
            };
            let reads = self.read_columns(&schema, &column.expression, &pointer);
            let mut computed = Column::derived(&column.name, ty, PropertyAdditivity::Undeclared {});
            for read in reads {
                computed.origins.extend(read.origins);
                computed.markings.extend(read.markings);
                computed.fanned |= read.fanned;
            }
            columns.push(computed);
            steps.push((column.name.clone(), Projection::Expression(typed)));
        }
        for grain in &schema.grain {
            let kept = steps.iter().any(|(name, step)| {
                name == grain && matches!(step, Projection::Column(source) if source == grain)
            });
            if !kept {
                self.fail(
                    codes::GRAIN_DROPPED,
                    format!(
                        "grain column {grain:?} must pass through unchanged as `input.{grain}`"
                    ),
                    format!("{at}/columns"),
                );
            }
        }
        Some(AdmittedNode {
            step: Step::Project {
                input: project.input.clone(),
                columns: steps,
            },
            schema: NodeSchema {
                columns,
                grain: schema.grain,
            },
        })
    }

    /// The right side of a join must be the rows of one object type at its
    /// identity grain (VIEW-JOIN-1): its single grain column is an exact id.
    fn rooted(&mut self, schema: &NodeSchema, at: &str) -> Option<(String, String, String)> {
        let rooted =
            match schema.grain.as_slice() {
                [grain] => schema.column(grain).and_then(|column| {
                    match (&column.identity, &column.exact) {
                        (Some(object_type), Some(origin)) if origin.property == "id" => Some((
                            column.name.clone(),
                            object_type.clone(),
                            origin.alias.clone(),
                        )),
                        _ => None,
                    }
                }),
                _ => None,
            };
        if rooted.is_none() {
            self.fail(
                codes::JOIN_UNDECLARED,
                "the right side of a join must be one object type's rows at their identity grain",
                format!("{at}/right"),
            );
        }
        rooted
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one closed join law, read top to bottom"
    )]
    fn join(&mut self, join: &norn_ir::JoinNode, at: &str) -> Option<AdmittedNode> {
        let left = self.schema(&join.left)?.clone();
        let right = self.schema(&join.right)?.clone();
        if !join.prefix.is_empty() && !valid_name(&format!("{}x", join.prefix))
            || join.prefix.is_empty()
        {
            self.fail(
                codes::INVALID_PLAN,
                "a join prefix is a non-empty lower-case identifier prefix",
                format!("{at}/prefix"),
            );
            return None;
        }
        let (right_id, right_type, right_alias) = self.rooted(&right, at)?;
        // Which side repeats: a value is fanned when the join can pair it
        // with more than one row of the other side.
        let (on, left_fans, right_fans) = match &join.on {
            JoinOn::Link(link) => {
                let Some(column) = left.column(&link.column) else {
                    self.fail(
                        codes::UNKNOWN_COLUMN,
                        format!("column {:?} is not in the left row", link.column),
                        format!("{at}/on/link/column"),
                    );
                    return None;
                };
                let (Some(source_type), Some(origin)) = (&column.identity, &column.exact) else {
                    self.fail(
                        codes::JOIN_UNDECLARED,
                        "a link join starts from a column holding object ids",
                        format!("{at}/on/link/column"),
                    );
                    return None;
                };
                let declared = self
                    .context
                    .catalog
                    .links_for_object_type(source_type)
                    .and_then(|links| links.iter().find(|l| l.id.as_str() == link.link));
                let Some(declared) = declared else {
                    self.fail(
                        codes::JOIN_UNDECLARED,
                        format!("type {source_type:?} declares no link {:?}", link.link),
                        format!("{at}/on/link/link"),
                    );
                    return None;
                };
                let admits_right = self
                    .context
                    .catalog
                    .target_types(declared.target.as_str())
                    .is_some_and(|targets| targets.contains(&right_type));
                if !admits_right {
                    self.fail(
                        codes::JOIN_UNDECLARED,
                        format!(
                            "link {:?} targets {:?}, not the right side's type {right_type:?}",
                            link.link,
                            declared.target.as_str()
                        ),
                        format!("{at}/right"),
                    );
                    return None;
                }
                self.links.insert((origin.alias.clone(), link.link.clone()));
                (
                    AdmittedJoin::Link {
                        column: link.column.clone(),
                        alias: origin.alias.clone(),
                        link: link.link.clone(),
                        right: right_id.clone(),
                    },
                    declared.cardinality.to == norn_ir::Multiplicity::Many,
                    declared.cardinality.from == norn_ir::Multiplicity::Many,
                )
            }
            JoinOn::Key(key) => {
                let components: Vec<(String, Vec<Normalization>)> = if key.right == "id" {
                    vec![("id".to_owned(), Vec::new())]
                } else {
                    let Some(Ok(object)) = self.context.catalog.object_type_named(&right_type)
                    else {
                        self.fail(
                            codes::JOIN_UNDECLARED,
                            "the right type is unavailable",
                            format!("{at}/right"),
                        );
                        return None;
                    };
                    let Some(declared) = object.keys.iter().find(|k| k.name.as_str() == key.right)
                    else {
                        self.fail(
                            codes::JOIN_UNDECLARED,
                            format!("type {right_type:?} declares no key {:?}", key.right),
                            format!("{at}/on/key/right"),
                        );
                        return None;
                    };
                    declared
                        .components
                        .iter()
                        .map(|component| {
                            (
                                component.as_str().to_owned(),
                                declared
                                    .normalize
                                    .get(component)
                                    .cloned()
                                    .unwrap_or_default(),
                            )
                        })
                        .collect()
                };
                if key.left.len() != components.len() {
                    self.fail(
                        codes::JOIN_UNDECLARED,
                        format!(
                            "key {:?} has {} components, the join names {}",
                            key.right,
                            components.len(),
                            key.left.len()
                        ),
                        format!("{at}/on/key/left"),
                    );
                    return None;
                }
                let mut pairs = Vec::new();
                for (index, (left_name, (property, normalize))) in
                    key.left.iter().zip(&components).enumerate()
                {
                    let Some(left_column) = left.column(left_name) else {
                        self.fail(
                            codes::UNKNOWN_COLUMN,
                            format!("column {left_name:?} is not in the left row"),
                            format!("{at}/on/key/left/{index}"),
                        );
                        return None;
                    };
                    let wanted = Origin::new(&right_alias, property);
                    let Some(right_column) = right
                        .columns
                        .iter()
                        .find(|column| column.exact.as_ref() == Some(&wanted))
                    else {
                        self.fail(
                            codes::UNKNOWN_COLUMN,
                            format!(
                                "the right row must carry key component {property:?} unchanged"
                            ),
                            format!("{at}/on/key/right"),
                        );
                        return None;
                    };
                    if strip(&left_column.ty) != strip(&right_column.ty) || !scalar(&left_column.ty)
                    {
                        self.fail(
                            codes::TYPE_MISMATCH,
                            format!(
                                "key component {property:?} is {}, the left column {left_name:?} is {}",
                                right_column.ty, left_column.ty
                            ),
                            format!("{at}/on/key/left/{index}"),
                        );
                        return None;
                    }
                    pairs.push((
                        left_name.clone(),
                        right_column.name.clone(),
                        normalize.clone(),
                    ));
                }
                // The right side is unique on its key: many-to-one.
                (AdmittedJoin::Key { pairs }, false, true)
            }
        };
        let mut columns: Vec<Column> = left
            .columns
            .iter()
            .map(|column| Column {
                fanned: column.fanned || left_fans,
                ..column.clone()
            })
            .collect();
        let mut renames = Vec::new();
        for column in &right.columns {
            let name = format!("{}{}", join.prefix, column.name);
            let mut right_column = Column {
                fanned: column.fanned || right_fans,
                ..column.renamed(&name)
            };
            if join.kind == JoinKind::Left {
                right_column = right_column.nullable();
            }
            renames.push((column.name.clone(), name));
            columns.push(right_column);
        }
        let mut grain = left.grain;
        if left_fans {
            grain.push(format!("{}{}", join.prefix, right_id));
        }
        Some(AdmittedNode {
            step: Step::Join {
                left: join.left.clone(),
                right: join.right.clone(),
                on,
                kind: join.kind,
                renames,
            },
            schema: NodeSchema { columns, grain },
        })
    }

    /// VIEW-AGG-1: the metric layer's refusals for summing (and, for an
    /// undeclared numeric, any aggregate of) a column.
    fn measure_law(&mut self, column: &Column, function: &str, sum: bool, at: &str) -> bool {
        if numeric(&column.ty) && matches!(column.additivity, PropertyAdditivity::Undeclared {}) {
            self.fail(
                metric_codes::UNDECLARED_ADDITIVITY,
                format!(
                    "{function} of {:?}: its additivity is undeclared",
                    column.name
                ),
                at,
            );
            return false;
        }
        if !sum {
            return true;
        }
        if !numeric(&column.ty) {
            self.fail(
                codes::TYPE_MISMATCH,
                format!(
                    "{function} needs a numeric column, {:?} is {}",
                    column.name, column.ty
                ),
                at,
            );
            return false;
        }
        let refusal = match &column.additivity {
            PropertyAdditivity::Additive {} => None,
            PropertyAdditivity::Ratio { .. } => Some((
                metric_codes::RATIO_AGGREGATION,
                "a stored ratio is not summed; sum its operands, then divide",
            )),
            PropertyAdditivity::SemiAdditive {} => Some((
                metric_codes::SNAPSHOT_AGGREGATION,
                "a semi-additive balance is not summed across time",
            )),
            PropertyAdditivity::NonAdditive {} => Some((
                metric_codes::INVALID_ADDITIVITY,
                "a non-additive value is not summed",
            )),
            PropertyAdditivity::Undeclared {} => Some((
                metric_codes::UNDECLARED_ADDITIVITY,
                "its additivity is undeclared",
            )),
        };
        if let Some((code, why)) = refusal {
            self.fail(code, format!("{function} of {:?}: {why}", column.name), at);
            return false;
        }
        if column.fanned {
            self.fail(
                codes::AMBIGUOUS_GRAIN,
                format!(
                    "{function} of {:?}: a join repeats it, so the sum would count it more than once; aggregate before joining",
                    column.name
                ),
                at,
            );
            return false;
        }
        true
    }

    fn currency_grouped(
        &mut self,
        column: &Column,
        keys: &[&Column],
        function: &str,
        at: &str,
    ) -> bool {
        let Some(currency) = &column.currency else {
            return true;
        };
        if keys.iter().any(|key| key.exact.as_ref() == Some(currency)) {
            return true;
        }
        self.fail(
            metric_codes::MIXED_CURRENCY,
            format!(
                "{function} of {:?} mixes currencies: group by its currency {:?}",
                column.name, currency.property
            ),
            at,
        );
        false
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one closed aggregate law, read top to bottom"
    )]
    fn aggregate(&mut self, aggregate: &norn_ir::AggregateNode, at: &str) -> Option<AdmittedNode> {
        let schema = self.schema(&aggregate.input)?.clone();
        if aggregate.measures.is_empty() {
            self.fail(
                codes::INVALID_PLAN,
                "an aggregate has at least one measure",
                format!("{at}/measures"),
            );
            return None;
        }
        let mut columns = Vec::new();
        let mut group_by = Vec::new();
        let mut keys = Vec::new();
        for (index, key) in aggregate.group_by.iter().enumerate() {
            let pointer = format!("{at}/groupBy/{index}");
            let Some(column) = schema.column(&key.column) else {
                self.fail(
                    codes::UNKNOWN_COLUMN,
                    format!("column {:?} is not in the input row", key.column),
                    pointer,
                );
                continue;
            };
            if !scalar(&column.ty) {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("group key {:?} is not a scalar", key.column),
                    pointer,
                );
                continue;
            }
            let mut output = Column {
                fanned: false,
                ..column.clone()
            };
            if let Some(grain) = key.grain {
                if !matches!(strip(&column.ty), Type::Date | Type::Timestamp { .. }) {
                    self.fail(
                        codes::TYPE_MISMATCH,
                        format!(
                            "a calendar grain buckets a date or timestamp, {:?} is {}",
                            key.column, column.ty
                        ),
                        pointer,
                    );
                    continue;
                }
                output.ty = if matches!(column.ty, Type::Optional(_)) {
                    Type::Optional(Box::new(Type::Date))
                } else {
                    Type::Date
                };
                output.exact = None;
                output.identity = None;
                group_by.push((key.column.clone(), Some(grain), column.ty.clone()));
            } else {
                group_by.push((key.column.clone(), None, column.ty.clone()));
                keys.push(column.clone());
            }
            columns.push(output);
        }
        let key_refs: Vec<&Column> = keys.iter().collect();
        let mut measures = Vec::new();
        for (index, measure) in aggregate.measures.iter().enumerate() {
            let pointer = format!("{at}/measures/{index}");
            if !valid_name(&measure.name) {
                self.fail(
                    codes::INVALID_PLAN,
                    format!(
                        "measure name {:?} must be a lower-case identifier",
                        measure.name
                    ),
                    format!("{pointer}/name"),
                );
                continue;
            }
            let function = match measure.function {
                AggregateFn::Sum => "sum",
                AggregateFn::Count => "count",
                AggregateFn::CountDistinct => "count_distinct",
                AggregateFn::Min => "min",
                AggregateFn::Max => "max",
            };
            let Some(name) = &measure.column else {
                if measure.function == AggregateFn::Count {
                    columns.push(Column::derived(
                        &measure.name,
                        Type::Integer,
                        PropertyAdditivity::Additive {},
                    ));
                    measures.push((measure.name.clone(), measure.function, None, None));
                } else {
                    self.fail(
                        codes::INVALID_PLAN,
                        format!("{function} needs a column"),
                        pointer,
                    );
                }
                continue;
            };
            let Some(column) = schema.column(name).cloned() else {
                self.fail(
                    codes::UNKNOWN_COLUMN,
                    format!("column {name:?} is not in the input row"),
                    format!("{pointer}/column"),
                );
                continue;
            };
            if !scalar(&column.ty) {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("{function} reads a scalar, {name:?} is {}", column.ty),
                    pointer,
                );
                continue;
            }
            let sum = measure.function == AggregateFn::Sum;
            if !self.measure_law(&column, function, sum, &pointer) {
                continue;
            }
            if sum && !self.currency_grouped(&column, &key_refs, function, &pointer) {
                continue;
            }
            let (ty, additivity) = match measure.function {
                AggregateFn::Count | AggregateFn::CountDistinct => {
                    (Type::Integer, PropertyAdditivity::Additive {})
                }
                AggregateFn::Sum => (column.ty.clone(), PropertyAdditivity::Additive {}),
                AggregateFn::Min | AggregateFn::Max => {
                    if !orderable(&column.ty) {
                        self.fail(
                            codes::TYPE_MISMATCH,
                            format!("{function} needs an ordered column"),
                            pointer,
                        );
                        continue;
                    }
                    (column.ty.clone(), PropertyAdditivity::NonAdditive {})
                }
            };
            let mut output = Column::derived(&measure.name, ty, additivity);
            output.origins.clone_from(&column.origins);
            output.markings.clone_from(&column.markings);
            if sum {
                output.currency.clone_from(&column.currency);
            }
            columns.push(output);
            measures.push((
                measure.name.clone(),
                measure.function,
                Some(name.clone()),
                Some(column.ty.clone()),
            ));
        }
        Some(AdmittedNode {
            step: Step::Aggregate {
                input: aggregate.input.clone(),
                group_by,
                measures,
            },
            schema: NodeSchema {
                columns,
                grain: aggregate
                    .group_by
                    .iter()
                    .map(|key| key.column.clone())
                    .collect(),
            },
        })
    }

    /// VIEW-ORDER-1: order keys, typed; with `covering` they must cover the grain.
    fn order(
        &mut self,
        schema: &NodeSchema,
        covering: &[String],
        order_by: &[OrderKey],
        at: &str,
    ) -> Option<Vec<(String, SortDirection, Type)>> {
        let mut keys = Vec::new();
        for (index, key) in order_by.iter().enumerate() {
            let Some(column) = schema.column(&key.column) else {
                self.fail(
                    codes::UNKNOWN_COLUMN,
                    format!("column {:?} is not in the input row", key.column),
                    format!("{at}/orderBy/{index}"),
                );
                return None;
            };
            if !orderable(&column.ty) {
                self.fail(
                    codes::TYPE_MISMATCH,
                    format!("{:?} is not ordered", key.column),
                    format!("{at}/orderBy/{index}"),
                );
                return None;
            }
            keys.push((key.column.clone(), key.direction, column.ty.clone()));
        }
        let covered: BTreeSet<&str> = covering
            .iter()
            .map(String::as_str)
            .chain(order_by.iter().map(|key| key.column.as_str()))
            .collect();
        if let Some(missing) = schema.grain.iter().find(|g| !covered.contains(g.as_str())) {
            self.fail(
                codes::ORDER_NOT_TOTAL,
                format!("the order is not total: add grain column {missing:?}"),
                format!("{at}/orderBy"),
            );
            return None;
        }
        Some(keys)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one closed window law, read top to bottom"
    )]
    fn window(&mut self, window: &norn_ir::WindowNode, at: &str) -> Option<AdmittedNode> {
        let schema = self.schema(&window.input)?.clone();
        for (index, column) in window.partition_by.iter().enumerate() {
            match schema.column(column) {
                Some(found) if scalar(&found.ty) => {}
                Some(_) => self.fail(
                    codes::TYPE_MISMATCH,
                    format!("partition {column:?} is not a scalar"),
                    format!("{at}/partitionBy/{index}"),
                ),
                None => self.fail(
                    codes::UNKNOWN_COLUMN,
                    format!("column {column:?} is not in the input row"),
                    format!("{at}/partitionBy/{index}"),
                ),
            }
        }
        let order_by = self.order(&schema, &window.partition_by, &window.order_by, at)?;
        if window.columns.is_empty() {
            self.fail(
                codes::INVALID_PLAN,
                "a window adds at least one column",
                format!("{at}/columns"),
            );
            return None;
        }
        let partition: Vec<Column> = window
            .partition_by
            .iter()
            .filter_map(|name| schema.column(name).cloned())
            .collect();
        let partition_refs: Vec<&Column> = partition.iter().collect();
        let mut columns = schema.columns.clone();
        let mut steps = Vec::new();
        for (index, column) in window.columns.iter().enumerate() {
            let pointer = format!("{at}/columns/{index}");
            if !valid_name(&column.name) {
                self.fail(
                    codes::INVALID_PLAN,
                    format!(
                        "column name {:?} must be a lower-case identifier",
                        column.name
                    ),
                    format!("{pointer}/name"),
                );
                continue;
            }
            let read = column
                .column
                .as_ref()
                .map(|name| (name, schema.column(name).cloned()));
            let output = match (column.function, read) {
                (WindowFn::RowNumber | WindowFn::Rank | WindowFn::DenseRank, None) => {
                    Column::derived(
                        &column.name,
                        Type::Integer,
                        PropertyAdditivity::NonAdditive {},
                    )
                }
                (WindowFn::Lag | WindowFn::Lead, Some((_, Some(found)))) => {
                    let mut output = Column::derived(
                        &column.name,
                        found.ty.clone(),
                        PropertyAdditivity::NonAdditive {},
                    )
                    .nullable();
                    output.origins.clone_from(&found.origins);
                    output.markings.clone_from(&found.markings);
                    output
                }
                (WindowFn::RunningSum, Some((_, Some(found)))) => {
                    if !self.measure_law(&found, "running_sum", true, &pointer)
                        || !self.currency_grouped(&found, &partition_refs, "running_sum", &pointer)
                    {
                        continue;
                    }
                    let mut output = Column::derived(
                        &column.name,
                        found.ty.clone(),
                        PropertyAdditivity::NonAdditive {},
                    );
                    output.origins.clone_from(&found.origins);
                    output.markings.clone_from(&found.markings);
                    output
                }
                (_, Some((name, None))) => {
                    self.fail(
                        codes::UNKNOWN_COLUMN,
                        format!("column {name:?} is not in the input row"),
                        format!("{pointer}/column"),
                    );
                    continue;
                }
                (WindowFn::RowNumber | WindowFn::Rank | WindowFn::DenseRank, Some(_)) => {
                    self.fail(
                        codes::INVALID_PLAN,
                        "a ranking function reads no column",
                        format!("{pointer}/column"),
                    );
                    continue;
                }
                (WindowFn::Lag | WindowFn::Lead | WindowFn::RunningSum, None) => {
                    self.fail(
                        codes::INVALID_PLAN,
                        "this window function reads a column",
                        format!("{pointer}/column"),
                    );
                    continue;
                }
            };
            let offset = column.offset.unwrap_or(1);
            if column.offset.is_some() && !matches!(column.function, WindowFn::Lag | WindowFn::Lead)
                || offset == 0
            {
                self.fail(
                    codes::INVALID_PLAN,
                    "only lag and lead take an offset of at least 1",
                    format!("{pointer}/offset"),
                );
                continue;
            }
            columns.push(output);
            steps.push((
                column.name.clone(),
                column.function,
                column.column.clone(),
                offset,
            ));
        }
        Some(AdmittedNode {
            step: Step::Window {
                input: window.input.clone(),
                partition_by: window.partition_by.clone(),
                order_by,
                columns: steps,
            },
            schema: NodeSchema {
                columns,
                grain: schema.grain,
            },
        })
    }

    fn union(&mut self, union: &norn_ir::UnionNode, at: &str) -> Option<AdmittedNode> {
        let distinct: BTreeSet<&String> = union.inputs.iter().collect();
        if union.inputs.len() < 2 || distinct.len() != union.inputs.len() {
            self.fail(
                codes::INVALID_PLAN,
                "a union reads two or more distinct nodes",
                format!("{at}/inputs"),
            );
            return None;
        }
        if !valid_name(&union.branch) {
            self.fail(
                codes::INVALID_PLAN,
                "the branch column is a lower-case identifier",
                format!("{at}/branch"),
            );
            return None;
        }
        let schemas: Vec<NodeSchema> = union
            .inputs
            .iter()
            .filter_map(|id| self.schema(id).cloned())
            .collect();
        let first = schemas.first()?.clone();
        let first_id = union.inputs.first().cloned().unwrap_or_default();
        for (index, (schema, id)) in schemas.iter().zip(&union.inputs).enumerate().skip(1) {
            let same = schema.grain == first.grain
                && schema.columns.len() == first.columns.len()
                && schema.columns.iter().zip(&first.columns).all(|(a, b)| {
                    a.name == b.name
                        && a.ty == b.ty
                        && a.additivity == b.additivity
                        && a.currency.as_ref().map(|c| &c.property)
                            == b.currency.as_ref().map(|c| &c.property)
                });
            if !same {
                self.fail(
                    codes::UNION_SHAPE,
                    format!(
                        "input {id:?} differs from {first_id:?} in its columns, types, contracts or grain"
                    ),
                    format!("{at}/inputs/{index}"),
                );
                return None;
            }
        }
        let mut columns = vec![Column::derived(
            &union.branch,
            Type::enumeration(union.inputs.iter().cloned()),
            PropertyAdditivity::Undeclared {},
        )];
        for (index, column) in first.columns.iter().enumerate() {
            let mut merged = column.clone();
            for other in schemas
                .iter()
                .skip(1)
                .filter_map(|schema| schema.columns.get(index))
            {
                if merged.exact != other.exact {
                    merged.exact = None;
                }
                if merged.identity != other.identity {
                    // Ids of different types name different objects; the
                    // branch column is part of the union's identity.
                    merged.identity = None;
                }
                merged.origins.extend(other.origins.iter().cloned());
                merged.markings.extend(other.markings.iter().cloned());
                merged.fanned |= other.fanned;
                if merged.currency != other.currency {
                    // Branches read different stored properties: summing
                    // across them needs a common column, never inferred.
                    merged.currency = merged.currency.map(|_| Origin::new("", ""));
                }
            }
            columns.push(merged);
        }
        let mut grain = vec![union.branch.clone()];
        grain.extend(first.grain);
        Some(AdmittedNode {
            step: Step::Union {
                inputs: union.inputs.clone(),
                branch: union.branch.clone(),
            },
            schema: NodeSchema { columns, grain },
        })
    }

    fn dedupe(&mut self, dedupe: &norn_ir::DedupeNode, at: &str) -> Option<AdmittedNode> {
        let schema = self.schema(&dedupe.input)?.clone();
        if dedupe.key.is_empty() {
            self.fail(
                codes::INVALID_PLAN,
                "a dedupe names at least one key column",
                format!("{at}/key"),
            );
            return None;
        }
        for (index, key) in dedupe.key.iter().enumerate() {
            match schema.column(key) {
                Some(found) if scalar(&found.ty) => {}
                Some(_) => self.fail(
                    codes::TYPE_MISMATCH,
                    format!("key {key:?} is not a scalar"),
                    format!("{at}/key/{index}"),
                ),
                None => self.fail(
                    codes::UNKNOWN_COLUMN,
                    format!("column {key:?} is not in the input row"),
                    format!("{at}/key/{index}"),
                ),
            }
        }
        let order_by = self.order(&schema, &dedupe.key, &dedupe.order_by, at)?;
        // A value is no longer repeated once its owner's id is in the key.
        let owners: BTreeSet<String> = dedupe
            .key
            .iter()
            .filter_map(|key| schema.column(key))
            .filter_map(|column| column.exact.as_ref())
            .filter(|origin| origin.property == "id")
            .map(|origin| origin.alias.clone())
            .collect();
        let columns = schema
            .columns
            .iter()
            .map(|column| {
                let owned = !column.origins.is_empty()
                    && column
                        .origins
                        .iter()
                        .all(|origin| owners.contains(&origin.alias));
                Column {
                    fanned: column.fanned && !owned,
                    ..column.clone()
                }
            })
            .collect();
        Some(AdmittedNode {
            step: Step::Dedupe {
                input: dedupe.input.clone(),
                key: dedupe.key.clone(),
                order_by,
                keep: dedupe.keep,
            },
            schema: NodeSchema {
                columns,
                grain: dedupe.key.clone(),
            },
        })
    }
}

/// `input.<column>` exactly, with optional surrounding whitespace.
fn passthrough(source: &str) -> Option<&str> {
    let name = source.trim().strip_prefix("input.")?;
    valid_name(name).then_some(name)
}

/// The column type of a computed scalar.
fn column_type(inferred: &InferredType) -> Option<Type> {
    if let InferredType::Optional(inner) = inferred {
        return Some(Type::Optional(Box::new(column_type(inner)?)));
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
