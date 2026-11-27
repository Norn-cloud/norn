//! D1 export/restore regression tests: restore fences, second-restore replay of
//! reconciled/compensated operations, and export/restore preservation of late
//! dispatch-proof evidence.

use std::collections::BTreeMap;

use norn_cell::export::Exporter;
use norn_cell::outbox_ledger::outcome_digest;
use norn_cell::{CellError, CellStore, ClaimOutcome, JournalMode, StoreOptions};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::world::outbox::OutboxState;
use norn_kernel::world::outbox_ledger::{OutboxLedgerRefusal, ReconciliationDecision};
use norn_kernel::{CellCommit, CellHost as _, Command, CommitError, Consumed, Generation};
use serde_json::json;

use super::outbox_generations::{
    Cell, SCOPE, TestResult, enqueue_cmd, make_command, open_cell, rv, test_model,
};

async fn export_cell(
    cell: &Cell,
    export_dir: &std::path::Path,
) -> Result<(Exporter, BTreeMap<String, String>), Box<dyn std::error::Error>> {
    let cell_id = String::from(
        cell.store()
            .cell_id()?
            .ok_or("source cell missing cell_id")?,
    );
    let mut exporter = Exporter::local(export_dir)?;
    let count = cell.store().export_commit_count()?;
    for seq in 1..=count {
        if let Some(record) = cell.store().export_record(seq)? {
            exporter.append(&record).await?;
        }
    }
    let manifest = exporter.verify().await?;
    let bindings = BTreeMap::from([
        ("scope".to_owned(), SCOPE.to_owned()),
        (
            "cell_identity_format".to_owned(),
            norn_cell::cell_identity::FORMAT.to_owned(),
        ),
        ("cell_id".to_owned(), cell_id),
    ]);
    assert_eq!(manifest.bindings, bindings);
    Ok((exporter, bindings))
}

async fn export_claimed_pair(
    dir: &std::path::Path,
) -> Result<
    (
        Exporter,
        BTreeMap<String, String>,
        Command<JsonObject>,
        Command<JsonObject>,
    ),
    Box<dyn std::error::Error>,
> {
    let src_path = dir.join("src.sqlite");
    let mut src = open_cell(&src_path)?;
    let c1 = make_command("fence-1", 0, 100, Some("operationKeyIdempotent"))?;
    let c2 = make_command("after-1", 0, 200, None)?;
    enqueue_cmd(&mut src, c1.clone())?;
    enqueue_cmd(&mut src, c2.clone())?;
    assert_eq!(src.claim_perform()?, ClaimOutcome::Fresh(c1.clone()));
    let (exporter, bindings) = export_cell(&src, &dir.join("export")).await?;
    Ok((exporter, bindings, c1, c2))
}

#[tokio::test]
async fn restore_fence_blocks_idempotent_reissue_and_confirmed_outcome_unmasks_head() -> TestResult
{
    let dir = tempfile::tempdir()?;
    let (mut exporter, bindings, c1, _c2) = export_claimed_pair(dir.path()).await?;
    let target = dir.path().join("restored-confirmed.sqlite");

    let report = CellStore::restore_from(
        &mut exporter,
        &target,
        test_model()?,
        SCOPE,
        &bindings,
        StoreOptions {
            journal_mode: JournalMode::Delete,
            ..StoreOptions::default()
        },
    )
    .await?;
    assert_eq!(report.fenced_commands, 2);

    let mut cell = open_cell(&target)?;
    assert_eq!(
        cell.claim_perform()?,
        ClaimOutcome::HeadFenced {
            command: c1.id.clone(),
            fenced_at: Generation(0),
        }
    );
    // Even `OperationKeyIdempotent` cannot auto-reissue while restore-fenced.
    let blocked = enqueue_cmd(
        &mut cell,
        make_command("fence-1", 1, 100, Some("operationKeyIdempotent"))?,
    );
    assert!(matches!(
        blocked,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 1,
            ..
        }))
    ));
    let confirmed_val = rv(json!({ "ok": true, "reconciled": "provider-lookup" }))?;
    let wrong_digest = outcome_digest(&rv(json!({ "ok": true, "reconciled": "other" }))?)?;
    // Mismatched confirmed_outcome payload vs digest fails closed.
    let err = cell.reconcile_outbox(
        &c1.id,
        Generation(0),
        ReconciliationDecision::ConfirmedOutcome {
            ok: true,
            outcome_digest: wrong_digest,
        },
        Some(&confirmed_val),
    );
    assert!(matches!(
        err,
        Err(CellError::OutboxLedgerRefused {
            refusal: OutboxLedgerRefusal::OutcomeDiverged,
            ..
        })
    ));

    let good_digest = outcome_digest(&confirmed_val)?;
    cell.reconcile_outbox(
        &c1.id,
        Generation(0),
        ReconciliationDecision::ConfirmedOutcome {
            ok: true,
            outcome_digest: good_digest,
        },
        Some(&confirmed_val),
    )?;
    assert!(matches!(
        cell.claim_perform()?,
        ClaimOutcome::Recorded { ref command, ref performed }
            if command == &c1 && performed.input == confirmed_val
    ));
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: c1.id,
        generation: Generation(0),
    }))?;
    Ok(())
}

