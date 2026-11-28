//! Source `kind == "delete"` mapping derivation (`DELETE_DERIVATION = 3`) and
//! cell-side observation preparation (#1028, #1931).
//!
//! Preserves `MAP-DERIVATION-1` compatibility for pre-B5 `derivation <= 2`
//! quarantined delete bundles while enabling `CompiledMapping::apply_delete`
//! and `prepare_observed_delete` for derivation 3 delete bundles.

use norn_checker::Diagnostic;
use norn_checker::mapping::{CompiledMapping, SourceDocument};
use norn_ir::mapping::MappedDelete;
use norn_ir::provenance::UtcTimestamp;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::snapshot_observe::{ObservedDelete, prepare_observed_delete, tombstone_key};
use norn_kernel::{
    CellCommit, DecisionTurn, FactKey, FactSlice, FactState, FactVersion, ObservationAuthorization,
    ObservationEvent, TypeCatalog,
};

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::StoreReader;

/// Runtime derivation number for `kind == "delete"` bundles (#1028/#1931).
pub(crate) const DELETE_DERIVATION: u32 = 3;

/// Target derivation number for a newly captured raw observation.
pub(crate) fn target_derivation_for(raw: &RuntimeValue) -> u32 {
    if raw.get("kind").and_then(RuntimeValue::as_str) == Some("delete") {
        DELETE_DERIVATION
    } else {
        crate::mapping_bundle::CURRENT_DERIVATION
    }
}

/// Derive `MappedDelete` directives (or diagnostics) for a `kind == "delete"`
/// source document under `compiled`.
pub(crate) fn derive_delete_bundle(
    compiled: &Result<CompiledMapping, Vec<Diagnostic>>,
    document: &SourceDocument,
    recorded_at: &UtcTimestamp,
) -> Result<(Vec<MappedDelete>, Vec<RuntimeValue>), CellError> {
    let outcome = match compiled {
        Ok(compiled) => compiled
            .apply_delete(document, recorded_at)
            .map_err(|diags| diags.iter().map(Diagnostic::to_json).collect::<Vec<_>>()),
        Err(diags) => Err(diags.iter().map(Diagnostic::to_json).collect()),
    };
    match outcome {
        Ok(deletes) => Ok((deletes, Vec::new())),
        Err(diags) => Ok((
            Vec::new(),
            diags
                .iter()
                .map(|d| norn_ir::runtime::encode(d).map_err(CellError::Encoding))
                .collect::<Result<Vec<_>, _>>()?,
        )),
    }
}

/// Prepare every `MappedDelete` in `bundle` via kernel
/// [`prepare_observed_delete`] and append the resulting writes/events to
/// `commit` and `authorization`, skipping tombstones when a strictly newer
/// source version for the same source record has already landed
/// (`OBS-LINK-CONTRIBUTION-1` source-version ordering).
pub(crate) fn prepare_mapping_deletes<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
    catalog: &TypeCatalog<'_>,
    turn: &DecisionTurn,
    commit: &mut CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>,
    authorization: &mut ObservationAuthorization,
) -> Result<(), CellError> {
    if bundle.deletes.is_empty() || has_newer_or_incomparable_landed_source_version(reader, bundle)?
    {
        return Ok(());
    }
    for delete in &bundle.deletes {
        let key = FactKey::new(delete.object_type.as_str(), &delete.key);
        let slice = delete_slice(reader, &key)?;
        let prepared = prepare_observed_delete(
            catalog,
            &slice,
            turn,
            ObservedDelete {
                key: &key,
                lifecycles: &delete.lifecycle_snapshots,
                provenance: &delete.provenance,
            },
        )
        .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        prepared
            .append_to(commit, authorization)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
    }
    Ok(())
}

/// Verify `row`'s frontier metadata and receipt claim-write binding against
/// `_norn_export_journal` + `_norn_receipt`.
pub(crate) fn verify_frontier_row_meta<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    row: &crate::link_claim_frontier::FrontierRow,
) -> Result<(), CellError> {
    let meta = crate::link_claim_frontier::claim_source_meta(
        reader.conn,
        Some(reader),
        row.commit_seq,
        Some(&row.claim_id),
        &row.claim.observation_id,
        true,
    )?;
    if meta.source_version != row.source_version
        || meta.version_kind != row.version_kind
        || meta.event_keys != row.event_keys
    {
        return Err(CellError::LedgerCorrupt {
            seq: row.commit_seq,
            reason: format!("frontier metadata mismatch for {}", row.claim_id),
        });
    }
    Ok(())
}

