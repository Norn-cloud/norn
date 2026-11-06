//! HOST-READ-4: an object's link neighbourhood and its source lineage (D67).
//!
//! A link is existence-level: it names two rows and nothing of their values.
//! It is listed only when the caller holds Population on both endpoints (and
//! on each endpoint's collection); a link to a row the caller cannot see is
//! skipped and never counted, so a page and its `next` are the ones the caller
//! would get if those rows did not exist. The cursor carries only identities
//! the caller already saw (the last listed link's endpoints), bound to the
//! object, the query, the scope, the caller and the snapshot; the cell's
//! physical keyset position never leaves the server.
//!
//! Lineage is value-level: it needs Values and Sources on the row. A withheld,
//! source-withheld, quarantined or absent object, and one without a retained
//! source bundle, all refuse with the same opaque 404.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use norn_cell::read::{LinkDirection, LinkEdgeRecord, LinkPagePosition, LinkRef};
use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::source_provenance::InstanceSourceProvenance;
use norn_cell::{MAIN_WORLD, ObjectRecord, engine::SqlEngine};
use serde::{Deserialize, Serialize};

use crate::authz::ReadCapability::{Population, Sources, Values};
use crate::authz::ReadRequest;
use crate::host::DevHost;
use crate::object_query::{self, CURSOR_KEY, Cursor};
use crate::object_routes::{
    MAX_QUERY_SCAN, ObjectAccess, SCAN_BATCH, collection, locked, mapped, not_found, permitted,
    scan_bound_exceeded, snapshot_moved,
};
use crate::objects::{NornLinkEdge, NornObjectLinkPage};
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// Default links per page.
pub const DEFAULT_LINK_LIMIT: usize = 50;
/// Largest links page a caller may ask for.
pub const MAX_LINK_LIMIT: usize = 100;

/// The raw query string fields of `GET /v1/objects/{type}/{id}/links`.
#[derive(Debug, Default, Clone)]
pub struct ObjectLinkParams {
    /// `out`, `in` or `both` (the default).
    pub direction: Option<String>,
    /// One declared link id, to list only those links.
    pub link: Option<String>,
    /// `limit`, 1..=100.
    pub limit: Option<String>,
    /// `after`, an opaque cursor from a prior page's `next`.
    pub after: Option<String>,
}

/// A validated links query.
struct LinkQuery {
    direction: LinkDirection,
    direction_name: &'static str,
    link: Option<String>,
    limit: usize,
}

/// One link candidate with its source and target rows, when they exist.
type Candidate = (LinkEdgeRecord, Option<ObjectRecord>, Option<ObjectRecord>);

/// One link's identity: declaring type, link id, source, target type, target.
type Identity = (String, String, String, String, String);

fn identity(edge: &LinkRef) -> Identity {
    (
        edge.source_type.clone(),
        edge.link.clone(),
        edge.source.clone(),
        edge.target_type.clone(),
        edge.target.clone(),
    )
}

/// What a links cursor binds: identities the caller saw, and no value.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct LinkCursor {
    v: u8,
    t: String,
    i: String,
    q: String,
    s: String,
    a: String,
    c: u64,
    r: Option<(u64, u64)>,
    e: Identity,
}

impl LinkCursor {
    /// The opaque wire form: base64url of the cursor's JSON.
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    /// The cursor `text` encodes, if it is a version-1 links cursor.
    fn decode(text: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
        serde_json::from_slice::<Self>(&bytes)
            .ok()
            .filter(|cursor| cursor.v == 1)
    }
}

/// Every link id the cell's model declares.
fn declared_links<E: SqlEngine>(host: &DevHost<E>) -> Result<BTreeSet<String>, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    Ok(reader
        .model()
        .object_types()
        .flat_map(|object| object.links.iter().map(|link| link.id.0.clone()))
        .collect())
}

/// Parse and validate `params`; every violation is reported at its pointer.
fn parse_links(params: &ObjectLinkParams, links: &BTreeSet<String>) -> Result<LinkQuery, Refusal> {
    let mut violations = Vec::new();
    let (direction, direction_name) = match params.direction.as_deref() {
        None | Some("both") => (LinkDirection::Both, "both"),
        Some("out") => (LinkDirection::Out, "out"),
        Some("in") => (LinkDirection::In, "in"),
        Some(_) => {
            violations.push((
                "query.value_invalid",
                "direction is out, in or both".to_owned(),
                "/direction".to_owned(),
            ));
            (LinkDirection::Both, "both")
        }
    };
    if let Some(link) = params.link.as_deref()
        && !links.contains(link)
    {
        violations.push((
            "query.property_unknown",
            format!("{link} is not a declared link"),
            "/link".to_owned(),
        ));
    }
    let limit = match params.limit.as_deref() {
        None => DEFAULT_LINK_LIMIT,
        Some(text) => match text.parse::<usize>() {
            Ok(limit)
                if (1..=MAX_LINK_LIMIT).contains(&limit)
                    && text.bytes().all(|b| b.is_ascii_digit()) =>
            {
                limit
            }
            _ => {
                violations.push((
                    "query.limit_invalid",
                    format!("limit must be 1..={MAX_LINK_LIMIT}"),
                    "/limit".to_owned(),
                ));
                DEFAULT_LINK_LIMIT
            }
        },
    };
    if violations.is_empty() {
        Ok(LinkQuery {
            direction,
            direction_name,
            link: params.link.clone(),
            limit,
        })
    } else {
        Err(object_query::refused(violations))
    }
}

