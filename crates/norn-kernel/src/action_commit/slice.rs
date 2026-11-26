use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{Expression, TurnFuel, Value as ExprValue};
use norn_ir::{
    ActionRoot, ObjectTypeName, ResourceLimitProfile, RuntimeValue, SliceDeclaration, SliceKey,
};

use crate::cell_host::{
    CellHost, FactKey, FactState, JsonObject, LinkReadExpectation, PredicateObject, PredicateQuery,
    PredicateReadExpectation,
};
use crate::decision_commit::{FactSlice, TypeCatalog};
use crate::limits::SliceMeter;

use super::host_failure;
use super::input::action_expression_context;
use super::read_plan::TrackedReadSet;
use super::receipt::SliceReadMode;
use super::types::{
    ActionAuthorizer, ActionCall, ActionParameterPin, ActionPreconditionOutcome,
    ActionPreconditionOutcomeRef, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
    ActionRefusal,
};

/// The facts, link sets, and expression values read by one action call.
pub(super) struct LoadedActionSlice {
    pub(super) facts: FactSlice,
    pub(super) tracked_reads: TrackedReadSet,
    pub(super) values: JsonObject,
    pub(super) link_reads: Vec<LinkReadExpectation>,
    pub(super) precondition_keys: BTreeSet<FactKey>,
    /// Folded aggregate inputs (#1105), by declared name.
    pub(super) aggregates: JsonObject,
    /// The predicate reads that pin each aggregate's members.
    pub(super) predicate_reads: Vec<PredicateReadExpectation>,
    /// Effective-dated parameter pins resolved in declaration order.
    pub(super) parameter_pins: Vec<ActionParameterPin>,
}

pub(super) struct ActionSliceContext<'a, 'model, 'call, H: CellHost + ?Sized> {
    pub(super) host: &'a H,
    pub(super) catalog: &'a TypeCatalog<'model>,
    pub(super) call: &'a ActionCall<'call>,
    pub(super) effective_input: &'a JsonObject,
    pub(super) authorizer: &'a dyn ActionAuthorizer,
}

pub(super) struct ActionSliceState {
    pub(super) facts: FactSlice,
    pub(super) values: JsonObject,
    pub(super) link_reads: Vec<LinkReadExpectation>,
    pub(super) predicate_snapshots: Vec<(PredicateQuery, Vec<PredicateObject>)>,
    pub(super) precondition_keys: BTreeSet<FactKey>,
    pub(super) entry_keys: BTreeMap<String, Option<FactKey>>,
    pub(super) entry_list_keys: BTreeMap<String, Vec<FactKey>>,
    pub(super) entry_depths: BTreeMap<String, u32>,
    pub(super) parameter_pins: Vec<ActionParameterPin>,
    pub(super) observed_records: u32,
    pub(super) meter: SliceMeter,
    pub(super) read_types: BTreeSet<ObjectTypeName>,
    pub(super) limits: crate::SliceLimits,
}

fn action_slice_key<X>(
    entry: &SliceDeclaration,
    effective_input: &JsonObject,
    values: &JsonObject,
    entry_depths: &BTreeMap<String, u32>,
) -> Result<(u32, Option<RuntimeValue>), ActionRefusal<X>> {
    Ok(match &entry.key {
        SliceKey::Property { from, property } if from == "input" => {
            let depth = 0;
            let value = effective_input.get(property.as_str()).cloned();
            (depth, value)
        }
        SliceKey::Property { from, property } => {
            let depth = entry_depths
                .get(from)
                .copied()
                .ok_or_else(|| ActionRefusal::SliceUnavailable {
                    entry: entry.name.clone(),
                    reason: format!("source entry {from:?} has no readable depth"),
                })?
                .saturating_add(1);
            let key = values
                .get(from)
                .and_then(RuntimeValue::as_object)
                .and_then(|object| object.get(property.as_str()))
                .cloned();
            (depth, key)
        }
        SliceKey::Link { from, .. } => {
            let depth = entry_depths
                .get(from)
                .copied()
                .ok_or_else(|| ActionRefusal::SliceUnavailable {
                    entry: entry.name.clone(),
                    reason: format!("link source entry {from:?} has no readable depth"),
                })?
                .saturating_add(1);
            (depth, None)
        }
        SliceKey::Parameter {
            from, default_from, ..
        } => {
            let from_depth =
                entry_depths
                    .get(from)
                    .copied()
                    .ok_or_else(|| ActionRefusal::SliceUnavailable {
                        entry: entry.name.clone(),
                        reason: format!("parameter source entry {from:?} has no readable depth"),
                    })?;
            let default_depth = match default_from.as_deref() {
                None => 0,
                Some(default_name) => entry_depths.get(default_name).copied().ok_or_else(|| {
                    ActionRefusal::SliceUnavailable {
                        entry: entry.name.clone(),
                        reason: format!(
                            "parameter defaultFrom {default_name:?} has no readable depth"
                        ),
                    }
                })?,
            };
            (from_depth.max(default_depth), None)
        }
        SliceKey::Predicate { .. } => (0, None),
        SliceKey::TriggerObject
        | SliceKey::TriggerBefore
        | SliceKey::TriggerAfter
        | SliceKey::TriggerChanges => {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "action slices cannot reference rule trigger keys".to_owned(),
            });
        }
    })
}

