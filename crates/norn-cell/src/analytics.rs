//! D102: the tenant analytics engine's typed contract.
//!
//! The cell decides who may read what, lowers an admitted plan and verifies
//! every answer; the engine (`DuckDB` in a native sidecar, the `engine` module) only
//! executes these typed operations over one tenant's own database. No
//! operation carries SQL, a table path or a column expression (#733): the
//! engine builds its statements from admitted names and binds every value.
//!
//! - ANALYTICS-TENANT-1: a tenant's database is named by the SHA-256 of the
//!   cell scope key the cell verified ([`TenantId`]); the engine never reads
//!   claims and never opens another tenant's file for a request.
//! - ANALYTICS-FRONTIER-1: the governed-fact mirror keeps every object
//!   version with its commit interval, so an answer at frontier `F` reads
//!   exactly the versions current at `F`, whatever was synced after it.
//! - MEMBERSHIP-DIGEST-1: a bucket's membership digest is
//!   [`MembershipDigest`] over its members in identity order; recomputing it
//!   over the paged members gives the same digest.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};
use transparency_sha2::{Digest as _, Sha256};

mod assemble;
mod dataset;
#[cfg(all(feature = "duckdb", feature = "native"))]
pub mod engine;
mod lower;
mod mirror;

pub use assemble::{Prepared, bucket_value};
pub use dataset::DatasetRecord;
pub use lower::{lower, table_schema};
pub use mirror::MirrorBatch;

/// A tenant's analytics database: the hex SHA-256 of its verified cell scope key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// ANALYTICS-TENANT-1: the database of the cell bound to `scope_key`.
    #[must_use]
    pub fn of_scope(scope_key: &str) -> Self {
        Self(hex(&Sha256::digest(scope_key.as_bytes())))
    }

    /// The file stem; 64 lowercase hex digits, so never a path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TenantId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(value))
        } else {
            Err("a tenant is 64 lowercase hex digits".to_owned())
        }
    }
}

impl From<TenantId> for String {
    fn from(value: TenantId) -> Self {
        value.0
    }
}

/// The hex SHA-256 of `bytes`.
#[must_use]
pub fn hex_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _infallible = write!(out, "{b:02x}");
        out
    })
}

/// How a property is stored and compared in the analytics database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ColumnKind {
    /// Text and enum values: compared and grouped by their exact text.
    Text,
    /// An integer, stored exactly as `DECIMAL(38,0)`.
    Integer,
    /// A decimal at its declared scale, stored exactly as `DECIMAL(38,scale)`.
    Decimal {
        /// Declared fractional digits, at most 37.
        scale: u32,
    },
    /// A boolean.
    Boolean,
    /// An RFC 3339 timestamp: its text and its exact UTC ordering key.
    Timestamp,
    /// A calendar date: its text and the ordering key of its UTC midnight.
    Date,
    /// Any other value, kept as canonical JSON; it can be exported but never
    /// filtered, grouped or summed.
    Opaque,
}

/// A pointwise law a property declares: every present value must satisfy it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "law", content = "v")]
pub enum PointLaw {
    /// One of the declared enum values.
    OneOf(Vec<String>),
    /// At least this canonical decimal.
    Min(String),
    /// At most this canonical decimal.
    Max(String),
    /// At least this many Unicode scalar values.
    MinLength(u64),
    /// At most this many Unicode scalar values.
    MaxLength(u64),
    /// A constraint the engine does not evaluate; a load of the column refuses.
    Unchecked,
}

/// One analytical column, named by its property id. (Its kind is
/// flattened, so unknown fields cannot be denied here.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    /// The property id.
    pub id: String,
    /// Storage and comparison.
    #[serde(flatten)]
    pub kind: ColumnKind,
    /// Whether a value may be absent.
    pub nullable: bool,
    /// BULK-LOAD-1: the property's declared pointwise laws, checked at load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub laws: Vec<PointLaw>,
}

/// An object type's analytical table, pinned to the definitions it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableSchema {
    /// The object type (the root type, for the governed mirror).
    pub name: String,
    /// Columns in declared order.
    pub columns: Vec<Column>,
    /// Content identity of the columns; a changed pin rebuilds the table.
    pub pin: String,
}

