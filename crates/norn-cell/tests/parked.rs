//! PARKED-1: a `norn.decision@1` perform waits outside the FIFO perform queue
//! and completes out of order, at most once; every other command keeps strict
//! FIFO order. P3: a parked result is recorded only by the commit that
//! consumes it ([`Settlement::Record`]), so a result row exists iff its
//! consuming commit does.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    reason = "integration assertions"
)]

use norn_cell::test_door::TestCommit as _;
use std::collections::BTreeMap;

use norn_cell::{CellError, ClaimOutcome, Model, Settlement, SqliteCell, StoreOptions};
use norn_ir::RuntimeValue;
use norn_kernel::cell_host::JsonObject;
use norn_kernel::world::outbox::Queue;
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, CommitError, Consumed, Effect, FactEvent,
    FreshnessPolicy, Generation, IdempotencyKey,
};
use norn_testkit::proptest::prelude::*;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

fn text(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

/// A flow's perform record, as the flow rule emits it.
fn perform(function: &str, version: &str) -> JsonObject {
    BTreeMap::from([
        ("kind".to_owned(), text("perform")),
        ("function".to_owned(), text(function)),
        ("functionVersion".to_owned(), text(version)),
    ])
}

fn decision() -> JsonObject {
    perform("norn.decision", "1")
}

fn question() -> JsonObject {
    perform("norn.question", "1")
}

fn tool() -> JsonObject {
    perform("billing.notify", "1")
}

fn command(id: &str, payload: JsonObject) -> Command<JsonObject> {
    Command {
        id: CommandId(id.to_owned()),
        generation: Generation(1),
        idempotency_key: IdempotencyKey(format!("{id}/1")),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload,
    }
}

fn dispatch(cell: &mut Cell, commands: Vec<Command<JsonObject>>) {
    cell.commit(CellCommit {
        effects: commands.into_iter().map(Effect::Perform).collect(),
        ..CellCommit::empty()
    })
    .unwrap();
}

fn answer(choice: &str) -> JsonObject {
    BTreeMap::from([("choice".to_owned(), text(choice))])
}

fn consume(cell: &mut Cell, id: &str) -> Result<(), CommitError<CellError>> {
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: CommandId(id.to_owned()),
        generation: Generation(1),
    }))
    .map(|_| ())
}

/// Record `choice` as parked `id`'s result and consume it, in one commit.
fn settle(
    cell: &mut Cell,
    id: &str,
    generation: u64,
    result: JsonObject,
) -> Result<(), CommitError<CellError>> {
    let consumed = Consumed::Result {
        command: CommandId(id.to_owned()),
        generation: Generation(generation),
    };
    cell.commit_settled(
        CellCommit::empty().consuming(consumed),
        Settlement::Record(result),
    )
    .map(|_| ())
}

fn responder() -> impl FnMut(&Command<JsonObject>) -> JsonObject + Send + 'static {
    |_command| BTreeMap::from([("delivered".to_owned(), RuntimeValue::Bool(true))])
}

fn fresh() -> Cell {
    SqliteCell::open_in_memory(
        Model::from_definitions(&[], &[]).unwrap(),
        "org_demo/dev",
        responder(),
    )
    .unwrap()
}

fn not_pending(result: &Result<(), CommitError<CellError>>) -> bool {
    matches!(
        result,
        Err(CommitError::NotPending { .. } | CommitError::Host(CellError::NotPending(_)))
    )
}

#[test]
fn a_parked_question_never_blocks_the_fifo_head() {
    let mut cell = fresh();
    dispatch(
        &mut cell,
        vec![command("q1", question()), command("t1", tool())],
    );
    // `norn.question@1` parks outside the FIFO queue; `t1` is still at the FIFO head.
    let head = cell.store().read().next_pending_command().unwrap().unwrap();
    assert_eq!(head.command.command_id, "t1");
    assert_eq!(head.queue, Queue::Fifo);
    let parked = cell.store().read().parked_commands().unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].command.command_id, "q1");
    assert_eq!(parked[0].queue, Queue::Parked);
    settle(
        &mut cell,
        "q1",
        1,
        BTreeMap::from([("status".to_owned(), text("Answered"))]),
    )
    .unwrap();
    assert!(cell.store().read().parked_commands().unwrap().is_empty());
}