/// Whether the caller may see `row` as a link endpoint: Population on the row
/// and on its collection. Quarantine is checked separately, under the lock.
fn endpoint_listed<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    row: &ObjectRecord,
    collections: &mut BTreeMap<String, bool>,
) -> bool {
    let collection = *collections
        .entry(row.object_type.clone())
        .or_insert_with(|| {
            mapped(host.scope(), access, actor, Population, &row.object_type, None).is_ok_and(
                |request| {
                    host.read_permitted(&ReadRequest {
                        capability: Population,
                        access: &request,
                    })
                },
            )
        });
    collection && permitted(host, access, actor, row, Population)
}

/// The state of one links scan under one snapshot.
struct LinkScan<'q> {
    query: &'q LinkQuery,
    resume: Option<&'q Identity>,
    start: Option<(u64, Option<(u64, u64)>)>,
    collections: BTreeMap<String, bool>,
    listed: Vec<LinkEdgeRecord>,
}

/// The links of one candidate batch the caller may see, in cell order.
fn visible_batch<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    scan: &mut LinkScan<'_>,
    batch: Vec<Candidate>,
) -> Result<Vec<LinkEdgeRecord>, Refusal> {
    let mut seen = Vec::new();
    for (edge, source, target) in batch {
        let (Some(source), Some(target)) = (source, target) else {
            continue;
        };
        if endpoint_listed(host, access, actor, &source, &mut scan.collections)
            && endpoint_listed(host, access, actor, &target, &mut scan.collections)
        {
            let readable = [&source, &target]
                .map(|row| permitted(host, access, actor, row, Values));
            seen.push((edge, [source, target], readable));
        }
    }
    let held = locked(host, access, &mut scan.start, |reader| {
        seen.iter()
            .map(|(_, rows, readable)| {
                rows.iter().zip(readable).try_fold(false, |held, (row, readable)| {
                    Ok::<bool, norn_cell::CellError>(
                        held || (*readable && reader.observation_is_quarantined(MAIN_WORLD, row)?),
                    )
                })
            })
            .collect::<Result<Vec<bool>, norn_cell::CellError>>()
            .map_err(Refusal::host)
    })?;
    Ok(seen
        .into_iter()
        .zip(held)
        .filter_map(|((edge, _, _), held)| (!held).then_some(edge))
        .collect())
}

/// Read the visible links touching `(object_type, id)` after the cursor's
/// link, until one past the page fills or the neighbourhood ends. The bound
/// counts only visible links, so hidden ones never refuse a page.
fn scan_links<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    (object_type, id): (&str, &str),
    scan: &mut LinkScan<'_>,
) -> Result<(), Refusal> {
    let mut position: Option<LinkPagePosition> = None;
    let mut resumed = scan.resume.is_none();
    let mut visible: usize = 0;
    loop {
        let (page, batch) = locked(host, access, &mut scan.start, |reader| {
            let page = reader
                .links_touching_page(
                    MAIN_WORLD,
                    object_type,
                    id,
                    scan.query.direction,
                    scan.query.link.as_deref(),
                    position.as_ref(),
                    SCAN_BATCH,
                )
                .map_err(Refusal::host)?;
            let mut batch = Vec::with_capacity(page.items.len());
            for item in &page.items {
                let edge = &item.edge;
                let source = reader
                    .object(MAIN_WORLD, &edge.source_type, &edge.source)
                    .map_err(Refusal::host)?;
                let target = reader
                    .object(MAIN_WORLD, &edge.target_type, &edge.target)
                    .map_err(Refusal::host)?;
                batch.push((item.clone(), source, target));
            }
            Ok((page.next_after, batch))
        })?;
        for edge in visible_batch(host, access, actor, scan, batch)? {
            visible = visible.saturating_add(1);
            if visible > MAX_QUERY_SCAN {
                return Err(scan_bound_exceeded());
            }
            if !resumed {
                resumed = scan.resume == Some(&identity(&edge.edge));
                continue;
            }
            scan.listed.push(edge);
            if scan.listed.len() > scan.query.limit {
                return Ok(());
            }
        }
        position = page;
        if position.is_none() {
            return if resumed {
                Ok(())
            } else {
                Err(object_query::cursor_invalid())
            };
        }
    }
}