#[tokio::test]
async fn restore_fence_require_compensation_completes_terminal_and_unblocks_fifo() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (mut exporter, bindings, c1, c2) = export_claimed_pair(dir.path()).await?;
    let target = dir.path().join("restored-compensated.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        test_model()?,
        SCOPE,
        &bindings,
        StoreOptions::default(),
    )
    .await?;
    let mut comp_cell = open_cell(&target)?;
    comp_cell.reconcile_outbox(
        &c1.id,
        Generation(0),
        ReconciliationDecision::RequireCompensation,
        None,
    )?;
    comp_cell.record_outbox_compensation(&c1.id, Generation(0), false)?;
    let mid = comp_cell
        .store()
        .read()
        .outbox_ledger("fence-1")?
        .ok_or("missing mid ledger")?;
    assert_eq!(
        mid.head_projection().state,
        OutboxState::CompensationPending { attempts: 1 }
    );
    comp_cell.record_outbox_compensation(&c1.id, Generation(0), true)?;
    let done = comp_cell
        .store()
        .read()
        .outbox_ledger("fence-1")?
        .ok_or("missing compensated ledger")?;
    assert_eq!(done.head_projection().state, OutboxState::Compensated);
    assert!(done.is_terminal());

    // Compensated operation refuses any subsequent reissue even at gen 1.
    let err = enqueue_cmd(&mut comp_cell, make_command("fence-1", 1, 100, None)?);
    assert!(matches!(
        err,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 1,
            ..
        }))
    ));
    // FIFO head advances to `after-1` (still restore-fenced at gen 0).
    assert_eq!(
        comp_cell.claim_perform()?,
        ClaimOutcome::HeadFenced {
            command: c2.id,
            fenced_at: Generation(0),
        }
    );
    Ok(())
}