/// Verify `row` against `_norn_export_journal` + `_norn_receipt` and parse its
/// source version.
pub(crate) fn parse_verified_frontier_version<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    row: &crate::link_claim_frontier::FrontierRow,
) -> Result<Option<norn_ir::SourceVersion>, CellError> {
    verify_frontier_row_meta(reader, row)?;
    Ok((!row.source_version.trim().is_empty())
        .then(|| norn_ir::SourceVersion::parse(row.version_kind, &row.source_version).ok())
        .flatten())
}

fn bundle_is_delete(bundle: &crate::mapping_bundle::MappingBundle) -> bool {
    !bundle.deletes.is_empty()
        || bundle.raw.get("kind").and_then(RuntimeValue::as_str) == Some("delete")
}

/// True when any already-landed bundle or accepted correction for `bundle`'s
/// source record (same mapping, dataset, connector instance, partition, source,
/// and `source_id`) or active non-reject frontier claim on its deleted subjects
/// has a strictly newer `source_version` than `bundle`, or when source ordering
/// cannot be established (`IncompatibleKinds` / `UnorderableOpaque`, failing closed).
pub(crate) fn has_newer_or_incomparable_landed_source_version<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
) -> Result<bool, CellError> {
    has_newer_or_incomparable_landed(reader, bundle, false)
}

/// True when a strictly newer (or incomparable) `kind == "delete"` bundle or
/// accepted delete correction for `bundle`'s source record has already landed
/// (including absent-subject no-op deletes, preventing delete-before-create
/// resurrection by stale upserts).
pub(crate) fn has_newer_or_incomparable_landed_delete<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
) -> Result<bool, CellError> {
    has_newer_or_incomparable_landed(reader, bundle, true)
}

fn has_newer_or_incomparable_landed<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
    deletes_only: bool,
) -> Result<bool, CellError> {
    let kind = reader.bundle_version_kind(bundle)?;
    let incoming_version = crate::mapping_links::parse_source_version(bundle, kind)?;
    if !deletes_only {
        let scope = bundle.source_scope()?;
        for delete in &bundle.deletes {
            for row in reader.active_claims_for_source_delete(
                &scope,
                delete.object_type.as_str(),
                &delete.key,
            )? {
                if reader.is_reject_carry_claim(&row)? {
                    continue;
                }
                if let Some(frontier_ver) = parse_verified_frontier_version(reader, &row)? {
                    match frontier_ver.try_cmp(&incoming_version) {
                        Ok(std::cmp::Ordering::Greater) | Err(_) => return Ok(true),
                        Ok(std::cmp::Ordering::Less | std::cmp::Ordering::Equal) => {}
                    }
                }
            }
        }
    }
    let Some(source) = bundle.raw.get("source").and_then(RuntimeValue::as_str) else {
        return Ok(false);
    };
    let Some(source_id) = bundle.raw.get("source_id").and_then(RuntimeValue::as_str) else {
        return Ok(false);
    };
    if has_newer_subject_history_bundle(
        reader,
        bundle,
        source,
        source_id,
        &incoming_version,
        deletes_only,
    )? {
        return Ok(true);
    }
    // Use `_norn_inbox/source_delete` when `deletes_only` (O(log D), backed by
    // `read_frontier_consistent` + `verify_zero_write_delete_inbox` so zero-write
    // absent-subject deletes never require scanning receipts on ordinary upserts)
    // or `_norn_inbox/source_event` otherwise.
    let kind_filter = if deletes_only {
        " AND json_extract(i.\"observed\", '$.input.raw.kind') = 'delete'"
    } else {
        ""
    };
    let inbox_sql = format!(
        "SELECT i.\"observation_id\", i.\"observed\", i.\"consumed_commit_seq\" \
         FROM \"{inbox}\" i \
         WHERE i.\"observation_id\" LIKE 'mapping-bundle:%'{kind_filter} \
           AND json_extract(i.\"observed\", '$.input.input.connectorInstance') = ?1 \
           AND json_extract(i.\"observed\", '$.input.input.partition') = ?2 \
           AND json_extract(i.\"observed\", '$.input.raw.source') = ?3 \
           AND json_extract(i.\"observed\", '$.input.raw.source_id') = ?4 \
           AND json_extract(i.\"observed\", '$.input.input.mapping') = ?5 \
           AND json_extract(i.\"observed\", '$.input.input.dataset') = ?6 \
           AND i.\"consumed_commit_seq\" IS NOT NULL",
        inbox = crate::system::INBOX,
    );
    let inbox_params = crate::engine::params![
        bundle.input.connector_instance.as_str(),
        bundle.input.partition.as_str(),
        source,
        source_id,
        bundle.input.mapping.as_str(),
        bundle.input.dataset.as_str(),
    ];
    let rows = if deletes_only {
        reader.read_frontier_consistent(|| reader.conn.query(&inbox_sql, &inbox_params))?
    } else {
        reader.conn.query(&inbox_sql, &inbox_params)?
    };
    for raw in rows {
        let row = crate::engine::SqlRow(raw);
        let obs_id: String = row.get(0)?;
        let encoded: String = row.get(1)?;
        let consumed_seq: i64 = row.get(2)?;
        let consumed_u64 = u64::try_from(consumed_seq).map_err(|err| CellError::LedgerCorrupt {
            seq: 0,
            reason: format!("negative consumed_commit_seq for {obs_id}: {err}"),
        })?;
        if reader.commit_is_verified_dispute_resolution(consumed_seq)? {
            continue;
        }
        let landed = authenticated_landed_bundle(reader, &obs_id, consumed_u64, &encoded)?;
        if (!deletes_only || bundle_is_delete(&landed))
            && landed_bundle_is_newer(reader, &landed, &incoming_version)?
        {
            return Ok(true);
        }
    }
    has_newer_landed_correction(
        reader,
        bundle,
        source,
        source_id,
        &incoming_version,
        deletes_only,
    )
}

