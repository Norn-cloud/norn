//! The shared commit admission check ([`check_commit`]) and its two inputs
//! (#3013, #3018, lead decision 2026-10-02):
//!
//! - **model** ([`CommitModel`]): what the cell's object types say — the
//!   canonical identity of an object (its hierarchy root and key), `is_a`
//!   through `extends` and `implements`, each link declaration, and any
//!   inherited accounting-leg and invariant declarations. It comes from the shared
//!   [`TypeCatalog`], never from a host. A model with no types declares no
//!   links or accounting legs, so a host without a model cannot commit them
//!   (fail closed).
//! - **state** ([`CommitView`]): what the host has committed — the delivery's
//!   pending flag, each canonical object's version, live concrete type, and
//!   live links, and each canonical link's version.
//!
//! Every host runs the same check, so all hosts refuse the same commits with
//! the same refusal.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::EffectiveKind;
use norn_expr::{EvaluationBudget, ExprError, Expression, Value as ExprValue};
use norn_ir::identifier_limits::{
    MAX_ACTION_CALL_ID_BYTES, MAX_LINK_NAME_BYTES, MAX_OBJECT_KEY_BYTES,
    MAX_OBJECT_TYPE_NAME_BYTES, MAX_TOMBSTONE_KEY_BYTES,
};
use norn_ir::posting::{AccountingError, AccountingLegIr};
use norn_ir::{
    ConstraintClass, DataQualityOutcome, InvariantId, InvariantOwner, Link, Multiplicity,
    ObjectTypeName, PropertyType, RuntimeValue,
};

use crate::cell_host::{
    CellCommit, CommitError, CommitVersions, Consumed, FactKey, FactMutation, FactVersion,
    FactWrite, LinkKey, LinkMutation, LinkReadExpectation, LinkReadTarget, LinkWrite,
    OwnedRowsCommitProof, WrittenLinkVersion, WrittenVersion,
};

/// Maximum complete outgoing link targets a turn may bind as one adjacency
/// read. This is the current `norn.limits/1` slice-record ceiling.
const MAX_LINK_READ_TARGETS: usize = 1_000;
/// Maximum aggregate target-key bytes bound to a complete link read.
const MAX_LINK_READ_BYTES: usize = 262_144;
use crate::decision_commit::{EffectiveValuesError, TypeCatalog};
use crate::snapshot_observe::ObservationAuthorization;

/// The model facts a commit is checked against, from the cell's
/// [`TypeCatalog`].
#[derive(Debug, Clone, Copy)]
pub struct CommitModel<'m, 'a> {
    catalog: &'m TypeCatalog<'a>,
}

impl<'m, 'a> CommitModel<'m, 'a> {
    /// The model of `catalog`.
    #[must_use]
    pub const fn new(catalog: &'m TypeCatalog<'a>) -> Self {
        Self { catalog }
    }

    /// The canonical identity of `key`: its hierarchy's root type and its
    /// key. One object answers to every type of its hierarchy; an unknown
    /// type is its own root.
    #[must_use]
    pub fn canonical(&self, key: &FactKey) -> FactKey {
        let root = self
            .catalog
            .ancestry(&key.object_type)
            .last()
            .map_or_else(|| key.object_type.clone(), |def| def.name.clone());
        FactKey {
            object_type: root,
            key: key.key.clone(),
        }
    }

    /// The canonical identity of link `key`: its endpoints' canonical
    /// identities.
    #[must_use]
    pub fn canonical_link(&self, key: &LinkKey) -> LinkKey {
        LinkKey::new(
            self.canonical(&key.source),
            key.link.clone(),
            self.canonical(&key.target),
        )
    }

    /// Whether an object of `concrete` type is a `ty`: the type itself, a
    /// supertype, or an interface the type or a supertype implements.
    #[must_use]
    pub fn is_a(&self, concrete: &ObjectTypeName, ty: &ObjectTypeName) -> bool {
        if concrete == ty {
            return true;
        }
        self.catalog
            .ancestry(concrete)
            .iter()
            .any(|def| def.name == *ty)
            || self.catalog.implements(concrete, ty.as_str())
    }

    /// The nearest declaration of `link` on `source`'s lineage (a subtype may
    /// narrow an inherited link).
    #[must_use]
    pub fn link(&self, source: &ObjectTypeName, link: &str) -> Option<&'a Link> {
        self.catalog.ancestry(source).into_iter().find_map(|def| {
            def.links
                .iter()
                .find(|declared| declared.id.as_str() == link)
        })
    }

    /// The declaring relation identity. Narrowing overrides share their
    /// furthest declaring ancestor's relation; unrelated same-name members do not.
    #[must_use]
    pub fn link_owner(&self, source: &ObjectTypeName, link: &str) -> Option<ObjectTypeName> {
        self.catalog
            .ancestry(source)
            .into_iter()
            .rev()
            .find_map(|def| {
                def.links
                    .iter()
                    .any(|declared| declared.id.as_str() == link)
                    .then(|| def.name.clone())
            })
    }

    /// Materialize link properties under the source's effective concrete
    /// contract, including defaults and canonical UTC timestamps.
    ///
    /// # Errors
    ///
    /// Returns a host lookup failure or an effective-value refusal when a
    /// submitted temporal value or default is invalid.
    pub fn effective_link_values<E, C, T, V: CommitView + ?Sized>(
        &self,
        commit: &CellCommit<E, C, T>,
        view: &V,
        key: &LinkKey,
        values: &crate::cell_host::JsonObject,
    ) -> Result<crate::cell_host::JsonObject, CommitError<V::Error>> {
        let source = concrete_after(commit, *self, view, &self.canonical(&key.source))?
            .unwrap_or_else(|| key.source.object_type.clone());
        self.catalog
            .effective_link_values(&source, &key.link, values)
            .map_err(|error| CommitError::EffectiveValues {
                key: key.source.clone(),
                reason: format!("link `{}`: {error}", key.link),
            })
    }

    /// The effective accounting-leg declaration of `concrete`, including a
    /// declaration inherited from a supertype.
    ///
    /// # Errors
    ///
    /// A conflicting redeclaration in the lineage is refused as an invalid leg record.
    pub fn accounting_leg(
        &self,
        concrete: &ObjectTypeName,
    ) -> Result<Option<&'a norn_ir::posting::AccountingLegDeclaration>, AccountingError> {
        let mut effective = None;
        for def in self.catalog.ancestry(concrete) {
            if let Some(declaration) = def.accounting_leg.as_ref() {
                if effective.is_some_and(|inherited| inherited != declaration) {
                    return Err(AccountingError::InvalidLegRecord);
                }
                effective = Some(declaration);
            }
        }
        Ok(effective)
    }
}