#[tokio::test]
async fn second_restore_replays_confirmed_outcome_and_compensation() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (mut exp1, bindings, c1, c2) = export_claimed_pair(dir.path()).await?;
    let first_path = dir.path().join("r1.sqlite");
    CellStore::restore_from(
        &mut exp1,
        &first_path,
        test_model()?,
        SCOPE,
        &bindings,
        StoreOptions::default(),
    )
    .await?;
    let mut cell = open_cell(&first_path)?;
    // ConfirmedOutcome with `None` payload fails closed when SQL result is NULL.
    let confirmed_val = rv(json!({ "ok": true, "via": "lookup" }))?;
    let digest = outcome_digest(&confirmed_val)?;
    assert!(matches!(
        cell.reconcile_outbox(
            &c1.id,
            Generation(0),
            ReconciliationDecision::ConfirmedOutcome {
                ok: true,
                outcome_digest: digest,
            },
            None,
        ),
        Err(CellError::OutboxLedgerRefused {
            refusal: OutboxLedgerRefusal::NotPending,
            ..
        })
    ));
    // Reconcile c1 with ConfirmedOutcome + consume; reconcile c2 with RequireCompensation + complete.
    cell.reconcile_outbox(
        &c1.id,
        Generation(0),
        ReconciliationDecision::ConfirmedOutcome {
            ok: true,
            outcome_digest: digest,
        },
        Some(&confirmed_val),
    )?;
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: c1.id.clone(),
        generation: Generation(0),
    }))?;
    cell.reconcile_outbox(
        &c2.id,
        Generation(0),
        ReconciliationDecision::RequireCompensation,
        None,
    )?;
    cell.record_outbox_compensation(&c2.id, Generation(0), true)?;

    let (mut exp2, bindings2) = export_cell(&cell, &dir.path().join("export2")).await?;
    let second_path = dir.path().join("r2.sqlite");
    let report = CellStore::restore_from(
        &mut exp2,
        &second_path,
        test_model()?,
        SCOPE,
        &bindings2,
        StoreOptions::default(),
    )
    .await?;
    assert_eq!(report.fenced_commands, 0);
    let restored = open_cell(&second_path)?;
    assert!(
        restored
            .store()
            .read()
            .outbox_ledger(&c1.id.0)?
            .ok_or("missing c1")?
            .is_terminal()
    );
    assert_eq!(
        restored
            .store()
            .read()
            .outbox_ledger(&c2.id.0)?
            .ok_or("missing c2")?
            .head_projection()
            .state,
        OutboxState::Compensated
    );
    Ok(())
}

#[tokio::test]
async fn export_restore_preserves_late_outcomes_across_both_paths() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut cell = open_cell(&dir.path().join("late-src.sqlite"))?;
    let a0 = make_command("late-a", 0, 10, None)?;
    let a1 = make_command("late-a", 1, 10, None)?;
    let b0 = make_command("late-b", 0, 20, None)?;
    let b1 = make_command("late-b", 1, 20, None)?;
    enqueue_cmd(&mut cell, a0.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(a0.clone()));
    let proof_a0 = cell
        .store()
        .read()
        .dispatch_proof("late-a", 0)?
        .ok_or("proof_a0")?;
    cell.reconcile_outbox(
        &a0.id,
        Generation(0),
        ReconciliationDecision::AuthorizeReissue { next_generation: 1 },
        None,
    )?;
    enqueue_cmd(&mut cell, a1.clone())?;
    // Path 1: durable `record_late_outcome` before gen 1 is even consumed.
    cell.record_late_outcome(&a0.id, proof_a0, true, &rv(json!({ "ok": true, "a": 0 }))?)?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(a1.clone()));
    assert!(
        cell.complete_perform(&a1, rv(json!({ "ok": true }))?)?
            .is_some()
    );
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: a1.id.clone(),
        generation: Generation(1),
    }))?;

    // Path 2: local `complete_perform` on superseded gen 0 before gen 1 is consumed.
    enqueue_cmd(&mut cell, b0.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(b0.clone()));
    cell.reconcile_outbox(
        &b0.id,
        Generation(0),
        ReconciliationDecision::AuthorizeReissue { next_generation: 1 },
        None,
    )?;
    enqueue_cmd(&mut cell, b1.clone())?;
    assert!(
        cell.complete_perform(&b0, rv(json!({ "ok": true, "b": 0 }))?)?
            .is_none()
    );
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(b1.clone()));
    assert!(
        cell.complete_perform(&b1, rv(json!({ "ok": true }))?)?
            .is_some()
    );
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: b1.id.clone(),
        generation: Generation(1),
    }))?;

    let (mut exp, bindings) = export_cell(&cell, &dir.path().join("late-exp")).await?;
    let restored_path = dir.path().join("late-restored.sqlite");
    CellStore::restore_from(
        &mut exp,
        &restored_path,
        test_model()?,
        SCOPE,
        &bindings,
        StoreOptions::default(),
    )
    .await?;
    let restored = open_cell(&restored_path)?;
    assert_eq!(
        restored
            .store()
            .read()
            .outbox_ledger("late-a")?
            .ok_or("late-a")?
            .prior_confirmed_successes(),
        vec![0]
    );
    assert_eq!(
        restored
            .store()
            .read()
            .outbox_ledger("late-b")?
            .ok_or("late-b")?
            .prior_confirmed_successes(),
        vec![0]
    );
    Ok(())
}
