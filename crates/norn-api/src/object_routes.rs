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
    object_query::{self, Cursor, ObjectQueryParams},
    objects::{
        NornAppliedQuery, NornObjectPage, NornObjectQueryPage, NornObjectRecord, NornQueriedRecord,
        NornQueryTotal,
    },
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

pub(crate) fn not_found() -> Refusal {
    Refusal::new(
        http::StatusCode::NOT_FOUND,
        "object.not_found",
        "no such readable object",
        "the object does not exist or is not readable by this principal",
    )
}

pub(crate) fn mapped(
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
pub(crate) fn collection<'h, E: SqlEngine>(
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

pub(crate) fn snapshot_moved() -> Refusal {
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

/// Visible rows one grid query reads at most. A sort or filter needs every
/// row before its first page, so over a larger visible collection it refuses
/// rather than order a part; an unsorted, unfiltered page streams by id and
/// counts approximately. Rows the caller may not see never count toward it.
pub const MAX_QUERY_SCAN: usize = 10_000;

/// One row a grid query lists: Population held; `readable` when Values is too.
struct Listed {
    row: ObjectRecord,
    readable: bool,
}

/// The rows one snapshot holds after `after`, up to the visible bound, with what the
/// caller may see of each.
struct Scan {
    listed: Vec<Listed>,
    quarantined: u64,
    exhausted: bool,
    version: (u64, Option<(u64, u64)>),
}

/// Whether the caller holds `capability` on this exact row.
pub(crate) fn permitted<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    row: &ObjectRecord,
    capability: ReadCapability,
) -> bool {
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
}

/// The cell's version: the receipt head and, when revisioned, the database revision.
pub(crate) fn version_of<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
) -> Result<(u64, Option<(u64, u64)>), Refusal> {
    let revision = match reader.read_isolation() {
        ReadIsolation::Revisioned => Some(
            reader
                .database_revision()
                .map_err(Refusal::host)?
                .ok_or_else(snapshot_unavailable)?,
        ),
        ReadIsolation::ExclusiveSynchronous => None,
    };
    Ok((reader.head_commit_seq().map_err(Refusal::host)?, revision))
}

/// Read one locked section at `start` (the first section fixes it): the
/// version before and after the read must both equal it.
pub(crate) fn locked<E: SqlEngine, T>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    start: &mut Option<(u64, Option<(u64, u64)>)>,
    read: impl FnOnce(&StoreReader<'_, E>) -> Result<T, Refusal>,
) -> Result<T, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    let version = version_of(&reader)?;
    if *start.get_or_insert(version) != version {
        return Err(snapshot_moved());
    }
    access.within_snapshot();
    let value = read(&reader)?;
    if version_of(&reader)? != version {
        return Err(snapshot_moved());
    }
    Ok(value)
}

/// Scan the type's rows after `after` under one snapshot, stopping once more
/// than `max` visible rows are read or `want` rows are listed. Authorization runs
/// outside the cell lock, as for HOST-READ-2 pages. A readable row is held
/// back when quarantined; a withheld row is listed whatever its evidence,
/// which is value-level and not the caller's to see (D67).
fn scan_listed<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    object_type: &str,
    (after, want, max): (Option<&str>, usize, usize),
) -> Result<Scan, Refusal> {
    let mut start = None;
    let mut scan = Scan {
        listed: Vec::new(),
        quarantined: 0,
        exhausted: false,
        version: (0, None),
    };
    let mut cursor = after.map(str::to_owned);
    loop {
        let take = SCAN_BATCH;
        let batch = locked(host, access, &mut start, |reader| {
            if !reader.model().has_type(object_type) {
                return Err(not_found());
            }
            reader
                .list_after(MAIN_WORLD, object_type, cursor.as_deref(), take)
                .map_err(Refusal::host)
        })?;
        let ended = batch.len() < take;
        if let Some(last) = batch.last() {
            cursor = Some(last.id.clone());
        }
        let seen: Vec<Listed> = batch
            .into_iter()
            .filter(|row| permitted(host, access, actor, row, Population))
            .map(|row| Listed {
                readable: permitted(host, access, actor, &row, Values),
                row,
            })
            .collect();
        let held = locked(host, access, &mut start, |reader| {
            seen.iter()
                .map(|listed| {
                    if listed.readable {
                        reader.observation_is_quarantined(MAIN_WORLD, &listed.row)
                    } else {
                        Ok(false)
                    }
                })
                .collect::<Result<Vec<bool>, _>>()
                .map_err(Refusal::host)
        })?;
        for (listed, quarantined) in seen.into_iter().zip(held) {
            if quarantined {
                scan.quarantined = scan.quarantined.saturating_add(1);
            } else {
                scan.listed.push(listed);
            }
        }
        scan.version = start.unwrap_or_default();
        if ended {
            scan.exhausted = true;
            return Ok(scan);
        }
        // The bound counts only rows the caller may see: a bound on raw rows
        // would reveal how many it may not (HOST-READ-2).
        let visible = u64::try_from(scan.listed.len())
            .unwrap_or(u64::MAX)
            .saturating_add(scan.quarantined);
        if visible > u64::try_from(max).unwrap_or(u64::MAX) || scan.listed.len() >= want {
            return Ok(scan);
        }
    }
}

/// The values a query may compare: none for a withheld row.
fn values(listed: &Listed) -> object_query::RowValues<'_> {
    listed.readable.then_some(&listed.row.values)
}

