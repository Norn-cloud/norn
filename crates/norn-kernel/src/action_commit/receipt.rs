use std::collections::BTreeSet;

use norn_ir::{FactChange, RuntimeValue};

use crate::cell_host::{
    CellCommit, CellHost, CommitError, FactExpectation, FactKey, FactWrite, JsonObject,
    LinkReadExpectation, LinkWrite,
};

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

pub(super) fn precondition_read_refusal<X>(
    error: &CommitError<X>,
    precondition_reads: &BTreeSet<FactKey>,
) -> Option<ActionRefusal<X>> {
    match error {
        CommitError::PredicateReadConflict { object_type } => {
            Some(ActionRefusal::StaleAggregateRead {
                object_type: object_type.clone(),
            })
        }
        CommitError::Conflict { key, .. } => {
            let key = if precondition_reads.contains(key) {
                Some(key.clone())
            } else {
                precondition_reads
                    .iter()
                    .find(|read| crate::snapshot_observe::tombstone_key(read) == *key)
                    .cloned()
            };
            key.map(|key| ActionRefusal::StalePreconditionRead { key, link: None })
        }
        CommitError::LinkReadConflict { source_key, link }
        | CommitError::UnsupportedLinkRead { source_key, link } => {
            Some(ActionRefusal::StalePreconditionRead {
                key: source_key.clone(),
                link: Some(link.clone()),
            })
        }
        CommitError::IdentityReuse
        | CommitError::IdentityCollision { .. }
        | CommitError::AtomicBoundary { .. }
        | CommitError::AtomicDeliveryBoundary { .. }
        | CommitError::ImmutableObject { .. }
        | CommitError::LifecycleInitialization { .. }
        | CommitError::OwnedRowsProtected { .. }
        | CommitError::DuplicateKey { .. }
        | CommitError::NotPending { .. }
        | CommitError::ReceiptNotAccepted { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::UnsupportedPredicateRead { .. }
        | CommitError::InvalidPredicateRead { .. }
        | CommitError::DanglingLink { .. }
        | CommitError::LinkTarget { .. }
        | CommitError::LinkCardinality { .. }
        | CommitError::LinkRequired { .. }
        | CommitError::TypeMismatch { .. }
        | CommitError::EffectiveValues { .. }
        | CommitError::TraversalLimitExceeded { .. }
        | CommitError::AccountingPosting { .. }
        | CommitError::InvariantViolation { .. }
        | CommitError::InvariantBudgetExceeded { .. }
        | CommitError::Law { .. }
        | CommitError::InvariantModel { .. }
        | CommitError::ObservationAuthorization
        | CommitError::EventLimitExceeded { .. }
        | CommitError::Overflow
        | CommitError::Host(_) => None,
    }
}

/// Kernel-owned attribution for direct reference validation. It cannot cross a
/// serialized commit boundary or be constructed by an action caller.
#[derive(Debug)]
pub struct ActionReferenceReads {
    pub(super) reads: Vec<FactExpectation>,
    pub(super) references: Vec<FactExpectation>,
    pub(super) property_reads: Vec<(FactExpectation, Vec<String>)>,
    pub(super) writes: Vec<FactWrite>,
    pub(super) links: Vec<LinkWrite>,
    pub(super) link_reads: Vec<LinkReadExpectation>,
    pub(super) event_count: usize,
}

impl ActionReferenceReads {
    /// Exact-version references whose existence, rather than values, was used.
    #[must_use]
    pub fn reference_reads(&self) -> &[FactExpectation] {
        &self.references
    }

    /// Exact property operands proved by the kernel's expression analysis.
    /// Empty properties preserve an independent patch target's identity only.
    #[must_use]
    pub fn property_reads(&self) -> &[(FactExpectation, Vec<String>)] {
        &self.property_reads
    }

    /// Bind the one-use proof to the kernel-prepared transaction.
    #[must_use]
    pub fn validates<E, C, I>(&self, commit: &CellCommit<E, C, I>) -> bool {
        self.reads == commit.reads
            && self.writes == commit.writes
            && self.links == commit.links
            && self.link_reads == commit.link_reads
            && commit.predicate_reads.is_empty()
            && self.event_count == commit.events.len()
            && commit.effects.is_empty()
            && commit.consumes.is_none()
    }

    /// Add ordinary CAS/lineage guards without changing any authorized output.
    /// An overlapping guard restores whole-object lineage for that source.
    pub fn with_read_guards<E, C, I>(
        mut self,
        commit: &mut CellCommit<E, C, I>,
        guards: &[FactExpectation],
    ) -> Option<Self> {
        if !self.validates(commit) {
            return None;
        }
        for guard in guards {
            if let Some(previous) = commit.reads.iter().find(|read| read.key == guard.key) {
                if previous.expected != guard.expected {
                    return None;
                }
            } else if commit.writes.iter().any(|write| write.key == guard.key) {
                return None;
            } else {
                commit.reads.push(guard.clone());
            }
            self.references.retain(|read| read.key != guard.key);
            self.property_reads
                .retain(|(read, _)| read.key != guard.key);
        }
        self.reads.clone_from(&commit.reads);
        Some(self)
    }
}
