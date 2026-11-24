//! Unified link-slot identity, point SQL lookup, bounded cardinality proofs,
//! IFC read-marking checks, and action-authorized predicate reads for
//! `norn-cell` (#1586).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::ObjectTypeName;
use norn_kernel::{
    CellCommit, CommitError, FactExpectation, FactKey, FactVersion, LinkKey, LinkMutation,
    LinkState, PredicateActionRead, PredicateObject, PredicateQuery, PredicateReadExpectation,
};
use norn_projections::ValueEncoding;
use norn_projections::sqlite_ddl::{
    OVERFLOW_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, WORLD_COLUMN, quote_identifier,
};

use crate::MAIN_WORLD;
use crate::codec::{DecodeContext, ValueOwner};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::reactive::{DecodeBudget, ReactiveLimits};
use crate::read::{LinkRecord, StoreReader, decode_row, query_projected};
use crate::store::LinkChange;
use crate::system::{LINK_CREATION, LINK_VERSIONS};

/// Unforgeable proof constructed ONLY by [`verify_mapping_links`] inside a
/// commit transaction after verifying every mapping bundle, exact pending
/// page/bundle consumption, and exact `mapping_link_plan` equality.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VerifiedMappingProof(());

impl VerifiedMappingProof {
    /// Consumes the in-transaction proof to confirm static mapping link exemption.
    pub(crate) const fn exempt_link_read_markings(self) -> bool {
        let () = self.0;
        true
    }
}

/// Admission mode for link write guards inside `Writer::apply_links`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LinkGuardMode {
    /// Governed `CellHost` live commit (`commit_guarded_submitted`): enforce both
    /// IFC read-marking and physical-slot target-identity guards.
    Host,
    /// Governed `CellHost` mapping commit whose in-transaction guard verified
    /// non-empty mapping bundles and exact `mapping_link_plan` equality: exempt
    /// static mapping links from `check_link_read_markings`, still enforce
    /// physical-slot target-identity guards.
    VerifiedMapping(VerifiedMappingProof),
    /// Direct low-level `CellStore` commit (non-`CellHost`): enforces IFC
    /// read-marking checks while permitting interface target-type replacement
    /// on the physical slot.
    UncheckedStore,
    /// Verified history replay (`restore`): skip live-admission guards.
    RetainedReplay,
}

impl LinkGuardMode {
    /// Select the link guard mode for a commit transaction.
    pub(crate) const fn from_origin(
        is_retained: bool,
        is_host: bool,
        mapping_proof: Option<VerifiedMappingProof>,
    ) -> Self {
        if is_retained {
            Self::RetainedReplay
        } else if !is_host {
            Self::UncheckedStore
        } else if let Some(proof) = mapping_proof {
            Self::VerifiedMapping(proof)
        } else {
            Self::Host
        }
    }

    /// Whether `check_link_read_markings` must run for this commit.
    pub(crate) const fn check_read_markings(self) -> bool {
        match self {
            Self::Host | Self::UncheckedStore => true,
            Self::VerifiedMapping(proof) => !proof.exempt_link_read_markings(),
            Self::RetainedReplay => false,
        }
    }

    /// Whether `check_slot_target_match` must run for this commit.
    pub(crate) const fn check_slot_target(self) -> bool {
        matches!(self, Self::Host | Self::VerifiedMapping(_))
    }
}