/// What a host has committed, as [`check_commit`] reads it. Every key it is
/// asked about is canonical ([`CommitModel::canonical`]).
pub trait CommitView {
    /// Host failure.
    type Error;

    /// Whether the consumed delivery is pending.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn pending(&self, consumed: &Consumed) -> Result<bool, Self::Error>;

    /// The current version of a canonical object.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn version(&self, key: &FactKey) -> Result<FactVersion, Self::Error>;

    /// The live concrete type of a canonical object; `None` when it does not
    /// exist.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Self::Error>;

    /// Whether this host can distinguish absent ownership from unreadable
    /// evidence. Hosts without values must refuse mutation of existing markers.
    fn can_read_observed_source_owners(&self) -> bool {
        false
    }

    /// The retained relation owner of an observed-source marker. Hosts that
    /// cannot decode this evidence return `None`, which refuses observed link
    /// writes to an existing marked source.
    ///
    /// # Errors
    /// The host's read failure.
    fn observed_source_owner(
        &self,
        marker: &FactKey,
    ) -> Result<Option<crate::snapshot_observe::ObservedSourceOwner>, Self::Error> {
        let _ = marker;
        Ok(None)
    }

    /// The current version of a canonical link.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn link_version(&self, key: &LinkKey) -> Result<FactVersion, Self::Error>;

    /// The complete canonical outgoing target set for one source/link pair.
    /// Hosts must bound enumeration before materializing target keys to 1,000
    /// records and 262,144 target-key bytes. `None` means enumeration is
    /// incomplete or the host cannot verify phantom additions/removals and a
    /// commit containing such an expectation must be refused.
    ///
    /// # Errors
    ///
    /// Returns the host read failure.
    fn link_targets(
        &self,
        source: &FactKey,
        link: &str,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        let _ = (source, link);
        Ok(None)
    }

    /// Whether a live outgoing target survives the excluded removals.
    /// A bounded existence proof need not enumerate a large complete adjacency.
    /// `None` means the host cannot complete the proof within its read limits.
    ///
    /// # Errors
    /// Returns the host read failure.
    fn has_link_target(
        &self,
        source: &FactKey,
        link: &str,
        excluded: &BTreeSet<FactKey>,
    ) -> Result<Option<bool>, Self::Error> {
        Ok(self.link_targets(source, link)?.map(|targets| {
            targets
                .iter()
                .any(|target| !excluded.contains(&target.key.target))
        }))
    }

    /// Every live link with the canonical object at either end, in any order
    /// and naming (the check canonicalizes them).
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Self::Error>;
}

/// The admission check every host runs before applying a commit.
///
/// Pure and host-independent: the model comes from the shared catalog, the
/// state from the host's view, so every host refuses the same commits with
/// the same refusal. Objects and links are compared by canonical identity.
/// Order:
///
/// 1. **shape** — no object twice across `reads` then `writes`
///    ([`CommitError::DuplicateKey`]), no link twice
///    ([`CommitError::DuplicateLink`]), and every link a declared link of
///    its source's type to an admitted target type
///    ([`CommitError::LinkTarget`]); every declared posting group is decoded,
///    complete, immutable, and exactly balanced
///    ([`CommitError::AccountingPosting`]);
/// 2. **delivery** — the consumed delivery, if any, is pending;
/// 3. **compare-and-swap** — every read, then every object write, matches
///    the current version ([`CommitError::Conflict`]); an object is put only
///    as its live concrete type ([`CommitError::TypeMismatch`]); every link
///    write matches the current link version ([`CommitError::LinkConflict`]);
/// 4. **endpoints** — no link dangles once the commit applies
///    ([`CommitError::DanglingLink`]): both endpoints of every added link
///    exist as their named types, and every live link of a deleted object
///    is removed by the same commit; every added link is admitted by the
///    declaration of its source's concrete type, which a subtype may narrow
///    ([`CommitError::LinkTarget`]);
/// 5. **cardinality** — by the source's concrete declaration, a to-one link
///    has at most one target per source and a from-one link at most one
///    source per target once the commit applies
///    ([`CommitError::LinkCardinality`], on the first add that exceeds it);
/// 6. **invariants** — each final `Put` satisfies every invariant on its type
///    lineage and implemented interfaces with the bounded `norn-expr`
///    evaluator ([`CommitError::InvariantViolation`]);
/// 7. **versions** — each written object's, then each written link's, next
///    version.
///
/// # Errors
///
/// The first failed check, or [`CommitError::Host`] if a lookup failed.
pub fn check_commit<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: &CommitModel<'_, '_>,
    view: &V,
) -> Result<CommitVersions, CommitError<V::Error>> {
    check_commit_inner(commit, *model, view, None, false)
}

