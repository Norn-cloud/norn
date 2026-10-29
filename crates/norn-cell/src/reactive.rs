//! Reactive queries over declared read slices (T10-RT-1, #2693; ruling
//! 2026-09-25 §10).
//!
//! A subscription **declares** its read slice up front: one object type (its
//! subtypes included), equality predicates on stored properties, and a
//! result bound. The API may additionally capture bounded dependencies for
//! admitted `WidgetIR` queries (proposed 2026-10-11 amendment). The [`Registry`] knows every
//! slice, so after a commit it can tell which slices the commit touched:
//!
//! 1. **Bounded by the profile.** A slice whose bound exceeds the resource
//!    limit profile's slice records (`norn.limits/1`: 1 000, ruling
//!    2026-09-27 §1) is refused at subscribe time
//!    ([`SubscribeError::OverProfile`]). A result that would exceed the
//!    profile's canonical bytes is truncated and marked incomplete.
//! 2. **Coalesced.** Identical slices share one [`QueryKey`] and are
//!    evaluated once per commit, however many subscribers they have.
//! 3. **Invalidated by intersection.** A changed object invalidates a slice
//!    when its type is the slice's type (or a subtype) and it was in the
//!    slice's last result (it may have left) or its new values match the
//!    predicates (it may have entered). Unaffected slices are not
//!    re-evaluated.
//! 4. **Fan-out cap.** At most [`ReactiveLimits::max_fanout_per_commit`]
//!    invalidated slices are re-evaluated inside the committing turn; the
//!    rest are reported stale (subscribers re-read when they choose), so one
//!    commit never does unbounded work.
//!
//! Evaluation ([`evaluate`]) reads the committed state through
//! [`StoreReader`]; it is pure with respect to the turn and never writes.
//! Broad scans are bounded by [`ReactiveLimits::max_scan`] examined rows
//! (ids and types only; full records are read one candidate at a time): past
//! it the result is marked incomplete, and an incomplete or stale slice is
//! conservatively invalidated by every related change, rather than
//! recomputed inline (an
//! asynchronous projection with a freshness contract is the follow-up for
//! scans that large).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ResourceLimitProfile, RuntimeValue as Value};
use serde::{Deserialize, Serialize};

use crate::codec::canonical_json;
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::model::Model;
use crate::read::StoreReader;

/// A declared read slice: objects of `object_type` (and its subtypes) whose
/// stored properties equal every value in `filter`, ordered by key, at most
/// `limit` of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SliceQuery {
    /// The object type.
    pub object_type: String,
    /// Equality predicates by property id.
    #[serde(default)]
    pub filter: BTreeMap<String, Value>,
    /// The result bound.
    pub limit: u32,
    /// An admitted `WidgetIR` binding; its target is resolved by the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingQuery>,
}

/// Declarative binding address and typed context query parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingQuery {
    /// Public widget identity.
    pub widget_id: String,
    /// Admitted binding identity within that widget.
    pub binding_id: String,
    /// Declared page context and selected-record keys.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

/// One bounded dependency. Ranges are recorded before reading any candidates,
/// including when the range is empty. Links name both empty and populated sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dependency {
    /// An addressed object, present or missing.
    Object {
        /// Addressed type, including its subtypes.
        object_type: String,
        /// Addressed endpoint identity.
        key: String,
    },
    /// An addressed link set, populated or empty.
    Links {
        /// Addressed type, including its subtypes.
        object_type: String,
        /// Addressed endpoint identity.
        key: String,
        /// One link id, or all link ids used by the read.
        link: Option<String>,
        /// Whether the addressed object is the source.
        outgoing: bool,
        /// Whether the addressed object is the target.
        incoming: bool,
    },
    /// Inverse authorization lookup through a declared source type's link.
    IncomingLinks {
        /// Declaring source type (distinct from the addressed target's type).
        source_type: String,
        /// Addressed target identity.
        target: String,
        /// Declared outgoing link id on the source type.
        link: String,
    },
    /// Predicate membership for an admitted bounded query.
    Range {
        /// Addressed type, including its subtypes.
        object_type: String,
        /// Conjunctive resolved predicates; empty means the whole type range.
        filters: Vec<DependencyFilter>,
    },
}

