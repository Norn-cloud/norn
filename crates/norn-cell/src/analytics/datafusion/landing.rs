//! MANIFEST-LAND-1 / MANIFEST-CACHE-1 / OLAP-02 on the adapter: landing
//! attempts the engine mints and holds per tenant, their staged rows, the
//! data and held-set files they write, the store that keeps those files as
//! a cache identity (one catalog commit with its receipt and lineage), and
//! the dataset a reader sees at a cache identity, rebuilt from exactly the
//! files of that identity, whichever engine wrote them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use datafusion::arrow::array::{ArrayRef, BooleanArray, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field};
use serde_json::value::RawValue;

use super::super::backend::StoreBody;
use super::super::store_body::{legacy_land_response, legacy_store_request};
use super::super::{
    ColumnKind, LandRequest, LandResponse, LandedFile, LoadRequest, LoadResponse, RuleCount,
    SettleRequest, StoreRequest, TableSchema, TenantId, ValidRange, dataset_cache_id,
};
use super::catalog::{self, sha256_file};
use super::columnar;
use super::files;
use super::laws::{self, HeldRow, TypedRow};
use super::residue::{self, Staged};
use super::{Engine, Outcome, STALE, engine_error, refusal};

/// A refusal of a landing attempt this engine does not hold.
pub const ATTEMPT_UNKNOWN: &str = "bulk.attempt_unknown";
/// A refusal of a landing attempt another operation holds.
pub const ATTEMPT_BUSY: &str = "bulk.attempt_busy";
const STORE_RECEIPT_VERSION: u8 = 1;

/// OLAP-02: one held landing attempt.
#[derive(Debug, Clone)]
pub(super) struct Held {
    leased: bool,
    landed: Option<Landed>,
}

/// The landing contract an attempt landed, its latest answer, and its
/// staged rows while residue remains.
#[derive(Debug, Clone)]
struct Landed {
    request: LandRequest,
    answer: LandResponse,
    staged: Option<Arc<Staged>>,
}

/// How far a landed attempt is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Staged,
    Written,
}

impl Phase {
    const fn of(response: &LandResponse) -> Self {
        if response.data.is_some() && response.held.is_some() {
            Self::Written
        } else {
            Self::Staged
        }
    }
}

/// The immutable part of a landing request: all of it but the presigned URL.
fn contract(request: &LandRequest) -> LandRequest {
    let mut request = request.clone();
    request.load.file.url.clear();
    request
}

/// The immutable part of a store request.
fn store_contract(request: &StoreRequest) -> StoreRequest {
    let mut request = request.clone();
    request.land = contract(&request.land);
    request.data.url.clear();
    request.held.url.clear();
    request
}

/// OLAP-02: exclusive use of one landing attempt; its one cleanup owner.
struct Lease<'e> {
    engine: &'e Engine,
    tenant: TenantId,
    token: String,
    dir: PathBuf,
    landed: Option<Landed>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let key = (self.tenant.clone(), std::mem::take(&mut self.token));
        if let Some(landed) = self.landed.take() {
            self.engine.attempts().insert(
                key,
                Held {
                    leased: false,
                    landed: Some(landed),
                },
            );
            return;
        }
        let _removed = std::fs::remove_dir_all(&self.dir);
        self.engine.attempts().remove(&key);
    }
}

/// OLAP-02 / MANIFEST-02: mint a landing attempt held by the returned lease.
fn begin_attempt<'e>(engine: &'e Engine, tenant: &TenantId) -> Outcome<(Lease<'e>, std::fs::File)> {
    let scratch = engine.private(tenant)?.join("scratch");
    let dir = tempfile::Builder::new()
        .prefix("a")
        .rand_bytes(31)
        .tempdir_in(&scratch)
        .map_err(|error| engine_error(error.to_string()))?
        .keep();
    let token = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let key = (tenant.clone(), token);
    let mut attempts = engine.attempts();
    if attempts.contains_key(&key) {
        drop(attempts);
        let _removed = std::fs::remove_dir_all(&dir);
        return Err(engine_error("a landing token was reused"));
    }
    attempts.insert(key.clone(), Held { leased: true, landed: None });
    drop(attempts);
    let lease = Lease {
        engine,
        tenant: key.0,
        token: key.1,
        dir,
        landed: None,
    };
    catalog::private_dir(&lease.dir)?;
    let file = catalog::create_new(&lease.dir.join("source"))?;
    Ok((lease, file))
}

