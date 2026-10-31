//! Pure source-state updates; delivery continuity remains the backend's responsibility.
use crate::{
    Batch, ClassSpec, JoinedTarget, MetricError, ObjectKey, Op, Plan, Record, RowRef, Seq,
    Snapshot, Unsupported,
};
use std::collections::{BTreeMap, BTreeSet};

/// Source decoder/rebuild state, never prescribed as an incremental engine store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(from = "SourceWire", into = "SourceWire"))]
pub struct SourceState {
    snapshot: Snapshot,
    incoming: BTreeMap<ObjectKey, BTreeSet<(ObjectKey, String)>>,
    // Compatibility with imported snapshots containing empty adjacency buckets:
    // the old delete cleared all of them. Export ops never create these buckets.
    empty_links: BTreeSet<(ObjectKey, String)>,
}
#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct SourceWire {
    snapshot: Snapshot,
}
#[cfg(feature = "serde")]
impl From<SourceWire> for SourceState {
    fn from(wire: SourceWire) -> Self {
        Self::from_snapshot(wire.snapshot)
    }
}
#[cfg(feature = "serde")]
impl From<SourceState> for SourceWire {
    fn from(source: SourceState) -> Self {
        Self {
            snapshot: source.into_snapshot(),
        }
    }
}
impl SourceState {
    /// Take ownership of a trusted snapshot and index its incoming edges.
    /// The existing `(source, role)` ordered map indexes outgoing edges.
    /// Construction is O(E log E); use `build` when admission is also needed.
    #[must_use]
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let mut incoming: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        let mut empty_links = BTreeSet::new();
        for (edge, targets) in &snapshot.links {
            if targets.is_empty() {
                empty_links.insert(edge.clone());
            }
            for target in targets {
                incoming
                    .entry(target.clone())
                    .or_default()
                    .insert(edge.clone());
            }
        }
        Self {
            snapshot,
            incoming,
            empty_links,
        }
    }
    /// Read complete source state, without class visibility filtering.
    /// Link mutation goes through `apply` so both endpoint indexes stay current.
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// Move out the snapshot, dropping its derived indexes. No snapshot clone.
    #[must_use]
    pub fn into_snapshot(self) -> Snapshot {
        self.snapshot
    }
    /// Set provenance before applying a commit's ops. Continuity remains the
    /// caller's responsibility, just as for `apply_batch`.
    pub fn set_head(&mut self, seq: Seq, hash: &str) {
        self.snapshot.seq = seq;
        hash.clone_into(&mut self.snapshot.hash);
    }
    /// Bind a pinned rebuild snapshot to its definition version. This does not
    /// admit authority changes through `apply`; the backend still rebuilds.
    pub fn set_definition_version(&mut self, version: u64) {
        self.snapshot.definition_version = version;
    }

    /// Validate snapshot/plan binding and class support.
    /// # Errors
    /// Invalid plan, unsupported context, duplicate classes or definition mismatch.
    pub fn build(
        plan: &Plan,
        classes: &[ClassSpec],
        snapshot: &Snapshot,
    ) -> Result<Self, Unsupported> {
        plan.validate()?;
        let unique: BTreeSet<_> = classes.iter().map(|c| &c.id).collect();
        if unique.len() != classes.len()
            || classes
                .iter()
                .any(|c| c.requires_acted_by_context || c.requires_link_context)
            || plan.definition_version != snapshot.definition_version
            || snapshot.objects.keys().any(|key| {
                key.object_type != plan.measure_type
                    && plan
                        .link
                        .as_ref()
                        .is_none_or(|link| key.object_type != link.target_type)
            })
        {
            return Err(Unsupported("snapshot/class binding or context".into()));
        }
        Ok(Self::from_snapshot(snapshot.clone()))
    }
    /// Apply one decoded source op. Defaults and lifecycle Transition resolution
    /// belong to the export adapter; Set is their shared resolved representation.
    /// Set/Put provenance uses the head set by `set_head` or `apply_batch`.
    /// Tombstones survive puts/deletes, and physical deletion cascades edges.
    /// # Errors
    /// Unknown objects for facts/access updates, or authority changes requiring rebuild.
    pub fn apply(&mut self, op: &Op) -> Result<(), MetricError> {
        match op {
            Op::MeasurePut(key, row) | Op::TargetPut(key, row) => {
                let mut row = row.clone();
                self.stamp(&mut row);
                self.snapshot.objects.insert(key.clone(), row);
            }
            Op::MeasureDelete(key) | Op::TargetDelete(key) => {
                self.snapshot.objects.remove(key);
                self.delete_links(key);
            }
            Op::Set {
                key,
                property,
                value,
            } => {
                let seq = self.snapshot.seq;
                let hash = self.snapshot.hash.clone();
                let row = self.record_mut(key).ok_or(MetricError::InvalidField)?;
                row.values.insert(property.clone(), value.clone());
                row.seq = seq;
                row.hash = hash;
            }
            Op::LinkAdd {
                source,
                role,
                target,
            } => {
                let edge = (source.clone(), role.clone());
                self.snapshot
                    .links
                    .entry(edge.clone())
                    .or_default()
                    .insert(target.clone());
                self.incoming
                    .entry(target.clone())
                    .or_default()
                    .insert(edge.clone());
                self.empty_links.remove(&edge);
            }
            Op::LinkRemove {
                source,
                role,
                target,
            } => {
                let edge = (source.clone(), role.clone());
                if let Some(targets) = self.snapshot.links.get_mut(&edge) {
                    targets.remove(target);
                    if targets.is_empty() {
                        self.snapshot.links.remove(&edge);
                    }
                }
                self.forget_incoming(target, &edge);
                self.empty_links.remove(&edge);
            }
            Op::Tombstone { key, set } => {
                if *set {
                    self.snapshot.tombstones.insert(key.clone());
                } else {
                    self.snapshot.tombstones.remove(key);
                }
            }
            Op::Access { key, class, access } => {
                self.record_mut(key)
                    .ok_or(MetricError::InvalidField)?
                    .access
                    .insert(class.clone(), access.clone());
            }
            Op::EpochChange { .. } | Op::DefinitionChange(_) => {
                return Err(MetricError::Rebuilding);
            }
        }
        Ok(())
    }
    fn forget_incoming(&mut self, target: &ObjectKey, edge: &(ObjectKey, String)) {
        if let Some(edges) = self.incoming.get_mut(target) {
            edges.remove(edge);
            if edges.is_empty() {
                self.incoming.remove(target);
            }
        }
    }
    // Visit only this source's contiguous role range and this target's incoming
    // index: O(degree * log E), not O(E). Imported empty buckets are cleared via
    // their own index once, preserving the previous decoder's exact snapshots.
    fn delete_links(&mut self, key: &ObjectKey) {
        let outgoing: Vec<_> = self
            .snapshot
            .links
            .range((key.clone(), String::new())..)
            .take_while(|((source, _), _)| source == key)
            .map(|(edge, _)| edge.clone())
            .collect();
        for edge in outgoing {
            if let Some(targets) = self.snapshot.links.remove(&edge) {
                for target in targets {
                    self.forget_incoming(&target, &edge);
                }
            }
        }
        if let Some(incoming) = self.incoming.remove(key) {
            for edge in incoming {
                if let Some(targets) = self.snapshot.links.get_mut(&edge) {
                    targets.remove(key);
                    if targets.is_empty() {
                        self.snapshot.links.remove(&edge);
                    }
                }
            }
        }
        for empty in std::mem::take(&mut self.empty_links) {
            self.snapshot.links.remove(&empty);
        }
    }
    fn stamp(&self, row: &mut Record) {
        row.seq = self.snapshot.seq;
        row.hash.clone_from(&self.snapshot.hash);
    }
    fn record_mut(&mut self, key: &ObjectKey) -> Option<&mut Record> {
        self.snapshot.objects.get_mut(key)
    }
    /// Apply the ops of a checked in-order batch to a copy, publishing atomically.
    /// This transactional convenience clones the entire source state: O(N) time
    /// and space per batch. Benchmark generators and incremental backends must
    /// evolve their own stores in place using `apply`.
    /// Continuity/authority checks remain the backend's responsibility.
    /// # Errors
    /// Invalid operation; state is unchanged on failure.
    pub fn apply_batch(&mut self, batch: &Batch) -> Result<(), MetricError> {
        let mut next = self.clone();
        next.snapshot.seq = batch.seq;
        next.snapshot.hash.clone_from(&batch.hash);
        if batch.main_world {
            for op in &batch.ops {
                next.apply(op)?;
            }
        }
        *self = next;
        Ok(())
    }
    /// Measure-role view of the authoritative store, in capture-key order.
    pub fn measures<'a>(
        &'a self,
        plan: &'a Plan,
    ) -> impl Iterator<Item = (&'a ObjectKey, &'a Record)> {
        self.snapshot
            .objects
            .iter()
            .filter(|(key, _)| key.object_type == plan.measure_type)
    }
    /// Join a measure to its target without filtering hidden source state.
    #[must_use]
    pub fn target<'a>(&'a self, plan: &Plan, key: &ObjectKey) -> JoinedTarget<'a> {
        let Some(link) = &plan.link else {
            return JoinedTarget::NotRequired;
        };
        let Some(targets) = self.snapshot.links.get(&(key.clone(), link.role.clone())) else {
            return JoinedTarget::Missing;
        };
        if targets.len() != 1 {
            return JoinedTarget::Missing;
        }
        let Some(target) = targets.first() else {
            return JoinedTarget::Missing;
        };
        let Some(record) = self.snapshot.objects.get(target) else {
            return JoinedTarget::Missing;
        };
        if target.object_type != link.target_type {
            return JoinedTarget::Missing;
        }
        JoinedTarget::Row(RowRef {
            key: target,
            record,
            tombstoned: self.snapshot.tombstones.contains(target),
        })
    }
}
