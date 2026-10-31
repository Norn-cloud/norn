//! Pure export-delivery, snapshot and drill-through contracts.
use crate::{Access, BucketAgg, ClassId, ClassSpec, Decimal, MetricError, ObjectKey, Plan, Record, RecordHash, Seq, Unsupported, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Scalar group, absent for an ungrouped aggregate.
pub type GroupKey = Option<String>;
/// Whole-result semantics, with no partial bucket disclosure.
pub type MetricResult = Result<Vec<Bucket>, MetricError>;
/// Sorted measure provenance; public drill-through is empty on any refusal.
pub type Lineage = Vec<Member>;

/// Total refusal order: all capture failures before all execution row failures.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CaptureKey {
    /// 0 = capture; 1 = row validity/currency. Buckets follow both phases.
    pub phase: u8,
    /// Measure identity in native key order.
    pub measure: ObjectKey,
    /// Field order within the row (currency uses `fields.len()`).
    pub field: usize,
}
/// Trusted complete dependency state at one pinned export head (§24.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Snapshot {
    /// Pinned head sequence.
    pub seq: Seq,
    /// Verified content address at that head.
    pub hash: RecordHash,
    /// Installed definition version at that head.
    pub definition_version: u64,
    /// One authoritative store for every dependency object. Measure and target
    /// are views by concrete type, including when the two types are identical.
    pub objects: BTreeMap<ObjectKey, Record>,
    /// Forward edges, retaining malformed cardinalities for capture refusals.
    pub links: BTreeMap<(ObjectKey, String), BTreeSet<ObjectKey>>,
    /// Observed tombstones persist across puts/deletes until explicitly cleared.
    pub tombstones: BTreeSet<ObjectKey>,
}
/// Ordered dependency operations after defaults and lifecycle resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Op {
    /// Whole measure replacement.
    MeasurePut(ObjectKey, Record),
    /// Physical measure deletion and incoming/outgoing link cascade.
    MeasureDelete(ObjectKey),
    /// Whole target replacement.
    TargetPut(ObjectKey, Record),
    /// Physical target deletion and incoming/outgoing link cascade.
    TargetDelete(ObjectKey),
    /// Fact Set or resolved Transition delta; last-write provenance is updated.
    Set {
        /// Object identity.
        key: ObjectKey,
        /// Property identity.
        property: String,
        /// New value.
        value: Value,
    },
    /// Forward edge insertion.
    LinkAdd {
        /// Source identity.
        source: ObjectKey,
        /// Link role.
        role: String,
        /// Target identity.
        target: ObjectKey,
    },
    /// Forward edge removal.
    LinkRemove {
        /// Source identity.
        source: ObjectKey,
        /// Link role.
        role: String,
        /// Target identity.
        target: ObjectKey,
    },
    /// Set or clear an observed tombstone.
    Tombstone {
        /// Object identity.
        key: ObjectKey,
        /// New observed state.
        set: bool,
    },
    /// In-epoch policy/marking update; never removes underlying object state.
    Access {
        /// Object identity.
        key: ObjectKey,
        /// Actor/epoch pair.
        class: ClassId,
        /// Complete new access.
        access: Access,
    },
    /// Authorization changed: stop serving results until rebuilt.
    EpochChange {
        /// Actor identity.
        actor: String,
        /// Current epoch.
        epoch: u64,
    },
    /// Definitions changed: stop serving results until rebuilt.
    DefinitionChange(u64),
}
/// Atomic export commit; duplicate detection precedes all other checks.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Batch {
    /// Commit sequence.
    pub seq: Seq,
    /// Previous verified record hash.
    pub previous: RecordHash,
    /// Verified record hash (cryptographic verification belongs to the adapter).
    pub hash: RecordHash,
    /// Pinned definition version in the receipt.
    pub definition_version: u64,
    /// False for non-main worlds: advance continuity but ignore operations.
    pub main_world: bool,
    /// Ordered writes, resolved facts, then links.
    pub ops: Vec<Op>,
}
/// Disclosed result bucket, ordered by group.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Bucket {
    /// Optional text dimension.
    pub group: GroupKey,
    /// Exact normalized value.
    pub value: Decimal,
    /// Class-visible contributing population (strictly below `u64::MAX`).
    pub population: u64,
    /// Singleton currency for sums, if declared.
    pub currency: Option<String>,
}
/// One contributing measure and its last write provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Member {
    /// Qualified measure identity.
    pub key: ObjectKey,
    /// Last write commit sequence.
    pub seq: Seq,
    /// Last write record content address.
    pub hash: RecordHash,
}
impl Member {
    /// Canonical contributor position: measure identity at capture phase/field zero.
    /// Ordering is decoded Rust `Ord`, never the cursor's serialized bytes.
    #[must_use]
    pub fn capture_key(&self) -> CaptureKey {
        CaptureKey {
            phase: 0,
            measure: self.key.clone(),
            field: 0,
        }
    }
}
/// Exclusive continuation position at one applied export head.
/// Restart with `None` after any head change, including a non-main commit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemberCursor {
    /// Last returned member's capture key; resume strictly after it.
    pub key: CaptureKey,
    /// Applied backend head when this cursor was issued, not last-write provenance.
    pub seq: Seq,
}
/// One public drill-through page, with population independent of its page size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemberPage {
    /// Up to the requested limit (filled unless exhausted), strictly ordered by capture key.
    pub items: Lineage,
    /// Last returned capture key at this head, only when further members remain.
    pub next: Option<MemberCursor>,
    /// Complete visible bucket population, including on an empty final page.
    /// Zero for absent buckets and whenever the whole result refuses.
    pub total_population: u64,
}
/// Paging request refusal; public result refusals instead return an empty page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PageError {
    /// The cursor's head differs from the current applied head (older or future).
    StaleCursor,
    /// A zero page limit cannot make progress.
    InvalidLimit,
}
impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::StaleCursor => "member cursor belongs to a different head",
            Self::InvalidLimit => "member page limit must be positive",
        })
    }
}
impl std::error::Error for PageError {}
/// Atomic delivery outcome; rebuilding outcomes invalidate all future results.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Apply {
    /// Applied completely; output and lineage settled at the new head.
    InOrder,
    /// Sequence ≤ head: exact no-op, even when rebuilding.
    Duplicate,
    /// Sequence discontinuity; rebuild required.
    Gap,
    /// Hash continuity mismatch; rebuild required.
    HashMismatch,
    /// Authorization epoch or definition changed; rebuild required.
    Rebuild,
    /// Unsupported input; rebuild required.
    Unsupported,
}
/// Engine-neutral IVM contract (§24). No method performs IO.
///
/// Hosts must drain to the current source head and atomically check current
/// definition/authorization epochs before exposing data (§24.4). `result` alone
/// is not an emission authorization or freshness guarantee.
pub trait IvmBackend: Sized {
    /// Build from complete, unfiltered dependency state at a pinned head.
    /// # Errors
    /// Structural profile, context-dependent policy or snapshot mismatch.
    fn build(plan: &Plan, classes: &[ClassSpec], snapshot: &Snapshot) -> Result<Self, Unsupported>;
    /// Apply one atomic verified commit (§24.2); retain hidden state, ignore
    /// duplicates and invalidate on gaps/hash/authority changes. Settles lineage
    /// before returning. No result may cross a required rebuild.
    fn apply(&mut self, batch: &Batch) -> Apply;
    /// Canonical class result (§24.3), with whole-result refusals. Unknown or
    /// obsolete classes fail closed. No native-only arithmetic limitations.
    /// # Errors
    /// The first canonical refusal, unknown class or required rebuild.
    fn result(&self, class: ClassId) -> MetricResult;
    /// Page of contributing measures only when `result(class)` is `Ok`. Every `Err`
    /// returns `Ok(MemberPage::default())`, including disclosure, marking, validity, overflow,
    /// unknown class and rebuild refusals. This is a public disclosure boundary;
    /// retained internal L3 state is available only through testing inspection.
    /// At a fixed head, pages concatenate to the complete capture-key-ordered
    /// lineage. `after` is exclusive; `next` is present exactly when more remain.
    /// Public result refusal takes precedence over cursor and limit validation.
    /// Otherwise validate the cursor head before the limit; duplicates preserve
    /// cursors, while every applied commit invalidates them. Implementations can
    /// use ordered range queries; full lineage materialization is not required.
    /// # Errors
    /// `PageError::StaleCursor` for a different head; `PageError::InvalidLimit`
    /// for zero. Unknown groups have population zero and no continuation.
    fn members_page(
        &self,
        class: ClassId,
        group: &GroupKey,
        after: Option<MemberCursor>,
        limit: u32,
    ) -> Result<MemberPage, PageError>;
    /// Applied export head (§24.2). Rejected records never advance it.
    fn head(&self) -> Seq;
}

/// Gate-only inspection of retained incremental state. Backend implementations
/// live behind their own `inspection = []` feature, enabled only by the
/// gate-owned `welldb-conformance` crate. This trait itself is always available;
/// the contract has no `testing` feature and no reference implementation.
/// No backend dependency graph may reach `welldb-oracle` or `welldb-conformance`,
/// under any dependency kind. The graph and default-production type are gated.
pub trait InspectBackend: IvmBackend {
    /// Raw aggregates, including during refusal; omit zero-population buckets.
    /// Rebuilding and unknown classes return an empty map.
    fn debug_buckets(&self, class: ClassId) -> BTreeMap<GroupKey, BucketAgg>;
    /// Retained valid contributors even while public results refuse. This is
    /// test-only internal L3 evidence and must never become public drill-through.
    fn raw_members(&self, class: ClassId, group: &GroupKey) -> Lineage;
}

