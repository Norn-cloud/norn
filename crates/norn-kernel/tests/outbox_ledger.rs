//! Pure reducer and replay tests for [`norn_kernel::world::outbox_ledger`].

use std::collections::{BTreeSet, VecDeque};

use norn_ir::cell_id::CellId;
use norn_kernel::CommandId;
use norn_kernel::world::outbox::{OperationKey, OutboxState, Queue};
use norn_kernel::world::outbox_ledger::{
    ApplyStatus, CompensationStatus, DispatchProof, HeadProjection, OperationLedger, OutboxDigest,
    OutboxLedgerRefusal, OutboxRecord, OutboxRecordKind, OutcomeEvidence, OutcomeSource,
    ProviderDedupContract, ReconciliationDecision, RecordedOutcome, ReissueAuthorization,
};

#[allow(clippy::unwrap_used, reason = "constant valid test fixture key")]
fn op_key(name: &str) -> OperationKey {
    let cell = CellId::try_from("0123456789abcdef0123456789abcdef".to_owned()).unwrap();
    OperationKey::derive(&cell, &CommandId(name.to_owned())).unwrap()
}

const fn digest(byte: u8) -> OutboxDigest {
    OutboxDigest::from_bytes([byte; 32])
}

const fn req(
    operation: OperationKey,
    generation: u64,
    queue: Queue,
    payload_byte: u8,
    dedup_contract: ProviderDedupContract,
    commit_seq: u64,
) -> OutboxRecord {
    OutboxRecord {
        operation,
        generation,
        kind: OutboxRecordKind::Requested {
            queue,
            payload_digest: digest(payload_byte),
            dedup_contract,
            commit_seq,
        },
    }
}

const fn dispatched(
    operation: OperationKey,
    generation: u64,
    claim_commit_seq: u64,
) -> OutboxRecord {
    OutboxRecord {
        operation,
        generation,
        kind: OutboxRecordKind::Dispatched { claim_commit_seq },
    }
}

const fn dispatch_outcome(
    operation: OperationKey,
    generation: u64,
    proof: DispatchProof,
    ok: bool,
    digest_byte: u8,
    commit_seq: u64,
) -> OutboxRecord {
    OutboxRecord {
        operation,
        generation,
        kind: OutboxRecordKind::Outcome {
            evidence: OutcomeEvidence::DispatchProof { proof },
            ok,
            outcome_digest: digest(digest_byte),
            commit_seq,
        },
    }
}

const fn reconciled(
    operation: OperationKey,
    generation: u64,
    decision: ReconciliationDecision,
    commit_seq: u64,
) -> OutboxRecord {
    OutboxRecord {
        operation,
        generation,
        kind: OutboxRecordKind::Reconciliation {
            decision,
            commit_seq,
        },
    }
}

const fn consumed(operation: OperationKey, generation: u64, commit_seq: u64) -> OutboxRecord {
    OutboxRecord {
        operation,
        generation,
        kind: OutboxRecordKind::Consumed { commit_seq },
    }
}

