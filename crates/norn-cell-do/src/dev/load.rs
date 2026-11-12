//! `norn dev --load <snapshot>`: recorded source rows through the cell's own
//! mapping path (#921, the recorded half).
//!
//! Each row of a recorded snapshot becomes one raw observation. The cell
//! captures it as mapping evidence (`inject_mapping_bundle`), recomputes the
//! admitted mapping over it and commits the canonical objects with their
//! retained source bundle (`process_mapping_bundle`, OBS-EVIDENCE-1). Nothing
//! here writes an object or a lineage record itself, so `/lineage` of a
//! loaded object resolves to the exact source row and mapping pin.
//!
//! Only the host process loads (DEV-ONE-AUTHORITY-1); it runs when the cell
//! is opened or first provisioned, before anything is served. Loading is
//! idempotent: a row already captured is not captured again, and evidence
//! already consumed is not mapped again.
//!
//! A snapshot is the recorded-export shape of `registry/packs/*/synthetic.json`:
//! `{ "profile": "...", "snapshot": [{ "dataset", "record" }] }`, plus optional
//! `"source"`, `"mapping"` and `"connectorInstance"` (default `demo`, the
//! instance the three-systems source schemas pin). Without the first two, a `synthetic-<system>-three/1`
//! profile names source `synthetic-<system>` and mapping
//! `<system>.three.mapping`. Each record's identity is its `id` or `name`.
//! `incremental` rows are not loaded yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::{CellStore, SqliteCell};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{LogicalTime, ObservationEvent, ObservationId, Observed, Provenance};
use serde_json::Value;

/// When the recorded exports were extracted: their fixed observation clock.
const EXTRACTED_AT_MS: u64 = 1_793_620_800_000;
const RECORDED_AT: &str = "2026-11-02T12:00:00Z";

/// What one load did, per snapshot file.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub(crate) files: Vec<Value>,
}

/// One recorded snapshot file, read and named.
struct Snapshot {
    path: PathBuf,
    source: String,
    mapping: String,
    instance: String,
    rows: Vec<(String, Value)>,
}

fn snapshot(path: &Path) -> Result<Snapshot, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let document: Value =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let named = |key: &str| document.get(key).and_then(Value::as_str).map(str::to_owned);
    let system = named("profile").and_then(|profile| {
        profile
            .strip_prefix("synthetic-")
            .and_then(|rest| rest.split_once("-three/"))
            .map(|(system, _)| system.to_owned())
    });
    let source = named("source")
        .or_else(|| system.as_ref().map(|system| format!("synthetic-{system}")))
        .ok_or_else(|| {
            format!(
                "{}: name its \"source\" (or use a synthetic-<system>-three profile)",
                path.display()
            )
        })?;
    let mapping = named("mapping")
        .or_else(|| {
            system
                .as_ref()
                .map(|system| format!("{system}.three.mapping"))
        })
        .ok_or_else(|| {
            format!(
                "{}: name its \"mapping\" (or use a synthetic-<system>-three profile)",
                path.display()
            )
        })?;
    // The mapping's source schema pins the connector instance it accepts.
    let instance = named("connectorInstance").unwrap_or_else(|| "demo".to_owned());
    let rows = document
        .get("snapshot")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{}: no \"snapshot\" rows", path.display()))?
        .iter()
        .map(|row| {
            let dataset = row.get("dataset").and_then(Value::as_str);
            match (dataset, row.get("record")) {
                (Some(dataset), Some(record)) => Ok((dataset.to_owned(), record.clone())),
                _ => Err(format!(
                    "{}: every snapshot row has a dataset and a record",
                    path.display()
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Snapshot {
        path: path.to_owned(),
        source,
        mapping,
        instance,
        rows,
    })
}

/// The raw observation of one recorded row, as a connector extract writes it.
fn raw(source: &str, mapping: &str, record: &Value) -> Result<Observed<RuntimeValue>, String> {
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
            "event_id" => "snapshot",
            "source_version" => "1",
            "pipeline_version" => mapping,
            "kind" => "upsert",
            "extracted_at" => EXTRACTED_AT_MS,
            "payload" => payload,
            "payload_digest" => digest,
        },
    })
}

/// Load every snapshot into `store` through the mapping path and give the
/// store back, ready to be served.
pub(crate) fn load(
    store: CellStore<NativeEngine>,
    scope: &str,
    paths: &[PathBuf],
) -> Result<(CellStore<NativeEngine>, Loaded), String> {
    let snapshots = paths
        .iter()
        .map(|path| snapshot(path))
        .collect::<Result<Vec<_>, _>>()?;
    let mut cell: SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, NativeEngine> =
        SqliteCell::new(store, scope, |_| BTreeMap::new()).map_err(|error| error.to_string())?;
    let mut mapped: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Per file and dataset: rows, capture time and mapping time (µs), slowest mapping.
    let mut timing: BTreeMap<(String, String), (u64, u128, u128, u128)> = BTreeMap::new();
    // Each row is captured and mapped before the next is captured: mapping a
    // bundle looks it up among the pending observations, so keeping that set
    // at one row keeps a load linear in its rows.
    for snapshot in &snapshots {
        let file = snapshot.path.display().to_string();
        for (dataset, record) in &snapshot.rows {
            let input = MappingInput {
                mapping: snapshot.mapping.clone(),
                dataset: dataset.clone(),
                connector_instance: snapshot.instance.clone(),
                partition: "snapshot".into(),
                recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
            };
            let label = record
                .get("id")
                .or_else(|| record.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let started = std::time::Instant::now();
            let (id, _new) = cell
                .inject_mapping_bundle(&raw(&snapshot.source, &snapshot.mapping, record)?, &input)
                .map_err(|error| format!("{file}: {dataset} {label} not captured: {error}"))?;
            let captured = started.elapsed().as_micros();
            let started = std::time::Instant::now();
            let processed = cell.process_mapping_bundle(&id).map_err(|error| {
                format!("{file}: {dataset} {label} was refused by the mapping: {error}")
            })?;
            let mapping = started.elapsed().as_micros();
            let spent = timing.entry((file.clone(), dataset.clone())).or_default();
            *spent = (
                spent.0.saturating_add(1),
                spent.1.saturating_add(captured),
                spent.2.saturating_add(mapping),
                spent.3.max(mapping),
            );
            let entry = mapped.entry(file.clone()).or_default();
            entry.0 = entry.0.saturating_add(1);
            if processed {
                entry.1 = entry.1.saturating_add(1);
            }
        }
    }
    let files = snapshots
        .iter()
        .map(|snapshot| {
            let file = snapshot.path.display().to_string();
            let (rows, newly) = mapped.get(&file).copied().unwrap_or_default();
            let datasets: serde_json::Map<String, Value> = timing
                .iter()
                .filter(|((timed, _), _)| *timed == file)
                .map(|((_, dataset), (rows, capture, map, slowest))| {
                    let ms = |micros: &u128| u64::try_from(micros / 1000).unwrap_or(u64::MAX);
                    (dataset.clone(), serde_json::json!({ "rows": rows, "captureMs": ms(capture), "mapMs": ms(map), "slowestMapMs": ms(slowest) }))
                })
                .collect();
            serde_json::json!({ "file": file, "source": snapshot.source, "mapping": snapshot.mapping, "rows": rows, "newlyMapped": newly, "datasets": datasets })
        })
        .collect();
    Ok((cell.into_store(), Loaded { files }))
}