/// OLAP-02: take the landed attempt of `request` for the step `phase`, every
/// check made before it is taken.
fn lease_attempt<'e>(
    engine: &'e Engine,
    request: &LandRequest,
    phase: Phase,
    answer: Option<&LandResponse>,
    through: Option<u64>,
) -> Outcome<(Lease<'e>, Landed)> {
    let tenant = &request.load.tenant;
    let dir = engine.private(tenant)?.join("scratch").join(&request.attempt);
    let mut attempts = engine.attempts();
    let Some(held) = attempts.get_mut(&(tenant.clone(), request.attempt.clone())) else {
        return Err(refusal(ATTEMPT_UNKNOWN, "the landing attempt is not held"));
    };
    if held.leased {
        return Err(refusal(ATTEMPT_BUSY, "the landing attempt is in use"));
    }
    let Some(landed) = held.landed.as_ref().filter(|landed| {
        landed.request == contract(request)
            && Phase::of(&landed.answer) == phase
            && answer.is_none_or(|answer| *answer == landed.answer)
    }) else {
        return Err(refusal(
            STALE,
            "the landing attempt is another landing's or another step's",
        ));
    };
    if let Some(through) = through
        && landed.answer.residue.last().map(|row| row.row) != Some(through)
    {
        return Err(refusal(
            STALE,
            "the settle does not end at the residue page the engine answered",
        ));
    }
    let landed = landed.clone();
    held.leased = true;
    drop(attempts);
    Ok((
        Lease {
            engine,
            tenant: tenant.clone(),
            token: request.attempt.clone(),
            dir,
            landed: None,
        },
        landed,
    ))
}

/// Read and type a source file: typed rows, typing refusals and counts.
pub(super) fn stage(request: &LoadRequest, file: &Path, bytes: u64, started: Instant) -> Outcome<Staged> {
    let source = files::read_source(request, file)?;
    let mut rows = Vec::new();
    let mut held = Vec::new();
    for row in source {
        let mut cells = Vec::new();
        let mut refused = None;
        for (column, text) in request.table.columns.iter().zip(&row.texts) {
            match files::type_text(column, text.as_deref()) {
                Ok(cell) => cells.push(cell),
                Err(reason) => {
                    refused = Some(reason);
                    break;
                }
            }
        }
        match refused {
            Some(reason) => held.push(HeldRow {
                row: row.row,
                reason,
                rule: None,
                holds: true,
                values: row.texts,
            }),
            None => rows.push(TypedRow { row: row.row, cells }),
        }
    }
    let reasons = files::count_reasons(held.iter().map(|row| row.reason.as_str()));
    let load = LoadResponse {
        load: request.load.clone(),
        rows: u64::try_from(rows.len()).unwrap_or(u64::MAX),
        rejected: reasons.values().fold(0_u64, |total, n| total.saturating_add(*n)),
        reasons,
        bytes,
        millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    Ok(Staged {
        table: request.table.clone(),
        rows,
        held,
        residue: std::collections::BTreeMap::new(),
        load,
    })
}

pub(super) fn digest_of(file: &Path) -> Outcome<(String, u64)> {
    sha256_file(file).map_err(|error| refusal("bulk.unreadable", error.to_string()))
}

/// The rows no rule holds back.
fn held_back(held: &[HeldRow]) -> BTreeSet<u64> {
    held.iter()
        .filter(|row| row.holds && row.rule.is_some())
        .map(|row| row.row)
        .collect()
}

fn data_system() -> Vec<Field> {
    vec![
        Field::new(":id", DataType::Utf8, true),
        Field::new(":v", DataType::UInt64, true),
        Field::new(":row", DataType::UInt64, true),
    ]
}

fn held_system() -> Vec<Field> {
    vec![
        Field::new(":row", DataType::UInt64, true),
        Field::new(":reason", DataType::Utf8, true),
        Field::new(":rule", DataType::Utf8, true),
        Field::new(":holds", DataType::Boolean, true),
    ]
}

/// Write the data file (typed rows no rule holds back, by row) and the
/// held-set file (by row, rule nulls first, reason) of `staged`.
pub(super) fn write_files(staged: &Staged, data: &Path, held: &Path) -> Outcome<(u64, u64)> {
    let table = &staged.table;
    let back = held_back(&staged.held);
    let rows: Vec<&TypedRow> = staged.rows.iter().filter(|row| !back.contains(&row.row)).collect();
    let schema = columnar::storage_schema(data_system(), table);
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.row.to_string()))),
        Arc::new(UInt64Array::from(vec![1_u64; rows.len()])),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.row))),
    ];
    for (index, column) in table.columns.iter().enumerate() {
        let cells: Vec<_> = rows
            .iter()
            .map(|row| row.cells.get(index).unwrap_or(&super::values::Cell::Null))
            .collect();
        arrays.extend(columnar::column_arrays(column, &cells)?);
    }
    columnar::write_parquet(data, &schema, &[columnar::batch(&schema, arrays)?])?;
    let mut quarantine: Vec<&HeldRow> = staged.held.iter().collect();
    quarantine.sort_by(|a, b| {
        (a.row, a.rule.as_ref().map(String::as_bytes), a.reason.as_bytes()).cmp(&(
            b.row,
            b.rule.as_ref().map(String::as_bytes),
            b.reason.as_bytes(),
        ))
    });
    let schema = columnar::text_schema(held_system(), table);
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from_iter_values(quarantine.iter().map(|r| r.row))),
        Arc::new(StringArray::from_iter_values(quarantine.iter().map(|r| r.reason.as_str()))),
        Arc::new(StringArray::from(
            quarantine.iter().map(|r| r.rule.clone()).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(quarantine.iter().map(|r| r.holds).collect::<Vec<_>>())),
    ];
    for index in 0..table.columns.len() {
        arrays.push(Arc::new(StringArray::from(
            quarantine
                .iter()
                .map(|r| r.values.get(index).cloned().flatten())
                .collect::<Vec<_>>(),
        )));
    }
    columnar::write_parquet(held, &schema, &[columnar::batch(&schema, arrays)?])?;
    Ok((
        u64::try_from(rows.len()).unwrap_or(u64::MAX),
        u64::try_from(quarantine.len()).unwrap_or(u64::MAX),
    ))
}

