//! Unified action read-footprint planning and tracked proof sealing (`ActionReadPlan`
//! + `TrackedReadSet`, Lane A3).
//!
//! Computes exact non-computed property footprints once during the read phase for
//! `CreateOnly` (client-key and naming-series), `Upsert`, `UpdateOnly` (including
//! lifecycle-bearing entities), and `FireTransition` (including `independentOf`
//! guards and transition guards), then seals [`ActionReferenceReads`] directly
//! from the tracked read plan without re-querying the authorizer.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ActionEffectProgram, ActionRoot, ObjectTypeName, PropertyId, RuntimeValue, SliceDeclaration,
    SliceKey, TransitionId, UpsertWriteMode,
};

use crate::cell_host::{CellCommit, CellHost, FactExpectation, FactKey, FactState, JsonObject};
use crate::decision_commit::{DecisionRefusal, TypeCatalog};
use crate::naming_series::NAMING_SERIES_COUNTER_TYPE;

use super::receipt::{
    ActionReferenceReads, SliceReadMode, collect_patch_law_property_reads,
    create_precondition_alias_reads, create_slice_property_reads, non_computed_property,
};
use super::types::{ActionPolicyReadFootprint, ActionRefusal};
use super::{check_kind, host_failure};

/// Loads a target fact and resolves its stored concrete subtype when present,
/// failing closed on an incompatible or unproven subtype so polymorphic actions
/// always run against the concrete subtype's invariants, lifecycles, and laws.
pub(super) fn load_target_with_concrete<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    object_kind: &str,
    target: &FactKey,
) -> Result<(FactKey, FactState), ActionRefusal<H::Error>> {
    crate::check_fact_key_identity(target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    match host.load_bounded(target, u64::MAX).map_err(host_failure)? {
        Some(bounded) => {
            if let Some(concrete) = bounded.concrete_type {
                if !catalog.lineage(&concrete).contains(&target.object_type) {
                    return Err(ActionRefusal::TargetNotFound);
                }
                check_kind(catalog, &concrete, object_kind)?;
                return Ok((
                    FactKey::new(concrete.as_str(), target.key.clone()),
                    bounded.state,
                ));
            }
            // SQLite reports missing values with the hierarchy's nonzero version when
            // a live sibling subtype occupies this key; refuse opaquely rather than
            // letting a create path surface a downstream TypeMismatch.
            if bounded.state.version != crate::cell_host::FactVersion::ABSENT {
                let canonical = crate::CommitModel::new(catalog).canonical(target);
                if canonical != *target
                    && host
                        .load_bounded(&canonical, u64::MAX)
                        .map_err(host_failure)?
                        .is_some_and(|root| root.concrete_type.is_some())
                {
                    return Err(ActionRefusal::TargetNotFound);
                }
            }
            Ok((target.clone(), bounded.state))
        }
        None => {
            let state = host.load(target).map_err(host_failure)?;
            let has_proper_subtypes = catalog.object_type_names().iter().any(|ty| {
                ty != &target.object_type && catalog.lineage(ty).contains(&target.object_type)
            });
            if state.exists() && has_proper_subtypes {
                return Err(ActionRefusal::TargetNotFound);
            }
            Ok((target.clone(), state))
        }
    }
}

/// Reconcile naming-series collision probes with already-captured slice reads
/// by canonical key so an `allow_gaps` create that slice-reads a skipped key
/// never emits duplicate read expectations or silently retries a stale
/// precondition read.
pub(super) fn reconcile_skipped_reads<X>(
    catalog: &TypeCatalog<'_>,
    reads: &mut Vec<FactExpectation>,
    probes: Vec<FactExpectation>,
    precondition_reads: &BTreeSet<FactKey>,
) -> Result<Vec<FactExpectation>, ActionRefusal<X>> {
    let model = crate::CommitModel::new(catalog);
    let mut skipped_reads = Vec::new();
    for skipped in probes {
        let canonical = model.canonical(&skipped.key);
        if let Some(existing) = reads
            .iter()
            .find(|read| model.canonical(&read.key) == canonical)
        {
            if existing.expected != skipped.expected {
                if precondition_reads
                    .iter()
                    .any(|key| model.canonical(key) == canonical)
                {
                    return Err(ActionRefusal::StalePreconditionRead {
                        key: existing.key.clone(),
                        link: None,
                    });
                }
                return Err(ActionRefusal::Decision(DecisionRefusal::Stale {
                    key: existing.key.clone(),
                    expected: skipped.expected,
                    actual: existing.expected,
                }));
            }
            continue;
        }
        skipped_reads.push(skipped.clone());
        reads.push(skipped);
    }
    Ok(skipped_reads)
}

/// Whether the action mutates an existing target or creates a new one.
#[derive(Clone)]
enum PlanKind {
    CreateOnly,
    ExistingTarget {
        target_properties: Vec<String>,
        alias_properties: BTreeMap<String, Vec<String>>,
    },
}

/// Precomputed per-entry slice read plan and tracked property footprint for one
/// closed action call.
#[derive(Clone)]
pub(super) struct ActionReadPlan {
    kind: PlanKind,
    /// Per-slice-entry load mode: `None` means full snapshot (used for target
    /// aliases on existing-target updates/transitions); `Some(empty)` means
    /// reference-only; `Some(props)` means projected read of `props`.
    slice_modes: Vec<Option<Vec<String>>>,
}

/// Tracked read footprint bound to one action call from slice load through commit.
#[derive(Clone)]
pub(super) struct TrackedReadSet {
    pub(super) policy_footprint: ActionPolicyReadFootprint,
    plan: Option<ActionReadPlan>,
}

impl TrackedReadSet {
    /// Plan exact reads for one action call before loading slice facts.
    /// Downgrades to conservative (`plan: None`) if the action, policy
    /// footprint, or host cannot safely project every read.
    pub(super) fn prepare<H: CellHost + ?Sized>(
        host: &H,
        catalog: &TypeCatalog<'_>,
        action: &ActionRoot,
        policy_footprint: ActionPolicyReadFootprint,
        target: &FactKey,
        target_exists: bool,
        effective_input: &JsonObject,
    ) -> Self {
        let plan = if host.supports_reference_reads() {
            ActionReadPlan::build(
                host,
                catalog,
                action,
                &policy_footprint,
                target,
                target_exists,
                effective_input,
            )
        } else {
            None
        };
        Self {
            policy_footprint,
            plan,
        }
    }

    /// Whether this call qualified for exact reference/property lineage.
    pub(super) const fn is_exact(&self) -> bool {
        self.plan.is_some()
    }

    /// Host read mode for `action.slice[index]`.
    pub(super) fn slice_mode(&self, index: usize) -> SliceReadMode<'_> {
        let Some(plan) = &self.plan else {
            return SliceReadMode::Full;
        };
        match plan.slice_modes.get(index) {
            Some(Some(props)) if props.is_empty() => SliceReadMode::Reference,
            Some(Some(props)) => SliceReadMode::Projected(props.as_slice()),
            Some(None) | None => SliceReadMode::Full,
        }
    }

    /// Seal [`ActionReferenceReads`] directly from the tracked read plan and
    /// prepared commit without re-querying the authorizer.
    pub(super) fn seal_proof<E, C, I>(
        &self,
        catalog: &TypeCatalog<'_>,
        action: &ActionRoot,
        commit: &CellCommit<E, C, I>,
        target: &FactKey,
        effective_input: &JsonObject,
        skipped_reads: &[FactExpectation],
    ) -> Option<ActionReferenceReads> {
        let plan = self.plan.as_ref()?;
        plan.seal_proof(
            catalog,
            action,
            commit,
            target,
            effective_input,
            skipped_reads,
        )
    }
}

