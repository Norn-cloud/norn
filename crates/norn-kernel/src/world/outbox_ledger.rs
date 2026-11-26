//! Immutable per-generation outbox ledger and replay contract (Lane D Wave 1).
//!
//! [`super::outbox::step`] defines the single-row D03 lifecycle. This module
//! lifts that lifecycle onto an append-only, immutable per-generation log keyed
//! by `(operation, generation)` ([`GenerationKey`]) whose mutable queue head is
//! purely a derived [`HeadProjection`]:
//!
//! - **Immutable per-generation history:** `Requested`, `Dispatched`,
//!   `Outcome`, and `Reconciliation` records attach to their own
//!   `(operation, generation)` and are never overwritten by a later generation.
//!   Once set on a generation, `dispatch`, `outcome`, and `consumed_commit_seq`
//!   are strictly write-once.
//! - **Old results never settle new generations:** a late provider outcome
//!   carries the [`DispatchProof`] of the generation that claimed it and
//!   attaches only to that original generation without mutating any newer
//!   active generation or its [`HeadProjection`].
//! - **Same-generation payload divergence fails closed:** re-recording
//!   `Requested` at the same `(operation, generation)` with a different
//!   [`OutboxDigest`] or [`ProviderDedupContract`] refuses with
//!   [`OutboxLedgerRefusal::PayloadDiverged`]; an identical replay is an
//!   idempotent no-op.
//! - **Ambiguous dispatches gate reissue:** an active generation that was
//!   claimed (`Dispatched` / `OutcomeUnknown`) with no recorded outcome refuses
//!   reissue with [`OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation`]
//!   unless either durable [`ReconciliationDecision::AuthorizeReissue`] was
//!   recorded or both generations declare
//!   [`ProviderDedupContract::OperationKeyIdempotent`] with the exact same
//!   payload digest (and no restore fence is active).
//! - **Pure replay equivalence:** replaying a record sequence through
//!   [`OperationLedger::replay`] yields the exact same ledger and
//!   [`HeadProjection`] as incremental [`OperationLedger::apply`], and any
//!   refused [`OperationLedger::apply`] leaves the ledger unchanged.

mod types;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::outbox::{OperationKey, OutboxState, Queue};
pub use types::{
    ApplyStatus, CompensationStatus, DispatchProof, GenerationKey, GenerationRecord,
    HeadProjection, OutboxDigest, OutboxLedgerRefusal, OutboxRecord, OutboxRecordKind,
    OutcomeEvidence, OutcomeSource, ProviderDedupContract, ReconciliationDecision,
    ReconciliationEntry, RecordedOutcome,
};

/// Immutable per-generation ledger for one business operation (`OperationKey`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationLedger {
    operation: OperationKey,
    queue: Queue,
    active: GenerationRecord,
    prior_generations: BTreeMap<u64, GenerationRecord>,
    fence: Option<u64>,
    authorized_reissue: Option<u64>,
    compensation: Option<CompensationStatus>,
}

impl OperationLedger {
    /// Initialize a ledger from its first [`OutboxRecordKind::Requested`] record.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxLedgerRefusal::NotInitialized`] if `record.kind` is not
    /// [`OutboxRecordKind::Requested`].
    pub const fn start(record: &OutboxRecord) -> Result<Self, OutboxLedgerRefusal> {
        let OutboxRecordKind::Requested {
            queue,
            payload_digest,
            dedup_contract,
            commit_seq,
        } = record.kind
        else {
            return Err(OutboxLedgerRefusal::NotInitialized);
        };
        Ok(Self {
            operation: record.operation,
            queue,
            active: GenerationRecord {
                generation: record.generation,
                queue,
                payload_digest,
                dedup_contract,
                enqueue_commit_seq: commit_seq,
                dispatch: None,
                outcome_unknown: false,
                outcome: None,
                fenced_commit_seq: None,
                reconciliations: Vec::new(),
                consumed_commit_seq: None,
            },
            prior_generations: BTreeMap::new(),
            fence: None,
            authorized_reissue: None,
            compensation: None,
        })
    }