/// Each temporal column's earliest and latest admitted value, by instant.
fn valid_ranges(staged: &Staged) -> Vec<ValidRange> {
    let back = held_back(&staged.held);
    let mut ranges = Vec::new();
    for (index, column) in staged.table.columns.iter().enumerate() {
        if !matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            continue;
        }
        let instants: Vec<(&str, (i64, i32))> = staged
            .rows
            .iter()
            .filter(|row| !back.contains(&row.row))
            .filter_map(|row| {
                let cell = row.cells.get(index)?;
                Some((cell.as_str()?, cell.instant()?))
            })
            .collect();
        let first = instants.iter().min_by_key(|(_, key)| *key);
        let last = instants.iter().rev().max_by_key(|(_, key)| *key);
        if let (Some((from, _)), Some((to, _))) = (first, last) {
            ranges.push(ValidRange {
                column: column.id.clone(),
                from: (*from).to_owned(),
                to: (*to).to_owned(),
            });
        }
    }
    ranges
}

/// The answer to a landing step: the next residue page, or the written files.
fn landing(dir: &Path, request: &LandRequest, staged: &Staged) -> Outcome<LandResponse> {
    let mut findings: std::collections::BTreeMap<String, RuleCount> = std::collections::BTreeMap::new();
    for row in &staged.held {
        if let Some(rule) = &row.rule {
            let count = findings.entry(rule.clone()).or_insert_with(|| RuleCount {
                label: row.reason.clone(),
                rows: 0,
                holds: false,
            });
            if row.reason.as_bytes() < count.label.as_bytes() {
                count.label.clone_from(&row.reason);
            }
            count.rows = count.rows.saturating_add(1);
            count.holds |= row.holds;
        }
    }
    let mut response = LandResponse {
        landing: request.landing.clone(),
        attempt: request.attempt.clone(),
        load: staged.load.clone(),
        data: None,
        held: None,
        valid_time: Vec::new(),
        findings,
        held_by_rules: u64::try_from(held_back(&staged.held).len()).unwrap_or(u64::MAX),
        residue_total: u64::try_from(staged.residue.len()).unwrap_or(u64::MAX),
        residue: Vec::new(),
        stored: None,
    };
    if !staged.residue.is_empty() {
        response.residue = residue::page(staged, 0, request.residue_page)?;
        return Ok(response);
    }
    let (data, held) = (dir.join("data.parquet"), dir.join("held.parquet"));
    let (data_rows, held_rows) = write_files(staged, &data, &held)?;
    response.valid_time = valid_ranges(staged);
    let landed = |path: &Path, rows: u64| -> Outcome<LandedFile> {
        catalog::seal(path)?;
        let (sha256, bytes) = sha256_file(path).map_err(|e| engine_error(e.to_string()))?;
        Ok(LandedFile { sha256, rows, bytes })
    };
    response.data = Some(landed(&data, data_rows)?);
    response.held = Some(landed(&held, held_rows)?);
    Ok(response)
}

