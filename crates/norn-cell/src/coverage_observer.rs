//! Domain-model coverage instrumentation over [`SqliteCell`] (#1656).
//!
//! [`ObservedSqliteCell`] wraps a [`SqliteCell`] without adding fields or runtime
//! overhead to production cells. Whenever a test drives commits through it, the
//! attached [`DomainCoverageRecorder`] tallies every committed lifecycle
//! transition `(object_type, lifecycle, from -> to)`, every immutable-object
//! refusal, and every governing law (`holds` and `violated`) evaluated on the
//! cell's admitted model.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use norn_ir::{ObjectTypeName, RuntimeValue};
use norn_kernel::cell_host::{FactKey, FactMutation, FactState};
use norn_kernel::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, LinkKey,
    LinkReadTarget, LinkState, ObservationAuthorization, ObservationEvent, Observed,
    OwnedRowsCommitProof, Performed, PredicateObject, PredicateQuery,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::{DefaultEngine, SqlEngine};
use crate::error::CellError;
use crate::host::SqliteCell;
use crate::store::MAIN_WORLD;

/// One observed lifecycle transition `(object_type, lifecycle, from, to)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TransitionKey {
    /// Concrete object type name.
    pub object_type: String,
    /// Lifecycle property name.
    pub lifecycle: String,
    /// State before the write (`"<initial>"` for creation).
    pub from: String,
    /// State after the write.
    pub to: String,
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
    /// Committed lifecycle transitions and creations.
    pub transitions: BTreeMap<TransitionKey, u64>,
    /// Disallowed lifecycle transitions refused before or at commit.
    pub disallowed_transitions: BTreeMap<TransitionKey, u64>,
    /// Refusals on immutable states or immutable object types `(object_type, target)`.
    pub immutable_refusals: BTreeMap<(String, String), u64>,
    /// Governing laws that held on a committed write.
    pub law_holds: BTreeMap<String, u64>,
    /// Governing laws that refused a write.
    pub law_violations: BTreeMap<String, u64>,
    /// Cedar rules that allowed an authorization request.
    pub cedar_allows: BTreeMap<CedarRuleKey, u64>,
    /// Cedar rules or deny reasons that denied an authorization request.
    pub cedar_denies: BTreeMap<CedarRuleKey, u64>,
}

/// Shared, thread-safe recorder attached to an [`ObservedSqliteCell`].
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

    /// Records one committed lifecycle transition.
    pub fn record_transition(&self, key: TransitionKey) {
        self.with_lock(|snap| bump(&mut snap.transitions, key));
    }

    /// Records one refused disallowed lifecycle transition.
    pub fn record_disallowed_transition(&self, key: TransitionKey) {
        self.with_lock(|snap| bump(&mut snap.disallowed_transitions, key));
    }

    /// Records one immutable-state or immutable-object refusal.
    pub fn record_immutable_refusal(
        &self,
        object_type: impl Into<String>,
        state: impl Into<String>,
    ) {
        self.with_lock(|snap| {
            bump(
                &mut snap.immutable_refusals,
                (object_type.into(), state.into()),
            );
        });
    }

    /// Records that a governing law held on a committed object write.
    pub fn record_law_hold(&self, law_id: impl Into<String>) {
        self.with_lock(|snap| bump(&mut snap.law_holds, law_id.into()));
    }

    /// Records that a governing law refused a proposed write.
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

/// Pre-commit snapshot of lifecycle property values for one written object.
struct PreWriteObservation {
    object_type: String,
    before_states: BTreeMap<String, String>,
    after_states: BTreeMap<String, String>,
    governing_law_ids: BTreeSet<String>,
}

/// Test-mode wrapper around [`SqliteCell`] that records domain-model coverage
/// on every commit without modifying production [`SqliteCell`] layout.
#[derive(Debug)]
pub struct ObservedSqliteCell<E, C, I, S: SqlEngine = DefaultEngine> {
    inner: SqliteCell<E, C, I, S>,
    recorder: Arc<DomainCoverageRecorder>,
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// Wraps this cell in an [`ObservedSqliteCell`] that tallies lifecycle
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

    fn capture_pre_commit<Ev, Cmd, Inp>(
        &self,
        commit: &CellCommit<Ev, Cmd, Inp>,
    ) -> Vec<PreWriteObservation> {
        let model = self.inner.store().model();
        let reader = self.inner.store().read();
        let mut captured = Vec::new();
        for write in &commit.writes {
            let FactMutation::Put { values } = &write.mutation else {
                continue;
            };
            let type_name = write.key.object_type.as_str();
            let lifecycle_props = lifecycle_properties_for(model, &write.key.object_type);
            let before_obj = reader
                .object(MAIN_WORLD, type_name, &write.key.key)
                .ok()
                .flatten();
            let before_states =
                extract_states(before_obj.as_ref().map(|r| &r.values), &lifecycle_props);
            let after_states = extract_states(Some(values), &lifecycle_props);
            let governing_law_ids = governing_law_ids_for(model, type_name);
            captured.push(PreWriteObservation {
                object_type: type_name.to_owned(),
                before_states,
                after_states,
                governing_law_ids,
            });
        }
        captured
    }