/// Check the kernel-prepared atomic parent/owned-row commit. Only the
/// in-process action planner can construct the one-use proof; serialized
/// commits and portable ABI inputs always use [`check_commit`].
///
/// # Errors
///
/// The first failed check, or [`CommitError::Host`] if a lookup failed.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the authorization is deliberately consumed once at the admission boundary"
)]
pub fn check_commit_owned_rows<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: &CommitModel<'_, '_>,
    view: &V,
    proof: OwnedRowsCommitProof,
) -> Result<CommitVersions, CommitError<V::Error>> {
    // Perimeter adapters may append audit events after action preparation.
    // Recheck the final ledger payload against the exact admitted envelope.
    if u32::try_from(commit.events.len()).map_or(true, |actual| actual > proof.max_events()) {
        return Err(CommitError::EventLimitExceeded {
            limit: proof.max_events(),
            actual: commit.events.len(),
        });
    }
    let trusted_owned_rows = if proof.consumes() != commit.consumes.as_ref()
        // Perimeter adapters may append visibility/tombstone CAS reads, but
        // cannot remove or alter any read sealed by kernel preparation.
        || proof.reads().iter().any(|read| !commit.reads.contains(read))
        || proof.link_reads() != commit.link_reads.as_slice()
        || proof.writes().len() != commit.writes.len()
        || proof.links().len() != commit.links.len()
    {
        false
    } else {
        let canonical_writes = proof
            .writes()
            .iter()
            .map(|write| {
                let mutation = match &write.mutation {
                    FactMutation::Put { values } => FactMutation::Put {
                        values: model
                            .catalog
                            .effective_object_values(&write.key.object_type, values)
                            .map_err(|error| CommitError::EffectiveValues {
                                key: write.key.clone(),
                                reason: error.to_string(),
                            })?,
                    },
                    FactMutation::Delete => FactMutation::Delete,
                };
                Ok(FactWrite {
                    key: write.key.clone(),
                    expected: write.expected,
                    mutation,
                })
            })
            .collect::<Result<Vec<_>, CommitError<V::Error>>>()?;
        let canonical_links = proof
            .links()
            .iter()
            .map(|write| {
                let mutation = match &write.mutation {
                    LinkMutation::Add { values } => LinkMutation::Add {
                        values: model.effective_link_values(commit, view, &write.key, values)?,
                    },
                    LinkMutation::Remove => LinkMutation::Remove,
                };
                Ok(LinkWrite {
                    key: write.key.clone(),
                    expected: write.expected,
                    mutation,
                })
            })
            .collect::<Result<Vec<_>, CommitError<V::Error>>>()?;
        canonical_writes == commit.writes && canonical_links == commit.links
    };
    check_commit_inner(commit, *model, view, None, trusted_owned_rows)
}

/// Shared host admission for a prepared observation. The opaque authority
/// only permits its exact writes and expected findings; all other writes are
/// checked as ordinary command mutations.
///
/// # Errors
///
/// Returns the first failed admission check, or [`CommitError::Host`] if a
/// host lookup failed.
pub fn check_commit_observed<E, C, T: serde::Serialize, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: &CommitModel<'_, '_>,
    view: &V,
    authorization: &ObservationAuthorization,
) -> Result<CommitVersions, CommitError<V::Error>> {
    if !authorization.validates(commit) {
        return Err(CommitError::ObservationAuthorization);
    }
    check_commit_inner(commit, *model, view, Some(authorization), false)
}

fn check_commit_inner<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
    trusted_owned_rows: bool,
) -> Result<CommitVersions, CommitError<V::Error>> {
    if let Some(authorization) = authorization
        && commit.events.len() > authorization.event_limit()
    {
        return Err(CommitError::EventLimitExceeded {
            limit: u32::try_from(authorization.event_limit()).unwrap_or(u32::MAX),
            actual: commit.events.len(),
        });
    }
    check_shape(commit, model, view, trusted_owned_rows)?;
    check_accounting_postings(commit, model, view)?;
    if let Some(consumed) = &commit.consumes {
        let is_pending = view.pending(consumed).map_err(CommitError::Host)?;
        if !is_pending {
            return Err(CommitError::NotPending {
                consumed: consumed.clone(),
            });
        }
    }
    check_versions(commit, model, view)?;
    for write in &commit.writes {
        let concrete = view
            .concrete(&model.canonical(&write.key))
            .map_err(CommitError::Host)?;
        if concrete
            .as_ref()
            .is_some_and(|concrete| model.catalog.immutable(concrete))
        {
            return Err(CommitError::ImmutableObject {
                key: write.key.clone(),
            });
        }
    }
    check_link_authority(commit, model, view, authorization)?;
    let links = live_links_after(commit, model, view)?;
    check_endpoints(commit, model, view, &links)?;
    for write in &commit.links {
        if let LinkMutation::Add { values } = &write.mutation {
            model.effective_link_values(commit, view, &write.key, values)?;
        }
    }
    check_cardinality(commit, model, view, &links)?;
    check_required_links(commit, model, view, authorization)?;
    check_acyclic_links(commit, model, view)?;
    check_invariants(commit, model.catalog, authorization).map_err(|error| match error {
        InvariantAdmissionError::EffectiveValues { key, reason } => {
            CommitError::EffectiveValues { key, reason }
        }
        InvariantAdmissionError::BudgetExceeded { key, limit } => {
            CommitError::InvariantBudgetExceeded { key, limit }
        }
        InvariantAdmissionError::MissingInterface { key, interface } => {
            CommitError::InvariantModel { key, interface }
        }
        InvariantAdmissionError::Violation {
            key,
            owner,
            id,
            message,
            outcome,
        } => CommitError::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        },
        InvariantAdmissionError::ObservationAuthorization => CommitError::ObservationAuthorization,
    })?;
    next_versions(commit)
}

