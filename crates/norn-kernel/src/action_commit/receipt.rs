use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ActionEffectProgram, ActionRoot, FactChange, ObjectTypeName, RuntimeValue, SliceDeclaration,
    SliceKey, UpsertWriteMode,
};

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
    if let CommitError::PredicateReadConflict { object_type } = error {
        return Some(ActionRefusal::StaleAggregateRead {
            object_type: object_type.clone(),
        });
    }
    let stale_fact = match error {
        CommitError::Conflict { key, .. } if precondition_reads.contains(key) => Some(key.clone()),
        CommitError::Conflict { key, .. } => precondition_reads
            .iter()
            .find(|read| crate::snapshot_observe::tombstone_key(read) == *key)
            .cloned(),
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
        | CommitError::LinkReadConflict { .. }
        | CommitError::UnsupportedLinkRead { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::PredicateReadConflict { .. }
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
    };
    let stale_link = match error {
        CommitError::LinkReadConflict { source_key, link }
        | CommitError::UnsupportedLinkRead { source_key, link } => {
            Some((source_key.clone(), link.clone()))
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
        | CommitError::Conflict { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::PredicateReadConflict { .. }
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
    };
    if let Some(key) = stale_fact {
        return Some(ActionRefusal::StalePreconditionRead { key, link: None });
    }
    stale_link.map(|(key, link)| ActionRefusal::StalePreconditionRead {
        key,
        link: Some(link),
    })
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
    pub(super) laws_digest: String,
}

impl ActionReferenceReads {
    /// Canonical order-independent digest of the governing law artifacts used
    /// when this proof was constructed. Hosts reject precision when their
    /// enforced governing-law set differs from the kernel's catalog.
    pub(crate) fn governing_laws_digest<'a>(
        laws: impl IntoIterator<Item = &'a norn_ir::ArtifactEnvelope>,
    ) -> Option<String> {
        let mut digests = Vec::new();
        for artifact in laws {
            let bytes = norn_ir::canonical::canonical_json_of(artifact).ok()?;
            digests.push(norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes));
        }
        digests.sort();
        let encoded = norn_ir::canonical::canonical_json_of(&digests).ok()?;
        Some(norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&encoded))
    }

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

    /// Whether the kernel built this proof against the exact governing-law set
    /// the host enforces inside its commit transaction.
    #[must_use]
    pub fn matches_governing_laws(&self, laws: &[norn_ir::ArtifactEnvelope]) -> bool {
        Self::governing_laws_digest(laws).is_some_and(|digest| digest == self.laws_digest)
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

/// Whether `property` is a declared non-computed stored property on `declared`
/// and every concrete subtype that reaches it. Computed properties may depend
/// on unrelated classified fields and fail closed to whole-object lineage.
fn non_computed_property(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    declared: &ObjectTypeName,
    property: &str,
) -> bool {
    let Some(base) = catalog.effective_type(declared) else {
        return false;
    };
    if base
        .property(property)
        .is_none_or(|declared_prop| declared_prop.computed)
    {
        return false;
    }
    catalog.object_type_names().iter().all(|ty| {
        !crate::governing_laws::reaches(catalog, ty, declared)
            || catalog
                .effective_type(ty)
                .and_then(|effective| effective.property(property))
                .is_some_and(|declared_prop| !declared_prop.computed)
    })
}

/// Attribute any `LAW-LINK-1` forward-link properties that governing laws on
/// `target` will read through `action.create_links`, failing closed on inverse
/// link laws, whole-target reads, computed properties, or ambiguous bindings.
fn collect_create_law_property_reads<'a>(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    action: &'a ActionRoot,
    target: &FactKey,
    alias_properties: &mut BTreeMap<&'a str, BTreeSet<String>>,
) -> Option<()> {
    if action
        .create_links
        .iter()
        .any(|binding| binding.source.is_some())
    {
        return None;
    }
    let index = crate::governing_laws::link_law_index(catalog).ok()?;
    if index
        .inverse
        .iter()
        .any(|(_, source, _)| crate::governing_laws::reaches(catalog, &target.object_type, source))
    {
        return None;
    }
    for artifact in catalog.governing_laws(&target.object_type).ok()? {
        let law = crate::governing_laws::decode(catalog, artifact).ok()?;
        let link_reads =
            crate::governing_laws::law_link_reads(catalog, &target.object_type, law).ok()?;
        if link_reads.is_empty() {
            continue;
        }
        let mut link_to_entry: BTreeMap<&str, &SliceDeclaration> = BTreeMap::new();
        for read in &link_reads {
            let crate::governing_laws::LinkRead::Forward(link_id) = read else {
                return None;
            };
            let mut matching = action
                .create_links
                .iter()
                .filter(|binding| binding.link == *link_id);
            let Some(binding) = matching.next() else {
                // `target` is newly created and creates no link for `link_id`,
                // so `self.<link_id>` evaluates to `null` without reading facts.
                continue;
            };
            if matching.next().is_some() {
                return None;
            }
            let entry = action.slice.iter().find(|entry| {
                entry.object_type.as_str() == binding.target_type.as_str()
                    && matches!(
                        &entry.key,
                        SliceKey::Property { from, property }
                            if from == "input"
                                && property.as_str() == binding.target_id_field.as_str()
                    )
            })?;
            link_to_entry.insert(link_id.as_str(), entry);
        }
        if link_to_entry.is_empty() {
            continue;
        }
        for text in crate::governing_laws::law_expressions(law) {
            if norn_expr::expression_reads(text, "self").ok()?.whole_root {
                return None;
            }
            let elements = norn_expr::expression_element_paths(text, "self").ok()?;
            if elements.keys().any(|collection| {
                collection
                    .first()
                    .is_some_and(|head| link_to_entry.contains_key(head.as_str()))
            }) {
                return None;
            }
            for path in norn_expr::expression_read_paths(text, "self").ok()? {
                let head = path.first()?;
                let Some(entry) = link_to_entry.get(head.as_str()) else {
                    continue;
                };
                let [_, property] = path.as_slice() else {
                    return None;
                };
                if !non_computed_property(catalog, &entry.object_type, property) {
                    return None;
                }
                alias_properties
                    .get_mut(entry.name.as_str())?
                    .insert(property.clone());
            }
        }
    }
    Some(())
}

