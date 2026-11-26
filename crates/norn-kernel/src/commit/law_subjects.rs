use super::post_state::{PostRow, Projection};
use super::{CommitLinks, MAX_LINK_LAW_SUBJECTS, PostState};
use crate::cell_host::{CellCommit, CommitError, FactKey, FactMutation, LinkKey};
use crate::commit_check::CommitView;
use crate::snapshot_observe::ObservationAuthorization;
use norn_expr::TurnFuel;
use norn_ir::ObjectTypeName;
use std::collections::BTreeSet;
/// LAW-LINK-MANY-1: the subjects whose governing law reads, through an
/// inverse role, the object `source` (of concrete type `concrete`): the
/// targets its forward link reaches before and after the commit, so a source
/// that moves re-checks both the subject it leaves and the one it joins.
fn inverse_subjects<V: CommitView + ?Sized>(
    post: &Projection<'_, '_, V>,
    index: &crate::governing_laws::LinkLawIndex,
    written: &FactKey,
    source: &FactKey,
    concrete: &ObjectTypeName,
    skip: &BTreeSet<FactKey>,
    out: &mut BTreeSet<FactKey>,
) -> Result<usize, CommitError<V::Error>> {
    let model = post.model();
    let readers: Vec<_> = index
        .inverse
        .iter()
        .filter(|(_, declared, _)| model.is_a(concrete, declared))
        .collect();
    if readers.is_empty() {
        return Ok(0);
    }
    let before: BTreeSet<LinkKey> = post
        .before()
        .links_of(source)
        .map_err(CommitError::Host)?
        .iter()
        .map(|link| model.canonical_link(link))
        .collect();
    let after = post.incident(source)?;
    let mut count = 0_usize;
    for link in before.union(&after) {
        if link.source != *source || skip.contains(&link.target) {
            continue;
        }
        let Some(ty) = post.concrete(&link.target)? else {
            continue;
        };
        let subject = FactKey::new(ty.as_str(), &link.target.key);
        if readers
            .iter()
            .any(|(reader, _, id)| *id == link.link && model.is_a(&subject.object_type, reader))
        {
            count = count.saturating_add(1);
            if count > MAX_LINK_LAW_SUBJECTS {
                return Err(CommitError::Law {
                    key: written.clone(),
                    reason: crate::governing_laws::LawEvaluationError::LinkFanOutExceeded {
                        target: format!("{}/{}", concrete.as_str(), source.key),
                        limit: MAX_LINK_LAW_SUBJECTS,
                    },
                });
            }
            out.insert(subject);
        }
    }
    Ok(count)
}

/// LAW-LINK-1: the live subjects whose governing law reads a link to the
/// object `written`, once `commit` applies, other than those in `skip`. More
/// than [`MAX_LINK_LAW_SUBJECTS`] on one target refuses the commit
/// (`core.compute.bound_exceeded`); it is never checked partially.
pub(crate) fn linked_subjects<V: CommitView + ?Sized>(
    post: &Projection<'_, '_, V>,
    index: &crate::governing_laws::LinkLawIndex,
    written: &FactKey,
    skip: &BTreeSet<FactKey>,
    out: &mut BTreeSet<FactKey>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    let target = model.canonical(written);
    let Some(concrete) = post.concrete(&target)? else {
        return Ok(());
    };
    let mut count = inverse_subjects(post, index, written, &target, &concrete, skip, out)?;
    let readers: Vec<_> = index
        .forward
        .iter()
        .filter(|(_, _, declared)| {
            crate::governing_laws::reaches(model.catalog, &concrete, declared)
        })
        .collect();
    if readers.is_empty() {
        return Ok(());
    }
    for link in post.incident(&target)? {
        if link.target != target || skip.contains(&link.source) {
            continue;
        }
        let Some(ty) = post.concrete(&link.source)? else {
            continue;
        };
        let source = FactKey::new(ty.as_str(), &link.source.key);
        if readers
            .iter()
            .any(|(subject, id, _)| *id == link.link && model.is_a(&source.object_type, subject))
        {
            count = count.saturating_add(1);
            if count > MAX_LINK_LAW_SUBJECTS {
                return Err(CommitError::Law {
                    key: written.clone(),
                    reason: crate::governing_laws::LawEvaluationError::LinkFanOutExceeded {
                        target: format!("{}/{}", concrete.as_str(), target.key),
                        limit: MAX_LINK_LAW_SUBJECTS,
                    },
                });
            }
            out.insert(source);
        }
    }
    Ok(())
}