fn load_action_slice_entry<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
    read_mode: SliceReadMode<'_>,
) -> Result<(), ActionRefusal<H::Error>> {
    let ActionSliceContext {
        host,
        catalog,
        call,
        effective_input,
        authorizer,
    } = *context;
    let (depth, key) =
        action_slice_key(entry, effective_input, &state.values, &state.entry_depths)?;
    match &entry.key {
        SliceKey::Link { .. } => return load_action_slice_links(context, state, entry, depth),
        SliceKey::Parameter { .. } => {
            return load_action_slice_parameter(context, state, entry, depth);
        }
        SliceKey::Predicate { .. } => {
            return super::predicate::load_action_slice_predicate(context, state, entry);
        }
        SliceKey::Property { .. }
        | SliceKey::TriggerObject
        | SliceKey::TriggerBefore
        | SliceKey::TriggerAfter
        | SliceKey::TriggerChanges => {}
    }
    let ActionSliceState {
        facts,
        values,
        precondition_keys,
        entry_keys,
        entry_depths,
        observed_records,
        meter,
        read_types,
        limits,
        ..
    } = state;

    let Some(key) = key else {
        if entry.optional {
            entry_keys.insert(entry.name.clone(), None);
            entry_depths.insert(entry.name.clone(), depth);
            return Ok(());
        }
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry.name.clone(),
            reason: "the key property is absent".to_owned(),
        });
    };
    let key = match key {
        RuntimeValue::Null if entry.optional => {
            entry_keys.insert(entry.name.clone(), None);
            entry_depths.insert(entry.name.clone(), depth);
            return Ok(());
        }
        RuntimeValue::String(key) if !key.trim().is_empty() => key,
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Number(_)
        | RuntimeValue::String(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "the key property is not a non-blank string".to_owned(),
            });
        }
    };
    let requested = FactKey::new(entry.object_type.as_str(), key);
    if read_types.insert(entry.object_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, &entry.object_type)?;
    }
    let state = load_action_fact(
        host,
        catalog,
        call,
        effective_input,
        authorizer,
        &entry.name,
        &entry.object_type,
        &requested,
        depth,
        facts,
        meter,
        observed_records,
        limits,
        read_types,
        read_mode,
    )?;
    precondition_keys.insert(known_fact_key(facts, catalog, &requested));
    let Some(object) = state.values.as_ref() else {
        return Err(ActionRefusal::SliceObjectMissing {
            entry: entry.name.clone(),
        });
    };
    let concrete_key = known_fact_key(facts, catalog, &requested);
    entry_keys.insert(entry.name.clone(), Some(concrete_key));
    entry_depths.insert(entry.name.clone(), depth);
    values.insert(entry.name.clone(), RuntimeValue::Object(object.clone()));
    Ok(())
}