// Outgoing links share the source's authority, not their target's authority.
// Authored incoming edges remain authored; pending target resolution carries
// the original source's sealed scope and exact relation intent.
#[allow(
    clippy::too_many_lines,
    reason = "one shared boundary fences source ownership, pending intents, and outgoing relations atomically"
)]
fn check_link_authority<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    if authorization.is_none()
        && commit
            .writes
            .iter()
            .any(|write| write.key.object_type.as_str() == "norn.PendingLinks")
    {
        return Err(CommitError::ObservationAuthorization);
    }
    check_source_markers(commit, view, authorization)?;
    let sources: BTreeSet<_> = commit
        .links
        .iter()
        .map(|write| model.canonical(&write.key.source))
        .chain(
            authorization
                .into_iter()
                .flat_map(ObservationAuthorization::pending_sources)
                .map(|source| model.canonical(source)),
        )
        .collect();
    let final_types: BTreeMap<_, _> = commit
        .writes
        .iter()
        .filter(|write| matches!(write.mutation, FactMutation::Put { .. }))
        .map(|write| (model.canonical(&write.key), &write.key.object_type))
        .collect();
    for source in sources {
        let concrete = view.concrete(&source).map_err(CommitError::Host)?;
        let source_type = concrete
            .as_ref()
            .or_else(|| final_types.get(&source).copied())
            .unwrap_or(&source.object_type);
        let marker = FactKey::new(
            crate::snapshot_observe::SOURCE_OWNERSHIP_TYPE,
            format!("{}/{}", source_type.as_str(), source.key),
        );
        let source_is_owned = model.catalog.contains(&marker.object_type)
            && view.concrete(&marker).map_err(CommitError::Host)?.is_some();
        let Some(authorization) = authorization else {
            // Creating an ownership marker cannot launder an authored edge.
            if source_is_owned || commit.writes.iter().any(|write| write.key == marker) {
                return Err(CommitError::ObservationAuthorization);
            }
            continue;
        };
        if !model.catalog.contains(&marker.object_type) {
            return Err(CommitError::ObservationAuthorization);
        }
        let mut scopes = authorization
            .source_scopes()
            .iter()
            .filter(|(scope, _)| model.canonical(&scope.key) == source);
        let Some((scope, owner)) = scopes.next() else {
            return Err(CommitError::ObservationAuthorization);
        };
        let retained = view
            .observed_source_owner(&marker)
            .map_err(CommitError::Host)?;
        let successor = retained.as_ref().is_some_and(|old| {
            commit.writes.iter().any(|write| {
                write.key == marker
                    && authorization.permits_source_owner_recreation(
                        &FactKey::new(source_type.as_str(), &source.key),
                        old,
                        write,
                    )
            })
        }) && recreates_observed_source(commit, view, source_type, &source.key)?;
        if scopes.any(|(_, other)| other != owner)
            || !commit.reads.contains(scope)
                && !commit.writes.iter().any(|write| {
                    model.canonical(&write.key) == source && write.expected == scope.expected
                })
            || source_is_owned && retained.as_ref() != Some(owner) && !successor
            || !source_is_owned && (concrete.is_some() || !commit.writes.iter().any(|write| write.key == marker && matches!(&write.mutation, FactMutation::Put { values } if values.get("owner").and_then(crate::snapshot_observe::ObservedSourceOwner::from_value).as_ref() == Some(owner))))
        {
            return Err(CommitError::ObservationAuthorization);
        }
    }
    Ok(())
}

fn check_source_markers<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    for write in commit.writes.iter().filter(|write| {
        write.key.object_type.as_str() == crate::snapshot_observe::SOURCE_OWNERSHIP_TYPE
    }) {
        let retained = view
            .observed_source_owner(&write.key)
            .map_err(CommitError::Host)?;
        let proposed = match &write.mutation {
            FactMutation::Put { values } => values
                .get("owner")
                .and_then(crate::snapshot_observe::ObservedSourceOwner::from_value),
            FactMutation::Delete => None,
        };
        let evidence_unavailable = !view.can_read_observed_source_owners()
            && view
                .concrete(&write.key)
                .map_err(CommitError::Host)?
                .is_some();
        let Some(authorization) = authorization else {
            return Err(CommitError::ObservationAuthorization);
        };
        if evidence_unavailable || proposed.is_none() {
            return Err(CommitError::ObservationAuthorization);
        }
        let mut permitted = false;
        for (scope, owner) in authorization.source_scopes() {
            if write.key.key != format!("{}/{}", scope.key.object_type.as_str(), scope.key.key)
                || proposed.as_ref() != Some(owner)
            {
                continue;
            }
            let guarded = commit.reads.contains(scope)
                || commit.writes.iter().any(|source_write| {
                    source_write.key == scope.key && source_write.expected == scope.expected
                });
            let materialized = commit.writes.iter().any(|source_write| {
                source_write.key == scope.key
                    && source_write.expected == scope.expected
                    && matches!(source_write.mutation, FactMutation::Put { .. })
            });
            // Ownership is authority: a guarded unchanged read of an authored
            // object (or an observed edit of it) cannot grant outgoing rights.
            let new_source = view
                .concrete(&scope.key)
                .map_err(CommitError::Host)?
                .is_none();
            let successor = retained.as_ref().is_some_and(|old| {
                authorization.permits_source_owner_recreation(&scope.key, old, write)
            }) && recreates_observed_source(
                commit,
                view,
                &scope.key.object_type,
                &scope.key.key,
            )?;
            permitted |= guarded
                && (retained.as_ref() == Some(owner)
                    || materialized && new_source && retained.is_none()
                    || successor);
        }
        if !permitted {
            return Err(CommitError::ObservationAuthorization);
        }
    }
    Ok(())
}

fn recreates_observed_source<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    view: &V,
    source_type: &ObjectTypeName,
    source_key: &str,
) -> Result<bool, CommitError<V::Error>> {
    let tombstone =
        crate::snapshot_observe::tombstone_key(&FactKey::new(source_type.as_str(), source_key));
    Ok(commit
        .writes
        .iter()
        .any(|write| write.key == tombstone && matches!(write.mutation, FactMutation::Delete))
        && view
            .concrete(&tombstone)
            .map_err(CommitError::Host)?
            .is_some())
}

