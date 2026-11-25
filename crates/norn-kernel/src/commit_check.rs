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

use crate::cell_host::{
    CellCommit, CommitError, CommitVersions, Consumed, FactKey, FactMutation, FactVersion,
    FactWrite, LinkKey, LinkMutation, LinkReadTarget, LinkWrite, OwnedRowsCommitProof,
    PredicateQuery, PredicateReadMember,
};
pub(crate) use crate::commit::{CommitLinks, MAX_LINK_LAW_SUBJECTS};
use crate::commit::{PostState, check_commit_local};
pub use crate::commit::{check_commit_identifiers, check_fact_key_identity};
use crate::decision_commit::{EffectiveValuesError, TypeCatalog};
use crate::snapshot_observe::ObservationAuthorization;
use norn_checker::EffectiveKind;
use norn_expr::{EvaluationBudget, ExprError, Value as ExprValue};
use norn_ir::posting::AccountingError;
use norn_ir::{
    ConstraintClass, DataQualityOutcome, InvariantId, InvariantOwner, Link, ObjectTypeName,
    PropertyType,
};
use std::collections::{BTreeMap, BTreeSet};

/// The model facts a commit is checked against, from the cell's
/// [`TypeCatalog`].
#[derive(Debug, Clone, Copy)]
pub struct CommitModel<'m, 'a> {
    pub(crate) catalog: &'m TypeCatalog<'a>,
}