/// A listed row as the grid shows it: values only when readable.
fn queried(listed: &Listed) -> NornQueriedRecord {
    NornQueriedRecord {
        object_type: listed.row.object_type.clone(),
        id: listed.row.id.clone(),
        scope: listed.row.scope.clone(),
        version: listed.readable.then_some(listed.row.version),
        values: listed.readable.then(|| listed.row.values.clone()),
        withheld: !listed.readable,
        withheld_properties: Vec::new(),
    }
}

/// The 413 for a sort or filter over more visible rows than one scan reads.
pub(crate) fn scan_bound_exceeded() -> Refusal {
    Refusal::new(
        http::StatusCode::PAYLOAD_TOO_LARGE,
        "object.scan_bound_exceeded",
        "the collection is too large to sort or filter in one scan",
        "page without sort or filter, which streams by id",
    )
}

/// The cursor `text` decodes to, when it continues this query: the same
/// type, normalized query, scope and caller (`bound`, in that order).
fn continued(text: Option<&str>, bound: [&str; 4]) -> Result<Option<Cursor>, Refusal> {
    text.map(|text| {
        Cursor::decode(text)
            .filter(|cursor| [&*cursor.t, &*cursor.q, &*cursor.s, &*cursor.a] == bound)
            .ok_or_else(object_query::cursor_invalid)
    })
    .transpose()
}

/// HOST-READ-3: one page of a grid query over a concept's objects (D67).
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared type (404), an invalid query
/// (400 with every violation), a cursor that does not continue it (400
/// `query.cursor_invalid`), a commit since the cursor (409
/// `object.snapshot_moved`), a sort or filter over more visible rows than one scan
/// (413), or a host failure.
pub fn get_object_query<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    params: &ObjectQueryParams,
    access: &dyn ObjectAccess,
) -> Result<NornObjectQueryPage, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let properties = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .model()
            .effective_property_types(object_type)
            .map_err(Refusal::host)?
    };
    let query = object_query::parse(params, &properties)?;
    let digest = query.digest(object_type);
    let cursor = continued(
        query.after.as_deref(),
        [
            object_type,
            &digest,
            &host.scope().key(),
            &Cursor::actor_digest(&actor),
        ],
    )?;
    let full = query.needs_full_scan();
    // A sort or filter reads the whole collection; a plain page streams from
    // the cursor's id and, past the first page, stops one row past the page.
    let (from, want) = match (&cursor, full) {
        (Some(cursor), false) => (Some(cursor.id.as_str()), query.limit.saturating_add(1)),
        _ => (None, usize::MAX),
    };
    let scan = scan_listed(
        host,
        access,
        &actor,
        object_type,
        (from, want, MAX_QUERY_SCAN),
    )?;
    if let Some(cursor) = &cursor
        && (cursor.c, cursor.r) != scan.version
    {
        return Err(snapshot_moved());
    }
    if full && !scan.exhausted {
        return Err(scan_bound_exceeded());
    }
    let mut matched: Vec<&Listed> = scan
        .listed
        .iter()
        .filter(|listed| object_query::matches(&query.filter, values(listed)))
        .collect();
    if !query.sort.is_empty() {
        matched.sort_by(|a, b| {
            object_query::compare(&query.sort, (&a.row.id, values(a)), (&b.row.id, values(b)))
        });
    }
    let begin = match (&cursor, full) {
        (Some(cursor), true) => matched
            .iter()
            .position(|listed| listed.row.id == cursor.id)
            .map(|at| at.saturating_add(1))
            .ok_or_else(object_query::cursor_invalid)?,
        _ => 0,
    };
    let page: Vec<&Listed> = matched
        .iter()
        .skip(begin)
        .take(query.limit)
        .copied()
        .collect();
    let more = begin.saturating_add(page.len()) < matched.len() || !scan.exhausted;
    let next = match page.last() {
        Some(last) if more => Some(
            Cursor {
                v: 1,
                t: object_type.to_owned(),
                q: digest,
                s: host.scope().key(),
                a: Cursor::actor_digest(&actor),
                c: scan.version.0,
                r: scan.version.1,
                id: last.row.id.clone(),
            }
            .encode(),
        ),
        _ => None,
    };
    let total = (cursor.is_none() || full).then(|| NornQueryTotal {
        count: u64::try_from(matched.len()).unwrap_or(u64::MAX),
        approximate: !scan.exhausted,
    });
    Ok(NornObjectQueryPage {
        object_type: object_type.to_owned(),
        records: page.into_iter().map(queried).collect(),
        quarantine_count: scan.quarantined,
        query: NornAppliedQuery {
            sort: query.sort_document(),
            filter: query.filter_document(),
        },
        next,
        total,
    })
}

/// HOST-READ-3: one object as a grid shows it (D67): withheld under
/// Population alone; absent, invisible and quarantined objects refuse alike.
///
/// # Errors
/// As [`get_object`].
pub fn get_queried_object<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<NornQueriedRecord, Refusal> {
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
    if !permitted(host, access, &actor, &row, Population) {
        return Err(not_found());
    }
    let readable = permitted(host, access, &actor, &row, Values);
    if readable
        && reader
            .observation_is_quarantined(MAIN_WORLD, &row)
            .map_err(Refusal::host)?
    {
        return Err(not_found());
    }
    Ok(queried(&Listed { row, readable }))
}
