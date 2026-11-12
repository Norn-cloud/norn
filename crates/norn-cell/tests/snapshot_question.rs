#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "native snapshot-question conformance"
)]

//! METRIC-SNAPSHOT-ASOF-1 on the real cell: a stock's weekly value is each
//! subject's last version (in valid time) at or before the week's end,
//! carried forward and summed across subjects, never over time. The generated
//! histories are the oracle vectors every other engine's lowering must match.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::compute::QueryContext;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, Model};
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::{
    ArtifactEnvelope, Decimal, LawRoot, ObjectTypeDef, QuestionIR, RuntimeValue as V,
};
use proptest::prelude::*;
use serde_json::json;

const TYPE: &str = "demo.Stock";
const WORLD: &str = "synthetic-stock";
const CLOCK: u64 = 1_792_972_800_000;
const WEEKS: [&str; 5] = [
    "2026-07-06",
    "2026-07-13",
    "2026-07-20",
    "2026-07-27",
    "2026-08-03",
];

fn object_type() -> ObjectTypeDef {
    serde_json::from_value(json!({"name": TYPE, "properties": [
        {"id": "quantity", "type": {"kind": "decimal", "scale": 3, "additivity": {"kind": "semi_additive"}}, "constraints": {"nullable": true}},
        {"id": "observed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
        {"id": "location", "type": {"kind": "text"}},
    ]}))
    .expect("typed model")
}

fn context() -> QueryContext {
    QueryContext {
        world: WORLD.into(),
        scope: "synthetic".into(),
        population_pin: "all-stock".into(),
        authorization_epoch: 1,
        mapping_pin: "fixture:stock".into(),
        now_ms: CLOCK,
        source_cursors: BTreeMap::new(),
        watermarks: BTreeMap::new(),
    }
}

/// The derived snapshot question: weekly over five weeks, optionally only `north`.
fn question(north_only: bool) -> (CellStore, norn_checker::metric::compute::QuestionPlan) {
    let object = object_type();
    let artifact: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": "demo.Stock.entity", "kind": "entity", "namespace": "demo",
        "name": TYPE, "version": "0.1.0", "lifecycle": "active", "scope": {}, "dependencies": [],
        "provenance": {"source": "system"}, "payload": {"entityVersion": "2.0", "objectType": object},
    }))
    .expect("artifact");
    let space = derive_metric_space(
        &[artifact],
        &BTreeSet::new(),
        &LawRoot::Entity(TYPE.to_owned()),
    )
    .expect("derived space");
    let metric = space
        .candidates()
        .iter()
        .find(|candidate| candidate.definition.name.ends_with(".quantity.as_of_observed_at"))
        .expect("the stock's snapshot candidate");
    let filters = if north_only {
        json!([{"path": {"property": "location"}, "operator": "eq", "value": "north"}])
    } else {
        json!([])
    };
    let question: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1", "name": "demo.stock_week", "objectSet": TYPE,
        "aggregate": [{"metric": metric.definition.name}],
        "groupBy": [{"path": {"property": "observed_at"}, "grain": "week"}],
        "filter": filters,
        "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-08-10T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 5000},
    }))
    .expect("question");
    let plan = space.admit(&question, 100).expect("admitted snapshot question");
    assert!(plan.as_of());
    let model = Model::from_definitions(&[object_type()], &[]).expect("model");
    (CellStore::open_in_memory(model).expect("cell"), plan)
}

fn commit(store: &mut CellStore, tick: u64, key: &str, quantity: Option<i64>, at: &str, location: &str) {
    let quantity = quantity.map_or(V::Null, |q| {
        V::from(Decimal::parse(&format!("{q}")).expect("exact quantity"))
    });
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK.checked_add(tick).expect("clock"),
            writes: vec![FactWriteRow::Put {
                object_type: TYPE.into(),
                key: key.into(),
                values: BTreeMap::from([
                    ("quantity".into(), quantity),
                    ("observed_at".into(), V::from(at)),
                    ("location".into(), V::from(location)),
                ]),
            }],
            ..CommitBatch::default()
        })
        .expect("version commits");
}

/// Week key → (total, nulls) of the cell's answer.
fn answer(store: &CellStore, plan: &norn_checker::metric::compute::QuestionPlan) -> BTreeMap<String, (String, u64)> {
    let mut context = context();
    context.now_ms = CLOCK.checked_add(1_000_000).expect("clock");
    store
        .evaluate_question(plan, context, |_| Ok(true))
        .expect("cell answer")
        .rows
        .into_values()
        .map(|row| {
            let V::String(week) = row.bucket else {
                panic!("a weekly bucket")
            };
            (
                week,
                (
                    row.totals[0].canonical(),
                    row.null_counts.first().copied().unwrap_or(0),
                ),
            )
        })
        .collect()
}

