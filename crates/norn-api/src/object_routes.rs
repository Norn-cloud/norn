//! HOST-READ-2: authorized typed object pages and objects (#873).
//!
//! Every read passes the shared HOST-READ-1 gate for the collection, then each
//! row is decided on its own: a record appears only when the principal holds
//! both Population and Values for that exact row. Visibility is applied
//! before data quality, so quarantine counts and cursors describe only rows
//! the principal may see. A cursor always names a visible row, and a page
//! fills from visible rows only: every response is the one the principal would
//! get if the unreadable rows did not exist. The work behind a page is bounded
//! by the collection, which only the server can grow; its latency is the one
//! remaining channel. An absent, an invisible and a quarantined object refuse
//! the same way. No table, column, row locator or SQL crosses this boundary
//! (#733).

use crate::{
    authz::{
        ReadCapability::{self, Population, Values},
        ReadRequest, read_denied,
    },
    host::DevHost,
    objects::{NornObjectPage, NornObjectRecord},
    refusal::Refusal,
    scope::{CellScope, ScopeQuery},
};
use norn_cell::{
    MAIN_WORLD, ObjectRecord, StoreReader,
    engine::{ReadIsolation, SqlEngine},
};

/// Largest page a caller may ask for.
pub const MAX_PAGE: usize = 100;
/// Rows read per store call, whatever the page size: a small page over many
/// unreadable rows costs batches, not one store read per row.
pub const SCAN_BATCH: usize = 256;

/// Server-owned mapping from principal and resource to the kernel PDP input.
/// No client role, grant or scope enters here.
pub trait ObjectAccess: Send + Sync {
    /// The PDP request for `capability` on a collection (`row` is `None`; the
    /// resource id is the object type) or on one row (its actual id and type).
    /// # Errors
    /// An unavailable mapping denies the read.
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal>;

    /// Fault-injection seam for tests: called inside each locked read section
    /// of a page, after its opening head check and before its reads. A host
    /// mapping does nothing here.
    #[doc(hidden)]
    fn within_snapshot(&self) {}
}

fn not_found() -> Refusal {
    Refusal::new(
        http::StatusCode::NOT_FOUND,
        "object.not_found",
        "no such readable object",
        "the object does not exist or is not readable by this principal",
    )
}

fn mapped(
    host_scope: &CellScope,
    access: &dyn ObjectAccess,
    actor: &str,
    capability: ReadCapability,
    object_type: &str,
    row: Option<&ObjectRecord>,
) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
    let request = access
        .access(host_scope, actor, capability, object_type, row)
        .map_err(|_denial| read_denied())?;
    let (expected_type, expected_id) = row.map_or((object_type, object_type), |row| {
        (row.object_type.as_str(), row.id.as_str())
    });
    if request.principal.subject_id != actor
        || request.resource.object_type != expected_type
        || request.resource.id != expected_id
        || row.is_some_and(|row| row.scope != host_scope.key())
    {
        return Err(read_denied());
    }
    Ok(request)
}

fn row_visible<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    row: &ObjectRecord,
) -> bool {
    [Population, Values].into_iter().all(|capability| {
        mapped(
            host.scope(),
            access,
            actor,
            capability,
            &row.object_type,
            Some(row),
        )
        .is_ok_and(|request| {
            host.read_permitted(&ReadRequest {
                capability,
                access: &request,
            })
        })
    })
}

/// The collection gate: scope and Population on the collection, under the epoch.
fn collection<'h, E: SqlEngine>(
    host: &'h DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
) -> Result<(std::sync::RwLockReadGuard<'h, ()>, String), Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    let request = mapped(host.scope(), access, actor, Population, object_type, None)?;
    let epoch = host.read_access(
        scope,
        &ReadRequest {
            capability: Population,
            access: &request,
        },
    )?;
    Ok((epoch, actor.to_owned()))
}

