//! D1: immutable per-generation outbox ledger, durable reconciliation, late
//! dispatch-proof evidence attachment, and fail-closed payload divergence.

use std::collections::BTreeMap;

use norn_cell::engine::{NativeEngine, SqlEngine};
use norn_cell::engine_release::{STAMP_FORMAT, STAMP_KEY};
use norn_cell::export::Exporter;
use norn_cell::outbox_ledger::{outcome_digest, payload_digest};
use norn_cell::{CellError, CellStore, ClaimOutcome, JournalMode, Model, SqliteCell, StoreOptions};
use norn_ir::{ObjectTypeDef, RuntimeValue};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::world::outbox::{OutboxState, Queue};
use norn_kernel::world::outbox_ledger::{
    ApplyStatus, DispatchProof, OperationLedger, OutboxLedgerRefusal, ProviderDedupContract,
    ReconciliationDecision,
};
use norn_kernel::{
    CellCommit, CellHost as _, Command, CommandId, CommitError, Consumed, Effect, FreshnessPolicy,
    Generation, IdempotencyKey, LogicalTime,
};
use proptest::prelude::*;
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Cell = SqliteCell<RuntimeValue, JsonObject, RuntimeValue>;

const SCOPE: &str = "org_d1/dev";

fn test_model() -> Result<Model, Box<dyn std::error::Error>> {
    let defs: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "d1.Order",
            "identity": [{ "name": "primary", "properties": ["id"] }],
            "properties": [
                { "id": "id", "type": { "kind": "text" } },
                { "id": "status", "type": { "kind": "text" } }
            ]
        }
    ]))?;
    Ok(Model::new(defs)?)
}

fn open_cell(path: &std::path::Path) -> Result<Cell, Box<dyn std::error::Error>> {
    let mut store = CellStore::open(path, test_model()?, StoreOptions::default())?;
    store.bind("scope", SCOPE)?;
    Ok(SqliteCell::new(store, SCOPE, |_cmd| {
        RuntimeValue::Object(BTreeMap::from([(
            "ok".to_owned(),
            RuntimeValue::Bool(true),
        )]))
    })?)
}

fn rv(value: serde_json::Value) -> Result<RuntimeValue, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(value)?)
}

fn json_obj(value: serde_json::Value) -> Result<JsonObject, Box<dyn std::error::Error>> {
    let RuntimeValue::Object(map) = rv(value)? else {
        return Err("expected JSON object".into());
    };
    Ok(map)
}

fn make_command(
    id: &str,
    generation: u64,
    amount: i64,
    dedup: Option<&str>,
) -> Result<Command<JsonObject>, Box<dyn std::error::Error>> {
    let mut payload = json!({
        "kind": "perform",
        "function": "billing.charge",
        "functionVersion": "1",
        "amount": amount,
    });
    if let Some(contract) = dedup
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert(
            "dedupContract".to_owned(),
            serde_json::Value::String(contract.to_owned()),
        );
    }
    Ok(Command {
        id: CommandId(id.to_owned()),
        generation: Generation(generation),
        idempotency_key: IdempotencyKey(format!("{id}/{generation}")),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload: json_obj(payload)?,
    })
}

fn enqueue_cmd(
    cell: &mut Cell,
    command: Command<JsonObject>,
) -> Result<(), CommitError<CellError>> {
    cell.commit(CellCommit {
        effects: vec![Effect::Perform(command)],
        ..CellCommit::empty()
    })
    .map(|_| ())
}

