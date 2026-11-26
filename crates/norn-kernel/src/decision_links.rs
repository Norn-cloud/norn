//! Checked link mutations emitted on [`norn_ir::DecisionOutput::links`] (#1586).
//!
//! Validates every emitted [`LinkChange`] against the cell's [`TypeCatalog`],
//! live or same-commit created endpoints, envelope bounds, authorization, and
//! canonical uniqueness, then loads current link versions from [`CellHost`] so
//! [`crate::check_commit`] enforces endpoints, link values, cardinality, and
//! required links atomically in one commit.

use std::collections::BTreeSet;

use norn_ir::{DecisionOutput, LinkChange, ObjectTypeName};

use crate::action_commit::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionReadAuthzRequest,
    ActionRefusal,
};
use crate::cell_host::{
    CellCommit, CellHost, FactKey, FactMutation, FactState, FactWrite, JsonObject, JsonValue,
    LinkKey, LinkMutation, LinkWrite,
};
use crate::commit_check::CommitModel;
use crate::decider::EnvelopeKind;
use crate::decision_commit::{
    DecisionRefusal, DecisionTurn, EffectiveValuesError, FactEvent, FactSlice, TypeCatalog,
    prepare_decision_with_links_validated,
};
use crate::effect::{Command, Effect, Timer};
use crate::envelope::{EnvelopeCounts, TurnEnvelopeMeter};
use crate::limits::{EnvelopeLimits, check_envelope_bytes};

/// Reject unknown or computed keys in submitted link property values before
/// effective-value materialization.
pub(crate) fn check_submitted_link_keys(
    properties: &[norn_checker::EffectiveProperty],
    submitted: &JsonObject,
) -> Result<(), EffectiveValuesError> {
    for key in submitted.keys() {
        match properties
            .iter()
            .find(|property| property.id.as_str() == key)
        {
            Some(property) if !property.computed => {}
            Some(_) => {
                return Err(EffectiveValuesError::ValueType {
                    property: key.clone(),
                    expected: "non-computed link property",
                });
            }
            None => {
                return Err(EffectiveValuesError::ValueType {
                    property: key.clone(),
                    expected: "declared link property",
                });
            }
        }
    }
    Ok(())
}

/// Pure validation of [`norn_ir::DecisionOutput::links`] during decision
/// preparation (before any host write).
pub(crate) fn validate_decision_links<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    output: &DecisionOutput,
    event_count: usize,
    limits: &EnvelopeLimits,
) -> Result<(), DecisionRefusal<X>> {
    let links = &output.links;
    if links.is_empty() {
        return Ok(());
    }
    let max_events = usize::try_from(limits.max_events).unwrap_or(usize::MAX);
    let total_events = event_count.saturating_add(links.len());
    if total_events > max_events {
        return Err(DecisionRefusal::EnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: limits.max_events,
            actual: total_events,
        });
    }
    let output_bytes = canonical_len(output)?;
    check_envelope_bytes(limits, output_bytes)
        .map_err(|exceeded| invalid_link(exceeded.to_string()))?;

    let model = CommitModel::new(catalog);
    let mut seen = BTreeSet::new();
    let mut materialized_links = Vec::with_capacity(links.len());
    for change in links {
        let (source, link_name, target, add_values) = unpack_link_change(change);
        if source.key.trim().is_empty() || target.key.trim().is_empty() {
            return Err(invalid_link("link endpoint keys must be non-empty"));
        }
        if is_protected_endpoint(catalog, &source.object_type)
            || is_protected_endpoint(catalog, &target.object_type)
            || is_owned_collection_link(catalog, &source.object_type, link_name)
        {
            return Err(invalid_link(
                "decision links cannot target system, owned-child, or owned-collection links",
            ));
        }
        let declared = model.link(&source.object_type, link_name).ok_or_else(|| {
            invalid_link(format!(
                "undeclared link {link_name:?} on {:?}",
                source.object_type.as_str()
            ))
        })?;
        if !model.is_a(&target.object_type, &declared.target) {
            return Err(invalid_link(format!(
                "link {link_name:?} targets {:?}, not {:?}",
                declared.target.as_str(),
                target.object_type.as_str()
            )));
        }
        let canonical =
            model.canonical_link(&LinkKey::new(source.clone(), link_name, target.clone()));
        if !seen.insert(canonical) {
            return Err(invalid_link(format!(
                "duplicate link change for {source} --{link_name}--> {target}"
            )));
        }
        let link_key = LinkKey::new(source, link_name, target);
        let materialized =
            validate_link_endpoints(catalog, model, slice, writes, &link_key, change, add_values)?;
        materialized_links.push(materialized);
    }
    let materialized_output = DecisionOutput {
        facts: output.facts.clone(),
        links: materialized_links,
        commands: output.commands.clone(),
        timers: output.timers.clone(),
    };
    let materialized_bytes = canonical_len(&materialized_output)?;
    check_envelope_bytes(limits, materialized_bytes)
        .map_err(|exceeded| invalid_link(exceeded.to_string()))?;
    Ok(())
}

