//! Domain-model coverage instrumentation over [`SqliteCell`] (#1656).
//!
//! [`ObservedSqliteCell`] wraps a [`SqliteCell`] without adding fields or runtime
//! overhead to production cells. Whenever a test drives commits through it, the
//! attached [`DomainCoverageRecorder`] tallies:
//! - exact committed lifecycle transitions from ledger events (`FactEvent::Transitioned`
//!   and `ObservationEvent::ObservedTransition`, preserving self-loops and
//!   multi-transition commits and never mis-crediting creations),
//! - typed immutable refusals (`CommitError::ImmutableObject` and
//!   `DecisionRefusal::ImmutableStateWrite`),
//! - evaluated governing-law holds (evaluated independently per law artifact
//!   against the committed object and live `CurrentLinks`
//!   via [`norn_kernel::governing_laws::evaluate_linked`], only when the law's
//!   activation condition holds and no observed finding violated the law) and
//!   exact `DataQualityOutcome::NotSatisfied` law/invariant violations (both
//!   native refusals and observed `DataQualityFinding` ledger events).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use norn_ir::{
    ArtifactKind, ArtifactPayload, DataQualityFinding, DataQualityOutcome, DataQualityRuleRef,
    JsonValue, LawIR, LawModality, LawResponse, ObjectTypeName, RuntimeValue,
};
use norn_kernel::cell_host::{FactKey, FactMutation, FactState};
use norn_kernel::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DecisionRefusal, DueTimer,
    FactEvent, LinkKey, LinkReadTarget, LinkState, ObservationAuthorization, ObservationEvent,
    Observed, OwnedRowsCommitProof, Performed, PredicateObject, PredicateQuery,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::{DefaultEngine, SqlEngine};
use crate::error::CellError;
use crate::host::SqliteCell;
use crate::store::MAIN_WORLD;

/// One observed lifecycle transition `(object_type, lifecycle, transition, from, to)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TransitionKey {
    /// Concrete object type name.
    pub object_type: String,
    /// Lifecycle property name.
    pub lifecycle: String,
    /// Declared transition id.
    pub transition: String,
    /// State left by the transition.
    pub from: String,
    /// State entered by the transition.
    pub to: String,
}

/// One observed immutable refusal `(object_type, lifecycle, state)`.
///
/// For entity-level `immutable: true` refusals, `lifecycle` is `"<object>"` and
/// `state` is `"<immutable_object>"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImmutableRefusalKey {
    /// Concrete object type name.
    pub object_type: String,
    /// Lifecycle property name, or `"<object>"` for entity-level immutability.
    pub lifecycle: String,
    /// Immutable state name, or `"<immutable_object>"` for entity-level immutability.
    pub state: String,
}

/// One observed Cedar rule key `(policy_id, rule_id)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CedarRuleKey {
    /// Source `AccessIR` policy id (or `"<default>"` when none matched).
    pub policy_id: String,
    /// Source `AccessIR` rule id (or deny reason code).
    pub rule_id: String,
}

/// Immutable snapshot of all domain-model coverage counters recorded so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainCoverageSnapshot {
    /// Committed lifecycle transitions (never object creations).
    pub transitions: BTreeMap<TransitionKey, u64>,
    /// Disallowed lifecycle transitions refused before or at commit.
    pub disallowed_transitions: BTreeMap<TransitionKey, u64>,
    /// Refusals on immutable states or immutable object types.
    pub immutable_refusals: BTreeMap<ImmutableRefusalKey, u64>,
    /// Governing laws that actively held (`Verdict::Holds`) on a committed write.
    pub law_holds: BTreeMap<String, u64>,
    /// Governing laws or invariants that evaluated `NotSatisfied` (refusal or finding).
    pub law_violations: BTreeMap<String, u64>,
    /// Cedar rules that allowed an authorization request.
    pub cedar_allows: BTreeMap<CedarRuleKey, u64>,
    /// Cedar rules or deny reasons that denied an authorization request.
    pub cedar_denies: BTreeMap<CedarRuleKey, u64>,
}