/// One stored value, already checked by the cell against its property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "t", content = "v")]
pub enum Value {
    /// Absent.
    Null,
    /// A boolean.
    Bool(bool),
    /// A canonical exact decimal.
    Number(String),
    /// Text, kept as given.
    Text(String),
    /// Timestamp or date text with its exact ordering key.
    Instant {
        /// The stored text.
        text: String,
        /// Whole UTC seconds.
        secs: i64,
        /// Nanoseconds within the second.
        nanos: u32,
    },
    /// Canonical JSON of any other value.
    Json(String),
}

/// One committed object version for the governed mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MirrorRow {
    /// The root type's table.
    pub table: String,
    /// The concrete object type at this version.
    pub object_type: String,
    /// Object identity within the root type.
    pub id: String,
    /// Object version.
    pub version: u64,
    /// The commit that wrote this version.
    pub commit_seq: u64,
    /// Tenant scope the writer recorded.
    pub scope: String,
    /// Values aligned with the table's columns; `None` is a tombstone.
    pub values: Option<Vec<Value>>,
    /// A value failed its property's check: a question reading this row refuses.
    pub bad: bool,
}

/// The mirror's state for one world: the last commit applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontierRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// The execution world.
    pub world: String,
    /// The current schemas of every mirrored root type; any changed pin
    /// clears the world's mirror, so it is synced again from the start.
    pub tables: Vec<TableSchema>,
}

/// The last commit the mirror applied for the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontierResponse {
    /// Commit sequence; 0 before the first sync.
    pub frontier: u64,
}

/// Apply committed versions `(after, through]` to the mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// The execution world.
    pub world: String,
    /// The mirror's frontier this batch continues; any other refuses.
    pub after: u64,
    /// The commit this batch completes.
    pub through: u64,
    /// Every version committed in `(after, through]`, in commit order.
    pub rows: Vec<MirrorRow>,
}

/// Where an analytical question reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum Source {
    /// Governed facts at an exact commit frontier.
    Mirror {
        /// The execution world.
        world: String,
        /// The root type.
        table: String,
        /// The commit the answer is exact at.
        frontier: u64,
        /// The cell's scope; a row of any other scope refuses as stale.
        scope: String,
        /// Identities the cell excludes (open findings).
        #[serde(default)]
        excluded: Vec<String>,
    },
    /// A loaded dataset at one load.
    Dataset {
        /// The dataset's object type.
        table: String,
        /// The load the answer reads; any other refuses as stale.
        load: String,
    },
}

/// A literal a filter compares with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "t", content = "v")]
pub enum Literal {
    /// A boolean.
    Bool(bool),
    /// A canonical exact decimal.
    Number(String),
    /// Text.
    Text(String),
}

/// What one filter tests on its column, with the question's equality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "t", content = "v")]
pub enum Test {
    /// The value is absent.
    IsNull,
    /// The value equals the literal (an absent value never does).
    Equals(Literal),
    /// A timestamp value has this ordering key (an absent value never does).
    Instant {
        /// Whole UTC seconds.
        secs: i64,
        /// Nanoseconds.
        nanos: u32,
    },
    /// The literal can never equal a present value (another value kind).
    NeverPresent,
    /// The literal is not a timestamp: a row reaching this filter with a
    /// present value refuses the question.
    RefusePresent,
}

/// One equality filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    /// The property.
    pub column: String,
    /// `ne` rather than `eq`.
    pub negate: bool,
    /// The test.
    pub test: Test,
}

/// The half-open window on the effective-time property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Window {
    /// The timestamp or date property.
    pub column: String,
    /// Inclusive start `(seconds, nanoseconds)`.
    pub start: (i64, u32),
    /// Exclusive end.
    pub end: (i64, u32),
}

/// A calendar period, in UTC; weeks are ISO weeks from Monday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grain {
    /// A day.
    Day,
    /// An ISO week.
    Week,
    /// A month.
    Month,
    /// A quarter.
    Quarter,
    /// A year.
    Year,
}

/// One output dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum Dimension {
    /// The property's value.
    Column {
        /// The property.
        column: String,
    },
    /// The first UTC day of the period holding the property's instant.
    Period {
        /// The timestamp or date property.
        column: String,
        /// The period.
        grain: Grain,
    },
}