fn validate_link_endpoints<X>(
    catalog: &TypeCatalog<'_>,
    model: CommitModel<'_, '_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    link: &LinkKey,
    change: &LinkChange,
    add_values: Option<&JsonObject>,
) -> Result<LinkChange, DecisionRefusal<X>> {
    let source = &link.source;
    let target = &link.target;
    let link_name = link.link.as_str();
    let allow_deleted = add_values.is_none();
    let source_concrete = captured_concrete(model, slice, writes, source, allow_deleted)
        .ok_or_else(|| {
            invalid_link(format!(
                "link source {source} does not exist in the slice or proposed writes"
            ))
        })?;
    let target_concrete = captured_concrete(model, slice, writes, target, allow_deleted)
        .ok_or_else(|| {
            invalid_link(format!(
                "link target {target} does not exist in the slice or proposed writes"
            ))
        })?;
    if !model.is_a(&source_concrete, &source.object_type)
        || !model.is_a(&target_concrete, &target.object_type)
    {
        return Err(invalid_link(
            "link endpoint concrete type does not match the declared link change type",
        ));
    }
    if is_protected_endpoint(catalog, &source_concrete)
        || is_protected_endpoint(catalog, &target_concrete)
        || is_owned_collection_link(catalog, &source_concrete, link_name)
    {
        return Err(invalid_link(
            "decision links cannot target system, owned-child, or owned-collection links",
        ));
    }
    let effective_link = model.link(&source_concrete, link_name).ok_or_else(|| {
        invalid_link(format!(
            "undeclared link {link_name:?} on concrete source {:?}",
            source_concrete.as_str()
        ))
    })?;
    if !model.is_a(&target_concrete, &effective_link.target) {
        return Err(invalid_link(format!(
            "concrete target {:?} is not admitted by narrowed link {link_name:?} target {:?}",
            target_concrete.as_str(),
            effective_link.target.as_str()
        )));
    }
    if let (
        Some(values),
        LinkChange::Add {
            source_type,
            source_key,
            link,
            target_type,
            target_key,
            ..
        },
    ) = (add_values, change)
    {
        let effective = catalog
            .effective_link_values(&source_concrete, link_name, values)
            .map_err(|error| invalid_link(format!("link {link_name:?}: {error}")))?;
        return Ok(LinkChange::Add {
            source_type: source_type.clone(),
            source_key: source_key.clone(),
            link: link.clone(),
            target_type: target_type.clone(),
            target_key: target_key.clone(),
            values: effective,
        });
    }
    Ok(change.clone())
}

/// Resolve [`LinkChange`]s against the host's current link versions to produce
/// CAS-checked [`LinkWrite`] entries for [`crate::CellCommit::links`].
pub(crate) fn prepare_decision_link_writes<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    links: &[LinkChange],
) -> Result<Vec<LinkWrite>, DecisionRefusal<H::Error>> {
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let model = CommitModel::new(catalog);
    let mut prepared = Vec::with_capacity(links.len());
    for change in links {
        let (source, link_name, target, add_values) = unpack_link_change(change);
        let source_key = resolved_endpoint_key(model, slice, writes, &source);
        let target_key = resolved_endpoint_key(model, slice, writes, &target);
        let key = LinkKey::new(source_key, link_name, target_key);
        let expected = host.load_link(&key).map_err(DecisionRefusal::Host)?.version;
        let mutation = match add_values {
            Some(values) => {
                let effective = catalog
                    .effective_link_values(&key.source.object_type, link_name, values)
                    .map_err(|error| invalid_link(format!("link {link_name:?}: {error}")))?;
                LinkMutation::Add { values: effective }
            }
            None => LinkMutation::Remove,
        };
        prepared.push(LinkWrite {
            key,
            expected,
            mutation,
        });
    }
    Ok(prepared)
}