#[test]
fn old_result_never_settles_new_generation_and_attaches_to_original_via_dispatch_proof() {
    let op = op_key("pay/1");
    let log = vec![
        req(op, 1, Queue::Fifo, 0xaa, ProviderDedupContract::None, 10),
        dispatched(op, 1, 11),
        OutboxRecord {
            operation: op,
            generation: 1,
            kind: OutboxRecordKind::CrashObserved,
        },
        reconciled(
            op,
            1,
            ReconciliationDecision::AuthorizeReissue { next_generation: 2 },
            12,
        ),
        req(op, 2, Queue::Fifo, 0xaa, ProviderDedupContract::None, 13),
    ];
    let mut ledger = OperationLedger::replay(&log).unwrap();
    let head_before = ledger.head_projection();
    assert_eq!(head_before.generation, 2);
    assert_eq!(head_before.state, OutboxState::Requested);
    assert_eq!(head_before.outcome, None);
    assert_eq!(ledger.prior_confirmed_successes(), Vec::<u64>::new());

    let gen1_proof = DispatchProof {
        operation: op,
        generation: 1,
        claim_commit_seq: 11,
    };
    let forged_proof = DispatchProof {
        operation: op,
        generation: 1,
        claim_commit_seq: 999,
    };
    let snapshot = ledger.clone();
    assert_eq!(
        ledger.apply(&dispatch_outcome(op, 2, gen1_proof, true, 0x11, 14)),
        Err(OutboxLedgerRefusal::InvalidDispatchProof)
    );
    assert_eq!(
        ledger.apply(&dispatch_outcome(op, 1, forged_proof, true, 0x11, 14)),
        Err(OutboxLedgerRefusal::InvalidDispatchProof)
    );
    assert_eq!(ledger, snapshot);

    // Valid late outcome for gen 1 attaches strictly to gen 1, leaving gen 2
    // and the entire HeadProjection completely unchanged.
    let late_gen1_outcome = dispatch_outcome(op, 1, gen1_proof, true, 0x11, 14);
    assert_eq!(ledger.apply(&late_gen1_outcome), Ok(ApplyStatus::Applied));
    assert_eq!(
        ledger.generation(1).unwrap().outcome,
        Some(RecordedOutcome {
            source: OutcomeSource::DispatchProof {
                claim_commit_seq: 11,
            },
            ok: true,
            outcome_digest: digest(0x11),
            commit_seq: 14,
        })
    );
    assert_eq!(ledger.head_projection(), head_before);
    assert_eq!(ledger.prior_confirmed_successes(), vec![1]);
    assert_eq!(
        ledger.apply(&consumed(op, 1, 15)),
        Err(OutboxLedgerRefusal::StaleGeneration)
    );

    let mut full_log = log;
    full_log.push(late_gen1_outcome);
    assert_eq!(OperationLedger::replay(&full_log).unwrap(), ledger);
}

#[test]
fn same_generation_payload_divergence_fails_closed_and_identical_replay_is_idempotent() {
    let op = op_key("order/42");
    let mut ledger = OperationLedger::start(&req(
        op,
        1,
        Queue::Fifo,
        0x01,
        ProviderDedupContract::None,
        1,
    ))
    .unwrap();
    assert_eq!(
        ledger.apply(&req(
            op,
            1,
            Queue::Fifo,
            0x01,
            ProviderDedupContract::None,
            2
        )),
        Ok(ApplyStatus::IdempotentDuplicate)
    );

    let before = ledger.clone();
    assert_eq!(
        ledger.apply(&req(
            op,
            1,
            Queue::Fifo,
            0x02,
            ProviderDedupContract::None,
            3
        )),
        Err(OutboxLedgerRefusal::PayloadDiverged)
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            1,
            Queue::Fifo,
            0x01,
            ProviderDedupContract::OperationKeyIdempotent,
            4,
        )),
        Err(OutboxLedgerRefusal::PayloadDiverged)
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            1,
            Queue::Parked,
            0x01,
            ProviderDedupContract::None,
            5
        )),
        Err(OutboxLedgerRefusal::QueueChanged)
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Parked,
            0x01,
            ProviderDedupContract::None,
            6
        )),
        Err(OutboxLedgerRefusal::QueueChanged)
    );
    assert_eq!(ledger, before);
}

#[test]
fn ambiguous_dispatch_without_dedup_contract_requires_durable_reconciliation() {
    let op = op_key("wire/7");
    let mut ledger = OperationLedger::replay(&[
        req(op, 1, Queue::Fifo, 0xaa, ProviderDedupContract::None, 1),
        dispatched(op, 1, 2),
    ])
    .unwrap();
    let before = ledger.clone();

    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::None,
            3
        )),
        Err(OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation { generation: 1 })
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            3,
        )),
        Err(OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation { generation: 1 })
    );
    assert_eq!(ledger, before);

    assert_eq!(
        ledger.apply(&OutboxRecord {
            operation: op,
            generation: 1,
            kind: OutboxRecordKind::CrashObserved,
        }),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(
        ledger.head_projection().state,
        OutboxState::OutcomeUnknown { generation: 1 }
    );
    assert_eq!(
        ledger.apply(&reconciled(
            op,
            1,
            ReconciliationDecision::AuthorizeReissue { next_generation: 2 },
            5,
        )),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0xbb,
            ProviderDedupContract::None,
            6
        )),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(ledger.active_generation(), 2);
}