/// One resolved predicate; operands are immutable values from this evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyFilter {
    /// Stored property identity.
    pub property_id: String,
    /// Admitted comparison operator.
    pub operator: norn_ir::WidgetFilterOperator,
    /// Resolved comparison operand.
    pub value: Value,
}

/// Capture budgets are independent of result bounds and decode work.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DependencySet {
    /// Unique dependencies in capture order.
    pub entries: Vec<Dependency>,
    bytes: usize,
    exhausted: bool,
}

impl DependencySet {
    /// Hard bound on unique dependencies per query.
    pub const MAX_ENTRIES: usize = 4_096;
    /// Hard bound on canonical dependency bytes per query.
    pub const MAX_BYTES: usize = 256 * 1024;

    /// Record one dependency, deduplicating before charging.
    ///
    /// # Errors
    /// Canonical encoding failure or an exhausted capture budget.
    pub fn record(&mut self, dependency: Dependency) -> Result<(), CellError> {
        if self.exhausted {
            return Err(CellError::DependencyBudgetExceeded);
        }
        if self.entries.contains(&dependency) {
            return Ok(());
        }
        let bytes = canonical_json(&dependency)?.len();
        if self.entries.len() >= Self::MAX_ENTRIES
            || bytes > Self::MAX_BYTES.saturating_sub(self.bytes)
        {
            self.exhausted = true;
            return Err(CellError::DependencyBudgetExceeded);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.push(dependency);
        Ok(())
    }
}

impl SliceQuery {
    /// Whether an object with `values` satisfies the predicates.
    #[must_use]
    pub fn matches(&self, values: &BTreeMap<String, Value>) -> bool {
        self.filter
            .iter()
            .all(|(property, expected)| values.get(property) == Some(expected))
    }
}

/// The identity of a declared slice: its canonical JSON. Identical slices
/// share one key and are evaluated once.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueryKey(pub String);

/// One subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(pub u64);

/// The bounds reactive queries run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReactiveLimits {
    /// Largest slice bound (`slice.max_records` of the limit profile).
    pub max_records: u32,
    /// Largest canonical result size (`slice.max_canonical_bytes`).
    pub max_canonical_bytes: u64,
    /// Slices re-evaluated inside one committing turn.
    pub max_fanout_per_commit: usize,
    /// Objects one evaluation scans before it reports an incomplete result.
    pub max_scan: usize,
    /// Candidate bytes decoded across all evaluations and invalidation reads in a commit.
    pub max_candidate_bytes_per_commit: u64,
}

impl ReactiveLimits {
    /// The limits of `profile`, with the default fan-out cap and scan bound.
    #[must_use]
    pub const fn from_profile(profile: &ResourceLimitProfile) -> Self {
        Self {
            max_records: profile.slice.max_records,
            max_canonical_bytes: profile.slice.max_canonical_bytes,
            max_fanout_per_commit: 64,
            max_scan: 10_000,
            max_candidate_bytes_per_commit: profile.slice.max_canonical_bytes,
        }
    }
}

/// Shared candidate decode budget for a subscription publication turn.
/// Candidate byte lengths are charged before owned values are fetched or decoded;
/// nonmatching candidates and invalidation reads consume the same budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeBudget {
    pub(crate) bytes: u64,
    pub(crate) rows: usize,
    pub(crate) dependencies: Option<DependencySet>,
}

impl DecodeBudget {
    /// A turn's aggregate limits (scan rows and candidate bytes).
    #[must_use]
    pub const fn new(limits: &ReactiveLimits) -> Self {
        Self {
            bytes: limits.max_candidate_bytes_per_commit,
            rows: limits.max_scan,
            dependencies: None,
        }
    }

    /// Begin one query's capture while retaining the turn's decode budget.
    pub fn capture(&mut self) {
        self.dependencies = Some(DependencySet::default());
    }

    /// Finish a capture, refusing even if a visibility helper concealed an error.
    ///
    /// # Errors
    /// A capture which exhausted either dependency bound.
    pub fn take_dependencies(&mut self) -> Result<DependencySet, CellError> {
        let dependencies = self.dependencies.take().unwrap_or_default();
        if dependencies.exhausted {
            return Err(CellError::DependencyBudgetExceeded);
        }
        Ok(dependencies)
    }

