use super::PostState;
use super::pre::{MAX_LINK_READ_BYTES, MAX_LINK_READ_TARGETS};
use crate::cell_host::{CellCommit, CommitError, FactKey, FactMutation, LinkKey, LinkMutation};
use crate::commit_check::CommitView;
use crate::snapshot_observe::ObservationAuthorization;
use norn_ir::Multiplicity;
use std::collections::{BTreeMap, BTreeSet};
/// The canonical live links of every object whose deletes, incoming-one
/// cardinality, or required owned collections need full adjacency once the
/// commit's link writes apply.
pub(super) fn live_links_after<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
) -> Result<BTreeMap<FactKey, BTreeSet<LinkKey>>, CommitError<V::Error>> {
    let model = post.model();
    let mut objects = BTreeSet::new();
    let needs_owned_links = |ty: &norn_ir::ObjectTypeName| {
        model
            .catalog
            .owned_collections(ty)
            .iter()
            .any(|collection| {
                model
                    .link(ty, collection.id.as_str())
                    .is_some_and(|link| link.cardinality.required)
            })
    };
    for link in &commit.links {
        if post
            .concrete(&link.key.source)?
            .as_ref()
            .is_some_and(needs_owned_links)
        {
            objects.insert(model.canonical(&link.key.source));
        }
    }
    for write in &commit.writes {
        if matches!(write.mutation, FactMutation::Delete)
            || needs_owned_links(&write.key.object_type)
        {
            objects.insert(model.canonical(&write.key));
        }
    }
    let mut after = BTreeMap::new();
    for object in objects {
        let edges = post.incident(&object)?;
        after.insert(object, edges);
    }
    Ok(after)
}

pub(super) fn check_endpoints<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    links: &BTreeMap<FactKey, BTreeSet<LinkKey>>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    for write in &commit.links {
        if !matches!(write.mutation, LinkMutation::Add { .. }) {
            continue;
        }
        let mut concrete = Vec::with_capacity(2);
        for endpoint in [&write.key.source, &write.key.target] {
            let found = post
                .concrete(endpoint)?
                .filter(|concrete| model.is_a(concrete, &endpoint.object_type));
            let Some(found) = found else {
                return Err(CommitError::DanglingLink {
                    key: Box::new(write.key.clone()),
                    missing: endpoint.clone(),
                });
            };
            concrete.push(found);
        }
        // The effective declaration is the source's concrete type's: a
        // subtype's override narrows the link even when the source is named
        // through a supertype.
        if let [source, target] = concrete.as_slice() {
            let admitted = model
                .link(source, &write.key.link)
                .is_some_and(|declared| model.is_a(target, &declared.target));
            if !admitted {
                return Err(CommitError::LinkTarget {
                    key: Box::new(write.key.clone()),
                });
            }
        }
    }
    for write in &commit.writes {
        if !matches!(write.mutation, FactMutation::Delete) {
            continue;
        }
        if let Some(link) = links
            .get(&model.canonical(&write.key))
            .and_then(|live| live.iter().next())
        {
            return Err(CommitError::DanglingLink {
                key: Box::new(link.clone()),
                missing: write.key.clone(),
            });
        }
    }
    Ok(())
}

