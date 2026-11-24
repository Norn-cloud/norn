//! Unified link-slot identity, point SQL lookup, IFC read-marking checks, and
//! action-authorized predicate reads for `norn-cell` (#1586).

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::{
    CellCommit, CommitError, FactVersion, LinkKey, LinkState, PredicateActionRead, PredicateObject,
    PredicateQuery,
};
use norn_projections::ValueEncoding;
use norn_projections::sqlite_ddl::{
    OVERFLOW_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, WORLD_COLUMN, quote_identifier,
};

use crate::MAIN_WORLD;
use crate::codec::{DecodeContext, ValueOwner};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{LinkRecord, StoreReader, decode_row, query_projected};
use crate::store::LinkChange;
use crate::system::LINK_CREATION;

/// The committed state of link `key` as the kernel sees it (O(1) point lookup).
pub(crate) fn link_state<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    key: &LinkKey,
) -> Result<LinkState, CellError> {
    let source_type = key.source.object_type.as_str();
    let version = reader.link_version(
        MAIN_WORLD,
        source_type,
        &key.link,
        &key.source.key,
        key.target.object_type.as_str(),
        &key.target.key,
    )?;
    let values = point_link(
        reader,
        MAIN_WORLD,
        source_type,
        &key.link,
        &key.source.key,
        Some(key.target.object_type.as_str()),
        &key.target.key,
    )?
    .map(|record| {
        record
            .values
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .collect()
    });
    Ok(LinkState {
        version: FactVersion(version),
        values,
    })
}

/// Record or clear original creation evidence for one link row.
pub(crate) fn record_link_creation<E: SqlEngine>(
    conn: &E,
    world: &str,
    table: &str,
    source: &str,
    target: &str,
    creation: Option<(Option<i64>, Option<i64>)>,
) -> Result<(), CellError> {
    conn.execute(
        &format!(
            "DELETE FROM \"{LINK_CREATION}\" WHERE \"world\" = ?1 AND \"link_table\" = ?2 AND \"source\" = ?3 AND \"target\" = ?4"
        ),
        params![world, table, source, target],
    )?;
    if let Some((commit, version)) = creation {
        conn.execute(
            &format!(
                "INSERT INTO \"{LINK_CREATION}\" (\"world\", \"link_table\", \"source\", \"target\", \"created_commit\", \"creation_version\") VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![world, table, source, target, commit, version],
        )?;
    }
    Ok(())
}

/// O(1) SQL primary-key point lookup for one link `(world, source, target)`.
///
/// When `target_type` is supplied, verifies the retained `_norn_link_version`
/// target hierarchy matches `canonical_type(target_type)` before returning any
/// stored row, so distinct interface-target subtypes sharing a raw key never
/// observe each other's edge payload.
pub(crate) fn point_link<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    object_type: &str,
    link: &str,
    source: &str,
    target_type: Option<&str>,
    target: &str,
) -> Result<Option<LinkRecord>, CellError> {
    let (_, table) = reader.model().link(object_type, link)?;
    if let Some(expected_target_type) = target_type
        && let Some((_, written_target_type)) =
            reader.link_version_row(world, &table.table, source, target)?
        && reader.canonical_type(&written_target_type)
            != reader.canonical_type(expected_target_type)
    {
        return Ok(None);
    }
    let mut columns = vec![(TARGET_COLUMN, false)];
    columns.extend(table.columns.iter().filter_map(|column| {
        column
            .column
            .as_deref()
            .map(|name| (name, column.encoding == ValueEncoding::Integer))
    }));
    if table.has_overflow {
        columns.push((OVERFLOW_COLUMN, false));
    }
    let suffix = format!(
        "FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3",
        quote_identifier(&table.table),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SOURCE_COLUMN),
        quote_identifier(TARGET_COLUMN),
    );
    let rows = query_projected(
        reader.conn,
        &table.table,
        &columns,
        &suffix,
        params![world, source, target],
    )?;
    let Some(values) = rows.into_iter().next() else {
        return Ok(None);
    };
    let source_type = reader
        .root_row(world, object_type, source)?
        .map_or_else(|| object_type.to_owned(), |(concrete, _, _)| concrete);
    let owner = ValueOwner::Link {
        source_type: &source_type,
        link,
    };
    let row = SqlRow(values);
    let stored_target: String = row.get(0)?;
    let decoded = decode_row(
        DecodeContext {
            model: reader.model(),
            owner,
        },
        &table.table,
        &table.columns,
        table.has_overflow,
        &row,
        1,
    )?;
    Ok(Some(LinkRecord {
        source: source.to_owned(),
        target: stored_target,
        values: decoded,
    }))
}