/// Shared, thread-safe recorder attached to an [`ObservedSqliteCell`] and shared
/// across decision/action/Cedar verification boundaries.
#[derive(Debug, Default)]
pub struct DomainCoverageRecorder {
    inner: Mutex<DomainCoverageSnapshot>,
}

impl DomainCoverageRecorder {
    /// Creates an empty coverage recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a cloned snapshot of all recorded counters.
    #[must_use]
    pub fn snapshot(&self) -> DomainCoverageSnapshot {
        match self.inner.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Merges all counters from `other` into this recorder.
    pub fn merge_snapshot(&self, other: &DomainCoverageSnapshot) {
        self.with_lock(|snap| {
            merge_map(&mut snap.transitions, &other.transitions);
            merge_map(
                &mut snap.disallowed_transitions,
                &other.disallowed_transitions,
            );
            merge_map(&mut snap.immutable_refusals, &other.immutable_refusals);
            merge_map(&mut snap.law_holds, &other.law_holds);
            merge_map(&mut snap.law_violations, &other.law_violations);
            merge_map(&mut snap.cedar_allows, &other.cedar_allows);
            merge_map(&mut snap.cedar_denies, &other.cedar_denies);
        });
    }

    /// Records one committed lifecycle transition.
    pub fn record_transition(&self, key: TransitionKey) {
        self.with_lock(|snap| bump(&mut snap.transitions, key));
    }

    /// Records one refused disallowed lifecycle transition.
    pub fn record_disallowed_transition(&self, key: TransitionKey) {
        self.with_lock(|snap| bump(&mut snap.disallowed_transitions, key));
    }

    /// Records one immutable-state or immutable-object refusal.
    pub fn record_immutable_refusal(&self, key: ImmutableRefusalKey) {
        self.with_lock(|snap| bump(&mut snap.immutable_refusals, key));
    }

    /// Records typed pre-commit [`DecisionRefusal`] outcomes (such as
    /// [`DecisionRefusal::ImmutableStateWrite`] or [`DecisionRefusal::NotFromState`])
    /// that fail inside `commit_decision` before reaching `CellHost::commit`.
    pub fn record_decision_refusal<X>(&self, refusal: &DecisionRefusal<X>) {
        if let DecisionRefusal::ImmutableStateWrite {
            key,
            lifecycle,
            state,
        } = refusal
        {
            self.record_immutable_refusal(ImmutableRefusalKey {
                object_type: key.object_type.as_str().to_owned(),
                lifecycle: lifecycle.clone(),
                state: state.clone(),
            });
        } else if let DecisionRefusal::NotFromState {
            lifecycle,
            key,
            transition,
            current,
            ..
        } = refusal
        {
            self.record_disallowed_transition(TransitionKey {
                object_type: key.object_type.as_str().to_owned(),
                lifecycle: lifecycle.clone(),
                transition: transition.clone(),
                from: current.clone().unwrap_or_else(|| "<unset>".to_owned()),
                to: "<refused>".to_owned(),
            });
        }
    }

    /// Records that a governing law actively held on a committed object write.
    pub fn record_law_hold(&self, law_id: impl Into<String>) {
        self.with_lock(|snap| bump(&mut snap.law_holds, law_id.into()));
    }

    /// Records that a governing law or invariant evaluated `NotSatisfied`.
    pub fn record_law_violation(&self, law_id: impl Into<String>) {
        self.with_lock(|snap| bump(&mut snap.law_violations, law_id.into()));
    }

    /// Records a Cedar allow hit for `(policy_id, rule_id)`.
    pub fn record_cedar_allow(&self, policy_id: impl Into<String>, rule_id: impl Into<String>) {
        self.with_lock(|snap| {
            bump(
                &mut snap.cedar_allows,
                CedarRuleKey {
                    policy_id: policy_id.into(),
                    rule_id: rule_id.into(),
                },
            );
        });
    }

    /// Records a Cedar deny hit for `(policy_id, rule_id_or_reason)`.
    pub fn record_cedar_deny(&self, policy_id: impl Into<String>, rule_id: impl Into<String>) {
        self.with_lock(|snap| {
            bump(
                &mut snap.cedar_denies,
                CedarRuleKey {
                    policy_id: policy_id.into(),
                    rule_id: rule_id.into(),
                },
            );
        });
    }

    fn with_lock(&self, f: impl FnOnce(&mut DomainCoverageSnapshot)) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard);
    }
}

fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, key: K) {
    let entry = map.entry(key).or_insert(0);
    *entry = entry.saturating_add(1);
}

fn merge_map<K: Ord + Clone>(dst: &mut BTreeMap<K, u64>, src: &BTreeMap<K, u64>) {
    for (key, count) in src {
        let entry = dst.entry(key.clone()).or_insert(0);
        *entry = entry.saturating_add(*count);
    }
}

/// Written object snapshot captured before commit for post-commit law accounting.
struct WrittenObjectSnapshot {
    key: FactKey,
    values: BTreeMap<String, RuntimeValue>,
}

/// Written objects and exact link keys touched by a commit.
struct CommitTargets {
    written: Vec<WrittenObjectSnapshot>,
    touched_links: Vec<LinkKey>,
}

/// Test-mode wrapper around [`SqliteCell`] that records domain-model coverage
/// on every commit without modifying production [`SqliteCell`] layout.
#[derive(Debug)]
pub struct ObservedSqliteCell<E, C, I, S: SqlEngine = DefaultEngine> {
    inner: SqliteCell<E, C, I, S>,
    recorder: Arc<DomainCoverageRecorder>,
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// Wraps this cell in an [`ObservedSqliteCell`] that tallies committed
    /// transitions, immutable refusals, and governing-law outcomes into
    /// `recorder`.
    #[must_use]
    pub fn into_observed(
        self,
        recorder: Arc<DomainCoverageRecorder>,
    ) -> ObservedSqliteCell<E, C, I, S> {
        ObservedSqliteCell::new(self, recorder)
    }
}

impl<E, C, I, S: SqlEngine> ObservedSqliteCell<E, C, I, S> {
    /// Wraps `inner` so commits update `recorder`.
    #[must_use]
    pub fn new(inner: SqliteCell<E, C, I, S>, recorder: Arc<DomainCoverageRecorder>) -> Self {
        Self { inner, recorder }
    }

    /// Borrows the underlying [`SqliteCell`].
    #[must_use]
    pub const fn inner(&self) -> &SqliteCell<E, C, I, S> {
        &self.inner
    }

    /// Mutably borrows the underlying [`SqliteCell`].
    pub const fn inner_mut(&mut self) -> &mut SqliteCell<E, C, I, S> {
        &mut self.inner
    }

    /// Unwraps and returns the underlying [`SqliteCell`].
    #[must_use]
    pub fn into_inner(self) -> SqliteCell<E, C, I, S> {
        self.inner
    }

    /// Borrows the attached [`DomainCoverageRecorder`].
    #[must_use]
    pub fn recorder(&self) -> &Arc<DomainCoverageRecorder> {
        &self.recorder
    }

    fn capture_commit_targets<Ev, Cmd, Inp>(commit: &CellCommit<Ev, Cmd, Inp>) -> CommitTargets {
        let written = commit
            .writes
            .iter()
            .filter_map(|write| match &write.mutation {
                FactMutation::Put { values } => Some(WrittenObjectSnapshot {
                    key: write.key.clone(),
                    values: values.clone(),
                }),
                FactMutation::Delete => None,
            })
            .collect();
        let touched_links = commit.links.iter().map(|lw| lw.key.clone()).collect();
        CommitTargets {
            written,
            touched_links,
        }
    }