fn map_corrupt_err(err: CellError, corrupt: &dyn Fn(&str) -> CellError) -> CellError {
    match err {
        CellError::LedgerCorrupt { .. } => err,
        other => corrupt(&format!("{other}")),
    }
}

/// Tamper-proof subject history check via immutable `_norn_fact_values`
/// (`_norn_fact_values/key_frontier` index across both subjects and their
/// tombstone keys for `bundle.deletes` and `bundle.records`) plus indexed
/// zero-event observation receipts (`_norn_receipt/noop_observation` for no-op
/// linkless observations): even if `_norn_inbox.observed` JSON fields were
/// corrupted to hide a newer landed upsert, no-op observation, or tombstone
/// delete, every candidate commit is authenticated directly from `_norn_receipt`
/// and `_norn_export_journal`.
fn collect_delete_history_commits<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
) -> Result<std::collections::BTreeSet<i64>, CellError> {
    let mut commits = std::collections::BTreeSet::new();
    let scope = bundle.source_scope()?;
    for (ty, key) in bundle
        .deletes
        .iter()
        .map(|d| (d.object_type.as_str(), d.key.as_str()))
        .chain(
            bundle
                .records
                .iter()
                .map(|r| (r.object_type.as_str(), r.key.as_str())),
        )
    {
        let buried = tombstone_key(&FactKey::new(ty, key));
        for raw in reader.conn.query(
            &format!(
                "SELECT DISTINCT \"commit_seq\" FROM \"{}\" \
                 WHERE \"world\" = ?1 AND \"id\" = ?2 AND \"object_type\" = ?3",
                crate::system::FACT_VALUES,
            ),
            crate::engine::params![crate::MAIN_WORLD, buried.key, buried.object_type.as_str()],
        )? {
            commits.insert(crate::engine::SqlRow(raw).get::<i64>(0)?);
        }
        for raw in reader.conn.query(
            &format!(
                "SELECT DISTINCT \"commit_seq\" FROM \"{}\" \
                 WHERE \"world\" = ?1 \
                   AND json_extract(\"event\", '$.op') = 'observed_deleted' \
                   AND json_extract(\"event\", '$.key.objectType') = ?2 \
                   AND json_extract(\"event\", '$.key.key') = ?3",
                crate::system::LEDGER,
            ),
            crate::engine::params![crate::MAIN_WORLD, ty, key],
        )? {
            commits.insert(crate::engine::SqlRow(raw).get::<i64>(0)?);
        }
        commits.extend(reader.delete_withdrawal_commits_for_subject(&scope, ty, key)?);
    }
    Ok(commits)
}