#[test]
fn a_parked_decision_never_blocks_the_fifo_head() {
    let mut cell = fresh();
    dispatch(
        &mut cell,
        vec![command("ask", decision()), command("notify", tool())],
    );
    assert_eq!(cell.store().read().parked_commands().unwrap().len(), 1);
    // The decision was enqueued first, yet the head is the tool call.
    let performed = cell.perform().unwrap().expect("the tool call is performed");
    assert_eq!(performed.command.0, "notify");
    consume(&mut cell, "notify").unwrap();
    // With the tool call consumed, the queue is empty: the decision is never performed.
    assert!(cell.perform().unwrap().is_none());
    assert_eq!(cell.claim_perform().unwrap(), ClaimOutcome::Empty);
    // The decision completes out of order whenever the cell resolves it.
    settle(&mut cell, "ask", 1, answer("approve")).unwrap();
    assert_eq!(cell.store().read().parked_commands().unwrap(), []);
}

#[test]
fn non_parked_commands_keep_strict_fifo() {
    let mut cell = fresh();
    dispatch(
        &mut cell,
        vec![command("first", tool()), command("second", tool())],
    );
    let second = command("second", tool());
    // The second command can be completed only after the first.
    assert!(
        cell.complete_perform(&second, answer("x"))
            .unwrap()
            .is_none()
    );
    assert!(
        not_pending(&settle(&mut cell, "first", 1, answer("x"))),
        "an ordinary command is never completed as parked"
    );
    assert!(not_pending(&consume(&mut cell, "second")));
    assert_eq!(cell.perform().unwrap().unwrap().command.0, "first");
    consume(&mut cell, "first").unwrap();
    assert_eq!(cell.perform().unwrap().unwrap().command.0, "second");
}

#[test]
fn concurrent_decide_and_expire_complete_a_parked_command_once() {
    let mut cell = fresh();
    dispatch(&mut cell, vec![command("ask", decision())]);
    settle(&mut cell, "ask", 1, answer("approve")).unwrap();
    let expired = settle(
        &mut cell,
        "ask",
        1,
        BTreeMap::from([("_tag".to_owned(), text("Expired"))]),
    );
    assert!(
        not_pending(&expired),
        "the second resolution writes nothing"
    );
    assert!(
        not_pending(&settle(&mut cell, "ask", 2, answer("x"))),
        "another generation never resolves it"
    );
    assert_eq!(
        cell.store().read().runs("ask").unwrap().len(),
        1,
        "one recorded result"
    );
    // Replay: the delivery is consumed exactly once.
    assert!(not_pending(&consume(&mut cell, "ask")));
    assert!(not_pending(&settle(&mut cell, "ask", 1, answer("approve"))));
}

fn open_at(path: &std::path::Path) -> Cell {
    SqliteCell::open(
        path,
        Model::from_definitions(&[], &[]).unwrap(),
        StoreOptions::default(),
        "org_demo/dev",
        responder(),
    )
    .unwrap()
}

/// A file an engine before P3 wrote can hold a parked result recorded apart
/// from its consuming commit; recovery consumes exactly that result, once.
#[test]
fn a_result_recorded_apart_is_final_and_consumed_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    dispatch(&mut open_at(&path), vec![command("ask", decision())]);
    {
        let mut store = norn_cell::CellStore::open(
            &path,
            Model::from_definitions(&[], &[]).unwrap(),
            StoreOptions::default(),
        )
        .unwrap();
        let approve = RuntimeValue::Object(answer("approve"));
        assert!(store.record_parked_apart("ask", 1, &approve).unwrap());
        // Crash after the result is recorded, before its delivery is consumed.
    }
    {
        let mut cell = open_at(&path);
        assert!(
            not_pending(&settle(&mut cell, "ask", 1, answer("reject"))),
            "the recorded result is final across restart"
        );
        let parked = cell.store().read().parked_commands().unwrap();
        assert_eq!(
            parked[0].result,
            Some(RuntimeValue::Object(answer("approve")))
        );
        let consumed = Consumed::Result {
            command: CommandId("ask".into()),
            generation: Generation(1),
        };
        cell.commit_settled(
            CellCommit::empty().consuming(consumed),
            Settlement::Recorded,
        )
        .unwrap();
        // Crash after the consuming commit, before the caller hears of it.
    }
    let mut cell = open_at(&path);
    assert!(
        not_pending(&consume(&mut cell, "ask")),
        "a retried delivery is refused"
    );
    assert_eq!(cell.store().read().parked_commands().unwrap(), []);
    cell.store().verified_head().unwrap();
}