    /// Record a dependency when query capture is active.
    ///
    /// # Errors
    /// Canonical encoding failure or an exhausted capture budget.
    pub fn depend(&mut self, dependency: Dependency) -> Result<(), CellError> {
        if let Some(dependencies) = &mut self.dependencies {
            dependencies.record(dependency)?;
        }
        Ok(())
    }

    /// Verify capture completeness even when an authorization helper concealed an error.
    ///
    /// # Errors
    /// Dependency capture exhausted its entry or byte bound.
    pub fn check_dependencies(&self) -> Result<(), CellError> {
        if self
            .dependencies
            .as_ref()
            .is_some_and(|dependencies| dependencies.exhausted)
        {
            return Err(CellError::DependencyBudgetExceeded);
        }
        Ok(())
    }

    /// Charge work consumed by a shared binding execution.
    pub fn charge_rows(&mut self, rows: usize) -> bool {
        if rows > self.rows {
            return false;
        }
        self.rows = self.rows.saturating_sub(rows);
        true
    }

    /// Bytes still available for candidate decoding.
    #[must_use]
    pub const fn remaining_bytes(&self) -> u64 {
        self.bytes
    }

    /// Rows still available for candidate reads under this shared budget.
    #[must_use]
    pub const fn remaining_rows(&self) -> usize {
        self.rows
    }

    /// Admit candidate or retained query-state bytes into this shared budget.
    /// Returns false without changing the budget when they do not fit.
    pub fn charge(&mut self, bytes: u64) -> bool {
        if bytes > self.bytes {
            return false;
        }
        self.bytes = self.bytes.saturating_sub(bytes);
        true
    }
}

/// A bounded object read, including a distinct budget-exhausted outcome.
#[derive(Debug)]
pub enum BudgetedRead {
    /// The object exists within the decode budget.
    Found(crate::ObjectRecord),
    /// The object does not exist through this type.
    Missing,
    /// Decoding would exceed the remaining candidate budget.
    Exhausted,
}

/// An identity-only object read under a candidate decode budget.
#[derive(Debug, PartialEq, Eq)]
pub enum BudgetedIdentityRead {
    /// The object's concrete type and key fit the decode budget.
    Found {
        /// The stored concrete object type.
        object_type: String,
        /// The object's key.
        id: String,
    },
    /// The object does not exist through this type.
    Missing,
    /// Reading the identity would exceed the remaining candidate budget.
    Exhausted,
}

/// The outcome of reading link endpoint identities under a candidate budget.
#[derive(Debug, PartialEq, Eq)]
pub enum BudgetedLinksRead {
    /// Every matching endpoint identity fit the budget.
    Complete(Vec<String>),
    /// There are more endpoints than the bounded read can admit, or decoding
    /// them would exceed the remaining candidate budget.
    Exhausted,
}

/// The visibility callback's result when authorizing one subscription item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleItem {
    /// The item passed per-object authorization.
    Visible,
    /// Authorization denied the item or its relation data was indeterminate.
    Hidden,
    /// Authorization could not finish within the shared decode budget.
    Exhausted,
}

/// Why a subscription was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscribeError {
    /// The slice bound exceeds the profile.
    #[error("slice bound {limit} exceeds the profile's {max} records")]
    OverProfile {
        /// The requested bound.
        limit: u32,
        /// The profile's maximum.
        max: u32,
    },
    /// The bound is zero.
    #[error("slice bound must be at least 1")]
    EmptyBound,
    /// The object type is not in the model.
    #[error("unknown object type `{0}`")]
    UnknownType(String),
}

impl SubscribeError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OverProfile { .. } => "subscription.over_profile",
            Self::EmptyBound => "subscription.empty_bound",
            Self::UnknownType(_) => "subscription.unknown_type",
        }
    }
}

/// One object of a slice result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceItem {
    /// The concrete object type.
    pub object_type: String,
    /// The identity key.
    pub key: String,
    /// The owner version.
    pub version: u64,
    /// The stored property values (nulls omitted).
    pub values: BTreeMap<String, Value>,
}

/// A slice's result at one commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceResult {
    /// The matching objects, by key, at most the slice bound.
    pub items: Vec<SliceItem>,
    /// `false` when the scan or the byte bound cut the result short.
    pub complete: bool,
    /// The commit the result reflects (`0` before the first commit).
    pub commit_seq: u64,
}