/// Enforce fail-closed action authorization and slice-capture bounds on links
/// emitted by a `synchronousRule` writer before committing.
pub(crate) fn authorize_action_decision_links<X>(
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    slice: &FactSlice,
    prepared_writes: &[FactWrite],
    links: &[LinkChange],
) -> Result<(), ActionRefusal<X>> {
    if links.is_empty() {
        return Ok(());
    }
    let model = CommitModel::new(catalog);
    for change in links {
        let (source, link_name, target, _) = unpack_link_change(change);
        for (is_source, endpoint) in [(true, &source), (false, &target)] {
            authorize_action_link_endpoint(
                LinkEndpointAuthz {
                    catalog,
                    model,
                    call,
                    effective_input,
                    authorizer,
                    slice,
                    prepared_writes,
                },
                endpoint,
                link_name,
                is_source,
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct LinkEndpointAuthz<'a> {
    catalog: &'a TypeCatalog<'a>,
    model: CommitModel<'a, 'a>,
    call: &'a ActionCall<'a>,
    effective_input: &'a JsonObject,
    authorizer: &'a dyn ActionAuthorizer,
    slice: &'a FactSlice,
    prepared_writes: &'a [FactWrite],
}

fn authorize_action_link_endpoint<X>(
    ctx: LinkEndpointAuthz<'_>,
    endpoint: &FactKey,
    link_name: &str,
    is_source: bool,
) -> Result<(), ActionRefusal<X>> {
    let LinkEndpointAuthz {
        catalog,
        model,
        call,
        effective_input,
        authorizer,
        slice,
        prepared_writes,
    } = ctx;
    let is_forbidden_type = |ty: &ObjectTypeName| {
        is_protected_endpoint(catalog, ty)
            || (is_source && is_owned_collection_link(catalog, ty, link_name))
    };
    if is_forbidden_type(&endpoint.object_type) {
        return Err(crate::synchronous_rule::refused(
            "a synchronous rule link cannot target system, owned-child, or owned-collection links",
        ));
    }
    let canonical = model.canonical(endpoint);
    if let Some(write) = prepared_writes.iter().find(|write| {
        model.canonical(&write.key) == canonical
            && matches!(write.mutation, FactMutation::Put { .. })
    }) {
        if !model.is_a(&write.key.object_type, &endpoint.object_type) {
            return Err(crate::synchronous_rule::refused(
                "a decision link endpoint concrete type does not match the declared type",
            ));
        }
        if is_forbidden_type(&write.key.object_type) {
            return Err(crate::synchronous_rule::refused(
                "a synchronous rule link cannot target system, owned-child, or owned-collection links",
            ));
        }
        // Written endpoints are authorized via `authorize_proposed_target`.
        return Ok(());
    }
    let (slice_key, slice_state) = slice
        .iter()
        .find(|(key, state)| model.canonical(key) == canonical && state.exists())
        .ok_or_else(|| {
            crate::synchronous_rule::refused(
                "a decision link endpoint is outside the captured slice or proposed writes",
            )
        })?;
    if !model.is_a(&slice_key.object_type, &endpoint.object_type) {
        return Err(crate::synchronous_rule::refused(
            "a decision link endpoint concrete type does not match the declared type",
        ));
    }
    if is_forbidden_type(&slice_key.object_type) {
        return Err(crate::synchronous_rule::refused(
            "a synchronous rule link cannot target system, owned-child, or owned-collection links",
        ));
    }
    authorizer
        .authorize_read(&ActionReadAuthzRequest {
            action: call.action,
            scope: &call.scope,
            actor: &call.actor,
            input: effective_input,
            declared_type: &endpoint.object_type,
            target: slice_key,
            target_values: slice_state.values.as_ref(),
        })
        .map_err(|_denied| ActionRefusal::TargetNotFound)?;
    if is_source {
        // Outgoing links share the source's write authority, not read-only access.
        match authorizer.authorize(&ActionAuthzRequest {
            action: call.action,
            scope: &call.scope,
            actor: &call.actor,
            input: effective_input,
            target: slice_key,
            target_values: slice_state.values.as_ref(),
        }) {
            Ok(()) => {}
            Err(ActionDenial::Invisible) => return Err(ActionRefusal::TargetNotFound),
            Err(ActionDenial::Forbidden { reason }) => {
                return Err(ActionRefusal::PermissionDenied { reason });
            }
        }
    }
    Ok(())
}

/// Merge static `createLinks` and dynamic `DecisionOutput.links` writes,
/// refusing duplicates and enforcing the envelope bound.
pub(crate) fn merge_action_link_writes<X>(
    catalog: &TypeCatalog<'_>,
    mut create_links: Vec<LinkWrite>,
    decision_links: Vec<LinkWrite>,
    limits: &EnvelopeLimits,
) -> Result<Vec<LinkWrite>, ActionRefusal<X>> {
    if decision_links.is_empty() {
        return Ok(create_links);
    }
    let max_links = usize::try_from(limits.max_events).unwrap_or(usize::MAX);
    let total = create_links.len().saturating_add(decision_links.len());
    if total > max_links {
        return Err(ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: limits.max_events,
            actual: total,
        }));
    }
    let model = CommitModel::new(catalog);
    let mut seen: BTreeSet<LinkKey> = create_links
        .iter()
        .map(|write| model.canonical_link(&write.key))
        .collect();
    for write in decision_links {
        if !seen.insert(model.canonical_link(&write.key)) {
            return Err(crate::synchronous_rule::refused(
                "duplicate link across createLinks and decision output",
            ));
        }
        create_links.push(write);
    }
    Ok(create_links)
}

fn unpack_link_change(change: &LinkChange) -> (FactKey, &str, FactKey, Option<&JsonObject>) {
    match change {
        LinkChange::Add {
            source_type,
            source_key,
            link,
            target_type,
            target_key,
            values,
        } => (
            FactKey::new(source_type.as_str(), source_key.clone()),
            link.as_str(),
            FactKey::new(target_type.as_str(), target_key.clone()),
            Some(values),
        ),
        LinkChange::Remove {
            source_type,
            source_key,
            link,
            target_type,
            target_key,
        } => (
            FactKey::new(source_type.as_str(), source_key.clone()),
            link.as_str(),
            FactKey::new(target_type.as_str(), target_key.clone()),
            None,
        ),
    }
}

fn is_protected_endpoint(catalog: &TypeCatalog<'_>, object_type: &ObjectTypeName) -> bool {
    object_type.as_str().starts_with("norn.") || catalog.is_owned_child_type(object_type)
}

fn is_owned_collection_link(
    catalog: &TypeCatalog<'_>,
    source_type: &ObjectTypeName,
    link_name: &str,
) -> bool {
    catalog
        .owned_collections(source_type)
        .iter()
        .any(|collection| collection.id.as_str() == link_name)
}

fn canonical_len<T: serde::Serialize, X>(value: &T) -> Result<u64, DecisionRefusal<X>> {
    let bytes = norn_ir::canonical::canonical_json_of(value)
        .map_err(|error| invalid_link(format!("cannot encode decision link payload: {error}")))?
        .len();
    Ok(u64::try_from(bytes).unwrap_or(u64::MAX))
}

fn captured_concrete(
    model: CommitModel<'_, '_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    key: &FactKey,
    allow_deleted: bool,
) -> Option<ObjectTypeName> {
    let canonical = model.canonical(key);
    if let Some(write) = writes
        .iter()
        .find(|write| model.canonical(&write.key) == canonical)
    {
        return match write.mutation {
            FactMutation::Put { .. } => Some(write.key.object_type.clone()),
            FactMutation::Delete if allow_deleted => slice
                .iter()
                .find(|(slice_key, state)| {
                    model.canonical(slice_key) == canonical && state.exists()
                })
                .map(|(slice_key, _)| slice_key.object_type.clone()),
            FactMutation::Delete => None,
        };
    }
    slice
        .iter()
        .find(|(slice_key, state)| model.canonical(slice_key) == canonical && state.exists())
        .map(|(slice_key, _)| slice_key.object_type.clone())
}

fn resolved_endpoint_key(
    model: CommitModel<'_, '_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    key: &FactKey,
) -> FactKey {
    let canonical = model.canonical(key);
    if let Some(write) = writes
        .iter()
        .find(|write| model.canonical(&write.key) == canonical)
    {
        return write.key.clone();
    }
    if let Some((slice_key, _)) = slice
        .iter()
        .find(|(slice_key, state)| model.canonical(slice_key) == canonical && state.exists())
    {
        return slice_key.clone();
    }
    key.clone()
}

fn invalid_link<X>(reason: impl Into<String>) -> DecisionRefusal<X> {
    DecisionRefusal::InvalidOutput {
        reason: reason.into(),
    }
}

pub(crate) fn widen_effect<C: From<JsonObject>, T: From<JsonObject>>(
    effect: Effect<JsonObject, JsonObject>,
) -> Effect<C, T> {
    match effect {
        Effect::Perform(command) => Effect::Perform(Command {
            id: command.id,
            generation: command.generation,
            idempotency_key: command.idempotency_key,
            freshness: command.freshness,
            deadline: command.deadline,
            payload: command.payload.into(),
        }),
        Effect::Schedule(timer) => Effect::Schedule(Timer {
            id: timer.id,
            due: timer.due,
            payload: timer.payload.into(),
        }),
        Effect::CancelTimer(id) => Effect::CancelTimer(id),
    }
}

/// Assemble a `CellCommit` from a validated `DecisionOutput` without loading
/// link versions (callers that admit links must materialize them via
/// [`decision_cell_commit_with_host`] or `finalize_decision_commit`).
pub(crate) fn build_decision_cell_commit<E, C, T, X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
) -> Result<CellCommit<E, C, T>, DecisionRefusal<X>>
where
    E: From<FactEvent>,
    C: From<JsonObject>,
    T: From<JsonObject>,
{
    let prepared = prepare_decision_with_links_validated(catalog, slice, turn, output, limits)?;
    Ok(CellCommit {
        consumes: prepared.consumes,
        reads: prepared.reads,
        writes: prepared.writes,
        events: prepared.events.into_iter().map(Into::into).collect(),
        effects: prepared.effects.into_iter().map(widen_effect).collect(),
        links: Vec::new(),
        link_reads: Vec::new(),
        predicate_reads: Vec::new(),
    })
}