#[test]
fn same_generation_payload_divergence_fails_closed_across_active_prior_and_consumed() -> TestResult
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_cell(&path)?;

    let g0 = make_command("charge-1", 0, 100, None)?;
    enqueue_cmd(&mut cell, g0.clone())?;
    // Identical replay is an idempotent no-op.
    enqueue_cmd(&mut cell, g0.clone())?;

    // Different payload at same active generation fails closed.
    let diverged_payload = make_command("charge-1", 0, 200, None)?;
    let err = enqueue_cmd(&mut cell, diverged_payload);
    assert!(matches!(
        err,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 0,
            ..
        }))
    ));

    // Different dedup contract at same active generation fails closed.
    let diverged_dedup = make_command("charge-1", 0, 100, Some("operationKeyIdempotent"))?;
    let err = enqueue_cmd(&mut cell, diverged_dedup);
    assert!(matches!(
        err,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 0,
            ..
        }))
    ));

    // Advance unclaimed gen 0 -> gen 1.
    let g1 = make_command("charge-1", 1, 150, None)?;
    enqueue_cmd(&mut cell, g1.clone())?;

    // Diverged replay of prior superseded gen 0 still fails closed.
    let prior_diverged = make_command("charge-1", 0, 999, None)?;
    let err = enqueue_cmd(&mut cell, prior_diverged);
    assert!(matches!(
        err,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 0,
            ..
        }))
    ));
    // Identical replay of prior superseded gen 0 is an idempotent no-op.
    enqueue_cmd(&mut cell, g0)?;

    // Claim, complete, and consume gen 1.
    let claimed = cell.claim_perform()?;
    assert_eq!(claimed, ClaimOutcome::Fresh(g1.clone()));
    let done = cell.complete_perform(&g1, rv(json!({ "ok": true }))?)?;
    assert!(done.is_some());
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: g1.id.clone(),
        generation: g1.generation,
    }))?;

    // Diverged replay of consumed gen 1 fails closed; identical replay is a no-op.
    let consumed_diverged = make_command("charge-1", 1, 151, None)?;
    let err = enqueue_cmd(&mut cell, consumed_diverged);
    assert!(matches!(
        err,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 1,
            ..
        }))
    ));
    enqueue_cmd(&mut cell, g1)?;
    Ok(())
}

#[test]
fn ambiguous_dispatch_requires_durable_reconciliation_or_idempotent_contract() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_cell(&path)?;

    // 1. Default contract (`ProviderDedupContract::None`): ambiguous dispatch
    // blocks reissue until durable `AuthorizeReissue` is recorded.
    let g0 = make_command("pay-1", 0, 50, None)?;
    enqueue_cmd(&mut cell, g0.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(g0.clone()));
    drop(cell);

    let mut cell = open_cell(&path)?;
    assert_eq!(
        cell.claim_perform()?,
        ClaimOutcome::OutcomeUnknown(g0.clone())
    );
    let g1 = make_command("pay-1", 1, 50, None)?;
    let refused = enqueue_cmd(&mut cell, g1.clone());
    assert!(matches!(
        refused,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 1,
            ..
        }))
    ));

    cell.reconcile_outbox(
        &g0.id,
        g0.generation,
        ReconciliationDecision::AuthorizeReissue { next_generation: 1 },
        None,
    )?;
    enqueue_cmd(&mut cell, g1.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(g1.clone()));
    let ok_val = rv(json!({ "ok": true }))?;
    assert!(cell.complete_perform(&g1, ok_val)?.is_some());
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: g1.id,
        generation: g1.generation,
    }))?;

    // 2. Declared `OperationKeyIdempotent` contract allows automatic reissue
    // with the exact same business payload, but refuses when payload changes.
    let idem0 = make_command("idem-1", 0, 75, Some("operationKeyIdempotent"))?;
    enqueue_cmd(&mut cell, idem0.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(idem0));

    let idem1_same = make_command("idem-1", 1, 75, Some("operationKeyIdempotent"))?;
    enqueue_cmd(&mut cell, idem1_same.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(idem1_same));

    let idem2_changed = make_command("idem-1", 2, 76, Some("operationKeyIdempotent"))?;
    let refused = enqueue_cmd(&mut cell, idem2_changed);
    assert!(matches!(
        refused,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 2,
            ..
        }))
    ));
    let ledger = cell
        .store()
        .read()
        .outbox_ledger("idem-1")?
        .ok_or("missing idem-1 ledger")?;
    assert_eq!(
        ledger.active().dedup_contract,
        ProviderDedupContract::OperationKeyIdempotent
    );
    assert_eq!(ledger.active_generation(), 1);
    Ok(())
}

