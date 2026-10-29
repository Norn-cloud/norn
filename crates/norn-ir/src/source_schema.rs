//! `SourceSchemaIR`: what Norn discovered about a source system's schema
//! (T8-SRC-1, #2596; decision record
//! `knowledge/architecture/build-plan-2026-09-09/source-discovery-decision.md`).
//!
//! A [`SourceSchemaIr`] document is the output contract of every source
//! discovery adapter: the SQL and file profilers (pool Functions, T8-SRC-2/3)
//! and the Frappe `DocType` metadata adapter (T8-SRC-4). Its JSON Schema
//! ([`source_schema_ir_schema`], checked in as
//! `schema/source_schema_ir.schema.json`) is what an adapter's output is
//! validated against before it becomes evidence.
//!
//! The document is an envelope (source identity and version, observed-at,
//! adapter and version, a reference to the raw metadata) plus six lists:
//!
//! | list | what |
//! | --- | --- |
//! | `datasets` | tables, views, sheets, files, streams, documents (`DocType`s) |
//! | `fields` | columns / fields of a dataset, with native and candidate types |
//! | `constraints` | keys, references, parent-child relations, checks |
//! | `profiles` | measurements (row/null/distinct counts, bounded samples) |
//! | `codeSets` | closed value sets (`Select` options, SQL enums, frequent values) |
//! | `lineage` | how one dataset derives from others (views, queries, config) |
//!
//! Rules the shape encodes (and `norn-checker`'s `source_schema` module
//! enforces):
//!
//! - **Declared vs inferred.** Every constraint, code set, and type candidate
//!   carries its [`Origin`]: `declared` by the source, or `inferred` /
//!   `observed_candidate` by Norn. An inference is never promoted to a
//!   declaration; a constraint is `declared` or `inferred`, a code set
//!   `declared` or `observed_candidate`.
//! - **Measurements are dated.** A [`SourceProfile`] records when
//!   (`asOf`), how (`full_scan` or `sample`), and over how many rows it was
//!   measured. Absent measurements are omitted, never invented.
//! - **Samples are bounded and redacted.** A [`SampleSet`] carries its
//!   [`SamplePolicy`]; at most [`MAX_SAMPLE_VALUES`] values, and none when
//!   the policy withholds them.
//! - **Never mapped directly to the ontology.** Lowering is
//!   `SourceSchemaIR → MappingIR → EntityIR`; nothing here names a Norn
//!   object type.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::MarkingId;
use crate::members::{WholeNumber, present_non_null};
use crate::rule::JsonObject;

/// The upper bound on sample values one [`SampleSet`] may carry, whatever
/// its policy says.
pub const MAX_SAMPLE_VALUES: u64 = 20;

/// The format tag of a [`SourceSchemaIr`] document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum SourceSchemaFormat {
    /// `norn.source-schema/1`.
    #[serde(rename = "norn.source-schema/1")]
    V1,
}

/// A discovered source schema. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSchemaIr {
    /// The format tag.
    pub format: SourceSchemaFormat,
    /// The source system and instance the schema was discovered on.
    pub source: SourceIdentity,
    /// When the metadata was read: RFC 3339 with an explicit offset.
    pub observed_at: String,
    /// The adapter that produced the document.
    pub adapter: SourceAdapter,
    /// The raw metadata the document was derived from, retained as evidence.
    pub raw_metadata: EvidenceRef,
    /// Datasets, in discovery order. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasets: Vec<SourceDataset>,
    /// Fields of the datasets. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<SourceField>,
    /// Declared and inferred constraints. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<SourceConstraint>,
    /// Measurements. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<SourceProfile>,
    /// Closed value sets. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_sets: Vec<CodeSet>,
    /// Dataset lineage. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<SourceLineage>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// The source system and instance a schema was discovered on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceIdentity {
    /// The source system (`ERPNext`, `PostgreSQL`, `CSV`).
    pub system: String,
    /// The instance within the system: a site, database, or file set.
    pub instance: String,
    /// The system's version, when the source reports it.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub version: Option<String>,
}

/// The adapter that produced a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAdapter {
    /// Adapter name (`norn.importers.frappe`).
    pub name: String,
    /// Adapter version.
    pub version: String,
}

/// A content-addressed reference to evidence (raw metadata, a profile
/// query result, a sample file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRef {
    /// Where the evidence is stored (an object-store URI or a fixture path).
    pub uri: String,
    /// Lower-case hex SHA-256 of the evidence bytes.
    pub sha256: String,
}