impl<'m, 'a> CommitModel<'m, 'a> {
    /// Whether `object_type` is a stored object type (not an interface).
    pub(crate) fn is_object_type(self, object_type: &ObjectTypeName) -> bool {
        self.catalog.contains(object_type)
    }

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
    /// narrow an inherited link), including extension-added members.
    #[must_use]
    pub fn link(&self, source: &ObjectTypeName, link: &str) -> Option<&'a Link> {
        self.catalog.ancestry(source).into_iter().find_map(|def| {
            self.catalog
                .declared_links(&def.name)
                .into_iter()
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
                self.catalog
                    .declared_links(&def.name)
                    .into_iter()
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
        let source = proposed_type(commit, *self, view, &self.canonical(&key.source))?
            .unwrap_or_else(|| key.source.object_type.clone());
        self.link_values(&source, key, values)
    }

    pub(crate) fn link_values<X>(
        self,
        source: &ObjectTypeName,
        key: &LinkKey,
        values: &crate::cell_host::JsonObject,
    ) -> Result<crate::cell_host::JsonObject, CommitError<X>> {
        self.catalog
            .effective_link_values(source, &key.link, values)
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

    /// Ownership of a fact and its complete outgoing membership partition.
    /// Local-only stores resolve every key within their one bound cell. A host
    /// with external or unresolved dependencies must override this method;
    /// applicability labels and caller hints are not ownership evidence.
    ///
    /// # Errors
    /// The host cannot resolve ownership under the commit's read snapshot.
    fn atomic_ownership(
        &self,
        _key: &FactKey,
    ) -> Result<crate::execution_profile::AtomicOwnership, Self::Error> {
        Ok(crate::execution_profile::AtomicOwnership::Local)
    }

    /// Ownership of the complete consumed timer, result or observation identity.
    /// Bound local-only stores own their delivery queues. Hosts that can resolve
    /// external or unknown queues must override this using snapshot evidence.
    ///
    /// # Errors
    /// Ownership cannot be resolved under the commit's read snapshot.
    fn consumed_ownership(
        &self,
        _consumed: &Consumed,
    ) -> Result<crate::execution_profile::AtomicOwnership, Self::Error> {
        Ok(crate::execution_profile::AtomicOwnership::Local)
    }

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

    /// Every live object matching `predicate`, as members in canonical key
    /// order, at most `limit` of them ([`crate::predicate_read`]). Hosts
    /// enumerate the queried type and apply [`PredicateQuery::matches`].
    /// `None` means enumeration is incomplete or this host cannot prove the
    /// set complete (for example, the type spans owners); a commit
    /// containing a predicate read is then refused.
    ///
    /// # Errors
    ///
    /// Returns the host read failure.
    fn predicate_members(
        &self,
        predicate: &PredicateQuery,
        limit: usize,
    ) -> Result<Option<Vec<PredicateReadMember>>, Self::Error> {
        let _ = (predicate, limit);
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

    /// Whether a live incoming source for `(link, target)` survives the
    /// excluded sources. `None` means the proof is incomplete within bounds.
    ///
    /// # Errors
    /// Returns the host read failure.
    fn has_link_source(
        &self,
        target: &FactKey,
        link: &str,
        excluded: &BTreeSet<FactKey>,
    ) -> Result<Option<bool>, Self::Error> {
        let _ = (target, link, excluded);
        Ok(None)
    }

    /// Every live link with the canonical object at either end, in any order
    /// and naming (the check canonicalizes them).
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Self::Error>;

    /// LAW-LINK-1: the current values of a live object, for a governing law
    /// that reads a link to it. `None` when the object is absent or this host
    /// cannot prove its values; such a law then refuses rather than guess.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn object_values(
        &self,
        key: &FactKey,
    ) -> Result<Option<crate::cell_host::JsonObject>, Self::Error> {
        let _ = key;
        Ok(None)
    }
}

/// The admission check every host runs before applying a commit.
///
/// The stage order below lives in one straight-line driver,
/// `commit_order::run_stages`, which is proved to run exactly this order and to
/// stop at the first refusal (`scripts/verify-kernel-commit-aeneas.sh`).
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
///    all written versions have a representable successor ([`CommitError::Overflow`]);
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
/// 7. **laws** — direct and affected surviving subjects satisfy their governing laws.
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
        || proof.predicate_reads() != commit.predicate_reads.as_slice()
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
    let profile = crate::execution_profile::ProfileView::new(view);
    profile.validate_consumed(commit.consumes.as_ref())?;
    let result = check_commit_local(commit, model, &profile, authorization, trusted_owned_rows);
    profile.validate(commit, model)?;
    result
}

/// The concrete type of canonical object `canonical` once `commit`
/// applies: the type a write in the commit puts it as (a put is only made
/// as the live concrete type, or creates the object), none if the commit
/// deletes it, else its live concrete type.
pub(crate) fn proposed_type<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    canonical: &FactKey,
) -> Result<Option<ObjectTypeName>, CommitError<V::Error>> {
    match commit
        .writes
        .iter()
        // `canonical` keeps the key: compare it first, the type walk second.
        .find(|fact| fact.key.key == canonical.key && model.canonical(&fact.key) == *canonical)
    {
        Some(fact) => Ok(match fact.mutation {
            FactMutation::Put { .. } => Some(fact.key.object_type.clone()),
            FactMutation::Delete => None,
        }),
        None => view.concrete(canonical).map_err(CommitError::Host),
    }
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

/// Evaluate every implicit temporal/decimal property check and effective invariant of
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
    evaluate_invariants_for_class(catalog, key, values, remaining, None, None)
}

/// LAW-COMMIT-1: evaluate a resolved invariant response with the shared value,
/// expression and fuel semantics, excluding unrelated declarations.
pub(crate) fn evaluate_named_invariant(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
    id: &str,
) -> Result<Vec<InvariantFailure>, InvariantEvaluationError> {
    evaluate_invariants_for_class(catalog, key, values, remaining, None, Some(id))
}