    fn record_commit_outcome(
        &self,
        targets: &CommitTargets,
        is_observed: bool,
        result: &Result<CommitReceipt, CommitError<CellError>>,
    ) {
        match result {
            Ok(receipt) => {
                let finding_violated = self.record_committed_ledger_events(receipt.seq.0);
                self.record_active_law_holds(targets, is_observed, &finding_violated);
            }
            Err(CommitError::ImmutableObject { key }) => {
                self.recorder.record_immutable_refusal(ImmutableRefusalKey {
                    object_type: key.object_type.as_str().to_owned(),
                    lifecycle: "<object>".to_owned(),
                    state: "<immutable_object>".to_owned(),
                });
            }
            Err(CommitError::Law { reason, .. }) => {
                if let norn_kernel::governing_laws::LawEvaluationError::Violated(failure) = reason
                    && failure.outcome == DataQualityOutcome::NotSatisfied
                {
                    self.recorder
                        .record_law_violation(rule_ref_id(&failure.rule_ref));
                }
            }
            Err(CommitError::InvariantViolation { id, .. }) => {
                self.recorder.record_law_violation(id.as_str());
            }
            Err(
                CommitError::IdentityReuse
                | CommitError::IdentityCollision { .. }
                | CommitError::AtomicBoundary { .. }
                | CommitError::AtomicDeliveryBoundary { .. }
                | CommitError::OwnedRowsProtected { .. }
                | CommitError::DuplicateKey { .. }
                | CommitError::NotPending { .. }
                | CommitError::ReceiptNotAccepted { .. }
                | CommitError::Conflict { .. }
                | CommitError::DuplicateLink { .. }
                | CommitError::IdentifierTooLong { .. }
                | CommitError::LinkConflict { .. }
                | CommitError::LinkReadConflict { .. }
                | CommitError::UnsupportedLinkRead { .. }
                | CommitError::DuplicateLinkRead { .. }
                | CommitError::LinkReadTooLarge { .. }
                | CommitError::InvalidLinkRead { .. }
                | CommitError::PredicateReadConflict { .. }
                | CommitError::UnsupportedPredicateRead { .. }
                | CommitError::InvalidPredicateRead { .. }
                | CommitError::DanglingLink { .. }
                | CommitError::LinkTarget { .. }
                | CommitError::LinkCardinality { .. }
                | CommitError::LinkRequired { .. }
                | CommitError::TypeMismatch { .. }
                | CommitError::EffectiveValues { .. }
                | CommitError::TraversalLimitExceeded { .. }
                | CommitError::AccountingPosting { .. }
                | CommitError::InvariantBudgetExceeded { .. }
                | CommitError::InvariantModel { .. }
                | CommitError::LifecycleInitialization { .. }
                | CommitError::ObservationAuthorization
                | CommitError::EventLimitExceeded { .. }
                | CommitError::Overflow
                | CommitError::Host(_),
            ) => {}
        }
    }

    /// Reads the exact ledger events appended by commit `commit_seq`, recording
    /// every committed transition (including self-loops and multi-transition
    /// commits) and every `DataQualityFinding` violation. Returns the set of
    /// `(object_type, key, rule_id)` pairs that recorded a finding violation.
    fn record_committed_ledger_events(
        &self,
        commit_seq: u64,
    ) -> BTreeSet<(String, String, String)> {
        let mut violated = BTreeSet::new();
        let reader = self.inner.store().read();
        let Ok(Some(receipt)) = reader.receipt_at(commit_seq) else {
            return violated;
        };
        let count = usize::try_from(receipt.body.event_count).unwrap_or(0);
        if count == 0 {
            return violated;
        }
        let after = receipt.body.first_event_seq.saturating_sub(1);
        let Ok(entries) = reader.ledger_page(after, count) else {
            return violated;
        };
        for entry in entries {
            if entry.commit_seq != commit_seq {
                continue;
            }
            self.observe_ledger_event(&entry.event, &mut violated);
        }
        violated
    }