/// A refused settled commit records nothing: no result row outlives its
/// consuming commit (P3 settlement invariant).
#[test]
fn a_refused_settlement_records_no_result() {
    let mut cell = fresh();
    dispatch(&mut cell, vec![command("ask", decision())]);
    // A settlement for a command that is not parked is refused whole.
    let refused = settle(&mut cell, "missing", 1, answer("approve"));
    assert!(matches!(
        refused,
        Err(CommitError::Host(CellError::NotPending(_)))
    ));
    // A settlement on a commit that consumes no result is refused up front.
    let mismatched = cell.commit_settled(CellCommit::empty(), Settlement::Record(answer("x")));
    assert!(matches!(
        mismatched,
        Err(CommitError::Host(CellError::Encoding(_)))
    ));
    assert_eq!(cell.store().read().runs("ask").unwrap().len(), 0);
    let parked = cell.store().read().parked_commands().unwrap();
    assert_eq!(parked[0].result, None, "nothing recorded");
    settle(&mut cell, "ask", 1, answer("approve")).unwrap();
}

/// Run `first` and `second` on two connections to one database file,
/// released together by a barrier; returns both outcomes.
fn race<T: Send + 'static>(
    path: &std::path::Path,
    first: impl FnOnce(&mut Cell) -> T + Send + 'static,
    second: impl FnOnce(&mut Cell) -> T + Send + 'static,
) -> (T, T) {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let spawn = |job: Box<dyn FnOnce(&mut Cell) -> T + Send>| {
        let barrier = std::sync::Arc::clone(&barrier);
        let path = path.to_owned();
        std::thread::spawn(move || {
            let mut cell = open_at(&path);
            barrier.wait();
            job(&mut cell)
        })
    };
    let a = spawn(Box::new(first));
    let b = spawn(Box::new(second));
    (a.join().unwrap(), b.join().unwrap())
}

#[test]
fn racing_connections_resolve_and_consume_a_parked_command_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    dispatch(&mut open_at(&path), vec![command("ask", decision())]);
    // Decide races expire on two connections. The loser either finds the
    // command settled and writes nothing, or hits the documented SQLite
    // contention (busy); it never records a second result.
    let settled = |result: Result<(), CommitError<CellError>>| {
        if not_pending(&result) {
            return Ok(false);
        }
        result.map(|()| true).map_err(|error| error.to_string())
    };
    let (decided, expired) = race(
        &path,
        move |cell| settled(settle(cell, "ask", 1, answer("approve"))),
        move |cell| {
            let expired = BTreeMap::from([("_tag".to_owned(), text("Expired"))]);
            settled(settle(cell, "ask", 1, expired))
        },
    );
    let recorded = [&decided, &expired]
        .iter()
        .filter(|outcome| matches!(outcome, Ok(true)))
        .count();
    assert_eq!(
        recorded, 1,
        "exactly one resolution is recorded: {decided:?} {expired:?}"
    );
    let cell = open_at(&path);
    assert_eq!(cell.store().read().parked_commands().unwrap(), []);
    assert_eq!(
        cell.store().read().runs("ask").unwrap().len(),
        1,
        "one run attempt"
    );
    let consuming = cell
        .store()
        .read()
        .receipts()
        .unwrap()
        .into_iter()
        .filter(|receipt| {
            matches!(&receipt.body.consumed, Some(norn_cell::Consumption::Result { command_id, .. }) if command_id == "ask")
        })
        .count();
    assert_eq!(consuming, 1, "one consuming commit");
    cell.store().verified_head().unwrap();
}

