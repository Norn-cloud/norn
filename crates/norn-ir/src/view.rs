//! Typed ViewIR v0 root: the payload of a `derived` artifact (T1-IR-1d, #2509).
//!
//! The rulings' "View" primitive is carried on the wire by the existing
//! `derived` kind; the kind string is unchanged (folding View into Widget is
//! #2633). A derived view is pure, deterministic, and recompute-on-read over
//! Source-bearing entity/relation facts and other derived views; no IO, no
//! nondeterminism, no side effects.
//!
//! The shape mirrors today's validator exactly; nested objects keep unchecked
//! members in `additional` (see [`crate::members`]).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, WholeNumber, present_non_null};
use crate::metric::TimeGrain;
use crate::rule::JsonObject;

/// Primitive-basis classification of an artifact as Source, Derived, Mutation, or Effect.
/// Derived views must be pure, deterministic, and declared over Source facts and other views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveBasisClass {
    Source,
    Derived,
    Mutation,
    Effect,
}

/// Identifies a Source-bearing entity or relation, or another Derived view, that a
/// Derived view reads. References are exact-versioned to make the read-set computable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewInputRef {
    /// Artifact id of the source entity, relation, or derived view being read.
    pub artifact_id: String,
    /// Kind of the referenced artifact.
    pub kind: ViewInputKind,
    /// Exact version of the referenced artifact.
    pub version: String,
    /// Alias used in the calc expression (e.g. "invoice", "`tax_rate`"); unique per view.
    pub alias: String,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Artifact kinds a derived view may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewInputKind {
    Entity,
    Relation,
    Derived,
}

/// `mode: "current"` — resolve against latest committed facts (default).
/// `mode: "as_of_param"` — resolve against the fact log up to a caller-supplied timestamp.
///   Views with this mode MUST list an input named "`as_of`" in their read-set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemporalScopeMode {
    Current,
    AsOfParam,
}

/// Optional time-dimension declaration for a Derived view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemporalScope {
    pub mode: TemporalScopeMode,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Maintenance strategy. Only `recompute_on_read` is supported in v0.
/// `incremental` is reserved for a future IVM phase and MUST be rejected at extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewMaintenance {
    RecomputeOnRead,
    /// Reserved — not implemented. The checker rejects artifacts carrying this value.
    Incremental,
}

/// Serializable calc expression leaf for a Derived view.
///
/// ViewIR v0 admits the `field_projection` form; ViewIR v1 adds `relational`
/// (a closed, typed operator plan). `effectful_ts` is decodable so the checker
/// can reject it (IO belongs in an Effect artifact); `raw_promise` is never
/// admitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CalcExpr {
    /// Simple field projection: select a non-empty subset of named fields from the primary input.
    FieldProjection {
        fields: Vec<String>,
        /// Members the validator does not check, kept verbatim.
        #[serde(flatten)]
        additional: JsonObject,
    },
    /// Effectful TS leaf reference. Rejected inside a Derived view.
    EffectfulTs { source_ref: String },
    /// ViewIR v1 (VIEW-TYPE-1): a closed relational plan over the declared
    /// inputs. Every node is typed by the checker against the effective
    /// catalog; nothing in it is executed raw.
    Relational(RelationalPlan),
}

/// ViewIR v1: a DAG of closed, typed operators. `output` names the node whose
/// rows the view returns. Node ids are local to the view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationalPlan {
    /// Operators by node id.
    pub nodes: BTreeMap<String, ViewNode>,
    /// The node whose rows are the view's rows.
    pub output: String,
}

/// One closed relational operator (VIEW-TYPE-1). No arbitrary function node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ViewNode {
    /// Every readable row of one declared input; its grain is the object id.
    Scan(ScanNode),
    /// Rows whose ZEN predicate is `true`.
    Filter(FilterNode),
    /// Exactly the listed columns, each a typed ZEN expression over the row.
    Project(ProjectNode),
    /// A join along a declared link or a declared key equivalence (VIEW-JOIN-1).
    Join(JoinNode),
    /// Grouped measures under the metric layer's additivity laws (VIEW-AGG-1).
    Aggregate(AggregateNode),
    /// Ranking, offsets and running totals over a total order (VIEW-ORDER-1).
    Window(WindowNode),
    /// Same-shaped inputs, tagged by branch (VIEW-UNION-1).
    Union(UnionNode),
    /// One row per key, chosen by a total order (VIEW-ORDER-1).
    Dedupe(DedupeNode),
}

/// Reads one declared input by alias.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanNode {
    /// The `inputs[].alias` read.
    pub input: String,
}

/// Keeps the rows for which `predicate` is `true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilterNode {
    /// The node filtered.
    pub input: String,
    /// A Boolean ZEN expression over `input.<column>`.
    pub predicate: String,
}

/// One output column of a projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectColumn {
    /// The output column name.
    pub name: String,
    /// A ZEN expression over `input.<column>`; `input.c` alone passes the
    /// column through with its full contract.
    pub expression: String,
}

/// Outputs exactly the listed columns; the input grain must pass through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectNode {
    /// The node projected.
    pub input: String,
    /// The output columns, in order.
    pub columns: Vec<ProjectColumn>,
}