/// HOST-READ-4: one page of the links touching an object (D67).
///
/// # Errors
/// A denied gate (the opaque 403); an absent, invisible or quarantined object
/// (404); an invalid query (400 with every violation); a cursor that does not
/// continue it (400 `query.cursor_invalid`); a commit since the cursor (409
/// `object.snapshot_moved`); more visible links than one scan reads (413);
/// or a host failure.
pub fn get_object_links<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    params: &ObjectLinkParams,
    access: &dyn ObjectAccess,
) -> Result<NornObjectLinkPage, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let query = parse_links(params, &declared_links(host)?)?;
    let digest = norn_cell::compute::lineage::keyed_lineage_handle(
        CURSOR_KEY,
        &serde_json::json!({
            "links": object_type,
            "id": id,
            "direction": query.direction_name,
            "link": query.link,
        })
        .to_string(),
    );
    let bound = |cursor: &LinkCursor| {
        [&*cursor.t, &*cursor.i, &*cursor.q, &*cursor.s, &*cursor.a]
            == [
                object_type,
                id,
                digest.as_str(),
                host.scope().key().as_str(),
                Cursor::actor_digest(&actor).as_str(),
            ]
    };
    let cursor = params
        .after
        .as_deref()
        .map(|text| {
            LinkCursor::decode(text)
                .filter(bound)
                .ok_or_else(object_query::cursor_invalid)
        })
        .transpose()?;
    let mut scan = LinkScan {
        query: &query,
        resume: cursor.as_ref().map(|cursor| &cursor.e),
        start: None,
        collections: BTreeMap::new(),
        listed: Vec::new(),
    };
    requested_row(host, access, &actor, (object_type, id), &mut scan.start)?;
    if let Some(cursor) = &cursor
        && (cursor.c, cursor.r) != scan.start.unwrap_or_default()
    {
        return Err(snapshot_moved());
    }
    scan_links(host, access, &actor, (object_type, id), &mut scan)?;
    let more = scan.listed.len() > query.limit;
    scan.listed.truncate(query.limit);
    let version = scan.start.unwrap_or_default();
    let next = scan.listed.last().filter(|_| more).map(|last| {
        LinkCursor {
            v: 1,
            t: object_type.to_owned(),
            i: id.to_owned(),
            q: digest,
            s: host.scope().key(),
            a: Cursor::actor_digest(&actor),
            c: version.0,
            r: version.1,
            e: identity(&last.edge),
        }
        .encode()
    });
    Ok(NornObjectLinkPage {
        object_type: object_type.to_owned(),
        id: id.to_owned(),
        items: scan.listed.into_iter().map(NornLinkEdge::from).collect(),
        next,
    })
}

/// The requested object, when the caller may see it: Population on the row,
/// and not quarantined when its values are readable. Opens the snapshot.
fn requested_row<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    (object_type, id): (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<(), Refusal> {
    let row = locked(host, access, start, |reader| {
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .object(MAIN_WORLD, object_type, id)
            .map_err(Refusal::host)
    })?
    .ok_or_else(not_found)?;
    if !permitted(host, access, actor, &row, Population) {
        return Err(not_found());
    }
    if permitted(host, access, actor, &row, Values)
        && locked(host, access, start, |reader| {
            reader
                .observation_is_quarantined(MAIN_WORLD, &row)
                .map_err(Refusal::host)
        })?
    {
        return Err(not_found());
    }
    Ok(())
}

/// HOST-READ-4: where an object's current version came from (D67).
///
/// # Errors
/// A denied gate (the opaque 403); an absent, quarantined, withheld or
/// source-withheld object, or one without a retained source bundle (one 404);
/// or a host failure.
pub fn get_object_lineage<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<InstanceSourceProvenance, Refusal> {
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
    let disclosed = [Population, Values, Sources]
        .into_iter()
        .all(|capability| permitted(host, access, &actor, &row, capability));
    if !disclosed
        || reader
            .observation_is_quarantined(MAIN_WORLD, &row)
            .map_err(Refusal::host)?
    {
        return Err(not_found());
    }
    let mut budget = DecodeBudget::new(&ReactiveLimits::from_profile(
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    ));
    let fact = norn_ir::FactRef::new(row.object_type.as_str(), id, row.version);
    reader
        .instance_source_provenance(MAIN_WORLD, &fact, &mut budget, |_| Ok(true))
        .map_err(Refusal::host)?
        .ok_or_else(not_found)
}
