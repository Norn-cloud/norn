//! Result/frontier contracts; producing evaluators must qualify their snapshot pins.
use std::collections::BTreeMap;

use crate::LedgerHead;
use norn_ir::{Decimal, ObjectTypeName, RuntimeValue, UtcTimestamp};
use serde::{Deserialize, Serialize};

/// Connector instance → partition → opaque consumed cursor.
pub type Frontier = BTreeMap<String, BTreeMap<String, String>>;
/// Observed source progress, supplied by the trusted connector host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Watermark {
    /// Connector's observed high-water cursor; only equality is used here.
    pub observed: String,
    /// Last authenticated heartbeat; absent means unavailable coverage.
    pub heartbeat_ms: Option<u64>,
    /// Source coverage is known through the consumed cursor.
    pub complete: bool,
}
/// Host-resolved query dependencies and authorized population, never client authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryContext {
    /// Selected execution world.
    pub world: String,
    /// Cell-bound tenant scope.
    pub scope: String,
    /// Authorized population identity and revocation epoch.
    pub population_pin: String,
    /// Server authorization epoch.
    pub authorization_epoch: u64,
    /// Admitted mapping version/digest.
    pub mapping_pin: String,
    /// Host-observed clock, not render time.
    pub now_ms: u64,
    /// Required source frontier; unrelated sources are omitted.
    pub source_cursors: Frontier,
    /// Watermarks for required dependencies.
    pub watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
    /// Core 07 disposition: excluded identity → finding/evidence pin.
    pub exclusions: BTreeMap<String, String>,
}
/// Declared execution strategy; qualification belongs to the producing evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Exact cell snapshot.
    Cell,
}
/// Snapshot freshness, independent of source cursor ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Complete and within declared source freshness.
    Exact,
    /// Known coverage at an aged frontier.
    Stale,
    /// Missing coverage or explicit exclusions.
    Incomplete,
}
/// One numeric contribution with original object-version identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Member {
    /// Original cell object version.
    pub version: u64,
    /// Operand contributions; none means its predicate excluded this identity.
    pub amounts: Vec<Option<Decimal>>,
}
/// One output bucket, retaining exact sums beside its ratio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRow {
    /// Scalar grouping key.
    pub bucket: RuntimeValue,
    /// One sum, or numerator then denominator.
    pub totals: Vec<Decimal>,
    /// Null means a zero-denominator ratio is unavailable.
    pub value: Option<Decimal>,
    /// Exact aggregate-membership lineage.
    pub members: BTreeMap<String, Member>,
}
/// Result envelope for QUERY-ENGINE-1; construction alone grants no authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    /// Content identity of model, declarations, bounds and arithmetic profile.
    pub query_pin: String,
    /// Actual model/definition pin.
    pub definition_pin: String,
    /// Physical evaluator.
    pub engine: Engine,
    /// Actual verified ledger head.
    pub revision: LedgerHead,
    /// Exclusive effective cutoff of the admitted question window.
    pub effective_as_of: UtcTimestamp,
    /// Actual knowledge revision; not a fabricated historical timestamp.
    pub known_as_of: u64,
    /// Root contribution identity type.
    pub grain: ObjectTypeName,
    /// Content-addressed membership and source lineage.
    pub lineage_ref: String,
    /// All output buckets.
    pub rows: BTreeMap<String, QueryRow>,
    /// Source freshness and coverage label.
    pub freshness: Freshness,
    /// Known coverage with no quarantined exclusions.
    pub complete: bool,
    /// Server-resolved scope, mappings, frontier and watermarks.
    #[serde(flatten)]
    pub context: QueryContext,
}