    /// Deterministically rebuild an [`OperationLedger`] by replaying an ordered
    /// slice of [`OutboxRecord`]s from its first `Requested` record.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxLedgerRefusal::EmptyLog`] when `records` is empty, or
    /// the first [`OutboxLedgerRefusal`] encountered during replay.
    pub fn replay(records: &[OutboxRecord]) -> Result<Self, OutboxLedgerRefusal> {
        let Some((first, rest)) = records.split_first() else {
            return Err(OutboxLedgerRefusal::EmptyLog);
        };
        let mut ledger = Self::start(first)?;
        for record in rest {
            let _status = ledger.apply(record)?;
        }
        Ok(ledger)
    }

    /// The business operation identity.
    #[must_use]
    pub const fn operation(&self) -> OperationKey {
        self.operation
    }

    /// The fixed queue assigned at first enqueue.
    #[must_use]
    pub const fn queue(&self) -> Queue {
        self.queue
    }

    /// The current active (highest requested) generation.
    #[must_use]
    pub const fn active_generation(&self) -> u64 {
        self.active.generation
    }

    /// The active generation's record.
    #[must_use]
    pub const fn active(&self) -> &GenerationRecord {
        &self.active
    }

    /// Immutable map of all superseded prior generations (`< active_generation`).
    #[must_use]
    pub const fn prior_generations(&self) -> &BTreeMap<u64, GenerationRecord> {
        &self.prior_generations
    }

    /// The highest restore fence generation recorded, if any.
    #[must_use]
    pub const fn fence(&self) -> Option<u64> {
        self.fence
    }

    /// The currently authorized reissue generation, if any.
    #[must_use]
    pub const fn authorized_reissue(&self) -> Option<u64> {
        self.authorized_reissue
    }

    /// Current compensation status, if any.
    #[must_use]
    pub const fn compensation(&self) -> Option<CompensationStatus> {
        self.compensation
    }

    /// Look up one generation's immutable record (active or prior).
    #[must_use]
    pub fn generation(&self, generation: u64) -> Option<&GenerationRecord> {
        if generation == self.active.generation {
            Some(&self.active)
        } else {
            self.prior_generations.get(&generation)
        }
    }

    /// Generations strictly older than [`Self::active_generation`] that have a
    /// recorded successful outcome (`ok == true`).
    #[must_use]
    pub fn prior_confirmed_successes(&self) -> Vec<u64> {
        self.prior_generations
            .iter()
            .filter(|(_, rec)| rec.outcome.is_some_and(|outcome| outcome.ok))
            .map(|(gen_id, _)| *gen_id)
            .collect()
    }