fn load_action_slice_parameter<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
    depth: u32,
) -> Result<(), ActionRefusal<H::Error>> {
    if depth > state.limits.max_relation_depth {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry.name.clone(),
            reason: format!(
                "relation depth {depth} exceeds {}",
                state.limits.max_relation_depth
            ),
        });
    }
    if state.read_types.insert(entry.object_type.clone()) {
        authorize_slice_type(
            context.call,
            context.effective_input,
            context.authorizer,
            &entry.object_type,
        )?;
    }
    let resolved = super::parameter::resolve_parameter_slice(
        &super::parameter::ParameterSliceInput {
            call: context.call,
            effective_input: context.effective_input,
            facts: &state.facts,
            entry_keys: &state.entry_keys,
            entry_list_keys: &state.entry_list_keys,
        },
        entry,
    )?;
    if !context
        .catalog
        .lineage(&resolved.key.object_type)
        .contains(&entry.object_type)
    {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry.name.clone(),
            reason: "selected parameter fact has a type outside the admitted entry type".to_owned(),
        });
    }
    state
        .values
        .insert(entry.name.clone(), RuntimeValue::Object(resolved.values));
    state
        .entry_keys
        .insert(entry.name.clone(), Some(resolved.key));
    state.entry_depths.insert(entry.name.clone(), depth);
    state.parameter_pins.push(resolved.pin);
    Ok(())
}

fn load_action_slice_links<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
    depth: u32,
) -> Result<(), ActionRefusal<H::Error>> {
    let ActionSliceContext {
        host,
        catalog,
        call,
        effective_input,
        authorizer,
    } = *context;
    let ActionSliceState {
        facts,
        values,
        link_reads,
        precondition_keys,
        entry_keys,
        entry_list_keys,
        entry_depths,
        observed_records,
        meter,
        read_types,
        limits,
        ..
    } = state;
    let SliceKey::Link { from, link } = &entry.key else {
        return Ok(());
    };
    let Some(source_key) = entry_keys.get(from).and_then(Option::as_ref) else {
        // A list sourced from an omitted optional entry is an empty
        // list; there is no existing source whose adjacency can change.
        values.insert(entry.name.clone(), RuntimeValue::Array(Vec::new()));
        entry_keys.insert(entry.name.clone(), None);
        entry_list_keys.insert(entry.name.clone(), Vec::new());
        entry_depths.insert(entry.name.clone(), depth);
        return Ok(());
    };
    if read_types.insert(entry.object_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, &entry.object_type)?;
    }
    let targets = action_link_snapshot(
        context,
        source_key,
        link,
        &entry.name,
        limits,
        *observed_records,
        meter.bytes(),
    )?;
    let mut items = Vec::with_capacity(targets.len());
    let mut item_keys = Vec::with_capacity(targets.len());
    for target in &targets {
        if !catalog
            .lineage(&target.key.target.object_type)
            .contains(&entry.object_type)
        {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "a link target has a type outside the admitted target type".to_owned(),
            });
        }
        let state = load_action_fact(
            host,
            catalog,
            call,
            effective_input,
            authorizer,
            &entry.name,
            &entry.object_type,
            &target.key.target,
            depth,
            facts,
            meter,
            observed_records,
            limits,
            read_types,
            SliceReadMode::Full,
        )?;
        let concrete_key = known_fact_key(facts, catalog, &target.key.target);
        precondition_keys.insert(concrete_key.clone());
        let Some(object) = state.values.as_ref() else {
            return Err(ActionRefusal::SliceObjectMissing {
                entry: entry.name.clone(),
            });
        };
        items.push(RuntimeValue::Object(object.clone()));
        item_keys.push(concrete_key);
    }
    record_action_link_read(link_reads, source_key, link, targets)?;
    values.insert(entry.name.clone(), RuntimeValue::Array(items));
    entry_keys.insert(entry.name.clone(), None);
    entry_list_keys.insert(entry.name.clone(), item_keys);
    entry_depths.insert(entry.name.clone(), depth);
    Ok(())
}

fn record_action_link_read<X>(
    link_reads: &mut Vec<LinkReadExpectation>,
    source_key: &FactKey,
    link: &str,
    targets: Vec<crate::cell_host::LinkReadTarget>,
) -> Result<(), ActionRefusal<X>> {
    if let Some(previous) = link_reads
        .iter()
        .find(|read: &&LinkReadExpectation| read.source == *source_key && read.link == link)
    {
        if previous.targets != targets {
            return Err(ActionRefusal::StalePreconditionRead {
                key: source_key.clone(),
                link: Some(link.to_owned()),
            });
        }
    } else {
        link_reads.push(LinkReadExpectation {
            source: source_key.clone(),
            link: link.to_owned(),
            targets,
        });
    }
    Ok(())
}