fn collect_full_history_commits<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
) -> Result<std::collections::BTreeSet<i64>, CellError> {
    let mut commits = std::collections::BTreeSet::new();
    let mut subject_keys: Vec<(String, String)> = Vec::new();
    for (ty, key) in bundle
        .deletes
        .iter()
        .map(|d| (d.object_type.as_str(), d.key.as_str()))
        .chain(
            bundle
                .records
                .iter()
                .map(|r| (r.object_type.as_str(), r.key.as_str())),
        )
    {
        subject_keys.push((ty.to_owned(), key.to_owned()));
        let buried = tombstone_key(&FactKey::new(ty, key));
        subject_keys.push((buried.object_type.as_str().to_owned(), buried.key));
    }
    for (object_type, key) in &subject_keys {
        for raw in reader.conn.query(
            &format!(
                "SELECT DISTINCT \"commit_seq\" FROM \"{}\" \
                 WHERE \"world\" = ?1 AND \"id\" = ?2 AND \"object_type\" = ?3",
                crate::system::FACT_VALUES,
            ),
            crate::engine::params![crate::MAIN_WORLD, key, object_type],
        )? {
            commits.insert(crate::engine::SqlRow(raw).get::<i64>(0)?);
        }
        for raw in reader.conn.query(
            &format!(
                "SELECT DISTINCT \"commit_seq\" FROM \"{}\" \
                 WHERE \"world\" = ?1 \
                   AND json_extract(\"event\", '$.op') = 'observed_deleted' \
                   AND json_extract(\"event\", '$.key.objectType') = ?2 \
                   AND json_extract(\"event\", '$.key.key') = ?3",
                crate::system::LEDGER,
            ),
            crate::engine::params![crate::MAIN_WORLD, object_type, key],
        )? {
            commits.insert(crate::engine::SqlRow(raw).get::<i64>(0)?);
        }
    }
    for raw in reader.conn.query(
        &format!(
            "SELECT \"commit_seq\" FROM \"{}\" \
             WHERE \"world\" = ?1 \
               AND json_extract(\"receipt\", '$.consumed.kind') = 'observation' \
               AND (json_array_length(json_extract(\"receipt\", '$.written')) = 0 \
                    OR json_extract(\"receipt\", '$.consumed.observationId') LIKE 'mapping-page:%')",
            crate::system::RECEIPT,
        ),
        crate::engine::params![crate::MAIN_WORLD],
    )? {
        commits.insert(crate::engine::SqlRow(raw).get::<i64>(0)?);
    }
    Ok(commits)
}

