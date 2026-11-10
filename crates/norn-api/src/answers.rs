//! Schemas for the cell's typed-question answers (QUERY-ENGINE-1, ASOF-KNOWLEDGE-1).
//!
//! The documents are produced by `norn_cell::compute`; these types only describe
//! their JSON. Decimals are raw JSON number tokens: read their text, never a
//! binary float, to keep the server's exact value and scale.

use std::collections::BTreeMap;
use utoipa::ToSchema;
use utoipa::openapi::schema::{Array, Object, Ref, Schema, SchemaType, Type};

/// Current or authenticated historical answer, matching the native service envelope.
#[derive(Debug, ToSchema)]
#[serde(untagged)]
pub enum NornQuestionAnswer {
    /// Current answer.
    Live(NornQueryResult),
    /// Historical answer.
    Historical(NornHistoricalQuestionResult),
}

/// One exact answer to an admitted typed question.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornQueryResult {
    /// Content identity of model, declarations, bounds and arithmetic profile.
    pub query_pin: String,
    /// Actual model/definition pin.
    pub definition_pin: String,
    /// Physical evaluator.
    pub engine: NornQueryEngine,
    /// Actual verified ledger head.
    pub revision: NornLedgerHead,
    /// Exclusive effective cutoff of the admitted question window.
    #[schema(schema_with = effective_as_of_schema)]
    pub effective_as_of: String,
    /// Actual knowledge revision (commit count), not a historical timestamp.
    pub known_as_of: u64,
    /// Root contribution identity type.
    pub grain: String,
    /// Opaque handle to this result's membership and source lineage, for its
    /// trace: keyed by the cell (D53), never a content hash of the rows.
    pub lineage_ref: String,
    /// Output buckets, keyed by the canonical JSON of their grouping value.
    pub rows: BTreeMap<String, NornQueryRow>,
    /// Source freshness and coverage label.
    pub freshness: NornQueryFreshness,
    /// Known coverage with no quarantined exclusions.
    pub complete: bool,
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
    /// Host-observed clock in milliseconds, not render time.
    pub now_ms: u64,
    /// Required source frontier: source → partition → cursor.
    pub source_cursors: BTreeMap<String, BTreeMap<String, String>>,
    /// Watermarks for required dependencies: source → partition → watermark.
    pub watermarks: BTreeMap<String, BTreeMap<String, NornQueryWatermark>>,
    /// Quarantined identities excluded from the answer → their finding or evidence pin.
    pub exclusions: BTreeMap<String, String>,
}

/// The evaluator that produced an answer.
#[derive(Debug, Clone, Copy, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NornQueryEngine {
    /// Exact maintained contribution totals.
    Ivm,
    /// Exact cell snapshot.
    Cell,
}

/// Snapshot freshness, independent of source cursor ordering.
#[derive(Debug, Clone, Copy, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NornQueryFreshness {
    /// Complete analytical coverage with admitted source heartbeat lag.
    Fresh,
    /// Complete coverage with zero heartbeat lag.
    Exact,
    /// Known coverage at an aged frontier.
    Stale,
    /// Missing coverage or explicit exclusions.
    Incomplete,
}

/// The verified ledger head an answer was read at.
#[derive(Debug, Clone, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NornLedgerHead {
    /// The last event sequence number (`0` when empty).
    pub seq: u64,
    /// The last event hash for a reader with whole-world Values (D53);
    /// otherwise a keyed handle of it, stable per revision: compare it only for equality.
    pub hash: String,
    /// How many commits.
    pub commits: u64,
}

/// A source partition's observed watermark.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornQueryWatermark {
    /// Connector's observed high-water cursor.
    pub observed: String,
    /// Last authenticated heartbeat; null means unavailable coverage.
    #[schema(required = true, nullable = true)]
    pub heartbeat_ms: Option<u64>,
    /// Source coverage is known through the consumed cursor.
    pub complete: bool,
}