/// How a join pairs rows. Ad-hoc joins on arbitrary columns do not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JoinOn {
    /// Along a declared link of the type whose ids a left column holds.
    Link(LinkJoin),
    /// A declared key equivalence: left columns equal, under the key's own
    /// normalization, a declared unique key (or the id) of the right type.
    Key(KeyJoin),
}

/// A join along a declared forward link role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkJoin {
    /// The left column holding the link's source object ids.
    pub column: String,
    /// The declared forward link role.
    pub link: String,
}

/// A join on a declared key of the right type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyJoin {
    /// Left columns, in the key's component order.
    pub left: Vec<String>,
    /// A key declared by the right type, or `id` for its identity.
    pub right: String,
}

/// Inner or left outer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JoinKind {
    /// Only matched pairs.
    Inner,
    /// Every left row; unmatched right columns are null.
    Left,
}

/// Joins two nodes; right columns are renamed with `prefix`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinNode {
    /// The left node.
    pub left: String,
    /// The right node: rows of one object type, at its identity grain.
    pub right: String,
    /// The declared relationship.
    pub on: JoinOn,
    /// Inner or left.
    pub kind: JoinKind,
    /// Prefix for every right column; output names must stay unique.
    pub prefix: String,
}

/// One grouping key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupKey {
    /// The grouped column.
    pub column: String,
    /// A calendar bucket for a date or timestamp column (ISO/UTC).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TimeGrain")]
    pub grain: Option<TimeGrain>,
}

/// An aggregate function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AggregateFn {
    /// Exact sum of an additive column.
    Sum,
    /// Rows, or non-null values of `column`.
    Count,
    /// Distinct non-null values of `column`.
    CountDistinct,
    /// Least non-null value.
    Min,
    /// Greatest non-null value.
    Max,
}

/// One measure of an aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Measure {
    /// The output column name.
    pub name: String,
    /// The function.
    #[serde(rename = "fn")]
    pub function: AggregateFn,
    /// The measured column; omitted only for a row `count`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub column: Option<String>,
}

/// Groups rows; the output grain is the group keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregateNode {
    /// The node aggregated.
    pub input: String,
    /// Group keys; empty means one row over the whole input.
    pub group_by: Vec<GroupKey>,
    /// Non-empty measures.
    pub measures: Vec<Measure>,
}

/// Sort direction. Nulls sort last in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// One order key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrderKey {
    /// The ordered column.
    pub column: String,
    /// Its direction.
    pub direction: SortDirection,
}

/// A window function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowFn {
    /// 1, 2, 3, … in order.
    RowNumber,
    /// Rank with gaps over equal order values.
    Rank,
    /// Rank without gaps.
    DenseRank,
    /// `column` of the row `offset` rows earlier; null before the first.
    Lag,
    /// `column` of the row `offset` rows later; null after the last.
    Lead,
    /// Exact sum of an additive column from the partition start to this row.
    RunningSum,
}

/// One window column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowColumn {
    /// The output column name.
    pub name: String,
    /// The function.
    #[serde(rename = "fn")]
    pub function: WindowFn,
    /// The column read by `lag`, `lead` and `running_sum`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub column: Option<String>,
    /// Rows back or ahead for `lag`/`lead` (default 1).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WholeNumber")]
    pub offset: Option<WholeNumber>,
}

/// Adds window columns; the grain is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowNode {
    /// The node windowed.
    pub input: String,
    /// Partition columns.
    pub partition_by: Vec<String>,
    /// Order keys; with the partition they must cover the input grain.
    pub order_by: Vec<OrderKey>,
    /// Non-empty window columns.
    pub columns: Vec<WindowColumn>,
}

/// Concatenates same-shaped nodes; `branch` names the input node of each row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnionNode {
    /// Two or more node ids.
    pub inputs: Vec<String>,
    /// The output column holding each row's input node id.
    pub branch: String,
}

/// Which row of a key a dedupe keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DedupeKeep {
    /// The first row in order.
    First,
    /// The last row in order.
    Last,
}

/// One row per key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DedupeNode {
    /// The node deduplicated.
    pub input: String,
    /// The key columns; the output grain.
    pub key: Vec<String>,
    /// Order keys; with the key they must cover the input grain.
    pub order_by: Vec<OrderKey>,
    /// Which row is kept.
    pub keep: DedupeKeep,
}

/// ViewIR v0 payload — the typed View root of a `derived` artifact.
///
/// Constraints enforced by `norn-checker`:
/// - `inputs` must be non-empty (a view with no inputs is a static constant — use a `setting`).
/// - `calc` must not be a `CalcExpr::EffectfulTs` variant (IO in a view is rejected).
/// - `maintenance` must be `recompute_on_read` (`incremental` is rejected as not-yet-supported).
/// - If `temporal_scope.mode == "as_of_param"`, one input alias must be "`as_of`".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewIrPayload {
    pub view_version: IrVersion01,
    pub view_id: String,
    pub inputs: Vec<ViewInputRef>,
    pub calc: CalcExpr,
    pub output_schema: JsonObject,
    pub maintenance: ViewMaintenance,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TemporalScope")]
    pub temporal_scope: Option<TemporalScope>,
    /// Scope dimensions the caller must supply (fail closed). Optional; when
    /// present, a non-empty list of non-empty strings (checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    pub scope_required: Option<Vec<String>>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}