#[test]
fn ambiguous_dispatch_with_idempotent_dedup_contract_requires_matching_payload() {
    let op = op_key("wire/8");
    let mut ledger = OperationLedger::replay(&[
        req(
            op,
            1,
            Queue::Fifo,
            0xcc,
            ProviderDedupContract::OperationKeyIdempotent,
            10,
        ),
        dispatched(op, 1, 11),
        OutboxRecord {
            operation: op,
            generation: 1,
            kind: OutboxRecordKind::CrashObserved,
        },
    ])
    .unwrap();

    // Payload change under OperationKeyIdempotent still requires reconciliation.
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0xdd,
            ProviderDedupContract::OperationKeyIdempotent,
            12,
        )),
        Err(OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation { generation: 1 })
    );
    // Identical payload reissues automatically while preserving gen 1's dispatch proof.
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0xcc,
            ProviderDedupContract::OperationKeyIdempotent,
            12,
        )),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(ledger.active_generation(), 2);
    assert!(ledger.generation(1).unwrap().dispatch.is_some());
}

#[test]
fn restore_fence_blocks_idempotent_reissue_and_pre_restore_consumption_until_reconciled() {
    let op = op_key("restore/9");
    let proof = DispatchProof {
        operation: op,
        generation: 1,
        claim_commit_seq: 2,
    };
    let mut ledger = OperationLedger::replay(&[
        req(
            op,
            1,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            1,
        ),
        dispatched(op, 1, 2),
        dispatch_outcome(op, 1, proof, true, 0xfe, 3),
        OutboxRecord {
            operation: op,
            generation: 1,
            kind: OutboxRecordKind::RestoreFenced { commit_seq: 4 },
        },
    ])
    .unwrap();

    assert_eq!(
        ledger.head_projection().state,
        OutboxState::ReconciliationRequired { fenced_at: 1 }
    );
    assert_eq!(ledger.head_projection().outcome, None);
    assert_eq!(
        ledger.apply(&dispatched(op, 1, 5)),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
    assert_eq!(
        ledger.apply(&consumed(op, 1, 5)),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            5,
        )),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
    assert_eq!(
        ledger.apply(&reconciled(
            op,
            1,
            ReconciliationDecision::ConfirmedOutcome {
                ok: false,
                outcome_digest: digest(0x00),
            },
            6,
        )),
        Err(OutboxLedgerRefusal::OutcomeDiverged)
    );
    assert_eq!(
        ledger.apply(&reconciled(
            op,
            1,
            ReconciliationDecision::ConfirmedOutcome {
                ok: true,
                outcome_digest: digest(0xfe),
            },
            6,
        )),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(
        ledger.active().outcome.unwrap().source,
        OutcomeSource::DispatchProof {
            claim_commit_seq: 2
        }
    );
    assert_eq!(ledger.head_projection().state, OutboxState::Succeeded);
    assert_eq!(ledger.apply(&consumed(op, 1, 7)), Ok(ApplyStatus::Applied));
    assert_eq!(
        ledger.head_projection().state,
        OutboxState::Consumed { commit_seq: 7 }
    );
}

#[test]
fn parked_queue_refuses_dispatch_and_settles_via_resolved() {
    let op = op_key("decision/1");
    let mut parked = OperationLedger::start(&req(
        op,
        0,
        Queue::Parked,
        0x10,
        ProviderDedupContract::None,
        1,
    ))
    .unwrap();
    assert_eq!(
        parked.apply(&dispatched(op, 0, 2)),
        Err(OutboxLedgerRefusal::ParkedCannotDispatch)
    );
    assert_eq!(
        parked.apply(&OutboxRecord {
            operation: op,
            generation: 0,
            kind: OutboxRecordKind::Outcome {
                evidence: OutcomeEvidence::Resolved,
                ok: true,
                outcome_digest: digest(0x20),
                commit_seq: 3,
            },
        }),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(parked.apply(&consumed(op, 0, 4)), Ok(ApplyStatus::Applied));
    assert!(parked.is_terminal());
}

#[test]
fn late_dispatch_evidence_attaches_to_prior_generation_even_after_active_consumed() {
    let op = op_key("late-after-consumed/1");
    let gen1_proof = DispatchProof {
        operation: op,
        generation: 1,
        claim_commit_seq: 2,
    };
    let gen2_proof = DispatchProof {
        operation: op,
        generation: 2,
        claim_commit_seq: 4,
    };
    let mut fifo = OperationLedger::replay(&[
        req(
            op,
            1,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            1,
        ),
        dispatched(op, 1, 2),
        req(
            op,
            2,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            3,
        ),
        dispatched(op, 2, 4),
        dispatch_outcome(op, 2, gen2_proof, true, 0xbb, 5),
        consumed(op, 2, 6),
    ])
    .unwrap();
    assert!(fifo.is_terminal());

    assert_eq!(
        fifo.apply(&dispatch_outcome(op, 1, gen1_proof, true, 0xbb, 7)),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(
        fifo.head_projection().state,
        OutboxState::Consumed { commit_seq: 6 }
    );
    assert_eq!(fifo.prior_confirmed_successes(), vec![1]);
    assert_eq!(
        fifo.apply(&dispatch_outcome(op, 1, gen1_proof, false, 0xcc, 8)),
        Err(OutboxLedgerRefusal::OutcomeDiverged)
    );
}

fn bfs_candidates(op: OperationKey) -> Vec<OutboxRecord> {
    let mut records = Vec::new();
    for gen_id in 0..=2_u64 {
        for payload in [0x01_u8, 0x02_u8] {
            for dedup in [
                ProviderDedupContract::None,
                ProviderDedupContract::OperationKeyIdempotent,
            ] {
                records.push(req(op, gen_id, Queue::Fifo, payload, dedup, 10));
            }
        }
        let claim_seq = gen_id.saturating_add(100);
        let proof = DispatchProof {
            operation: op,
            generation: gen_id,
            claim_commit_seq: claim_seq,
        };
        records.push(dispatched(op, gen_id, claim_seq));
        for ok in [false, true] {
            let outcome_byte = if ok { 0xa1 } else { 0xa2 };
            records.push(dispatch_outcome(op, gen_id, proof, ok, outcome_byte, 20));
            records.push(reconciled(
                op,
                gen_id,
                ReconciliationDecision::ConfirmedOutcome {
                    ok,
                    outcome_digest: digest(outcome_byte),
                },
                30,
            ));
            records.push(OutboxRecord {
                operation: op,
                generation: gen_id,
                kind: OutboxRecordKind::Compensation { ok, commit_seq: 40 },
            });
        }
        records.push(OutboxRecord {
            operation: op,
            generation: gen_id,
            kind: OutboxRecordKind::CrashObserved,
        });
        records.push(OutboxRecord {
            operation: op,
            generation: gen_id,
            kind: OutboxRecordKind::RestoreFenced { commit_seq: 25 },
        });
        if gen_id < 2 {
            records.push(reconciled(
                op,
                gen_id,
                ReconciliationDecision::AuthorizeReissue {
                    next_generation: gen_id.saturating_add(1),
                },
                35,
            ));
        }
        records.push(reconciled(
            op,
            gen_id,
            ReconciliationDecision::RequireCompensation,
            36,
        ));
        records.push(consumed(op, gen_id, 50));
    }
    records
}

fn assert_step_invariants(
    current: &OperationLedger,
    next: &OperationLedger,
    record: &OutboxRecord,
) {
    assert!(next.active_generation() >= current.active_generation());
    for g in 0..=2_u64 {
        if let Some(old_rec) = current.generation(g) {
            let new_rec = next.generation(g);
            assert!(new_rec.is_some(), "generations never vanish");
            if let Some(new_rec) = new_rec {
                assert_eq!(new_rec.payload_digest, old_rec.payload_digest);
                assert_eq!(new_rec.dedup_contract, old_rec.dedup_contract);
                if let Some(proof) = old_rec.dispatch {
                    assert_eq!(new_rec.dispatch, Some(proof));
                }
                if let Some(outcome) = old_rec.outcome {
                    assert_eq!(new_rec.outcome, Some(outcome));
                }
                if let Some(seq) = old_rec.consumed_commit_seq {
                    assert_eq!(new_rec.consumed_commit_seq, Some(seq));
                }
            }
        }
    }
    if matches!(record.kind, OutboxRecordKind::Dispatched { .. }) {
        assert!(current.fence().is_none_or(|f| record.generation > f));
        assert!(
            current
                .generation(record.generation)
                .is_some_and(|rec| rec.dispatch.is_none())
        );
    }
    if matches!(record.kind, OutboxRecordKind::Requested { .. })
        && record.generation > current.active_generation()
        && current.active().is_ambiguous_dispatch()
        && current.active().dedup_contract == ProviderDedupContract::None
    {
        assert_eq!(
            current.authorized_reissue(),
            Some(ReissueAuthorization {
                from_generation: current.active_generation(),
                next_generation: record.generation,
            })
        );
    }
}

#[test]
fn restore_fence_invalidates_pre_restore_confirmation_and_reissue_authorization() {
    let op = op_key("restore-invalidation/1");
    let mut confirmed = OperationLedger::replay(&[
        req(op, 1, Queue::Fifo, 0xaa, ProviderDedupContract::None, 1),
        dispatched(op, 1, 2),
        reconciled(
            op,
            1,
            ReconciliationDecision::ConfirmedOutcome {
                ok: true,
                outcome_digest: digest(0x11),
            },
            3,
        ),
        OutboxRecord {
            operation: op,
            generation: 1,
            kind: OutboxRecordKind::RestoreFenced { commit_seq: 4 },
        },
    ])
    .unwrap();
    // Pre-restore ConfirmedOutcome does not satisfy post-restore consumption,
    // and replaying the pre-restore sequence-3 confirmation is also refused.
    assert_eq!(
        confirmed.head_projection().state,
        OutboxState::ReconciliationRequired { fenced_at: 1 }
    );
    assert_eq!(
        confirmed.apply(&consumed(op, 1, 5)),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
    assert_eq!(
        confirmed.apply(&reconciled(
            op,
            1,
            ReconciliationDecision::ConfirmedOutcome {
                ok: true,
                outcome_digest: digest(0x11),
            },
            3,
        )),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
    assert_eq!(
        confirmed.apply(&reconciled(
            op,
            1,
            ReconciliationDecision::ConfirmedOutcome {
                ok: true,
                outcome_digest: digest(0x11),
            },
            6,
        )),
        Ok(ApplyStatus::Applied)
    );
    assert_eq!(
        confirmed.apply(&consumed(op, 1, 7)),
        Ok(ApplyStatus::Applied)
    );
}

#[test]
fn restore_fence_invalidates_and_rejects_pre_restore_reissue_authorization() {
    let op = op_key("restore-invalidation-auth/1");
    let mut auth = OperationLedger::replay(&[
        req(op, 1, Queue::Fifo, 0xaa, ProviderDedupContract::None, 1),
        dispatched(op, 1, 2),
        reconciled(
            op,
            1,
            ReconciliationDecision::AuthorizeReissue { next_generation: 3 },
            3,
        ),
        OutboxRecord {
            operation: op,
            generation: 1,
            kind: OutboxRecordKind::RestoreFenced { commit_seq: 4 },
        },
    ])
    .unwrap();
    assert_eq!(auth.authorized_reissue(), None);
    assert_eq!(
        auth.apply(&reconciled(
            op,
            1,
            ReconciliationDecision::AuthorizeReissue { next_generation: 3 },
            3,
        )),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
    assert_eq!(
        auth.apply(&req(
            op,
            3,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::None,
            5
        )),
        Err(OutboxLedgerRefusal::Fenced { fenced_at: 1 })
    );
}

#[test]
fn reissue_authorization_is_invalidated_when_head_advances_to_another_generation() {
    let op = op_key("auth-bound/1");
    let mut ledger = OperationLedger::replay(&[
        req(
            op,
            1,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            1,
        ),
        dispatched(op, 1, 2),
        reconciled(
            op,
            1,
            ReconciliationDecision::AuthorizeReissue { next_generation: 3 },
            3,
        ),
        // Advance to gen 2 through the idempotent dedup path; gen 1's
        // authorization for gen 3 must be invalidated.
        req(
            op,
            2,
            Queue::Fifo,
            0xaa,
            ProviderDedupContract::OperationKeyIdempotent,
            4,
        ),
        dispatched(op, 2, 5),
    ])
    .unwrap();
    assert_eq!(ledger.authorized_reissue(), None);
    assert_eq!(
        ledger.apply(&req(
            op,
            3,
            Queue::Fifo,
            0xbb,
            ProviderDedupContract::None,
            6,
        )),
        Err(OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation { generation: 2 })
    );
}

#[test]
fn same_generation_divergence_fails_closed_even_after_supersession_or_consumption() {
    let op = op_key("diverge-history/1");
    let mut ledger = OperationLedger::replay(&[
        req(op, 1, Queue::Fifo, 0x10, ProviderDedupContract::None, 1),
        req(op, 2, Queue::Fifo, 0x20, ProviderDedupContract::None, 2),
        dispatched(op, 2, 3),
        dispatch_outcome(
            op,
            2,
            DispatchProof {
                operation: op,
                generation: 2,
                claim_commit_seq: 3,
            },
            true,
            0x99,
            4,
        ),
        consumed(op, 2, 5),
    ])
    .unwrap();
    let before = ledger.clone();
    // Superseded gen 1: identical replay is idempotent, divergent payload fails closed.
    assert_eq!(
        ledger.apply(&req(
            op,
            1,
            Queue::Fifo,
            0x10,
            ProviderDedupContract::None,
            6
        )),
        Ok(ApplyStatus::IdempotentDuplicate)
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            1,
            Queue::Fifo,
            0x11,
            ProviderDedupContract::None,
            6
        )),
        Err(OutboxLedgerRefusal::PayloadDiverged)
    );
    // Consumed gen 2: identical replay is idempotent, divergent payload fails closed.
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0x20,
            ProviderDedupContract::None,
            7
        )),
        Ok(ApplyStatus::IdempotentDuplicate)
    );
    assert_eq!(
        ledger.apply(&req(
            op,
            2,
            Queue::Fifo,
            0x21,
            ProviderDedupContract::None,
            7
        )),
        Err(OutboxLedgerRefusal::PayloadDiverged)
    );
    // A brand-new generation after terminal Consumed still refuses Terminal.
    assert_eq!(
        ledger.apply(&req(
            op,
            3,
            Queue::Fifo,
            0x30,
            ProviderDedupContract::None,
            8
        )),
        Err(OutboxLedgerRefusal::Terminal)
    );
    assert_eq!(ledger, before);
}

/// Bounded BFS over reachable `OperationLedger` states across generations
/// 0..=2 verifying monotonicity, write-once immutability, fence enforcement,
/// and ambiguous-dispatch reconciliation gating.
#[test]
fn bounded_bfs_preserves_all_immutable_generation_invariants() {
    let op = op_key("bfs/1");
    let start = OperationLedger::start(&req(
        op,
        0,
        Queue::Fifo,
        0x01,
        ProviderDedupContract::None,
        1,
    ))
    .unwrap();
    let candidates = bfs_candidates(op);
    let key_of = |ledger: &OperationLedger| serde_json::to_string(ledger).unwrap();
    let mut seen: BTreeSet<String> = BTreeSet::from([key_of(&start)]);
    let mut queue: VecDeque<OperationLedger> = VecDeque::from([start]);
    let mut explored = 0_usize;

    while let Some(current) = queue.pop_front() {
        explored = explored.saturating_add(1);
        for record in &candidates {
            let mut next = current.clone();
            match next.apply(record) {
                Err(_) | Ok(ApplyStatus::IdempotentDuplicate) => assert_eq!(next, current),
                Ok(ApplyStatus::Applied) => {
                    assert_step_invariants(&current, &next, record);
                    let under_cap = !matches!(
                        next.compensation(),
                        Some(CompensationStatus::Pending { attempts, .. }) if attempts > 2
                    );
                    if under_cap && seen.len() < 400 && seen.insert(key_of(&next)) {
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    assert!(
        explored >= 50,
        "expected >= 50 explored states, got {explored}"
    );
}

#[test]
fn json_roundtrip_preserves_records_ledger_and_head_projection() {
    let op = op_key("serde/1");
    let records = vec![
        req(
            op,
            1,
            Queue::Fifo,
            0x5a,
            ProviderDedupContract::OperationKeyIdempotent,
            10,
        ),
        dispatched(op, 1, 11),
    ];
    let encoded_records = serde_json::to_string(&records).unwrap();
    let decoded_records: Vec<OutboxRecord> = serde_json::from_str(&encoded_records).unwrap();
    assert_eq!(decoded_records, records);

    let ledger = OperationLedger::replay(&decoded_records).unwrap();
    let ledger_replayed = OperationLedger::replay(&records).unwrap();
    assert_eq!(ledger, ledger_replayed);
    assert!(serde_json::to_string(&ledger).is_ok());

    let head = ledger.head_projection();
    let encoded_head = serde_json::to_string(&head).unwrap();
    let decoded_head: HeadProjection = serde_json::from_str(&encoded_head).unwrap();
    assert_eq!(decoded_head, head);

    assert!(serde_json::from_str::<OutboxDigest>("\"not-hex\"").is_err());
    assert!(serde_json::from_str::<OperationKey>(&format!("\"{}\"", "A".repeat(64))).is_err());
}
