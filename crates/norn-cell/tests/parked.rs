//! PARKED-1: a `norn.decision@1` perform waits outside the FIFO perform queue
//! and completes out of order, at most once; every other command keeps strict
//! FIFO order.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    reason = "integration assertions"
)]

use std::collections::BTreeMap;

use norn_cell::{CellError, Model, SqliteCell, StoreOptions};
use norn_ir::RuntimeValue;
use norn_kernel::cell_host::JsonObject;
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

fn not_pending(result: Result<(), CommitError<CellError>>) -> bool {
    matches!(
        result,
        Err(CommitError::NotPending { .. } | CommitError::Host(CellError::NotPending(_)))
    )
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
    assert!(cell.claim_perform().unwrap().is_none());
    // The decision completes out of order whenever the cell resolves it.
    assert!(
        cell.complete_parked(&CommandId("ask".into()), Generation(1), answer("approve"))
            .unwrap()
            .is_some()
    );
    consume(&mut cell, "ask").unwrap();
    assert!(cell.store().read().parked_commands().unwrap().is_empty());
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
        cell.complete_parked(&CommandId("first".into()), Generation(1), answer("x"))
            .unwrap()
            .is_none(),
        "an ordinary command is never completed as parked"
    );
    assert!(not_pending(consume(&mut cell, "second")));
    assert_eq!(cell.perform().unwrap().unwrap().command.0, "first");
    consume(&mut cell, "first").unwrap();
    assert_eq!(cell.perform().unwrap().unwrap().command.0, "second");
}

#[test]
fn concurrent_decide_and_expire_complete_a_parked_command_once() {
    let mut cell = fresh();
    dispatch(&mut cell, vec![command("ask", decision())]);
    let ask = CommandId("ask".into());
    let decided = cell
        .complete_parked(&ask, Generation(1), answer("approve"))
        .unwrap();
    let expired = cell
        .complete_parked(
            &ask,
            Generation(1),
            BTreeMap::from([("_tag".to_owned(), text("Expired"))]),
        )
        .unwrap();
    assert!(decided.is_some());
    assert!(expired.is_none(), "the second resolution writes nothing");
    assert!(
        cell.complete_parked(&ask, Generation(2), answer("x"))
            .unwrap()
            .is_none(),
        "another generation never resolves it"
    );
    assert_eq!(
        cell.store().read().parked_commands().unwrap()[0].result,
        Some(RuntimeValue::Object(answer("approve")))
    );
    consume(&mut cell, "ask").unwrap();
    // Replay: the delivery is consumed exactly once.
    assert!(not_pending(consume(&mut cell, "ask")));
    assert!(
        cell.complete_parked(&ask, Generation(1), answer("approve"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_crash_between_commit_and_delivery_never_completes_twice() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    let open = || {
        SqliteCell::<FactEvent, JsonObject, JsonObject>::open(
            &path,
            Model::from_definitions(&[], &[]).unwrap(),
            StoreOptions::default(),
            "org_demo/dev",
            responder(),
        )
        .unwrap()
    };
    let ask = CommandId("ask".into());
    {
        let mut cell = open();
        dispatch(&mut cell, vec![command("ask", decision())]);
        cell.complete_parked(&ask, Generation(1), answer("approve"))
            .unwrap()
            .unwrap();
        // Crash before the consuming commit: the recorded result survives.
    }
    {
        let mut cell = open();
        assert!(
            cell.complete_parked(&ask, Generation(1), answer("reject"))
                .unwrap()
                .is_none(),
            "the recorded result is final across restart"
        );
        consume(&mut cell, "ask").unwrap();
        // Crash after the consuming commit, before the caller hears of it.
    }
    let mut cell = open();
    assert!(
        not_pending(consume(&mut cell, "ask")),
        "a retried delivery is refused"
    );
    assert!(cell.store().read().parked_commands().unwrap().is_empty());
    // The retained ledger and history still verify on reopen.
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
                    if let Some(id) = parked.get(at % parked.len().max(1)) {
                        let choice = if first { "approve" } else { "reject" };
                        let done = cell.complete_parked(&CommandId((*id).clone()), Generation(1), answer(choice)).unwrap();
                        prop_assert_eq!(done.is_some(), !resolved.contains_key(*id));
                        resolved.entry((*id).clone()).or_insert_with(|| choice.to_owned());
                    }
                }
                Step::ConsumeParked(at) => {
                    if let Some(id) = parked.get(at % parked.len().max(1)) {
                        let ok = consume(&mut cell, id).is_ok();
                        prop_assert_eq!(ok, resolved.contains_key(*id) && !consumed.contains(id));
                        if ok { consumed.push((*id).clone()); }
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