fn has_newer_subject_history_bundle<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
    source: &str,
    source_id: &str,
    incoming_version: &norn_ir::SourceVersion,
    deletes_only: bool,
) -> Result<bool, CellError> {
    let candidate_commits = if deletes_only {
        collect_delete_history_commits(reader, bundle)?
    } else {
        collect_full_history_commits(reader, bundle)?
    };
    for seq_i64 in candidate_commits {
        let seq_u64 = u64::try_from(seq_i64).map_err(|err| CellError::LedgerCorrupt {
            seq: 0,
            reason: format!("negative commit_seq in subject history: {err}"),
        })?;
        if reader.commit_is_verified_dispute_resolution(seq_i64)? {
            continue;
        }
        if subject_commit_has_newer_bundle(
            reader,
            seq_u64,
            bundle,
            source,
            source_id,
            incoming_version,
            deletes_only,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Authenticate a commit that wrote or tombstoned a deleted subject in
/// `_norn_fact_values` directly from `_norn_receipt` and `_norn_export_journal`
/// (never relying on mutable `_norn_inbox` fields to discover it).
fn subject_commit_has_newer_bundle<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    commit_seq: u64,
    incoming: &crate::mapping_bundle::MappingBundle,
    source: &str,
    source_id: &str,
    incoming_version: &norn_ir::SourceVersion,
    deletes_only: bool,
) -> Result<bool, CellError> {
    let corrupt = |msg: &str| CellError::LedgerCorrupt {
        seq: commit_seq,
        reason: format!("unverified subject commit {commit_seq}: {msg}"),
    };
    let Some(receipt) = reader
        .receipt_at(commit_seq)
        .map_err(|err| map_corrupt_err(err, &corrupt))?
    else {
        return Err(corrupt("missing receipt"));
    };
    if receipt.body.world != crate::MAIN_WORLD {
        return Err(corrupt("receipt world mismatch"));
    }
    let Some(crate::Consumption::Observation {
        observation_id: primary_obs_id,
    }) = receipt.body.consumed.as_ref()
    else {
        return Ok(false);
    };
    if !primary_obs_id.starts_with("mapping-bundle:")
        && !primary_obs_id.starts_with("mapping-page:")
    {
        return Ok(false);
    }
    let has_journal = reader
        .conn
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
            crate::engine::params![],
            |_row| Ok(()),
        )?
        .is_some();
    if !has_journal {
        return Err(corrupt("missing export journal table"));
    }
    let Some(export) = crate::export_journal::read_record(reader.conn, commit_seq)
        .map_err(|err| map_corrupt_err(err, &corrupt))?
    else {
        return Err(corrupt("missing export journal record"));
    };
    if export.receipt != receipt {
        return Err(corrupt("export journal receipt mismatch"));
    }
    let Some(consumed_val) = export.consumed_input.as_ref() else {
        return Err(corrupt("export journal missing consumed_input"));
    };
    let crate::export_journal::ConsumedInput::Observation {
        observation_id,
        observed,
        members,
        ..
    } = crate::export_journal::decode_observation_input(consumed_val)
        .map_err(|err| corrupt(&format!("invalid consumed_input: {err}")))?
    else {
        return Err(corrupt("consuming commit did not consume an observation"));
    };
    if &observation_id != primary_obs_id {
        return Err(corrupt(
            "consumed_input observation id disagrees with receipt",
        ));
    }
    let candidates: Vec<(String, RuntimeValue)> = if observation_id.starts_with("mapping-bundle:") {
        vec![(observation_id, observed)]
    } else {
        members
            .into_iter()
            .filter(|m| m.observation_id.starts_with("mapping-bundle:"))
            .map(|m| (m.observation_id, m.observed))
            .collect()
    };
    for (obs_id, auth_observed) in candidates {
        let landed = decode_verified_journal_bundle(&obs_id, &auth_observed, &corrupt)?;
        if landed.input.mapping == incoming.input.mapping
            && landed.input.dataset == incoming.input.dataset
            && landed.input.connector_instance == incoming.input.connector_instance
            && landed.input.partition == incoming.input.partition
            && landed.raw.get("source").and_then(RuntimeValue::as_str) == Some(source)
            && landed.raw.get("source_id").and_then(RuntimeValue::as_str) == Some(source_id)
            && (!deletes_only || bundle_is_delete(&landed))
        {
            reader
                .recheck_mapping_bundle(&landed)
                .map_err(|err| corrupt(&format!("bundle failed recheck: {err}")))?;
            if landed_bundle_is_newer(reader, &landed, incoming_version)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn decode_verified_journal_bundle(
    obs_id: &str,
    auth_observed: &RuntimeValue,
    corrupt: &dyn Fn(&str) -> CellError,
) -> Result<crate::mapping_bundle::MappingBundle, CellError> {
    let observed_ev: norn_kernel::Observed<RuntimeValue> = norn_ir::runtime::decode(auth_observed)
        .map_err(|err| corrupt(&format!("invalid Observed payload: {err}")))?;
    if observed_ev.id.0 != obs_id {
        return Err(corrupt("observed id mismatch"));
    }
    let landed: crate::mapping_bundle::MappingBundle = norn_ir::runtime::decode(&observed_ev.input)
        .map_err(|err| corrupt(&format!("invalid MappingBundle payload: {err}")))?;
    if obs_id != format!("mapping-bundle:{}", landed.digest) {
        return Err(corrupt("bundle digest mismatch"));
    }
    Ok(landed)
}

fn decode_and_recheck_bundle<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    obs_id: &str,
    auth_observed: &RuntimeValue,
    corrupt: &dyn Fn(&str) -> CellError,
) -> Result<crate::mapping_bundle::MappingBundle, CellError> {
    let landed = decode_verified_journal_bundle(obs_id, auth_observed, corrupt)?;
    reader
        .recheck_mapping_bundle(&landed)
        .map_err(|err| corrupt(&format!("bundle failed recheck: {err}")))?;
    Ok(landed)
}

/// Authenticate a consumed `mapping-bundle:` inbox row against its consuming
/// commit's durable receipt and `_norn_export_journal` `consumed_input` (either
/// directly consumed or consumed as a member of a page/reprocess commit), and
/// cryptographically recheck the bundle against retained definitions.
fn authenticated_landed_bundle<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    obs_id: &str,
    consumed_seq: u64,
    inbox_encoded: &str,
) -> Result<crate::mapping_bundle::MappingBundle, CellError> {
    let corrupt = |msg: &str| CellError::LedgerCorrupt {
        seq: consumed_seq,
        reason: format!("unverified landed bundle {obs_id}: {msg}"),
    };
    let Some(receipt) = reader
        .receipt_at(consumed_seq)
        .map_err(|err| map_corrupt_err(err, &corrupt))?
    else {
        return Err(corrupt("missing consuming receipt"));
    };
    if receipt.body.world != crate::MAIN_WORLD {
        return Err(corrupt("consuming receipt world mismatch"));
    }
    let Some(crate::Consumption::Observation {
        observation_id: primary_obs_id,
    }) = receipt.body.consumed.as_ref()
    else {
        return Err(corrupt("consuming receipt did not consume an observation"));
    };
    let has_journal = reader
        .conn
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
            crate::engine::params![],
            |_row| Ok(()),
        )?
        .is_some();
    if !has_journal {
        return Err(corrupt("missing export journal table"));
    }
    let Some(export) = crate::export_journal::read_record(reader.conn, consumed_seq)
        .map_err(|err| map_corrupt_err(err, &corrupt))?
    else {
        return Err(corrupt("missing consuming export journal record"));
    };
    if export.receipt != receipt {
        return Err(corrupt("export journal receipt mismatch"));
    }
    let Some(consumed_val) = export.consumed_input.as_ref() else {
        return Err(corrupt("consuming export journal missing consumed_input"));
    };
    let crate::export_journal::ConsumedInput::Observation {
        observation_id,
        observed,
        members,
        ..
    } = crate::export_journal::decode_observation_input(consumed_val)
        .map_err(|err| corrupt(&format!("invalid consumed_input: {err}")))?
    else {
        return Err(corrupt("consuming commit did not consume an observation"));
    };
    if &observation_id != primary_obs_id {
        return Err(corrupt(
            "consumed_input observation id disagrees with receipt",
        ));
    }
    let auth_observed = if observation_id == obs_id {
        observed
    } else if let Some(member) = members.into_iter().find(|m| m.observation_id == obs_id) {
        member.observed
    } else {
        return Err(corrupt(
            "landed bundle missing from consuming commit consumed_input",
        ));
    };
    let inbox_val = RuntimeValue::from_json_text(inbox_encoded)
        .map_err(|err| corrupt(&format!("corrupt inbox JSON: {err}")))?;
    let inbox_canon =
        crate::codec::canonical_json(&inbox_val).map_err(|err| map_corrupt_err(err, &corrupt))?;
    let auth_canon = crate::codec::canonical_json(&auth_observed)
        .map_err(|err| map_corrupt_err(err, &corrupt))?;
    if inbox_canon != auth_canon {
        return Err(corrupt("inbox payload disagrees with export journal"));
    }
    decode_and_recheck_bundle(reader, obs_id, &auth_observed, &corrupt)
}

fn has_newer_landed_correction<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
    source: &str,
    source_id: &str,
    incoming_version: &norn_ir::SourceVersion,
    deletes_only: bool,
) -> Result<bool, CellError> {
    let resolution_rows = reader.conn.query(
        &format!(
            "SELECT \"dispute_id\", \"decision\", \"actor\", \"reason\", \
                    \"commit_seq\", \"receipt_hash\" \
             FROM \"{res}\" ORDER BY \"commit_seq\", \"dispute_id\"",
            res = crate::link_claim_frontier::DISPUTE_RESOLUTION,
        ),
        crate::engine::params![],
    )?;
    let mut matched = false;
    for raw in resolution_rows {
        let row = crate::engine::SqlRow(raw);
        let rec = crate::mapping_conflict_scoped::decode_dispute_resolution_row(&row)?;
        if rec.decision != "accept-second-as-correction" {
            reader.verify_dispute_resolution_record(&rec)?;
            continue;
        }
        let landed = reader.verified_dispute_resolution_bundle(&rec)?;
        if !matched
            && landed.input.mapping == bundle.input.mapping
            && landed.input.dataset == bundle.input.dataset
            && landed.input.connector_instance == bundle.input.connector_instance
            && landed.input.partition == bundle.input.partition
            && landed.raw.get("source").and_then(RuntimeValue::as_str) == Some(source)
            && landed.raw.get("source_id").and_then(RuntimeValue::as_str) == Some(source_id)
            && (!deletes_only || bundle_is_delete(&landed))
            && landed_bundle_is_newer(reader, &landed, incoming_version)?
        {
            matched = true;
        }
    }
    Ok(matched)
}

