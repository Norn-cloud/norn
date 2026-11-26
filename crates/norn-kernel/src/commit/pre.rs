use crate::cell_host::{
    CellCommit, CommitError, FactKey, FactMutation, FactVersion, LinkKey, LinkReadExpectation,
    LinkReadTarget,
};
use crate::commit_check::{CommitModel, CommitView};
use crate::commit_versions::{CurrentVersion, first_conflict};
use crate::snapshot_observe::ObservationAuthorization;
use norn_ir::RuntimeValue;
use norn_ir::identifier_limits::{
    MAX_ACTION_CALL_ID_BYTES, MAX_LINK_NAME_BYTES, MAX_OBJECT_KEY_BYTES,
    MAX_OBJECT_TYPE_NAME_BYTES, MAX_TOMBSTONE_KEY_BYTES,
};
use norn_ir::posting::{AccountingError, AccountingLegIr};
use std::collections::{BTreeMap, BTreeSet};
/// Maximum complete outgoing link targets a turn may bind as one adjacency
/// read. This is the current `norn.limits/1` slice-record ceiling.
pub(super) const MAX_LINK_READ_TARGETS: usize = 1_000;
/// Maximum aggregate target-key bytes bound to a complete link read.
pub(super) const MAX_LINK_READ_BYTES: usize = 262_144;

use crate::commit_check::proposed_type;
pub(super) fn check_event_limit<E, C, T, X>(
    commit: &CellCommit<E, C, T>,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<X>> {
    if let Some(authorization) = authorization
        && commit.events.len() > authorization.event_limit()
    {
        return Err(CommitError::EventLimitExceeded {
            limit: u32::try_from(authorization.event_limit()).unwrap_or(u32::MAX),
            actual: commit.events.len(),
        });
    }
    Ok(())
}

pub(super) fn check_delivery<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    if let Some(consumed) = &commit.consumes {
        let is_pending = view.pending(consumed).map_err(CommitError::Host)?;
        if !is_pending {
            return Err(CommitError::NotPending {
                consumed: consumed.clone(),
            });
        }
    }
    Ok(())
}

/// A write that observed an object already present and immutable can never
/// succeed on retry, so `ImmutableObject` outranks the stale-version
/// `Conflict`. A write whose expectation differs from the current version (an
/// absent object that a concurrent create has since made present) is a
/// retryable `Conflict` that `check_versions` reports, so creates can still
/// re-read and replay or allocate the next number.
pub(super) fn check_immutable<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    for write in &commit.writes {
        let key = model.canonical(&write.key);
        let concrete = view.concrete(&key).map_err(CommitError::Host)?;
        if concrete
            .as_ref()
            .is_some_and(|concrete| model.catalog.immutable(concrete))
            && view.version(&key).map_err(CommitError::Host)? == write.expected
        {
            return Err(CommitError::ImmutableObject {
                key: write.key.clone(),
            });
        }
    }
    Ok(())
}

