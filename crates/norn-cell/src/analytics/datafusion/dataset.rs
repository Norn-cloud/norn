//! MANIFEST-CACHE-1 / MANIFEST-LINEAGE-1 on the adapter: the dataset a
//! reader sees. A direct load and a stored landing publish their data and
//! held-set files with the dataset's load and lineage in one catalog commit;
//! a reader at another cache identity first rebuilds the current dataset from
//! exactly the files of that identity (each verified against its digest),
//! whichever engine wrote them; lineage answers every source row.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use datafusion::logical_expr::lit;

use super::super::{
    LineageRequest, LineageResponse, LoadRequest, LoadResponse, RowFinding, TableSchema,
    dataset_cache_id,
};
use super::catalog;
use super::columnar;
use super::files::Caps;
use super::landing::{KeptLanding, digest_of, json_text};
use super::laws::TypedRow;
use super::staging;
use super::{DATASET_COLD, Engine, Outcome, STALE, Tenant, engine_error, refusal};

/// BULK-LOAD-1: a direct load, staged on disk and streamed as a landing is,
/// its files and lineage published together.
pub(super) fn load(
    engine: &Engine,
    tenant: &mut Tenant,
    request: &LoadRequest,
    file: &Path,
) -> Outcome<LoadResponse> {
    let (digest, bytes) = digest_of(file)?;
    if digest != request.file.sha256 {
        return Err(refusal(
            "bulk.digest_mismatch",
            "the file is not the recorded upload",
        ));
    }
    let dir = tempfile::Builder::new()
        .prefix("load")
        .tempdir_in(tenant.staging())
        .map_err(|error| engine_error(error.to_string()))?;
    let caps = Caps::of(engine.memory);
    let staged = staging::stage(request, &[], file, dir.path(), bytes, caps)?;
    let (data, held) = (
        dir.path().join("data.parquet"),
        dir.path().join("held.parquet"),
    );
    staging::finish(engine, &staged, &data, &held)?;
    let objects = tenant.objects();
    let (data_path, data_sha, _) = catalog::install(&data, &objects)?;
    let (held_path, held_sha, _) = catalog::install(&held, &objects)?;
    let files = json_text(&Files {
        data: data_path,
        data_sha256: data_sha,
        held: held_path,
        held_sha256: held_sha,
    })?;
    let schema = json_text(&request.table)?;
    let name = &request.table.name;
    tenant.catalog.write(|tx| {
        catalog::set(tx, &format!("load:{name}"), &request.load)?;
        catalog::set(tx, &format!("dataset:{name}"), &schema)?;
        catalog::set(tx, &format!("files:{name}"), &files)?;
        catalog::set(
            tx,
            &format!("lineage:{}", request.load),
            &format!("{} {}", request.file.sha256, request.file.uri),
        )
    })?;
    Ok(staged.load)
}

/// The files a dataset is read from now.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Files {
    data: PathBuf,
    data_sha256: String,
    held: PathBuf,
    held_sha256: String,
}

/// A dataset as a reader sees it.
pub(super) struct Dataset {
    pub table: TableSchema,
    pub rows: Vec<TypedRow>,
    /// Rows a rule holds back.
    pub held: BTreeSet<u64>,
}

/// MANIFEST-CACHE-1: rebuild the current dataset `table` at the cache
/// identity `load` from the files kept for it, each verified first.
fn hydrate(tenant: &mut Tenant, table: &str, load: &str) -> Outcome<()> {
    let cold = || {
        refusal(
            DATASET_COLD,
            "the dataset's files are not kept by this engine",
        )
    };
    let text = tenant
        .catalog
        .meta(&format!("landing:{load}"))?
        .ok_or_else(cold)?;
    let kept: KeptLanding = serde_json::from_str(&text).map_err(|e| engine_error(e.to_string()))?;
    if kept.table.name != table
        || dataset_cache_id(&kept.table, &kept.data.sha256, &kept.held.sha256) != load
    {
        return Err(refusal(STALE, "the kept landing is not this dataset"));
    }
    catalog::verify_object(&kept.data.path, &kept.data.sha256)?;
    catalog::verify_object(&kept.held.path, &kept.held.sha256)?;
    let files = json_text(&Files {
        data: kept.data.path.clone(),
        data_sha256: kept.data.sha256.clone(),
        held: kept.held.path.clone(),
        held_sha256: kept.held.sha256.clone(),
    })?;
    let schema = json_text(&kept.table)?;
    tenant.catalog.write(|tx| {
        catalog::set(tx, &format!("load:{table}"), load)?;
        catalog::set(tx, &format!("dataset:{table}"), &schema)?;
        catalog::set(tx, &format!("files:{table}"), &files)?;
        catalog::set(
            tx,
            &format!("lineage:{load}"),
            &format!("{} {}", kept.data.sha256, kept.data.uri),
        )
    })
}