fn action_link_snapshot<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    source_key: &FactKey,
    link: &str,
    entry_name: &str,
    limits: &crate::SliceLimits,
    observed_records: u32,
    read_bytes: u64,
) -> Result<Vec<crate::cell_host::LinkReadTarget>, ActionRefusal<H::Error>> {
    let host = context.host;
    let catalog = context.catalog;
    let max_records = usize::try_from(limits.max_records).unwrap_or(usize::MAX);
    let remaining_records =
        max_records.saturating_sub(usize::try_from(observed_records).unwrap_or(usize::MAX));
    let max_targets = remaining_records.saturating_add(1);
    let max_key_bytes = limits.max_canonical_bytes.saturating_sub(read_bytes);
    let targets = host
        .read_link_targets(source_key, link, max_targets, max_key_bytes)
        .map_err(host_failure)?
        .ok_or_else(|| ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: "the host cannot provide a complete bounded link snapshot".to_owned(),
        })?;
    if targets.len() > remaining_records {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!(
                "link target count exceeds the remaining {remaining_records} slice records"
            ),
        });
    }
    let model = crate::CommitModel::new(catalog);
    let invalid_order = targets.windows(2).any(|pair| {
        matches!(pair, [left, right] if model.canonical_link(&left.key) >= model.canonical_link(&right.key))
    });
    let invalid_identity = targets
        .iter()
        .any(|target| target.key.source != *source_key || target.key.link != link);
    if invalid_order || invalid_identity {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: "the host returned a non-canonical link target snapshot".to_owned(),
        });
    }
    Ok(targets)
}

/// Loads and authorizes the action's declared slice under the resource profile.
/// Every distinct fact snapshot is retained once, and duplicate aliases must
/// observe the same version and values.
pub(super) fn load_action_slice<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    action_target: &FactKey,
    target_state: &FactState,
    meter: SliceMeter,
) -> Result<LoadedActionSlice, ActionRefusal<H::Error>> {
    let catalog = context.catalog;
    let call = context.call;
    let limits = ResourceLimitProfile::norn_limits_v1().slice;
    let reserved_bytes = meter.bytes();
    let policy_footprint = context.authorizer.action_policy_read_footprint(call.action);
    let tracked_reads = TrackedReadSet::prepare(
        context.host,
        catalog,
        call.action,
        policy_footprint,
        action_target,
        target_state.exists(),
        context.effective_input,
    );
    // Keep the separately authorized target snapshot out of the slice cache
    // until declared entries have passed their read-type gates. If an entry
    // aliases the target, it must be loaded through the same bounded read and
    // object authorization path as every other slice fact.
    let facts = FactSlice::new();
    let values = JsonObject::new();
    let link_reads = Vec::new();
    let precondition_keys = BTreeSet::new();
    let entry_keys = BTreeMap::<String, Option<FactKey>>::new();
    let entry_list_keys = BTreeMap::<String, Vec<FactKey>>::new();
    let entry_depths = BTreeMap::<String, u32>::new();
    let parameter_pins = Vec::new();
    let observed_records = meter.records();
    let read_types = BTreeSet::<ObjectTypeName>::new();
    let mut state = ActionSliceState {
        facts,
        values,
        link_reads,
        predicate_snapshots: Vec::new(),
        precondition_keys,
        entry_keys,
        entry_list_keys,
        entry_depths,
        parameter_pins,
        observed_records,
        meter,
        read_types,
        limits,
    };
    for (index, entry) in call.action.slice.iter().enumerate() {
        load_action_slice_entry(context, &mut state, entry, tracked_reads.slice_mode(index))?;
    }
    let aggregates = load_action_aggregates(
        context,
        &mut state.read_types,
        &mut state.meter,
        &state.limits,
        &mut state.predicate_snapshots,
    )?;
    let predicate_reads = super::predicate::predicate_read_expectations(state.predicate_snapshots);
    let ActionSliceState {
        mut facts,
        values,
        link_reads,
        precondition_keys,
        parameter_pins,
        limits,
        ..
    } = state;
    reconcile_action_target_snapshot::<H::Error>(catalog, &mut facts, action_target, target_state)?;

    let actual_bytes = norn_ir::canonical::canonical_json_of(&values)
        .map_err(|error| ActionRefusal::SliceUnavailable {
            entry: "$slice".to_owned(),
            reason: error.to_string(),
        })?
        .len();
    // An undeclared slice contributes no materialized input; do not charge
    // the empty object's braces against existing owned-row-only calls.
    let combined_bytes = reserved_bytes.saturating_add(if call.action.slice.is_empty() {
        0
    } else {
        u64::try_from(actual_bytes).unwrap_or(u64::MAX)
    });
    if combined_bytes > limits.max_canonical_bytes {
        return Err(ActionRefusal::SliceUnavailable {
            entry: "$slice".to_owned(),
            reason: format!(
                "combined canonical slice size {combined_bytes} exceeds {} bytes",
                limits.max_canonical_bytes
            ),
        });
    }
    Ok(LoadedActionSlice {
        facts,
        tracked_reads,
        values,
        link_reads,
        precondition_keys,
        aggregates,
        predicate_reads,
        parameter_pins,
    })
}