/// Refuse any attempt to update or delete an immutable GL leg, and require
/// every newly written leg group in this commit to balance independently.
///
/// Every write to a marked type is discovered from the catalog. The record's
/// leg id must equal the fact key, and the journal id is the same identity
/// carried by `AccountingPostingIr::journal_id`; group pins keep books and
/// graph versions from cancelling one another.
pub(super) fn check_accounting_postings<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    type GroupKey = (String, String, String, u64, String);
    let mut groups = BTreeMap::<GroupKey, (FactKey, Vec<AccountingLegIr>)>::new();

    let mut precisions = BTreeMap::new();
    for write in &commit.writes {
        let requested_declaration =
            model
                .accounting_leg(&write.key.object_type)
                .map_err(|reason| CommitError::AccountingPosting {
                    key: write.key.clone(),
                    reason,
                })?;
        // A write may name an ancestor of an already-live object. In
        // particular, deletes are not type-checked by `check_versions`, so
        // resolve its declaration from the live concrete type as well.
        let live_declaration = view
            .concrete(&model.canonical(&write.key))
            .map_err(CommitError::Host)?
            .map(|concrete| model.accounting_leg(&concrete))
            .transpose()
            .map_err(|reason| CommitError::AccountingPosting {
                key: write.key.clone(),
                reason,
            })?
            .flatten();
        // Keep the requested declaration for creates and for a mismatched
        // alias, while preferring the live concrete declaration when present.
        let declaration = live_declaration.or(requested_declaration);
        let Some(declaration) = declaration else {
            continue;
        };
        let refuse = |reason| CommitError::AccountingPosting {
            key: write.key.clone(),
            reason,
        };
        let values = match &write.mutation {
            FactMutation::Put { values } => values,
            FactMutation::Delete => return Err(refuse(AccountingError::JournalDeleteForbidden)),
        };
        if write.expected != FactVersion::ABSENT {
            return Err(refuse(AccountingError::PostedJournalImmutable));
        }
        let journal_id = values
            .get(declaration.journal_id_property.as_str())
            .and_then(RuntimeValue::as_str)
            .ok_or_else(|| refuse(AccountingError::InvalidLegRecord))?;
        let leg_value = values
            .get(declaration.leg_property.as_str())
            .ok_or_else(|| refuse(AccountingError::InvalidLegRecord))?;
        let leg: AccountingLegIr = norn_ir::runtime::decode(leg_value)
            .map_err(|_decode| refuse(AccountingError::InvalidLegRecord))?;
        if leg.leg_id != write.key.key {
            return Err(refuse(AccountingError::LegIdentityMismatch));
        }
        let precision_key = (
            leg.account.scope_key.clone(),
            leg.account.book_id.clone(),
            leg.account.graph_id.clone(),
            leg.account.graph_version,
            leg.unit_id.clone(),
        );
        if precisions
            .insert(precision_key, leg.unit_precision)
            .is_some_and(|prior| prior != leg.unit_precision)
        {
            return Err(refuse(AccountingError::UnitPrecisionMismatch));
        }
        let group = (
            leg.account.scope_key.clone(),
            leg.account.book_id.clone(),
            leg.account.graph_id.clone(),
            leg.account.graph_version,
            journal_id.to_owned(),
        );
        groups
            .entry(group)
            .or_insert_with(|| (write.key.clone(), Vec::new()))
            .1
            .push(leg);
    }

    for ((_, _, _, _, journal_id), (key, legs)) in groups {
        if let Err(reason) =
            norn_checker::posting::validate_balanced_accounting_legs(&journal_id, &legs)
        {
            return Err(CommitError::AccountingPosting { key, reason });
        }
    }
    Ok(())
}