/// One page of a concept's visible, unquarantined objects after `after`.
///
/// # Errors
/// A denied gate (one opaque 403), an undeclared type (404), an oversized
/// page (400), an engine that cannot prove one snapshot (503
/// `object.snapshot_unavailable`), a commit while the page was read (409 `object.snapshot_moved`,
/// `retryable`: the client repeats the read with bounded backoff; the host
/// never retries internally) or a host failure. Unreadable rows never refuse a page: a bound on raw rows would
/// reveal how many exist.
pub fn get_object_page<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    after: Option<&str>,
    limit: usize,
    access: &dyn ObjectAccess,
) -> Result<NornObjectPage, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    if limit == 0 || limit > MAX_PAGE {
        return Err(Refusal::invalid(format!("limit must be 1..={MAX_PAGE}")));
    }
    let mut page = NornObjectPage {
        object_type: object_type.to_owned(),
        records: Vec::new(),
        quarantine_count: 0,
        scanned_through: None,
    };
    // The cell lock is held per batch, never for the whole scan, and row
    // authorization runs without it. Each locked section reads the cell's
    // version before and after its reads, and both must equal the version the
    // page started at. The version is the receipt head together with the
    // engine's database revision, which changes on every write by any
    // connection, receipt-less evidence (mapping conflicts, identity) included.
    // Only an engine that declares `ExclusiveSynchronous` (the Durable Object's,
    // the only connection to its storage, serving each page in one synchronous
    // call) may go without a revision; any other engine without one refuses.
    let mut snapshot = None;
    let mut within = |read: &mut dyn FnMut(&StoreReader<'_, E>) -> Result<(), Refusal>| {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let version = || {
            let revision = match reader.read_isolation() {
                ReadIsolation::Revisioned => Some(
                    reader
                        .database_revision()
                        .map_err(Refusal::host)?
                        .ok_or_else(snapshot_unavailable)?,
                ),
                ReadIsolation::ExclusiveSynchronous => None,
            };
            Ok::<_, Refusal>((reader.head_commit_seq().map_err(Refusal::host)?, revision))
        };
        let start = match snapshot {
            Some(start) => start,
            None => *snapshot.insert(version()?),
        };
        if version()? != start {
            return Err(snapshot_moved());
        }
        access.within_snapshot();
        read(&reader)?;
        if version()? != start {
            return Err(snapshot_moved());
        }
        Ok(())
    };
    let mut cursor = after.map(str::to_owned);
    loop {
        let mut batch = Vec::new();
        within(&mut |reader| {
            if !reader.model().has_type(object_type) {
                return Err(not_found());
            }
            batch = reader
                .list_after(MAIN_WORLD, object_type, cursor.as_deref(), SCAN_BATCH)
                .map_err(Refusal::host)?;
            Ok(())
        })?;
        let exhausted = batch.len() < SCAN_BATCH;
        let visible: Vec<(bool, ObjectRecord)> = batch
            .into_iter()
            .map(|row| (row_visible(host, access, &actor, &row), row))
            .collect();
        let mut quarantined = Vec::with_capacity(visible.len());
        within(&mut |reader| {
            quarantined = visible
                .iter()
                .map(|(seen, row)| {
                    if *seen {
                        reader.observation_is_quarantined(MAIN_WORLD, row)
                    } else {
                        Ok(false)
                    }
                })
                .collect::<Result<Vec<bool>, _>>()
                .map_err(Refusal::host)?;
            Ok(())
        })?;
        for ((seen, row), quarantined) in visible.into_iter().zip(quarantined) {
            cursor = Some(row.id.clone());
            if !seen {
                continue;
            }
            page.scanned_through = Some(row.id.clone());
            if quarantined {
                page.quarantine_count = page.quarantine_count.saturating_add(1);
            } else {
                page.records.push(row.into());
            }
            if page.records.len() == limit {
                return Ok(page);
            }
        }
        if exhausted {
            // Nothing visible remains: no cursor, so none names an unseen row.
            page.scanned_through = None;
            return Ok(page);
        }
    }
}

fn snapshot_unavailable() -> Refusal {
    Refusal::new(
        http::StatusCode::SERVICE_UNAVAILABLE,
        "object.snapshot_unavailable",
        "the cell cannot read a consistent page",
        "this engine offers no database revision, so a page cannot prove it saw one snapshot",
    )
}

fn snapshot_moved() -> Refusal {
    let mut refusal = Refusal::new(
        http::StatusCode::CONFLICT,
        "object.snapshot_moved",
        "the cell changed while the page was read",
        "repeat the read; a page is never assembled from two snapshots",
    );
    refusal.retryable = true;
    refusal
}

/// One visible, unquarantined object.
///
/// # Errors
/// A denied collection gate (one opaque 403); an absent, invisible or
/// quarantined object (one 404); or a host failure.
pub fn get_object<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<NornObjectRecord, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let cell = host.cell()?;
    let reader = cell.store().read();
    if !reader.model().has_type(object_type) {
        return Err(not_found());
    }
    let row = reader
        .object(MAIN_WORLD, object_type, id)
        .map_err(Refusal::host)?
        .ok_or_else(not_found)?;
    if !row_visible(host, access, &actor, &row)
        || reader
            .observation_is_quarantined(MAIN_WORLD, &row)
            .map_err(Refusal::host)?
    {
        return Err(not_found());
    }
    Ok(row.into())
}
