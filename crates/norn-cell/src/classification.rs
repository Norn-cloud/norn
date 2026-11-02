//! GRAPH-CLASS-1: classify a complete authorized base slice at one cell revision.
use crate::engine::SqlEngine;
use crate::reactive::{
    self, BudgetedRead, DecodeBudget, DependencySet, ReactiveLimits, SliceItem, SliceQuery,
    VisibleItem,
};
use crate::{CellError, CellStore};
use norn_checker::{ArtifactRef, classification::Classifier};
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use std::collections::{BTreeMap, BTreeSet};

/// A pure read result; membership uses the original base object identities.
pub struct Classification {
    /// Original base IDs whose admitted predicate holds.
    pub members: BTreeSet<String>,
    /// Cell commit revision shared by definitions, visibility and base rows.
    pub commit_seq: u64,
    /// Active definition version used by this read.
    pub definition_version: u64,
    /// Base range, object and authorization reads, including empty ranges.
    pub dependencies: DependencySet,
}

/// Failure to establish complete authoritative membership.
#[derive(Debug, thiserror::Error)]
pub enum ClassificationError {
    /// GRAPH-CLASS-1 refuses unknown, partial, stale or unbound input.
    #[error("core.graph.incomplete_classification")]
    Incomplete,
    /// A store read or dependency capture failed.
    #[error(transparent)]
    Store(#[from] CellError),
}

/// Evaluate installed classification with ordinary per-object visibility.
/// The trusted host must perform its usual `norn.read`/PDP checks in `visible`,
/// capturing authorization reads through the supplied shared budget.
/// No transaction, action, obligation, or membership write is produced.
///
/// # Errors
/// Refuses missing exact definitions, changed revisions, incomplete scans,
/// unknown predicate inputs and exhausted aggregate bounds.
pub fn evaluate<E: SqlEngine>(
    store: &CellStore<E>,
    classifier: &Classifier,
    world: &str,
    visible: &mut dyn FnMut(&SliceItem, &mut DecodeBudget) -> VisibleItem,
) -> Result<Classification, ClassificationError> {
    let incomplete = || ClassificationError::Incomplete;
    let head = store.verified_head()?;
    let reader = store.read();
    let installed = reader.installed_definitions(None)?.ok_or_else(incomplete)?;
    if installed.model_hash != reader.model().model_hash() {
        return Err(incomplete());
    }
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(&installed.canonical_set).map_err(|_error| incomplete())?;
    let definitions: BTreeSet<_> = artifacts
        .iter()
        .map(ArtifactRef::of)
        .collect::<Result<_, _>>()
        .map_err(|_error| incomplete())?;
    if !classifier
        .definitions()
        .iter()
        .all(|pin| definitions.contains(pin))
    {
        return Err(incomplete());
    }
    let norn_ir::definition_catalog::Member::Type { type_name } =
        &classifier.declaration().base.member
    else {
        return Err(incomplete());
    };
    let mut limits = ReactiveLimits::from_profile(&classifier.declaration().bounds);
    limits.max_scan = usize::try_from(classifier.declaration().bounds.slice.max_records)
        .map_err(|_error| incomplete())?;
    let mut budget = DecodeBudget::new(&limits);
    budget.capture();
    // A filled requested prefix is ordinarily complete. Request one beyond the
    // scan cap so only actual exhaustion can establish complete membership.
    let query = SliceQuery {
        object_type: type_name.clone(),
        filter: BTreeMap::new(),
        limit: classifier
            .declaration()
            .bounds
            .slice
            .max_records
            .saturating_add(1),
        binding: None,
    };
    let slice = reactive::evaluate_visible_with_shared_budget(
        &reader,
        world,
        &query,
        &limits,
        head.commits,
        &mut budget,
        visible,
    )?;
    if !slice.complete {
        return Err(incomplete());
    }
    let mut rows = Vec::new();
    for item in &slice.items {
        // Ordinary slices omit nulls; classification needs explicit stored NULL
        // to distinguish a known null from an absent/unknown predicate input.
        let BudgetedRead::Found(record) =
            reader.object_with_budget(world, type_name, &item.key, &mut budget)?
        else {
            return Err(incomplete());
        };
        if record.version != item.version {
            return Err(incomplete());
        }
        rows.push((item.key.clone(), RuntimeValue::Object(record.values)));
    }
    let members = classifier
        .classify(&rows, true)
        .map_err(|_error| incomplete())?;
    let dependencies = budget.take_dependencies()?;
    if store.verified_head()? != head {
        return Err(incomplete());
    }
    Ok(Classification {
        members,
        commit_seq: head.commits,
        definition_version: installed.version,
        dependencies,
    })
}