pub(super) fn check_shape<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    trusted_owned_rows: bool,
) -> Result<(), CommitError<V::Error>> {
    check_commit_identifiers(commit)?;
    let mut seen = BTreeSet::new();
    let keys = commit
        .reads
        .iter()
        .map(|read| &read.key)
        .chain(commit.writes.iter().map(|write| &write.key));
    for key in keys {
        if !seen.insert(model.canonical(key)) {
            return Err(CommitError::DuplicateKey { key: key.clone() });
        }
    }
    let mut links = BTreeSet::new();
    for write in &commit.links {
        if !links.insert(model.canonical_link(&write.key)) {
            return Err(CommitError::DuplicateLink {
                key: Box::new(write.key.clone()),
            });
        }
    }
    check_link_read_shapes(&commit.link_reads, model)?;
    crate::predicate_read::check_predicate_read_shapes(&commit.predicate_reads, model)?;
    for write in &commit.links {
        let admitted = model
            .link(&write.key.source.object_type, &write.key.link)
            .is_some_and(|declared| model.is_a(&write.key.target.object_type, &declared.target));
        if !admitted {
            return Err(CommitError::LinkTarget {
                key: Box::new(write.key.clone()),
            });
        }
    }
    if !trusted_owned_rows {
        for write in &commit.writes {
            let live = view
                .concrete(&model.canonical(&write.key))
                .map_err(CommitError::Host)?;
            let protected = std::iter::once(&write.key.object_type)
                .chain(live.as_ref())
                .any(|ty| {
                    ty.as_str() == crate::action_commit::OWNED_ROWS_TYPE
                        || model.catalog.is_owned_child_type(ty)
                        || (matches!(write.mutation, FactMutation::Delete)
                            && !model.catalog.owned_collections(ty).is_empty())
                });
            if protected {
                return Err(CommitError::OwnedRowsProtected {
                    key: write.key.clone(),
                });
            }
        }
        for write in &commit.links {
            // Ancestor keys share the concrete object's identity. Check both
            // live and proposed endpoints, including removes and deletes.
            let mut protected = false;
            for endpoint in [&write.key.source, &write.key.target] {
                let canonical = model.canonical(endpoint);
                let live = view.concrete(&canonical).map_err(CommitError::Host)?;
                let proposed = proposed_type(commit, model, view, &canonical)?;
                for ty in std::iter::once(&endpoint.object_type)
                    .chain(live.as_ref())
                    .chain(proposed.as_ref())
                {
                    protected |= model.catalog.is_owned_child_type(ty)
                        || (endpoint == &write.key.source
                            && model
                                .catalog
                                .owned_collections(ty)
                                .iter()
                                .any(|collection| collection.id.as_str() == write.key.link));
                }
            }
            if protected {
                return Err(CommitError::OwnedRowsProtected {
                    key: write.key.source.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Check complete adjacency expectations, including empty declared-link reads.
fn check_link_read_shapes<X>(
    reads: &[LinkReadExpectation],
    model: CommitModel<'_, '_>,
) -> Result<(), CommitError<X>> {
    let mut link_reads = BTreeSet::new();
    for read in reads {
        check_fact_key_identity(&read.source)?;
        if read.link.trim().is_empty() {
            return Err(CommitError::InvalidLinkRead {
                source_key: read.source.clone(),
                link: read.link.clone(),
            });
        }
        if read.link.len() > MAX_LINK_NAME_BYTES {
            return Err(CommitError::IdentifierTooLong {
                field: "link name",
                actual_bytes: read.link.len(),
                max_bytes: MAX_LINK_NAME_BYTES,
            });
        }
        let canonical_source = model.canonical(&read.source);
        if !link_reads.insert((canonical_source.clone(), read.link.clone())) {
            return Err(CommitError::DuplicateLinkRead {
                source_key: read.source.clone(),
                link: read.link.clone(),
            });
        }
        check_link_read_target_count(read)?;
        let Some(declared) = model.link(&read.source.object_type, &read.link) else {
            return Err(CommitError::InvalidLinkRead {
                source_key: read.source.clone(),
                link: read.link.clone(),
            });
        };
        let mut previous: Option<LinkKey> = None;
        for target in &read.targets {
            check_link_key_identity(&target.key)?;
            let canonical = model.canonical_link(&target.key);
            if canonical.source != canonical_source
                || canonical.link != read.link
                || previous.as_ref().is_some_and(|prior| prior >= &canonical)
            {
                return Err(CommitError::InvalidLinkRead {
                    source_key: read.source.clone(),
                    link: read.link.clone(),
                });
            }
            let admitted = model.is_a(&target.key.target.object_type, &declared.target);
            if !admitted {
                return Err(CommitError::LinkTarget {
                    key: Box::new(target.key.clone()),
                });
            }
            previous = Some(canonical);
        }
        check_link_read_target_bytes(read)?;
    }
    Ok(())
}

fn check_link_read_target_count<X>(
    read: &crate::cell_host::LinkReadExpectation,
) -> Result<(), CommitError<X>> {
    if read.targets.len() > MAX_LINK_READ_TARGETS {
        return Err(CommitError::LinkReadTooLarge {
            source_key: read.source.clone(),
            link: read.link.clone(),
            actual: read.targets.len(),
            max: MAX_LINK_READ_TARGETS,
        });
    }
    Ok(())
}

fn check_link_read_target_bytes<X>(
    read: &crate::cell_host::LinkReadExpectation,
) -> Result<(), CommitError<X>> {
    let aggregate_bytes = read.targets.iter().fold(0_usize, |total, target| {
        total.saturating_add(target.key.target.key.len())
    });
    if aggregate_bytes > MAX_LINK_READ_BYTES {
        return Err(CommitError::LinkReadTooLarge {
            source_key: read.source.clone(),
            link: read.link.clone(),
            actual: aggregate_bytes,
            max: MAX_LINK_READ_BYTES,
        });
    }
    Ok(())
}

/// Check identifiers in a raw commit before canonicalization or host access.
/// Hosts also call this before preparing effective values, which may read
/// link endpoints from storage.
///
/// # Errors
///
/// Returns [`CommitError::IdentifierTooLong`] for the first oversized identity.
pub fn check_commit_identifiers<E, C, T, X>(
    commit: &CellCommit<E, C, T>,
) -> Result<(), CommitError<X>> {
    for key in commit
        .reads
        .iter()
        .map(|read| &read.key)
        .chain(commit.writes.iter().map(|write| &write.key))
    {
        check_fact_key_identity(key)?;
    }
    for write in &commit.links {
        check_link_key_identity(&write.key)?;
    }
    for read in &commit.link_reads {
        check_fact_key_identity(&read.source)?;
        if read.link.len() > MAX_LINK_NAME_BYTES {
            return Err(CommitError::IdentifierTooLong {
                field: "link name",
                actual_bytes: read.link.len(),
                max_bytes: MAX_LINK_NAME_BYTES,
            });
        }
        check_link_read_target_count(read)?;
        for target in &read.targets {
            check_link_key_identity(&target.key)?;
        }
        check_link_read_target_bytes(read)?;
    }
    for read in &commit.predicate_reads {
        if read.members.len() > crate::predicate_read::MAX_PREDICATE_MEMBERS {
            return Err(CommitError::InvalidPredicateRead {
                object_type: read.predicate.object_type.clone(),
                reason: "too many members",
            });
        }
        for member in &read.members {
            check_fact_key_identity(&member.key)?;
        }
    }
    Ok(())
}

/// Check one raw object identity before canonicalization or host access.
///
/// # Errors
///
/// Returns [`CommitError::IdentifierTooLong`] when its type name or key exceeds
/// the shared byte limit, including the derived limits for system keys.
pub fn check_fact_key_identity<X>(key: &FactKey) -> Result<(), CommitError<X>> {
    let object_type_bytes = key.object_type.as_str().len();
    if object_type_bytes > MAX_OBJECT_TYPE_NAME_BYTES {
        return Err(CommitError::IdentifierTooLong {
            field: "object type name",
            actual_bytes: object_type_bytes,
            max_bytes: MAX_OBJECT_TYPE_NAME_BYTES,
        });
    }

    let max_key_bytes = match key.object_type.as_str() {
        crate::action_commit::ACTION_CALL_TYPE => MAX_ACTION_CALL_ID_BYTES,
        crate::snapshot_observe::OBSERVED_TOMBSTONE_TYPE
        | crate::snapshot_observe::SOURCE_OWNERSHIP_TYPE => MAX_TOMBSTONE_KEY_BYTES,
        _ => MAX_OBJECT_KEY_BYTES,
    };
    let key_bytes = key.key.len();
    if key_bytes > max_key_bytes {
        return Err(CommitError::IdentifierTooLong {
            field: "object key",
            actual_bytes: key_bytes,
            max_bytes: max_key_bytes,
        });
    }
    Ok(())
}

/// Check both endpoints and the declared link name before canonicalization or
/// host access.
fn check_link_key_identity<X>(key: &LinkKey) -> Result<(), CommitError<X>> {
    check_fact_key_identity(&key.source)?;
    check_fact_key_identity(&key.target)?;
    let link_bytes = key.link.len();
    if link_bytes > MAX_LINK_NAME_BYTES {
        return Err(CommitError::IdentifierTooLong {
            field: "link name",
            actual_bytes: link_bytes,
            max_bytes: MAX_LINK_NAME_BYTES,
        });
    }
    Ok(())
}

pub(super) fn check_versions<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    // ID-STABLE-1: event payloads never establish the per-write observation authority.
    let authorized = authorization.map(ObservationAuthorization::indexed);
    for write in &commit.writes {
        if matches!(write.mutation, FactMutation::Put { .. }) {
            let key = model.canonical(&write.key);
            let version = view.version(&key).map_err(CommitError::Host)?;
            let live = view.concrete(&key).map_err(CommitError::Host)?.is_some();
            if version.has_committed_deletion(live)
                && !authorized
                    .as_ref()
                    .is_some_and(|index| index.authorizes(write))
            {
                return Err(CommitError::IdentityReuse);
            }
        }
    }
    let reads: Vec<(&FactKey, u64)> = commit
        .reads
        .iter()
        .map(|read| (&read.key, read.expected.0))
        .collect();
    let writes: Vec<(&FactKey, u64)> = commit
        .writes
        .iter()
        .map(|write| (&write.key, write.expected.0))
        .collect();
    if let Some((key, expected, current)) =
        first_conflict(&mut ObjectVersions { model, view }, &reads, &writes)
            .map_err(CommitError::Host)?
    {
        return Err(CommitError::Conflict {
            key: key.clone(),
            expected: FactVersion(expected),
            current: FactVersion(current),
        });
    }
    for write in &commit.writes {
        if !matches!(write.mutation, FactMutation::Put { .. }) {
            continue;
        }
        if let Some(concrete) = view
            .concrete(&model.canonical(&write.key))
            .map_err(CommitError::Host)?
            && concrete != write.key.object_type
        {
            return Err(CommitError::TypeMismatch {
                key: write.key.clone(),
                concrete,
            });
        }
    }
    let links: Vec<(&LinkKey, u64)> = commit
        .links
        .iter()
        .map(|write| (&write.key, write.expected.0))
        .collect();
    if let Some((key, expected, current)) =
        first_conflict(&mut LinkVersions { model, view }, &links, &[]).map_err(CommitError::Host)?
    {
        return Err(CommitError::LinkConflict {
            key: Box::new(key.clone()),
            expected: FactVersion(expected),
            current: FactVersion(current),
        });
    }
    for read in &commit.link_reads {
        let source = model.canonical(&read.source);
        let Some(current) = view
            .link_targets(&source, &read.link)
            .map_err(CommitError::Host)?
        else {
            return Err(CommitError::UnsupportedLinkRead {
                source_key: read.source.clone(),
                link: read.link.clone(),
            });
        };
        let expected: Vec<_> = read
            .targets
            .iter()
            .map(|target| LinkReadTarget {
                key: model.canonical_link(&target.key),
                version: target.version,
            })
            .collect();
        let mut current: Vec<_> = current
            .into_iter()
            .map(|target| LinkReadTarget {
                key: model.canonical_link(&target.key),
                version: target.version,
            })
            .collect();
        current.sort_by(|left, right| left.key.cmp(&right.key));
        if current != expected {
            return Err(CommitError::LinkReadConflict {
                source_key: read.source.clone(),
                link: read.link.clone(),
            });
        }
    }
    crate::predicate_read::recheck_predicate_reads(&commit.predicate_reads, model, view)?;
    Ok(())
}

/// Each read or written object's current version, by canonical identity.
struct ObjectVersions<'m, 'a, 'v, V: ?Sized> {
    model: CommitModel<'m, 'a>,
    view: &'v V,
}

impl<V: CommitView + ?Sized> CurrentVersion<&FactKey, V::Error> for ObjectVersions<'_, '_, '_, V> {
    fn current(&mut self, key: &&FactKey) -> Result<u64, V::Error> {
        Ok(self.view.version(&self.model.canonical(key))?.0)
    }
}

/// Each written link's current version, by canonical identity.
struct LinkVersions<'m, 'a, 'v, V: ?Sized> {
    model: CommitModel<'m, 'a>,
    view: &'v V,
}

impl<V: CommitView + ?Sized> CurrentVersion<&LinkKey, V::Error> for LinkVersions<'_, '_, '_, V> {
    fn current(&mut self, key: &&LinkKey) -> Result<u64, V::Error> {
        Ok(self.view.link_version(&self.model.canonical_link(key))?.0)
    }
}