/// Evaluate only integrity invariants and intrinsic temporal/decimal property checks.
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
        None,
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
    selected: Option<&str>,
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
    if let Some(id) = selected {
        invariants.retain(|effective| effective.id.as_str() == id);
    }

    let mut failures = if selected.is_none() {
        evaluate_typed_properties(catalog, key, &values, remaining)?
    } else {
        Vec::new()
    };
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
    let environment = catalog.shared_expression_environment(&key.object_type);

    for effective in invariants {
        // Charge one deterministic unit to dispatch each declaration. This
        // also bounds a catalog with many zero-cost/invalid expressions.
        if *remaining == 0 {
            return Err(InvariantEvaluationError::BudgetExceeded);
        }
        *remaining = (*remaining).saturating_sub(1);
        let expression = match catalog.compiled_expression(effective.invariant.expression.as_str())
        {
            Ok(expression) => expression,
            Err(reason) => {
                failures.push(InvariantFailure {
                    owner: effective.owner,
                    id: effective.id,
                    message: effective.invariant.message.clone(),
                    outcome: DataQualityOutcome::EvaluationFailed { reason },
                    classification: effective.invariant.classification,
                    finding_outcome: None,
                });
                continue;
            }
        };
        let attempt = match &environment {
            Ok(env) => expression.evaluate_prepared(
                &catalog.prepared_invariant(&key.object_type, &expression, env),
                &context,
                EvaluationBudget::new(*remaining),
                env,
                &norn_expr::Type::Bool,
            ),
            Err(reason) => norn_expr::EvaluationAttempt {
                result: Err(ExprError::Compile {
                    message: reason.clone(),
                }),
                units_used: 0,
            },
        };
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

/// The stored properties of `object_type` whose values the runtime domain
/// check reads ([`evaluate_typed_properties`]): each costs one unit of the
/// invariant budget, before any invariant is dispatched.
fn typed_properties<'c>(
    catalog: &'c TypeCatalog<'_>,
    object_type: &ObjectTypeName,
) -> Vec<&'c norn_checker::entity_instance::EffectiveProperty> {
    catalog
        .effective_type(object_type)
        .into_iter()
        .flat_map(|object_type| &object_type.properties)
        .filter(|property| {
            !property.computed
                && matches!(
                    &property.kind,
                    EffectiveKind::Base(
                        PropertyType::Date {}
                            | PropertyType::Timestamp { .. }
                            | PropertyType::Decimal { .. }
                            | PropertyType::Record { .. }
                    )
                )
        })
        .collect()
}

/// MANIFEST-LAWS-1 (S2): the units the invariant budget is charged for a
/// row of `object_type` before its first invariant: one per typed property
/// the runtime domain check reads. The dataset path adds it once per row
/// to its fuel bound.
pub(crate) fn typed_property_units(catalog: &TypeCatalog<'_>, object_type: &ObjectTypeName) -> u64 {
    u64::try_from(typed_properties(catalog, object_type).len()).unwrap_or(u64::MAX)
}

/// VAL-DECIMAL-SCALE-1: apply the shared runtime decimal domain check, including Record members, before commit.
fn evaluate_typed_properties(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
) -> Result<Vec<InvariantFailure>, InvariantEvaluationError> {
    if catalog.effective_view_refused(&key.object_type) {
        return Err(InvariantEvaluationError::InvalidEffectiveValues(
            crate::EffectiveValuesError::Unsupported {
                property: key.object_type.as_str().to_owned(),
                feature: "the effective type view is refused".to_owned(),
            },
        ));
    }
    let properties = typed_properties(catalog, &key.object_type);
    let units = u64::try_from(properties.len()).unwrap_or(u64::MAX);
    if units > *remaining {
        return Err(InvariantEvaluationError::BudgetExceeded);
    }
    *remaining = (*remaining).saturating_sub(units);

    properties
        .into_iter()
        .filter_map(|property| {
            let value = values.get(property.id.as_str())?;
            let error = property.check_runtime_value(value).err()?;
            if let norn_checker::entity_instance::InstanceError::Unsupported(reason) = error {
                return Some(Err(InvariantEvaluationError::InvalidEffectiveValues(
                    crate::EffectiveValuesError::Unsupported {
                        property: property.id.as_str().to_owned(),
                        feature: reason,
                    },
                )));
            }
            Some(Ok({
                let (kind, reason) = match &property.kind {
                    EffectiveKind::Base(PropertyType::Decimal { .. }) => (
                        "decimal",
                        "must satisfy its declared decimal scale and bounds",
                    ),
                    EffectiveKind::Base(PropertyType::Record { .. }) => (
                        "record",
                        "must satisfy its declared member types and bounds",
                    ),
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
                    finding_outcome: (!matches!(
                        &property.kind,
                        EffectiveKind::Base(
                            PropertyType::Decimal { .. } | PropertyType::Record { .. }
                        )
                    ))
                    .then(|| DataQualityOutcome::InvalidTemporalObservation {
                        property: property.id.clone(),
                        value: value
                            .as_str()
                            .map_or_else(|| format!("{value:?}"), str::to_owned),
                        reason: reason.to_owned(),
                    }),
                }
            }))
        })
        .collect()
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

pub(crate) enum InvariantAdmissionError {
    Law {
        key: FactKey,
        reason: crate::governing_laws::LawEvaluationError,
    },
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
        version: String,
        id: InvariantId,
        message: String,
        outcome: Box<DataQualityOutcome>,
    },
    ObservationAuthorization,
}

