//! LAW-LINK-1 / LAW-LINK-MANY-1: a subject's links, their ends and their
//! values as the commit leaves them: one post-commit view of each object's
//! liveness, concrete type, values and adjacency (#1701).

use std::collections::BTreeSet;

use norn_ir::ObjectTypeName;

use super::{CommitModel, CommitView, MAX_LINK_LAW_SUBJECTS, concrete_after};
use crate::cell_host::{CellCommit, CommitError, FactKey, FactMutation, LinkKey, LinkMutation};

/// A link target as a commit leaves it: its key, version and values.
type ResolvedTarget = (FactKey, u64, crate::cell_host::JsonObject);

/// The live links touching `object` once the commit applies.
pub(super) fn links_after_commit<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    object: &FactKey,
) -> Result<BTreeSet<LinkKey>, CommitError<V::Error>> {
    let mut live: BTreeSet<LinkKey> = view
        .links_of(object)
        .map_err(CommitError::Host)?
        .iter()
        .map(|link| model.canonical_link(link))
        .filter(|link| link.source == *object || link.target == *object)
        .collect();
    for write in &commit.links {
        let key = model.canonical_link(&write.key);
        if key.source != *object && key.target != *object {
            continue;
        }
        match write.mutation {
            LinkMutation::Add { .. } => {
                live.insert(key);
            }
            LinkMutation::Remove => {
                live.remove(&key);
            }
        }
    }
    Ok(live)
}

/// The values an object has once the commit applies; `None` when it is
/// deleted, absent, or unprovable by this host.
pub(super) fn values_after_commit<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    object: &FactKey,
) -> Result<Option<crate::cell_host::JsonObject>, CommitError<V::Error>> {
    if let Some(write) = commit
        .writes
        .iter()
        .rev()
        .find(|write| model.canonical(&write.key) == *object)
    {
        return Ok(match &write.mutation {
            FactMutation::Put { values } => Some(values.clone()),
            FactMutation::Delete => None,
        });
    }
    view.object_values(object).map_err(CommitError::Host)
}

/// The canonical object `canonical` as the commit leaves it, keyed by its
/// post-commit concrete type ([`concrete_after`]: a subtype this commit
/// creates is that subtype, not its hierarchy root); the root when the host
/// cannot name it. `None` when the commit deletes it: a deleted object has
/// no law left to hold and is no member of an inverse role (#1701).
pub(super) fn subject_after<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    canonical: &FactKey,
) -> Result<Option<FactKey>, CommitError<V::Error>> {
    let deleted = commit.writes.iter().any(|write| {
        matches!(write.mutation, FactMutation::Delete) && model.canonical(&write.key) == *canonical
    });
    if deleted {
        return Ok(None);
    }
    let concrete = concrete_after(commit, model, view, canonical)?
        .unwrap_or_else(|| canonical.object_type.clone());
    Ok(Some(FactKey::new(
        concrete.as_str(),
        canonical.key.as_str(),
    )))
}

/// LAW-LINK-1: a subject's link targets as the commit leaves them, read
/// through the host's view.
pub(crate) struct CommitLinks<'r, 'a, E, C, T, V: CommitView + ?Sized> {
    commit: &'r CellCommit<E, C, T>,
    model: CommitModel<'r, 'a>,
    view: &'r V,
    subject: FactKey,
    host: Option<V::Error>,
}

impl<'r, 'a, E, C, T, V: CommitView + ?Sized> CommitLinks<'r, 'a, E, C, T, V> {
    /// The links of `subject` once `commit` applies over `view`.
    #[must_use]
    pub(crate) fn new(
        commit: &'r CellCommit<E, C, T>,
        model: CommitModel<'r, 'a>,
        view: &'r V,
        subject: &FactKey,
    ) -> Self {
        Self {
            commit,
            model,
            view,
            subject: model.canonical(subject),
            host: None,
        }
    }

    /// The host failure a read hit, if any; the caller reports it as such.
    pub(crate) fn take_host_error(&mut self) -> Option<V::Error> {
        self.host.take()
    }
}