/// One summed operand: its property and its own filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operand {
    /// The decimal or integer property.
    pub column: String,
    /// Filters this operand alone applies.
    pub filters: Vec<Filter>,
}

/// METRIC-SNAPSHOT-ASOF-1: a semi-additive sum reads, per period, each
/// current subject's last version before the period's end (by valid time,
/// then version), never summing a subject over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsOf {
    /// The valid-time property the versions are ordered by.
    pub column: String,
    /// Each period's label (none without a time grouping) and its exclusive end.
    pub periods: Vec<(Option<String>, (i64, u32))>,
}

/// QUERY-ENGINE-1: an admitted keyed sum or ratio, lowered from the shared
/// plan with the shared projection's semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lowered {
    /// The question's filters, applied first.
    pub filters: Vec<Filter>,
    /// The time window of the first operand's effective time.
    pub window: Window,
    /// The grouping, in the question's order: none buckets everything
    /// together, one by its value, more by the tuple of their values.
    pub dimensions: Vec<Dimension>,
    /// One operand, or numerator then denominator.
    pub operands: Vec<Operand>,
    /// A snapshot sum's periods; its time grouping is the period's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<AsOf>,
}

/// Evaluate a lowered question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluateRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// What it reads.
    pub source: Source,
    /// The lowered question.
    pub query: Lowered,
    /// Members are returned inline when the population has at most this many
    /// rows (the plan's bound); otherwise each bucket carries its count and
    /// membership digest and members are paged.
    pub inline_members: u64,
}

/// A bucket's grouping value as the engine grouped it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "t", content = "v")]
pub enum GroupKey {
    /// No value (ungrouped, or a null dimension).
    Null,
    /// A boolean.
    Bool(bool),
    /// A canonical decimal.
    Number(String),
    /// The text of the bucket's first member by identity.
    Text(String),
    /// The first day of a period, in days since 1970-01-01.
    Period {
        /// The period.
        grain: Grain,
        /// Days since the epoch.
        day: i64,
    },
    /// The values of several dimensions, in the question's order.
    Tuple(Vec<GroupKey>),
}

/// One contributing member: identity, version and operand coefficients
/// (millionths; `None` when excluded by its filters or null).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberRow {
    /// Object identity (or a dataset row's index).
    pub id: String,
    /// Object version (a dataset row's is 1).
    pub version: u64,
    /// Coefficients per operand.
    pub amounts: Vec<Option<i64>>,
}

/// One bucket's exact totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BucketTotals {
    /// The grouping value.
    pub group: GroupKey,
    /// Per operand, the exact sum of coefficients.
    pub totals: Vec<String>,
    /// Per operand, the sum of coefficient magnitudes (bounds every partial sum).
    pub magnitude: Vec<String>,
    /// Per operand, members whose measured value was null.
    pub nulls: Vec<u64>,
    /// Contributing members.
    pub count: u64,
    /// MEMBERSHIP-DIGEST-1 over the members in identity order.
    pub digest: String,
    /// The members, when the population is within the inline bound.
    pub members: Option<Vec<MemberRow>>,
}

/// The answer to an [`EvaluateRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluateResponse {
    /// Rows in the population the question read.
    pub population: u64,
    /// Excluded identities that were in the population.
    pub excluded: Vec<String>,
    /// Buckets in group order.
    pub buckets: Vec<BucketTotals>,
}

/// Page one bucket's members in identity order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembersRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// What the answer read.
    pub source: Source,
    /// The same lowered question.
    pub query: Lowered,
    /// The bucket.
    pub group: GroupKey,
    /// Start after this identity.
    pub after: Option<String>,
    /// Page size.
    pub limit: u32,
}

/// One page of members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembersResponse {
    /// Members after the cursor, in identity order.
    pub members: Vec<MemberRow>,
}

/// A dataset file's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    /// RFC 4180 CSV with a header row; an empty field is null.
    Csv,
    /// Apache Parquet.
    Parquet,
}