/// Enforce physical-slot exclusivity across interface-target subtypes sharing a
/// raw target key so CAS, uniqueness, authorization, and physical storage agree
/// on one canonical `(source_root, source_key, link, target_root, target_key)`
/// edge identity.
pub(crate) fn check_commit_link_slots<E, C, I, S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    commit: &CellCommit<E, C, I>,
) -> Result<(), CommitError<CellError>> {
    if commit.links.is_empty() {
        return Ok(());
    }
    let mut seen: BTreeMap<(String, String, String), String> = BTreeMap::new();
    for write in &commit.links {
        let source_type = write.key.source.object_type.as_str();
        let Ok((_, table)) = reader.model().link(source_type, &write.key.link) else {
            // Undeclared links are refused by `check_commit` as `LinkTarget`.
            continue;
        };
        let Ok(source_root) = reader.root_type(source_type) else {
            continue;
        };
        let target_root = reader.canonical_type(write.key.target.object_type.as_str());
        let slot = (
            table.table.clone(),
            write.key.source.key.clone(),
            write.key.target.key.clone(),
        );
        if let Some(previous_root) = seen.insert(slot, target_root.clone())
            && previous_root != target_root
        {
            return Err(CommitError::DuplicateLink {
                key: Box::new(write.key.clone()),
            });
        }
        for candidate in &reader.model().schema().link_tables {
            if candidate.link.as_str() != write.key.link {
                continue;
            }
            if reader.root_type(candidate.source.as_str()).ok() != Some(source_root) {
                continue;
            }
            if let Some((_, written_target_type)) = reader
                .link_version_row(
                    MAIN_WORLD,
                    &candidate.table,
                    &write.key.source.key,
                    &write.key.target.key,
                )
                .map_err(CommitError::Host)?
                && reader.canonical_type(&written_target_type) != target_root
            {
                return Err(CommitError::EffectiveValues {
                    key: write.key.source.clone(),
                    reason: format!(
                        "link `{}` target key `{}` is already bound to `{written_target_type}`, not `{}`",
                        write.key.link,
                        write.key.target.key,
                        write.key.target.object_type.as_str(),
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Defense-in-depth inside `Writer::apply_links`: a `Remove` must never delete
/// a physical link row or overwrite `_norn_link_version` belonging to a
/// different canonical target hierarchy.
pub(crate) fn check_remove_slot_match<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    table_name: &str,
    source: &str,
    target: &str,
    target_type: &str,
) -> Result<(), CellError> {
    if let Some((_, written_target_type)) =
        reader.link_version_row(world, table_name, source, target)?
        && reader.canonical_type(&written_target_type) != reader.canonical_type(target_type)
    {
        return Err(CellError::ComputeRefused(
            "link removal target type does not match stored edge",
        ));
    }
    Ok(())
}

/// Fail-closed IFC check for link mutations emitted under classified read
/// inputs: because edge membership in `object_links` is disclosed to callers
/// with `Population` access on both endpoints without per-link classification
/// gating, any link mutation (`Add` or `Remove`) emitted under non-empty read
/// classification markings (including `norn.unknown-lineage`) is refused.
pub(crate) fn check_link_read_markings(
    links: &[LinkChange],
    read_markings: &BTreeSet<String>,
) -> Result<(), CellError> {
    let Some(first) = links.first() else {
        return Ok(());
    };
    if read_markings.is_empty() {
        return Ok(());
    }
    if read_markings.contains("norn.unknown-lineage") {
        return Err(CellError::MarkingDeclassificationInvalid(
            "link mutation refused: decision read inputs have unknown classification lineage"
                .to_owned(),
        ));
    }
    let link_name = match first {
        LinkChange::Add { link, .. } | LinkChange::Remove { link, .. } => link.as_str(),
    };
    Err(CellError::MarkingDeclassificationInvalid(format!(
        "link `{link_name}` cannot materialize under classified read inputs without per-link classification disclosure gating"
    )))
}

/// Run an action-authorized predicate read so every matched row passes
/// `authorize` BEFORE host-side source-identity dispute checks can surface.
pub(crate) fn read_predicate_for_action<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    predicate: &PredicateQuery,
    max_members: usize,
    max_bytes: u64,
    authorize: &mut dyn FnMut(&PredicateObject) -> bool,
    load: impl FnOnce(
        &StoreReader<'_, S>,
        &PredicateQuery,
        usize,
        u64,
    ) -> Result<Option<Vec<PredicateObject>>, CellError>,
) -> Result<PredicateActionRead, CellError> {
    let Some(objects) = load(reader, predicate, max_members.saturating_add(1), max_bytes)? else {
        return Ok(PredicateActionRead::Concealed);
    };
    for object in &objects {
        if !authorize(object) {
            return Ok(PredicateActionRead::Concealed);
        }
    }
    for object in &objects {
        reader.refuse_disputed_subject(&object.key)?;
    }
    Ok(PredicateActionRead::Admitted(objects))
}