/// A [`CellCommit`] specialized to the event, command, and input types of host `H`.
pub type HostCellCommit<H> =
    CellCommit<<H as CellHost>::Event, <H as CellHost>::Command, <H as CellHost>::Input>;

/// Build the complete [`CellCommit`] for a decision, including CAS-checked
/// [`LinkWrite`] entries resolved against `host` for any [`DecisionOutput::links`].
///
/// # Errors
///
/// A [`DecisionRefusal`] if preparation, link validation, or host link loading fails.
pub fn decision_cell_commit_with_host<H>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
) -> Result<HostCellCommit<H>, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let mut commit =
        build_decision_cell_commit::<_, _, _, H::Error>(catalog, slice, turn, output, limits)?;
    let _counts = finalize_decision_commit(host, catalog, slice, output, None, &mut commit)?;
    Ok(commit)
}

/// Build the complete [`CellCommit`] for a decision while enforcing and
/// charging the aggregate [`TurnEnvelopeMeter`] (including `commit.links`).
///
/// # Errors
///
/// A [`DecisionRefusal`] if preparation, link validation, host link loading, or
/// the aggregate turn envelope meter refuses; on error `meter` is unchanged.
pub fn decision_cell_commit_with_host_and_meter<H>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    meter: &mut TurnEnvelopeMeter,
) -> Result<HostCellCommit<H>, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let limits = meter.decision_limits();
    let mut commit =
        build_decision_cell_commit::<_, _, _, H::Error>(catalog, slice, turn, output, &limits)?;
    let counts = finalize_decision_commit(host, catalog, slice, output, Some(meter), &mut commit)?;
    meter.charge(counts);
    Ok(commit)
}

