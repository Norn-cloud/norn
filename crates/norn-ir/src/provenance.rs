//! The provenance envelope on instances (T8-ID-1, #2603, provenance part;
//! ruling 2026-09-25 §2: "Provenance is not a feature; it is refusing to
//! discard the observation").
//!
//! Every fact that entered from outside carries one [`InstanceProvenance`]
//! per contributing source record: the record's identity in its source system
//! ([`SourceRecordRef`]), the [`MappingVersion`] and optional transform that
//! produced the fact, when Norn recorded it, and the upstream facts it was
//! derived from ([`FactRef`]). Source identities are kept per external
//! system; several source records may resolve to one object, so an instance
//! carries a list ([`ProvenancedInstance`]), never one flattened record.
//!
//! The envelope is data only: mappings (MappingIR) emit it, the cell stores
//! it with the fact, and lineage projections (`norn-projections`) read it.
//! Every consistency rule (timestamp ranges, non-empty identities, versions
//! starting at `1`) is a `norn-checker` diagnostic, not a decode error.
//!
//! Stable ids ([`FactRef::stable_id`], [`SourceRecordRef::stable_id`]) are
//! the node ids lineage traversals and interop exports use, so the same fact
//! or source record has the same id in every projection.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::MarkingId;
use crate::ObjectTypeName;
use crate::members::present_non_null;

/// Grammar of a [`UtcTimestamp`] (published as the schema `pattern`;
/// `norn-checker` also checks the date and time exist): RFC 3339 in UTC
/// (`Z`), with optional fractional seconds.
pub const UTC_TIMESTAMP_PATTERN: &str =
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,9})?Z$";

/// An RFC 3339 instant in UTC (`2026-10-01T09:30:00Z`).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct UtcTimestamp(#[schemars(regex(pattern = UTC_TIMESTAMP_PATTERN))] pub String);

impl UtcTimestamp {
    /// The timestamp as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The version of the mapping that produced a fact
/// (`erpnext.sales_order@3`): the `Mapping` technical-view object it names.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct MappingVersion(pub String);

impl MappingVersion {
    /// The version as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The identity of one record in an external source system.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceRecordRef {
    /// The source system (`ERPNext`); the key of its `Source` technical-view
    /// object. Contains no `/`.
    pub system: String,
    /// The record's identity in that system (`Sales Order/SO-1001`).
    pub record: String,
}

impl SourceRecordRef {
    /// The record of `system` identified by `record`.
    #[must_use]
    pub fn new(system: impl Into<String>, record: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            record: record.into(),
        }
    }

    /// The stable lineage id: `source:{system}/{record}`.
    #[must_use]
    pub fn stable_id(&self) -> String {
        format!("source:{}/{}", self.system, self.record)
    }
}

/// One committed version of one object: the unit lineage cites.
///
/// Versions start at `1` and bump with every committed write to the object
/// (the cell's owner version), so `(objectType, key, version)` names one
/// immutable fact.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactRef {
    /// The object type.
    pub object_type: ObjectTypeName,
    /// The object's identity key value.
    pub key: String,
    /// The object version this fact is.
    pub version: u64,
}

impl FactRef {
    /// Version `version` of object `key` of type `object_type`.
    #[must_use]
    pub fn new(object_type: impl Into<String>, key: impl Into<String>, version: u64) -> Self {
        Self {
            object_type: ObjectTypeName(object_type.into()),
            key: key.into(),
            version,
        }
    }

    /// The stable lineage id: `fact:{objectType}/{key}@v{version}`. The type
    /// name contains no `/`, and the version follows the last `@v`, so the id
    /// is unambiguous for any key.
    #[must_use]
    pub fn stable_id(&self) -> String {
        format!(
            "fact:{}/{}@v{}",
            self.object_type.as_str(),
            self.key,
            self.version
        )
    }
}

/// Where one instance (or one version of it) came from: one record per
/// contributing source record. See the module docs.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceProvenance {
    /// The source record the fact was produced from.
    pub source: SourceRecordRef,
    /// The mapping version that produced it.
    pub mapping_version: MappingVersion,
    /// The transform within that mapping (a mapping rule id such as
    /// `sales_order.header`); omitted when the mapping has one.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub transform: Option<String>,
    /// When the source system reported the record, when it says so.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "UtcTimestamp")]
    pub observed_at: Option<UtcTimestamp>,
    /// When Norn recorded the fact.
    pub recorded_at: UtcTimestamp,
    /// Source fields read by the value-producing expression, plus the map's
    /// subject, source-record, and guard expressions, in mapping source order.
    /// Nested property reads include the parent collection and qualified child
    /// predicate/value fields (`collection.field`).
    /// Omitted when unavailable or empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_fields: Vec<String>,
    /// Effective classifications propagated from the value-producing and
    /// identity-source fields, guards, and parent collections. Omitted when
    /// empty for older or unmarked facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// The upstream facts the transform read, in mapping order. Omitted when
    /// empty (the fact came from the source record alone).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream: Vec<FactRef>,
}

/// One fact with every provenance record of the source records that resolved
/// to it (two ERPNext rows merged into one customer carry two records).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvenancedInstance {
    /// The fact.
    pub fact: FactRef,
    /// Its provenance records, one per contributing source record.
    pub provenance: Vec<InstanceProvenance>,
}

/// Emit the JSON Schema of one [`InstanceProvenance`] record.
#[must_use]
pub fn instance_provenance_schema() -> Schema {
    schema_for!(InstanceProvenance)
}