/// The file a load reads: a presigned URL for one object, and its identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSource {
    /// A short-lived GET URL for exactly this object.
    pub url: String,
    /// The object's durable URI, recorded as lineage.
    pub uri: String,
    /// The SHA-256 the cell recorded; any other content refuses.
    pub sha256: String,
    /// The format.
    pub format: FileFormat,
}

/// BULK-LOAD-1: load a dataset file into its object type's table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// The admitted object type's table.
    pub table: TableSchema,
    /// The load identity the cell assigned.
    pub load: String,
    /// The file.
    pub file: FileSource,
    /// The file's column (CSV header or Parquet field) for each column id
    /// that is named differently; any other column is read by its id.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// The outcome of a load: rows admitted and rows quarantined, by reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadResponse {
    /// The load identity.
    pub load: String,
    /// Rows admitted.
    pub rows: u64,
    /// Rows quarantined.
    pub rejected: u64,
    /// Quarantined rows per reason code.
    pub reasons: BTreeMap<String, u64>,
    /// File size.
    pub bytes: u64,
    /// Wall time of the load, for the record.
    pub millis: u64,
}

/// Profile columns of a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// What it reads.
    pub source: Source,
    /// The properties to profile, in order.
    pub columns: Vec<String>,
}

/// The percentiles a profile reports, by nearest rank.
pub const PROFILE_QUANTILES: [u32; 5] = [0, 25, 50, 75, 100];
/// Equal-population bins of a numeric histogram.
pub const PROFILE_BINS: u64 = 10;
/// A temporal histogram counts per day up to this many days, else per month.
pub const PROFILE_DAY_BINS: usize = 62;
/// Outliers lie more than this many interquartile ranges outside the quartiles.
pub const PROFILE_OUTLIER_K: u8 = 3;
/// Most outlier rows a profile names.
pub const PROFILE_OUTLIER_EXAMPLES: u64 = 20;

/// The 1-based rank of nearest-rank `percent` among `n` sorted values.
#[must_use]
pub fn nearest_rank(percent: u32, n: u64) -> u64 {
    u64::from(percent).saturating_mul(n).div_ceil(100).max(1)
}

/// The `[first, last]` 1-based ranks of each non-empty equal-population bin
/// of `n` sorted values.
#[must_use]
pub fn bin_ranks(n: u64) -> Vec<(u64, u64)> {
    (0..PROFILE_BINS)
        .filter_map(|bin| {
            let start = bin.saturating_mul(n) / PROFILE_BINS;
            let end = bin.saturating_add(1).saturating_mul(n) / PROFILE_BINS;
            (end > start).then(|| (start.saturating_add(1), end))
        })
        .collect()
}

/// Every rank a profile of `n` sorted values reads.
#[must_use]
pub fn profile_ranks(n: u64) -> std::collections::BTreeSet<u64> {
    let mut ranks: std::collections::BTreeSet<u64> = PROFILE_QUANTILES
        .iter()
        .map(|p| nearest_rank(*p, n))
        .collect();
    for (first, last) in bin_ranks(n) {
        ranks.insert(first);
        ranks.insert(last);
    }
    ranks
}

/// Tukey's far-out values of a numeric column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutlierSummary {
    /// `q1 − k·(q3 − q1)`, canonical.
    pub lower: String,
    /// `q3 + k·(q3 − q1)`, canonical.
    pub upper: String,
    /// Rows outside a fence.
    pub count: u64,
    /// `(id, value)` of the furthest from their fence first, then by id.
    pub examples: Vec<(String, String)>,
}

/// One column's profile over the source's rows, as the exact profile reads
/// it: counts, distinct values, and for ordered columns the sorted values at
/// [`profile_ranks`] (temporal columns sorted by their text), day or month
/// label counts of temporal columns, and numeric outliers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColumnProfile {
    /// The property.
    pub column: String,
    /// Rows with a value.
    pub count: u64,
    /// Rows without one.
    pub nulls: u64,
    /// Distinct present values (text compared as written).
    pub distinct: u64,
    /// Sorted values at their 1-based ranks.
    pub ranks: BTreeMap<u64, Value>,
    /// `day` or `month` and the count per label (temporal columns).
    pub labels: Option<(String, Vec<(String, u64)>)>,
    /// Numeric columns with a computable fence.
    pub outliers: Option<OutlierSummary>,
}

