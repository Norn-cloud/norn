//! RUN-LEDGER-1: runs of admitted flows are recorded and refreshed in the
//! cell's own commits, and survive reopen.
#![allow(clippy::expect_used, missing_docs, reason = "integration assertions")]

use std::collections::BTreeMap;

use norn_cell::flow_run::{FlowRef, RunFilter, RunStart};
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, Model, StoreOptions};
use norn_ir::{ObjectTypeDef, RuntimeValue};

const RUN: &str = "billing.RouteRun";

fn model() -> Model {
    let run: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": RUN,
        "identity": [{ "name": "primary", "properties": ["run_id"] }],
        "properties": [
            { "id": "run_id", "type": { "kind": "text" } },
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "started" }, { "value": "awaiting_perform_load" }, { "value": "completed" }
            ] }, "default": "started" }
        ],
        "lifecycles": [{ "property": "status", "initial": ["started"], "terminal": ["completed"],
            "transitions": [
                { "id": "to_awaiting_perform_load", "from": ["started"], "to": "awaiting_perform_load" },
                { "id": "to_completed", "from": ["awaiting_perform_load"], "to": "completed" }
            ] }]
    }))
    .expect("run type");
    Model::from_definitions(&[run], &[]).expect("model")
}

fn start(run_id: &str, flow: &str) -> RunStart {
    RunStart {
        run_id: run_id.to_owned(),
        flow: FlowRef {
            id: flow.to_owned(),
            version: "1.0.0".to_owned(),
        },
        object_type: RUN.to_owned(),
        state_property: "status".to_owned(),
    }
}

fn put(run_id: &str, status: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: RUN.to_owned(),
        key: run_id.to_owned(),
        values: BTreeMap::from([
            ("run_id".to_owned(), RuntimeValue::String(run_id.to_owned())),
            ("status".to_owned(), RuntimeValue::String(status.to_owned())),
        ]),
    }
}

fn batch(time: u64, writes: Vec<FactWriteRow>, starts: &[RunStart]) -> CommitBatch {
    CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        logical_time: time,
        writes,
        events: starts
            .iter()
            .map(|start| start.event().expect("event"))
            .collect(),
        ..CommitBatch::default()
    }
}

fn store() -> CellStore {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    store
}

fn refusal(error: &CellError) -> Option<&'static str> {
    if let CellError::RunRefused(code) = error {
        Some(*code)
    } else {
        None
    }
}

#[test]
fn a_start_records_the_run_and_later_writes_refresh_it_in_the_same_commit() {
    let mut store = store();
    store
        .commit(&batch(
            10,
            vec![put("run-1", "started")],
            &[start("run-1", "billing.route.flow.rule")],
        ))
        .expect("start");
    let row = store
        .read()
        .flow_run("main", "run-1")
        .expect("read")
        .expect("recorded");
    assert_eq!(row.generation, 1);
    assert_eq!(row.flow.id, "billing.route.flow.rule");
    assert_eq!(row.object_type, RUN);
    assert_eq!(row.state.as_deref(), Some("started"));
    assert_eq!(row.version, 1);
    assert_eq!((row.created_at_ms, row.updated_at_ms), (10, 10));
    let started = row.created_commit_seq;
    assert_eq!(row.updated_commit_seq, started);

    store
        .commit(&batch(25, vec![put("run-1", "awaiting_perform_load")], &[]))
        .expect("advance");
    let row = store
        .read()
        .flow_run("main", "run-1")
        .expect("read")
        .expect("recorded");
    assert_eq!(row.state.as_deref(), Some("awaiting_perform_load"));
    assert_eq!(row.version, 2);
    assert_eq!((row.created_at_ms, row.updated_at_ms), (10, 25));
    assert_eq!(row.created_commit_seq, started);
    assert!(row.updated_commit_seq > started);

    store
        .commit(&batch(
            30,
            vec![FactWriteRow::Delete {
                object_type: RUN.to_owned(),
                key: "run-1".to_owned(),
            }],
            &[],
        ))
        .expect("delete");
    let row = store
        .read()
        .flow_run("main", "run-1")
        .expect("read")
        .expect("kept");
    assert_eq!(row.state, None, "a deleted run object has no state");
    assert_eq!(row.version, 3);
}

