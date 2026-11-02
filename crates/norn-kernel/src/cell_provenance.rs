//! CELL-QUALIFY-1: portable receipt use is fenced by authenticated local origin.

use crate::result_reconciliation::ResultRecord;
use norn_ir::{
    cell_id::CellId,
    cell_reference::{CellReference, ReferenceNamespace},
    function::Digest,
};

/// Portable lookup identity, absent for local-only legacy evidence.
/// Local receipt storage/consumption keys do not change.
///
/// # Errors
/// Canonical serialization failed.
pub fn receipt_reference(
    record: &ResultRecord,
) -> Result<Option<Digest>, norn_ir::canonical::CanonicalJsonError> {
    match &record.origin {
        None => Ok(None),
        Some(origin) => CellReference::new(
            origin.cell.clone(),
            origin.world.clone(),
            ReferenceNamespace::FunctionReceipt,
            record.receipt.0.clone(),
        )
        .content_digest()
        .map(Some),
    }
}

pub(crate) fn local_origin(record: &ResultRecord, context: Option<(&CellId, &str)>) -> bool {
    match &record.origin {
        None => true,
        Some(origin) => context.is_some_and(|(cell, world)| origin.belongs_to(cell, world)),
    }
}