/// Exact non-computed slice property operands read ONLY by `action.preconditions`,
/// keyed by slice alias.
fn create_precondition_alias_reads<'a>(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    action: &'a ActionRoot,
) -> Option<BTreeMap<&'a str, BTreeSet<String>>> {
    let mut entries: BTreeMap<&str, &SliceDeclaration> = BTreeMap::new();
    let mut alias_properties: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for entry in &action.slice {
        if entry.optional
            || !matches!(&entry.key, SliceKey::Property { from, .. } if from == "input")
            || entries.insert(entry.name.as_str(), entry).is_some()
        {
            return None;
        }
        alias_properties.insert(entry.name.as_str(), BTreeSet::new());
    }
    for expression in &action.preconditions {
        if norn_expr::expression_reads(&expression.0, "slice")
            .ok()?
            .whole_root
        {
            return None;
        }
        if !norn_expr::expression_element_paths(&expression.0, "slice")
            .ok()?
            .is_empty()
        {
            return None;
        }
        for path in norn_expr::expression_read_paths(&expression.0, "slice").ok()? {
            let [alias, property] = path.as_slice() else {
                return None;
            };
            let entry = entries.get(alias.as_str())?;
            if !non_computed_property(catalog, &entry.object_type, property) {
                return None;
            }
            alias_properties
                .get_mut(alias.as_str())?
                .insert(property.clone());
        }
    }
    Some(alias_properties)
}

/// Exact non-computed slice property operands read by a closed `CreateOnly`
/// action's preconditions and `LAW-LINK-1` governing laws, keyed by slice alias.
/// Returns `None` (fail closed to whole-object lineage) for any unproved read.
pub(super) fn create_slice_property_reads(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    action: &ActionRoot,
    target: &FactKey,
) -> Option<BTreeMap<String, Vec<String>>> {
    let mut alias_properties = create_precondition_alias_reads(catalog, action)?;
    collect_create_law_property_reads(catalog, action, target, &mut alias_properties)?;
    Some(
        alias_properties
            .into_iter()
            .map(|(alias, props)| (alias.to_owned(), props.into_iter().collect()))
            .collect(),
    )
}

/// Host read strategy for one action slice fact.
#[derive(Clone, Copy)]
pub(super) enum SliceReadMode<'a> {
    Full,
    Reference,
    Projected(&'a [String]),
}

impl SliceReadMode<'_> {
    pub(super) const fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }

    pub(super) fn load<H: CellHost + ?Sized>(
        self,
        host: &H,
        requested: &FactKey,
        remaining_bytes: u64,
    ) -> Result<Option<crate::cell_host::BoundedFact>, H::Error> {
        match self {
            Self::Full => host.load_bounded(requested, remaining_bytes),
            Self::Reference => host.load_reference(requested, remaining_bytes),
            Self::Projected(properties) => {
                let prop_refs: Vec<&str> = properties.iter().map(String::as_str).collect();
                host.load_projected(requested, &prop_refs, remaining_bytes)
            }
        }
    }
}

/// Precomputed per-entry slice read plan for a closed `CreateOnly` action.
pub(super) struct CreateSlicePlan(Option<Vec<Vec<String>>>);

impl CreateSlicePlan {
    pub(super) fn mode_for(&self, index: usize) -> SliceReadMode<'_> {
        let Some(plan) = &self.0 else {
            return SliceReadMode::Full;
        };
        match plan.get(index) {
            Some(props) if props.is_empty() => SliceReadMode::Reference,
            Some(props) => SliceReadMode::Projected(props.as_slice()),
            None => SliceReadMode::Full,
        }
    }
}