/// What kind of collection a dataset is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    /// A database table.
    Table,
    /// A database view.
    View,
    /// A workbook sheet.
    Sheet,
    /// A delimited or columnar file.
    File,
    /// An API stream (not necessarily one table).
    Stream,
    /// A document type (a Frappe `DocType`).
    Document,
    /// A child table owned by a parent document type (a Frappe `istable`
    /// `DocType`).
    ChildDocument,
}

/// One dataset of the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDataset {
    /// Stable id of the dataset within the document; every other list
    /// references datasets by it.
    pub id: String,
    /// The source-native name (`Sales Order`, `public.orders`).
    pub native_name: String,
    /// The native namespace (schema, module, workbook), when the source has
    /// one.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub namespace: Option<String>,
    /// What kind of collection it is.
    pub kind: DatasetKind,
    /// The source's description of the dataset.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
}

/// A normalized type a native type is a candidate for.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NormalizedType {
    /// A short string.
    String,
    /// Long or formatted text.
    Text,
    /// An integer.
    Integer,
    /// An exact decimal.
    Decimal,
    /// A boolean.
    Boolean,
    /// A calendar date.
    Date,
    /// A date and time (with or without an offset; see the native type).
    Datetime,
    /// A time of day.
    Time,
    /// An elapsed duration.
    Duration,
    /// Structured JSON.
    Json,
    /// Binary content or an attachment reference.
    Binary,
    /// The name or id of a record of another dataset.
    Reference,
    /// Not determined.
    Unknown,
}

/// Who asserted a fact about the source.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Declared by the source's own metadata.
    Declared,
    /// Inferred by Norn from metadata or measurements.
    Inferred,
    /// A candidate Norn observed in the data (for example frequent values);
    /// never a declaration.
    ObservedCandidate,
}

/// A candidate normalized type and who asserted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeCandidate {
    /// The candidate type.
    #[serde(rename = "type")]
    pub normalized: NormalizedType,
    /// `declared` (the native type maps to it exactly) or `inferred`.
    pub origin: Origin,
}

/// One field of a dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceField {
    /// The dataset id.
    pub dataset: String,
    /// The source-native field name, unique within the dataset.
    pub name: String,
    /// Position in the dataset's declared order, unique within the dataset.
    pub ordinal: WholeNumber,
    /// The source-native type (`varchar(140)`, `Link`, `Currency`).
    pub native_type: String,
    /// The candidate normalized type.
    pub type_candidate: TypeCandidate,
    /// The source's own required (non-nullable) bit, when it declares one.
    /// Omitted: the source does not say. Measured nullability is a profile.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    pub required: Option<bool>,
    /// Classification markings declared for this source field.
    /// Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// The source's label or description of the field.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
}

/// What a constraint asserts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// The fields identify a record.
    PrimaryKey,
    /// No two records share the fields' values.
    Unique,
    /// A physical foreign key to `target`.
    ForeignKey,
    /// A logical reference to `target` the source does not enforce
    /// physically (a Frappe `Link`).
    LogicalReference,
    /// The dataset's records are owned by `target`'s records (a Frappe child
    /// table row's `parent`).
    ParentChild,
    /// A check expression in the source's native dialect.
    Check,
}

/// The referenced side of a reference constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConstraintTarget {
    /// The referenced dataset id.
    pub dataset: String,
    /// The referenced fields, in the same order as the constraint's fields.
    pub fields: Vec<String>,
}

/// One declared or inferred constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceConstraint {
    /// Stable id of the constraint within the document.
    pub id: String,
    /// What it asserts.
    pub kind: ConstraintKind,
    /// The constrained dataset id.
    pub dataset: String,
    /// The constrained fields, in order. Non-empty.
    pub fields: Vec<String>,
    /// The referenced side: required for `foreign_key`, `logical_reference`,
    /// and `parent_child`; absent otherwise.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ConstraintTarget")]
    pub target: Option<ConstraintTarget>,
    /// The native check expression: required for `check`, absent otherwise.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub expression: Option<String>,
    /// For `parent_child`: the target (parent) dataset's field holding the
    /// child rows (a Frappe `Table` field). Absent otherwise.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub collection: Option<String>,
    /// `declared` or `inferred`.
    pub origin: Origin,
    /// Evidence for the constraint. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