#[test]
fn late_outcome_attaches_via_dispatch_proof_without_settling_active_head() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_cell(&path)?;

    let g0 = make_command("wire-1", 0, 500, None)?;
    enqueue_cmd(&mut cell, g0.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(g0.clone()));
    let proof0 = cell
        .store()
        .read()
        .dispatch_proof("wire-1", 0)?
        .ok_or("missing gen-0 dispatch proof")?;

    cell.reconcile_outbox(
        &g0.id,
        Generation(0),
        ReconciliationDecision::AuthorizeReissue { next_generation: 1 },
        None,
    )?;
    let g1 = make_command("wire-1", 1, 500, None)?;
    enqueue_cmd(&mut cell, g1.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(g1.clone()));

    // Forged proof fails closed with `InvalidDispatchProof`.
    let forged = DispatchProof {
        claim_commit_seq: proof0.claim_commit_seq.saturating_add(99),
        ..proof0
    };
    let outcome0 = rv(json!({ "ok": true, "providerTx": "tx-0" }))?;
    let err = cell.record_late_outcome(&g0.id, forged, true, &outcome0);
    assert!(matches!(
        err,
        Err(CellError::OutboxLedgerRefused {
            refusal: OutboxLedgerRefusal::InvalidDispatchProof,
            ..
        })
    ));

    // Valid DispatchProof attaches immutably to gen 0 without touching gen 1.
    let status = cell.record_late_outcome(&g0.id, proof0, true, &outcome0)?;
    assert_eq!(status, ApplyStatus::Applied);
    let dup = cell.record_late_outcome(&g0.id, proof0, true, &outcome0)?;
    assert_eq!(dup, ApplyStatus::IdempotentDuplicate);

    // Conflicting outcome for gen 0 fails closed with `OutcomeDiverged`.
    let conflict = rv(json!({ "ok": false, "error": "timeout" }))?;
    let err = cell.record_late_outcome(&g0.id, proof0, false, &conflict);
    assert!(matches!(
        err,
        Err(CellError::OutboxLedgerRefused {
            refusal: OutboxLedgerRefusal::OutcomeDiverged,
            ..
        })
    ));

    // Active head (gen 1) is completely unaffected by gen 0's late outcome.
    let head = cell.store().read().head_command()?.ok_or("missing head")?;
    assert_eq!(head.command.generation, 1);
    assert_eq!(head.result, None);

    let ledger = cell
        .store()
        .read()
        .outbox_ledger("wire-1")?
        .ok_or("missing wire-1 ledger")?;
    assert_eq!(ledger.active_generation(), 1);
    assert_eq!(ledger.head_projection().outcome, None);
    assert_eq!(ledger.prior_confirmed_successes(), vec![0]);

    // Completing and consuming gen 1 preserves gen 0's prior confirmed success.
    let outcome1 = rv(json!({ "ok": true, "providerTx": "tx-1" }))?;
    assert!(cell.complete_perform(&g1, outcome1)?.is_some());
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: g1.id,
        generation: g1.generation,
    }))?;
    let settled = cell
        .store()
        .read()
        .outbox_ledger("wire-1")?
        .ok_or("missing settled ledger")?;
    assert!(settled.is_terminal());
    assert_eq!(settled.prior_confirmed_successes(), vec![0]);
    Ok(())
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
    let export_dir = dir.join("export");
    let mut src = open_cell(&src_path)?;
    let cell_id = String::from(
        src.store()
            .cell_id()?
            .ok_or("source cell missing cell_id")?,
    );
    let c1 = make_command("fence-1", 0, 100, Some("operationKeyIdempotent"))?;
    let c2 = make_command("after-1", 0, 200, None)?;
    enqueue_cmd(&mut src, c1.clone())?;
    enqueue_cmd(&mut src, c2.clone())?;
    assert_eq!(src.claim_perform()?, ClaimOutcome::Fresh(c1.clone()));

    let mut exporter = Exporter::local(&export_dir)?;
    let count = src.store().export_commit_count()?;
    for seq in 1..=count {
        if let Some(record) = src.store().export_record(seq)? {
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

#[test]
fn fork_cannot_reconcile_compensate_or_record_late_outcome() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_cell(&path)?;
    let cmd = make_command("fork-cmd", 0, 10, None)?;
    enqueue_cmd(&mut cell, cmd.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(cmd.clone()));
    let proof = cell
        .store()
        .read()
        .dispatch_proof("fork-cmd", 0)?
        .ok_or("missing proof")?;

    let head = cell.store().verified_head()?;
    let mut fork = cell.store().fork("what-if", &head)?;
    assert!(matches!(
        fork.reconcile_outbox(
            SCOPE,
            10,
            "fork-cmd",
            0,
            ReconciliationDecision::AuthorizeReissue { next_generation: 1 },
            None,
        ),
        Err(CellError::WorldPerformForbidden)
    ));
    assert!(matches!(
        fork.record_outbox_compensation(SCOPE, 11, "fork-cmd", 0, true),
        Err(CellError::WorldPerformForbidden)
    ));
    assert!(matches!(
        fork.record_late_outcome("fork-cmd", proof, true, &rv(json!({ "ok": true }))?),
        Err(CellError::WorldPerformForbidden)
    ));
    Ok(())
}

#[test]
fn v10_store_upgrades_to_v11_with_validated_outbox_ledgers() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("upgrade-v10.sqlite");
    let mut cell = open_cell(&path)?;
    let consumed = make_command("done-1", 0, 10, None)?;
    let claimed = make_command("claim-1", 0, 20, None)?;
    let pending = make_command("wait-1", 0, 30, None)?;
    enqueue_cmd(&mut cell, consumed.clone())?;
    enqueue_cmd(&mut cell, claimed.clone())?;
    enqueue_cmd(&mut cell, pending.clone())?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(consumed.clone()));
    assert!(
        cell.complete_perform(&consumed, rv(json!({ "ok": true }))?)?
            .is_some()
    );
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: consumed.id,
        generation: Generation(0),
    }))?;
    assert_eq!(cell.claim_perform()?, ClaimOutcome::Fresh(claimed.clone()));
    drop(cell);

    // Simulate a storage-v10 store (pre-`_norn_outbox_record`, stamped v10, with
    // a restore fence on `wait-1`).
    let raw = NativeEngine::open(&path, StoreOptions::default())?;
    raw.execute_batch("DROP TABLE \"_norn_outbox_record\";")?;
    raw.execute(
        "UPDATE \"_norn_meta\" SET \"value\" = ?1 WHERE \"key\" = ?2",
        &[
            norn_cell::engine::SqlValue::Text(format!(
                r#"{{"format":"{STAMP_FORMAT}","storage":10,"writer":"v10-fixture"}}"#
            )),
            norn_cell::engine::SqlValue::Text(STAMP_KEY.to_owned()),
        ],
    )?;
    raw.execute(
        "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES ('restore:perform_fence:wait-1', '0')",
        &[],
    )?;
    drop(raw);

    let mut upgraded = open_cell(&path)?;
    let done_ledger = upgraded
        .store()
        .read()
        .outbox_ledger("done-1")?
        .ok_or("missing done-1 ledger")?;
    assert!(done_ledger.is_terminal());

    let claim_ledger = upgraded
        .store()
        .read()
        .outbox_ledger("claim-1")?
        .ok_or("missing claim-1 ledger")?;
    assert!(claim_ledger.active().is_ambiguous_dispatch());
    assert_eq!(
        upgraded.claim_perform()?,
        ClaimOutcome::OutcomeUnknown(claimed)
    );

    let wait_ledger = upgraded
        .store()
        .read()
        .outbox_ledger("wait-1")?
        .ok_or("missing wait-1 ledger")?;
    assert_eq!(
        wait_ledger.head_projection().state,
        OutboxState::ReconciliationRequired { fenced_at: 0 }
    );
    // Unauthorized reissue on upgraded ambiguous command fails closed.
    let err = enqueue_cmd(&mut upgraded, make_command("claim-1", 1, 20, None)?);
    assert!(matches!(
        err,
        Err(CommitError::Host(CellError::OutboxReissueRefused {
            generation: 1,
            ..
        }))
    ));
    Ok(())
}