/// Attach resolved `DecisionOutput.links` to a prepared commit and verify the
/// shared turn envelope meter before submitting to the host.
pub(crate) fn finalize_decision_commit<H>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    output: &DecisionOutput,
    meter: Option<&TurnEnvelopeMeter>,
    commit: &mut CellCommit<H::Event, H::Command, H::Input>,
) -> Result<EnvelopeCounts, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
{
    commit.links =
        prepare_decision_link_writes(host, catalog, slice, &commit.writes, &output.links)?;
    let counts = EnvelopeCounts::new(
        commit.events.len().saturating_add(commit.links.len()),
        output.commands.len(),
        output.timers.len(),
    );
    if let Some(meter) = meter {
        meter
            .check(counts)
            .map_err(|exceeded| DecisionRefusal::TurnEnvelopeExceeded {
                kind: exceeded.kind,
                limit: exceeded.limit,
                actual: exceeded.actual,
            })?;
    }
    Ok(counts)
}

/// Inputs for authorizing and resolving all links of an atomic action commit.
pub(crate) struct ActionCommitLinks<'a> {
    pub(crate) catalog: &'a TypeCatalog<'a>,
    pub(crate) call: &'a ActionCall<'a>,
    pub(crate) effective_input: &'a JsonObject,
    pub(crate) authorizer: &'a dyn ActionAuthorizer,
    pub(crate) target: &'a FactKey,
    pub(crate) slice: &'a FactSlice,
    pub(crate) prepared_writes: &'a [FactWrite],
    pub(crate) decision_links: &'a [LinkChange],
    pub(crate) limits: &'a EnvelopeLimits,
}

