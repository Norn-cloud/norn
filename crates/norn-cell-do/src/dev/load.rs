//! `norn dev --load <snapshot>`: recorded source rows through the cell's own
//! mapping path (#921, the recorded half).
//!
//! Each row of a recorded snapshot becomes one raw observation, delivered in
//! pages of consecutive rows of one dataset (OBS-PAGE-1). The cell captures
//! each row as mapping evidence (`inject_mapping_page`), recomputes the
//! admitted mapping over it and commits the page's canonical objects with
//! their retained source bundles (`process_mapping_page`, OBS-EVIDENCE-1). Nothing
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
//! A row may carry a `"sourceVersion"` (it orders lexicographically): a
//! later row with the same identity and a higher version is that record's
//! next change, committed as its next version (a holding's balance over time).
//! Without one, a row is the snapshot event at version `1`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::{CellStore, MappingOutcome, SqliteCell};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{LogicalTime, ObservationEvent, ObservationId, Observed, Provenance};
use serde_json::Value;

/// When the recorded exports were extracted: their fixed observation clock.
const EXTRACTED_AT_MS: u64 = 1_793_620_800_000;
const RECORDED_AT: &str = "2026-11-02T12:00:00Z";
/// Rows per page; the cell's bound is higher (OBS-PAGE-1 hosts default to 500).
const PAGE_ROWS: usize = 500;

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
    /// Each row's dataset, record and optional source version.
    rows: Vec<(String, Value, Option<String>)>,
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
            let version = row
                .get("sourceVersion")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match (dataset, row.get("record")) {
                (Some(dataset), Some(record)) => Ok((dataset.to_owned(), record.clone(), version)),
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
    // Per file and dataset: rows, capture time and mapping time (µs), slowest
    // page, pages read, and page rows captured (more than pages read when a
    // page was split before a canonical object it repeats).
    let mut timing: BTreeMap<(String, String), [u128; 6]> = BTreeMap::new();
    // OBS-PAGE-1: consecutive rows of one dataset travel as one page, split
    // before a repeated record identity and at the page bound.
    for snapshot in &snapshots {
        let file = snapshot.path.display().to_string();
        let mut pages: Vec<(String, Vec<Observed<RuntimeValue>>)> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (dataset, record, version) in &snapshot.rows {
            let observed = raw(&snapshot.source, &snapshot.mapping, record, version.as_deref())?;
            let identity = observed.provenance.source_id.clone();
            let split = pages.last().is_none_or(|(current, rows)| {
                current != dataset || rows.len() >= PAGE_ROWS || !seen.insert(identity.clone())
            });
            if split {
                seen = std::collections::BTreeSet::from([identity]);
                pages.push((dataset.clone(), Vec::new()));
            }
            if let Some((_, rows)) = pages.last_mut() {
                rows.push(observed);
            }
        }
        for (dataset, rows) in &pages {
            let input = MappingInput {
                mapping: snapshot.mapping.clone(),
                dataset: dataset.clone(),
                connector_instance: snapshot.instance.clone(),
                partition: "snapshot".into(),
                recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
            };
            let started = std::time::Instant::now();
            let ids = cell
                .inject_mapping_page(rows, &input)
                .map_err(|error| format!("{file}: a {dataset} page was not captured: {error}"))?;
            let captured = started.elapsed().as_micros();
            let started = std::time::Instant::now();
            let entry = mapped.entry(file.clone()).or_default();
            entry.0 = entry.0.saturating_add(rows.len());
            for (id, _new) in &ids {
                for (member, outcome) in cell
                    .process_mapping_page(id)
                    .map_err(|error| format!("{file}: a {dataset} page failed: {error}"))?
                    .unwrap_or_default()
                {
                    match outcome {
                        MappingOutcome::Landed => entry.1 = entry.1.saturating_add(1),
                        MappingOutcome::AlreadyLanded => {}
                        MappingOutcome::Quarantined(reasons) => {
                            return Err(format!(
                                "{file}: {dataset} {member} was refused by the mapping: {}",
                                reasons.join(", ")
                            ));
                        }
                    }
                }
            }
            let mapping = started.elapsed().as_micros();
            let spent = timing.entry((file.clone(), dataset.clone())).or_default();
            *spent = [
                spent[0].saturating_add(u128::try_from(rows.len()).unwrap_or(u128::MAX)),
                spent[1].saturating_add(captured),
                spent[2].saturating_add(mapping),
                spent[3].max(mapping),
                spent[4].saturating_add(1),
                spent[5].saturating_add(u128::try_from(ids.len()).unwrap_or(u128::MAX)),
            ];
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
                .map(|((_, dataset), [rows, capture, map, slowest, pages, committed])| {
                    let ms = |micros: &u128| u64::try_from(micros / 1000).unwrap_or(u64::MAX);
                    (dataset.clone(), serde_json::json!({ "rows": rows, "captureMs": ms(capture), "mapMs": ms(map), "slowestPageMs": ms(slowest), "pages": pages, "pageCommits": committed }))
                })
                .collect();
            serde_json::json!({ "file": file, "source": snapshot.source, "mapping": snapshot.mapping, "rows": rows, "newlyMapped": newly, "datasets": datasets })
        })
        .collect();
    Ok((cell.into_store(), Loaded { files }))
}