impl ActionReadPlan {
    fn build<H: CellHost + ?Sized>(
        host: &H,
        catalog: &TypeCatalog<'_>,
        action: &ActionRoot,
        policy_footprint: &ActionPolicyReadFootprint,
        target: &FactKey,
        target_exists: bool,
        effective_input: &JsonObject,
    ) -> Option<Self> {
        let ActionPolicyReadFootprint::Exact { target_attributes } = policy_footprint else {
            return None;
        };
        if !action.commands.is_empty()
            || action.synchronous_rule.is_some()
            || !action.aggregates.is_empty()
        {
            return None;
        }
        match &action.effect_program {
            Some(ActionEffectProgram::UpsertInputObject {
                write_mode: UpsertWriteMode::CreateOnly,
                ..
            }) => {
                // Validate full create slice + LAW-LINK-1 closure up front.
                create_slice_property_reads(catalog, action, target)?;
                let slice_modes = plan_slice_modes(
                    host,
                    catalog,
                    action,
                    effective_input,
                    None,
                    &create_precondition_alias_reads(catalog, action)?,
                )?;
                Some(Self {
                    kind: PlanKind::CreateOnly,
                    slice_modes,
                })
            }
            Some(ActionEffectProgram::UpsertInputObject {
                write_mode: UpsertWriteMode::Upsert | UpsertWriteMode::UpdateOnly,
                ..
            }) if target_exists => Self::build_existing_target(
                host,
                catalog,
                action,
                target,
                target_attributes,
                effective_input,
                None,
            ),
            Some(ActionEffectProgram::FireTransition {
                lifecycle,
                transition,
                accept_receipt: None,
                ..
            }) if target_exists => Self::build_existing_target(
                host,
                catalog,
                action,
                target,
                target_attributes,
                effective_input,
                Some((lifecycle, transition)),
            ),
            Some(
                ActionEffectProgram::UpsertInputObject {
                    write_mode:
                        UpsertWriteMode::Upsert
                        | UpsertWriteMode::UpdateOnly
                        | UpsertWriteMode::DraftOnly { .. },
                    ..
                }
                | ActionEffectProgram::FireTransition { .. }
                | ActionEffectProgram::MutateParentWithRows { .. },
            )
            | None => None,
        }
    }

