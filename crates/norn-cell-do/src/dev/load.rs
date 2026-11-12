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
//! A snapshot is a recorded export ([`crate::recorded`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, MappingOutcome, SqliteCell};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{ObservationEvent, Observed};
use serde_json::Value;

use crate::recorded::Recorded;

/// Rows per page; the cell's bound is higher (OBS-PAGE-1 hosts default to 500).
const PAGE_ROWS: usize = norn_api::host::PAGE_ROWS;

/// What one load did, per snapshot file.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub(crate) files: Vec<Value>,
}

/// One recorded snapshot file, read and named.
struct Snapshot {
    path: PathBuf,
    recorded: Recorded,
}

fn snapshot(path: &Path) -> Result<Snapshot, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let document: Value =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let recorded =
        Recorded::parse(&document).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Snapshot {
        path: path.to_owned(),
        recorded,
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
        let recorded = &snapshot.recorded;
        for row in &recorded.rows {
            let dataset = &row.dataset;
            let observed = recorded
                .raw(row)
                .map_err(|error| format!("{file}: {error}"))?;
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
            let input = recorded.input(dataset);
            let started = std::time::Instant::now();
            let ids = cell
                .inject_mapping_page(rows, &input)
                .map_err(|error| format!("{file}: a {dataset} page was not captured: {error}"))?;
            let captured = started.elapsed().as_micros();
            let started = std::time::Instant::now();
            let entry = mapped.entry(file.clone()).or_default();
            entry.0 = entry.0.saturating_add(rows.len());
            for (id, _new) in &ids.pages {
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
                spent[5].saturating_add(u128::try_from(ids.pages.len()).unwrap_or(u128::MAX)),
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
            serde_json::json!({ "file": file, "source": snapshot.recorded.source, "mapping": snapshot.recorded.mapping, "rows": rows, "newlyMapped": newly, "datasets": datasets })
        })
        .collect();
    Ok((cell.into_store(), Loaded { files }))
}