/// Check each new acyclic edge against the final graph, including removals
/// and other additions in this commit. Only the same declared forward role
/// participates; inverse roles do not create false two-node cycles.
pub(super) fn check_acyclic_links<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    let mut remaining = MAX_LINK_READ_TARGETS;
    let mut remaining_bytes = MAX_LINK_READ_BYTES;
    for write in &commit.links {
        if !matches!(write.mutation, LinkMutation::Add { .. }) {
            continue;
        }
        let key = model.canonical_link(&write.key);
        let Some(source) = post.concrete(&key.source)? else {
            return Err(CommitError::DanglingLink {
                key: Box::new(key.clone()),
                missing: key.source.clone(),
            });
        };
        if !model
            .link(&source, &key.link)
            .is_some_and(|link| link.acyclic)
        {
            continue;
        }
        let owner = model.link_owner(&source, &key.link);
        charge_acyclic_bytes(&mut remaining_bytes, &key.source, key.target.key.len())?;
        let mut pending = vec![key.target.clone()];
        let mut visited = BTreeSet::new();
        while let Some(node) = pending.pop() {
            if node == key.source {
                return Err(CommitError::EffectiveValues {
                    key: key.source.clone(),
                    reason: format!("acyclic link `{}` would create a cycle", key.link),
                });
            }
            if !visited.insert(node.clone()) {
                continue;
            }
            remaining =
                remaining
                    .checked_sub(1)
                    .ok_or_else(|| CommitError::TraversalLimitExceeded {
                        key: key.source.clone(),
                        reason: "acyclic link traversal exceeds the 1000-record commit budget"
                            .to_owned(),
                    })?;
            let Some(node_type) = post.concrete(&node)? else {
                continue;
            };
            if model.link_owner(&node_type, &key.link) != owner {
                continue;
            }
            let targets = post
                .before()
                .link_targets(&node, &key.link)
                .map_err(CommitError::Host)?
                .ok_or_else(|| CommitError::TraversalLimitExceeded {
                    key: key.source.clone(),
                    reason: "acyclic link adjacency proof is incomplete or exceeds the read budget"
                        .to_owned(),
                })?;
            for target in &targets {
                charge_acyclic_bytes(
                    &mut remaining_bytes,
                    &key.source,
                    target.key.target.key.len(),
                )?;
            }
            let mut outgoing: BTreeSet<_> = targets
                .iter()
                .map(|target| model.canonical_link(&target.key))
                .collect();
            for (changed, add) in post.changes(&node) {
                if changed.source != node || changed.link != key.link {
                    continue;
                }
                match super::overlay_core::edge_delta(outgoing.contains(changed), *add) {
                    super::overlay_core::EdgeDelta::Insert => {
                        charge_acyclic_bytes(
                            &mut remaining_bytes,
                            &key.source,
                            changed.target.key.len(),
                        )?;
                        outgoing.insert(changed.clone());
                    }
                    super::overlay_core::EdgeDelta::Remove => {
                        outgoing.remove(changed);
                    }
                    super::overlay_core::EdgeDelta::Keep => {}
                }
            }
            // Bound the frontier as well as node visits before allocating it.
            if outgoing.len() > remaining.saturating_sub(pending.len()) {
                return Err(CommitError::TraversalLimitExceeded {
                    key: key.source.clone(),
                    reason: "acyclic link frontier exceeds the 1000-record commit budget"
                        .to_owned(),
                });
            }
            pending.extend(outgoing.into_iter().map(|link| link.target));
        }
    }
    Ok(())
}

fn charge_acyclic_bytes<X>(
    remaining: &mut usize,
    source: &FactKey,
    bytes: usize,
) -> Result<(), CommitError<X>> {
    *remaining =
        remaining
            .checked_sub(bytes)
            .ok_or_else(|| CommitError::TraversalLimitExceeded {
                key: source.clone(),
                reason: "acyclic link traversal exceeds the commit byte budget".to_owned(),
            })?;
    Ok(())
}

