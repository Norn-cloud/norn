//! Canonical identity indexes shared by every reader of the proposed state.
use super::overlay_core::{EdgeDelta, RowChange, RowSource, edge_delta, resolve};
use crate::cell_host::{
    CellCommit, CommitError, CommitVersions, FactKey, FactMutation, FactVersion, FactWrite,
    JsonObject, LinkKey, LinkMutation, LinkWrite, WrittenLinkVersion, WrittenVersion,
};
use crate::{CommitModel, CommitView};
use norn_ir::ObjectTypeName;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

#[derive(PartialOrd, Ord, PartialEq, Eq)]
struct Canon(FactKey);

impl Canon {
    fn of(model: CommitModel<'_, '_>, key: &FactKey) -> Self {
        Self(model.canonical(key))
    }
}

pub(super) enum PostRow<'p> {
    Live {
        concrete: ObjectTypeName,
        version: FactVersion,
        values: Cow<'p, JsonObject>,
    },
    Deleted {
        tombstone: FactVersion,
    },
    Absent,
}

pub(super) struct VersionPlan(CommitVersions);

impl VersionPlan {
    pub(super) fn checked<X>(
        writes: &[FactWrite],
        links: &[LinkWrite],
    ) -> Result<Self, CommitError<X>> {
        let written = writes
            .iter()
            .map(|w| {
                Ok(WrittenVersion {
                    key: w.key.clone(),
                    version: w.expected.checked_next().ok_or(CommitError::Overflow)?,
                })
            })
            .collect::<Result<_, CommitError<X>>>()?;
        let linked = links
            .iter()
            .map(|w| {
                Ok(WrittenLinkVersion {
                    key: w.key.clone(),
                    version: w.expected.checked_next().ok_or(CommitError::Overflow)?,
                })
            })
            .collect::<Result<_, CommitError<X>>>()?;
        Ok(Self(CommitVersions { written, linked }))
    }
}

struct PutRow<'c> {
    concrete: &'c ObjectTypeName,
    values: JsonObject,
}

/// The admitted projection and the exact ordered successors used to construct it.
/// Observation preparation can build a Projection, but cannot obtain a PostState.
pub(crate) struct PostState<'c, 'a, V: ?Sized> {
    projection: Projection<'c, 'a, V>,
    plan: VersionPlan,
}
impl<'c, 'a, V: CommitView + ?Sized> PostState<'c, 'a, V> {
    pub(super) fn seal<E, C, T>(
        commit: &'c CellCommit<E, C, T>,
        model: CommitModel<'c, 'a>,
        view: &'c V,
        plan: VersionPlan,
    ) -> Result<Self, CommitError<V::Error>> {
        let projection = Projection::project(&commit.writes, &commit.links, model, view, &plan)?;
        Ok(Self { projection, plan })
    }
    pub(super) fn into_versions(self) -> CommitVersions {
        self.plan.0
    }
}
impl<'c, 'a, V: ?Sized> std::ops::Deref for PostState<'c, 'a, V> {
    type Target = Projection<'c, 'a, V>;
    fn deref(&self) -> &Self::Target {
        &self.projection
    }
}
pub(crate) struct Projection<'c, 'a, V: ?Sized> {
    model: CommitModel<'c, 'a>,
    view: &'c V,
    rows: BTreeMap<Canon, RowChange>,
    puts: Vec<PutRow<'c>>,
    edges: BTreeMap<FactKey, Vec<(LinkKey, bool)>>,
}

impl<'c, 'a, V: CommitView + ?Sized> Projection<'c, 'a, V> {
    fn project(
        writes: &'c [FactWrite],
        links: &'c [LinkWrite],
        model: CommitModel<'c, 'a>,
        view: &'c V,
        plan: &VersionPlan,
    ) -> Result<Self, CommitError<V::Error>> {
        let mut rows = BTreeMap::new();
        let mut puts = Vec::new();
        for (write, successor) in writes.iter().zip(&plan.0.written) {
            let version = successor.version.0;
            let change = match &write.mutation {
                FactMutation::Put { values } => {
                    let slot = puts.len();
                    let values = model
                        .catalog
                        .effective_object_values(&write.key.object_type, values)
                        .map_err(|error| CommitError::EffectiveValues {
                            key: write.key.clone(),
                            reason: error.to_string(),
                        })?;
                    puts.push(PutRow {
                        concrete: &write.key.object_type,
                        values,
                    });
                    RowChange::Put { slot, version }
                }
                FactMutation::Delete => RowChange::Delete { version },
            };
            rows.insert(Canon::of(model, &write.key), change);
        }
        let mut edges = BTreeMap::<_, Vec<_>>::new();
        for write in links {
            let key = model.canonical_link(&write.key);
            let add = matches!(write.mutation, LinkMutation::Add { .. });
            if key.source != key.target {
                edges
                    .entry(key.target.clone())
                    .or_default()
                    .push((key.clone(), add));
            }
            edges
                .entry(key.source.clone())
                .or_default()
                .push((key, add));
        }
        Ok(Self {
            model,
            view,
            rows,
            puts,
            edges,
        })
    }