/// #1105: fold each declared aggregate over a complete, bounded predicate
/// read of its type. Every member passes the same object read authorization
/// as a slice read; an unreadable member refuses rather than silently
/// shrinking the fold. The host reads under the slice bytes still unspent, and
/// each member is then charged to the slice meter like any other record, so
/// an aggregate cannot outgrow the slice's byte bound. The returned reads pin
/// the members for the commit.
fn load_action_aggregates<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    read_types: &mut BTreeSet<ObjectTypeName>,
    meter: &mut SliceMeter,
    limits: &crate::SliceLimits,
    snapshots: &mut Vec<(PredicateQuery, Vec<PredicateObject>)>,
) -> Result<JsonObject, ActionRefusal<H::Error>> {
    let ActionSliceContext {
        catalog,
        call,
        effective_input,
        authorizer,
        ..
    } = *context;
    let mut values = JsonObject::new();
    // Aggregates over the same predicate (or a predicate slice entry) fold one
    // snapshot: the commit pins each distinct predicate once.
    for aggregate in &call.action.aggregates {
        let unavailable = |reason: String| ActionRefusal::AggregateUnavailable {
            aggregate: aggregate.name.clone(),
            reason,
        };
        if read_types.insert(aggregate.object_type.clone()) {
            authorize_slice_type(call, effective_input, authorizer, &aggregate.object_type)?;
        }
        let predicate =
            crate::action_aggregate::predicate_query(aggregate, effective_input, catalog)
                .map_err(unavailable)?;
        let max_items = usize::try_from(aggregate.max_items).unwrap_or(usize::MAX);
        let objects = super::predicate::load_aggregate_predicate_objects(
            context, aggregate, &predicate, max_items, meter, limits, snapshots,
        )?;
        let value = norn_decisions::fold_aggregate(
            &aggregate.aggregate,
            objects.iter().map(|object| &object.values),
        )
        .map_err(|error| unavailable(error.to_string()))?;
        values.insert(aggregate.name.clone(), value);
    }
    Ok(values)
}