impl InvariantAdmissionError {
    pub(crate) fn into_commit_error<X>(self) -> CommitError<X> {
        match self {
            Self::Law { key, reason } => CommitError::Law { key, reason },
            Self::EffectiveValues { key, reason } => CommitError::EffectiveValues { key, reason },
            Self::BudgetExceeded { key, limit } => {
                CommitError::InvariantBudgetExceeded { key, limit }
            }
            Self::MissingInterface { key, interface } => {
                CommitError::InvariantModel { key, interface }
            }
            Self::Violation {
                key,
                owner,
                version,
                id,
                message,
                outcome,
            } => CommitError::InvariantViolation {
                key,
                owner,
                version,
                id,
                message,
                outcome,
            },
            Self::ObservationAuthorization => CommitError::ObservationAuthorization,
        }
    }
}

/// LAW-LINK-1: the observed-write invariant failures admission recomputes,
/// for a finding refresh that must match it exactly.
pub(crate) fn observed_invariant_failures(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut u64,
) -> Option<Vec<InvariantFailure>> {
    evaluate_commit_invariants(catalog, key, values, remaining, true).ok()
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

#[allow(
    clippy::too_many_lines,
    reason = "one stage recomputes written and read-bound findings over the same links"
)]
pub(crate) fn check_invariants<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), InvariantAdmissionError> {
    let model = post.model();
    let catalog = model.catalog;
    let mut remaining = EvaluationBudget::default().units();
    let authorized = authorization.map(ObservationAuthorization::indexed);
    for write in &commit.writes {
        let crate::cell_host::FactMutation::Put { values } = &write.mutation else {
            continue;
        };
        let has_observation_authority = authorized
            .as_ref()
            .is_some_and(|index| index.authorizes(write));
        let failures = evaluate_commit_invariants(
            catalog,
            &write.key,
            values,
            &mut remaining,
            has_observation_authority,
        )?;
        let mut links = CommitLinks::new(post, &write.key);
        if let Some((actual, expected)) = authorized.as_ref().and_then(|index| {
            ObservationAuthorization::findings_for_write(
                index, catalog, write, &failures, &mut links,
            )
        }) {
            if links.take_host_error().is_some() {
                return Err(InvariantAdmissionError::ObservationAuthorization);
            }
            if !actual.is_ok_and(|actual| actual.as_slice() == expected) {
                return Err(InvariantAdmissionError::ObservationAuthorization);
            }
            continue;
        }
        if let Some(failure) = failures
            .into_iter()
            .find(|failure| failure.classification == ConstraintClass::Integrity)
        {
            if let Some(law_failure) =
                crate::governing_laws::invariant_failure(catalog, &write.key.object_type, &failure)
                    .map_err(|reason| InvariantAdmissionError::Law {
                        key: write.key.clone(),
                        reason,
                    })?
            {
                return Err(InvariantAdmissionError::Law {
                    key: write.key.clone(),
                    reason: crate::governing_laws::LawEvaluationError::Violated(Box::new(
                        law_failure,
                    )),
                });
            }
            return Err(InvariantAdmissionError::Violation {
                key: Box::new(write.key.clone()),
                version: catalog
                    .invariant_owner_version(&failure.owner)
                    .map_err(|error| InvariantAdmissionError::EffectiveValues {
                        key: write.key.clone(),
                        reason: error.to_string(),
                    })?
                    .ok_or_else(|| InvariantAdmissionError::EffectiveValues {
                        key: write.key.clone(),
                        reason: "invariant owner is unavailable".to_owned(),
                    })?,
                owner: failure.owner,
                id: failure.id,
                message: failure.message,
                outcome: Box::new(failure.outcome),
            });
        }
    }
    if let Some(authorization) = authorization {
        check_read_findings(post, authorization, &mut remaining)?;
    }
    Ok(())
}

