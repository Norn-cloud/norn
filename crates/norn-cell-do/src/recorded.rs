//! Recorded source exports: the snapshot shape of
//! `registry/packs/*/synthetic.json`, loaded through the cell's own mapping
//! path by `norn dev --load` and by `POST /v1/observations:load` (#921).
//!
//! `{ "profile": "...", "snapshot": [{ "dataset", "record", "sourceVersion"? }] }`,
//! plus optional `"source"`, `"mapping"` and `"connectorInstance"` (default
//! `demo`, the instance the three-systems source schemas pin). Without the
//! first two, a `synthetic-<system>-three/1` profile names source
//! `synthetic-<system>` and mapping `<system>.three.mapping`. Each record's
//! identity is its `id` or `name`. A row's `sourceVersion` orders
//! lexicographically: a later row with the same identity and a higher version
//! is that record's next change. Without one, a row is the snapshot event at
//! version `1`.

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
    /// Its rows, in export order.
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
        let rows = document
            .get("snapshot")
            .and_then(Value::as_array)
            .ok_or("no \"snapshot\" rows")?
            .iter()
            .map(|row| {
                let dataset = row.get("dataset").and_then(Value::as_str);
                let version = row
                    .get("sourceVersion")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                match (dataset, row.get("record")) {
                    (Some(dataset), Some(record)) => Ok(RecordedRow {
                        dataset: dataset.to_owned(),
                        record: record.clone(),
                        version,
                    }),
                    _ => Err("every snapshot row has a dataset and a record".to_owned()),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
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