    fn observe_ledger_event(
        &self,
        event: &RuntimeValue,
        violated: &mut BTreeSet<(String, String, String)>,
    ) {
        // Dispatch internally-tagged `ObservationEvent` variants directly from
        // the `RuntimeValue` object so `DataQualityFinding` (which embeds
        // `RuntimeValue` / `RawValue` fields) is never buffered through serde's
        // `#[serde(tag = "op")]` `Content` deserializer.
        if let RuntimeValue::Object(obj) = event
            && let Some(op) = obj.get("op").and_then(RuntimeValue::as_str)
        {
            match op {
                "data_quality_finding" => {
                    if let Some(fact_val) = obj.get("fact")
                        && let Ok(fact_json) = serde_json::to_string(fact_val)
                        && let Ok(fact) = serde_json::from_str::<DataQualityFinding>(&fact_json)
                    {
                        self.record_finding_violation(&fact, violated);
                    }
                    return;
                }
                "observed_fact" => {
                    if let Some(inner_event) = obj.get("event") {
                        self.observe_ledger_event(inner_event, violated);
                    }
                    return;
                }
                _ => {}
            }
        }
        let Ok(json_text) = serde_json::to_string(event) else {
            return;
        };
        if let Ok(fact_event) = serde_json::from_str::<FactEvent>(&json_text) {
            if let FactEvent::Transitioned {
                key,
                lifecycle,
                transition,
                from,
                to,
            } = fact_event
            {
                self.recorder.record_transition(TransitionKey {
                    object_type: key.object_type.as_str().to_owned(),
                    lifecycle: lifecycle.as_str().to_owned(),
                    transition: transition.as_str().to_owned(),
                    from,
                    to,
                });
            }
            return;
        }
        if let Ok(obs_event) = serde_json::from_str::<ObservationEvent>(&json_text) {
            match obs_event {
                ObservationEvent::ObservedTransition {
                    key,
                    property,
                    transition,
                    from,
                    to,
                    ..
                } => {
                    self.recorder.record_transition(TransitionKey {
                        object_type: key.object_type.as_str().to_owned(),
                        lifecycle: property.as_str().to_owned(),
                        transition: transition.as_str().to_owned(),
                        from,
                        to,
                    });
                }
                ObservationEvent::DataQualityFinding { fact } => {
                    self.record_finding_violation(&fact, violated);
                }
                ObservationEvent::ObservedFact { .. }
                | ObservationEvent::ObservedDeleted { .. }
                | ObservationEvent::ObservedRecreated { .. } => {}
            }
        }
    }

    fn record_finding_violation(
        &self,
        finding: &DataQualityFinding,
        violated: &mut BTreeSet<(String, String, String)>,
    ) {
        let rule_id = rule_ref_id(&finding.rule_ref);
        if finding.outcome == DataQualityOutcome::NotSatisfied {
            self.recorder.record_law_violation(rule_id.clone());
        }
        for subject in &finding.subjects {
            violated.insert((
                subject.object_type.as_str().to_owned(),
                subject.key.clone(),
                rule_id.clone(),
            ));
        }
    }

