//! The reference model's shape admission: what `norn_kernel::check_commit`
//! refuses before delivery, written out for the model (#1701).

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use norn_kernel::{CommitError, CommitModel, FactKey, FactState, FactVersion};

use super::LawCommit;

/// One expectation per canonical object: a second write is a duplicate; a
/// read through any alias merges at one version, else is stale. No link
/// twice, and every link a declared link of its source to an admitted target.
pub(super) fn admit_shape(
    facts: &BTreeMap<FactKey, FactState>,
    commit: &LawCommit,
    model: CommitModel<'_, '_>,
) -> Result<(), CommitError<Infallible>> {
    let mut pinned: BTreeMap<FactKey, (&FactKey, FactVersion)> = BTreeMap::new();
    for write in &commit.writes {
        let pin = (&write.key, write.expected);
        if pinned.insert(model.canonical(&write.key), pin).is_some() {
            return Err(CommitError::DuplicateKey {
                key: write.key.clone(),
            });
        }
    }
    for read in &commit.reads {
        let canonical = model.canonical(&read.key);
        let current = facts
            .get(&canonical)
            .map_or(FactVersion::ABSENT, |state| state.version);
        let (prior_key, prior) = *pinned
            .entry(canonical)
            .or_insert((&read.key, read.expected));
        if prior != read.expected {
            let stale = if read.expected == current {
                (prior_key, prior)
            } else {
                (&read.key, read.expected)
            };
            return Err(CommitError::Conflict {
                key: stale.0.clone(),
                expected: stale.1,
                current,
            });
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
    for write in &commit.links {
        let declared = model.link(&write.key.source.object_type, &write.key.link);
        if !declared.is_some_and(|link| model.is_a(&write.key.target.object_type, &link.target)) {
            return Err(CommitError::LinkTarget {
                key: Box::new(write.key.clone()),
            });
        }
    }
    Ok(())
}