#[test]
fn a_stock_carries_its_last_valid_time_value_forward_and_sums_across_subjects() {
    for north_only in [false, true] {
        let (mut store, plan) = question(north_only);
        let history = [
            ("s1", Some(10), "2026-07-07T10:00:00Z", "north"),
            ("s1", Some(5), "2026-07-14T09:00:00Z", "north"),
            ("s1", Some(7), "2026-07-16T09:00:00Z", "north"),
            ("s2", Some(100), "2026-07-22T00:00:00Z", "north"),
            ("s1", Some(3), "2026-07-28T00:00:00Z", "south"),
            // Committed later but observed earlier: valid time orders the series.
            ("s2", Some(50), "2026-07-21T00:00:00Z", "north"),
            // Before the window, then a tie on the same instant: the later version wins.
            ("s3", Some(1), "2026-06-01T00:00:00Z", "north"),
            ("s3", Some(2), "2026-06-01T00:00:00Z", "north"),
            ("s4", None, "2026-07-08T00:00:00Z", "north"),
        ];
        for (tick, (key, quantity, at, location)) in (1u64..).zip(history) {
            commit(&mut store, tick, key, quantity, at, location);
        }
        let s1_moves_south = if north_only { 102 } else { 105 };
        let expected: BTreeMap<String, (String, u64)> = [
            (WEEKS[0], 12),
            (WEEKS[1], 9),
            (WEEKS[2], 109),
            (WEEKS[3], s1_moves_south),
            (WEEKS[4], s1_moves_south),
        ]
        .into_iter()
        .map(|(week, total)| (week.to_owned(), (total.to_string(), 1)))
        .collect();
        assert_eq!(answer(&store, &plan), expected, "north only: {north_only}");
    }
}

/// Observation instants spanning the window, two before it and one at a week boundary.
const INSTANTS: [&str; 10] = [
    "2026-06-01T00:00:00Z",
    "2026-07-05T23:59:59Z",
    "2026-07-06T00:00:00Z",
    "2026-07-09T12:00:00Z",
    "2026-07-13T00:00:00Z",
    "2026-07-19T23:59:59Z",
    "2026-07-24T08:00:00Z",
    "2026-07-27T00:00:00Z",
    "2026-08-05T18:30:00Z",
    "2026-08-10T00:00:00Z",
];
/// The exclusive end of each week bucket, as an index into [`INSTANTS`] order.
const WEEK_ENDS: [&str; 5] = [
    "2026-07-13T00:00:00Z",
    "2026-07-20T00:00:00Z",
    "2026-07-27T00:00:00Z",
    "2026-08-03T00:00:00Z",
    "2026-08-10T00:00:00Z",
];

prop_compose! {
    fn version()(subject in 0usize..3, quantity in proptest::option::weighted(0.85, 0i64..1_000), at in 0usize..INSTANTS.len(), north in any::<bool>())
        -> (usize, Option<i64>, usize, bool) {
        (subject, quantity, at, north)
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// METRIC-ASOF-ORACLE: the cell equals an independent per-subject fold.
    #[test]
    fn the_cell_equals_an_independent_per_subject_fold(history in proptest::collection::vec(version(), 1..14), north_only in any::<bool>()) {
        let (mut store, plan) = question(north_only);
        for (tick, (subject, quantity, at, north)) in (1u64..).zip(&history) {
            let location = if *north { "north" } else { "south" };
            commit(&mut store, tick, &format!("s{subject}"), *quantity, INSTANTS[*at], location);
        }
        let mut expected = BTreeMap::new();
        for (week, end) in WEEKS.iter().zip(WEEK_ENDS) {
            let (mut total, mut nulls, mut present) = (0i64, 0u64, false);
            for subject in 0..3 {
                // ISO-8601 Z instants of one shape order as text; the later commit breaks ties.
                let last = history
                    .iter()
                    .enumerate()
                    .filter(|(_, (s, _, at, _))| *s == subject && INSTANTS[*at] < end)
                    .max_by_key(|(commit, (_, _, at, _))| (INSTANTS[*at], *commit));
                let Some((_, (_, quantity, _, north))) = last else { continue };
                if north_only && !north {
                    continue;
                }
                present = true;
                match quantity {
                    Some(q) => total = total.checked_add(*q).expect("bounded total"),
                    None => nulls = nulls.checked_add(1).expect("bounded nulls"),
                }
            }
            if present {
                expected.insert((*week).to_owned(), (total.to_string(), nulls));
            }
        }
        prop_assert_eq!(answer(&store, &plan), expected);
    }
}