/// One step of an arbitrary interleaving.
#[derive(Debug, Clone)]
enum Step {
    Perform,
    ConsumeHead,
    Resolve(usize, bool),
    ConsumeParked(usize),
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    /// For any interleaving of performs, resolutions and consumptions over a
    /// mixed queue: parked commands are never performed, each completes and is
    /// consumed at most once with its first result, and ordinary commands are
    /// performed in enqueue order.
    #[test]
    fn any_interleaving_keeps_fifo_and_at_most_once(
        kinds in prop::collection::vec(any::<bool>(), 1..6),
        steps in prop::collection::vec(prop_oneof![
            Just(Step::Perform),
            Just(Step::ConsumeHead),
            (0_usize..6, any::<bool>()).prop_map(|(at, first)| Step::Resolve(at, first)),
            (0_usize..6).prop_map(Step::ConsumeParked),
        ], 1..24),
    ) {
        let mut cell = fresh();
        let ids: Vec<String> = (0..kinds.len()).map(|n| format!("c{n}")).collect();
        dispatch(&mut cell, kinds.iter().zip(&ids).map(|(parked, id)| {
            command(id, if *parked { decision() } else { tool() })
        }).collect());
        let ordinary: Vec<&String> = kinds.iter().zip(&ids).filter(|(parked, _)| !**parked).map(|(_, id)| id).collect();
        let parked: Vec<&String> = kinds.iter().zip(&ids).filter(|(parked, _)| **parked).map(|(_, id)| id).collect();
        let mut performed_order = Vec::new();
        let mut resolved: BTreeMap<String, String> = BTreeMap::new();
        let mut consumed: Vec<String> = Vec::new();
        for step in steps {
            match step {
                Step::Perform => {
                    if let Some(performed) = cell.perform().unwrap() {
                        prop_assert!(!parked.contains(&&performed.command.0), "a parked command is never performed");
                        if performed_order.last() != Some(&performed.command.0) {
                            performed_order.push(performed.command.0);
                        }
                    }
                }
                Step::ConsumeHead => {
                    if let Some(head) = performed_order.last().cloned()
                        && !consumed.contains(&head)
                        && consume(&mut cell, &head).is_ok()
                    {
                        consumed.push(head);
                    }
                }
                Step::Resolve(at, first) => {
                    if let Some(id) = parked.get(at.checked_rem(parked.len()).unwrap_or(0)) {
                        let choice = if first { "approve" } else { "reject" };
                        let done = settle(&mut cell, id, 1, answer(choice));
                        prop_assert_eq!(done.is_ok(), !resolved.contains_key(*id));
                        prop_assert!(done.is_ok() || not_pending(&done));
                        if done.is_ok() {
                            resolved.insert((*id).clone(), choice.to_owned());
                            consumed.push((*id).clone());
                        }
                    }
                }
                Step::ConsumeParked(at) => {
                    if let Some(id) = parked.get(at.checked_rem(parked.len()).unwrap_or(0)) {
                        // A settled result was consumed by its own commit.
                        prop_assert!(not_pending(&consume(&mut cell, id)));
                    }
                }
            }
        }
        // Ordinary commands were performed in enqueue order.
        let expected: Vec<String> = ordinary.iter().take(performed_order.len()).map(|id| (*id).clone()).collect();
        prop_assert_eq!(performed_order, expected);
        // Every consumption happened once.
        let mut unique = consumed.clone();
        unique.sort();
        unique.dedup();
        prop_assert_eq!(unique.len(), consumed.len());
    }
}