/// One invariant or implicit property check that did not hold (or could not
/// be evaluated) for a final object value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantFailure {
    /// The object type or interface that owns the invariant.
    pub owner: InvariantOwner,
    /// The invariant identity within its owner.
    pub id: InvariantId,
    /// The invariant's human-readable refusal message.
    pub message: String,
    /// Why evaluation did not produce `true`.
    pub outcome: DataQualityOutcome,
    /// Whether the failure refuses command writes or remains an expectation.
    pub classification: ConstraintClass,
    /// A more specific observation finding outcome for implicit checks.
    pub finding_outcome: Option<DataQualityOutcome>,
}

/// A bounded failure while evaluating the effective invariants of one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvariantEvaluationError {
    /// A submitted value or effective default cannot be materialized exactly.
    InvalidEffectiveValues(EffectiveValuesError),
    /// The bounded expression evaluation budget was exhausted.
    BudgetExceeded,
    /// The type references an interface absent from this catalog.
    MissingInterface(String),
}

/// Evaluate every implicit temporal property check and effective invariant of
/// `key` over the exact complete final values using one cumulative per-commit
/// budget. The caller shares `remaining` across all object writes in the
/// commit.
///
/// # Errors
///
/// Returns [`InvariantEvaluationError::InvalidEffectiveValues`] for a
/// submitted value or declared default that cannot be materialized,
/// [`InvariantEvaluationError::BudgetExceeded`] when the expression budget
/// is exhausted, or [`InvariantEvaluationError::MissingInterface`] when an
/// implemented interface is absent from the catalog.
pub fn evaluate_invariants(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
) -> Result<Vec<InvariantFailure>, InvariantEvaluationError> {
    evaluate_invariants_for_class(catalog, key, values, remaining, None)
}

/// Evaluate only integrity invariants and intrinsic temporal property checks.
///
/// Command admission uses this path so quality expectations consume no
/// dispatch or expression fuel. Observation preparation continues to call
/// [`evaluate_invariants`] and evaluates every declared class.
fn evaluate_integrity_invariants(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
) -> Result<Vec<InvariantFailure>, InvariantEvaluationError> {
    evaluate_invariants_for_class(
        catalog,
        key,
        values,
        remaining,
        Some(ConstraintClass::Integrity),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one cumulative evaluator preserves the same fuel and failure ordering for both admission modes"
)]
fn evaluate_invariants_for_class(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
    class: Option<ConstraintClass>,
) -> Result<Vec<InvariantFailure>, InvariantEvaluationError> {
    let values = catalog
        .effective_object_values(&key.object_type, values)
        .map_err(InvariantEvaluationError::InvalidEffectiveValues)?;
    let mut invariants = catalog.invariants(&key.object_type).map_err(|missing| {
        InvariantEvaluationError::MissingInterface(missing.as_str().to_owned())
    })?;
    if let Some(class) = class {
        invariants.retain(|effective| effective.invariant.classification == class);
    }

    let mut failures = evaluate_temporal_properties(catalog, key, &values, remaining)?;
    if invariants.is_empty() {
        return Ok(failures);
    }
    let dispatch_units = u64::try_from(invariants.len()).unwrap_or(u64::MAX);
    if dispatch_units > *remaining {
        return Err(InvariantEvaluationError::BudgetExceeded);
    }

    let object = values
        .iter()
        .map(|(name, value)| {
            crate::decision_commit::expr_value(value).map(|value| (name.clone(), value))
        })
        .collect::<Result<BTreeMap<_, _>, _>>();
    let object = match object {
        Ok(object) => object,
        Err(reason) => {
            *remaining = (*remaining).saturating_sub(dispatch_units);
            failures.extend(invariants.into_iter().map(|effective| InvariantFailure {
                owner: effective.owner,
                id: effective.id,
                message: effective.invariant.message.clone(),
                outcome: DataQualityOutcome::EvaluationFailed {
                    reason: reason.clone(),
                },
                classification: effective.invariant.classification,
                finding_outcome: None,
            }));
            return Ok(failures);
        }
    };
    let context = ExprValue::object([("self", ExprValue::Object(object))]);

    for effective in invariants {
        // Charge one deterministic unit to dispatch each declaration. This
        // also bounds a catalog with many zero-cost/invalid expressions.
        if *remaining == 0 {
            return Err(InvariantEvaluationError::BudgetExceeded);
        }
        *remaining = (*remaining).saturating_sub(1);
        let expression = match Expression::compile(effective.invariant.expression.as_str()) {
            Ok(expression) => expression,
            Err(error) => {
                failures.push(InvariantFailure {
                    owner: effective.owner,
                    id: effective.id,
                    message: effective.invariant.message.clone(),
                    outcome: DataQualityOutcome::EvaluationFailed {
                        reason: error.to_string(),
                    },
                    classification: effective.invariant.classification,
                    finding_outcome: None,
                });
                continue;
            }
        };
        let attempt = expression.evaluate_metered(&context, EvaluationBudget::new(*remaining));
        *remaining = (*remaining).saturating_sub(attempt.units_used);
        match attempt.result {
            Err(ExprError::BudgetExhausted { .. }) => {
                return Err(InvariantEvaluationError::BudgetExceeded);
            }
            Err(error) => failures.push(InvariantFailure {
                owner: effective.owner,
                id: effective.id,
                message: effective.invariant.message.clone(),
                outcome: DataQualityOutcome::EvaluationFailed {
                    reason: error.to_string(),
                },
                classification: effective.invariant.classification,
                finding_outcome: None,
            }),
            Ok(evaluation) => match evaluation.value() {
                ExprValue::Bool(true) => {}
                ExprValue::Bool(false) => failures.push(InvariantFailure {
                    owner: effective.owner,
                    id: effective.id,
                    message: effective.invariant.message.clone(),
                    outcome: DataQualityOutcome::NotSatisfied,
                    classification: effective.invariant.classification,
                    finding_outcome: None,
                }),
                other @ (ExprValue::Null
                | ExprValue::Number(_)
                | ExprValue::String(_)
                | ExprValue::Array(_)
                | ExprValue::Object(_)) => failures.push(InvariantFailure {
                    owner: effective.owner,
                    id: effective.id,
                    message: effective.invariant.message.clone(),
                    outcome: DataQualityOutcome::NonBoolean {
                        actual: expression_type_name(other).to_owned(),
                    },
                    classification: effective.invariant.classification,
                    finding_outcome: None,
                }),
            },
        }
    }

    Ok(failures)
}