    // Preparation needs the same projection before admission runs its CAS checks.
    pub(crate) fn preview(
        writes: &'c [FactWrite],
        links: &'c [LinkWrite],
        model: CommitModel<'c, 'a>,
        view: &'c V,
    ) -> Result<Self, CommitError<V::Error>> {
        Self::project(
            writes,
            links,
            model,
            view,
            &VersionPlan::checked(writes, links)?,
        )
    }
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            model: self.model,
            view: self.view,
            rows: BTreeMap::new(),
            puts: Vec::new(),
            edges: BTreeMap::new(),
        }
    }
    pub(crate) fn before(&self) -> &'c V {
        self.view
    }
    pub(crate) fn model(&self) -> CommitModel<'c, 'a> {
        self.model
    }
    fn source(&self, key: &Canon) -> Result<RowSource<'_, PutRow<'c>>, CommitError<V::Error>> {
        resolve(&self.puts, self.rows.get(key).copied()).ok_or(CommitError::Overflow)
    }

    pub(crate) fn concrete(
        &self,
        key: &FactKey,
    ) -> Result<Option<ObjectTypeName>, CommitError<V::Error>> {
        let key = Canon::of(self.model, key);
        match self.source(&key)? {
            RowSource::Put { row, .. } => Ok(Some(row.concrete.clone())),
            RowSource::Deleted { .. } => Ok(None),
            RowSource::Before => self.view.concrete(&key.0).map_err(CommitError::Host),
        }
    }

    pub(super) fn row(&self, key: &FactKey) -> Result<PostRow<'_>, CommitError<V::Error>> {
        Ok(self.row_matching(key, |_| true)?.unwrap_or(PostRow::Absent))
    }
    /// Filter on concrete lineage before asking the host for any values.
    pub(super) fn row_matching(
        &self,
        key: &FactKey,
        accept: impl FnOnce(&ObjectTypeName) -> bool,
    ) -> Result<Option<PostRow<'_>>, CommitError<V::Error>> {
        let key = Canon::of(self.model, key);
        match self.source(&key)? {
            RowSource::Put { row, version } => Ok(accept(row.concrete).then(|| PostRow::Live {
                concrete: row.concrete.clone(),
                version: FactVersion(version),
                values: Cow::Borrowed(&row.values),
            })),
            RowSource::Deleted { tombstone } => Ok(Some(PostRow::Deleted {
                tombstone: FactVersion(tombstone),
            })),
            RowSource::Before => {
                let version = self.view.version(&key.0).map_err(CommitError::Host)?;
                let Some(concrete) = self.view.concrete(&key.0).map_err(CommitError::Host)? else {
                    return Ok(Some(if version == FactVersion::ABSENT {
                        PostRow::Absent
                    } else {
                        PostRow::Deleted { tombstone: version }
                    }));
                };
                if !accept(&concrete) {
                    return Ok(None);
                }
                let values = self
                    .view
                    .object_values(&key.0)
                    .map_err(CommitError::Host)?
                    .ok_or_else(|| CommitError::Law {
                        key: key.0,
                        reason: crate::governing_laws::LawEvaluationError::Unsupported(
                            "LAW-LINK-1: a live row's values cannot be read".into(),
                        ),
                    })?;
                Ok(Some(PostRow::Live {
                    concrete,
                    version,
                    values: Cow::Owned(values),
                }))
            }
        }
    }
    pub(super) fn changes(&self, canonical: &FactKey) -> &[(LinkKey, bool)] {
        self.edges.get(canonical).map_or(&[], Vec::as_slice)
    }

    pub(super) fn incident(
        &self,
        object: &FactKey,
    ) -> Result<BTreeSet<LinkKey>, CommitError<V::Error>> {
        let object = Canon::of(self.model, object).0;
        let mut live: BTreeSet<_> = self
            .view
            .links_of(&object)
            .map_err(CommitError::Host)?
            .iter()
            .map(|k| self.model.canonical_link(k))
            .filter(|k| k.source == object || k.target == object)
            .collect();
        if let Some(changes) = self.edges.get(&object) {
            for (key, add) in changes {
                match edge_delta(live.contains(key), *add) {
                    EdgeDelta::Keep => {}
                    EdgeDelta::Insert => {
                        live.insert(key.clone());
                    }
                    EdgeDelta::Remove => {
                        live.remove(key);
                    }
                }
            }
        }
        Ok(live)
    }
}
