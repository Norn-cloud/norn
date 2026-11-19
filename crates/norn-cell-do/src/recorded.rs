//! Recorded source exports: the snapshot shape of
//! `registry/packs/*/synthetic.json`, loaded through the cell's own mapping
//! path by `norn dev --load` and by `POST /v1/observations:load` (#921).
//!
//! `{ "profile": "...", "snapshot": [{ "dataset", "record", "sourceVersion"? }] }`,
//! plus optional `"incremental"` changes, `"source"`, `"mapping"` and
//! `"connectorInstance"` (default
//! `demo`, the instance the three-systems source schemas pin). Without the
//! first two, a `synthetic-<system>-three/1` profile names source
//! `synthetic-<system>` and mapping `<system>.three.mapping`. Each record's
//! identity is its `id` or `name`. A row's `sourceVersion` orders
//! lexicographically: a later row with the same identity and a higher version
//! is that record's next change. Without one, a row is the snapshot event at
//! version `1`.
//!
//! RECORDED-INCREMENTAL-1: an `"incremental"` row is
//! `{ "sequence", "operation": "upsert", "dataset", "record", "sourceVersion"? }`
//! and loads after every snapshot row, in file order. Without a
//! `sourceVersion` its version is `1+<sequence, 20 digits>`, which orders
//! after the snapshot's `1` and by sequence; when any other row (snapshot or
//! incremental) carries a `sourceVersion`, a version-less change would not
//! order against it, so the whole export is refused. A row this loader cannot read (another
//! operation, no sequence) refuses the whole export: no row of a recorded
//! export is ever skipped.

use norn_cell::mapping_bundle::MappingInput;
use norn_ir::RuntimeValue;
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use serde_json::Value;

/// When the recorded exports were extracted: their fixed observation clock,
/// so loading the same export twice captures each row once.
const EXTRACTED_AT_MS: u64 = 1_793_620_800_000;
const RECORDED_AT: &str = "2026-11-02T12:00:00Z";

/// One recorded row.
#[derive(Debug, Clone)]
pub struct RecordedRow {
    /// The source dataset it was read from.
    pub dataset: String,
    /// The record, as the source exported it.
    pub record: Value,
    /// Its source version, when the export records changes over time.
    pub version: Option<String>,
}

/// A recorded export, named.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The source system.
    pub source: String,
    /// The admitted mapping that reads it.
    pub mapping: String,
    /// The connector instance the mapping's source schema pins.
    pub instance: String,
    /// Its rows: the snapshot in export order, then the incremental changes.
    pub rows: Vec<RecordedRow>,
}

impl Recorded {
    /// Read a recorded export document.
    ///
    /// # Errors
    /// A missing name, or a row without a dataset and a record.
    pub fn parse(document: &Value) -> Result<Self, String> {
        let named = |key: &str| document.get(key).and_then(Value::as_str).map(str::to_owned);
        let system = named("profile").and_then(|profile| {
            profile
                .strip_prefix("synthetic-")
                .and_then(|rest| rest.split_once("-three/"))
                .map(|(system, _)| system.to_owned())
        });
        let source = named("source")
            .or_else(|| system.as_ref().map(|system| format!("synthetic-{system}")))
            .ok_or("name its \"source\" (or use a synthetic-<system>-three profile)")?;
        let mapping = named("mapping")
            .or_else(|| {
                system
                    .as_ref()
                    .map(|system| format!("{system}.three.mapping"))
            })
            .ok_or("name its \"mapping\" (or use a synthetic-<system>-three profile)")?;
        let instance = named("connectorInstance").unwrap_or_else(|| "demo".to_owned());
        let row = |row: &Value, version: Option<String>| match (
            row.get("dataset").and_then(Value::as_str),
            row.get("record"),
        ) {
            (Some(dataset), Some(record)) => Ok(RecordedRow {
                dataset: dataset.to_owned(),
                record: record.clone(),
                version,
            }),
            _ => Err("every row has a dataset and a record".to_owned()),
        };
        let source_version = |value: &Value| {
            value
                .get("sourceVersion")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let mut rows = document
            .get("snapshot")
            .and_then(Value::as_array)
            .ok_or("no \"snapshot\" rows")?
            .iter()
            .map(|value| row(value, source_version(value)))
            .collect::<Result<Vec<_>, String>>()?;
        if let Some(changes) = document.get("incremental") {
            let changes = changes
                .as_array()
                .ok_or("\"incremental\" is a list of changes")?;
            // Any recorded version, on a snapshot row or on any change in
            // the file, wherever it stands relative to a version-less one.
            let versioned = rows.iter().any(|row| row.version.is_some())
                || changes
                    .iter()
                    .any(|change| source_version(change).is_some());
            for (index, change) in changes.iter().enumerate() {
                let operation = change.get("operation").and_then(Value::as_str);
                if operation != Some("upsert") {
                    return Err(format!(
                        "incremental row {index}: operation {} does not load; a recorded export carries upserts only",
                        operation.unwrap_or("(none)")
                    ));
                }
                let sequence = change
                    .get("sequence")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| format!("incremental row {index} has no \"sequence\""))?;
                // A synthesised version orders only against other synthesised
                // versions, so it is refused beside a recorded one.
                let version = match source_version(change) {
                    Some(version) => version,
                    None if versioned => {
                        return Err(format!(
                            "incremental row {index} has no \"sourceVersion\" while other rows carry one; a recorded export versions every change or none"
                        ));
                    }
                    None => format!("1+{sequence:020}"),
                };
                rows.push(
                    row(change, Some(version))
                        .map_err(|error| format!("incremental row {index}: {error}"))?,
                );
            }
        }
        Ok(Self {
            source,
            mapping,
            instance,
            rows,
        })
    }