/// Presence is checked under the final concrete contract, including inherited
/// roles, after applying this commit's removals and additions atomically.
pub(super) fn check_required_links<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    // Index final types and link changes once. Repeatedly searching all
    // writes for each affected source would make bulk creates quadratic.
    let mut sources: BTreeSet<_> = commit
        .writes
        .iter()
        .map(|w| model.canonical(&w.key))
        .collect();
    sources.extend(commit.links.iter().map(|w| model.canonical(&w.key.source)));
    let observed: BTreeSet<_> = authorization
        .into_iter()
        .flat_map(ObservationAuthorization::observed_sources)
        .map(|key| model.canonical(key))
        .collect();
    let mut roles_by_type = BTreeMap::new();
    for source in sources {
        if observed.contains(&source) {
            continue;
        }
        let Some(concrete) = post.concrete(&source)? else {
            continue;
        };
        let roles = roles_by_type.entry(concrete.clone()).or_insert_with(|| {
            let roles: BTreeSet<_> = model
                .catalog
                .ancestry(&concrete)
                .into_iter()
                .flat_map(|def| {
                    model
                        .catalog
                        .declared_links(&def.name)
                        .into_iter()
                        .map(|link| link.id.as_str())
                })
                .collect();
            roles
                .into_iter()
                .filter(|role| {
                    model
                        .link(&concrete, role)
                        .is_some_and(|link| link.cardinality.required)
                })
                .collect::<Vec<_>>()
        });
        for &role in roles.iter() {
            let role_changes = post
                .changes(&source)
                .iter()
                .filter(|(key, _)| key.source == source && key.link == role);
            // Added endpoints already passed admission and are live in the
            // final graph. A replacement therefore needs no snapshot scan.
            if role_changes.clone().any(|(_, add)| *add) {
                continue;
            }
            let excluded = role_changes.map(|(key, _)| key.target.clone()).collect();
            let present = post
                .before()
                .has_link_target(&source, role, &excluded)
                .map_err(CommitError::Host)?
                .ok_or_else(|| CommitError::EffectiveValues {
                    key: source.clone(),
                    reason: format!("required link `{role}` presence proof is incomplete"),
                })?;
            if !present {
                return Err(CommitError::EffectiveValues {
                    key: source,
                    reason: format!("required link `{role}` has no target in the final graph"),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn check_cardinality<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    links: &BTreeMap<FactKey, BTreeSet<LinkKey>>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    for write in &commit.links {
        if !matches!(write.mutation, LinkMutation::Add { .. }) {
            continue;
        }
        let key = model.canonical_link(&write.key);
        // The source's concrete type's declaration (endpoints passed, so the
        // source exists once the commit applies).
        let Some(declared) = post
            .concrete(&key.source)?
            .and_then(|source| model.link(&source, &write.key.link))
        else {
            continue;
        };
        if declared.cardinality.to == Multiplicity::One {
            check_one(post, &write.key, &key, Direction::Outgoing)?;
        }
        if declared.cardinality.from == Multiplicity::One {
            check_one(post, &write.key, &key, Direction::Incoming)?;
        }
    }
    for (source, source_links) in links {
        let Some(concrete) = post.concrete(source)? else {
            continue;
        };
        let required = model
            .catalog
            .owned_collections(&concrete)
            .into_iter()
            .filter(|collection| {
                model
                    .link(&concrete, collection.id.as_str())
                    .is_some_and(|link| link.cardinality.required)
            })
            .map(|collection| collection.id.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        for link in required {
            if !source_links
                .iter()
                .any(|live| live.source == *source && live.link == link)
            {
                return Err(CommitError::LinkRequired {
                    parent: source.clone(),
                    link,
                });
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Direction {
    Outgoing,
    Incoming,
}

/// Bounded adjacency proofs exclude all touched endpoints. A second new edge
/// or any unchanged edge in the same role violates multiplicity one.
fn check_one<V: CommitView + ?Sized>(
    post: &PostState<'_, '_, V>,
    written: &LinkKey,
    key: &LinkKey,
    direction: Direction,
) -> Result<(), CommitError<V::Error>> {
    let (endpoint, label) = match direction {
        Direction::Outgoing => (&key.source, "outgoing"),
        Direction::Incoming => (&key.target, "incoming"),
    };
    let mut added = false;
    let mut excluded = BTreeSet::new();
    let refusal = || CommitError::LinkCardinality {
        key: Box::new(written.clone()),
        endpoint: endpoint.clone(),
    };
    for (edge, add) in post.changes(endpoint) {
        if edge.link != key.link {
            continue;
        }
        let (this, other) = match direction {
            Direction::Outgoing => (&edge.source, &edge.target),
            Direction::Incoming => (&edge.target, &edge.source),
        };
        if this != endpoint {
            continue;
        }
        if *add && added {
            return Err(refusal());
        }
        added |= *add;
        excluded.insert(other.clone());
    }
    let present = match direction {
        Direction::Outgoing => post
            .before()
            .has_link_target(endpoint, &key.link, &excluded),
        Direction::Incoming => post
            .before()
            .has_link_source(endpoint, &key.link, &excluded),
    }
    .map_err(CommitError::Host)?;
    match present {
        Some(true) => Err(refusal()),
        Some(false) => Ok(()),
        None => Err(CommitError::EffectiveValues {
            key: endpoint.clone(),
            reason: format!(
                "bounded cardinality proof unavailable for {label} link `{}`",
                key.link
            ),
        }),
    }
}