#[test]
fn a_claim_on_a_parked_command_is_refused_by_name() {
    // The same writer applies claims for live commits and retained replay, so
    // a journal that claims a parked command fails loudly, not as a generic
    // not-pending claim.
    let mut cell = fresh();
    dispatch(&mut cell, vec![command("ask", decision())]);
    let mut store =
        norn_cell::CellStore::open_in_memory(Model::from_definitions(&[], &[]).unwrap()).unwrap();
    store.bind("scope", "org_demo/dev").unwrap();
    let enqueue = norn_cell::CommitBatch {
        world: "main".into(),
        scope: "org_demo/dev".into(),
        logical_time: 1,
        effects: vec![norn_cell::EffectRow::Enqueue {
            command: cell.store().read().parked_commands().unwrap()[0]
                .command
                .clone(),
        }],
        ..norn_cell::CommitBatch::default()
    };
    store.commit(&enqueue).unwrap();
    let claim = norn_cell::CommitBatch {
        world: "main".into(),
        scope: "org_demo/dev".into(),
        logical_time: 2,
        effects: vec![norn_cell::EffectRow::ClaimPerform {
            command_id: "ask".into(),
            generation: 1,
        }],
        ..norn_cell::CommitBatch::default()
    };
    let refused = store.commit(&claim).unwrap_err();
    assert!(
        matches!(refused, CellError::ParkedClaimUnsupported { ref command_id, generation: 1 } if command_id == "ask"),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .starts_with("restore.parked_claim_unsupported")
    );
    assert_eq!(
        store.read().parked_commands().unwrap().len(),
        1,
        "nothing changed"
    );
}

/// Sol S1 (#1163): ordinary queue progress reads only the rows it selects. A
/// backlog of parked decisions whose recorded results no longer decode never
/// blocks the FIFO head, its claim, its completion or its consumption, nor
/// the completion of another parked command.
#[test]
fn queue_progress_never_decodes_unrelated_parked_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_at(&path);
    let mut commands: Vec<_> = (0..8)
        .map(|index| command(&format!("ask{index}"), decision()))
        .collect();
    commands.push(command("notify", tool()));
    commands.push(command("late", decision()));
    dispatch(&mut cell, commands);
    drop(cell);
    {
        let mut store = norn_cell::CellStore::open(
            &path,
            Model::from_definitions(&[], &[]).unwrap(),
            StoreOptions::default(),
        )
        .unwrap();
        let approve = RuntimeValue::Object(answer("approve"));
        for index in 0..8 {
            assert!(
                store
                    .record_parked_apart(&format!("ask{index}"), 1, &approve)
                    .unwrap()
            );
        }
    }
    let mut cell = open_at(&path);
    let raw = rusqlite::Connection::open(&path).unwrap();
    let corrupted = raw
        .execute(
            "UPDATE \"_norn_outbox\" SET \"result\" = 'unreadable' WHERE \"command_id\" LIKE 'ask%'",
            [],
        )
        .unwrap();
    assert_eq!(corrupted, 8);
    drop(raw);
    // The corruption is real: a queue-wide read of parked rows fails.
    assert!(cell.store().read().parked_commands().is_err());
    let performed = cell.perform().unwrap().expect("the tool call is the head");
    assert_eq!(performed.command.0, "notify");
    consume(&mut cell, "notify").unwrap();
    settle(&mut cell, "late", 1, answer("reject")).unwrap();
    assert!(cell.perform().unwrap().is_none());
}

fn queues(cell: &Cell) -> Vec<(String, Queue)> {
    cell.store()
        .read()
        .pending_commands()
        .unwrap()
        .into_iter()
        .map(|row| (row.command.command_id, row.queue))
        .collect()
}

fn reissue(cell: &mut Cell, command: Command<JsonObject>) -> Result<(), CommitError<CellError>> {
    cell.commit(CellCommit {
        effects: vec![Effect::Perform(command)],
        ..CellCommit::empty()
    })
    .map(|_| ())
}