    /// The mapping input of one of its datasets.
    #[must_use]
    pub fn input(&self, dataset: &str) -> MappingInput {
        MappingInput {
            mapping: self.mapping.clone(),
            dataset: dataset.to_owned(),
            connector_instance: self.instance.clone(),
            partition: "snapshot".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
        }
    }

    /// The raw observation of one of its rows.
    ///
    /// # Errors
    /// A record without an `id` or `name`, or one that is not exact JSON.
    pub fn raw(&self, row: &RecordedRow) -> Result<Observed<RuntimeValue>, String> {
        raw(
            &self.source,
            &self.mapping,
            &row.record,
            row.version.as_deref(),
        )
    }

    /// Every row as the raw observation and mapping input a host ingests.
    ///
    /// # Errors
    /// As [`Self::raw`].
    pub fn observations(&self) -> Result<Vec<(Observed<RuntimeValue>, MappingInput)>, String> {
        self.rows
            .iter()
            .map(|row| Ok((self.raw(row)?, self.input(&row.dataset))))
            .collect()
    }
}

/// The raw observation of one recorded row, as a connector extract writes it.
fn raw(
    source: &str,
    mapping: &str,
    record: &Value,
    version: Option<&str>,
) -> Result<Observed<RuntimeValue>, String> {
    let id = record
        .get("id")
        .or_else(|| record.get("name"))
        .and_then(Value::as_str)
        .ok_or("a recorded row needs an \"id\" or \"name\"")?
        .to_owned();
    let payload =
        RuntimeValue::from_json_text(record.to_string()).map_err(|error| error.to_string())?;
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(Observed {
        id: ObservationId(format!("{source}/{id}")),
        provenance: Provenance {
            source: source.to_owned(),
            source_id: id.clone(),
            mapping_version: mapping.to_owned(),
            observed_at: LogicalTime::from_millis(EXTRACTED_AT_MS),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => source,
            "source_id" => id,
            "event_id" => version.unwrap_or("snapshot"),
            "source_version" => version.unwrap_or("1"),
            "pipeline_version" => mapping,
            "kind" => "upsert",
            "extracted_at" => EXTRACTED_AT_MS,
            "payload" => payload,
            "payload_digest" => digest,
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "parse scenarios")]
mod tests {
    use super::*;
    use serde_json::json;

    fn export(snapshot_version: Option<&str>, change_version: Option<&str>) -> Value {
        let mut row = json!({ "dataset": "Item", "record": { "id": "i-1" } });
        if let Some(version) = snapshot_version {
            row["sourceVersion"] = json!(version);
        }
        let mut change = json!({ "sequence": 1, "operation": "upsert",
            "dataset": "Item", "record": { "id": "i-1" } });
        if let Some(version) = change_version {
            change["sourceVersion"] = json!(version);
        }
        json!({ "profile": "synthetic-erpnext-three/1", "snapshot": [row], "incremental": [change] })
    }

    #[test]
    fn a_version_less_change_beside_a_versioned_snapshot_refuses_the_export() {
        let refused = Recorded::parse(&export(Some("2026-01-01"), None)).unwrap_err();
        assert!(
            refused.contains("incremental row 0") && refused.contains("sourceVersion"),
            "{refused}"
        );
    }

    #[test]
    fn a_version_less_change_before_a_versioned_change_refuses_the_export() {
        let mut document = export(None, None);
        document["incremental"].as_array_mut().unwrap().push(
            json!({ "sequence": 2, "operation": "upsert", "dataset": "Item",
                "record": { "id": "i-1" }, "sourceVersion": "2026-01-01" }),
        );
        let refused = Recorded::parse(&document).unwrap_err();
        assert!(
            refused.contains("incremental row 0") && refused.contains("sourceVersion"),
            "{refused}"
        );
    }

    #[test]
    fn a_change_versions_by_its_sequence_only_beside_a_version_less_snapshot() {
        let recorded = Recorded::parse(&export(None, None)).unwrap();
        assert_eq!(
            recorded.rows[1].version.as_deref(),
            Some("1+00000000000000000001")
        );
        let recorded = Recorded::parse(&export(Some("2026-01-01"), Some("2026-01-02"))).unwrap();
        assert_eq!(recorded.rows[1].version.as_deref(), Some("2026-01-02"));
    }
}