pub(super) fn check_governing_laws<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    authorization: Option<&ObservationAuthorization>,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    let mut remaining = TurnFuel::default();
    let violated = |key: &FactKey, failure: crate::governing_laws::LawFailure| CommitError::Law {
        key: key.clone(),
        reason: crate::governing_laws::LawEvaluationError::Violated(Box::new(failure)),
    };
    let mut subjects = LawSubjects::default();
    let authorized = authorization.map(ObservationAuthorization::indexed);
    for write in &commit.writes {
        let FactMutation::Put { .. } = &write.mutation else {
            continue;
        };
        subjects.direct.insert(model.canonical(&write.key));
        let PostRow::Live { values, .. } = post.row(&write.key)? else {
            unreachable!("sealed Put is live")
        };
        let failures = evaluate(post, &write.key, &values, &mut remaining)?;
        if authorized
            .as_ref()
            .is_some_and(|index| index.authorizes(write))
        {
            continue;
        }
        if let Some(failure) = failures.into_iter().find(|failure| failure.refuses_native) {
            return Err(violated(&write.key, failure));
        }
    }
    // LAW-LINK-1: only a native commit refuses for its subjects; an observed
    // commit's linked subjects are re-checked by the observation path.
    if authorization.is_some() {
        return Ok(());
    }
    let index = crate::governing_laws::link_law_index(model.catalog).map_err(|reason| {
        CommitError::Law {
            key: commit.writes.first().map_or_else(
                || FactKey::new("norn.Commit", ""),
                |write| write.key.clone(),
            ),
            reason,
        }
    })?;
    if index.is_empty() {
        return Ok(());
    }
    law_subjects(commit, post, &index, &mut subjects)?;
    let unreadable = |subject: &FactKey| CommitError::Law {
        key: subject.clone(),
        reason: crate::governing_laws::LawEvaluationError::Unsupported(
            "LAW-LINK-1: a linked subject's values cannot be read".into(),
        ),
    };
    for subject in &subjects.relinked {
        let PostRow::Live { values, .. } = post.row(subject)? else {
            return Err(unreadable(subject));
        };
        if let Some(failure) = evaluate(post, subject, &values, &mut remaining)?
            .into_iter()
            .find(|failure| failure.refuses_native)
        {
            return Err(violated(subject, failure));
        }
    }
    // A target write refuses when it breaks a law a linked subject satisfied;
    // a subject already in violation (a quarantined observation) does not
    // freeze its target.
    let before = post.snapshot();
    for subject in subjects.dependent.difference(&subjects.relinked) {
        let PostRow::Live { values, .. } = post.row(subject)? else {
            return Err(unreadable(subject));
        };
        let after = evaluate(post, subject, &values, &mut remaining)?;
        if !after.iter().any(|failure| failure.refuses_native) {
            continue;
        }
        let held = evaluate(&before, subject, &values, &mut remaining)?;
        if let Some(failure) = after.into_iter().find(|failure| {
            failure.refuses_native && !held.iter().any(|prior| prior.rule_ref == failure.rule_ref)
        }) {
            return Err(violated(subject, failure));
        }
    }
    Ok(())
}

#[derive(Default)]
struct LawSubjects {
    direct: BTreeSet<FactKey>,
    relinked: BTreeSet<FactKey>,
    dependent: BTreeSet<FactKey>,
}

fn law_subjects<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    post: &PostState<'_, '_, V>,
    index: &crate::governing_laws::LinkLawIndex,
    subjects: &mut LawSubjects,
) -> Result<(), CommitError<V::Error>> {
    let model = post.model();
    // A subject whose link changed is a changed subject.
    for write in &commit.links {
        let key = model.canonical_link(&write.key);
        if subjects.direct.contains(&key.source) {
            continue;
        }
        let Some(ty) = post.concrete(&key.source)? else {
            continue;
        };
        let source = FactKey::new(ty.as_str(), &key.source.key);
        if index
            .forward
            .iter()
            .any(|(subject, link, _)| *link == key.link && model.is_a(&source.object_type, subject))
        {
            subjects.relinked.insert(source);
        }
    }
    // LAW-LINK-MANY-1: a source whose link is added or removed (a line moved,
    // added to or deleted from its document) re-checks the subject it links.
    for write in &commit.links {
        let key = model.canonical_link(&write.key);
        if subjects.direct.contains(&key.target) {
            continue;
        }
        let source = if let Some(ty) = post.concrete(&key.source)? {
            ty
        } else if let Some(old) = post
            .before()
            .concrete(&key.source)
            .map_err(CommitError::Host)?
        {
            old
        } else {
            continue;
        };
        let Some(ty) = post.concrete(&key.target)? else {
            continue;
        };
        let subject = FactKey::new(ty.as_str(), &key.target.key);
        if index.inverse.iter().any(|(reader, declared, link)| {
            *link == key.link
                && model.is_a(&source, declared)
                && model.is_a(&subject.object_type, reader)
        }) {
            subjects.dependent.insert(subject);
        }
    }
    for write in &commit.writes {
        if matches!(write.mutation, FactMutation::Put { .. }) {
            linked_subjects(
                post,
                index,
                &write.key,
                &subjects.direct,
                &mut subjects.dependent,
            )?;
        }
    }
    Ok(())
}

fn evaluate<V: CommitView + ?Sized>(
    on: &Projection<'_, '_, V>,
    key: &FactKey,
    values: &crate::cell_host::JsonObject,
    remaining: &mut TurnFuel,
) -> Result<Vec<crate::governing_laws::LawFailure>, CommitError<V::Error>> {
    let mut links = CommitLinks::new(on, key);
    let evaluated = crate::governing_laws::evaluate_linked(
        on.model().catalog,
        key,
        values,
        &mut links,
        remaining,
    );
    if let Some(error) = links.take_host_error() {
        return Err(CommitError::Host(error));
    }
    evaluated.map_err(|reason| CommitError::Law {
        key: key.clone(),
        reason,
    })
}
