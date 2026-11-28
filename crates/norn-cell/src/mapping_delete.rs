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
    if bundle.deletes.is_empty() || has_newer_landed_source_version(reader, bundle)? {
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

/// True when any already-landed bundle or accepted correction for `bundle`'s
/// source record (same mapping, dataset, connector instance, partition, source,
/// and `source_id`) or active non-reject frontier claim on its deleted subjects
/// has a strictly newer `source_version` than `bundle`.
fn has_newer_landed_source_version<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    bundle: &crate::mapping_bundle::MappingBundle,
) -> Result<bool, CellError> {
    let kind = reader.bundle_version_kind(bundle)?;
    let incoming_version = crate::mapping_links::parse_source_version(bundle, kind)?;
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
            if crate::mapping_links::parse_frontier_version(&row)
                .and_then(|v| v.try_cmp(&incoming_version).ok())
                == Some(std::cmp::Ordering::Greater)
            {
                return Ok(true);
            }
        }
    }
    let Some(source) = bundle.raw.get("source").and_then(RuntimeValue::as_str) else {
        return Ok(false);
    };
    let Some(source_id) = bundle.raw.get("source_id").and_then(RuntimeValue::as_str) else {
        return Ok(false);
    };
    let rows = reader.conn.query(
        &format!(
            "SELECT i.\"observed\" \
             FROM \"{inbox}\" i \
             WHERE i.\"consumed_commit_seq\" IS NOT NULL \
               AND substr(i.\"observation_id\", 1, 15) = 'mapping-bundle:' \
               AND json_extract(i.\"observed\", '$.input.input.mapping') = ?1 \
               AND json_extract(i.\"observed\", '$.input.input.dataset') = ?2 \
               AND json_extract(i.\"observed\", '$.input.input.connectorInstance') = ?3 \
               AND json_extract(i.\"observed\", '$.input.input.partition') = ?4 \
               AND json_extract(i.\"observed\", '$.input.raw.source') = ?5 \
               AND json_extract(i.\"observed\", '$.input.raw.source_id') = ?6 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM \"{res}\" r WHERE r.\"commit_seq\" = i.\"consumed_commit_seq\" \
               )",
            inbox = crate::system::INBOX,
            res = crate::link_claim_frontier::DISPUTE_RESOLUTION,
        ),
        crate::engine::params![
            bundle.input.mapping.as_str(),
            bundle.input.dataset.as_str(),
            bundle.input.connector_instance.as_str(),
            bundle.input.partition.as_str(),
            source,
            source_id,
        ],
    )?;
    for raw in rows {
        let encoded: String = crate::engine::SqlRow(raw).get(0)?;
        if let Ok(observed) = serde_json::from_str::<norn_kernel::Observed<RuntimeValue>>(&encoded)
            && let Ok(landed) =
                norn_ir::runtime::decode::<crate::mapping_bundle::MappingBundle>(&observed.input)
            && landed_bundle_is_newer(reader, &landed, &incoming_version)?
        {
            return Ok(true);
        }
    }
    let corrected_rows = reader.conn.query(
        &format!(
            "SELECT i.\"observation_id\", i.\"observed\" \
             FROM \"{inbox}\" i \
             JOIN \"{res}\" r ON r.\"dispute_id\" = i.\"observation_id\" \
             WHERE r.\"decision\" = 'accept-second-as-correction' \
               AND COALESCE( \
                   json_extract(i.\"observed\", '$.input.incoming.input.mapping'), \
                   json_extract(i.\"observed\", '$.input.bundle.input.mapping') \
               ) = ?1 \
               AND COALESCE( \
                   json_extract(i.\"observed\", '$.input.incoming.input.dataset'), \
                   json_extract(i.\"observed\", '$.input.bundle.input.dataset') \
               ) = ?2 \
               AND COALESCE( \
                   json_extract(i.\"observed\", '$.input.incoming.input.connectorInstance'), \
                   json_extract(i.\"observed\", '$.input.bundle.input.connectorInstance') \
               ) = ?3 \
               AND COALESCE( \
                   json_extract(i.\"observed\", '$.input.incoming.input.partition'), \
                   json_extract(i.\"observed\", '$.input.bundle.input.partition') \
               ) = ?4 \
               AND COALESCE( \
                   json_extract(i.\"observed\", '$.input.incoming.raw.source'), \
                   json_extract(i.\"observed\", '$.input.bundle.raw.source') \
               ) = ?5 \
               AND COALESCE( \
                   json_extract(i.\"observed\", '$.input.incoming.raw.source_id'), \
                   json_extract(i.\"observed\", '$.input.bundle.raw.source_id') \
               ) = ?6",
            inbox = crate::system::INBOX,
            res = crate::link_claim_frontier::DISPUTE_RESOLUTION,
        ),
        crate::engine::params![
            bundle.input.mapping.as_str(),
            bundle.input.dataset.as_str(),
            bundle.input.connector_instance.as_str(),
            bundle.input.partition.as_str(),
            source,
            source_id,
        ],
    )?;
    for raw in corrected_rows {
        let row = crate::engine::SqlRow(raw);
        let obs_id: String = row.get(0)?;
        let encoded: String = row.get(1)?;
        let Ok(observed) = serde_json::from_str::<norn_kernel::Observed<RuntimeValue>>(&encoded)
        else {
            continue;
        };
        let landed = if obs_id.starts_with("mapping-conflict:") {
            norn_ir::runtime::decode::<crate::mapping_conflict::MappingConflict>(&observed.input)
                .ok()
                .map(|c| c.incoming)
        } else if obs_id.starts_with("mapping-identity:") {
            norn_ir::runtime::decode::<crate::mapping_identity::IdentityEvidence>(&observed.input)
                .ok()
                .map(|i| i.bundle)
        } else {
            None
        };
        if let Some(landed) = landed
            && landed_bundle_is_newer(reader, &landed, &incoming_version)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Compare a landed bundle's source version parsed under its OWN declared
/// `VersionKind` against `incoming_version`, refusing incomparable kinds.
fn landed_bundle_is_newer<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    landed: &crate::mapping_bundle::MappingBundle,
    incoming_version: &norn_ir::SourceVersion,
) -> Result<bool, CellError> {
    let landed_kind = reader.bundle_version_kind(landed)?;
    let landed_version = crate::mapping_links::parse_source_version(landed, landed_kind)?;
    Ok(landed_version.try_cmp(incoming_version).ok() == Some(std::cmp::Ordering::Greater))
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
/// a live tombstone exists so [`prepare_observed_snapshot`] can lift it and
/// emit `ObservationEvent::ObservedRecreated` (#3013).
pub(crate) fn upsert_slice<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
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
