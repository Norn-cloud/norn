//! CELL-QUALIFY-1: portable receipt use is fenced by authenticated local origin.

use crate::result_reconciliation::ResultRecord;
use norn_ir::{
    cell_id::CellId,
    cell_reference::{QualifiedCommit, ReferenceNamespace},
    function::Digest,
};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptReferenceEnvelope<'a> {
    format: &'static str,
    namespace: ReferenceNamespace,
    origin: &'a QualifiedCommit,
    receipt: &'a Digest,
}

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
        Some(origin) => norn_ir::canonical::domain_hash_hex(
            b"norn.function.receipt-reference/v1\0",
            &ReceiptReferenceEnvelope {
                format: "norn.function.receipt-reference/v1",
                namespace: ReferenceNamespace::FunctionReceipt,
                origin,
                receipt: &record.receipt,
            },
        )
        .map(|hex| Some(Digest::from_hex(&hex))),
    }
}

pub(crate) fn local_origin(record: &ResultRecord, context: Option<(&CellId, &str)>) -> bool {
    match &record.origin {
        None => true,
        Some(origin) => context.is_some_and(|(cell, world)| origin.belongs_to(cell, world)),
    }
}
