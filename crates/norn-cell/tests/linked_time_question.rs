#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "native linked-time conformance"
)]

//! METRIC-GRAIN-LINK-1 on the real cell: an entry with no time of its own is
//! bucketed by its event's time through an optional to-one link. An entry
//! without an event is skipped and counted (`unlinked`), never bucketed, and
//! correcting an event's time moves its entries.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::compute::QueryContext;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, Model};
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::{ArtifactEnvelope, Decimal, LawRoot, ObjectTypeDef, QuestionIR, RuntimeValue as V};
use serde_json::json;

const WORLD: &str = "synthetic-ledger";
const CLOCK: u64 = 1_792_972_800_000;

fn types() -> [ObjectTypeDef; 2] {
    [
        serde_json::from_value(json!({"name": "demo.Event", "properties": [
            {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}},
        ]}))
        .expect("event type"),
        serde_json::from_value(json!({"name": "demo.Entry", "properties": [
            {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
        ], "links": [
            {"id": "event", "target": "demo.Event", "cardinality": {"to": "one", "from": "many"}},
        ]}))
        .expect("entry type"),
    ]
}

fn envelope(object: &ObjectTypeDef) -> ArtifactEnvelope {
    let name = object.name.as_str();
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
        "namespace": "demo", "name": name, "version": "0.1.0", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {"entityVersion": "2.0", "objectType": object},
    }))
    .expect("artifact")
}

/// The derived "amount by event.occurred_at" question, weekly over five weeks.
fn fixture() -> (CellStore, QuestionPlan) {
    let [event, entry] = types();
    let space = derive_metric_space(
        &[envelope(&event), envelope(&entry)],
        &BTreeSet::new(),
        &LawRoot::Entity("demo.Entry".to_owned()),
    )
    .expect("derived space");
    let metric = space
        .candidates()
        .iter()
        .find(|candidate| {
            candidate.definition.type_params.expr.as_deref() == Some("amount")
                && candidate
                    .definition
                    .norn
                    .time_dimension
                    .as_ref()
                    .is_some_and(|time| time.links.len() == 1)
        })
        .expect("the entry's linked-time sum");
    let question: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1", "name": "demo.entries_week", "objectSet": "demo.Entry",
        "aggregate": [{"metric": metric.definition.name}],
        "groupBy": [{"path": {"links": ["event"], "property": "occurred_at"}, "grain": "week"}],
        "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-08-10T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 5000},
    }))
    .expect("question");
    let plan = space
        .admit(&question, 100)
        .expect("admitted linked question");
    assert!(plan.time_link().is_some());
    let model = Model::from_definitions(&types(), &[]).expect("model");
    (CellStore::open_in_memory(model).expect("cell"), plan)
}

fn put(object_type: &str, key: &str, values: BTreeMap<String, V>) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.into(),
        key: key.into(),
        values,
    }
}

fn event(key: &str, at: &str) -> FactWriteRow {
    put(
        "demo.Event",
        key,
        BTreeMap::from([("occurred_at".into(), V::from(at))]),
    )
}

fn entry(key: &str, amount: i64) -> FactWriteRow {
    let amount = V::from(Decimal::parse(&amount.to_string()).expect("exact amount"));
    put(
        "demo.Entry",
        key,
        BTreeMap::from([("amount".into(), amount)]),
    )
}

fn link(entry: &str, event: &str) -> LinkChange {
    LinkChange::Add {
        object_type: "demo.Entry".into(),
        link: "event".into(),
        source: entry.into(),
        target: event.into(),
        values: BTreeMap::new(),
        target_type: None,
    }
}

fn commit(store: &mut CellStore, tick: u64, writes: Vec<FactWriteRow>, links: Vec<LinkChange>) {
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK.checked_add(tick).expect("clock"),
            writes,
            links,
            ..CommitBatch::default()
        })
        .expect("commit");
}

/// Week → total, and the unlinked count.
fn answer(store: &CellStore, plan: &QuestionPlan) -> (BTreeMap<String, String>, u64) {
    let context = QueryContext {
        world: WORLD.into(),
        scope: "synthetic".into(),
        population_pin: "all-entries".into(),
        authorization_epoch: 1,
        mapping_pin: "fixture:ledger".into(),
        now_ms: CLOCK.checked_add(1_000_000).expect("clock"),
        source_cursors: BTreeMap::new(),
        watermarks: BTreeMap::new(),
    };
    let result = store
        .evaluate_question(plan, context, |_| Ok(true))
        .expect("cell answer");
    let rows = result
        .rows
        .into_values()
        .map(|row| {
            let V::String(week) = row.bucket else {
                panic!("a weekly bucket")
            };
            (week, row.totals[0].canonical())
        })
        .collect();
    (rows, result.unlinked)
}

#[test]
fn an_entry_takes_its_events_time_and_one_without_an_event_is_counted_never_bucketed() {
    let (mut store, plan) = fixture();
    commit(
        &mut store,
        1,
        vec![
            event("e1", "2026-07-07T10:00:00Z"),
            event("e2", "2026-07-15T09:00:00Z"),
            event("e3", "2026-07-22T00:00:00Z"),
            entry("x1", 10),
            entry("x2", 20),
            entry("x3", 5),
            entry("x4", 7),
            entry("x5", 3),
        ],
        vec![
            link("x1", "e1"),
            link("x2", "e1"),
            link("x3", "e2"),
            link("x5", "e3"),
        ],
    );
    let weeks = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(week, total)| ((*week).to_owned(), (*total).to_owned()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        answer(&store, &plan),
        (
            weeks(&[
                ("2026-07-06", "30"),
                ("2026-07-13", "5"),
                ("2026-07-20", "3")
            ]),
            1
        )
    );
    // METRIC-LINK-MOVES: correcting e2's time moves x3 into the first week.
    commit(
        &mut store,
        2,
        vec![event("e2", "2026-07-08T00:00:00Z")],
        Vec::new(),
    );
    assert_eq!(
        answer(&store, &plan),
        (weeks(&[("2026-07-06", "35"), ("2026-07-20", "3")]), 1)
    );
}