    fn record_active_law_holds(
        &self,
        targets: &CommitTargets,
        is_observed: bool,
        finding_violated: &BTreeSet<(String, String, String)>,
    ) {
        let model = self.inner.store().model();
        let reader = self.inner.store().read();
        let ctx = SubjectHoldCtx {
            model,
            reader: &reader,
            is_observed,
            finding_violated,
        };
        let mut direct_keys = BTreeSet::new();
        let mut linked_triggers: BTreeMap<FactKey, BTreeSet<(bool, String, ObjectTypeName)>> =
            BTreeMap::new();

        for item in &targets.written {
            let item_key = resolve_concrete_key(&reader, item.key.clone());
            direct_keys.insert(item_key.clone());
            self.record_subject_law_holds(&ctx, &item_key, Some(&item.values), None);
            // A write to an object referenced by a link re-evaluates only laws
            // on the connected neighbor that actually read that specific edge
            // and matching endpoint type.
            if let Ok(edges) =
                reader.links_touching(MAIN_WORLD, item_key.object_type.as_str(), &item_key.key)
            {
                let item_ty = item_key.object_type.as_str();
                for edge in edges {
                    if edge.target == item_key.key && model.is_a(item_ty, &edge.target_type) {
                        let src = resolve_concrete_key(
                            &reader,
                            FactKey::new(edge.source_type.clone(), edge.source.clone()),
                        );
                        if src != item_key {
                            linked_triggers.entry(src).or_default().insert((
                                true,
                                edge.link.clone(),
                                item_key.object_type.clone(),
                            ));
                        }
                    }
                    if edge.source == item_key.key && model.is_a(item_ty, &edge.source_type) {
                        let dst = resolve_concrete_key(
                            &reader,
                            FactKey::new(edge.target_type, edge.target),
                        );
                        if dst != item_key {
                            linked_triggers.entry(dst).or_default().insert((
                                false,
                                edge.link,
                                item_key.object_type.clone(),
                            ));
                        }
                    }
                }
            }
        }
        for lw in &targets.touched_links {
            let src = resolve_concrete_key(&reader, lw.source.clone());
            let dst = resolve_concrete_key(&reader, lw.target.clone());
            if !direct_keys.contains(&src) {
                linked_triggers.entry(src.clone()).or_default().insert((
                    true,
                    lw.link.clone(),
                    dst.object_type.clone(),
                ));
            }
            if !direct_keys.contains(&dst) {
                linked_triggers.entry(dst).or_default().insert((
                    false,
                    lw.link.clone(),
                    src.object_type,
                ));
            }
        }
        for (key, triggers) in linked_triggers {
            if !direct_keys.contains(&key) {
                self.record_subject_law_holds(&ctx, &key, None, Some(&triggers));
            }
        }
    }

    fn record_subject_law_holds(
        &self,
        ctx: &SubjectHoldCtx<'_, S>,
        key: &FactKey,
        fallback_values: Option<&BTreeMap<String, RuntimeValue>>,
        touched_link_triggers: Option<&BTreeSet<(bool, String, ObjectTypeName)>>,
    ) {
        let concrete_key = resolve_concrete_key(ctx.reader, key.clone());
        let ty_name = concrete_key.object_type.as_str();
        let Ok(artifacts) = ctx.model.governing_laws(ty_name) else {
            return;
        };
        if artifacts.is_empty() {
            return;
        }
        let Some(effective_values) = ctx
            .reader
            .object(MAIN_WORLD, ty_name, &concrete_key.key)
            .ok()
            .flatten()
            .map(|record| record.values)
            .or_else(|| fallback_values.cloned())
        else {
            return;
        };
        let key = &concrete_key;
        for artifact in &artifacts {
            let Some(law) = decode_law_artifact(artifact) else {
                continue;
            };
            if let Some(triggers) = touched_link_triggers {
                let single = [artifact.clone()];
                let single_catalog = ctx.model.type_catalog().with_governing_laws(&single);
                let Ok(index) = norn_kernel::governing_laws::link_law_index(&single_catalog) else {
                    continue;
                };
                let reads_touched_edge =
                    index.forward.iter().any(|(subj, link, declared_target)| {
                        *subj == key.object_type
                            && triggers.iter().any(|(is_fwd, t_link, t_ty)| {
                                *is_fwd
                                    && t_link == link
                                    && norn_kernel::governing_laws::reaches(
                                        &single_catalog,
                                        t_ty,
                                        declared_target,
                                    )
                            })
                    }) || index.inverse.iter().any(|(subj, declared_src, link)| {
                        *subj == key.object_type
                            && triggers.iter().any(|(is_fwd, t_link, t_ty)| {
                                !*is_fwd
                                    && t_link == link
                                    && norn_kernel::governing_laws::reaches(
                                        &single_catalog,
                                        t_ty,
                                        declared_src,
                                    )
                            })
                    });
                if !reads_touched_edge {
                    continue;
                }
            }
            if ctx.is_observed
                && ctx.finding_violated.contains(&(
                    ty_name.to_owned(),
                    key.key.clone(),
                    law.id.clone(),
                ))
            {
                continue;
            }
            if evaluate_single_law_hold(
                ctx.model,
                ctx.reader,
                artifact,
                &law,
                key,
                &effective_values,
            ) {
                self.recorder.record_law_hold(law.id);
            }
        }
    }
}