/// Hydrate (on cache miss) and verify the current schema and files for `table` at `load`.
fn open_files(tenant: &mut Tenant, table: &str, load: &str) -> Outcome<(TableSchema, Files)> {
    if tenant.catalog.meta(&format!("load:{table}"))?.as_deref() != Some(load) {
        hydrate(tenant, table, load)?;
    }
    // N7: the dataset's schema and files from one catalog state.
    let (schema, files) = tenant.catalog.read(|tx| {
        Ok((
            catalog::get(tx, &format!("dataset:{table}"))?,
            catalog::get(tx, &format!("files:{table}"))?,
        ))
    })?;
    let cold = || {
        refusal(
            DATASET_COLD,
            "the dataset's files are not kept by this engine",
        )
    };
    let schema: TableSchema =
        serde_json::from_str(&schema.ok_or_else(|| refusal(super::UNSUPPORTED, "no such table"))?)
            .map_err(|e| engine_error(e.to_string()))?;
    let files: Files =
        serde_json::from_str(&files.ok_or_else(cold)?).map_err(|e| engine_error(e.to_string()))?;
    // N8: each file is the bytes its record names before it is read.
    tenant.verify(&files.data, &files.data_sha256)?;
    tenant.verify(&files.held, &files.held_sha256)?;
    Ok((schema, files))
}

/// The dataset `table` at `load`, hydrated first on a cache miss.
pub(super) fn dataset(
    engine: &Engine,
    tenant: &mut Tenant,
    table: &str,
    load: &str,
) -> Outcome<Dataset> {
    let (schema, files) = open_files(tenant, table, load)?;
    let rows = read_rows(engine, tenant, &schema, &files.data)?;
    let mut held = BTreeSet::new();
    let plan = super::scan::Plan::Scan {
        files: vec![files.held],
        filter: Some(
            super::scan::col(":holds")
                .eq(lit(true))
                .and(super::scan::col(":rule").is_not_null()),
        ),
        columns: Some(vec![":row".to_owned()]),
        order: Vec::new(),
    };
    for batch in engine.collect(tenant, plan)? {
        held.extend(
            columnar::system(&batch, ":row", columnar::u64_at)?
                .into_iter()
                .flatten(),
        );
    }
    Ok(Dataset {
        table: schema,
        rows,
        held,
    })
}

/// The typed rows of a data file.
fn read_rows(
    engine: &Engine,
    tenant: &Tenant,
    table: &TableSchema,
    data: &Path,
) -> Outcome<Vec<TypedRow>> {
    let mut rows = Vec::new();
    for batch in engine.scan(tenant, &[data.to_path_buf()], None)? {
        let numbers = columnar::system(&batch, ":row", columnar::u64_at)?;
        let columns = table
            .columns
            .iter()
            .map(|column| columnar::column_cells(&batch, column))
            .collect::<Outcome<Vec<_>>>()?;
        for (at, number) in numbers.into_iter().enumerate() {
            rows.push(TypedRow {
                row: number.ok_or_else(|| engine_error("a data row without its number"))?,
                cells: columns
                    .iter()
                    .map(|cells| cells.get(at).cloned().unwrap_or(super::values::Cell::Null))
                    .collect(),
            });
        }
    }
    rows.sort_by_key(|row| row.row);
    Ok(rows)
}

/// MANIFEST-LINEAGE-1: what the landing holds for one source row.
pub(super) fn lineage(
    engine: &Engine,
    tenant: &mut Tenant,
    request: &LineageRequest,
) -> Outcome<LineageResponse> {
    let (_schema, files) = open_files(tenant, &request.table, &request.load)?;
    let row_filter = || super::scan::col(":row").eq(lit(request.row));
    let data_plan = super::scan::Plan::Scan {
        files: vec![files.data],
        filter: Some(row_filter()),
        columns: Some(vec![":row".to_owned()]),
        order: Vec::new(),
    };
    let matched_rows: usize = engine
        .collect(tenant, data_plan)?
        .iter()
        .map(datafusion::arrow::record_batch::RecordBatch::num_rows)
        .fold(0_usize, usize::saturating_add);
    let admitted = matched_rows == 1;
    let held_plan = super::scan::Plan::Scan {
        files: vec![files.held],
        filter: Some(row_filter()),
        columns: Some(vec![
            ":reason".to_owned(),
            ":rule".to_owned(),
            ":holds".to_owned(),
        ]),
        order: Vec::new(),
    };
    let mut held: Vec<(Option<String>, String, bool)> = Vec::new();
    for batch in engine.collect(tenant, held_plan)? {
        let reason = columnar::system(&batch, ":reason", columnar::text_at)?;
        let rule = columnar::system(&batch, ":rule", columnar::text_at)?;
        let holds = columnar::system(&batch, ":holds", columnar::bool_at)?;
        for ((reason, rule), holds) in reason.into_iter().zip(rule).zip(holds) {
            held.push((rule, reason.unwrap_or_default(), holds.unwrap_or(false)));
        }
    }
    held.sort_by(|a, b| {
        (a.0.as_ref().map(String::as_bytes), a.1.as_bytes())
            .cmp(&(b.0.as_ref().map(String::as_bytes), b.1.as_bytes()))
    });
    let mut response = LineageResponse {
        admitted,
        refused: None,
        findings: Vec::new(),
    };
    for (rule, reason, holds) in held {
        match rule {
            Some(rule) => response.findings.push(RowFinding {
                row: request.row,
                rule,
                label: reason,
                holds,
            }),
            None => response.refused = Some(reason),
        }
    }
    Ok(response)
}
