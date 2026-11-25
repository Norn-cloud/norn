//! ACTOR-STAMP-1 (#1091, #1101): the protected creator stamp of a governed
//! create, from kernel preparation through shared host admission.
//!
//! A protected property (FA-PROTECT-1) is written only by the kernel's stamp
//! of the verified principal on the create that stamps it. Decision
//! preparation admits that one value ([`CreatorStamp`]); shared host
//! admission ([`check_protected`]) keeps every other commit, including
//! a direct [`crate::CellHost::commit`], from creating, changing, clearing or
//! deleting a protected value. Only a [`CreatorStampProof`], which the kernel
//! alone constructs, carries a stamp to host admission.

use std::collections::BTreeSet;

use crate::cell_host::{
    Admission, Authorized, CellCommit, CommitError, CommitVersions, Consumed, FactExpectation,
    FactKey, FactMutation, FactWrite, JsonObject, JsonValue, LinkMutation, LinkReadExpectation,
    LinkWrite, ObjectTypeName, OwnedRowsCommitProof, PredicateReadExpectation, Proposed,
};
use crate::commit_check::{CommitModel, CommitView};
use crate::snapshot_observe::ObservationAuthorization;

/// The protected creator properties one governed create may write, each only
/// with the verified principal that executes the call.
///
/// The kernel builds it from the action's server-stamp derivations; no
/// caller, rule output, observation or update can construct one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatorStamp {
    /// The object the action creates.
    pub(crate) key: FactKey,
    /// The protected properties stamped with the principal.
    pub(crate) properties: BTreeSet<String>,
    /// The verified execution principal.
    pub(crate) principal: String,
}

impl CreatorStamp {
    /// Whether `value` of `property` on `key` is exactly this stamp.
    pub(crate) fn permits(&self, key: &FactKey, property: &str, value: &JsonValue) -> bool {
        self.key == *key
            && self.properties.contains(property)
            && value.as_str() == Some(self.principal.as_str())
    }
}

/// One-use in-process authority for the protected creator stamp of a
/// kernel-prepared governed create.
///
/// The proof has no wire representation and no public constructor. The
/// kernel creates it only after it has prepared and authorized the create,
/// and host admission consumes it ([`check_commit_stamped`]).
#[derive(Debug)]
pub struct CreatorStampProof {
    stamp: CreatorStamp,
    consumes: Option<Consumed>,
    reads: Vec<FactExpectation>,
    writes: Vec<FactWrite>,
    links: Vec<LinkWrite>,
    link_reads: Vec<LinkReadExpectation>,
    predicate_reads: Vec<PredicateReadExpectation>,
    max_events: u32,
}

impl CreatorStampProof {
    /// Bind `stamp` to the exact prepared transaction, as the owned-row
    /// proof is: its fact and link writes, read and version fences, consumed
    /// delivery, link and predicate reads, and the admitted event ceiling.
    pub(crate) fn new<E, C, T>(
        stamp: CreatorStamp,
        commit: &CellCommit<E, C, T>,
        max_events: u32,
    ) -> Self {
        Self {
            stamp,
            consumes: commit.consumes.clone(),
            reads: commit.reads.clone(),
            writes: commit.writes.clone(),
            links: commit.links.clone(),
            link_reads: commit.link_reads.clone(),
            predicate_reads: commit.predicate_reads.clone(),
            max_events,
        }
    }

    /// ACTOR-STAMP-1: refuse a final ledger payload above the admitted event
    /// ceiling. `events` counts the commit's events, its created links (as
    /// the action's envelope charges them) and every event the host attaches
    /// to the same transaction.
    ///
    /// # Errors
    ///
    /// [`CommitError::EventLimitExceeded`] when `events` exceeds the ceiling.
    pub fn check_events<X>(&self, events: usize) -> Result<(), CommitError<X>> {
        if u32::try_from(events).map_or(true, |actual| actual > self.max_events) {
            return Err(CommitError::EventLimitExceeded {
                limit: self.max_events,
                actual: events,
            });
        }
        Ok(())
    }