    fn record_commit_outcome(
        &self,
        captured: &[PreWriteObservation],
        result: &Result<CommitReceipt, CommitError<CellError>>,
    ) {
        match result {
            Ok(_) => {
                for obs in captured {
                    for (prop, after) in &obs.after_states {
                        let from = obs
                            .before_states
                            .get(prop)
                            .cloned()
                            .unwrap_or_else(|| "<initial>".to_owned());
                        if from != *after {
                            self.recorder.record_transition(TransitionKey {
                                object_type: obs.object_type.clone(),
                                lifecycle: prop.clone(),
                                from,
                                to: after.clone(),
                            });
                        }
                    }
                    for law_id in &obs.governing_law_ids {
                        self.recorder.record_law_hold(law_id.clone());
                    }
                }
            }
            Err(CommitError::ImmutableObject { key }) => {
                self.recorder
                    .record_immutable_refusal(key.object_type.as_str(), "<immutable_object>");
            }
            Err(CommitError::Law { reason, .. }) => {
                if let norn_kernel::governing_laws::LawEvaluationError::Violated(failure) = reason {
                    let rule_id = match &failure.rule_ref {
                        norn_ir::DataQualityRuleRef::Law { law_id, .. } => law_id.clone(),
                        norn_ir::DataQualityRuleRef::Invariant { invariant_id, .. } => {
                            invariant_id.as_str().to_owned()
                        }
                        norn_ir::DataQualityRuleRef::Builtin { id, .. } => id.clone(),
                    };
                    self.recorder.record_law_violation(rule_id);
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
                | CommitError::ObservationAuthorization
                | CommitError::EventLimitExceeded { .. }
                | CommitError::Overflow
                | CommitError::Host(_),
            ) => {}
        }
    }
}

fn lifecycle_properties_for(
    model: &crate::model::Model,
    object_type: &ObjectTypeName,
) -> Vec<String> {
    let catalog = model.type_catalog();
    catalog
        .lifecycles(object_type)
        .into_keys()
        .map(|prop| prop.as_str().to_owned())
        .collect()
}

fn extract_states(
    values: Option<&BTreeMap<String, RuntimeValue>>,
    lifecycle_props: &[String],
) -> BTreeMap<String, String> {
    let Some(values) = values else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for prop in lifecycle_props {
        if let Some(RuntimeValue::String(state)) = values.get(prop) {
            out.insert(prop.clone(), state.clone());
        }
    }
    out
}

fn governing_law_ids_for(model: &crate::model::Model, object_type: &str) -> BTreeSet<String> {
    model
        .governing_laws(object_type)
        .map_or_default(|artifacts| {
            artifacts
                .into_iter()
                .filter_map(|artifact| {
                    artifact
                        .payload
                        .to_json_value()
                        .as_object()
                        .and_then(|obj| obj.get("id"))
                        .and_then(norn_ir::JsonValue::as_string)
                        .map(str::to_owned)
                })
                .collect()
        })
}

impl<E, C, I, S: SqlEngine> CellHost for ObservedSqliteCell<E, C, I, S>
where
    E: Serialize,
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

    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.inner.load_reference(key, max_bytes)
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.inner.load_bounded(key, max_bytes)
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
        let captured = self.capture_pre_commit(&commit);
        let result = self.inner.commit(commit);
        self.record_commit_outcome(&captured, &result);
        result
    }

    fn commit_action_references(
        &mut self,
        commit: CellCommit<E, C, I>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let captured = self.capture_pre_commit(&commit);
        let result = self.inner.commit_action_references(commit, proof, quantity);
        self.record_commit_outcome(&captured, &result);
        result
    }

    fn commit_action_quantity(
        &mut self,
        commit: CellCommit<E, C, I>,
        requests: &[norn_ir::quantity_ledger::QuantityRequest],
        idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let captured = self.capture_pre_commit(&commit);
        let result = self
            .inner
            .commit_action_quantity(commit, requests, idempotency_key);
        self.record_commit_outcome(&captured, &result);
        result
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<E, C, I>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let captured = self.capture_pre_commit(&commit);
        let result = self.inner.commit_owned_rows(commit, proof);
        self.record_commit_outcome(&captured, &result);
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
        let captured = self.capture_pre_commit(&commit);
        let result = self.inner.commit_observed(commit, authorization);
        self.record_commit_outcome(&captured, &result);
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