#[test]
fn a_start_without_its_creating_write_or_a_repeated_start_commits_nothing() {
    let mut store = store();
    let ledger = |store: &CellStore| store.read().ledger(0).expect("ledger").len();

    let orphan = store
        .commit(&batch(10, Vec::new(), &[start("run-1", "flow.a")]))
        .expect_err("no creating write");
    assert_eq!(refusal(&orphan), Some("run.start_invalid"));
    assert_eq!(ledger(&store), 0);
    assert!(
        store
            .read()
            .flow_run("main", "run-1")
            .expect("read")
            .is_none()
    );

    let mut wrong_type = start("run-1", "flow.a");
    wrong_type.state_property = "run_id".to_owned();
    wrong_type.object_type = "billing.Other".to_owned();
    let refused = store
        .commit(&batch(10, vec![put("run-1", "started")], &[wrong_type]))
        .expect_err("another type");
    assert_eq!(refusal(&refused), Some("run.start_invalid"));
    assert_eq!(ledger(&store), 0);
    assert!(
        store
            .read()
            .object("main", RUN, "run-1")
            .expect("read")
            .is_none(),
        "the write rolls back with the refused start"
    );

    store
        .commit(&batch(
            10,
            vec![put("run-1", "started")],
            &[start("run-1", "flow.a")],
        ))
        .expect("start");
    let committed = ledger(&store);
    let repeated = store
        .commit(&batch(
            11,
            vec![put("run-1", "started")],
            &[start("run-1", "flow.a")],
        ))
        .expect_err("already started");
    assert_eq!(refusal(&repeated), Some("run.exists"));
    assert_eq!(ledger(&store), committed);

    let mut malformed = batch(12, vec![put("run-2", "started")], &[]);
    malformed.events = vec![RuntimeValue::Object(BTreeMap::from([(
        "type".to_owned(),
        RuntimeValue::String(norn_cell::flow_run::RUN_STARTED.to_owned()),
    )]))];
    assert_eq!(
        refusal(&store.commit(&malformed).expect_err("malformed")),
        Some("run.start_invalid")
    );
    assert!(
        store
            .read()
            .flow_run("main", "run-2")
            .expect("read")
            .is_none()
    );
}

#[test]
fn runs_page_by_id_and_filter_by_flow_and_state() {
    let mut store = store();
    for (index, (flow, status)) in [
        ("flow.a", "started"),
        ("flow.b", "started"),
        ("flow.a", "completed"),
        ("flow.a", "started"),
        ("flow.a", "started"),
    ]
    .into_iter()
    .enumerate()
    {
        let run = format!("run-{index}");
        store
            .commit(&batch(10, vec![put(&run, status)], &[start(&run, flow)]))
            .expect("start");
    }
    let ids = |filter: &RunFilter| -> Vec<String> {
        store
            .read()
            .flow_runs("main", filter)
            .expect("page")
            .into_iter()
            .map(|row| row.run_id)
            .collect()
    };
    let first = RunFilter {
        flow: Some("flow.a".to_owned()),
        state: Some("started".to_owned()),
        after: None,
        limit: 2,
    };
    assert_eq!(ids(&first), ["run-0", "run-3"]);
    let next = RunFilter {
        after: Some("run-3".to_owned()),
        ..first
    };
    assert_eq!(ids(&next), ["run-4"]);
    assert_eq!(
        ids(&RunFilter {
            limit: 10,
            ..RunFilter::default()
        })
        .len(),
        5
    );
    assert_eq!(
        ids(&RunFilter {
            state: Some("completed".to_owned()),
            limit: 10,
            ..RunFilter::default()
        }),
        ["run-2"]
    );
    assert!(
        store
            .read()
            .flow_runs(
                "fork-1",
                &RunFilter {
                    limit: 10,
                    ..RunFilter::default()
                }
            )
            .expect("page")
            .is_empty(),
        "runs are per world"
    );
}

#[test]
fn the_ledger_survives_reopen() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    store
        .commit(&batch(
            10,
            vec![put("run-1", "started")],
            &[start("run-1", "flow.a")],
        ))
        .expect("start");
    let before = store.read().flow_run("main", "run-1").expect("read");
    drop(store);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("reopen");
    assert_eq!(
        store.read().flow_run("main", "run-1").expect("read"),
        before
    );
}

#[test]
fn live_runs_count_only_non_terminal_existing_runs_of_one_flow() {
    let mut store = store();
    for (run, flow) in [
        ("run-1", "flow.a"),
        ("run-2", "flow.a"),
        ("run-3", "flow.a"),
        ("run-4", "flow.b"),
    ] {
        store
            .commit(&batch(10, vec![put(run, "started")], &[start(run, flow)]))
            .expect("start");
    }
    store
        .commit(&batch(20, vec![put("run-2", "completed")], &[]))
        .expect("complete");
    store
        .commit(&batch(
            30,
            vec![FactWriteRow::Delete {
                object_type: RUN.to_owned(),
                key: "run-3".to_owned(),
            }],
            &[],
        ))
        .expect("delete");
    let reader = store.read();
    assert_eq!(
        reader.live_runs("main", "flow.a").expect("count"),
        1,
        "run-1 only"
    );
    assert_eq!(reader.live_runs("main", "flow.b").expect("count"), 1);
    assert_eq!(reader.live_runs("main", "flow.none").expect("count"), 0);
    assert_eq!(reader.live_runs("fork-1", "flow.a").expect("count"), 0);
}
