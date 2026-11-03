//! CELL-QUALIFY-1: portable receipt use is fenced by authenticated local origin.

use crate::result_reconciliation::{
    Classification, DispatchedCommand, ReconcileContext, RefusalReason, ResultRecord,
};
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
        None => context.is_none(),
        Some(origin) => context.is_some_and(|(cell, world)| origin.belongs_to(cell, world)),
    }
}

pub(crate) fn check_origin(
    context: &ReconcileContext<'_>,
    dispatched: &DispatchedCommand,
) -> Option<(Classification, RefusalReason)> {
    let invocation = &dispatched.invocation;
    let Some(origin) = invocation.provenance_commit.qualified() else {
        return context.cell.map(|_| {
            (
                Classification::Rejected,
                RefusalReason::OutOfScope {
                    detail: "legacy Function completion is local-only".to_owned(),
                },
            )
        });
    };
    if invocation.content_digest().ok().as_ref() != Some(&dispatched.dispatch) {
        return Some((
            Classification::Rejected,
            RefusalReason::BindingMismatch {
                member: "qualified dispatch".to_owned(),
            },
        ));
    }
    if origin.world != invocation.world
        || context
            .cell
            .is_none_or(|cell| !origin.belongs_to(cell, context.world))
    {
        return Some((
            Classification::Rejected,
            RefusalReason::OutOfScope {
                detail: "cell-qualified provenance".to_owned(),
            },
        ));
    }
    None
}