/// How a profile was measured.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMethod {
    /// Every row was read.
    FullScan,
    /// A sample of rows was read.
    Sample,
}

/// Whether a distinct count is exact.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CountPrecision {
    /// Counted exactly over the rows seen.
    Exact,
    /// Estimated (for example `HyperLogLog`).
    Approximate,
}

/// One measurement of a dataset, or of one of its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceProfile {
    /// The measured dataset id.
    pub dataset: String,
    /// The measured field. Omitted: a dataset-level profile.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub field: Option<String>,
    /// When it was measured: RFC 3339 with an explicit offset.
    pub as_of: String,
    /// How it was measured.
    pub method: ProfileMethod,
    /// How many rows the measurement read.
    pub rows_seen: WholeNumber,
    /// Null values among the rows seen (field profiles only).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WholeNumber")]
    pub null_count: Option<WholeNumber>,
    /// Distinct non-null values among the rows seen (field profiles only).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WholeNumber")]
    pub distinct_count: Option<WholeNumber>,
    /// Whether `distinctCount` is exact; present exactly when it is.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "CountPrecision")]
    pub distinct_precision: Option<CountPrecision>,
    /// Bounded sample values (field profiles only).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "SampleSet")]
    pub samples: Option<SampleSet>,
    /// Evidence for the measurement (the query or file it read). Omitted
    /// when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

/// How sample values are redacted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Redaction {
    /// Values are kept as read (non-sensitive fields only).
    None,
    /// Values are masked (characters replaced, shape kept).
    Masked,
    /// Values are replaced by a keyed hash.
    Hashed,
    /// No values are kept.
    Withheld,
}

/// The sample policy a [`SampleSet`] was taken under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SamplePolicy {
    /// The most values kept: at most [`MAX_SAMPLE_VALUES`].
    pub max_values: WholeNumber,
    /// How kept values are redacted.
    pub redaction: Redaction,
}

/// Bounded, redacted sample values of one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SampleSet {
    /// The policy the values were taken under.
    pub policy: SamplePolicy,
    /// The values, as text after redaction; at most `policy.maxValues`, and
    /// none when the policy is `withheld`. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

/// One value of a [`CodeSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeValue {
    /// The stored value.
    pub value: String,
    /// The source's display label, when it differs.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub label: Option<String>,
}

/// A closed set of values of one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeSet {
    /// Stable id of the code set within the document.
    pub id: String,
    /// The dataset id.
    pub dataset: String,
    /// The field whose values it lists.
    pub field: String,
    /// `declared` (a `Select`, an enum, a check) or `observed_candidate`
    /// (frequent values). A candidate never becomes an ontology enum without
    /// review.
    pub origin: Origin,
    /// The values, unique. Non-empty.
    pub values: Vec<CodeValue>,
}

/// How a dataset derives from others.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LineageKind {
    /// A database view definition.
    View,
    /// A saved query or report.
    Query,
    /// Source configuration (a Frappe virtual `DocType`, a sync setting).
    Config,
}

/// One lineage edge: `output` derives from `inputs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceLineage {
    /// Stable id of the edge within the document.
    pub id: String,
    /// How the output derives.
    pub kind: LineageKind,
    /// Input dataset ids. Non-empty.
    pub inputs: Vec<String>,
    /// The derived dataset id.
    pub output: String,
    /// Evidence (the view or query text). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

impl SourceSchemaIr {
    /// The dataset with id `id`.
    #[must_use]
    pub fn dataset(&self, id: &str) -> Option<&SourceDataset> {
        self.datasets.iter().find(|dataset| dataset.id == id)
    }

    /// The fields of dataset `dataset`, in document order.
    pub fn fields_of<'a>(&'a self, dataset: &'a str) -> impl Iterator<Item = &'a SourceField> {
        self.fields
            .iter()
            .filter(move |field| field.dataset == dataset)
    }

    /// The field `name` of dataset `dataset`.
    #[must_use]
    pub fn field(&self, dataset: &str, name: &str) -> Option<&SourceField> {
        self.fields
            .iter()
            .find(|field| field.dataset == dataset && field.name == name)
    }
}

/// Emit the JSON Schema of a [`SourceSchemaIr`] document: the contract every
/// discovery adapter's output is validated against.
#[must_use]
pub fn source_schema_ir_schema() -> Schema {
    schema_for!(SourceSchemaIr)
}