    fn build_existing_target<H: CellHost + ?Sized>(
        host: &H,
        catalog: &TypeCatalog<'_>,
        action: &ActionRoot,
        target: &FactKey,
        policy_target_attributes: &BTreeSet<String>,
        effective_input: &JsonObject,
        fired: Option<(&PropertyId, &TransitionId)>,
    ) -> Option<Self> {
        if !action.create_links.is_empty()
            || action.quantity_effect.is_some()
            || catalog
                .ancestry(&target.object_type)
                .iter()
                .any(|ty| ty.accounting_leg.is_some())
        {
            return None;
        }
        let alias_reads = create_precondition_alias_reads(catalog, action)?;
        let model = crate::CommitModel::new(catalog);
        let canonical_target = model.canonical(target);
        let mut target_properties = collect_existing_target_properties(
            catalog,
            action,
            target,
            policy_target_attributes,
            fired,
        )?;
        let mut external_aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in &action.slice {
            let props = alias_reads.get(entry.name.as_str())?;
            let entry_canonical = slice_entry_canonical(&model, entry, effective_input)?;
            if entry_canonical == canonical_target {
                for prop in props {
                    if !non_computed_property(catalog, &target.object_type, prop) {
                        return None;
                    }
                    target_properties.insert(prop.clone());
                }
            } else {
                external_aliases.insert(entry.name.clone(), props.iter().cloned().collect());
            }
        }
        let slice_modes = plan_slice_modes(
            host,
            catalog,
            action,
            effective_input,
            Some(&canonical_target),
            &alias_reads,
        )?;
        Some(Self {
            kind: PlanKind::ExistingTarget {
                target_properties: target_properties.into_iter().collect(),
                alias_properties: external_aliases,
            },
            slice_modes,
        })
    }

    fn seal_proof<E, C, I>(
        &self,
        catalog: &TypeCatalog<'_>,
        action: &ActionRoot,
        commit: &CellCommit<E, C, I>,
        target: &FactKey,
        effective_input: &JsonObject,
        skipped_reads: &[FactExpectation],
    ) -> Option<ActionReferenceReads> {
        if !commit.effects.is_empty()
            || commit.consumes.is_some()
            || !commit.link_reads.is_empty()
            || !commit.predicate_reads.is_empty()
        {
            return None;
        }
        let allow_counter = matches!(self.kind, PlanKind::CreateOnly);
        let allow_target_update = matches!(self.kind, PlanKind::ExistingTarget { .. });
        if commit.writes.iter().any(|write| {
            write.expected.0 != 0
                && !(allow_target_update && write.key == *target)
                && !(allow_counter && write.key.object_type.as_str() == NAMING_SERIES_COUNTER_TYPE)
        }) {
            return None;
        }
        let mut references = commit.reads.clone();
        if allow_target_update {
            references.extend(
                commit
                    .writes
                    .iter()
                    .filter(|write| write.expected.0 != 0 && write.key == *target)
                    .map(|write| FactExpectation {
                        key: write.key.clone(),
                        expected: write.expected,
                    }),
            );
        }
        if references.is_empty() {
            return None;
        }
        let property_reads = match &self.kind {
            PlanKind::CreateOnly => bind_create_property_reads(
                catalog,
                action,
                target,
                effective_input,
                &references,
                skipped_reads,
            )?,
            PlanKind::ExistingTarget {
                target_properties,
                alias_properties,
            } => bind_existing_property_reads(
                catalog,
                action,
                target,
                effective_input,
                &references,
                target_properties,
                alias_properties,
            )?,
        };
        let laws_digest =
            ActionReferenceReads::governing_laws_digest(catalog.law_artifacts.iter().copied())?;
        Some(ActionReferenceReads {
            reads: commit.reads.clone(),
            references,
            property_reads,
            writes: commit.writes.clone(),
            links: commit.links.clone(),
            link_reads: commit.link_reads.clone(),
            event_count: commit.events.len(),
            laws_digest,
        })
    }
}

