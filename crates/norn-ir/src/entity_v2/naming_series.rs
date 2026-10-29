//! Declarative business-numbering series attached to an entity key.
//!
//! The IR records a pattern and its scoping/reset rules. It does not contain
//! allocation logic or tenant counter state; counters are facts owned by a
//! tenant cell and travel separately as [`NamingCounterSeed`] values.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::object::{KeyName, ObjectTypeName, PropertyId};

/// A naming-series declaration attached to one generated entity key.
///
/// `pattern` is a sequence of literal text and brace tokens: `{YYYY}`, `{YY}`,
/// `{MM}`, `{DD}`, `{#####}` (one to eighteen `#` characters), and
/// `{property_id}` for every property in `scope_fields`. The checker validates
/// the grammar and cross-field constraints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamingSeriesIR {
    /// Literal text and supported placeholders for the generated key value.
    pub pattern: String,
    /// Explicit IANA timezone used to derive date tokens from the commit time.
    pub timezone: String,
    /// Required text properties substituted by matching `{property_id}`
    /// tokens. Omitted when the series has no scope fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_fields: Vec<PropertyId>,
    /// Counter reset boundary. Reset periods are rendered in `timezone`.
    pub reset_policy: NamingResetPolicy,
    /// Whether committed counter allocations must be contiguous within one
    /// commit or may leave gaps under the runtime policy.
    pub gap_policy: NamingGapPolicy,
}

/// The local calendar boundary at which a naming-series counter resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamingResetPolicy {
    /// Keep one counter for all time, regardless of date tokens in the pattern.
    Never,
    /// Start a new counter for each local calendar year.
    Year,
    /// Start a new counter for each local calendar month.
    Month,
    /// Start a new counter for each local calendar day.
    Day,
}

/// Whether allocations may leave gaps in the business numbering sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamingGapPolicy {
    /// Counter advancement is committed only with the containing create
    /// transaction, so a refused or aborted commit consumes no number.
    GaplessWithinCommit,
    /// The allocator may skip a candidate number already occupied by an
    /// imported or existing key while completing the same atomic create.
    AllowGaps,
}

/// Tenant-owned counter state exported or restored independently from the
/// definition IR. `current` is the last committed counter value (`0` before
/// the first allocation). `period` is empty for `never`, otherwise the local
/// period key: `YYYY`, `YYYY-MM`, or `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamingCounterSeed {
    /// Entity type that owns the generated key.
    pub object_type: ObjectTypeName,
    /// Key declaration carrying the naming series.
    pub key_name: KeyName,
    /// Exact scope values for this counter partition.
    pub scope: BTreeMap<PropertyId, String>,
    /// Local reset period, or empty when `resetPolicy` is `never`.
    pub period: String,
    /// Last committed counter value in this scope and period.
    pub current: u64,
}
