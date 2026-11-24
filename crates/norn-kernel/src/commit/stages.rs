//! The typed transition from snapshot checks to checks over a sealed commit.
use super::edges::{
    check_acyclic_links, check_cardinality, check_endpoints, check_required_links, live_links_after,
};
use super::law_subjects::check_governing_laws;
use super::post_state::{PostState, VersionPlan};
use super::pre::{
    check_accounting_postings, check_delivery, check_event_limit, check_immutable, check_shape,
    check_versions,
};
use crate::cell_host::{CellCommit, CommitError, CommitVersions, FactKey, LinkKey, LinkMutation};
use crate::commit_check::{InvariantAdmissionError, check_invariants, check_link_authority};
use crate::commit_order::{CommitStage, CommitStages, run_stages};
use crate::{CommitModel, CommitView, ObservationAuthorization};
use std::collections::{BTreeMap, BTreeSet};

struct PreStages<'c, 'a, V: ?Sized> {
    model: CommitModel<'c, 'a>,
    view: &'c V,
    trusted_owned_rows: bool,
}
struct PostStages<'c, 'a, V: ?Sized> {
    post: PostState<'c, 'a, V>,
    links: BTreeMap<FactKey, BTreeSet<LinkKey>>,
}
enum Phase<'c, 'a, V: ?Sized> {
    Pre(PreStages<'c, 'a, V>),
    Post(PostStages<'c, 'a, V>),
}
struct LocalStages<'c, 'a, E, C, T, V: ?Sized> {
    phase: Phase<'c, 'a, V>,
    commit: &'c CellCommit<E, C, T>,
    authorization: Option<&'c ObservationAuthorization>,
}

pub(crate) fn check_commit_local<E, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
    trusted_owned_rows: bool,
) -> Result<CommitVersions, CommitError<V::Error>> {
    let mut stages = LocalStages {
        commit,
        authorization,
        phase: Phase::Pre(PreStages {
            model,
            view,
            trusted_owned_rows,
        }),
    };
    run_stages(&mut stages)?;
    match stages.phase {
        Phase::Post(stages) => Ok(stages.post.into_versions()),
        Phase::Pre(_) => unreachable!("run_stages completed without Versions"),
    }
}

impl<E, C, T, V: CommitView + ?Sized> CommitStages<CommitError<V::Error>>
    for LocalStages<'_, '_, E, C, T, V>
{
    fn run(&mut self, stage: CommitStage) -> Result<(), CommitError<V::Error>> {
        let commit = self.commit;
        let authorization = self.authorization;
        match &mut self.phase {
            Phase::Pre(pre) => {
                let (model, view) = (pre.model, pre.view);
                match stage {
                    CommitStage::EventLimit => check_event_limit(commit, authorization),
                    CommitStage::Shape => check_shape(commit, model, view, pre.trusted_owned_rows),
                    CommitStage::Accounting => check_accounting_postings(commit, model, view),
                    CommitStage::Delivery => check_delivery(commit, view),
                    CommitStage::Immutable => check_immutable(commit, model, view),
                    CommitStage::Versions => {
                        check_versions(commit, model, view, authorization)?;
                        let plan = VersionPlan::checked(&commit.writes, &commit.links)?;
                        self.phase = Phase::Post(PostStages {
                            post: PostState::seal(
                                &commit.writes,
                                &commit.links,
                                model,
                                view,
                                plan,
                            )?,
                            links: BTreeMap::new(),
                        });
                        Ok(())
                    }
                    CommitStage::LinkAuthority
                    | CommitStage::Endpoints
                    | CommitStage::LinkValues
                    | CommitStage::Cardinality
                    | CommitStage::RequiredLinks
                    | CommitStage::Acyclic
                    | CommitStage::Invariants
                    | CommitStage::Laws => unreachable!("post stage before Versions"),
                }
            }
            Phase::Post(stages) => {
                let post = &stages.post;
                match stage {
                    CommitStage::LinkAuthority => check_link_authority(commit, post, authorization),
                    CommitStage::Endpoints => {
                        stages.links = live_links_after(commit, post)?;
                        check_endpoints(commit, post, &stages.links)
                    }
                    CommitStage::LinkValues => {
                        for write in &commit.links {
                            if let LinkMutation::Add { values } = &write.mutation {
                                let concrete =
                                    post.concrete(&write.key.source)?.ok_or_else(|| {
                                        CommitError::DanglingLink {
                                            key: Box::new(write.key.clone()),
                                            missing: write.key.source.clone(),
                                        }
                                    })?;
                                post.model().link_values(&concrete, &write.key, values)?;
                            }
                        }
                        Ok(())
                    }
                    CommitStage::Cardinality => check_cardinality(commit, post, &stages.links),
                    CommitStage::RequiredLinks => check_required_links(commit, post, authorization),
                    CommitStage::Acyclic => check_acyclic_links(commit, post),
                    CommitStage::Invariants => check_invariants(commit, post, authorization)
                        .map_err(InvariantAdmissionError::into_commit_error),
                    CommitStage::Laws => check_governing_laws(commit, post, authorization),
                    CommitStage::EventLimit
                    | CommitStage::Shape
                    | CommitStage::Accounting
                    | CommitStage::Delivery
                    | CommitStage::Immutable
                    | CommitStage::Versions => unreachable!("pre stage after Versions"),
                }
            }
        }
    }
}