/// Plan per-entry `CreateOnly` slice reads before loading any slice facts,
/// downgrading `reference_only` to `false` whenever any entry cannot be
/// projected safely without metering unread classified fields.
pub(super) fn prepare_create_slice_plan<H: CellHost + ?Sized>(
    host: &H,
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    action: &ActionRoot,
    effective_input: &JsonObject,
    reference_only: &mut bool,
) -> CreateSlicePlan {
    let create_only = matches!(
        action.effect_program,
        Some(ActionEffectProgram::UpsertInputObject {
            write_mode: UpsertWriteMode::CreateOnly,
            ..
        })
    );
    if !(*reference_only && create_only) {
        return CreateSlicePlan(None);
    }
    let planned = plan_create_projected_slice(host, catalog, action, effective_input);
    if planned.is_none() {
        *reference_only = false;
    }
    CreateSlicePlan(planned)
}

/// Plan the projected precondition properties per slice entry (merged by
/// canonical key across duplicate aliases) and confirm `host` can project every
/// non-empty property set without metering unread classified columns.
fn plan_create_projected_slice<H: CellHost + ?Sized>(
    host: &H,
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    action: &ActionRoot,
    effective_input: &JsonObject,
) -> Option<Vec<Vec<String>>> {
    let by_alias = create_precondition_alias_reads(catalog, action)?;
    let model = crate::CommitModel::new(catalog);
    let mut by_canonical: BTreeMap<FactKey, BTreeSet<String>> = BTreeMap::new();
    let mut entry_canonicals: Vec<(&SliceDeclaration, FactKey)> =
        Vec::with_capacity(action.slice.len());
    for entry in &action.slice {
        let SliceKey::Property { from, property } = &entry.key else {
            return None;
        };
        if from != "input" {
            return None;
        }
        let key = effective_input
            .get(property.as_str())
            .and_then(RuntimeValue::as_str)
            .filter(|value| !value.trim().is_empty())?;
        let canonical = model.canonical(&FactKey::new(entry.object_type.as_str(), key));
        let props = by_alias.get(entry.name.as_str())?;
        by_canonical
            .entry(canonical.clone())
            .or_default()
            .extend(props.iter().cloned());
        entry_canonicals.push((entry, canonical));
    }
    let mut per_entry = Vec::with_capacity(entry_canonicals.len());
    for (entry, canonical) in entry_canonicals {
        let props = by_canonical.get(&canonical)?;
        if !props
            .iter()
            .all(|prop| non_computed_property(catalog, &entry.object_type, prop))
        {
            return None;
        }
        let prop_list: Vec<String> = props.iter().cloned().collect();
        if !prop_list.is_empty() {
            let prop_refs: Vec<&str> = prop_list.iter().map(String::as_str).collect();
            if !host.supports_projected_read(&entry.object_type, &prop_refs) {
                return None;
            }
        }
        per_entry.push(prop_list);
    }
    Some(per_entry)
}

/// Bind each proved `CreateOnly` slice alias's read properties onto its exact
/// canonical `FactExpectation` in `references`, merging duplicate aliases and
/// refusing if any reference is unaccounted for.
pub(super) fn create_reference_property_reads(
    catalog: &crate::decision_commit::TypeCatalog<'_>,
    action: &ActionRoot,
    target: &FactKey,
    effective_input: &JsonObject,
    references: &[FactExpectation],
) -> Option<Vec<(FactExpectation, Vec<String>)>> {
    let by_alias = create_slice_property_reads(catalog, action, target)?;
    let model = crate::CommitModel::new(catalog);
    let mut matched = vec![false; references.len()];
    let mut merged: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for entry in &action.slice {
        let SliceKey::Property { from, property } = &entry.key else {
            return None;
        };
        if from != "input" {
            return None;
        }
        let key = effective_input
            .get(property.as_str())
            .and_then(RuntimeValue::as_str)
            .filter(|value| !value.trim().is_empty())?;
        let canonical = model.canonical(&FactKey::new(entry.object_type.as_str(), key));
        let mut candidates = references
            .iter()
            .enumerate()
            .filter(|(_, reference)| model.canonical(&reference.key) == canonical);
        let (index, reference) = candidates.next()?;
        if candidates.next().is_some() || reference.expected.0 == 0 {
            return None;
        }
        *matched.get_mut(index)? = true;
        let properties = by_alias.get(&entry.name)?;
        if !properties.is_empty() {
            merged
                .entry(index)
                .or_default()
                .extend(properties.iter().cloned());
        }
    }
    if !matched.iter().all(|seen| *seen) {
        return None;
    }
    let mut property_reads = Vec::with_capacity(merged.len());
    for (index, properties) in merged {
        let reference = references.get(index)?.clone();
        property_reads.push((reference, properties.into_iter().collect()));
    }
    Some(property_reads)
}