/// Verify mapping bundle evidence, exact pending page/bundle consumption, and
/// `mapping_link_plan` equality inside the commit transaction. This is the
/// ONLY constructor of [`VerifiedMappingProof`].
pub(crate) fn verify_mapping_links<E, C, I, S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    commit: &CellCommit<E, C, I>,
    mapped: &[crate::mapping_bundle::MappingBundle],
) -> Result<Option<VerifiedMappingProof>, CommitError<CellError>> {
    // OBS-EVIDENCE-1: preparation cannot authorize a bundle disputed before BEGIN.
    for bundle in mapped {
        reader
            .check_mapping_bundle(bundle)
            .map_err(CommitError::Host)?;
    }
    if let Some(norn_kernel::Consumed::Observation {
        id: norn_kernel::ObservationId(observation_id),
    }) = &commit.consumes
    {
        // OBS-PAGE-1: a page commit applies exactly its still-pending
        // members, in page order; a bundle commit exactly its bundle.
        let applied: Vec<String> = mapped
            .iter()
            .map(|b| format!("mapping-bundle:{}", b.digest))
            .collect();
        let expected = if observation_id.starts_with(crate::mapping_page::PAGE_PREFIX) {
            reader
                .pending_page_member_ids(observation_id)
                .map_err(CommitError::Host)?
                .ok_or(CommitError::Host(CellError::SourceEvidenceChanged))?
        } else if mapped.is_empty() {
            Vec::new()
        } else {
            vec![observation_id.clone()]
        };
        if applied != expected {
            return Err(CommitError::Host(CellError::SourceEvidenceChanged));
        }
    } else if !mapped.is_empty() {
        return Err(CommitError::Host(CellError::MappingBundleMismatch));
    }
    if mapped.is_empty() {
        return Ok(None);
    }
    if reader
        .mapping_link_plan(mapped, commit)
        .map_err(CommitError::Host)?
        .links
        != commit.links
    {
        return Err(CommitError::Host(CellError::SourceEvidenceChanged));
    }
    Ok(Some(VerifiedMappingProof(())))
}

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

fn live_link_row_exists<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    table_name: &str,
    source: &str,
    target: &str,
) -> Result<bool, CellError> {
    Ok(reader
        .conn
        .query_optional(
            &format!(
                "SELECT 1 FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3",
                quote_identifier(table_name),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(SOURCE_COLUMN),
                quote_identifier(TARGET_COLUMN),
            ),
            params![world, source, target],
            |_| Ok(()),
        )?
        .is_some())
}

/// Verify whether a legacy live physical link row lacking `_norn_link_version`
/// typed metadata unambiguously belongs to `expected_target_type`.
///
/// For object-type targets, the table only holds that hierarchy. For interface
/// targets, inspects live concrete roots implementing the interface at `target`
/// and fails closed (`false`) if zero or multiple unrelated roots exist or the
/// sole live root differs from `expected_target_type`.
fn legacy_row_matches_target_type<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    declared_target: &str,
    target: &str,
    expected_target_type: &str,
) -> Result<bool, CellError> {
    let expected_root = reader.canonical_type(expected_target_type);
    if reader.model().chain(declared_target).is_ok() {
        return Ok(reader.canonical_type(declared_target) == expected_root);
    }
    let mut candidate_roots = BTreeSet::new();
    for def in reader.model().object_types() {
        if reader.model().is_a(def.name.as_str(), declared_target) {
            candidate_roots.insert(reader.canonical_type(def.name.as_str()));
        }
    }
    let mut live_roots = BTreeSet::new();
    for root in candidate_roots {
        if let Some((concrete, _, _)) = reader.root_row(world, &root, target)?
            && reader.model().is_a(&concrete, declared_target)
        {
            live_roots.insert(root);
        }
    }
    Ok(live_roots.len() == 1 && live_roots.contains(&expected_root))
}

/// O(1) SQL primary-key point lookup for one link `(world, source, target)`.
///
/// When `target_type` is supplied, verifies `_norn_link_version.target_type`
/// (or unambiguous concrete target type for legacy rows without version
/// metadata) matches `canonical_type(target_type)` before returning any row.
pub(crate) fn point_link<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    object_type: &str,
    link: &str,
    source: &str,
    target_type: Option<&str>,
    target: &str,
) -> Result<Option<LinkRecord>, CellError> {
    let (declaration, table) = reader.model().link(object_type, link)?;
    let version_row = if target_type.is_some() {
        reader.link_version_row(world, &table.table, source, target)?
    } else {
        None
    };
    if let (Some(expected_target_type), Some((_, written_target_type))) =
        (target_type, version_row.as_ref())
        && reader.canonical_type(written_target_type) != reader.canonical_type(expected_target_type)
    {
        return Ok(None);
    }
    let mut columns = vec![(SOURCE_COLUMN, false), (TARGET_COLUMN, false)];
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
    if let Some(expected_target_type) = target_type
        && version_row.is_none()
        && !legacy_row_matches_target_type(
            reader,
            world,
            declaration.target.as_str(),
            target,
            expected_target_type,
        )?
    {
        return Ok(None);
    }
    let source_type = reader
        .root_row(world, object_type, source)?
        .map_or_else(|| object_type.to_owned(), |(concrete, _, _)| concrete);
    let owner = ValueOwner::Link {
        source_type: &source_type,
        link,
    };
    let row = SqlRow(values);
    let stored_target: String = row.get(1)?;
    let decoded = decode_row(
        DecodeContext {
            model: reader.model(),
            owner,
        },
        &table.table,
        &table.columns,
        table.has_overflow,
        &row,
        2,
    )?;
    Ok(Some(LinkRecord {
        source: source.to_owned(),
        target: stored_target,
        values: decoded,
    }))
}