fn resolve_concrete_key<S: SqlEngine>(
    reader: &crate::read::StoreReader<'_, S>,
    key: FactKey,
) -> FactKey {
    reader
        .object(MAIN_WORLD, key.object_type.as_str(), &key.key)
        .ok()
        .flatten()
        .map_or(key, |record| {
            FactKey::new(record.object_type.as_str(), record.id)
        })
}

struct SubjectHoldCtx<'a, S: SqlEngine> {
    model: &'a crate::model::Model,
    reader: &'a crate::read::StoreReader<'a, S>,
    is_observed: bool,
    finding_violated: &'a BTreeSet<(String, String, String)>,
}

fn evaluate_single_law_hold<S: SqlEngine>(
    model: &crate::model::Model,
    reader: &crate::read::StoreReader<'_, S>,
    artifact: &norn_ir::ArtifactEnvelope,
    law: &LawIR,
    key: &FactKey,
    effective_values: &norn_kernel::cell_host::JsonObject,
) -> bool {
    match &law.response {
        LawResponse::Predicate { .. } => {
            if !linked_law_condition_active(model, reader, artifact, law, key, effective_values) {
                return false;
            }
            let single = [artifact.clone()];
            let catalog = model.type_catalog().with_governing_laws(&single);
            let mut links = crate::link_laws::CurrentLinks {
                reader,
                world: MAIN_WORLD,
                subject: key.clone(),
            };
            let mut remaining = norn_kernel::TurnFuel::new(1_000_000);
            norn_kernel::governing_laws::evaluate_linked(
                &catalog,
                key,
                effective_values,
                &mut links,
                &mut remaining,
            )
            .is_ok_and(|failures| failures.is_empty())
        }
        LawResponse::Invariant { .. } => {
            if !matches!(law.modality, LawModality::Always {}) || law.condition.is_some() {
                return false;
            }
            let single = [artifact.clone()];
            let catalog = model.type_catalog().with_governing_laws(&single);
            let mut links = crate::link_laws::CurrentLinks {
                reader,
                world: MAIN_WORLD,
                subject: key.clone(),
            };
            let mut remaining = norn_kernel::TurnFuel::new(1_000_000);
            norn_kernel::governing_laws::evaluate_linked(
                &catalog,
                key,
                effective_values,
                &mut links,
                &mut remaining,
            )
            .is_ok_and(|failures| failures.is_empty())
        }
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => false,
    }
}

fn linked_law_condition_active<S: SqlEngine>(
    model: &crate::model::Model,
    reader: &crate::read::StoreReader<'_, S>,
    artifact: &norn_ir::ArtifactEnvelope,
    law: &LawIR,
    key: &FactKey,
    effective_values: &norn_kernel::cell_host::JsonObject,
) -> bool {
    if law.condition.is_none() && law.exceptions.is_empty() {
        return true;
    }
    let mut probe_law = law.clone();
    probe_law.modality = LawModality::Always {};
    probe_law.response = LawResponse::Predicate {
        expression: norn_ir::ZenExpression("false".to_owned()),
    };
    let Ok(json_text) = serde_json::to_string(&probe_law) else {
        return false;
    };
    let Ok(json_val) = serde_json::from_str::<JsonValue>(&json_text) else {
        return false;
    };
    let Ok(payload_val) = ArtifactPayload::from_kind_json(&ArtifactKind::Law, json_val) else {
        return false;
    };
    let mut probe_env = artifact.clone();
    probe_env.payload = payload_val;
    let single = [probe_env];
    let catalog = model.type_catalog().with_governing_laws(&single);
    let mut links = crate::link_laws::CurrentLinks {
        reader,
        world: MAIN_WORLD,
        subject: key.clone(),
    };
    let mut remaining = norn_kernel::TurnFuel::new(1_000_000);
    // The probe law emits `NotSatisfied` on its `false` predicate iff its
    // activation condition holds and no exception lifts the law.
    norn_kernel::governing_laws::evaluate_linked(
        &catalog,
        key,
        effective_values,
        &mut links,
        &mut remaining,
    )
    .is_ok_and(|failures| {
        failures
            .iter()
            .any(|failure| failure.outcome == DataQualityOutcome::NotSatisfied)
    })
}