#[derive(Debug, Clone)]
enum Step {
    Enqueue {
        generation: u64,
        amount: i64,
        idempotent: bool,
    },
    Claim,
    CompleteActive {
        ok: bool,
        tag: u8,
    },
    AuthorizeNext,
    AttachLate {
        generation: u64,
        ok: bool,
        tag: u8,
    },
    ConsumeActive,
}

fn step_strategy() -> BoxedStrategy<Step> {
    prop_oneof![
        3 => (0_u64..4, 1_i64..4, any::<bool>()).prop_map(|(generation, amount, idempotent)| {
            Step::Enqueue {
                generation,
                amount,
                idempotent,
            }
        }),
        2 => Just(Step::Claim),
        2 => (any::<bool>(), any::<u8>()).prop_map(|(ok, tag)| Step::CompleteActive { ok, tag }),
        2 => Just(Step::AuthorizeNext),
        2 => (0_u64..4, any::<bool>(), any::<u8>())
            .prop_map(|(generation, ok, tag)| Step::AttachLate { generation, ok, tag }),
        2 => Just(Step::ConsumeActive),
    ]
    .boxed()
}

fn check_projection_parity(cell: &Cell, command_id: &str) -> Result<(), TestCaseError> {
    let reader = cell.store().read();
    let Ok(records) = reader.outbox_records(command_id) else {
        return Err(TestCaseError::fail("outbox_records read failed"));
    };
    let Ok(ledger_opt) = reader.outbox_ledger(command_id) else {
        return Err(TestCaseError::fail("outbox_ledger read failed"));
    };
    let Ok(entry_opt) = reader.outbox_entry(command_id) else {
        return Err(TestCaseError::fail("outbox_entry read failed"));
    };
    let Some(ledger) = ledger_opt else {
        prop_assert_eq!(records.len(), 0);
        prop_assert!(entry_opt.is_none());
        return Ok(());
    };
    let Some(entry) = entry_opt else {
        return Err(TestCaseError::fail(
            "ledger present without _norn_outbox row",
        ));
    };
    let head = ledger.head_projection();
    prop_assert_eq!(entry.command.generation, head.generation);
    prop_assert_eq!(entry.queue, Queue::Fifo);
    prop_assert_eq!(entry.queue, head.queue);
    let Ok(stored_digest) = payload_digest(&entry.command.command) else {
        return Err(TestCaseError::fail("payload digest failed"));
    };
    prop_assert_eq!(stored_digest, head.payload_digest);
    prop_assert_eq!(entry.result.is_some(), head.outcome.is_some());
    // When all records belong to a single epoch, full-log replay matches the
    // active ledger identically.
    let Ok(pending) = reader.pending_command(command_id) else {
        return Err(TestCaseError::fail("pending_command read failed"));
    };
    prop_assert_eq!(pending.is_none(), ledger.is_terminal());
    let has_consumed = records.iter().any(|record| {
        matches!(
            record.kind,
            norn_kernel::world::outbox_ledger::OutboxRecordKind::Consumed { .. }
        )
    });
    if !ledger.is_terminal() && !has_consumed {
        let Ok(replayed) = OperationLedger::replay(&records) else {
            return Err(TestCaseError::fail("single-epoch replay failed"));
        };
        prop_assert_eq!(replayed, ledger);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn proptest_outbox_record_log_replay_equals_projected_head(
        steps in proptest::collection::vec(step_strategy(), 1..20)
    ) {
        let Ok(dir) = tempfile::tempdir() else {
            return Err(TestCaseError::fail("tempdir failed"));
        };
        let path = dir.path().join("prop.sqlite");
        let Ok(mut cell) = open_cell(&path) else {
            return Err(TestCaseError::fail("open_cell failed"));
        };
        let cmd_id = CommandId("op-prop".to_owned());
        let mut proofs: BTreeMap<u64, DispatchProof> = BTreeMap::new();
        for step in steps {
            match step {
                Step::Enqueue { generation, amount, idempotent } => {
                    let dedup = idempotent.then_some("operationKeyIdempotent");
                    let Ok(cmd) = make_command(&cmd_id.0, generation, amount, dedup) else {
                        return Err(TestCaseError::fail("make_command failed"));
                    };
                    let _res = enqueue_cmd(&mut cell, cmd);
                }
                Step::Claim => {
                    if let Ok(ClaimOutcome::Fresh(cmd)) = cell.claim_perform()
                        && let Ok(Some(proof)) = cell.store().read().dispatch_proof(&cmd.id.0, cmd.generation.0)
                    {
                        proofs.insert(cmd.generation.0, proof);
                    }
                }
                Step::CompleteActive { ok, tag } => {
                    if let Ok(Some(head)) = cell.store().read().head_command()
                        && let Ok(cmd) = make_command(&cmd_id.0, head.command.generation, 1, None)
                        && let Ok(val) = rv(json!({ "ok": ok, "tag": tag }))
                    {
                        let _res = cell.complete_perform(&cmd, val);
                    }
                }
                Step::AuthorizeNext => {
                    if let Ok(Some(ledger)) = cell.store().read().outbox_ledger(&cmd_id.0) {
                        let current = ledger.active_generation();
                        let _res = cell.reconcile_outbox(
                            &cmd_id,
                            Generation(current),
                            ReconciliationDecision::AuthorizeReissue {
                                next_generation: current.saturating_add(1),
                            },
                            None,
                        );
                    }
                }
                Step::AttachLate { generation, ok, tag } => {
                    if let Some(proof) = proofs.get(&generation).copied()
                        && let Ok(val) = rv(json!({ "ok": ok, "lateTag": tag }))
                    {
                        let _res = cell.record_late_outcome(&cmd_id, proof, ok, &val);
                    }
                }
                Step::ConsumeActive => {
                    if let Ok(Some(head)) = cell.store().read().head_command() {
                        let _res = cell.commit(CellCommit::empty().consuming(Consumed::Result {
                            command: cmd_id.clone(),
                            generation: Generation(head.command.generation),
                        }));
                    }
                }
            }
            check_projection_parity(&cell, &cmd_id.0)?;
        }
        cell.advance_to(LogicalTime::from_millis(10));
        drop(cell);
        let Ok(reopened) = open_cell(&path) else {
            return Err(TestCaseError::fail("reopen failed"));
        };
        check_projection_parity(&reopened, &cmd_id.0)?;
    }
}