/// Reconciles a declared slice read of the action target with the separately
/// authorized target snapshot, preserving both type-gated reads and CAS.
pub(super) fn reconcile_action_target_snapshot<X>(
    catalog: &TypeCatalog<'_>,
    facts: &mut FactSlice,
    action_target: &FactKey,
    target_state: &FactState,
) -> Result<(), ActionRefusal<X>> {
    let model = crate::CommitModel::new(catalog);
    let target_identity = model.canonical(action_target);
    let same_target = |key: &FactKey| model.canonical(key) == target_identity;
    if let Some((_, observed)) = facts.iter().find(|(key, _)| same_target(key)) {
        if observed.version != target_state.version || observed.values != target_state.values {
            return Err(ActionRefusal::StalePreconditionRead {
                key: action_target.clone(),
                link: None,
            });
        }
        // Keep the concrete, authorized slice identity and snapshot in the
        // prepared fact cache. Its read version is also recorded as a
        // precondition read by the declared entry.
    } else {
        // The action target still participates in decision preparation and
        // write CAS even when the declared slice does not read it.
        facts.insert(action_target.clone(), target_state.clone());
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the read is bounded, authorized, and inserted into one prepared slice"
)]
fn load_action_fact<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    entry_name: &str,
    declared_type: &ObjectTypeName,
    requested: &FactKey,
    relation_depth: u32,
    facts: &mut FactSlice,
    meter: &mut SliceMeter,
    observed_records: &mut u32,
    limits: &crate::SliceLimits,
    read_types: &mut BTreeSet<ObjectTypeName>,
    read_mode: SliceReadMode<'_>,
) -> Result<FactState, ActionRefusal<H::Error>> {
    if !read_mode.is_full()
        && !authorizer
            .action_policy_read_footprint(call.action)
            .is_exact()
    {
        return Err(ActionRefusal::TargetNotFound);
    }
    if relation_depth > limits.max_relation_depth {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!(
                "relation depth {relation_depth} exceeds {}",
                limits.max_relation_depth
            ),
        });
    }
    if read_types.insert(declared_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, declared_type)?;
    }
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(requested);
    if let Some((previous_key, previous_state)) = facts
        .iter()
        .find(|(key, _)| model.canonical(key) == canonical)
    {
        // Reuse the first authorized snapshot for aliases of the same fact.
        // A single CAS identity must never acquire a later alias's newer
        // version or values while the action is still being prepared.
        if !catalog
            .lineage(&previous_key.object_type)
            .contains(&requested.object_type)
        {
            return Err(ActionRefusal::TargetNotFound);
        }
        if authorizer
            .authorize_read(&ActionReadAuthzRequest {
                action: call.action,
                scope: &call.scope,
                actor: &call.actor,
                input: effective_input,
                declared_type,
                target: previous_key,
                target_values: previous_state.values.as_ref(),
            })
            .is_err()
        {
            return Err(ActionRefusal::TargetNotFound);
        }
        admit_action_fact_record::<H::Error>(
            previous_state.values.as_ref(),
            meter,
            observed_records,
            relation_depth,
            limits,
            entry_name,
        )?;
        return Ok(previous_state.clone());
    }
    if meter.records() >= limits.max_records {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!("the slice exceeds {} object records", limits.max_records),
        });
    }
    let remaining_bytes = limits.max_canonical_bytes.saturating_sub(meter.bytes());
    let bounded = read_mode
        .load(host, requested, remaining_bytes)
        .map_err(host_failure)?
        // Unsupported and over-bound snapshots are indistinguishable from an
        // unreadable object. Do not expose row existence or size before
        // object-level authorization can run.
        .ok_or(ActionRefusal::TargetNotFound)?;
    let fact_key = bounded.concrete_type.as_ref().map_or_else(
        || requested.clone(),
        |kind| FactKey::new(kind.as_str(), requested.key.clone()),
    );
    if let Some(concrete) = &bounded.concrete_type
        && !catalog.lineage(concrete).contains(&requested.object_type)
    {
        return Err(ActionRefusal::TargetNotFound);
    }
    let target_values = bounded.state.values.as_ref();
    if let Err(_denial) = authorizer.authorize_read(&ActionReadAuthzRequest {
        action: call.action,
        scope: &call.scope,
        actor: &call.actor,
        input: effective_input,
        declared_type,
        target: &fact_key,
        target_values,
    }) {
        // Read denial and invisibility share the same response so the action
        // cannot probe protected object identities or values.
        return Err(ActionRefusal::TargetNotFound);
    }
    admit_action_fact_record::<H::Error>(
        bounded.state.values.as_ref(),
        meter,
        observed_records,
        relation_depth,
        limits,
        entry_name,
    )?;
    facts.insert(fact_key, bounded.state.clone());
    Ok(bounded.state)
}

pub(super) fn admit_action_fact_record<X>(
    values: Option<&JsonObject>,
    meter: &mut SliceMeter,
    observed_records: &mut u32,
    relation_depth: u32,
    limits: &crate::SliceLimits,
    entry_name: &str,
) -> Result<(), ActionRefusal<X>> {
    let Some(values) = values else {
        return Ok(());
    };
    let bytes = norn_ir::canonical::canonical_json_of(values)
        .map_err(|error| ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: error.to_string(),
        })?
        .len();
    meter
        .admit_record(u64::try_from(bytes).unwrap_or(u64::MAX), relation_depth)
        .map_err(|error| ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: error.to_string(),
        })?;
    *observed_records = (*observed_records).saturating_add(1);
    if *observed_records > limits.max_records {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!("the slice exceeds {} object records", limits.max_records),
        });
    }
    Ok(())
}