/// One output bucket, with its exact sums beside its value.
#[derive(Debug, Clone, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NornQueryRow {
    /// Scalar grouping value.
    #[schema(schema_with = bucket_schema)]
    pub bucket: serde_json::Value,
    /// One sum, or numerator then denominator.
    #[schema(schema_with = totals_schema)]
    pub totals: Vec<String>,
    /// The bucket's value; null means a zero-denominator ratio is unavailable.
    #[schema(required = true, schema_with = value_schema)]
    pub value: Option<String>,
    /// Exact aggregate membership: contributing object identity → its contribution.
    pub members: BTreeMap<String, NornQueryMember>,
    /// DATA-VALUES-1 (D53): the reader lacks Values on a member and the bucket
    /// has fewer members than the server's declared minimum, so `totals` is
    /// empty and `value` null; membership stays. Absent when false.
    #[serde(default)]
    #[schema(required = false)]
    pub suppressed: bool,
}

/// One object version's contribution to a bucket.
#[derive(Debug, Clone, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NornQueryMember {
    /// Original cell object version.
    pub version: u64,
    /// Operand contributions, in `totals` order; null when withheld (DATA-VALUES-1).
    #[schema(required = true, schema_with = amounts_schema)]
    pub amounts: Option<Vec<Option<String>>>,
    /// The reader holds no Values grant on this member; absent when false.
    #[serde(default)]
    #[schema(required = false)]
    pub restricted: bool,
}

/// An answer read at a knowledge cutoff, beside its historical world.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornHistoricalQuestionResult {
    /// Historical commit authority; source world and scope remain in the result.
    pub world: NornWorld,
    /// Inclusive receipt clock of the knowledge cutoff, in milliseconds.
    pub knowledge_time_ms: u64,
    /// The exact answer, including its effective cutoff and versioned members.
    pub result: NornQueryResult,
}

/// An execution world.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NornWorld {
    /// The tenant's authoritative world.
    Main,
    /// A scenario fork.
    Fork {
        /// The fork's identity.
        id: String,
    },
    /// A read-only view as of one commit.
    Historical {
        /// The commit the view is pinned to.
        commit: String,
    },
}

// `schema_with` replaces the field's schema, doc comment included, so each
// helper restates its field's description.
fn effective_as_of_schema() -> Ref {
    Ref::builder()
        .ref_location_from_schema_name("NornIrUtcTimestamp")
        .description(Some(
            "Exclusive effective cutoff of the admitted question window.",
        ))
        .build()
}

fn bucket_schema() -> Schema {
    let mut schema = Object::with_type(SchemaType::AnyValue);
    schema.description = Some("Scalar grouping value.".to_owned());
    Schema::Object(schema)
}

fn decimal(nullable: bool, description: &str) -> Object {
    let mut schema = Object::with_type(if nullable {
        SchemaType::from_iter([Type::Number, Type::Null])
    } else {
        SchemaType::new(Type::Number)
    });
    schema.description = Some(format!(
        "{description}An exact decimal as a raw JSON number token; read its text, not a binary float."
    ));
    schema
}

fn decimal_list(nullable: bool, description: &str) -> Schema {
    let mut array = Array::new(decimal(nullable, ""));
    array.description = Some(description.to_owned());
    Schema::Array(array)
}

fn value_schema() -> Schema {
    Schema::Object(decimal(
        true,
        "The bucket's value; null means a zero-denominator ratio is unavailable, or the bucket is `suppressed` (DATA-VALUES-1). ",
    ))
}

fn totals_schema() -> Schema {
    decimal_list(
        false,
        "One sum, or numerator then denominator; empty when the bucket is `suppressed`.",
    )
}

pub(crate) fn amounts_schema() -> Schema {
    let mut array = Array::new(decimal(true, ""));
    array.description = Some(
        "Operand contributions, in `totals` order; a null operand means a predicate excluded it. \
         The whole list is null when the reader may not see the values of every member of the \
         bucket (DATA-VALUES-1): amounts are row values."
            .to_owned(),
    );
    array.schema_type = utoipa::openapi::schema::SchemaType::from_iter([
        utoipa::openapi::schema::Type::Array,
        utoipa::openapi::schema::Type::Null,
    ]);
    Schema::Array(array)
}