/// Compare a landed bundle's source version parsed under its OWN declared
/// `VersionKind` against `incoming_version`; fails closed (`Ok(true)`) when
/// source ordering cannot be established (`IncompatibleKinds` / `UnorderableOpaque`).
fn landed_bundle_is_newer<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    landed: &crate::mapping_bundle::MappingBundle,
    incoming_version: &norn_ir::SourceVersion,
) -> Result<bool, CellError> {
    let landed_kind = reader.bundle_version_kind(landed)?;
    let landed_version = crate::mapping_links::parse_source_version(landed, landed_kind)?;
    Ok(match landed_version.try_cmp(incoming_version) {
        Ok(std::cmp::Ordering::Greater) | Err(_) => true,
        Ok(std::cmp::Ordering::Less | std::cmp::Ordering::Equal) => false,
    })
}

fn is_delete_withdrawal_claim(
    write: &norn_kernel::FactWrite,
    delete: &norn_ir::mapping::MappedDelete,
) -> bool {
    if write.key.object_type.as_str() != norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE {
        return false;
    }
    let norn_kernel::FactMutation::Put { values } = &write.mutation else {
        return false;
    };
    crate::link_claim_frontier::parse_claim_from_values(&write.key.key, values).is_ok_and(|claim| {
        claim.source_type == delete.object_type
            && claim.source_key == delete.key
            && claim.target.is_none()
    })
}

