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
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    already_consumes: bool,
    acceptance: &crate::result_reconciliation::Acceptance,
    writes: &[FactWrite],
    reads: &mut Vec<FactExpectation>,
) -> Result<crate::cell_host::Consumed, ActionRefusal<X>> {
    if already_consumes {
        return Err(ActionRefusal::ReceiptRefused {
            reason: crate::receipt_binding::ReceiptRefusal::BindingMismatch,
        });
    }
    merge_receipt_pins(catalog, &acceptance.reads, writes, reads)?;
    Ok(acceptance.consumed.clone())
}

/// Each receipt pin joins the commit's expectations by canonical identity
/// (hierarchy root and key, #1701): a pin on a supertype of an object the
/// commit writes or reads as its concrete type is that same expectation, so
/// a pin at the same version adds nothing (reads stay disjoint from writes)
/// and a pin at another version is a stale read.
pub(super) fn merge_receipt_pins<X>(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    pins: &[FactExpectation],
    writes: &[FactWrite],
    reads: &mut Vec<FactExpectation>,
) -> Result<(), ActionRefusal<X>> {
    let model = crate::CommitModel::new(catalog);
    for pin in pins {
        let canonical = model.canonical(&pin.key);
        let expected = writes
            .iter()
            .find(|write| model.canonical(&write.key) == canonical)
            .map(|write| write.expected)
            .or_else(|| {
                reads
                    .iter()
                    .find(|read| model.canonical(&read.key) == canonical)
                    .map(|read| read.expected)
            });
        match expected {
            Some(version) if version != pin.expected => {
                return Err(ActionRefusal::StalePreconditionRead {
                    key: pin.key.clone(),
                    link: None,
                });
            }
            Some(_) => {}
            None => reads.push(pin.clone()),
        }
    }
    Ok(())
}

/// Target's pre-commit version from `slice`, matched by canonical identity.
pub(super) fn target_before_version(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    slice: &crate::decision_commit::FactSlice,
    target: &FactKey,
) -> crate::cell_host::FactVersion {
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(target);
    slice
        .iter()
        .find(|(key, _)| model.canonical(key) == canonical)
        .map_or(crate::cell_host::FactVersion::ABSENT, |(_, state)| {
            state.version
        })
}

/// Ensure an untouched action target is CAS-verified inside the commit transaction,
/// matching existing writes and reads by canonical identity.
pub(super) fn ensure_target_read(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    target: &FactKey,
    before: crate::cell_host::FactVersion,
    writes: &[FactWrite],
    reads: &mut Vec<FactExpectation>,
) {
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(target);
    if !writes
        .iter()
        .any(|write| model.canonical(&write.key) == canonical)
        && !reads
            .iter()
            .any(|read| model.canonical(&read.key) == canonical)
    {
        reads.push(FactExpectation {
            key: target.clone(),
            expected: before,
        });
    }
}

/// Exact target version at `receipt.seq`: written version (matched by canonical
/// identity) or CAS-verified pre-commit version.
pub(super) fn committed_target_version(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    receipt: &crate::cell_host::CommitReceipt,
    target: &FactKey,
    before: crate::cell_host::FactVersion,
) -> crate::cell_host::FactVersion {
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(target);
    receipt
        .written
        .iter()
        .find(|written| model.canonical(&written.key) == canonical)
        .map_or(before, |written| written.version)
}
