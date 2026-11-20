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
//! `--load` runs in the host process when the cell is opened or first
//! provisioned, before anything is served; a refused snapshot or a
//! quarantined row ends the session (DEV-LOAD-REFUSED-1). While serving, the
//! same mapping path takes slices over `POST /v1/observations:load`, decided
//! by the cell's policy as on a hosted cell. Loading is idempotent: a row
//! already captured is not captured again, and evidence already consumed is
//! not mapped again.
//!
//! A snapshot is a recorded export ([`crate::recorded`]): its snapshot rows,
//! then its incremental changes (RECORDED-INCREMENTAL-1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, MappingOutcome, SqliteCell};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{ObservationEvent, Observed};
use serde_json::Value;

use crate::recorded::Recorded;

/// Rows per page, as every host delivers them (see `norn_api::host::PAGE_ROWS`).
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
                        // SOURCE-RESOLVE-1: kept as evidence, written once
                        // a deciding source reports.
                        MappingOutcome::AlreadyLanded | MappingOutcome::Withheld(_) => {}
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dev-load scenarios over the committed packs"
)]
mod tests {
    //! RECORDED-INCREMENTAL-1 through `norn dev --load`: the product packs as
    //! `norn init --template three-systems` lays them out (the hosted trusted
    //! base, the cell-owned evidence types and six packs) admit on the dev
    //! host, and the packs' committed synthetic exports load whole, snapshot
    //! and incremental rows alike, so every canon type has records.
    use super::*;
    use norn_cell::StoreOptions;
    use norn_cell::engine::NativeEngine;

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn template() -> Value {
        let text = std::fs::read_to_string(
            repo().join("tools/norn-ts/templates/three-systems/template.json"),
        )
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn names(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect()
    }

    /// Every artifact the template copies into a project.
    fn artifacts() -> Vec<Value> {
        let template = template();
        let mut paths: Vec<PathBuf> = names(&template["core"])
            .iter()
            .map(|id| repo().join(format!("registry/core/artifacts/{id}.json")))
            .collect();
        for pack in names(&template["packs"]) {
            let dir = repo().join(format!("registry/packs/{pack}/artifacts"));
            let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            files.sort();
            paths.extend(files);
        }
        paths
            .iter()
            .map(|path| serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
            .collect()
    }

    /// The template is what a hosted tenant provisions: its core is the
    /// hosted trusted base plus the cell-owned `norn.` evidence types, and its
    /// packs are the tenant's packs, so a project admits what a hosted tenant
    /// does.
    #[test]
    fn the_template_is_the_hosted_base_cell_owned_types_and_tenant_packs() {
        let base = crate::base::trusted_base().unwrap();
        let template = template();
        let (mut owned, reserved): (Vec<String>, Vec<String>) = names(&template["core"])
            .into_iter()
            .partition(|name| name.starts_with("norn."));
        let mut ids: Vec<String> = reserved
            .iter()
            .map(|name| {
                let path = repo().join(format!("registry/core/artifacts/{name}.json"));
                let artifact: Value =
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                artifact["artifactId"].as_str().unwrap().to_owned()
            })
            .collect();
        ids.sort();
        let mut expected: Vec<String> = base.ids().iter().cloned().collect();
        expected.sort();
        assert_eq!(ids, expected);
        owned.sort();
        assert_eq!(owned, crate::base::CELL_OWNED);
        assert_eq!(names(&template["packs"]), crate::base::TENANT_PACKS);
    }

    #[test]
    fn the_synthetic_exports_load_whole_snapshot_and_incremental() {
        let admitted = crate::dev::admit::admit(&artifacts(), 1_000)
            .unwrap_or_else(|refused| panic!("{:#?}", refused.diagnostics));
        let scope = "local/dev";
        let directory = tempfile::tempdir().unwrap();
        let engine = NativeEngine::open(
            &directory.path().join("cell.sqlite"),
            StoreOptions::default(),
        )
        .unwrap();
        let store =
            CellStore::provision_definitions(engine, admitted.model, &admitted.input, scope)
                .unwrap();
        let paths: Vec<PathBuf> = names(&template()["data"])
            .iter()
            .map(|system| repo().join(format!("registry/packs/{system}/synthetic.json")))
            .collect();
        let (store, loaded) = load(store, scope, &paths).unwrap_or_else(|error| panic!("{error}"));
        // Every row is read: 7 snapshot rows each, plus 6, 4 and 6 changes.
        let rows: u64 = loaded
            .files
            .iter()
            .map(|file| file["rows"].as_u64().unwrap())
            .sum();
        assert_eq!(rows, 7 * 3 + 6 + 4 + 6, "{:#?}", loaded.files);
        let reader = store.read();
        for object_type in [
            "canon.Party",
            "canon.Item",
            "canon.Holding",
            "canon.OrderLine",
            "canon.Delivery",
            "canon.Posting",
            "canon.SettlementObservation",
        ] {
            let objects = reader
                .list(norn_cell::MAIN_WORLD, object_type, 100)
                .unwrap();
            assert!(!objects.is_empty(), "{object_type} has no records");
        }
        // The holding's later change is its current state.
        let holdings = reader
            .list(norn_cell::MAIN_WORLD, "canon.Holding", 100)
            .unwrap();
        let north = holdings
            .iter()
            .find(|holding| holding.id.contains("north"))
            .expect("the north holding");
        let quantity = serde_json::to_string(&north.values["quantity"]).unwrap();
        assert!(
            quantity.contains("180") && !quantity.contains("200"),
            "{quantity}"
        );
        // Loading again lands nothing new.
        let (_store, again) = load(store, scope, &paths).unwrap();
        let newly: u64 = again
            .files
            .iter()
            .map(|file| file["newlyMapped"].as_u64().unwrap())
            .sum();
        assert_eq!(newly, 0);
    }

    #[test]
    fn an_export_with_an_unreadable_change_is_refused_whole() {
        let directory = tempfile::tempdir().unwrap();
        let mut document: Value = serde_json::from_str(
            &std::fs::read_to_string(repo().join("registry/packs/erpnext/synthetic.json")).unwrap(),
        )
        .unwrap();
        document["incremental"][1]["operation"] = Value::from("delete");
        let path = directory.path().join("erpnext.json");
        std::fs::write(&path, document.to_string()).unwrap();
        let error = snapshot(&path).err().expect("refused");
        assert!(error.contains("incremental row 1"), "{error}");
    }
}
