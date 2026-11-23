//! Checked link mutations emitted on [`norn_ir::DecisionOutput::links`] (#1586).
//!
//! Validates every emitted [`LinkChange`] against the cell's [`TypeCatalog`],
//! live or same-commit created endpoints, envelope bounds, authorization, and
//! canonical uniqueness, then loads current link versions from [`CellHost`] so
//! [`crate::check_commit`] enforces endpoints, link values, cardinality, and
//! required links atomically in one commit.

use std::collections::BTreeSet;

use norn_ir::{DecisionOutput, LinkChange, ObjectTypeName};

use crate::action_commit::{ActionAuthorizer, ActionCall, ActionReadAuthzRequest, ActionRefusal};
use crate::cell_host::{
    CellCommit, CellHost, FactKey, FactMutation, FactState, FactWrite, JsonObject, JsonValue,
    LinkKey, LinkMutation, LinkWrite,
};
use crate::commit_check::CommitModel;
use crate::decider::EnvelopeKind;
use crate::decision_commit::{DecisionRefusal, FactSlice, TypeCatalog};
use crate::envelope::{EnvelopeCounts, TurnEnvelopeMeter};
use crate::limits::EnvelopeLimits;

/// Pure validation of [`norn_ir::DecisionOutput::links`] during decision
/// preparation (before any host write).
pub(crate) fn validate_decision_links<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    links: &[LinkChange],
    limits: &EnvelopeLimits,
) -> Result<(), DecisionRefusal<X>> {
    if links.is_empty() {
        return Ok(());
    }
    let max_links = usize::try_from(limits.max_events).unwrap_or(usize::MAX);
    if links.len() > max_links {
        return Err(DecisionRefusal::EnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: limits.max_events,
            actual: links.len(),
        });
    }
    let model = CommitModel::new(catalog);
    let mut seen = BTreeSet::new();
    for change in links {
        let (source, link_name, target, add_values) = unpack_link_change(change);
        if source.key.trim().is_empty() || target.key.trim().is_empty() {
            return Err(invalid_link("link endpoint keys must be non-empty"));
        }
        if is_protected_endpoint(catalog, &source.object_type)
            || is_protected_endpoint(catalog, &target.object_type)
        {
            return Err(invalid_link(
                "decision links cannot target system or owned-child object types",
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
        if let Some(values) = add_values {
            let link_key = LinkKey::new(source, link_name, target);
            validate_add_link_endpoints(catalog, model, slice, writes, &link_key, values)?;
        }
    }
    Ok(())
}

fn validate_add_link_endpoints<X>(
    catalog: &TypeCatalog<'_>,
    model: CommitModel<'_, '_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    link: &LinkKey,
    values: &JsonObject,
) -> Result<(), DecisionRefusal<X>> {
    let source = &link.source;
    let target = &link.target;
    let link_name = link.link.as_str();
    let source_concrete = live_concrete_after(model, slice, writes, source).ok_or_else(|| {
        invalid_link(format!(
            "link source {source} does not exist in the slice or proposed writes"
        ))
    })?;
    let target_concrete = live_concrete_after(model, slice, writes, target).ok_or_else(|| {
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
    catalog
        .effective_link_values(&source_concrete, link_name, values)
        .map_err(|error| invalid_link(format!("link {link_name:?}: {error}")))?;
    Ok(())
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
            Some(values) => LinkMutation::Add {
                values: values.clone(),
            },
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
        let (source, _, target, _) = unpack_link_change(change);
        for endpoint in [&source, &target] {
            if is_protected_endpoint(catalog, &endpoint.object_type) {
                return Err(crate::synchronous_rule::refused(
                    "a synchronous rule link cannot target system or owned-child types",
                ));
            }
            let canonical = model.canonical(endpoint);
            let is_written_put = prepared_writes.iter().any(|write| {
                model.canonical(&write.key) == canonical
                    && matches!(write.mutation, FactMutation::Put { .. })
            });
            if is_written_put {
                // Written endpoints are authorized via `authorize_proposed_target`.
                continue;
            }
            let (slice_key, slice_state) = slice
                .iter()
                .find(|(key, state)| model.canonical(key) == canonical && state.exists())
                .ok_or_else(|| {
                    crate::synchronous_rule::refused(
                        "a decision link endpoint is outside the captured slice or proposed writes",
                    )
                })?;
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

fn live_concrete_after(
    model: CommitModel<'_, '_>,
    slice: &FactSlice,
    writes: &[FactWrite],
    key: &FactKey,
) -> Option<ObjectTypeName> {
    let canonical = model.canonical(key);
    if let Some(write) = writes
        .iter()
        .find(|write| model.canonical(&write.key) == canonical)
    {
        return match write.mutation {
            FactMutation::Put { .. } => Some(write.key.object_type.clone()),
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
        commit.events.len(),
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
    let mut created_links = Vec::new();
    for binding in &call.action.create_links {
        let linked_target = action_link_endpoint::<H::Error>(
            effective_input,
            &binding.target_type,
            &binding.target_id_field,
        )?;
        let source = match &binding.source {
            Some(source) => action_link_endpoint::<H::Error>(
                effective_input,
                &source.object_type,
                &source.id_field,
            )?,
            None => target.clone(),
        };
        if CommitModel::new(catalog)
            .link(&source.object_type, &binding.link)
            .is_none_or(|link| link.target.as_str() != binding.target_type)
            || !prepared_writes.iter().any(|write| {
                write.key == source && matches!(write.mutation, FactMutation::Put { .. })
            })
            || !(slice.get(&linked_target).is_some_and(FactState::exists)
                || prepared_writes.iter().any(|write| {
                    write.key == linked_target && matches!(write.mutation, FactMutation::Put { .. })
                }))
            || (binding.source.is_some() && call.action.synchronous_rule.is_none())
            || !created_links.iter().all(|write: &LinkWrite| {
                write.key != LinkKey::new(source.clone(), &binding.link, linked_target.clone())
            })
        {
            return Err(crate::synchronous_rule::refused(
                "a link requires a written source and a captured or proposed target",
            ));
        }
        let key = LinkKey::new(source, &binding.link, linked_target);
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
