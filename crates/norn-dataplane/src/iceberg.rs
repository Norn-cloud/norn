//! The Iceberg table specification for raw observations (T12-DP-1).
//!
//! Ruling 2026-09-25 §8: the lake format is Apache Iceberg in R2 Data Catalog
//! (Lakekeeper on the own pool), read by `DuckDB` with raw payloads as
//! `VARIANT`. This module only *specifies* the table, in the Iceberg table
//! metadata JSON encoding. Creating it in a catalog and writing data files is
//! later work.
//!
//! # Table
//!
//! - Identifier: namespace `norn_<tenant>` (one namespace per tenant), table
//!   [`RAW_OBSERVATION_TABLE`].
//! - Format version 3 (needed for the `variant` type). Columns map one to one
//!   onto the [`RawObservation`](crate::RawObservation) JSON fields, in the
//!   same snake-case names. `extracted_at` is milliseconds in the contract and
//!   `timestamptz` (microseconds, UTC) in the table; the writer multiplies by
//!   1000.
//! - **Append-only.** There are no identifier fields, no equality deletes, no
//!   position deletes, and no row-level updates: a source delete is a
//!   `kind = 'delete'` row, and a redaction is a new observation recorded
//!   under a new policy version. Snapshot expiry and compaction (managed by
//!   the catalog) never rewrite row content.
//! - Partitioned by `identity(source)` and `day(extracted_at)`; sorted by
//!   `source, source_id, source_version, event_id` so the "latest observation
//!   wins" transform ([`crate::converge`]) is a sorted scan.

//!
//! DP-TABLE-1: the catalog request declares append-only v3 observations.
//! This is a storage contract, not evidence of a running Iceberg writer.

use serde_json::{Value, json};

use crate::raw_observation::RAW_OBSERVATION_CONTRACT;

/// The table name inside a tenant namespace.
pub const RAW_OBSERVATION_TABLE: &str = "raw_observation";

/// The Iceberg format version the table requires (`variant` is v3).
pub const RAW_OBSERVATION_FORMAT_VERSION: u8 = 3;

/// One top-level column of the raw observation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    /// The Iceberg field id; stable forever.
    pub id: u32,
    /// The column name; equals the contract JSON field name.
    pub name: &'static str,
    /// Whether the column is required.
    pub required: bool,
}

/// The top-level columns, in field-id order.
pub const RAW_OBSERVATION_COLUMNS: &[Column] = &[
    Column {
        id: 1,
        name: "contract",
        required: true,
    },
    Column {
        id: 2,
        name: "source",
        required: true,
    },
    Column {
        id: 3,
        name: "source_id",
        required: true,
    },
    Column {
        id: 4,
        name: "event_id",
        required: true,
    },
    Column {
        id: 5,
        name: "kind",
        required: true,
    },
    Column {
        id: 6,
        name: "source_version",
        required: true,
    },
    Column {
        id: 7,
        name: "payload",
        required: true,
    },
    Column {
        id: 8,
        name: "payload_digest",
        required: true,
    },
    Column {
        id: 9,
        name: "extracted_at",
        required: true,
    },
    Column {
        id: 10,
        name: "cursor",
        required: false,
    },
    Column {
        id: 11,
        name: "pipeline_version",
        required: true,
    },
    Column {
        id: 12,
        name: "redaction",
        required: false,
    },
];

/// The Iceberg schema (table metadata `schemas[0]`).
#[must_use]
pub fn table_schema() -> Value {
    json!({
        "type": "struct",
        "schema-id": 0,
        "fields": [
            { "id": 1, "name": "contract", "required": true, "type": "string",
              "doc": "always norn.raw_observation/v1" },
            { "id": 2, "name": "source", "required": true, "type": "string",
              "doc": "source system, e.g. erpnext" },
            { "id": 3, "name": "source_id", "required": true, "type": "string",
              "doc": "record identity in the source" },
            { "id": 4, "name": "event_id", "required": true, "type": "string",
              "doc": "source event identity; dedup key within source" },
            { "id": 5, "name": "kind", "required": true, "type": "string",
              "doc": "upsert or delete" },
            { "id": 6, "name": "source_version", "required": true, "type": "string",
              "doc": "source-asserted version, ordered lexicographically" },
            { "id": 7, "name": "payload", "required": true, "type": "variant",
              "doc": "record as received, after redaction" },
            { "id": 8, "name": "payload_digest", "required": true, "type": "string",
              "doc": "Norn1 hash of payload" },
            { "id": 9, "name": "extracted_at", "required": true, "type": "timestamptz",
              "doc": "host receipt time" },
            { "id": 10, "name": "cursor", "required": false, "type": "variant",
              "doc": "source cursor at extraction" },
            { "id": 11, "name": "pipeline_version", "required": true, "type": "string",
              "doc": "extract pipeline and version (provenance mapping_version)" },
            { "id": 12, "name": "redaction", "required": false, "type": {
                "type": "struct",
                "fields": [
                    { "id": 13, "name": "policy_id", "required": true, "type": "string" },
                    { "id": 14, "name": "policy_version", "required": true, "type": "string" },
                    { "id": 15, "name": "redacted", "required": true, "type": {
                        "type": "list",
                        "element-id": 16,
                        "element": "string",
                        "element-required": true
                    } }
                ]
            }, "doc": "redaction policy and replaced JSON pointers" }
        ]
    })
}

/// The partition spec (table metadata `partition-specs[0]`).
#[must_use]
pub fn partition_spec() -> Value {
    json!({
        "spec-id": 0,
        "fields": [
            { "source-id": 2, "field-id": 1000, "name": "source", "transform": "identity" },
            { "source-id": 9, "field-id": 1001, "name": "extracted_at_day", "transform": "day" }
        ]
    })
}

/// The sort order (table metadata `sort-orders[0]`).
#[must_use]
pub fn sort_order() -> Value {
    let field = |source_id: u32| {
        json!({
            "transform": "identity",
            "source-id": source_id,
            "direction": "asc",
            "null-order": "nulls-first"
        })
    };
    json!({
        "order-id": 1,
        "fields": [field(2), field(3), field(6), field(4)]
    })
}

/// The table properties.
#[must_use]
pub fn table_properties() -> Value {
    json!({
        "format-version": RAW_OBSERVATION_FORMAT_VERSION.to_string(),
        "write.format.default": "parquet",
        "write.parquet.compression-codec": "zstd",
        "norn.contract": RAW_OBSERVATION_CONTRACT,
        "norn.append-only": "true"
    })
}

/// The whole specification as one create-table request body (Iceberg REST
/// catalog `CreateTableRequest` shape, without `location`).
#[must_use]
pub fn create_table_request() -> Value {
    json!({
        "name": RAW_OBSERVATION_TABLE,
        "schema": table_schema(),
        "partition-spec": partition_spec(),
        "write-order": sort_order(),
        "stage-create": false,
        "properties": table_properties()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn append_only_table_conforms() {
        let table = super::create_table_request();
        assert_eq!(table["properties"]["norn.append-only"], "true");
        assert_eq!(table["properties"]["format-version"], "3");
        assert_eq!(table["schema"]["fields"][6]["type"], "variant");
    }
}