/// D03 (#1699): the queue is stored once, at enqueue, and survives reopen. A
/// newer generation cannot move a command to another queue, and an equal or
/// older generation never re-executes or rewrites the stored payload.
#[test]
fn the_queue_is_stored_at_enqueue_and_a_reissue_never_moves_or_rewrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_at(&path);
    dispatch(
        &mut cell,
        vec![command("ask", decision()), command("notify", tool())],
    );
    let stored = vec![
        ("ask".to_owned(), Queue::Parked),
        ("notify".to_owned(), Queue::Fifo),
    ];
    assert_eq!(queues(&cell), stored);
    reissue(&mut cell, command("notify", tool())).unwrap();
    // D1: an equal-generation reissue with a different payload fails closed
    // (`PayloadDiverged`) and leaves the stored payload unchanged.
    let diverged = reissue(&mut cell, command("notify", perform("billing.notify", "2")));
    assert!(
        matches!(
            diverged,
            Err(CommitError::Host(CellError::OutboxReissueRefused {
                generation: 1,
                ..
            }))
        ),
        "{diverged:?}"
    );
    let mut moved = command("notify", decision());
    moved.generation = Generation(2);
    let moved = reissue(&mut cell, moved);
    assert!(
        matches!(
            moved,
            Err(CommitError::Host(CellError::OutboxReissueRefused {
                generation: 2,
                ..
            }))
        ),
        "{moved:?}"
    );
    let mut older = command("notify", perform("billing.notify", "0"));
    older.generation = Generation(0);
    reissue(&mut cell, older).unwrap();
    drop(cell);

    let cell = open_at(&path);
    assert_eq!(queues(&cell), stored);
    let head = cell.store().read().head_command().unwrap().unwrap();
    assert_eq!(
        (head.command.command_id.as_str(), head.command.generation),
        ("notify", 1)
    );
    // The diverged and the older payloads were never written.
    let version = head
        .command
        .command
        .get("payload")
        .and_then(|payload| payload.get("functionVersion"))
        .and_then(RuntimeValue::as_str);
    assert_eq!(version, Some("1"));
}

/// D03 (#1699): an empty queue and a head a restore fenced are different
/// answers; a reconciled claim takes only its own target, and only at a
/// generation above the fence.
#[test]
fn a_fenced_head_is_not_an_empty_queue_and_only_its_reconciled_generation_is_claimed() {
    let mut empty = fresh();
    assert_eq!(empty.claim_perform().unwrap(), ClaimOutcome::Empty);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    let mut cell = open_at(&path);
    dispatch(
        &mut cell,
        vec![command("notify", tool()), command("later", tool())],
    );
    // What a restore records for the command it fences.
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES ('restore:perform_fence:notify', '1')",
        [],
    )
    .unwrap();
    drop(raw);
    let notify = CommandId("notify".to_owned());
    let fenced = ClaimOutcome::HeadFenced {
        command: notify.clone(),
        fenced_at: Generation(1),
    };
    assert_eq!(cell.claim_perform().unwrap(), fenced);
    assert!(cell.perform().unwrap().is_none());
    assert_eq!(
        cell.claim_reconciled_perform(&notify, Generation(1))
            .unwrap(),
        fenced
    );
    assert!(matches!(
        cell.claim_reconciled_perform(&CommandId("later".to_owned()), Generation(1)),
        Err(CellError::PerformClaimNotPending { ref command_id, generation: 1 }) if command_id == "later"
    ));
    let mut next = command("notify", tool());
    next.generation = Generation(2);
    // D1: reissuing over an active restore fence requires durable reconciliation first.
    let unfenced = reissue(&mut cell, next.clone());
    assert!(
        matches!(
            unfenced,
            Err(CommitError::Host(CellError::OutboxReissueRefused {
                generation: 2,
                ..
            }))
        ),
        "{unfenced:?}"
    );
    cell.reconcile_outbox(
        &notify,
        Generation(1),
        norn_kernel::world::outbox_ledger::ReconciliationDecision::AuthorizeReissue {
            next_generation: 2,
        },
        None,
    )
    .unwrap();
    reissue(&mut cell, next.clone()).unwrap();
    assert_eq!(cell.claim_perform().unwrap(), fenced);
    assert_eq!(
        cell.claim_reconciled_perform(&notify, Generation(2))
            .unwrap(),
        ClaimOutcome::Fresh(next)
    );
    assert!(cell.store().read().perform_claimed("notify", 2).unwrap());
}