/// MANIFEST-LAND-1 / MANIFEST-LAWS-1: land `request` in a fresh attempt.
pub(super) fn land(
    engine: &Engine,
    request: &LandRequest,
    fetch: impl FnOnce(std::fs::File) -> Outcome<()>,
) -> Outcome<LandResponse> {
    if !request.attempt.is_empty() {
        return Err(refusal(
            "analytics.invalid",
            "the engine names a landing's attempt; send none",
        ));
    }
    let (mut lease, source) = begin_attempt(engine, &request.load.tenant)?;
    fetch(source)?;
    let request = &LandRequest {
        attempt: lease.token.clone(),
        ..request.clone()
    };
    let started = Instant::now();
    let file = lease.dir.join("source");
    catalog::seal(&file)?;
    let (digest, bytes) = digest_of(&file)?;
    if digest != request.load.file.sha256 {
        return Err(refusal("bulk.digest_mismatch", "the file is not the recorded upload"));
    }
    let landed = (|| {
        let mut staged = stage(&request.load, &file, bytes, started)?;
        let checked = laws::check_rules(&staged.table, &request.checks, &staged.rows)?;
        staged.residue = checked.residue;
        staged.held.extend(checked.findings);
        residue::fits(&staged, request.residue_page)?;
        let answer = landing(&lease.dir, request, &staged)?;
        Ok::<_, super::super::EngineRefusal>((answer, staged))
    })();
    let _source = std::fs::remove_file(&file);
    let (answer, staged) = landed?;
    let keep = !answer.residue.is_empty();
    lease.landed = Some(Landed {
        request: contract(request),
        answer: answer.clone(),
        staged: keep.then(|| Arc::new(staged)),
    });
    Ok(answer)
}

/// MANIFEST-LAWS-1: record the per-row findings of a residue page and answer
/// the next step.
pub(super) fn settle(engine: &Engine, request: &SettleRequest) -> Outcome<LandResponse> {
    let land = &request.land;
    let (mut lease, landed) = lease_attempt(engine, land, Phase::Staged, None, Some(request.through))?;
    let mut staged = landed
        .staged
        .as_deref()
        .cloned()
        .ok_or_else(|| refusal(STALE, "the landing attempt is not staged"))?;
    residue::settle(&mut staged, &land.checks, request)?;
    let answer = landing(&lease.dir, land, &staged)?;
    let keep = !answer.residue.is_empty();
    lease.landed = Some(Landed {
        request: contract(land),
        answer: answer.clone(),
        staged: keep.then(|| Arc::new(staged)),
    });
    Ok(answer)
}

/// A store's answer, and the recorded bytes of a replayed one.
pub(super) struct StoreAnswer {
    pub answer: LandResponse,
    recorded: Option<Box<RawValue>>,
}

/// MANIFEST-CACHE-1 file record of a stored landing.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct KeptLanding {
    pub table: TableSchema,
    pub data: KeptFile,
    pub held: KeptFile,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct KeptFile {
    pub path: PathBuf,
    pub sha256: String,
    pub uri: String,
}

#[derive(serde::Serialize)]
struct StoreReceipt<'r> {
    version: u8,
    request: &'r StoreRequest,
    response: &'r LandResponse,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedStore {
    #[serde(default)]
    version: Option<u8>,
    request: Box<RawValue>,
    response: Box<RawValue>,
}

pub(super) fn json_text(value: &impl serde::Serialize) -> Outcome<String> {
    serde_json::to_string(value).map_err(|error| engine_error(error.to_string()))
}