/// The profile of a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileResponse {
    /// Rows profiled.
    pub rows: u64,
    /// Rows the cell excluded (open quarantining findings).
    pub quarantined: u64,
    /// Per requested column.
    pub columns: Vec<ColumnProfile>,
}

/// An export's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Apache Parquet; decimals keep their declared scale.
    Parquet,
    /// One JSON object per line.
    Jsonl,
}

/// Export columns of every row of a source, with identity and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRequest {
    /// The tenant.
    pub tenant: TenantId,
    /// What it reads.
    pub source: Source,
    /// The properties, in order.
    pub columns: Vec<String>,
    /// The format.
    pub format: ExportFormat,
}

/// A typed refusal from the engine: one of the Core 03 compute codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineRefusal {
    /// `core.compute.*`, `analytics.*` or `bulk.*`.
    pub code: String,
    /// What was refused, for the operator; never row data.
    pub detail: String,
}

/// MEMBERSHIP-DIGEST-1: SHA-256 over each member in identity (byte) order,
/// each written as `<identity bytes>:<identity>,<version>` then
/// `,<coefficient>` per operand (empty when absent), then `;`. The length
/// prefix makes the encoding unambiguous whatever an identity contains, and
/// the engine computes the same bytes in SQL.
#[derive(Clone, Default)]
pub struct MembershipDigest(Sha256);

impl MembershipDigest {
    /// Absorb the next member; callers feed members in identity order.
    pub fn push(&mut self, member: &MemberRow) {
        let mut line = format!("{}:{},{}", member.id.len(), member.id, member.version);
        for amount in &member.amounts {
            line.push(',');
            if let Some(n) = amount {
                line.push_str(&n.to_string());
            }
        }
        line.push(';');
        self.0.update(line.as_bytes());
    }

    /// The hex digest.
    #[must_use]
    pub fn finish(self) -> String {
        hex(&self.0.finalize())
    }

    /// The digest of `members`, which must be in identity order.
    #[must_use]
    pub fn of(members: &[MemberRow]) -> String {
        let mut digest = Self::default();
        for member in members {
            digest.push(member);
        }
        digest.finish()
    }
}

/// The runtime value of a stored analytical value.
///
/// # Errors
/// A number that is not an exact decimal.
pub fn runtime_value(value: &Value) -> Result<RuntimeValue, norn_ir::RuntimeValueError> {
    Ok(match value {
        Value::Null => RuntimeValue::Null,
        Value::Bool(flag) => RuntimeValue::Bool(*flag),
        Value::Number(text) => RuntimeValue::Number(norn_ir::Decimal::parse(text)?),
        Value::Text(text) | Value::Instant { text, .. } => RuntimeValue::String(text.clone()),
        Value::Json(text) => RuntimeValue::from_json_text(text)?,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "digest property assertions")]
mod tests {
    use super::*;

    proptest::proptest! {
        /// MEMBERSHIP-DIGEST-1: paging in any page size recomputes the digest.
        #[test]
        fn membership_digest_is_page_independent(
            mut members in proptest::collection::vec(
                ("[a-z0-9]{1,8}", 1u64..5, proptest::collection::vec(proptest::option::of(proptest::num::i64::ANY), 1..3)),
                0..60,
            ),
            page in 1usize..9,
        ) {
            members.sort_by(|a, b| a.0.cmp(&b.0));
            members.dedup_by(|a, b| a.0 == b.0);
            let rows: Vec<MemberRow> = members.into_iter().map(|(id, version, amounts)| MemberRow { id, version, amounts }).collect();
            let whole = MembershipDigest::of(&rows);
            let mut paged = MembershipDigest::default();
            for chunk in rows.chunks(page) {
                for member in chunk {
                    paged.push(member);
                }
            }
            proptest::prop_assert_eq!(whole, paged.finish());
        }
    }

    #[test]
    fn tenant_ids_are_scope_digests_never_paths() {
        let tenant = TenantId::of_scope("org_demo/analytics");
        assert_eq!(tenant.as_str().len(), 64);
        assert!(TenantId::try_from("../etc".to_owned()).is_err());
        assert!(TenantId::try_from("A".repeat(64)).is_err());
    }
}