fn slice_entry_canonical(
    model: &crate::CommitModel<'_, '_>,
    entry: &SliceDeclaration,
    effective_input: &JsonObject,
) -> Option<FactKey> {
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
    Some(model.canonical(&FactKey::new(entry.object_type.as_str(), key)))
}

fn plan_slice_modes<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    effective_input: &JsonObject,
    canonical_target: Option<&FactKey>,
    by_alias: &BTreeMap<&str, BTreeSet<String>>,
) -> Option<Vec<Option<Vec<String>>>> {
    let model = crate::CommitModel::new(catalog);
    let mut by_canonical: BTreeMap<FactKey, BTreeSet<String>> = BTreeMap::new();
    let mut entry_canonicals: Vec<(&SliceDeclaration, FactKey)> =
        Vec::with_capacity(action.slice.len());
    for entry in &action.slice {
        let canonical = slice_entry_canonical(&model, entry, effective_input)?;
        let props = by_alias.get(entry.name.as_str())?;
        by_canonical
            .entry(canonical.clone())
            .or_default()
            .extend(props.iter().cloned());
        entry_canonicals.push((entry, canonical));
    }
    let mut modes = Vec::with_capacity(entry_canonicals.len());
    for (entry, canonical) in entry_canonicals {
        // Keep direct-target slice aliases on full load so target-snapshot
        // reconciliation sees the complete stored row; property lineage is
        // still narrowed by `target_properties`.
        if canonical_target.is_some_and(|target_key| *target_key == canonical) {
            modes.push(None);
            continue;
        }
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
        modes.push(Some(prop_list));
    }
    Some(modes)
}

fn collect_self_expression_reads(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    text: &str,
    out: &mut BTreeSet<String>,
) -> Option<()> {
    if norn_expr::expression_reads(text, "self").ok()?.whole_root
        || !norn_expr::expression_element_paths(text, "self")
            .ok()?
            .is_empty()
    {
        return None;
    }
    for path in norn_expr::expression_read_paths(text, "self").ok()? {
        let [property] = path.as_slice() else {
            return None;
        };
        if !non_computed_property(catalog, object_type, property) {
            return None;
        }
        out.insert(property.clone());
    }
    Some(())
}

fn collect_existing_target_properties(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    target: &FactKey,
    policy_target_attributes: &BTreeSet<String>,
    fired: Option<(&PropertyId, &TransitionId)>,
) -> Option<BTreeSet<String>> {
    let effective = catalog.effective_type(&target.object_type)?;
    let mut properties = BTreeSet::new();
    for attr in policy_target_attributes {
        if !non_computed_property(catalog, &target.object_type, attr) {
            return None;
        }
        properties.insert(attr.clone());
    }
    // Every declared lifecycle on `target` must resolve; updates check all
    // lifecycle states (for immutable states / undeclared writes), and
    // transitions also evaluate the fired transition's guards and `independentOf`.
    let lifecycles = catalog.lifecycles(&target.object_type);
    if catalog
        .lifecycle_properties(&target.object_type)
        .into_iter()
        .any(|prop| !lifecycles.contains_key(&prop))
    {
        return None;
    }
    for prop in lifecycles.keys() {
        if !non_computed_property(catalog, &target.object_type, prop.as_str()) {
            return None;
        }
        properties.insert(prop.as_str().to_owned());
    }
    if let Some((lifecycle, transition_id)) = fired {
        let independent = crate::independence::independent_actor_properties(
            catalog,
            action,
            &target.object_type,
            lifecycle,
            transition_id,
        )?;
        for prop in independent {
            if !non_computed_property(catalog, &target.object_type, prop.as_str()) {
                return None;
            }
            properties.insert(prop.as_str().to_owned());
        }
        let declared = lifecycles.get(lifecycle)?;
        let matching: Vec<_> = declared
            .transitions
            .iter()
            .filter(|candidate| candidate.id == *transition_id)
            .collect();
        if matching.is_empty() {
            return None;
        }
        for candidate in matching {
            for guard in &candidate.guards {
                collect_self_expression_reads(
                    catalog,
                    &target.object_type,
                    guard.as_str(),
                    &mut properties,
                )?;
            }
        }
    }
    for invariant in catalog.invariants(&target.object_type).ok()? {
        collect_self_expression_reads(
            catalog,
            &target.object_type,
            invariant.invariant.expression.as_str(),
            &mut properties,
        )?;
    }
    collect_patch_law_property_reads(catalog, target, &mut properties)?;
    for property in &effective.properties {
        if !property.computed
            && matches!(
                &property.kind,
                norn_checker::EffectiveKind::Base(
                    norn_ir::PropertyType::Date {} | norn_ir::PropertyType::Timestamp { .. }
                )
            )
        {
            properties.insert(property.id.as_str().to_owned());
        }
    }
    Some(properties)
}