/// MANIFEST-CACHE-1: keep an attempt's written files as its cache identity:
/// files installed as objects first, then the kept record, the receipt and
/// the load's lineage in one catalog commit.
pub(super) fn store(
    engine: &Engine,
    request: &StoreRequest,
    send: impl FnOnce(&Path, &Path) -> Outcome<()>,
) -> Outcome<StoreAnswer> {
    let land = &request.land;
    let tenant = &land.load.tenant;
    let table = &land.load.table;
    if request.landed.landing != land.landing || request.landed.attempt != land.attempt {
        return Err(refusal(STALE, "the answer is not this landing's"));
    }
    if request.landed.data.is_none() || request.landed.held.is_none() {
        return Err(refusal("analytics.invalid", "the landing has residue to settle"));
    }
    let (lease, landed) = match lease_attempt(engine, land, Phase::Written, Some(&request.landed), None) {
        Ok(leased) => leased,
        Err(unknown) if unknown.code == ATTEMPT_UNKNOWN => {
            return replayed(engine, request, false)?.ok_or(unknown);
        }
        Err(refused) => return Err(refused),
    };
    let answer = landed.answer;
    let (Some(data_file), Some(held_file)) = (&answer.data, &answer.held) else {
        return Err(engine_error("a written attempt names its files"));
    };
    let (data, held) = (lease.dir.join("data.parquet"), lease.dir.join("held.parquet"));
    send(&data, &held)?;
    let cache = dataset_cache_id(table, &data_file.sha256, &held_file.sha256);
    let response = LandResponse {
        stored: Some(cache.clone()),
        ..answer.clone()
    };
    engine.with(tenant, |tenant| {
        let mut kept = Vec::new();
        for (path, file) in [(&data, data_file), (&held, held_file)] {
            catalog::verify_object(path, &file.sha256)?;
            let (target, _sha, _bytes) = catalog::install(path, &tenant.objects())?;
            kept.push(target);
        }
        let record = json_text(&KeptLanding {
            table: table.clone(),
            data: KeptFile {
                path: kept.first().cloned().unwrap_or_default(),
                sha256: data_file.sha256.clone(),
                uri: request.data.uri.clone(),
            },
            held: KeptFile {
                path: kept.get(1).cloned().unwrap_or_default(),
                sha256: held_file.sha256.clone(),
                uri: request.held.uri.clone(),
            },
        })?;
        let receipt = json_text(&StoreReceipt {
            version: STORE_RECEIPT_VERSION,
            request: &store_contract(request),
            response: &response,
        })?;
        tenant.catalog.write(|tx| {
            catalog::set(tx, &format!("landing:{cache}"), &record)?;
            catalog::set(tx, &format!("store:{}", land.attempt), &receipt)?;
            catalog::set(
                tx,
                &format!("lineage:{}", land.load.load),
                &format!("{} {}", land.load.file.sha256, land.load.file.uri),
            )
        })
    })?;
    drop(lease);
    Ok(StoreAnswer {
        answer: response,
        recorded: None,
    })
}

/// Store a call's landed files or return the exact recorded JSON body.
pub(super) fn store_json(
    engine: &Engine,
    body: &StoreBody,
    send: impl FnOnce(&Path, &Path) -> Outcome<()>,
) -> Outcome<Box<RawValue>> {
    let result = if body.is_legacy() {
        replayed(engine, body.request(), true)?
            .ok_or_else(|| refusal(ATTEMPT_UNKNOWN, "the legacy receipt does not exist"))?
    } else {
        store(engine, body.request(), send)?
    };
    result.recorded.map_or_else(
        || serde_json::value::to_raw_value(&result.answer).map_err(|e| engine_error(e.to_string())),
        Ok,
    )
}

/// OLAP-02: the answer of a store of `request` that already happened.
fn replayed(engine: &Engine, request: &StoreRequest, legacy: bool) -> Outcome<Option<StoreAnswer>> {
    let land = &request.land;
    let Some(text) = engine.with(&land.load.tenant, |tenant| {
        tenant.catalog.meta(&format!("store:{}", land.attempt))
    })?
    else {
        return Ok(None);
    };
    let decode = |error: serde_json::Error| engine_error(error.to_string());
    let receipt: RecordedStore = serde_json::from_str(&text).map_err(decode)?;
    let recorded: StoreRequest = match (receipt.version, legacy) {
        (None, _) => legacy_store_request(&receipt.request).map_err(decode)?,
        (Some(STORE_RECEIPT_VERSION), false) => {
            serde_json::from_str(receipt.request.get()).map_err(decode)?
        }
        (Some(STORE_RECEIPT_VERSION), true) => {
            return Err(refusal(STALE, "the store is another request's"));
        }
        (Some(_), _) => return Err(engine_error("unknown store receipt version")),
    };
    if recorded != store_contract(request) {
        return Err(refusal(STALE, "the store is another request's"));
    }
    let answer = match receipt.version {
        None => legacy_land_response(&receipt.response).map_err(decode)?,
        Some(_) => serde_json::from_str(receipt.response.get()).map_err(decode)?,
    };
    Ok(Some(StoreAnswer {
        answer,
        recorded: Some(receipt.response),
    }))
}