fn rule_ref_id(rule_ref: &DataQualityRuleRef) -> String {
    match rule_ref {
        DataQualityRuleRef::Law { law_id, .. } => law_id.clone(),
        DataQualityRuleRef::Invariant { invariant_id, .. } => invariant_id.as_str().to_owned(),
        DataQualityRuleRef::Builtin { id, .. } => id.clone(),
    }
}

fn decode_law_artifact(artifact: &norn_ir::ArtifactEnvelope) -> Option<LawIR> {
    serde_json::to_string(&artifact.payload.to_json_value())
        .ok()
        .and_then(|text| serde_json::from_str::<LawIR>(&text).ok())
}

impl<E, C, I, S: SqlEngine> CellHost for ObservedSqliteCell<E, C, I, S>
where
    E: Serialize + 'static,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    type Event = E;
    type Command = C;
    type Input = I;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.inner.load(key)
    }

    fn supports_reference_reads(&self) -> bool {
        self.inner.supports_reference_reads()
    }
    fn supports_projected_read(
        &self,
        object_type: &norn_ir::ObjectTypeName,
        properties: &[&str],
    ) -> bool {
        self.inner.supports_projected_read(object_type, properties)
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.inner.load_reference(key, max_bytes)
    }
    fn load_projected(
        &self,
        key: &FactKey,
        properties: &[&str],
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.inner.load_projected(key, properties, max_bytes)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.inner.load_bounded(key, max_bytes)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &norn_kernel::receipt_binding::ReceiptReference,
        manifest: &norn_ir::function::Digest,
    ) -> Result<
        Option<norn_kernel::receipt_binding::ReceiptBinding<norn_kernel::cell_host::JsonObject>>,
        norn_kernel::receipt_binding::ReceiptBindingError<CellError>,
    > {
        self.inner.resolve_accepted_receipt(reference, manifest)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.inner.load_link(key)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, CellError> {
        self.inner.read_predicate(predicate, max_members, max_bytes)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        self.inner
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn commit(
        &mut self,
        commit: CellCommit<E, C, I>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let targets = Self::capture_commit_targets(&commit);
        let result = self.inner.commit(commit);
        self.record_commit_outcome(&targets, false, &result);
        result
    }
    fn commit_with_evidence(
        &mut self,
        commit: CellCommit<E, C, I>,
        evidence: norn_kernel::CommitEvidence<'_>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let targets = Self::capture_commit_targets(&commit);
        let result = self.inner.commit_with_evidence(commit, evidence);
        self.record_commit_outcome(&targets, false, &result);
        result
    }
    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<E, C, I>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let targets = Self::capture_commit_targets(&commit);
        let result = self.inner.commit_owned_rows(commit, proof);
        self.record_commit_outcome(&targets, false, &result);
        result
    }
    fn commit_observed(
        &mut self,
        commit: CellCommit<E, C, I>,
        authorization: ObservationAuthorization,
    ) -> Result<CommitReceipt, CommitError<CellError>>
    where
        E: From<ObservationEvent>,
    {
        let targets = Self::capture_commit_targets(&commit);
        let result = self.inner.commit_observed(commit, authorization);
        self.record_commit_outcome(&targets, true, &result);
        result
    }
    fn perform(&mut self) -> Result<Option<Performed<I>>, CellError> {
        self.inner.perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<I>>, CellError> {
        self.inner.observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<I>>, CellError> {
        self.inner.clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.inner.publish(receipt)
    }
}