/// One object a commit wrote, with its state after the commit.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The concrete type (or the written type, for a deleted object).
    pub object_type: String,
    /// The identity key.
    pub key: String,
    /// The stored values after the commit; `None` when deleted.
    pub after: Option<BTreeMap<String, Value>>,
}

/// What one commit asks of the registry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Slices to re-evaluate in this turn.
    pub evaluate: Vec<QueryKey>,
    /// Invalidated slices over the fan-out cap, reported stale.
    pub stale: Vec<QueryKey>,
}

#[derive(Debug, Clone)]
struct Entry {
    query: SliceQuery,
    subscribers: BTreeSet<SubscriptionId>,
    last: Option<SliceResult>,
}

/// The declared slices of one cell and their subscribers.
#[derive(Debug, Clone)]
pub struct Registry {
    limits: ReactiveLimits,
    next_id: u64,
    entries: BTreeMap<QueryKey, Entry>,
    by_subscription: BTreeMap<SubscriptionId, QueryKey>,
}

impl Registry {
    /// An empty registry under `limits`.
    #[must_use]
    pub const fn new(limits: ReactiveLimits) -> Self {
        Self {
            limits,
            next_id: 1,
            entries: BTreeMap::new(),
            by_subscription: BTreeMap::new(),
        }
    }

    /// The limits.
    #[must_use]
    pub const fn limits(&self) -> &ReactiveLimits {
        &self.limits
    }

    /// The key of `query`.
    ///
    /// # Errors
    ///
    /// The query does not canonicalize (it always does: plain JSON).
    pub fn key_of(query: &SliceQuery) -> Result<QueryKey, CellError> {
        Ok(QueryKey(canonical_json(query)?))
    }

    /// Declare a subscription to `query`. Identical slices share one key.
    ///
    /// # Errors
    ///
    /// [`SubscribeError`] for an over-profile or empty bound or an unknown
    /// type; [`CellError`] when the key cannot be computed.
    pub fn subscribe(
        &mut self,
        model: &Model,
        query: SliceQuery,
    ) -> Result<Result<(SubscriptionId, QueryKey), SubscribeError>, CellError> {
        if query.limit == 0 {
            return Ok(Err(SubscribeError::EmptyBound));
        }
        if query.limit > self.limits.max_records {
            return Ok(Err(SubscribeError::OverProfile {
                limit: query.limit,
                max: self.limits.max_records,
            }));
        }
        if !model.has_type(&query.object_type) {
            return Ok(Err(SubscribeError::UnknownType(query.object_type)));
        }
        let key = Self::key_of(&query)?;
        let id = SubscriptionId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.entries
            .entry(key.clone())
            .or_insert_with(|| Entry {
                query,
                subscribers: BTreeSet::new(),
                last: None,
            })
            .subscribers
            .insert(id);
        self.by_subscription.insert(id, key.clone());
        Ok(Ok((id, key)))
    }