impl<E, C, T, V: CommitView + ?Sized> crate::governing_laws::LinkTargets
    for CommitLinks<'_, '_, E, C, T, V>
{
    fn target(
        &mut self,
        link: &str,
    ) -> Result<
        Option<crate::governing_laws::LinkedTarget>,
        crate::governing_laws::LawEvaluationError,
    > {
        let unreadable = || {
            crate::governing_laws::LawEvaluationError::Unsupported(format!(
                "LAW-LINK-1: the target of {link} cannot be read"
            ))
        };
        let (commit, model, view) = (self.commit, self.model, self.view);
        let resolved = (|| -> Result<Option<ResolvedTarget>, CommitError<V::Error>> {
            let live = links_after_commit(commit, model, view, &self.subject)?;
            let mut targets = live
                .into_iter()
                .filter(|key| key.source == self.subject && key.link == link)
                .map(|key| key.target);
            let Some(target) = targets.next() else {
                return Ok(None);
            };
            if targets.next().is_some() {
                return Err(CommitError::Law {
                    key: self.subject.clone(),
                    reason: unreadable(),
                });
            }
            let Some(values) = values_after_commit(commit, model, view, &target)? else {
                return Err(CommitError::Law {
                    key: target,
                    reason: unreadable(),
                });
            };
            let Some(member) = subject_after(commit, model, view, &target)? else {
                return Err(CommitError::Law {
                    key: target,
                    reason: unreadable(),
                });
            };
            let version = match commit
                .writes
                .iter()
                .rev()
                .find(|write| model.canonical(&write.key) == target)
            {
                Some(write) => write.expected.0.saturating_add(1),
                None => view.version(&target).map_err(CommitError::Host)?.0,
            };
            Ok(Some((member, version, values)))
        })();
        let (key, version, values) = match resolved {
            Ok(Some(found)) => found,
            Ok(None) => return Ok(None),
            Err(CommitError::Host(error)) => {
                self.host = Some(error);
                return Err(unreadable());
            }
            Err(_) => return Err(unreadable()),
        };
        Ok(Some(crate::governing_laws::LinkedTarget {
            key,
            version,
            values,
        }))
    }

    fn sources(
        &mut self,
        role: &str,
        source: &ObjectTypeName,
        link: &str,
    ) -> Result<Vec<crate::governing_laws::LinkedTarget>, crate::governing_laws::LawEvaluationError>
    {
        let unreadable = || {
            crate::governing_laws::LawEvaluationError::Unsupported(format!(
                "LAW-LINK-MANY-1: the sources of {role} cannot be read"
            ))
        };
        let (commit, model, view) = (self.commit, self.model, self.view);
        let resolved = (|| -> Result<Vec<ResolvedTarget>, CommitError<V::Error>> {
            let mut found = Vec::new();
            for key in links_after_commit(commit, model, view, &self.subject)? {
                if key.target != self.subject || key.link != link {
                    continue;
                }
                // #1701: a line this commit creates as a subtype is a member
                // by that subtype, not by its hierarchy root.
                let Some(member) = subject_after(commit, model, view, &key.source)? else {
                    return Err(CommitError::Law {
                        key: key.source,
                        reason: unreadable(),
                    });
                };
                if !model.is_a(&member.object_type, source) {
                    continue;
                }
                if found.len() >= MAX_LINK_LAW_SUBJECTS {
                    return Err(CommitError::Law {
                        key: self.subject.clone(),
                        reason: crate::governing_laws::LawEvaluationError::LinkFanOutExceeded {
                            target: format!(
                                "{}/{}",
                                self.subject.object_type.as_str(),
                                self.subject.key
                            ),
                            limit: MAX_LINK_LAW_SUBJECTS,
                        },
                    });
                }
                let Some(values) = values_after_commit(commit, model, view, &key.source)? else {
                    return Err(CommitError::Law {
                        key: key.source,
                        reason: unreadable(),
                    });
                };
                let version = match commit
                    .writes
                    .iter()
                    .rev()
                    .find(|write| model.canonical(&write.key) == key.source)
                {
                    Some(write) => write.expected.0.saturating_add(1),
                    None => view.version(&key.source).map_err(CommitError::Host)?.0,
                };
                found.push((member, version, values));
            }
            Ok(found)
        })();
        match resolved {
            Ok(found) => Ok(found
                .into_iter()
                .map(
                    |(key, version, values)| crate::governing_laws::LinkedTarget {
                        key,
                        version,
                        values,
                    },
                )
                .collect()),
            Err(CommitError::Host(error)) => {
                self.host = Some(error);
                Err(unreadable())
            }
            Err(CommitError::Law { reason, .. }) => Err(reason),
            Err(_) => Err(unreadable()),
        }
    }
}