/// Enforce physical-slot exclusivity within each physical link table across
/// both live rows and tombstones (never across sibling source tables, and
/// never allowing a different target root to overwrite a tombstoned version
/// slot and replay version 1), plus O(1) table-existence probes for added
/// endpoints (#1527 S2).
pub(crate) fn check_commit_link_slots<E, C, I, S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    commit: &CellCommit<E, C, I>,
) -> Result<(), CommitError<CellError>> {
    if commit.links.is_empty() {
        return Ok(());
    }
    let mut seen: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let mut add_endpoints: BTreeSet<&str> = BTreeSet::new();
    for write in &commit.links {
        if matches!(write.mutation, LinkMutation::Add { .. }) {
            add_endpoints.insert(write.key.source.key.as_str());
            add_endpoints.insert(write.key.target.key.as_str());
        }
        let source_type = write.key.source.object_type.as_str();
        let Ok((declaration, table)) = reader.model().link(source_type, &write.key.link) else {
            // Undeclared links are refused by `check_commit` as `LinkTarget`.
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
        match reader
            .link_version_row(
                MAIN_WORLD,
                &table.table,
                &write.key.source.key,
                &write.key.target.key,
            )
            .map_err(CommitError::Host)?
        {
            Some((_, written_target_type)) => {
                if reader.canonical_type(&written_target_type) != target_root {
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
            None => {
                if live_link_row_exists(
                    reader,
                    MAIN_WORLD,
                    &table.table,
                    &write.key.source.key,
                    &write.key.target.key,
                )
                .map_err(CommitError::Host)?
                    && !legacy_row_matches_target_type(
                        reader,
                        MAIN_WORLD,
                        declaration.target.as_str(),
                        &write.key.target.key,
                        write.key.target.object_type.as_str(),
                    )
                    .map_err(CommitError::Host)?
                {
                    return Err(CommitError::EffectiveValues {
                        key: write.key.source.clone(),
                        reason: format!(
                            "link `{}` target key `{}` has ambiguous or mismatched legacy edge identity",
                            write.key.link, write.key.target.key,
                        ),
                    });
                }
            }
        }
    }
    verify_link_endpoint_tables(reader, &add_endpoints).map_err(CommitError::Host)
}

/// Bounded O(#tables) table-existence probe for link tables referenced by
/// `_norn_link_version` rows touching added link endpoints (#1527 S2).
/// Uses two separate covering-index seeks (`source`, `target`) instead of `OR`
/// and probes each discovered table via O(1) `SELECT 1 FROM <table> LIMIT 1`.
fn verify_link_endpoint_tables<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    endpoints: &BTreeSet<&str>,
) -> Result<(), CellError> {
    if endpoints.is_empty() {
        return Ok(());
    }
    let mut probed: BTreeSet<String> = BTreeSet::new();
    for id in endpoints {
        for (column, index) in [
            ("source", "_norn_link_version/source_order"),
            ("target", "_norn_link_version/target_order"),
        ] {
            let rows = reader.conn.query(
                &format!(
                    "SELECT DISTINCT \"link_table\" FROM \"{LINK_VERSIONS}\" INDEXED BY \"{index}\" WHERE \"world\" = ?1 AND \"{column}\" = ?2"
                ),
                params![MAIN_WORLD, *id],
            )?;
            for values in rows {
                let table: String = SqlRow(values).get(0)?;
                if probed.insert(table.clone()) {
                    let _ = reader.conn.query_optional(
                        &format!("SELECT 1 FROM {} LIMIT 1", quote_identifier(&table)),
                        params![],
                        |_| Ok(()),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Defense-in-depth inside `Writer::apply_links` (governed `CellHost` and
/// verified mapping commits): a link mutation must never overwrite a version
/// slot (live or tombstoned) or live legacy row belonging to a different or
/// ambiguous canonical target root.
pub(crate) fn check_slot_target_match<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    table_name: &str,
    declared_target: &str,
    source: &str,
    target: &str,
    target_type: &str,
) -> Result<(), CellError> {
    let matches = match reader.link_version_row(world, table_name, source, target)? {
        Some((_, written_target_type)) => {
            reader.canonical_type(&written_target_type) == reader.canonical_type(target_type)
        }
        None => {
            if live_link_row_exists(reader, world, table_name, source, target)? {
                legacy_row_matches_target_type(reader, world, declared_target, target, target_type)?
            } else {
                true
            }
        }
    };
    if !matches {
        return Err(CellError::ComputeRefused(
            "link mutation target type does not match stored edge slot",
        ));
    }
    Ok(())
}

/// Bounded outgoing `has_link_target` existence proof (`LIMIT (excluded + 1)`).
pub(crate) fn has_link_target<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    source: &FactKey,
    link: &str,
    excluded: &BTreeSet<FactKey>,
) -> Result<Option<bool>, CellError> {
    let Some((source_type, _, _)) =
        reader.root_row(MAIN_WORLD, source.object_type.as_str(), &source.key)?
    else {
        return Ok(Some(false));
    };
    let limits = norn_ir::ResourceLimitProfile::norn_limits_v1().slice;
    let fetch = excluded.len().saturating_add(1).min(
        usize::try_from(limits.max_records)
            .unwrap_or(usize::MAX)
            .saturating_add(1),
    );
    let Some(targets) = reader.link_targets_with_versions(
        MAIN_WORLD,
        &source_type,
        link,
        &source.key,
        fetch,
        limits.max_canonical_bytes,
    )?
    else {
        return Ok(None);
    };
    if targets
        .iter()
        .any(|(key, ty, _)| !excluded.contains(&FactKey::new(reader.canonical_type(ty), key)))
    {
        return Ok(Some(true));
    }
    Ok((targets.len() < fetch).then_some(false))
}

/// Bounded incoming `has_link_source` existence proof (`LIMIT (excluded + 1)`)
/// for `from: one` cardinality checks without loading full endpoint adjacency.
pub(crate) fn has_link_source<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    target: &FactKey,
    link: &str,
    excluded: &BTreeSet<FactKey>,
) -> Result<Option<bool>, CellError> {
    let Some((concrete_target, _, _)) =
        reader.root_row(MAIN_WORLD, target.object_type.as_str(), &target.key)?
    else {
        return Ok(Some(false));
    };
    let target_root = reader.canonical_type(target.object_type.as_str());
    let limits = norn_ir::ResourceLimitProfile::norn_limits_v1().slice;
    let fetch = excluded.len().saturating_add(1).min(
        usize::try_from(limits.max_records)
            .unwrap_or(usize::MAX)
            .saturating_add(1),
    );
    let fetch_sql = i64::try_from(fetch).unwrap_or(i64::MAX);
    let mut key_bytes = 0_u64;
    for table in &reader.model().schema().link_tables {
        if table.link.as_str() != link {
            continue;
        }
        let (declaration, _) = reader.model().link(table.source.as_str(), link)?;
        if !reader
            .model()
            .is_a(&concrete_target, declaration.target.as_str())
        {
            continue;
        }
        // Pin `INDEXED BY "{table}/target"` (present on both unique `from: one`
        // and non-unique `from: many` link tables) so SQLite always seeks
        // `(_world=? AND _target=?)` instead of scanning the PK by `_world`.
        let target_index = format!("{}/target", table.table);
        let rows = reader.conn.query(
            &format!(
                "SELECT {} FROM {} INDEXED BY {} WHERE {} = ?1 AND {} = ?2 LIMIT ?3",
                quote_identifier(SOURCE_COLUMN),
                quote_identifier(&table.table),
                quote_identifier(&target_index),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(TARGET_COLUMN),
            ),
            params![MAIN_WORLD, target.key, fetch_sql],
        )?;
        let source_root = reader.canonical_type(table.source.as_str());
        let mut source_keys = Vec::with_capacity(rows.len());
        for values in rows {
            let source_key: String = SqlRow(values).get(0)?;
            key_bytes =
                key_bytes.saturating_add(u64::try_from(source_key.len()).unwrap_or(u64::MAX));
            if key_bytes > limits.max_canonical_bytes {
                return Ok(None);
            }
            source_keys.push(source_key);
        }
        source_keys.sort_unstable();
        for source_key in &source_keys {
            let target_matches =
                match reader.link_version_row(MAIN_WORLD, &table.table, source_key, &target.key)? {
                    Some((_, written_target_type)) => {
                        reader.canonical_type(&written_target_type) == target_root
                    }
                    None => {
                        if reader.model().chain(declaration.target.as_str()).is_ok() {
                            reader.canonical_type(declaration.target.as_str()) == target_root
                        } else if legacy_row_matches_target_type(
                            reader,
                            MAIN_WORLD,
                            declaration.target.as_str(),
                            &target.key,
                            target.object_type.as_str(),
                        )? {
                            true
                        } else {
                            return Ok(None);
                        }
                    }
                };
            if !target_matches {
                continue;
            }
            if !excluded.contains(&FactKey::new(source_root.clone(), source_key.as_str())) {
                return Ok(Some(true));
            }
        }
        if source_keys.len() >= fetch {
            return Ok(None);
        }
    }
    Ok(Some(false))
}

/// Fold every matched member of `commit.predicate_reads` (both `SliceKey::Predicate`
/// and action aggregate preconditions) into `marking_reads` so classified
/// aggregate/predicate inputs propagate into write and link IFC guards.
pub(crate) fn extend_predicate_marking_reads(
    predicate_reads: &[PredicateReadExpectation],
    marking_reads: &mut Vec<FactExpectation>,
) {
    for read in predicate_reads {
        for member in &read.members {
            let expectation = FactExpectation {
                key: member.key.clone(),
                expected: member.version,
            };
            if !marking_reads.contains(&expectation) {
                marking_reads.push(expectation);
            }
        }
    }
}

/// Extract canonical `(object_type, sorted inspected_properties)` specs from
/// every predicate read (every filter key plus any temporal/window property)
/// so `Writer::apply_writes` resolves schema and per-row candidate IFC markings
/// inside the commit transaction, even when a predicate matches zero rows.
pub(crate) fn predicate_marking_specs(
    predicate_reads: &[PredicateReadExpectation],
) -> Vec<(ObjectTypeName, Vec<String>)> {
    let mut by_type: BTreeMap<ObjectTypeName, BTreeSet<String>> = BTreeMap::new();
    for read in predicate_reads {
        let props = by_type
            .entry(read.predicate.object_type.clone())
            .or_default();
        props.extend(read.predicate.filter.keys().cloned());
        if let Some(window) = &read.predicate.window {
            props.insert(window.property.clone());
        }
    }
    by_type
        .into_iter()
        .map(|(object_type, props)| (object_type, props.into_iter().collect()))
        .collect()
}

/// Resolve IFC markings for all filter and window properties inspected by
/// `predicate_reads` inside the commit transaction:
/// - Schema-declared object-level and inspected-property markings across
///   `object_type` and every concrete subtype (covers empty 0-row tables).
/// - Per-row occurrence markings and dynamic/stored markings on inspected
///   properties across all live candidate rows (matching, non-matching, or
///   truncated) using the same bounded scan (`scan_cap` + `DecodeBudget`),
///   failing closed with `norn.unknown-lineage` if bounds or snapshots fail.
/// - Never taints from unrelated classified properties on non-matching rows.
pub(crate) fn predicate_filter_lineage_markings<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    predicate_reads: &[(ObjectTypeName, Vec<String>)],
) -> Result<BTreeSet<String>, CellError> {
    let mut labels = BTreeSet::new();
    if predicate_reads.is_empty() {
        return Ok(labels);
    }
    let profile = norn_ir::ResourceLimitProfile::norn_limits_v1();
    let scan_cap = usize::try_from(profile.slice.max_records).unwrap_or(usize::MAX);
    let limits = ReactiveLimits::from_profile(&profile);
    for (object_type_name, inspected_properties) in predicate_reads {
        let object_type = object_type_name.as_str();
        if !reader
            .model()
            .object_types()
            .any(|defined| defined.name.as_str() == object_type)
        {
            labels.insert("norn.unknown-lineage".to_owned());
            continue;
        }
        let mut declared_props: BTreeSet<&str> = BTreeSet::new();
        for def in reader.model().object_types() {
            if !reader.model().is_a(def.name.as_str(), object_type) {
                continue;
            }
            labels.extend(reader.model().object_markings(def.name.as_str())?);
            let prop_markings = reader
                .model()
                .effective_property_markings(def.name.as_str())?;
            for prop in inspected_properties {
                if let Some(declared) = prop_markings.get(prop) {
                    declared_props.insert(prop.as_str());
                    labels.extend(declared.iter().cloned());
                }
            }
        }
        if inspected_properties
            .iter()
            .any(|prop| !declared_props.contains(prop.as_str()))
        {
            labels.insert("norn.unknown-lineage".to_owned());
        }
        let mut budget = DecodeBudget::new(&limits);
        let Some(records) = reader.list_after_with_budget(
            world,
            object_type,
            scan_cap.saturating_add(1),
            &mut budget,
        )?
        else {
            labels.insert("norn.unknown-lineage".to_owned());
            continue;
        };
        if records.len() > scan_cap {
            labels.insert("norn.unknown-lineage".to_owned());
            continue;
        }
        collect_candidate_row_markings(reader, world, &records, inspected_properties, &mut labels)?;
    }
    Ok(labels)
}

fn collect_candidate_row_markings<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    records: &[crate::read::ObjectRecord],
    inspected_properties: &[String],
    labels: &mut BTreeSet<String>,
) -> Result<(), CellError> {
    for record in records {
        let Some(occurrence) =
            reader.fact_occurrence_markings(world, &record.object_type, &record.id, None)?
        else {
            labels.insert("norn.unknown-lineage".to_owned());
            continue;
        };
        labels.extend(occurrence);
        if inspected_properties.is_empty() {
            continue;
        }
        let Some(snapshot) =
            reader.effective_markings(world, &record.object_type, &record.id, None)?
        else {
            labels.insert("norn.unknown-lineage".to_owned());
            continue;
        };
        let declared_on_row = reader
            .model()
            .effective_property_markings(&record.object_type)?;
        for prop in inspected_properties {
            if !declared_on_row.contains_key(prop) {
                continue;
            }
            match snapshot.get(prop) {
                Some(marks) => labels.extend(marks.iter().cloned()),
                None => {
                    labels.insert("norn.unknown-lineage".to_owned());
                }
            }
        }
    }
    Ok(())
}

/// Fail-closed IFC check for live link mutations emitted under classified read
/// inputs: because edge membership in `object_links` is disclosed to callers
/// with `Population` access on both endpoints without per-link classification
/// gating, any live link mutation (`Add` or `Remove`) emitted under non-empty
/// read classification markings (including `norn.unknown-lineage`) is refused.
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

/// Run an action-authorized predicate read so EVERY matched row in the bounded
/// scan passes `authorize` BEFORE host-side source-identity dispute checks or
/// `max_members + 1` truncation can surface.
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
    // Pass `usize::MAX` so `select_members` does not truncate away a hidden
    // row past `max_members + 1` before authorization; `load` still bounds the
    // scan by `scan_cap` (`max_records`) and `max_bytes`.
    let Some(mut objects) = load(reader, predicate, usize::MAX, max_bytes)? else {
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
    objects.truncate(max_members.saturating_add(1));
    Ok(PredicateActionRead::Admitted(objects))
}
