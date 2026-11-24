//! MANIFEST-CACHE-1 / MANIFEST-LINEAGE-1 on the adapter: the dataset a
//! reader sees. A direct load and a stored landing publish their data and
//! held-set files with the dataset's load and lineage in one catalog commit;
//! a reader at another cache identity first rebuilds the current dataset from
//! exactly the files of that identity (each verified against its digest),
//! whichever engine wrote them; lineage answers every source row.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use datafusion::common::Column as DfColumn;
use datafusion::logical_expr::{Expr, lit};

use super::super::{
    LineageRequest, LineageResponse, LoadRequest, LoadResponse, RowFinding, TableSchema,
    dataset_cache_id,
};
use super::catalog;
use super::columnar;
use super::landing::{KeptLanding, digest_of, json_text, stage, write_files};
use super::laws::TypedRow;
use super::{DATASET_COLD, Engine, Outcome, STALE, Tenant, engine_error, refusal};

/// BULK-LOAD-1: a direct load, its files and lineage published together.
pub(super) fn load(
    tenant: &mut Tenant,
    request: &LoadRequest,
    file: &Path,
) -> Outcome<LoadResponse> {
    let started = Instant::now();
    let (digest, bytes) = digest_of(file)?;
    if digest != request.file.sha256 {
        return Err(refusal(
            "bulk.digest_mismatch",
            "the file is not the recorded upload",
        ));
    }
    let staged = stage(request, file, bytes, started)?;
    let staging = tenant.staging();
    let (data, held) = (
        staging.join("load-data.parquet"),
        staging.join("load-held.parquet"),
    );
    for stale in [&data, &held] {
        let _removed = std::fs::remove_file(stale);
    }
    write_files(&staged, &data, &held)?;
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
    files: Files,
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

/// The dataset `table` at `load`, hydrated first on a cache miss.
pub(super) fn dataset(
    engine: &Engine,
    tenant: &mut Tenant,
    table: &str,
    load: &str,
) -> Outcome<Dataset> {
    if tenant.catalog.meta(&format!("load:{table}"))?.as_deref() != Some(load) {
        hydrate(tenant, table, load)?;
    }
    let schema: TableSchema = serde_json::from_str(
        &tenant
            .catalog
            .meta(&format!("dataset:{table}"))?
            .ok_or_else(|| refusal(super::UNSUPPORTED, "no such table"))?,
    )
    .map_err(|e| engine_error(e.to_string()))?;
    let files: Files =
        serde_json::from_str(&tenant.catalog.meta(&format!("files:{table}"))?.ok_or_else(
            || {
                refusal(
                    DATASET_COLD,
                    "the dataset's files are not kept by this engine",
                )
            },
        )?)
        .map_err(|e| engine_error(e.to_string()))?;
    let rows = read_rows(engine, tenant, &schema, &files.data, None)?;
    let mut held = BTreeSet::new();
    for batch in engine.scan(tenant, std::slice::from_ref(&files.held), None)? {
        let row = columnar::system(&batch, ":row", columnar::u64_at)?;
        let rule = columnar::system(&batch, ":rule", columnar::text_at)?;
        let holds = columnar::system(&batch, ":holds", columnar::bool_at)?;
        for ((row, rule), holds) in row.into_iter().zip(rule).zip(holds) {
            if let (Some(row), Some(_), Some(true)) = (row, rule, holds) {
                held.insert(row);
            }
        }
    }
    Ok(Dataset {
        table: schema,
        rows,
        held,
        files,
    })
}

/// The typed rows of a data file, optionally only those a typed filter keeps.
fn read_rows(
    engine: &Engine,
    tenant: &Tenant,
    table: &TableSchema,
    data: &Path,
    filter: Option<Expr>,
) -> Outcome<Vec<TypedRow>> {
    let mut rows = Vec::new();
    for batch in engine.scan(tenant, &[data.to_path_buf()], filter)? {
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
    let dataset = dataset(engine, tenant, &request.table, &request.load)?;
    let row_filter =
        |name: &str| Expr::Column(DfColumn::new_unqualified(name)).eq(lit(request.row));
    let admitted = read_rows(
        engine,
        tenant,
        &dataset.table,
        &dataset.files.data,
        Some(row_filter(":row")),
    )?
    .len()
        == 1;
    let mut held: Vec<(Option<String>, String, bool)> = Vec::new();
    for batch in engine.scan(
        tenant,
        std::slice::from_ref(&dataset.files.held),
        Some(row_filter(":row")),
    )? {
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