    /// Whether `commit` is the transaction this proof was issued for. A
    /// perimeter adapter may add reads, never drop or change a sealed one.
    fn binds<E, C, T, V: CommitView + ?Sized>(
        &self,
        commit: &CellCommit<E, C, T>,
        model: CommitModel<'_, '_>,
        view: &V,
    ) -> Result<bool, CommitError<V::Error>> {
        if self.consumes != commit.consumes
            || self.reads.iter().any(|read| !commit.reads.contains(read))
            || self.link_reads != commit.link_reads
            || self.predicate_reads != commit.predicate_reads
            || self.writes.len() != commit.writes.len()
            || self.links.len() != commit.links.len()
        {
            return Ok(false);
        }
        let links = self
            .links
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
        Ok(links == commit.links
            && canonical_writes::<V::Error>(model, &self.writes)? == commit.writes)
    }
}

/// Kernel-prepared writes as host admission sees them: each `Put` through
/// the effective-value builder. Shared by the owned-row and stamp proofs.
///
/// # Errors
///
/// [`CommitError::EffectiveValues`] when a prepared value is invalid.
pub(crate) fn canonical_writes<X>(
    model: CommitModel<'_, '_>,
    writes: &[FactWrite],
) -> Result<Vec<FactWrite>, CommitError<X>> {
    writes
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
        .collect()
}

impl<'c, E, C, T> Proposed<'c, E, C, T> {
    /// Select the admission from the host's authority: an observation
    /// authorization, an owned-rows proof, or neither.
    ///
    /// # Errors
    ///
    /// [`CommitError::ObservationAuthorization`] when both are supplied.
    pub fn authorize<X>(
        self,
        authorization: Option<&'c ObservationAuthorization>,
        owned_rows: Option<OwnedRowsCommitProof>,
    ) -> Result<Authorized<'c, E, C, T>, CommitError<X>> {
        let admission = match (authorization, owned_rows) {
            (None, None) => Admission::Plain,
            (None, Some(proof)) => Admission::OwnedRows(Box::new(proof)),
            (Some(authorization), None) => Admission::Observed(authorization),
            (Some(_), Some(_)) => return Err(CommitError::ObservationAuthorization),
        };
        Ok(Authorized {
            commit: self.commit,
            admission,
        })
    }

    /// [`Self::authorize`], or the admission of a kernel-prepared governed
    /// create when `stamp` is given (ACTOR-STAMP-1).
    ///
    /// # Errors
    ///
    /// [`CommitError::ObservationAuthorization`] when more than one authority
    /// is supplied.
    pub fn authorize_with<X>(
        self,
        authorization: Option<&'c ObservationAuthorization>,
        owned_rows: Option<OwnedRowsCommitProof>,
        stamp: Option<CreatorStampProof>,
    ) -> Result<Authorized<'c, E, C, T>, CommitError<X>> {
        match stamp {
            None => self.authorize(authorization, owned_rows),
            Some(_) if authorization.is_some() || owned_rows.is_some() => {
                Err(CommitError::ObservationAuthorization)
            }
            Some(proof) => Ok(Authorized {
                commit: self.commit,
                admission: Admission::Stamped(Box::new(proof)),
            }),
        }
    }
}

/// The kernel authority a commit's shared admission runs under.
#[derive(Debug, Clone, Copy)]
pub(crate) enum KernelTrust<'a> {
    /// An ordinary or observed commit.
    Plain,
    /// The kernel-prepared parent and owned-row commit.
    OwnedRows,
    /// A kernel-prepared governed create carrying its creator stamp.
    Stamped(&'a CreatorStamp),
}

impl<'a> KernelTrust<'a> {
    /// The owned-row admission when its proof matched, else the plain one.
    pub(crate) const fn owned(trusted: bool) -> Self {
        if trusted {
            Self::OwnedRows
        } else {
            Self::Plain
        }
    }

    /// Whether this is the trusted owned-row admission.
    pub(crate) const fn owned_rows(self) -> bool {
        matches!(self, Self::OwnedRows)
    }

    /// The creator stamp a governed create's admission carries.
    pub(crate) const fn stamp(self) -> Option<&'a CreatorStamp> {
        match self {
            Self::Stamped(stamp) => Some(stamp),
            Self::Plain | Self::OwnedRows => None,
        }
    }
}

/// Check a kernel-prepared governed create under its creator stamp: the
/// ordinary admission, except that the one stamped protected value of the
/// created object is admitted when the commit is exactly the transaction the
/// proof was issued for. Any other commit gets the plain admission, which
/// refuses the stamped value.
///
/// # Errors
///
/// The first failed check, or [`CommitError::Host`] if a lookup failed.
pub fn check_commit_stamped<E: 'static, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: &CommitModel<'_, '_>,
    view: &V,
    proof: &CreatorStampProof,
) -> Result<CommitVersions, CommitError<V::Error>> {
    // Recheck the final ledger payload against the admitted envelope; a host
    // that attaches events checks them too (`CreatorStampProof::check_events`).
    proof.check_events::<V::Error>(commit.events.len().saturating_add(commit.links.len()))?;
    let trust = if proof.binds(commit, *model, view)? {
        KernelTrust::Stamped(&proof.stamp)
    } else {
        KernelTrust::Plain
    };
    crate::commit_check::check_commit_inner(commit, *model, view, None, trust)
}

