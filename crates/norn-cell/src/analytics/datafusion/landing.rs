//! MANIFEST-LAND-1 / MANIFEST-CACHE-1 / OLAP-02 on the adapter: a landing's
//! steps. A land stages its source on disk ([`super::staging`]) under an
//! attempt the engine mints ([`super::attempts`]); a settle records a residue
//! page's findings; once no residue is left the attempt's data and held-set
//! files are written, and a store keeps them as a cache identity (installed
//! first, then one catalog commit with its receipt and lineage).

use std::path::{Path, PathBuf};

use serde_json::value::RawValue;

use super::super::backend::StoreBody;
use super::super::store_body::{legacy_land_response, legacy_store_request};
use super::super::{
    LandRequest, LandResponse, LandedFile, SettleRequest, StoreRequest, TableSchema,
    dataset_cache_id,
};
use super::attempts::{self, ATTEMPT_UNKNOWN, Landed, Phase, contract};
use super::catalog::{self, sha256_file};
use super::files::Caps;
use super::staging::{self, Staged};
use super::{Engine, Outcome, STALE, Tenant, engine_error, refusal};

const STORE_RECEIPT_VERSION: u8 = 1;

/// The immutable part of a store request.
fn store_contract(request: &StoreRequest) -> StoreRequest {
    let mut request = request.clone();
    request.land = contract(&request.land);
    request.data.url.clear();
    request.held.url.clear();
    request
}

pub(super) fn digest_of(file: &Path) -> Outcome<(String, u64)> {
    sha256_file(file).map_err(|error| refusal("bulk.unreadable", error.to_string()))
}

/// A finished file's landed record: sealed, hashed.
fn landed_file(path: &Path, rows: u64) -> Outcome<LandedFile> {
    catalog::seal(path)?;
    let (sha256, bytes) = sha256_file(path).map_err(|e| engine_error(e.to_string()))?;
    Ok(LandedFile {
        sha256,
        rows,
        bytes,
    })
}

/// The answer to a landing step: the next residue page, or (none left) the
/// attempt's written files, after which its staging is removed.
fn landing(
    engine: &Engine,
    tenant: &Tenant,
    dir: &Path,
    request: &LandRequest,
    staged: &Staged,
) -> Outcome<LandResponse> {
    let mut response = LandResponse {
        landing: request.landing.clone(),
        attempt: request.attempt.clone(),
        load: staged.load.clone(),
        data: None,
        held: None,
        valid_time: Vec::new(),
        findings: staged.counts.clone(),
        held_by_rules: staged.held_by_rules,
        residue_total: u64::try_from(staged.residue.len()).unwrap_or(u64::MAX),
        residue: Vec::new(),
        stored: None,
    };
    if !staged.residue.is_empty() {
        response.residue = super::residue::page(engine, tenant, staged, 0, request.residue_page)?;
        return Ok(response);
    }
    let (data, held) = (dir.join("data.parquet"), dir.join("held.parquet"));
    let written = staging::finish(engine, tenant, staged, &data, &held)?;
    response.valid_time = written.valid_time;
    response.data = Some(landed_file(&data, written.data_rows)?);
    response.held = Some(landed_file(&held, written.held_rows)?);
    remove_staging(staged);
    Ok(response)
}

/// Remove a staged landing's intermediate files.
pub(super) fn remove_staging(staged: &Staged) {
    let mut files: Vec<&PathBuf> = vec![&staged.typed, &staged.typing];
    files.extend(staged.findings.iter());
    for file in files {
        let _removed = std::fs::remove_file(file);
    }
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
    let (mut lease, source) = attempts::begin(engine, &request.load.tenant)?;
    fetch(source)?;
    let request = &LandRequest {
        attempt: lease.token.clone(),
        ..request.clone()
    };
    let file = lease.dir.join("source");
    catalog::seal(&file)?;
    let (digest, bytes) = digest_of(&file)?;
    if digest != request.load.file.sha256 {
        return Err(refusal(
            "bulk.digest_mismatch",
            "the file is not the recorded upload",
        ));
    }
    let caps = Caps::of(engine.memory);
    let dir = lease.dir.clone();
    let landed = engine.with(&request.load.tenant, |tenant| {
        let staged = staging::stage(&request.load, &request.checks, &file, &dir, bytes, caps)?;
        super::residue::fits(engine, tenant, &staged, request.residue_page)?;
        let answer = landing(engine, tenant, &dir, request, &staged)?;
        Ok((answer, staged))
    });
    let _source = std::fs::remove_file(&file);
    let (answer, staged) = landed?;
    let keep = !answer.residue.is_empty();
    lease.landed = Some(Landed {
        request: contract(request),
        answer: answer.clone(),
        staged: keep.then_some(staged),
    });
    Ok(answer)
}

/// MANIFEST-LAWS-1: record the per-row findings of a residue page and answer
/// the next step. A refused settle removes the attempt, its files with it.
pub(super) fn settle(engine: &Engine, request: &SettleRequest) -> Outcome<LandResponse> {
    let land = &request.land;
    let (mut lease, landed) =
        attempts::lease(engine, land, Phase::Staged, None, Some(request.through))?;
    let mut staged = landed
        .staged
        .ok_or_else(|| refusal(STALE, "the landing attempt is not staged"))?;
    let dir = lease.dir.clone();
    let answer = engine.with(&land.load.tenant, |tenant| {
        super::residue::settle(engine, tenant, &mut staged, &land.checks, request)?;
        landing(engine, tenant, &dir, land, &staged)
    })?;
    let keep = !answer.residue.is_empty();
    lease.landed = Some(Landed {
        request: contract(land),
        answer: answer.clone(),
        staged: keep.then_some(staged),
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
        return Err(refusal(
            "analytics.invalid",
            "the landing has residue to settle",
        ));
    }
    let (lease, landed) =
        match attempts::lease(engine, land, Phase::Written, Some(&request.landed), None) {
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
    let (data, held) = (
        lease.dir.join("data.parquet"),
        lease.dir.join("held.parquet"),
    );
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