fn known_fact_key(facts: &FactSlice, catalog: &TypeCatalog<'_>, requested: &FactKey) -> FactKey {
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(requested);
    facts
        .keys()
        .find(|key| model.canonical(key) == canonical)
        .cloned()
        .unwrap_or_else(|| requested.clone())
}

pub(super) fn authorize_slice_type<X>(
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    object_type: &ObjectTypeName,
) -> Result<(), ActionRefusal<X>> {
    if authorizer
        .authorize_read_type(&ActionReadTypeAuthzRequest {
            action: call.action,
            scope: &call.scope,
            actor: &call.actor,
            input: effective_input,
            object_type,
        })
        .is_err()
    {
        return Err(ActionRefusal::TargetNotFound);
    }
    Ok(())
}

/// Evaluates every declared action precondition over the derived input and the
/// authorized slice, carrying forward the derivations' remaining fuel.
pub(super) fn check_action_preconditions<X>(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    input: &JsonObject,
    slice: &LoadedActionSlice,
    remaining_fuel: &mut TurnFuel,
    receipt_evidence_bytes: &mut usize,
    max_receipt_bytes: usize,
) -> Result<Vec<ActionPreconditionOutcome>, ActionRefusal<X>> {
    let aggregates = (!action.aggregates.is_empty()).then_some(&slice.aggregates);
    let context =
        action_expression_context(input, &slice.values, aggregates).map_err(|reason| {
            ActionRefusal::PreconditionFailed {
                precondition: action
                    .preconditions
                    .first()
                    .map_or_else(String::new, |expression| expression.as_str().to_owned()),
                reason: Some(reason),
            }
        })?;
    let catalog_env = action
        .slice
        .first()
        .map(|entry| &entry.object_type)
        .or_else(|| {
            action
                .aggregates
                .first()
                .map(|aggregate| &aggregate.object_type)
        })
        .map(|object_type| catalog.expression_environment(object_type))
        .transpose()
        .map_err(|reason| ActionRefusal::PreconditionFailed {
            precondition: String::new(),
            reason: Some(reason),
        })?;
    let env =
        norn_expr::schema::action_environment(action, catalog_env.as_ref(), action.derive.len())
            .map_err(|reason| ActionRefusal::PreconditionFailed {
                precondition: String::new(),
                reason: Some(reason),
            })?;
    let mut outcomes = Vec::with_capacity(action.preconditions.len());
    for precondition in &action.preconditions {
        let source = precondition.as_str();
        let refusal = |reason| ActionRefusal::PreconditionFailed {
            precondition: source.to_owned(),
            reason,
        };
        let expression =
            Expression::compile(source).map_err(|error| refusal(Some(error.to_string())))?;
        let attempt = remaining_fuel.spend(|budget| {
            expression.evaluate_admitted_metered(&context, budget, &env, &norn_expr::Type::Bool)
        });
        let evaluated = attempt
            .result
            .map_err(|error| refusal(Some(error.to_string())))?;
        match evaluated.value() {
            ExprValue::Bool(true) => {
                let encoded =
                    norn_ir::canonical::canonical_json_of(&ActionPreconditionOutcomeRef {
                        expression: source,
                        satisfied: true,
                    })
                    .map_err(|error| refusal(Some(error.to_string())))?;
                let overhead = if outcomes.is_empty() { 24 } else { 1 };
                let next_bytes = (*receipt_evidence_bytes)
                    .saturating_add(encoded.len())
                    .saturating_add(overhead);
                if next_bytes > max_receipt_bytes {
                    return Err(ActionRefusal::InputBoundExceeded {
                        subject: "action call receipt evidence".to_owned(),
                        actual_bytes: next_bytes,
                        max_bytes: max_receipt_bytes,
                    });
                }
                *receipt_evidence_bytes = next_bytes;
                outcomes.push(ActionPreconditionOutcome {
                    expression: source.to_owned(),
                    satisfied: true,
                });
            }
            ExprValue::Bool(false) => return Err(refusal(None)),
            ExprValue::Null
            | ExprValue::Number(_)
            | ExprValue::String(_)
            | ExprValue::Array(_)
            | ExprValue::Object(_) => {
                return Err(refusal(Some(
                    "the expression did not evaluate to a boolean".to_owned(),
                )));
            }
        }
    }
    Ok(outcomes)
}
