//! Serializable population declarations for future authenticated export.
//! These contracts alone grant no source admission, receipt proof or disclosure.
use serde::{Deserialize, Serialize};

/// One predeclared object/time sample. Cursors select complete committed
/// incarnations, never caller-provided property values or receipt timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sample {
    /// Stable object identity.
    pub key: String,
    /// Feature knowledge cutoff in UTC epoch microseconds.
    pub at: i64,
    /// Latest committed object incarnation at this source cursor.
    pub feature_cursor: u64,
    /// Outcome incarnation at this later source cursor.
    pub outcome_cursor: u64,
}

/// Serializable declaration of a bounded historical analytical population.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportPlan {
    /// One concrete ontology object type in this first profile.
    pub object_type: String,
    /// Direct numeric feature properties, in order.
    pub features: Vec<String>,
    /// Direct numeric outcome property.
    pub outcome: String,
    /// Required UTC timestamp property providing valid time for each incarnation.
    pub event_time: String,
    /// Maximum feature age at each row's knowledge cutoff.
    pub max_age_micros: i64,
    /// Extraction's knowledge cutoff; outcomes must already be recorded by it.
    pub as_of: i64,
    /// Exact declared population; an unavailable or unreadable member refuses all.
    pub samples: Vec<Sample>,
}