fn resolve_written_endpoint(
    model: &CommitModel<'_, '_>,
    prepared_writes: &[FactWrite],
    declared: &FactKey,
) -> Option<FactKey> {
    let canonical = model.canonical(declared);
    prepared_writes.iter().find_map(|write| {
        (matches!(write.mutation, FactMutation::Put { .. })
            && model.canonical(&write.key) == canonical
            && model.is_a(&write.key.object_type, &declared.object_type))
        .then(|| write.key.clone())
    })
}

fn resolve_linked_target(
    model: &CommitModel<'_, '_>,
    slice: &FactSlice,
    prepared_writes: &[FactWrite],
    declared: &FactKey,
) -> Option<FactKey> {
    let canonical = model.canonical(declared);
    resolve_written_endpoint(model, prepared_writes, declared).or_else(|| {
        slice.iter().find_map(|(key, state)| {
            (state.exists()
                && model.canonical(key) == canonical
                && model.is_a(&key.object_type, &declared.object_type))
            .then(|| key.clone())
        })
    })
}

/// Authorize and resolve all action-created links (`createLinks` +
/// `synchronousRule` `DecisionOutput.links`) for one atomic action commit.
pub(crate) fn prepare_action_commit_links<H: CellHost + ?Sized>(
    host: &H,
    ctx: &ActionCommitLinks<'_>,
) -> Result<Vec<LinkWrite>, ActionRefusal<H::Error>> {
    let ActionCommitLinks {
        catalog,
        call,
        effective_input,
        authorizer,
        target,
        slice,
        prepared_writes,
        decision_links,
        limits,
    } = *ctx;
    authorize_action_decision_links(
        catalog,
        call,
        effective_input,
        authorizer,
        slice,
        prepared_writes,
        decision_links,
    )?;
    let model = CommitModel::new(catalog);
    let mut created_links = Vec::new();
    let mut seen_created = BTreeSet::new();
    let refused = || {
        crate::synchronous_rule::refused(
            "a link requires a written source and a captured or proposed target",
        )
    };
    for binding in &call.action.create_links {
        let declared_target = action_link_endpoint::<H::Error>(
            effective_input,
            &binding.target_type,
            &binding.target_id_field,
        )?;
        let declared_source = match &binding.source {
            Some(source) => action_link_endpoint::<H::Error>(
                effective_input,
                &source.object_type,
                &source.id_field,
            )?,
            None => target.clone(),
        };
        let source = resolve_written_endpoint(&model, prepared_writes, &declared_source)
            .ok_or_else(refused)?;
        let linked_target = resolve_linked_target(&model, slice, prepared_writes, &declared_target)
            .ok_or_else(refused)?;
        let key = LinkKey::new(source.clone(), &binding.link, linked_target.clone());
        if model
            .link(&source.object_type, &binding.link)
            .is_none_or(|link| !model.is_a(&linked_target.object_type, &link.target))
            || (binding.source.is_some() && call.action.synchronous_rule.is_none())
            || !seen_created.insert(model.canonical_link(&key))
        {
            return Err(refused());
        }
        let expected = host
            .load_link(&key)
            .map_err(|error| ActionRefusal::Decision(DecisionRefusal::Host(error)))?
            .version;
        created_links.push(LinkWrite {
            key,
            expected,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }
    let decision_link_writes =
        prepare_decision_link_writes(host, catalog, slice, prepared_writes, decision_links)?;
    merge_action_link_writes(catalog, created_links, decision_link_writes, limits)
}

/// Refuse hostless preparation calls when `output.links` is non-empty so
/// callers never receive a committable `CellCommit` that silently drops links.
pub(crate) fn reject_hostless_links<X>(output: &DecisionOutput) -> Result<(), DecisionRefusal<X>> {
    if output.links.is_empty() {
        return Ok(());
    }
    Err(invalid_link(
        "hostless decision preparation cannot materialize decision links; use commit_decision or decision_cell_commit_with_host",
    ))
}

fn action_link_endpoint<X>(
    input: &JsonObject,
    object_type: &str,
    field: &str,
) -> Result<FactKey, ActionRefusal<X>> {
    input
        .get(field)
        .and_then(JsonValue::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(|key| FactKey::new(object_type, key))
        .ok_or_else(|| ActionRefusal::InvalidInput {
            field: field.to_owned(),
        })
}