    /// Drop a subscription; a slice with no subscribers left is forgotten.
    pub fn unsubscribe(&mut self, id: SubscriptionId) {
        let Some(key) = self.by_subscription.remove(&id) else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.subscribers.remove(&id);
            if entry.subscribers.is_empty() {
                self.entries.remove(&key);
            }
        }
    }

    /// The declared slice of `key`.
    #[must_use]
    pub fn query(&self, key: &QueryKey) -> Option<&SliceQuery> {
        self.entries.get(key).map(|entry| &entry.query)
    }

    /// The last published result of `key`.
    #[must_use]
    pub fn last(&self, key: &QueryKey) -> Option<&SliceResult> {
        self.entries.get(key).and_then(|entry| entry.last.as_ref())
    }

    /// The subscribers of `key`.
    #[must_use]
    pub fn subscribers(&self, key: &QueryKey) -> Vec<SubscriptionId> {
        self.entries
            .get(key)
            .map(|entry| entry.subscribers.iter().copied().collect())
            .unwrap_or_default()
    }

    /// How many distinct slices are declared.
    #[must_use]
    pub fn slice_count(&self) -> usize {
        self.entries.len()
    }

    /// The slices `changes` touch (see the module docs), each once.
    #[must_use]
    pub fn invalidated(&self, model: &Model, changes: &[Change]) -> Vec<QueryKey> {
        self.entries
            .iter()
            .filter(|(_, entry)| {
                let slice_type = entry.query.object_type.as_str();
                changes.iter().any(|change| {
                    // The written type may be an ancestor alias of the
                    // slice's type (a delete through a supertype), so
                    // relatedness runs both ways.
                    let Some(last) = entry.last.as_ref().filter(|last| last.complete) else {
                        // Stale or incomplete (scan- or byte-bounded): any
                        // change in the same root hierarchy may move the
                        // result, siblings included (they share the scan).
                        return same_root(model, &change.object_type, slice_type);
                    };
                    let related = model.is_a(&change.object_type, slice_type)
                        || model.is_a(slice_type, &change.object_type);
                    if !related {
                        return false;
                    }
                    let was_in = last.items.iter().any(|item| item.key == change.key);
                    let is_in = model.is_a(&change.object_type, slice_type)
                        && change
                            .after
                            .as_ref()
                            .is_some_and(|values| entry.query.matches(values));
                    was_in || is_in
                })
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Forget the cached result of `key` (it was invalidated but not
    /// re-evaluated): the next subscriber re-evaluates it, and every related
    /// change invalidates it until then.
    pub fn mark_stale(&mut self, key: &QueryKey) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last = None;
        }
    }

    /// Conservatively invalidate slices sharing an undecoded write's hierarchy.
    #[must_use]
    pub fn invalidated_hierarchy(&self, model: &Model, object_type: &str) -> Vec<QueryKey> {
        self.entries
            .iter()
            .filter(|(_, entry)| same_root(model, object_type, &entry.query.object_type))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Split `invalidated` into the slices re-evaluated in this turn and the
    /// ones over the fan-out cap.
    #[must_use]
    pub fn plan(&self, invalidated: Vec<QueryKey>) -> Plan {
        let cap = self.limits.max_fanout_per_commit;
        let mut evaluate = invalidated;
        let stale = if evaluate.len() > cap {
            evaluate.split_off(cap)
        } else {
            Vec::new()
        };
        Plan { evaluate, stale }
    }

    /// Record the new result of `key`; returns its subscribers to notify.
    pub fn record(&mut self, key: &QueryKey, result: SliceResult) -> Vec<SubscriptionId> {
        let Some(entry) = self.entries.get_mut(key) else {
            return Vec::new();
        };
        entry.last = Some(result);
        entry.subscribers.iter().copied().collect()
    }
}

/// Whether `left` and `right` share a root type (one root table).
fn same_root(model: &Model, left: &str, right: &str) -> bool {
    match (model.chain(left), model.chain(right)) {
        (Ok(left), Ok(right)) => !left.is_empty() && left.first() == right.first(),
        _ => false,
    }
}

/// Evaluate `query` in `world` against the committed state at `commit_seq`.
///
/// # Errors
///
/// SQL engine or decode errors; an unknown type.
pub fn evaluate<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    query: &SliceQuery,
    limits: &ReactiveLimits,
    commit_seq: u64,
) -> Result<SliceResult, CellError> {
    evaluate_with_budget(
        reader,
        world,
        query,
        limits,
        commit_seq,
        &mut DecodeBudget::new(limits),
    )
}

/// Evaluate a slice while charging the committing turn's shared decode budget.
///
/// # Errors
/// SQL engine or decode errors; an unknown type.
pub fn evaluate_with_budget<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    query: &SliceQuery,
    limits: &ReactiveLimits,
    commit_seq: u64,
    budget: &mut DecodeBudget,
) -> Result<SliceResult, CellError> {
    evaluate_visible_with_budget(
        reader,
        world,
        query,
        limits,
        commit_seq,
        budget,
        &mut |_| true,
    )
}

/// [`evaluate_with_budget`] for one reader's view: only items `visible`
/// admits are part of the result, and the bound counts visible items only,
/// so a hidden object never shortens (and so reveals itself in) a result
/// (#2997: per-object read authorization of subscriptions).
///
/// # Errors
/// SQL engine or decode errors; an unknown type.
pub fn evaluate_visible_with_budget<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    query: &SliceQuery,
    limits: &ReactiveLimits,
    commit_seq: u64,
    budget: &mut DecodeBudget,
    visible: &mut dyn FnMut(&SliceItem) -> bool,
) -> Result<SliceResult, CellError> {
    evaluate_visible_with_shared_budget(
        reader,
        world,
        query,
        limits,
        commit_seq,
        budget,
        &mut |item, _budget| {
            if visible(item) {
                VisibleItem::Visible
            } else {
                VisibleItem::Hidden
            }
        },
    )
}

