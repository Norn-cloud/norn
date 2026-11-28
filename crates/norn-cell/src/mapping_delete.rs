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

/// Prepare every `MappedDelete` in `deletes` via kernel
/// [`prepare_observed_delete`] and append the resulting writes/events to
/// `commit` and `authorization`.
pub(crate) fn prepare_mapping_deletes<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    deletes: &[MappedDelete],
    catalog: &TypeCatalog<'_>,
    turn: &DecisionTurn,
    commit: &mut CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>,
    authorization: &mut ObservationAuthorization,
) -> Result<(), CellError> {
    for delete in deletes {
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