/// The read-bound findings `authorization` seals, recomputed over the state
/// the commit leaves; each must match exactly and be guarded by a read of its
/// own object.
fn check_read_findings<V: CommitView + ?Sized>(
    post: &PostState<'_, '_, V>,
    authorization: &ObservationAuthorization,
    remaining: &mut u64,
) -> Result<(), InvariantAdmissionError> {
    type CachedReadInvariantStates = Vec<(crate::cell_host::JsonObject, Vec<InvariantFailure>)>;
    let model = post.model();
    let catalog = model.catalog;
    let mut read_invariant_cache: BTreeMap<(FactKey, FactVersion), CachedReadInvariantStates> =
        BTreeMap::new();
    for authorized in authorization.read_findings() {
        // #1701: the sealed subject and the read that guards it must be one
        // object under this catalog, or the read guards another object.
        if model.canonical(&authorized.key) != model.canonical(&authorized.read_key) {
            return Err(InvariantAdmissionError::ObservationAuthorization);
        }
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
                remaining,
                true,
            )?;
            read_invariant_cache
                .entry(cache_key)
                .or_default()
                .push((authorized.values.clone(), failures.clone()));
            failures
        };
        let mut links = CommitLinks::new(post, &authorized.key);
        if !ObservationAuthorization::findings_for_read(catalog, authorized, &failures, &mut links)
            .is_ok_and(|actual| actual.as_slice() == authorized.findings.as_slice())
            || links.take_host_error().is_some()
        {
            return Err(InvariantAdmissionError::ObservationAuthorization);
        }
    }
    Ok(())
}

/// Outgoing links share the source's authority, not their target's authority.
/// Authored incoming edges remain authored; pending target resolution carries
/// the original source's sealed scope and exact relation intent.
///
/// OBS-LINK-CONVERGE-1: only opaque claim edges share canonical relation authority.
/// Authored writes retain their namespace and claim-metadata protections.
#[allow(
    clippy::too_many_lines,
    reason = "one shared boundary fences source ownership, pending intents, and outgoing relations atomically"
)]
pub(crate) fn check_link_authority<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    let view = post.before();
    // One write index for every claim write (not a scan of the authorized
    // writes per claim).
    let indexed = authorization.map(ObservationAuthorization::indexed);
    if commit.writes.iter().any(|write| {
        model.canonical(&write.key).object_type.as_str()
            == norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE
            && !authorization
                .zip(indexed.as_ref())
                .is_some_and(|(a, index)| a.authorizes_claim_write(index, write))
    }) {
        return Err(CommitError::ObservationAuthorization);
    }
    if authorization.is_none()
        && commit
            .writes
            .iter()
            .any(|write| write.key.object_type.as_str() == "norn.PendingLinks")
    {
        return Err(CommitError::ObservationAuthorization);
    }
    check_source_markers(commit, view, authorization)?;
    let mut convergent: BTreeMap<&crate::LinkKey, Vec<&crate::LinkWrite>> = BTreeMap::new();
    for write in authorization.map_or_else(Vec::new, ObservationAuthorization::convergent_links) {
        convergent.entry(&write.key).or_default().push(write);
    }
    let sources: BTreeSet<_> = commit
        .links
        .iter()
        .filter(|write| {
            !convergent
                .get(&write.key)
                .is_some_and(|same| same.contains(write))
        })
        .map(|write| model.canonical(&write.key.source))
        .chain(
            authorization
                .into_iter()
                .flat_map(ObservationAuthorization::pending_sources)
                .map(|source| model.canonical(source)),
        )
        .collect();
    for source in sources {
        // Ownership fences the old identity even when this commit removes the row.
        let concrete = view.concrete(&source).map_err(CommitError::Host)?;
        let source_type = if let Some(old) = &concrete {
            old.clone()
        } else {
            post.concrete(&source)?
                .unwrap_or_else(|| source.object_type.clone())
        };
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
        }) && recreates_observed_source(commit, view, &source_type, &source.key)?;
        if scopes.any(|(_, other)| other != owner)
            || !commit.reads.contains(scope)
                && !commit.writes.iter().any(|write| {
                    write.key.key == source.key
                        && model.canonical(&write.key) == source
                        && write.expected == scope.expected
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