    /// Whether the active head is terminal (`Consumed` or `Compensated`).
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self.compensation,
            Some(CompensationStatus::Compensated { .. })
        ) || self.active.consumed_commit_seq.is_some()
    }

    /// Derive the mutable queue head purely from the immutable ledger.
    #[must_use]
    pub fn head_projection(&self) -> HeadProjection {
        let active_fence = self.active_fence();
        let reconciled_outcome = self.active.has_reconciled_outcome();
        let masked_by_fence = active_fence.is_some() && !reconciled_outcome;

        let state = match self.compensation {
            Some(CompensationStatus::Compensated { .. }) => OutboxState::Compensated,
            Some(CompensationStatus::Pending { attempts, .. }) => {
                OutboxState::CompensationPending { attempts }
            }
            None => {
                if let Some(commit_seq) = self.active.consumed_commit_seq {
                    OutboxState::Consumed { commit_seq }
                } else if let Some(fenced_at) = active_fence.filter(|_| !reconciled_outcome) {
                    OutboxState::ReconciliationRequired { fenced_at }
                } else if let Some(outcome) = self.active.outcome {
                    if outcome.ok {
                        OutboxState::Succeeded
                    } else {
                        OutboxState::Failed
                    }
                } else if self.active.dispatch.is_some() {
                    if self.active.outcome_unknown {
                        OutboxState::OutcomeUnknown {
                            generation: self.active.generation,
                        }
                    } else {
                        OutboxState::Dispatched {
                            generation: self.active.generation,
                        }
                    }
                } else {
                    OutboxState::Requested
                }
            }
        };

        let outcome = if masked_by_fence {
            None
        } else {
            self.active.outcome
        };

        let has_prior_confirmed_success = self
            .prior_generations
            .values()
            .any(|rec| rec.outcome.is_some_and(|recorded| recorded.ok));

        HeadProjection {
            operation: self.operation,
            queue: self.queue,
            generation: self.active.generation,
            payload_digest: self.active.payload_digest,
            dedup_contract: self.active.dedup_contract,
            state,
            outcome,
            has_prior_confirmed_success,
        }
    }

    /// Validate and apply one immutable [`OutboxRecord`] to this ledger.
    ///
    /// On refusal (`Err`), `self` is left completely unchanged.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OutboxLedgerRefusal`] explaining why `record` cannot
    /// be admitted.
    pub fn apply(&mut self, record: &OutboxRecord) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if record.operation != self.operation {
            return Err(OutboxLedgerRefusal::OperationMismatch);
        }
        if record.generation < self.active.generation {
            return self.apply_prior(record);
        }
        if self.is_terminal() {
            return Err(OutboxLedgerRefusal::Terminal);
        }

        match record.kind {
            OutboxRecordKind::Requested {
                queue,
                payload_digest,
                dedup_contract,
                commit_seq,
            } => self.apply_requested(
                record.generation,
                queue,
                payload_digest,
                dedup_contract,
                commit_seq,
            ),
            OutboxRecordKind::Dispatched { claim_commit_seq } => {
                self.apply_dispatched(record.generation, claim_commit_seq)
            }
            OutboxRecordKind::Outcome {
                evidence,
                ok,
                outcome_digest,
                commit_seq,
            } => self.apply_active_outcome(
                record.generation,
                evidence,
                ok,
                outcome_digest,
                commit_seq,
            ),
            OutboxRecordKind::CrashObserved => self.apply_active_crash(record.generation),
            OutboxRecordKind::RestoreFenced { commit_seq } => {
                self.apply_restore_fenced(record.generation, commit_seq)
            }
            OutboxRecordKind::Reconciliation {
                decision,
                commit_seq,
            } => self.apply_reconciliation(record.generation, decision, commit_seq),
            OutboxRecordKind::Compensation { ok, commit_seq } => {
                self.apply_compensation(record.generation, ok, commit_seq)
            }
            OutboxRecordKind::Consumed { commit_seq } => {
                self.apply_consumed(record.generation, commit_seq)
            }
        }
    }

    fn apply_prior(&mut self, record: &OutboxRecord) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        match record.kind {
            OutboxRecordKind::Outcome {
                evidence: OutcomeEvidence::DispatchProof { proof },
                ok,
                outcome_digest,
                commit_seq,
            } => self.attach_dispatch_outcome(
                record.generation,
                proof,
                ok,
                outcome_digest,
                commit_seq,
            ),
            OutboxRecordKind::Reconciliation {
                decision: decision @ ReconciliationDecision::ConfirmedOutcome { ok, outcome_digest },
                commit_seq,
            } => self.confirm_prior_generation_outcome(
                record.generation,
                decision,
                ok,
                outcome_digest,
                commit_seq,
            ),
            OutboxRecordKind::Compensation { ok, commit_seq } => {
                self.apply_compensation(record.generation, ok, commit_seq)
            }
            OutboxRecordKind::CrashObserved => {
                if self.is_terminal() {
                    return Err(OutboxLedgerRefusal::Terminal);
                }
                self.apply_prior_crash(record.generation)
            }
            OutboxRecordKind::Requested { .. }
            | OutboxRecordKind::Dispatched { .. }
            | OutboxRecordKind::Outcome {
                evidence: OutcomeEvidence::Resolved,
                ..
            }
            | OutboxRecordKind::RestoreFenced { .. }
            | OutboxRecordKind::Reconciliation {
                decision:
                    ReconciliationDecision::AuthorizeReissue { .. }
                    | ReconciliationDecision::RequireCompensation,
                ..
            }
            | OutboxRecordKind::Consumed { .. } => {
                if self.is_terminal() {
                    Err(OutboxLedgerRefusal::Terminal)
                } else {
                    Err(OutboxLedgerRefusal::StaleGeneration)
                }
            }
        }
    }

    fn active_fence(&self) -> Option<u64> {
        self.fence
            .filter(|fenced_at| self.active.generation <= *fenced_at)
    }

    fn generation_mut(&mut self, generation: u64) -> Option<&mut GenerationRecord> {
        if generation == self.active.generation {
            Some(&mut self.active)
        } else {
            self.prior_generations.get_mut(&generation)
        }
    }

    fn apply_requested(
        &mut self,
        generation: u64,
        queue: Queue,
        payload_digest: OutboxDigest,
        dedup_contract: ProviderDedupContract,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation == self.active.generation {
            if queue != self.queue {
                return Err(OutboxLedgerRefusal::QueueChanged);
            }
            if self.active.payload_digest != payload_digest
                || self.active.dedup_contract != dedup_contract
            {
                return Err(OutboxLedgerRefusal::PayloadDiverged);
            }
            return Ok(ApplyStatus::IdempotentDuplicate);
        }

        if self.compensation.is_some() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        if queue != self.queue {
            return Err(OutboxLedgerRefusal::QueueChanged);
        }

        let authorized = self.authorized_reissue == Some(generation);
        if let Some(fenced_at) = self.active_fence() {
            if !authorized || generation <= fenced_at {
                return Err(OutboxLedgerRefusal::Fenced { fenced_at });
            }
        } else if self.active.is_ambiguous_dispatch() {
            let idempotent_retry_ok = self.active.dedup_contract
                == ProviderDedupContract::OperationKeyIdempotent
                && dedup_contract == ProviderDedupContract::OperationKeyIdempotent
                && self.active.payload_digest == payload_digest;
            if !authorized && !idempotent_retry_ok {
                return Err(
                    OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation {
                        generation: self.active.generation,
                    },
                );
            }
        } else if self.active.outcome.is_some_and(|outcome| outcome.ok) {
            return Err(OutboxLedgerRefusal::NotPending);
        }

        let next = GenerationRecord {
            generation,
            queue,
            payload_digest,
            dedup_contract,
            enqueue_commit_seq: commit_seq,
            dispatch: None,
            outcome_unknown: false,
            outcome: None,
            fenced_commit_seq: None,
            reconciliations: Vec::new(),
            consumed_commit_seq: None,
        };
        let previous = std::mem::replace(&mut self.active, next);
        self.prior_generations.insert(previous.generation, previous);
        if authorized {
            self.authorized_reissue = None;
        }
        Ok(ApplyStatus::Applied)
    }

    fn apply_dispatched(
        &mut self,
        generation: u64,
        claim_commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation != self.active.generation {
            return Err(OutboxLedgerRefusal::StaleGeneration);
        }
        if self.queue == Queue::Parked {
            return Err(OutboxLedgerRefusal::ParkedCannotDispatch);
        }
        if self.compensation.is_some() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        if let Some(fenced_at) = self.active_fence() {
            return Err(OutboxLedgerRefusal::Fenced { fenced_at });
        }
        if self.active.dispatch.is_some() {
            return Err(OutboxLedgerRefusal::AlreadyClaimed);
        }
        if self.active.outcome.is_some() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        self.active.dispatch = Some(DispatchProof {
            operation: self.operation,
            generation,
            claim_commit_seq,
        });
        Ok(ApplyStatus::Applied)
    }

    fn apply_active_outcome(
        &mut self,
        generation: u64,
        evidence: OutcomeEvidence,
        ok: bool,
        outcome_digest: OutboxDigest,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation > self.active.generation {
            return Err(OutboxLedgerRefusal::UnknownGeneration);
        }
        if self.compensation.is_some() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        if let Some(fenced_at) = self.active_fence() {
            return Err(OutboxLedgerRefusal::Fenced { fenced_at });
        }
        match evidence {
            OutcomeEvidence::DispatchProof { proof } => {
                self.attach_dispatch_outcome(generation, proof, ok, outcome_digest, commit_seq)
            }
            OutcomeEvidence::Resolved => {
                if self.active.dispatch.is_some() {
                    return Err(OutboxLedgerRefusal::NotPending);
                }
                if let Some(existing) = self.active.outcome {
                    if existing.source == OutcomeSource::Resolved
                        && existing.ok == ok
                        && existing.outcome_digest == outcome_digest
                    {
                        return Ok(ApplyStatus::IdempotentDuplicate);
                    }
                    return Err(OutboxLedgerRefusal::OutcomeDiverged);
                }
                self.active.outcome = Some(RecordedOutcome {
                    source: OutcomeSource::Resolved,
                    ok,
                    outcome_digest,
                    commit_seq,
                });
                Ok(ApplyStatus::Applied)
            }
        }
    }

    fn attach_dispatch_outcome(
        &mut self,
        generation: u64,
        proof: DispatchProof,
        ok: bool,
        outcome_digest: OutboxDigest,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        let operation = self.operation;
        let Some(gen_rec) = self.generation_mut(generation) else {
            return Err(OutboxLedgerRefusal::UnknownGeneration);
        };
        if proof.operation != operation
            || proof.generation != generation
            || gen_rec.dispatch != Some(proof)
        {
            return Err(OutboxLedgerRefusal::InvalidDispatchProof);
        }
        let source = OutcomeSource::DispatchProof {
            claim_commit_seq: proof.claim_commit_seq,
        };
        if let Some(existing) = gen_rec.outcome {
            if existing.source == source
                && existing.ok == ok
                && existing.outcome_digest == outcome_digest
            {
                return Ok(ApplyStatus::IdempotentDuplicate);
            }
            return Err(OutboxLedgerRefusal::OutcomeDiverged);
        }
        gen_rec.outcome = Some(RecordedOutcome {
            source,
            ok,
            outcome_digest,
            commit_seq,
        });
        gen_rec.outcome_unknown = false;
        Ok(ApplyStatus::Applied)
    }

    fn confirm_prior_generation_outcome(
        &mut self,
        generation: u64,
        decision: ReconciliationDecision,
        ok: bool,
        outcome_digest: OutboxDigest,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        let Some(gen_rec) = self.prior_generations.get_mut(&generation) else {
            return Err(OutboxLedgerRefusal::UnknownGeneration);
        };
        if gen_rec.dispatch.is_none() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        if let Some(existing) = gen_rec.outcome {
            if existing.ok != ok || existing.outcome_digest != outcome_digest {
                return Err(OutboxLedgerRefusal::OutcomeDiverged);
            }
            if gen_rec.has_reconciled_outcome() {
                return Ok(ApplyStatus::IdempotentDuplicate);
            }
        } else {
            gen_rec.outcome = Some(RecordedOutcome {
                source: OutcomeSource::Reconciliation,
                ok,
                outcome_digest,
                commit_seq,
            });
            gen_rec.outcome_unknown = false;
        }
        gen_rec.reconciliations.push(ReconciliationEntry {
            decision,
            commit_seq,
        });
        Ok(ApplyStatus::Applied)
    }

    fn apply_prior_crash(&mut self, generation: u64) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        let Some(gen_rec) = self.prior_generations.get_mut(&generation) else {
            return Err(OutboxLedgerRefusal::UnknownGeneration);
        };
        if !gen_rec.is_ambiguous_dispatch() {
            return Err(OutboxLedgerRefusal::StaleGeneration);
        }
        if gen_rec.outcome_unknown {
            return Ok(ApplyStatus::IdempotentDuplicate);
        }
        gen_rec.outcome_unknown = true;
        Ok(ApplyStatus::Applied)
    }

    const fn apply_active_crash(
        &mut self,
        generation: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation != self.active.generation {
            return Err(OutboxLedgerRefusal::UnknownGeneration);
        }
        if self.active.is_ambiguous_dispatch() && !self.active.outcome_unknown {
            self.active.outcome_unknown = true;
            return Ok(ApplyStatus::Applied);
        }
        Ok(ApplyStatus::IdempotentDuplicate)
    }

    fn apply_restore_fenced(
        &mut self,
        generation: u64,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation != self.active.generation {
            return Err(OutboxLedgerRefusal::StaleGeneration);
        }
        if self.compensation.is_some() {
            return Ok(ApplyStatus::IdempotentDuplicate);
        }
        self.fence = Some(
            self.fence
                .map_or(generation, |existing| existing.max(generation)),
        );
        if self
            .authorized_reissue
            .is_some_and(|authorized| authorized <= generation)
        {
            self.authorized_reissue = None;
        }
        self.active.fenced_commit_seq = Some(commit_seq);
        Ok(ApplyStatus::Applied)
    }

    fn apply_reconciliation(
        &mut self,
        generation: u64,
        decision: ReconciliationDecision,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation != self.active.generation {
            return Err(OutboxLedgerRefusal::StaleGeneration);
        }
        if self.compensation.is_some() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        let active_fence = self.active_fence();
        if active_fence.is_none() && !self.active.is_ambiguous_dispatch() {
            return Err(OutboxLedgerRefusal::NotPending);
        }

        match decision {
            ReconciliationDecision::ConfirmedOutcome { ok, outcome_digest } => {
                if let Some(existing) = self.active.outcome {
                    if existing.ok != ok || existing.outcome_digest != outcome_digest {
                        return Err(OutboxLedgerRefusal::OutcomeDiverged);
                    }
                    if self.active.has_reconciled_outcome() {
                        return Ok(ApplyStatus::IdempotentDuplicate);
                    }
                } else {
                    self.active.outcome = Some(RecordedOutcome {
                        source: OutcomeSource::Reconciliation,
                        ok,
                        outcome_digest,
                        commit_seq,
                    });
                    self.active.outcome_unknown = false;
                }
                self.active.reconciliations.push(ReconciliationEntry {
                    decision,
                    commit_seq,
                });
                Ok(ApplyStatus::Applied)
            }
            ReconciliationDecision::AuthorizeReissue { next_generation } => {
                if next_generation <= self.active.generation {
                    return Err(OutboxLedgerRefusal::StaleGeneration);
                }
                if let Some(fenced_at) = active_fence
                    && next_generation <= fenced_at
                {
                    return Err(OutboxLedgerRefusal::Fenced { fenced_at });
                }
                self.authorized_reissue = Some(next_generation);
                self.active.reconciliations.push(ReconciliationEntry {
                    decision,
                    commit_seq,
                });
                Ok(ApplyStatus::Applied)
            }
            ReconciliationDecision::RequireCompensation => {
                self.compensation = Some(CompensationStatus::Pending {
                    for_generation: generation,
                    attempts: 0,
                });
                self.active.reconciliations.push(ReconciliationEntry {
                    decision,
                    commit_seq,
                });
                Ok(ApplyStatus::Applied)
            }
        }
    }

    fn apply_compensation(
        &mut self,
        generation: u64,
        ok: bool,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        match self.compensation {
            Some(CompensationStatus::Compensated { .. }) => Err(OutboxLedgerRefusal::Terminal),
            Some(CompensationStatus::Pending {
                for_generation,
                attempts,
            }) => {
                if generation != for_generation {
                    return Err(OutboxLedgerRefusal::StaleGeneration);
                }
                self.compensation = Some(if ok {
                    CompensationStatus::Compensated {
                        for_generation,
                        commit_seq,
                    }
                } else {
                    CompensationStatus::Pending {
                        for_generation,
                        attempts: attempts.saturating_add(1),
                    }
                });
                Ok(ApplyStatus::Applied)
            }
            None => {
                if generation != self.active.generation || self.active_fence().is_none() {
                    return Err(OutboxLedgerRefusal::NotPending);
                }
                self.compensation = Some(if ok {
                    CompensationStatus::Compensated {
                        for_generation: generation,
                        commit_seq,
                    }
                } else {
                    CompensationStatus::Pending {
                        for_generation: generation,
                        attempts: 1,
                    }
                });
                Ok(ApplyStatus::Applied)
            }
        }
    }

    fn apply_consumed(
        &mut self,
        generation: u64,
        commit_seq: u64,
    ) -> Result<ApplyStatus, OutboxLedgerRefusal> {
        if generation != self.active.generation {
            return Err(OutboxLedgerRefusal::StaleGeneration);
        }
        if self.compensation.is_some() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        if self.active.outcome.is_none() {
            return Err(OutboxLedgerRefusal::NotPending);
        }
        if let Some(fenced_at) = self.active_fence()
            && !self.active.has_reconciled_outcome()
        {
            return Err(OutboxLedgerRefusal::Fenced { fenced_at });
        }
        self.active.consumed_commit_seq = Some(commit_seq);
        Ok(ApplyStatus::Applied)
    }
}