/// [`evaluate_with_budget`] for a reader whose per-item authorization also
/// consumes the publication's shared decode budget. Exhaustion leaves the
/// result incomplete so the subscription is treated as stale.
///
/// # Errors
/// SQL engine or decode errors; an unknown type.
pub fn evaluate_visible_with_shared_budget<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    query: &SliceQuery,
    limits: &ReactiveLimits,
    commit_seq: u64,
    budget: &mut DecodeBudget,
    visible: &mut dyn FnMut(&SliceItem, &mut DecodeBudget) -> VisibleItem,
) -> Result<SliceResult, CellError> {
    budget.depend(Dependency::Range {
        object_type: query.object_type.clone(),
        filters: query
            .filter
            .iter()
            .map(|(property_id, value)| DependencyFilter {
                property_id: property_id.clone(),
                operator: norn_ir::WidgetFilterOperator::Eq,
                value: value.clone(),
            })
            .collect(),
    })?;
    // Ids and types only, at most `max_scan` examined rows of the hierarchy:
    // full records are read one at a time, only for candidates.
    let scan = limits.max_scan.min(budget.rows);
    let mut scan_budget = DecodeBudget {
        bytes: budget.bytes.min(limits.max_canonical_bytes),
        rows: scan,
        dependencies: None,
    };
    let scan_bytes = scan_budget.bytes;
    let (rows, exhausted) =
        reader.root_rows_with_budget(world, &query.object_type, scan, &mut scan_budget)?;
    let charged = scan_bytes.saturating_sub(scan_budget.bytes);
    budget.bytes = budget.bytes.saturating_sub(charged);
    budget.rows = budget
        .rows
        .saturating_sub(scan.saturating_sub(scan_budget.rows));
    let mut slice_budget = DecodeBudget {
        bytes: limits.max_canonical_bytes.saturating_sub(charged),
        rows: scan_budget.rows,
        dependencies: None,
    };
    let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
    let mut items = Vec::new();
    let mut bytes = 0_u64;
    let mut within_bytes = true;
    for (id, concrete) in rows {
        if items.len() >= limit {
            break;
        }
        if !reader.model().is_a(&concrete, &query.object_type) {
            continue;
        }
        let before = budget.bytes;
        let mut candidate_budget = DecodeBudget {
            bytes: budget.bytes.min(slice_budget.bytes),
            rows: 0,
            dependencies: budget.dependencies.take(),
        };
        let available = candidate_budget.bytes;
        let record =
            reader.object_with_budget(world, &query.object_type, &id, &mut candidate_budget)?;
        budget.dependencies = candidate_budget.dependencies.take();
        let charged = available.saturating_sub(candidate_budget.bytes);
        budget.bytes = before.saturating_sub(charged);
        slice_budget.bytes = slice_budget.bytes.saturating_sub(charged);
        let record = match record {
            BudgetedRead::Found(record) => record,
            BudgetedRead::Missing => continue,
            BudgetedRead::Exhausted => {
                within_bytes = false;
                break;
            }
        };
        let values: BTreeMap<String, Value> = record
            .values
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .collect();
        if !query.matches(&values) {
            continue;
        }
        let version = reader.version(world, &query.object_type, &id)?;
        let item = SliceItem {
            object_type: record.object_type,
            key: id,
            version,
            values,
        };
        match visible(&item, budget) {
            VisibleItem::Visible => {}
            VisibleItem::Hidden => continue,
            VisibleItem::Exhausted => {
                within_bytes = false;
                break;
            }
        }
        let size = u64::try_from(canonical_json(&item)?.len()).unwrap_or(u64::MAX);
        bytes = bytes.saturating_add(size);
        if bytes > limits.max_canonical_bytes {
            within_bytes = false;
            break;
        }
        items.push(item);
    }
    let complete = within_bytes && (exhausted || items.len() >= limit);
    Ok(SliceResult {
        items,
        complete,
        commit_seq,
    })
}
