use norn_ir::{FactChange, RuntimeValue};

use crate::cell_host::{CellHost, FactExpectation, FactKey, FactWrite, JsonObject};

use super::host_failure;
use super::types::ActionRefusal;

/// The property changes and delivery a receipt-accepting transition commits.
pub(super) struct ReceiptEffect {
    pub(super) changes: Vec<FactChange>,
    pub(super) acceptance: crate::result_reconciliation::Acceptance,
}

/// RECEIPT-EFFECT-1: resolve the input's receipt reference through the host and
/// turn the authenticated result into the declared property changes. Nothing
/// is consumed here; [`super::commit_effect`] consumes the acceptance's delivery in
/// the same commit as the transition.
pub(super) fn resolve_receipt_effect<H: CellHost + ?Sized>(
    host: &H,
    effective_input: &JsonObject,
    declaration: &norn_ir::ReceiptAcceptance,
    target: &FactKey,
) -> Result<ReceiptEffect, ActionRefusal<H::Error>> {
    use crate::receipt_binding::{ReceiptReference, ReceiptRefusal};
    let refuse = |reason| ActionRefusal::ReceiptRefused { reason };
    let reference = effective_input
        .get(&declaration.reference_field)
        .ok_or_else(|| ActionRefusal::InvalidInput {
            field: declaration.reference_field.clone(),
        })?;
    let reference = ReceiptReference::parse(reference).map_err(|error| refuse(error.into()))?;
    let binding = match host.resolve_accepted_receipt(&reference, &declaration.manifest_digest) {
        Ok(Some(binding)) => binding,
        Ok(None) => return Err(refuse(ReceiptRefusal::EffectUnsupported)),
        Err(error) => return Err(error.into_refusal().map_or_else(host_failure, refuse)),
    };
    // The host authenticates; the kernel re-checks the pins it can see.
    // RUN-BIND-1: a receipt resolves only the run it was dispatched under.
    binding
        .check_commit(&declaration.manifest_digest, &target.key)
        .map_err(refuse)?;
    let (digest, result, acceptance) = binding.into_parts();
    if acceptance.requires_cell_lane || acceptance.destination.is_some() {
        // Predicate pins and written destinations need their own fences.
        return Err(refuse(ReceiptRefusal::EffectUnsupported));
    }
    let mut changes = Vec::with_capacity(declaration.result_bindings.len().saturating_add(1));
    changes.push(FactChange::Set {
        object_type: target.object_type.clone(),
        key: target.key.clone(),
        property: declaration.digest_property.clone(),
        value: RuntimeValue::String(digest.0),
    });
    for (property, member) in &declaration.result_bindings {
        let value = result
            .get(member.as_str())
            .cloned()
            .ok_or_else(|| refuse(ReceiptRefusal::ResultType))?;
        changes.push(FactChange::Set {
            object_type: target.object_type.clone(),
            key: target.key.clone(),
            property: property.clone(),
            value,
        });
    }
    Ok(ReceiptEffect {
        changes,
        acceptance,
    })
}

/// RECEIPT-EFFECT-1: the receipt's delivery joins the transition's commit, with
/// every reconciliation pin as a compare-and-swap read. The host's pending
/// check makes this the receipt's only consumption; a refused commit applies
/// neither the transition nor the consumption.
pub(super) fn consume_receipt<X>(
    already_consumes: bool,
    acceptance: &crate::result_reconciliation::Acceptance,
    writes: &[FactWrite],
    reads: &mut Vec<FactExpectation>,
) -> Result<crate::cell_host::Consumed, ActionRefusal<X>> {
    let mismatch = ActionRefusal::ReceiptRefused {
        reason: crate::receipt_binding::ReceiptRefusal::BindingMismatch,
    };
    if already_consumes {
        return Err(mismatch);
    }
    for pin in &acceptance.reads {
        if let Some(write) = writes.iter().find(|write| write.key == pin.key) {
            if write.expected != pin.expected {
                return Err(ActionRefusal::StalePreconditionRead {
                    key: pin.key.clone(),
                    link: None,
                });
            }
        } else if let Some(read) = reads.iter().find(|read| read.key == pin.key) {
            if read.expected != pin.expected {
                return Err(ActionRefusal::StalePreconditionRead {
                    key: pin.key.clone(),
                    link: None,
                });
            }
        } else {
            reads.push(pin.clone());
        }
    }
    Ok(acceptance.consumed.clone())
}