fn evaluate_temporal_properties(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
) -> Result<Vec<InvariantFailure>, InvariantEvaluationError> {
    let properties = catalog
        .effective_type(&key.object_type)
        .into_iter()
        .flat_map(|object_type| &object_type.properties)
        .filter(|property| {
            !property.computed
                && matches!(
                    &property.kind,
                    EffectiveKind::Base(PropertyType::Date {} | PropertyType::Timestamp { .. })
                )
        })
        .collect::<Vec<_>>();
    let units = u64::try_from(properties.len()).unwrap_or(u64::MAX);
    if units > *remaining {
        return Err(InvariantEvaluationError::BudgetExceeded);
    }
    *remaining = (*remaining).saturating_sub(units);

    Ok(properties
        .into_iter()
        .filter_map(|property| {
            let value = values.get(property.id.as_str())?;
            property.check_runtime_value(value).err().map(|_| {
                let (kind, reason) = match &property.kind {
                    EffectiveKind::Base(PropertyType::Date {}) => {
                        ("date", "must be a valid calendar date in YYYY-MM-DD form")
                    }
                    EffectiveKind::Base(_)
                    | EffectiveKind::Registry(_)
                    | EffectiveKind::Unresolved => (
                        "timestamp",
                        "must be an RFC 3339 date-time with an admitted explicit offset",
                    ),
                };
                InvariantFailure {
                    owner: InvariantOwner::Object(key.object_type.clone()),
                    id: InvariantId(format!("$property.{}", property.id.as_str())),
                    message: format!("property `{}` must be a valid {kind}", property.id.as_str()),
                    outcome: DataQualityOutcome::NotSatisfied,
                    classification: ConstraintClass::Integrity,
                    finding_outcome: Some(DataQualityOutcome::InvalidTemporalObservation {
                        property: property.id.clone(),
                        value: value
                            .as_str()
                            .map_or_else(|| format!("{value:?}"), str::to_owned),
                        reason: reason.to_owned(),
                    }),
                }
            })
        })
        .collect())
}

/// The existing per-commit invariant evaluation fuel ceiling.
#[must_use]
pub fn invariant_evaluation_budget() -> u64 {
    EvaluationBudget::default().units()
}

fn expression_type_name(value: &ExprValue) -> &'static str {
    match value {
        ExprValue::Null => "null",
        ExprValue::Bool(_) => "boolean",
        ExprValue::Number(_) => "number",
        ExprValue::String(_) => "string",
        ExprValue::Array(_) => "array",
        ExprValue::Object(_) => "object",
    }
}

enum InvariantAdmissionError {
    EffectiveValues {
        key: FactKey,
        reason: String,
    },
    BudgetExceeded {
        key: FactKey,
        limit: u64,
    },
    MissingInterface {
        key: FactKey,
        interface: String,
    },
    Violation {
        key: Box<FactKey>,
        owner: InvariantOwner,
        id: InvariantId,
        message: String,
        outcome: Box<DataQualityOutcome>,
    },
    ObservationAuthorization,
}

fn evaluate_commit_invariants(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
    include_quality: bool,
) -> Result<Vec<InvariantFailure>, InvariantAdmissionError> {
    let result = if include_quality {
        evaluate_invariants(catalog, key, values, remaining)
    } else {
        evaluate_integrity_invariants(catalog, key, values, remaining)
    };
    result.map_err(|error| match error {
        InvariantEvaluationError::InvalidEffectiveValues(error) => {
            InvariantAdmissionError::EffectiveValues {
                key: key.clone(),
                reason: error.to_string(),
            }
        }
        InvariantEvaluationError::BudgetExceeded => InvariantAdmissionError::BudgetExceeded {
            key: key.clone(),
            limit: EvaluationBudget::default().units(),
        },
        InvariantEvaluationError::MissingInterface(interface) => {
            InvariantAdmissionError::MissingInterface {
                key: key.clone(),
                interface,
            }
        }
    })
}