/// ACTOR-STAMP-1 / FA-PROTECT-1 at shared admission. For every written object
/// whose effective type has protected properties: a delete refuses; a create
/// may hold a protected value only when it is the declared default or the
/// kernel's stamp; an update must keep every protected value unchanged, and a
/// host that cannot prove the stored values refuses. The owned-row admission
/// carries its own kernel authority and is exempt, and so is each exact write
/// a sealed observation's kernel-prepared authorization covers
/// (`index.authorizes(write)`, as the lifecycle, invariant, law and version
/// stages exempt it): every other write in an observed commit is checked.
///
/// # Errors
///
/// [`CommitError::ImmutableObject`] for the first refused write, or
/// [`CommitError::Host`] if a lookup failed.
pub(crate) fn check_protected<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
    trust: KernelTrust<'_>,
) -> Result<(), CommitError<V::Error>> {
    let stamp = match trust {
        KernelTrust::Stamped(stamp) => Some(stamp),
        KernelTrust::OwnedRows => return Ok(()),
        KernelTrust::Plain => None,
    };
    let authorized = authorization.map(ObservationAuthorization::indexed);
    for write in &commit.writes {
        // Only the exact write a sealed observation prepared is exempt; any
        // other write in the same commit is an ordinary command mutation.
        if authorized
            .as_ref()
            .is_some_and(|index| index.authorizes(write))
        {
            continue;
        }
        let key = model.canonical(&write.key);
        // A stale write (a concurrent create or update won) is the retryable
        // `Conflict` the version check reports, never a protected refusal.
        if view.version(&key).map_err(CommitError::Host)? != write.expected {
            continue;
        }
        let live = view.concrete(&key).map_err(CommitError::Host)?;
        let object_type = live.as_ref().unwrap_or(&write.key.object_type);
        let Some(effective) = model.catalog.effective_type(object_type) else {
            continue;
        };
        let protected: Vec<&norn_checker::EffectiveProperty> = effective
            .properties
            .iter()
            .filter(|property| property.protected())
            .collect();
        if protected.is_empty() {
            continue;
        }
        let refused = || CommitError::ImmutableObject {
            key: write.key.clone(),
        };
        let FactMutation::Put { values } = &write.mutation else {
            return Err(refused());
        };
        let before: Option<JsonObject> = if live.is_some() {
            Some(
                view.object_values(&key)
                    .map_err(CommitError::Host)?
                    .ok_or_else(refused)?,
            )
        } else {
            None
        };
        for property in protected {
            let id = property.id.as_str();
            let value = present(values, id);
            let admitted = before.as_ref().map_or_else(
                || {
                    value.is_none_or(|value| {
                        is_default(model, object_type, property, value)
                            || stamp.is_some_and(|stamp| stamp.permits(&write.key, id, value))
                    })
                },
                |before| value == present(before, id),
            );
            if !admitted {
                return Err(refused());
            }
        }
    }
    Ok(())
}

/// A non-null value of `property`, if any.
fn present<'v>(values: &'v JsonObject, property: &str) -> Option<&'v JsonValue> {
    values
        .get(property)
        .filter(|value| !matches!(value, JsonValue::Null))
}

/// Whether `value` is the property's declared default, both compared as the
/// effective-value builder canonicalizes them (a timestamp's offset, an
/// exact decimal's scale), as hosts store them.
fn is_default(
    model: CommitModel<'_, '_>,
    object_type: &ObjectTypeName,
    property: &norn_checker::EffectiveProperty,
    value: &JsonValue,
) -> bool {
    let id = property.id.as_str();
    let canonical = |value: JsonValue| {
        model
            .catalog
            .effective_object_values(object_type, &JsonObject::from([(id.to_owned(), value)]))
            .ok()
            .and_then(|mut values| values.remove(id))
    };
    property
        .default
        .as_ref()
        .and_then(|default| property.runtime_default_literal(&default.0).ok())
        .and_then(canonical)
        .is_some_and(|default| Some(default) == canonical(value.clone()))
}
