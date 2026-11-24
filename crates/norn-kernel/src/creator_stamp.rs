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
    Admission, Authorized, CellCommit, CommitError, CommitVersions, FactKey, FactMutation,
    JsonObject, JsonValue, OwnedRowsCommitProof, Proposed,
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
pub struct CreatorStampProof(CreatorStamp);

impl CreatorStampProof {
    pub(crate) const fn new(stamp: CreatorStamp) -> Self {
        Self(stamp)
    }
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

impl KernelTrust<'_> {
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
}

/// Check a kernel-prepared governed create under its creator stamp: the
/// ordinary admission, except that the one stamped protected value of the
/// created object is admitted.
///
/// # Errors
///
/// The first failed check, or [`CommitError::Host`] if a lookup failed.
pub fn check_commit_stamped<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: &CommitModel<'_, '_>,
    view: &V,
    proof: &CreatorStampProof,
) -> Result<CommitVersions, CommitError<V::Error>> {
    crate::commit_check::check_commit_inner(
        commit,
        *model,
        view,
        None,
        KernelTrust::Stamped(&proof.0),
    )
}

/// ACTOR-STAMP-1 / FA-PROTECT-1 at shared admission. For every written object
/// whose effective type has protected properties: a delete refuses; a create
/// may hold a protected value only when it is the declared default or the
/// kernel's stamp; an update must keep every protected value unchanged, and a
/// host that cannot prove the stored values refuses. Owned-row and sealed
/// observation admissions carry their own kernel authority and are exempt.
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
        KernelTrust::Plain if authorization.is_some() => return Ok(()),
        KernelTrust::Plain => None,
    };
    for write in &commit.writes {
        let key = model.canonical(&write.key);
        let live = view.concrete(&key).map_err(CommitError::Host)?;
        let Some(effective) = model
            .catalog
            .effective_type(live.as_ref().unwrap_or(&write.key.object_type))
        else {
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
                        is_default(property, value)
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

/// Whether `value` is the property's declared default.
fn is_default(property: &norn_checker::EffectiveProperty, value: &JsonValue) -> bool {
    property
        .default
        .as_ref()
        .and_then(|default| property.runtime_default_literal(&default.0).ok())
        .is_some_and(|default| default == *value)
}