/// True when `key` is currently absent and `bundle`'s source record already has
/// a strictly newer (or incomparable) landed delete, suppressing stale
/// delete-before-create resurrection.
pub(crate) fn record_suppressed_by_newer_delete<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
    key: &FactKey,
    cached: &mut Option<bool>,
) -> Result<bool, CellError> {
    if reader
        .object(crate::MAIN_WORLD, key.object_type.as_str(), &key.key)?
        .is_some()
    {
        return Ok(false);
    }
    if let Some(suppressed) = *cached {
        return Ok(suppressed);
    }
    let suppressed = has_newer_or_incomparable_landed_delete(reader, bundle)?;
    *cached = Some(suppressed);
    Ok(suppressed)
}

/// Transaction-time guard rechecking delete and tombstone-lift eligibility
/// inside the commit write transaction so a concurrent newer linkless commit
/// cannot race past pre-transaction preparation.
pub(crate) fn verify_delete_and_tombstone_eligibility<E: 'static, C, I, S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    bundle: &crate::mapping_bundle::MappingBundle,
    commit: &CellCommit<E, C, I>,
) -> Result<(), norn_kernel::CommitError<CellError>> {
    // Fast path: check whether `commit` actually attempts a delete mutation
    // (tombstone Put / withdrawal claim / link removal / `ObservedDeleted`),
    // tombstone lift (Delete), or absent-record create BEFORE querying history.
    let has_delete_effect = bundle.deletes.iter().any(|delete| {
        let target_key = FactKey::new(delete.object_type.as_str(), &delete.key);
        let buried = tombstone_key(&target_key);
        commit.writes.iter().any(|w| {
            (w.key == buried && matches!(w.mutation, norn_kernel::FactMutation::Put { .. }))
                || is_delete_withdrawal_claim(w, delete)
        }) || commit.links.iter().any(|l| {
            l.key.source == target_key && matches!(l.mutation, norn_kernel::LinkMutation::Remove)
        }) || commit.events.iter().any(|event| {
            let any: &dyn std::any::Any = event;
            matches!(
                any.downcast_ref::<ObservationEvent>(),
                Some(ObservationEvent::ObservedDeleted { key, .. }) if *key == target_key
            )
        })
    });
    let has_tombstone_lift = bundle.records.iter().any(|record| {
        let buried = tombstone_key(&FactKey::new(record.object_type.as_str(), &record.key));
        commit
            .writes
            .iter()
            .any(|w| w.key == buried && matches!(w.mutation, norn_kernel::FactMutation::Delete))
    });
    if (has_delete_effect || has_tombstone_lift)
        && has_newer_or_incomparable_landed_source_version(reader, bundle)
            .map_err(norn_kernel::CommitError::Host)?
    {
        return Err(norn_kernel::CommitError::Host(
            CellError::SourceEvidenceChanged,
        ));
    }
    let mut has_record_create = false;
    for record in &bundle.records {
        let target_key = FactKey::new(record.object_type.as_str(), &record.key);
        let puts_record = commit.writes.iter().any(|w| {
            w.key == target_key && matches!(w.mutation, norn_kernel::FactMutation::Put { .. })
        });
        if puts_record
            && reader
                .object(
                    crate::MAIN_WORLD,
                    target_key.object_type.as_str(),
                    &target_key.key,
                )
                .map_err(norn_kernel::CommitError::Host)?
                .is_none()
        {
            has_record_create = true;
            break;
        }
    }
    if has_record_create
        && has_newer_or_incomparable_landed_delete(reader, bundle)
            .map_err(norn_kernel::CommitError::Host)?
    {
        return Err(norn_kernel::CommitError::Host(
            CellError::SourceEvidenceChanged,
        ));
    }
    Ok(())
}

fn delete_slice<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    key: &FactKey,
) -> Result<FactSlice, CellError> {
    let mut slice = reader.mapping_observation_slice(key)?;
    let buried = tombstone_key(key);
    let state = FactState {
        version: FactVersion(reader.version(
            crate::MAIN_WORLD,
            buried.object_type.as_str(),
            &buried.key,
        )?),
        values: reader
            .object(crate::MAIN_WORLD, buried.object_type.as_str(), &buried.key)?
            .map(|r| r.values),
    };
    slice.insert(buried, state);
    Ok(slice)
}

/// Observation slice for a mapped upsert: includes `tombstone_key(key)` when
/// a live tombstone exists AND no strictly newer (or incomparable) source
/// version has already landed, so [`prepare_observed_snapshot`] lifts older
/// tombstones (`ObservedRecreated`, #3013) without letting a late upsert
/// resurrect a newer deletion.
pub(crate) fn upsert_slice<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
    key: &FactKey,
) -> Result<FactSlice, CellError> {
    let mut slice = reader.mapping_observation_slice(key)?;
    if reader
        .model()
        .has_type(norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE)
    {
        let buried = tombstone_key(key);
        if let Some(record) =
            reader.object(crate::MAIN_WORLD, buried.object_type.as_str(), &buried.key)?
            && !has_newer_or_incomparable_landed_source_version(reader, bundle)?
        {
            slice.insert(
                buried,
                FactState {
                    version: FactVersion(record.version),
                    values: Some(record.values),
                },
            );
        }
    }
    Ok(slice)
}