fn check_invariants<E, C, T>(
    commit: &CellCommit<E, C, T>,
    catalog: &TypeCatalog<'_>,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), InvariantAdmissionError> {
    let mut remaining = EvaluationBudget::default().units();
    for write in &commit.writes {
        let crate::cell_host::FactMutation::Put { values } = &write.mutation else {
            continue;
        };
        let has_observation_authority =
            authorization.is_some_and(|value| value.authorizes_write(write));
        let failures = evaluate_commit_invariants(
            catalog,
            &write.key,
            values,
            &mut remaining,
            has_observation_authority,
        )?;
        if let Some((actual, expected)) =
            authorization.and_then(|value| value.findings_for_write(catalog, write, &failures))
        {
            if !actual.is_ok_and(|actual| actual.as_slice() == expected) {
                return Err(InvariantAdmissionError::ObservationAuthorization);
            }
            continue;
        }
        if let Some(failure) = failures
            .into_iter()
            .find(|failure| failure.classification == ConstraintClass::Integrity)
        {
            return Err(InvariantAdmissionError::Violation {
                key: Box::new(write.key.clone()),
                owner: failure.owner,
                id: failure.id,
                message: failure.message,
                outcome: Box::new(failure.outcome),
            });
        }
    }
    if let Some(authorization) = authorization {
        type CachedReadInvariantStates = Vec<(crate::cell_host::JsonObject, Vec<InvariantFailure>)>;
        let mut read_invariant_cache: BTreeMap<(FactKey, FactVersion), CachedReadInvariantStates> =
            BTreeMap::new();
        for authorized in authorization.read_findings() {
            let cache_key = (authorized.key.clone(), authorized.expected);
            let cached_failures = read_invariant_cache.get(&cache_key).and_then(|states| {
                states
                    .iter()
                    .find(|(values, _)| values == &authorized.values)
                    .map(|(_, failures)| failures.clone())
            });
            let failures = if let Some(failures) = cached_failures {
                failures
            } else {
                let failures = evaluate_commit_invariants(
                    catalog,
                    &authorized.key,
                    &authorized.values,
                    &mut remaining,
                    true,
                )?;
                read_invariant_cache
                    .entry(cache_key)
                    .or_default()
                    .push((authorized.values.clone(), failures.clone()));
                failures
            };
            if !ObservationAuthorization::findings_for_read(catalog, authorized, &failures)
                .is_ok_and(|actual| actual.as_slice() == authorized.findings.as_slice())
            {
                return Err(InvariantAdmissionError::ObservationAuthorization);
            }
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
fn check_accounting_postings<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    type GroupKey = (String, String, String, u64, String);
    let mut groups = BTreeMap::<GroupKey, (FactKey, Vec<AccountingLegIr>)>::new();

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

fn check_shape<E, C, T, V: CommitView + ?Sized>(
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
                let proposed = concrete_after(commit, model, view, &canonical)?;
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
        let identity = (model.canonical(&read.source), read.link.clone());
        if !link_reads.insert(identity) {
            return Err(CommitError::DuplicateLinkRead {
                source_key: read.source.clone(),
                link: read.link.clone(),
            });
        }
        if read.targets.len() > MAX_LINK_READ_TARGETS {
            return Err(CommitError::LinkReadTooLarge {
                source_key: read.source.clone(),
                link: read.link.clone(),
                actual: read.targets.len(),
                max: MAX_LINK_READ_TARGETS,
            });
        }
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
            if canonical.source != model.canonical(&read.source)
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
        if read.targets.len() > MAX_LINK_READ_TARGETS {
            return Err(CommitError::LinkReadTooLarge {
                source_key: read.source.clone(),
                link: read.link.clone(),
                actual: read.targets.len(),
                max: MAX_LINK_READ_TARGETS,
            });
        }
        for target in &read.targets {
            check_link_key_identity(&target.key)?;
        }
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

fn check_versions<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    let expectations = commit
        .reads
        .iter()
        .map(|read| (&read.key, read.expected))
        .chain(
            commit
                .writes
                .iter()
                .map(|write| (&write.key, write.expected)),
        );
    for (key, expected) in expectations {
        let current = view
            .version(&model.canonical(key))
            .map_err(CommitError::Host)?;
        if current != expected {
            return Err(CommitError::Conflict {
                key: key.clone(),
                expected,
                current,
            });
        }
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
    for write in &commit.links {
        let current = view
            .link_version(&model.canonical_link(&write.key))
            .map_err(CommitError::Host)?;
        if current != write.expected {
            return Err(CommitError::LinkConflict {
                key: Box::new(write.key.clone()),
                expected: write.expected,
                current,
            });
        }
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
    Ok(())
}

/// The canonical live links of every object the commit's links and deletes
/// touch, once the commit's link writes apply.
fn live_links_after<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<BTreeMap<FactKey, BTreeSet<LinkKey>>, CommitError<V::Error>> {
    let mut objects = BTreeSet::new();
    for write in &commit.links {
        let key = model.canonical_link(&write.key);
        objects.insert(key.source);
        objects.insert(key.target);
    }
    for write in &commit.writes {
        // Scalar puts need no adjacency unless a required owned collection
        // must remain populated. Link writes and deletes still check all ends.
        let needs_owned_links = model
            .catalog
            .owned_collections(&write.key.object_type)
            .iter()
            .any(|collection| {
                model
                    .link(&write.key.object_type, collection.id.as_str())
                    .is_some_and(|link| link.cardinality.required)
            });
        if matches!(write.mutation, FactMutation::Delete) || needs_owned_links {
            objects.insert(model.canonical(&write.key));
        }
    }
    let mut after = BTreeMap::new();
    for object in objects {
        let live: BTreeSet<LinkKey> = view
            .links_of(&object)
            .map_err(CommitError::Host)?
            .iter()
            .map(|link| model.canonical_link(link))
            .filter(|link| link.source == object || link.target == object)
            .collect();
        after.insert(object, live);
    }
    for write in &commit.links {
        let key = model.canonical_link(&write.key);
        for end in [&key.source, &key.target] {
            if let Some(live) = after.get_mut(end) {
                match write.mutation {
                    LinkMutation::Add { .. } => {
                        live.insert(key.clone());
                    }
                    LinkMutation::Remove => {
                        live.remove(&key);
                    }
                }
            }
        }
    }
    Ok(after)
}

/// The concrete type of canonical object `canonical` once `commit`
/// applies: the type a write in the commit puts it as (a put is only made
/// as the live concrete type, or creates the object), none if the commit
/// deletes it, else its live concrete type.
fn concrete_after<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    canonical: &FactKey,
) -> Result<Option<ObjectTypeName>, CommitError<V::Error>> {
    match commit
        .writes
        .iter()
        .find(|fact| model.canonical(&fact.key) == *canonical)
    {
        Some(fact) => Ok(match fact.mutation {
            FactMutation::Put { .. } => Some(fact.key.object_type.clone()),
            FactMutation::Delete => None,
        }),
        None => view.concrete(canonical).map_err(CommitError::Host),
    }
}

fn check_endpoints<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    links: &BTreeMap<FactKey, BTreeSet<LinkKey>>,
) -> Result<(), CommitError<V::Error>> {
    for write in &commit.links {
        if !matches!(write.mutation, LinkMutation::Add { .. }) {
            continue;
        }
        let mut concrete = Vec::with_capacity(2);
        for endpoint in [&write.key.source, &write.key.target] {
            let found = concrete_after(commit, model, view, &model.canonical(endpoint))?
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
fn check_acyclic_links<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
) -> Result<(), CommitError<V::Error>> {
    let mut remaining = MAX_LINK_READ_TARGETS;
    let mut remaining_bytes = MAX_LINK_READ_BYTES;
    for write in &commit.links {
        if !matches!(write.mutation, LinkMutation::Add { .. }) {
            continue;
        }
        let key = model.canonical_link(&write.key);
        let source = concrete_after(commit, model, view, &key.source)?
            .unwrap_or_else(|| key.source.object_type.clone());
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
            let node_type = concrete_after(commit, model, view, &node)?
                .unwrap_or_else(|| node.object_type.clone());
            if model.link_owner(&node_type, &key.link) != owner {
                continue;
            }
            let targets = view
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
            for change in &commit.links {
                let changed = model.canonical_link(&change.key);
                if changed.source != node || changed.link != key.link {
                    continue;
                }
                match change.mutation {
                    LinkMutation::Add { .. } => {
                        if !outgoing.contains(&changed) {
                            charge_acyclic_bytes(
                                &mut remaining_bytes,
                                &key.source,
                                changed.target.key.len(),
                            )?;
                            outgoing.insert(changed);
                        }
                    }
                    LinkMutation::Remove => {
                        outgoing.remove(&changed);
                    }
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
fn check_required_links<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    // Index final types and link changes once. Repeatedly searching all
    // writes for each affected source would make bulk creates quadratic.
    let mut sources: BTreeMap<_, _> = commit
        .writes
        .iter()
        .map(|write| {
            let concrete = match write.mutation {
                FactMutation::Put { .. } => Some(write.key.object_type.clone()),
                FactMutation::Delete => None,
            };
            (model.canonical(&write.key), concrete)
        })
        .collect();
    let mut changes = BTreeMap::<(FactKey, String), Vec<(LinkKey, bool)>>::new();
    for write in &commit.links {
        let key = model.canonical_link(&write.key);
        if let std::collections::btree_map::Entry::Vacant(entry) = sources.entry(key.source.clone())
        {
            let concrete = view.concrete(entry.key()).map_err(CommitError::Host)?;
            entry.insert(concrete);
        }
        changes
            .entry((key.source.clone(), key.link.clone()))
            .or_default()
            .push((key, matches!(write.mutation, LinkMutation::Add { .. })));
    }
    let observed: BTreeSet<_> = authorization
        .into_iter()
        .flat_map(ObservationAuthorization::observed_sources)
        .map(|key| model.canonical(key))
        .collect();
    let mut roles_by_type = BTreeMap::new();
    for (source, concrete) in sources {
        if observed.contains(&source) {
            continue;
        }
        let Some(concrete) = concrete else {
            continue;
        };
        let roles = roles_by_type.entry(concrete.clone()).or_insert_with(|| {
            let roles: BTreeSet<_> = model
                .catalog
                .ancestry(&concrete)
                .into_iter()
                .flat_map(|def| def.links.iter().map(|link| link.id.as_str()))
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
            let role_changes = changes.get(&(source.clone(), role.to_owned()));
            // Added endpoints already passed admission and are live in the
            // final graph. A replacement therefore needs no snapshot scan.
            if role_changes.is_some_and(|changes| changes.iter().any(|(_, add)| *add)) {
                continue;
            }
            let excluded = role_changes
                .into_iter()
                .flatten()
                .map(|(key, _)| key.target.clone())
                .collect();
            let present = view
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

fn check_cardinality<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    links: &BTreeMap<FactKey, BTreeSet<LinkKey>>,
) -> Result<(), CommitError<V::Error>> {
    for write in &commit.links {
        if !matches!(write.mutation, LinkMutation::Add { .. }) {
            continue;
        }
        let key = model.canonical_link(&write.key);
        // The source's concrete type's declaration (endpoints passed, so the
        // source exists once the commit applies).
        let Some(declared) = concrete_after(commit, model, view, &key.source)?
            .and_then(|source| model.link(&source, &write.key.link))
        else {
            continue;
        };
        for (multiplicity, outgoing) in [
            (declared.cardinality.to, true),
            (declared.cardinality.from, false),
        ] {
            if multiplicity != Multiplicity::One {
                continue;
            }
            let endpoint = if outgoing { &key.source } else { &key.target };
            let count = links.get(endpoint).map_or(0, |live| {
                live.iter()
                    .filter(|link| {
                        link.link == key.link
                            && if outgoing {
                                link.source == *endpoint
                            } else {
                                link.target == *endpoint
                            }
                    })
                    .count()
            });
            if count > 1 {
                return Err(CommitError::LinkCardinality {
                    key: Box::new(write.key.clone()),
                    endpoint: endpoint.clone(),
                });
            }
        }
    }
    for (source, source_links) in links {
        let Some(concrete) = concrete_after(commit, model, view, source)? else {
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
            let declared = model.link(&concrete, &link);
            if declared.is_some_and(|declaration| declaration.cardinality.required)
                && !source_links
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

fn next_versions<E, C, T, X>(
    commit: &CellCommit<E, C, T>,
) -> Result<CommitVersions, CommitError<X>> {
    let written = commit
        .writes
        .iter()
        .map(|write| {
            write
                .expected
                .checked_next()
                .map(|version| WrittenVersion {
                    key: write.key.clone(),
                    version,
                })
                .ok_or(CommitError::Overflow)
        })
        .collect::<Result<_, _>>()?;
    let linked = commit
        .links
        .iter()
        .map(|write| {
            write
                .expected
                .checked_next()
                .map(|version| WrittenLinkVersion {
                    key: write.key.clone(),
                    version,
                })
                .ok_or(CommitError::Overflow)
        })
        .collect::<Result<_, _>>()?;
    Ok(CommitVersions { written, linked })
}