pub(super) fn bind_create_property_reads(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    target: &FactKey,
    effective_input: &JsonObject,
    references: &[FactExpectation],
    skipped_reads: &[FactExpectation],
) -> Option<Vec<(FactExpectation, Vec<String>)>> {
    let by_alias = create_slice_property_reads(catalog, action, target)?;
    let model = crate::CommitModel::new(catalog);
    let mut matched = vec![false; references.len()];
    let mut merged: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for entry in &action.slice {
        let canonical = slice_entry_canonical(&model, entry, effective_input)?;
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
    // Naming-series collision probes check existence (`version == ABSENT`) only
    // and carry occurrence-only reference lineage (no property reads).
    for (index, reference) in references.iter().enumerate() {
        let covered = matched.get_mut(index)?;
        if !*covered && reference.expected.0 != 0 && skipped_reads.contains(reference) {
            *covered = true;
        }
    }
    if matched.iter().any(|covered| !covered) {
        return None;
    }
    let mut property_reads = Vec::with_capacity(merged.len());
    for (index, props) in merged {
        let reference = references.get(index)?.clone();
        property_reads.push((reference, props.into_iter().collect()));
    }
    Some(property_reads)
}

fn bind_existing_property_reads(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    target: &FactKey,
    effective_input: &JsonObject,
    references: &[FactExpectation],
    target_properties: &[String],
    alias_properties: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<(FactExpectation, Vec<String>)>> {
    let model = crate::CommitModel::new(catalog);
    let canonical_target = model.canonical(target);
    let mut matched = vec![false; references.len()];
    let mut merged: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut target_candidates = references
        .iter()
        .enumerate()
        .filter(|(_, reference)| model.canonical(&reference.key) == canonical_target);
    let (target_index, target_ref) = target_candidates.next()?;
    if target_candidates.next().is_some() || target_ref.expected.0 == 0 {
        return None;
    }
    *matched.get_mut(target_index)? = true;
    merged
        .entry(target_index)
        .or_default()
        .extend(target_properties.iter().cloned());
    for entry in &action.slice {
        let canonical = slice_entry_canonical(&model, entry, effective_input)?;
        if canonical == canonical_target {
            continue;
        }
        let props = alias_properties.get(entry.name.as_str())?;
        let mut candidates = references
            .iter()
            .enumerate()
            .filter(|(_, reference)| model.canonical(&reference.key) == canonical);
        let (index, reference) = candidates.next()?;
        if candidates.next().is_some() || reference.expected.0 == 0 {
            return None;
        }
        *matched.get_mut(index)? = true;
        if !props.is_empty() {
            merged
                .entry(index)
                .or_default()
                .extend(props.iter().cloned());
        }
    }
    if matched.iter().any(|covered| !covered) {
        return None;
    }
    // Retain the target entry even when `target_properties` is empty so the
    // host keeps exact target-preservation lineage instead of whole-row reads.
    let mut property_reads = Vec::with_capacity(merged.len());
    for (index, props) in merged {
        let reference = references.get(index)?.clone();
        property_reads.push((reference, props.into_iter().collect()));
    }
    Some(property_reads)
}