/// The most bytes of one retained diagnostic text: a refused row's own
/// field can be as large as the request, and its full value is already in
/// the retained raw evidence, so a diagnostic never repeats it unbounded.
pub(crate) const DIAGNOSTIC_TEXT_BYTES: usize = 256;

/// `text`, cut to [`DIAGNOSTIC_TEXT_BYTES`] at a character boundary.
pub(crate) fn bounded_text(text: &str) -> String {
    bounded(text).0
}

fn bounded(text: &str) -> (String, bool) {
    if text.len() <= DIAGNOSTIC_TEXT_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = DIAGNOSTIC_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (format!("{}…", text.get(..end).unwrap_or_default()), true)
}

/// MAP-EXPLAIN-1: a row refusal as retained evidence with bounded field text.
pub(crate) fn explained(refusal: &norn_checker::mapping::RowRefusal) -> serde_json::Value {
    let mut value = refusal.diagnostic().to_json();
    let text = |text: &str| bounded(text).0.into();
    if let Some(fields) = value.as_object_mut() {
        if let Some(message) = fields.get("message").and_then(serde_json::Value::as_str) {
            let message = text(message);
            fields.insert("message".to_owned(), message);
        }
        fields.insert("map".to_owned(), text(&refusal.map));
        fields.insert("reason".to_owned(), text(&refusal.reason));
        if let Some(field) = &refusal.field {
            fields.insert("field".to_owned(), text(&field.field));
            if let Some(raw) = &field.value {
                let (preview, cut) = bounded(raw);
                fields.insert("value".to_owned(), preview.into());
                if cut {
                    fields.insert("valueTruncated".to_owned(), true.into());
                }
            } else {
                fields.insert("value".to_owned(), serde_json::Value::Null);
            }
            fields.insert("expected".to_owned(), text(&field.expected));
            fields.insert("reason".to_owned(), text(&field.reason));
        }
    }
    value
}

impl<S: SqlEngine> StoreReader<'_, S> {
    /// Emit `target: None` withdrawal claims under `obs_id` for every active
    /// contribution owned by `bundle`'s source scope on deleted subjects.
    pub(crate) fn delete_withdrawal_claims_for_obs(
        &self,
        bundle: &crate::mapping_bundle::MappingBundle,
        obs_id: &str,
        batch_history: &std::collections::BTreeMap<
            String,
            Vec<(
                norn_ir::SourceVersion,
                String,
                norn_ir::mapping::ObservedLinkClaim,
            )>,
        >,
    ) -> Result<Vec<norn_ir::mapping::ObservedLinkClaim>, CellError> {
        if bundle.deletes.is_empty()
            || has_newer_or_incomparable_landed_source_version(self, bundle)?
        {
            return Ok(Vec::new());
        }
        let scope = bundle.source_scope()?;
        let mut withdrawals = Vec::new();
        for delete in &bundle.deletes {
            let mut seen_contributions = std::collections::BTreeSet::new();
            let active = self.active_claims_for_source_delete(
                &scope,
                delete.object_type.as_str(),
                &delete.key,
            )?;
            let mut candidates: Vec<norn_ir::mapping::ObservedLinkClaim> =
                active.into_iter().map(|row| row.claim).collect();
            for records in batch_history.values() {
                if let Some((_, _, last)) = records.last()
                    && last.source_scope == scope
                    && last.source_type == delete.object_type
                    && last.source_key == delete.key
                    && last.target.is_some()
                {
                    candidates.push(last.clone());
                }
            }
            for existing in candidates {
                let contribution = existing.contribution_id()?;
                if !seen_contributions.insert(contribution) {
                    continue;
                }
                let mut provenance = delete.provenance.clone();
                provenance.transform =
                    Some(format!("{}.{}#delete", delete.map, existing.role.as_str()));
                withdrawals.push(norn_ir::mapping::ObservedLinkClaim {
                    observation_id: obs_id.to_owned(),
                    source_scope: existing.source_scope,
                    source_type: existing.source_type,
                    source_key: existing.source_key,
                    role: existing.role,
                    target: None,
                    provenance,
                    supersedes: std::collections::BTreeSet::new(),
                });
            }
        }
        Ok(withdrawals)
    }
}
