//! OLAP-02 / MANIFEST-02 on the adapter, B1: the landing attempts an engine
//! mints and holds. An attempt is a private directory (its source, its staged
//! files, its written files) and, in memory, only its contract, its last
//! answer and its staged counts and residue. The engine holds a bounded
//! number of attempts (in all and per tenant) and bytes on disk; an idle
//! attempt past its time to live, or the oldest idle one when a bound is
//! reached, is evicted, its directory removed with it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::{LandRequest, LandResponse, TenantId};
use super::staging::Staged;
use super::{EXHAUSTED, Engine, Outcome, STALE, engine_error, refusal};

/// A refusal of a landing attempt this engine does not hold.
pub const ATTEMPT_UNKNOWN: &str = "bulk.attempt_unknown";
/// A refusal of a landing attempt another operation holds.
pub const ATTEMPT_BUSY: &str = "bulk.attempt_busy";
/// The most attempts an engine holds.
const MAX_ATTEMPTS: usize = 64;
/// The most attempts one tenant holds.
const MAX_TENANT_ATTEMPTS: usize = 16;
/// How long an idle attempt is kept.
pub(super) const ATTEMPT_TTL: Duration = Duration::from_secs(3600);

/// OLAP-02: one held landing attempt.
#[derive(Debug)]
pub(super) struct Held {
    leased: bool,
    landed: Option<Landed>,
    dir: PathBuf,
    touched: Instant,
    bytes: u64,
}

/// The landing contract an attempt landed, its latest answer, and its
/// staged state while residue remains.
#[derive(Debug)]
pub(super) struct Landed {
    pub request: LandRequest,
    pub answer: LandResponse,
    pub staged: Option<Staged>,
}

/// How far a landed attempt is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
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
pub(super) fn contract(request: &LandRequest) -> LandRequest {
    let mut request = request.clone();
    request.load.file.url.clear();
    request
}

/// OLAP-02: exclusive use of one landing attempt; its one cleanup owner.
pub(super) struct Lease<'e> {
    engine: &'e Engine,
    tenant: TenantId,
    pub token: String,
    pub dir: PathBuf,
    /// Set when the step leaves the attempt for a next step.
    pub landed: Option<Landed>,
}

/// The bytes of the files directly in `dir`.
fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter_map(|entry| entry.metadata().ok())
            .fold(0_u64, |total, meta| total.saturating_add(meta.len()))
    })
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let key = (self.tenant.clone(), std::mem::take(&mut self.token));
        let mut attempts = self.engine.attempts();
        if let (Some(landed), Some(held)) = (self.landed.take(), attempts.get_mut(&key)) {
            held.leased = false;
            held.landed = Some(landed);
            held.touched = Instant::now();
            held.bytes = dir_bytes(&self.dir);
        } else {
            attempts.remove(&key);
            drop(attempts);
            let _removed = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn is_full(
    attempts: &BTreeMap<(TenantId, String), Held>,
    tenant: &TenantId,
    memory: usize,
) -> bool {
    let byte_cap = u64::try_from(memory).unwrap_or(u64::MAX).saturating_mul(64);
    let mine = attempts.keys().filter(|(owner, _)| owner == tenant).count();
    let bytes = attempts
        .values()
        .fold(0_u64, |total, held| total.saturating_add(held.bytes));
    attempts.len() >= MAX_ATTEMPTS || mine >= MAX_TENANT_ATTEMPTS || bytes > byte_cap
}

/// Evict idle attempts past their time to live, then refuse a new one while
/// `tenant` or the engine is at a bound (in attempts or bytes on disk): a
/// live landing is never evicted to make room.
fn make_room(engine: &Engine, tenant: &TenantId) -> Outcome<()> {
    let mut attempts = engine.attempts();
    let ttl = engine.attempt_ttl;
    let expired: Vec<_> = attempts
        .iter()
        .filter(|(_, held)| !held.leased && held.touched.elapsed() >= ttl)
        .map(|(key, _)| key.clone())
        .collect();
    let mut removed = Vec::new();
    for key in expired {
        removed.extend(attempts.remove(&key).map(|held| held.dir));
    }
    let full = is_full(&attempts, tenant, engine.memory);
    drop(attempts);
    remove_dirs(removed);
    if full {
        return Err(refusal(
            EXHAUSTED,
            "the engine holds as many landings as it may",
        ));
    }
    Ok(())
}

fn remove_dirs(dirs: Vec<PathBuf>) {
    for dir in dirs {
        let _removed = std::fs::remove_dir_all(dir);
    }
}

/// OLAP-02 / MANIFEST-02: mint a landing attempt held by the returned lease.
pub(super) fn begin<'e>(
    engine: &'e Engine,
    tenant: &TenantId,
) -> Outcome<(Lease<'e>, std::fs::File)> {
    make_room(engine, tenant)?;
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
    if is_full(&attempts, tenant, engine.memory) {
        drop(attempts);
        let _removed = std::fs::remove_dir_all(&dir);
        return Err(refusal(
            EXHAUSTED,
            "the engine holds as many landings as it may",
        ));
    }
    if attempts.contains_key(&key) {
        drop(attempts);
        let _removed = std::fs::remove_dir_all(&dir);
        return Err(engine_error("a landing token was reused"));
    }
    let held = Held {
        leased: true,
        landed: None,
        dir: dir.clone(),
        touched: Instant::now(),
        bytes: 0,
    };
    attempts.insert(key.clone(), held);
    drop(attempts);
    let lease = Lease {
        engine,
        tenant: key.0,
        token: key.1,
        dir,
        landed: None,
    };
    super::catalog::private_dir(&lease.dir)?;
    let file = super::catalog::create_new(&lease.dir.join("source"))?;
    Ok((lease, file))
}

/// OLAP-02: take the landed attempt of `request` for the step `phase`, every
/// check made before it is taken; while leased, its landed state is the
/// lease's alone (moved, never cloned).
pub(super) fn lease<'e>(
    engine: &'e Engine,
    request: &LandRequest,
    phase: Phase,
    answer: Option<&LandResponse>,
    through: Option<u64>,
) -> Outcome<(Lease<'e>, Landed)> {
    let tenant = &request.load.tenant;
    let mut attempts = engine.attempts();
    let Some(held) = attempts.get_mut(&(tenant.clone(), request.attempt.clone())) else {
        return Err(refusal(ATTEMPT_UNKNOWN, "the landing attempt is not held"));
    };
    if held.leased {
        return Err(refusal(ATTEMPT_BUSY, "the landing attempt is in use"));
    }
    let matches = held.landed.as_ref().is_some_and(|landed| {
        landed.request == contract(request)
            && Phase::of(&landed.answer) == phase
            && answer.is_none_or(|answer| *answer == landed.answer)
    });
    if !matches {
        return Err(refusal(
            STALE,
            "the landing attempt is another landing's or another step's",
        ));
    }
    if let Some(through) = through
        && held
            .landed
            .as_ref()
            .and_then(|landed| landed.answer.residue.last())
            .map(|row| row.row)
            != Some(through)
    {
        return Err(refusal(
            STALE,
            "the settle does not end at the residue page the engine answered",
        ));
    }
    let landed = held
        .landed
        .take()
        .ok_or_else(|| refusal(STALE, "the landing attempt is not landed"))?;
    held.leased = true;
    let dir = held.dir.clone();
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

/// The attempts an engine holds, by tenant and token.
pub(super) type Attempts = BTreeMap<(TenantId, String), Held>;
