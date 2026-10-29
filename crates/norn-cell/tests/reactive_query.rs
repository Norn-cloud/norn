#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! T10-RT-1 (#2693): declared read slices — changed vs unaffected slices,
//! coalescing (N identical subscriptions → one evaluation), the per-commit
//! fan-out cap, over-profile refusal, and evaluation against committed
//! state.

mod common;

use std::collections::BTreeMap;

use norn_cell::reactive::{
    Change, ReactiveLimits, Registry, SliceQuery, SubscribeError, VisibleItem, evaluate,
    evaluate_visible_with_shared_budget,
};
use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, Model};
use norn_ir::{ResourceLimitProfile, RuntimeValue as Value};
use serde_json::json;

fn model() -> Model {
    common::model_of(&[
        serde_json::from_value(json!({
            "name": "commerce.SalesOrder",
            "properties": [
                { "id": "so_number", "type": { "kind": "text" } },
                { "id": "status", "type": { "kind": "text" } }
            ]
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "crm.Customer",
            "properties": [{ "id": "status", "type": { "kind": "text" } }]
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "crm.KeyAccount",
            "extends": "crm.Customer"
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "crm.Prospect",
            "extends": "crm.Customer"
        }))
        .expect("decodes"),
    ])
}

fn tombstone_model() -> Model {
    tombstone_model_with_status(false)
}

fn tombstone_model_with_status(status: bool) -> Model {
    common::model_of(&[
        serde_json::from_value(json!({
            "name": "test.Node",
            "properties": if status { json!([{"id":"status", "type":{"kind":"text"}, "constraints":{"nullable":true}}]) } else { json!([]) }
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "test.SpecialNode",
            "extends": "test.Node"
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "norn.SystemRecord",
            "properties": [
                { "id": "objectType", "type": { "kind": "text" } },
                { "id": "key", "type": { "kind": "text" } }
            ]
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "norn.ObservedTombstone",
            "extends": "norn.SystemRecord"
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "norn.ArchivedObservedTombstone",
            "extends": "norn.ObservedTombstone"
        }))
        .expect("decodes"),
        serde_json::from_value(json!({
            "name": "norn.OtherSystemRecord",
            "extends": "norn.SystemRecord"
        }))
        .expect("decodes"),
    ])
}

fn node_write(object_type: &str, key: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.to_owned(),
        key: key.to_owned(),
        values: BTreeMap::new(),
    }
}

fn system_record_write(record_type: &str, object_type: &str, key: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: record_type.to_owned(),
        key: format!("{object_type}/{key}"),
        values: BTreeMap::from([
            ("objectType".to_owned(), Value::from(object_type)),
            ("key".to_owned(), Value::from(key)),
        ]),
    }
}

fn tombstone_write(object_type: &str, key: &str) -> FactWriteRow {
    system_record_write("norn.ObservedTombstone", object_type, key)
}

fn limits() -> ReactiveLimits {
    ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1())
}

fn on_hold() -> SliceQuery {
    SliceQuery {
        binding: None,
        object_type: "commerce.SalesOrder".to_owned(),
        filter: BTreeMap::from([("status".to_owned(), Value::from("on_hold"))]),
        limit: 50,
    }
}

fn values(status: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("status".to_owned(), Value::from(status))])
}

fn change(object_type: &str, key: &str, status: Option<&str>) -> Change {
    Change {
        object_type: object_type.to_owned(),
        key: key.to_owned(),
        after: status.map(values),
    }
}

#[test]
fn a_change_entering_or_leaving_a_slice_invalidates_it_and_others_do_not() {
    let model = model();
    let mut registry = Registry::new(limits());
    let (_, key) = registry
        .subscribe(&model, on_hold())
        .expect("key")
        .expect("admitted");
    // The host records the first evaluation at subscribe time.
    registry.record(&key, result_with(&[], true));
    // Entering: a sales order put on hold.
    assert_eq!(
        registry.invalidated(
            &model,
            &[change("commerce.SalesOrder", "SO-1", Some("on_hold"))]
        ),
        std::slice::from_ref(&key)
    );
    // Unaffected: another status, another type.
    assert!(
        registry
            .invalidated(
                &model,
                &[change("commerce.SalesOrder", "SO-2", Some("draft"))]
            )
            .is_empty()
    );
    assert!(
        registry
            .invalidated(&model, &[change("crm.Customer", "C-1", Some("on_hold"))])
            .is_empty()
    );

    // Leaving: an order in the last result changes status or is deleted.
    let mut store = CellStore::open_in_memory(model.clone()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![FactWriteRow::Put {
                object_type: "commerce.SalesOrder".to_owned(),
                key: "SO-1".to_owned(),
                values: BTreeMap::from([
                    ("so_number".to_owned(), Value::from("SO-1")),
                    ("status".to_owned(), Value::from("on_hold")),
                ]),
            }],
            ..CommitBatch::default()
        })
        .expect("commits");
    let result = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &limits(), 1).expect("evaluates");
    assert_eq!(result.items.len(), 1);
    assert!(result.complete);
    registry.record(&key, result);
    assert_eq!(
        registry.invalidated(
            &model,
            &[change("commerce.SalesOrder", "SO-1", Some("approved"))]
        ),
        std::slice::from_ref(&key)
    );
    assert_eq!(
        registry.invalidated(&model, &[change("commerce.SalesOrder", "SO-1", None)]),
        [key]
    );
}

#[test]
fn identical_subscriptions_coalesce_into_one_evaluation() {
    let model = model();
    let mut registry = Registry::new(limits());
    let keys: Vec<_> = (0..5)
        .map(|_| {
            registry
                .subscribe(&model, on_hold())
                .expect("key")
                .expect("admitted")
                .1
        })
        .collect();
    assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(registry.slice_count(), 1);
    let invalidated = registry.invalidated(
        &model,
        &[change("commerce.SalesOrder", "SO-1", Some("on_hold"))],
    );
    assert_eq!(invalidated.len(), 1, "one evaluation for five subscribers");
    assert_eq!(registry.subscribers(&invalidated[0]).len(), 5);
}

#[test]
fn the_per_commit_fan_out_is_capped() {
    let model = model();
    let mut registry = Registry::new(ReactiveLimits {
        max_fanout_per_commit: 2,
        ..limits()
    });
    for limit in 1..=5 {
        registry
            .subscribe(&model, SliceQuery { limit, ..on_hold() })
            .expect("key")
            .expect("admitted");
    }
    let plan = registry.plan(registry.invalidated(
        &model,
        &[change("commerce.SalesOrder", "SO-1", Some("on_hold"))],
    ));
    assert_eq!(plan.evaluate.len(), 2);
    assert_eq!(plan.stale.len(), 3);
}

#[test]
fn over_profile_empty_and_unknown_slices_are_refused() {
    let model = model();
    let mut registry = Registry::new(limits());
    let refused = registry
        .subscribe(
            &model,
            SliceQuery {
                binding: None,
                limit: 1_001,
                ..on_hold()
            },
        )
        .expect("key")
        .expect_err("over profile");
    assert_eq!(
        refused,
        SubscribeError::OverProfile {
            limit: 1_001,
            max: 1_000
        }
    );
    assert_eq!(refused.code(), "subscription.over_profile");
    let empty = registry
        .subscribe(
            &model,
            SliceQuery {
                binding: None,
                limit: 0,
                ..on_hold()
            },
        )
        .expect("key")
        .expect_err("empty");
    assert_eq!(empty, SubscribeError::EmptyBound);
    let unknown = registry
        .subscribe(
            &model,
            SliceQuery {
                binding: None,
                object_type: "acme.Nope".to_owned(),
                ..on_hold()
            },
        )
        .expect("key")
        .expect_err("unknown");
    assert_eq!(unknown.code(), "subscription.unknown_type");
    assert_eq!(registry.slice_count(), 0);
}

#[test]
fn evaluation_filters_orders_and_bounds() {
    let model = model();
    let mut store = CellStore::open_in_memory(model).expect("opens");
    let writes = [
        ("SO-3", "on_hold"),
        ("SO-1", "on_hold"),
        ("SO-2", "draft"),
        ("SO-4", "on_hold"),
    ]
    .into_iter()
    .map(|(key, status)| FactWriteRow::Put {
        object_type: "commerce.SalesOrder".to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            ("so_number".to_owned(), Value::from(key)),
            ("status".to_owned(), Value::from(status)),
        ]),
    })
    .collect();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes,
            ..CommitBatch::default()
        })
        .expect("commits");
    let result = evaluate(
        &store.read(),
        MAIN_WORLD,
        &SliceQuery {
            binding: None,
            limit: 2,
            ..on_hold()
        },
        &limits(),
        1,
    )
    .expect("evaluates");
    let keys: Vec<&str> = result.items.iter().map(|item| item.key.as_str()).collect();
    assert_eq!(keys, ["SO-1", "SO-3"], "by key, filtered, bounded");
    assert!(result.complete);
    assert_eq!(result.commit_seq, 1);
}

fn result_with(keys: &[(&str, &str)], complete: bool) -> norn_cell::reactive::SliceResult {
    norn_cell::reactive::SliceResult {
        items: keys
            .iter()
            .map(|(object_type, key)| norn_cell::reactive::SliceItem {
                object_type: (*object_type).to_owned(),
                key: (*key).to_owned(),
                version: 1,
                values: values("on_hold"),
            })
            .collect(),
        complete,
        commit_seq: 1,
    }
}

#[test]
fn a_subtype_deleted_through_its_supertype_invalidates_the_subtype_slice() {
    let model = model();
    let mut registry = Registry::new(limits());
    let query = SliceQuery {
        binding: None,
        object_type: "crm.KeyAccount".to_owned(),
        ..on_hold()
    };
    let (_, key) = registry
        .subscribe(&model, query)
        .expect("key")
        .expect("admitted");
    registry.record(&key, result_with(&[("crm.KeyAccount", "KA-1")], true));
    assert_eq!(
        registry.invalidated(&model, &[change("crm.Customer", "KA-1", None)]),
        std::slice::from_ref(&key)
    );
    // An unrelated key of the supertype does not touch it.
    assert!(
        registry
            .invalidated(&model, &[change("crm.Customer", "C-9", Some("draft"))])
            .is_empty()
    );
}

#[test]
fn stale_and_incomplete_slices_are_invalidated_by_any_related_change() {
    let model = model();
    let mut registry = Registry::new(limits());
    let (_, key) = registry
        .subscribe(&model, on_hold())
        .expect("key")
        .expect("admitted");
    let unrelated_status = [change("commerce.SalesOrder", "SO-9", Some("draft"))];

    registry.record(&key, result_with(&[], true));
    assert!(registry.invalidated(&model, &unrelated_status).is_empty());

    registry.record(&key, result_with(&[], false));
    assert_eq!(
        registry.invalidated(&model, &unrelated_status),
        std::slice::from_ref(&key),
        "an incomplete result may hide the change"
    );

    registry.record(&key, result_with(&[], true));
    registry.mark_stale(&key);
    assert!(
        registry.last(&key).is_none(),
        "the next subscriber re-evaluates"
    );
    assert_eq!(
        registry.invalidated(&model, &unrelated_status),
        std::slice::from_ref(&key)
    );
    // Other types stay unaffected.
    assert!(
        registry
            .invalidated(&model, &[change("crm.Customer", "C-1", Some("on_hold"))])
            .is_empty()
    );
}

#[test]
fn the_scan_bound_counts_every_examined_row() {
    let model = model();
    let mut store = CellStore::open_in_memory(model).expect("opens");
    let writes = [("SO-1", "draft"), ("SO-2", "draft"), ("SO-3", "on_hold")]
        .into_iter()
        .map(|(key, status)| FactWriteRow::Put {
            object_type: "commerce.SalesOrder".to_owned(),
            key: key.to_owned(),
            values: BTreeMap::from([
                ("so_number".to_owned(), Value::from(key)),
                ("status".to_owned(), Value::from(status)),
            ]),
        })
        .collect();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes,
            ..CommitBatch::default()
        })
        .expect("commits");
    let bounded = ReactiveLimits {
        max_scan: 2,
        ..limits()
    };
    let result = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &bounded, 1).expect("evaluates");
    assert!(result.items.is_empty());
    assert!(
        !result.complete,
        "the match past the scan bound is not claimed absent"
    );
    let result = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &limits(), 1).expect("evaluates");
    assert_eq!(result.items.len(), 1);
    assert!(result.complete);
}

#[test]
fn an_incomplete_slice_is_invalidated_by_sibling_changes_sharing_its_scan() {
    let model = model();
    let mut registry = Registry::new(limits());
    let query = SliceQuery {
        binding: None,
        object_type: "crm.KeyAccount".to_owned(),
        ..on_hold()
    };
    let (_, key) = registry
        .subscribe(&model, query)
        .expect("key")
        .expect("admitted");
    // Sibling rows filled the scan bound: the result is incomplete.
    registry.record(&key, result_with(&[], false));
    assert_eq!(
        registry.invalidated(&model, &[change("crm.Prospect", "P-1", None)]),
        std::slice::from_ref(&key),
        "deleting a sibling row may bring a match into the scan"
    );
    // A complete result ignores siblings.
    registry.record(&key, result_with(&[], true));
    assert!(
        registry
            .invalidated(&model, &[change("crm.Prospect", "P-1", None)])
            .is_empty()
    );
    // Other hierarchies never touch it.
    registry.record(&key, result_with(&[], false));
    assert!(
        registry
            .invalidated(&model, &[change("commerce.SalesOrder", "SO-1", None)])
            .is_empty()
    );
}

fn candidate_store(status: &str, count: usize) -> CellStore {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: (0..count)
                .map(|index| FactWriteRow::Put {
                    object_type: "commerce.SalesOrder".to_owned(),
                    key: format!("SO-{index:04}"),
                    values: BTreeMap::from([
                        (
                            "so_number".to_owned(),
                            Value::from(format!("SO-{index:04}")),
                        ),
                        ("status".to_owned(), Value::from(status)),
                    ]),
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("commits");
    store
}

#[test]
fn nonmatching_candidates_consume_the_decode_budget() {
    let store = candidate_store(&"d".repeat(400), 10);
    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 1_000,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let result = norn_cell::reactive::evaluate_with_budget(
        &store.read(),
        MAIN_WORLD,
        &on_hold(),
        &bounded,
        1,
        &mut budget,
    )
    .expect("evaluates");
    assert!(result.items.is_empty());
    assert!(
        !result.complete,
        "no-match decoding still exhausts the budget"
    );
    assert!(
        budget.remaining_bytes() < 600,
        "decoded candidates were charged"
    );
    let full = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &limits(), 1).expect("evaluates");
    assert!(full.complete);
}

#[test]
fn candidate_budget_is_shared_across_slices_in_one_turn() {
    let store = candidate_store("on_hold", 1);
    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 350,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let first = norn_cell::reactive::evaluate_with_budget(
        &store.read(),
        MAIN_WORLD,
        &on_hold(),
        &bounded,
        1,
        &mut budget,
    )
    .expect("evaluates");
    assert_eq!(first.items.len(), 1);
    assert!(first.complete);
    let second = norn_cell::reactive::evaluate_with_budget(
        &store.read(),
        MAIN_WORLD,
        &SliceQuery {
            binding: None,
            limit: 2,
            ..on_hold()
        },
        &bounded,
        1,
        &mut budget,
    )
    .expect("evaluates");
    assert!(second.items.is_empty());
    assert!(
        !second.complete,
        "a second slice cannot start a fresh turn budget"
    );
    let fresh = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &bounded, 1).expect("evaluates");
    assert_eq!(fresh.items.len(), 1);
}

#[test]
fn oversized_candidate_is_refused_before_decoding() {
    let store = candidate_store(&"d".repeat(50_000), 1);
    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 256,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let read = store
        .read()
        .object_with_budget(MAIN_WORLD, "commerce.SalesOrder", "SO-0000", &mut budget)
        .expect("bounded read");
    assert!(matches!(read, norn_cell::reactive::BudgetedRead::Exhausted));
    // The large text value was never charged/decoded, only its small metadata.
    assert!(budget.remaining_bytes() > 0);
}

#[test]
fn per_item_authorization_budget_exhaustion_makes_the_result_incomplete() {
    let store = candidate_store("on_hold", 1);
    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let result = evaluate_visible_with_shared_budget(
        &store.read(),
        MAIN_WORLD,
        &on_hold(),
        &bounded,
        1,
        &mut budget,
        &mut |_item, _budget| VisibleItem::Exhausted,
    )
    .expect("evaluates");

    assert!(result.items.is_empty());
    assert!(!result.complete, "an indeterminate authorization is stale");
}

#[test]
fn matching_root_pages_resume_after_a_deleted_boundary_and_ignore_other_sources() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let source = "source'quoted";
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: [
                ("a", "other"),
                ("b'boundary", source),
                ("c", "other"),
                ("d", source),
                ("e", source),
            ]
            .into_iter()
            .map(|(key, source)| FactWriteRow::Put {
                object_type: "commerce.SalesOrder".to_owned(),
                key: key.to_owned(),
                values: BTreeMap::from([
                    ("so_number".to_owned(), Value::from(source)),
                    ("status".to_owned(), Value::from("draft")),
                ]),
            })
            .collect(),
            ..CommitBatch::default()
        })
        .expect("commits");
    let predicates = [("so_number", source), ("status", "draft")];
    let mut budget = norn_cell::reactive::DecodeBudget::new(&limits());
    let (first, complete) = store
        .read()
        .matching_root_rows_after_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            &predicates,
            None,
            1,
            &mut budget,
        )
        .expect("first filtered page");
    assert_eq!(first[0].0, "b'boundary");
    assert_eq!(first.len(), 1);
    assert!(!complete);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![FactWriteRow::Delete {
                object_type: "commerce.SalesOrder".to_owned(),
                key: first[0].0.clone(),
            }],
            ..CommitBatch::default()
        })
        .expect("delete boundary");
    let mut budget = norn_cell::reactive::DecodeBudget::new(&limits());
    let (next, complete) = store
        .read()
        .matching_root_rows_after_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            &predicates,
            Some(&first[0].0),
            2,
            &mut budget,
        )
        .expect("page after deleted boundary");
    assert_eq!(
        next.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
        ["d", "e"]
    );
    assert!(complete);
}

#[test]
fn matching_root_pages_charge_only_matching_metadata_and_resume_after_byte_exhaustion() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let long_matching = format!("c{}", "x".repeat(1_024));
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: [
                (format!("a{}", "x".repeat(16_384)), "other"),
                ("b".to_owned(), "source"),
                (long_matching.clone(), "source"),
            ]
            .into_iter()
            .map(|(key, source)| FactWriteRow::Put {
                object_type: "commerce.SalesOrder".to_owned(),
                key,
                values: BTreeMap::from([
                    ("so_number".to_owned(), Value::from(source)),
                    ("status".to_owned(), Value::from("draft")),
                ]),
            })
            .collect(),
            ..CommitBatch::default()
        })
        .expect("commits");
    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 128,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (first, complete) = store
        .read()
        .matching_root_rows_after_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            &[("so_number", "source")],
            None,
            10,
            &mut budget,
        )
        .expect("byte-bounded source page");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, "b");
    assert!(!complete);
    let mut budget = norn_cell::reactive::DecodeBudget::new(&limits());
    let (next, complete) = store
        .read()
        .matching_root_rows_after_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            &[("so_number", "source")],
            Some(&first[0].0),
            10,
            &mut budget,
        )
        .expect("fresh budget resumes matching stream");
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].0, long_matching);
    assert!(complete);
}

#[test]
fn live_root_scans_skip_large_tombstone_history_before_budgeting() {
    let mut store = CellStore::open_in_memory(tombstone_model_with_status(true)).expect("opens");
    let history_rows = 10_001;
    let mut writes = Vec::with_capacity(history_rows * 2 + 1);
    for index in 0..history_rows {
        let key = format!("deleted-{index:04}");
        writes.push(node_write("test.Node", &key));
        writes.push(tombstone_write("test.Node", &key));
    }
    let live_key = "zz-live";
    writes.push(node_write("test.Node", live_key));
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes,
            ..CommitBatch::default()
        })
        .expect("commits historical rows and one live row");

    let live_bytes = u64::try_from(
        live_key
            .len()
            .saturating_add("test.Node".len())
            .saturating_add(std::mem::size_of::<(String, String)>()),
    )
    .expect("row size");
    let bounded = ReactiveLimits {
        max_scan: 1,
        max_candidate_bytes_per_commit: live_bytes,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut budget)
        .expect("scans visible rows");

    assert_eq!(rows, vec![(live_key.to_owned(), "test.Node".to_owned())]);
    assert!(
        complete,
        "tombstones do not consume the candidate scan bound"
    );
    assert_eq!(budget.remaining_rows(), 0);
    assert_eq!(budget.remaining_bytes(), 0);
    let mut active_budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (active_rows, active_complete) = store
        .read()
        .active_root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut active_budget)
        .expect("scans active live rows");
    assert_eq!(active_rows, rows);
    assert!(
        active_complete,
        "active tombstoned roots do not count toward the bound"
    );
    assert_eq!(active_budget.remaining_rows(), 0);
    assert_eq!(active_budget.remaining_bytes(), 0);
}

#[test]
fn tombstones_in_another_world_do_not_hide_root_rows() {
    let mut store = CellStore::open_in_memory(tombstone_model()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![node_write("test.Node", "same-key")],
            ..CommitBatch::default()
        })
        .expect("commits main row");
    store
        .commit(&CommitBatch {
            world: "fork-1".to_owned(),
            scope: "s".to_owned(),
            writes: vec![tombstone_write("test.Node", "same-key")],
            ..CommitBatch::default()
        })
        .expect("commits fork tombstone");

    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 2, &mut budget)
        .expect("scans main world");

    assert_eq!(rows, vec![("same-key".to_owned(), "test.Node".to_owned())]);
    assert!(complete);
}

#[test]
fn tombstone_keys_match_the_root_row_concrete_type_and_id() {
    let mut store = CellStore::open_in_memory(tombstone_model()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![
                node_write("test.Node", "base-target"),
                node_write("test.SpecialNode", "child-target"),
                node_write("test.SpecialNode", "child-deleted"),
                tombstone_write("test.SpecialNode", "base-target"),
                tombstone_write("test.Node", "child-target"),
                tombstone_write("test.SpecialNode", "child-deleted"),
            ],
            ..CommitBatch::default()
        })
        .expect("commits rows and tombstones");

    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 4, &mut budget)
        .expect("scans base hierarchy");

    assert_eq!(
        rows,
        vec![
            ("base-target".to_owned(), "test.Node".to_owned()),
            ("child-target".to_owned(), "test.SpecialNode".to_owned()),
        ]
    );
    assert!(complete);
}

#[test]
fn subtype_tombstone_hides_target_but_sibling_sharing_root_does_not() {
    let mut store = CellStore::open_in_memory(tombstone_model()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![
                node_write("test.Node", "hidden-by-subtype"),
                node_write("test.Node", "visible-by-sibling"),
                system_record_write(
                    "norn.ArchivedObservedTombstone",
                    "test.Node",
                    "hidden-by-subtype",
                ),
                system_record_write("norn.OtherSystemRecord", "test.Node", "visible-by-sibling"),
            ],
            ..CommitBatch::default()
        })
        .expect("commits target rows and shared-root system records");

    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 2, &mut budget)
        .expect("scans base hierarchy");

    assert_eq!(
        rows,
        vec![("visible-by-sibling".to_owned(), "test.Node".to_owned())]
    );
    assert!(complete);
}

#[test]
fn untombstoned_root_scan_works_when_model_has_no_tombstone_type() {
    let store = candidate_store("draft", 1);
    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "commerce.SalesOrder", 2, &mut budget)
        .expect("scans without tombstone schema");

    assert_eq!(
        rows,
        vec![("SO-0000".to_owned(), "commerce.SalesOrder".to_owned())]
    );
    assert!(complete);
}

#[test]
fn deleting_a_tombstone_restores_root_row_visibility() {
    let mut store = CellStore::open_in_memory(tombstone_model()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![
                node_write("test.Node", "restored"),
                tombstone_write("test.Node", "restored"),
            ],
            ..CommitBatch::default()
        })
        .expect("commits row and tombstone");

    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 2, &mut budget)
        .expect("scans tombstoned row");
    assert!(rows.is_empty());
    assert!(complete);

    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![FactWriteRow::Delete {
                object_type: "norn.ObservedTombstone".to_owned(),
                key: "test.Node/restored".to_owned(),
            }],
            ..CommitBatch::default()
        })
        .expect("deletes tombstone");
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 2, &mut budget)
        .expect("scans visible recreated row");

    assert_eq!(rows, vec![("restored".to_owned(), "test.Node".to_owned())]);
    assert!(complete);
}

#[test]
fn untombstoned_root_scan_still_reports_live_row_overflow() {
    let mut store = CellStore::open_in_memory(tombstone_model()).expect("opens");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![
                node_write("test.Node", "first"),
                node_write("test.Node", "second"),
            ],
            ..CommitBatch::default()
        })
        .expect("commits visible rows");

    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .untombstoned_root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut budget)
        .expect("scans bounded visible prefix");

    assert_eq!(rows, vec![("first".to_owned(), "test.Node".to_owned())]);
    assert!(
        !complete,
        "the caller must refuse to publish an incomplete live prefix"
    );
}

#[test]
fn long_scan_ids_are_bounded_before_owned_metadata_is_allocated() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let prefix = "x".repeat(50 * 1_024);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: (0..20)
                .map(|index| {
                    let key = format!("{prefix}{index:04}");
                    FactWriteRow::Put {
                        object_type: "commerce.SalesOrder".to_owned(),
                        key: key.clone(),
                        values: BTreeMap::from([
                            ("so_number".to_owned(), Value::from(key)),
                            ("status".to_owned(), Value::from("draft")),
                        ]),
                    }
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("commits");
    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 100 * 1_024,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .root_rows_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            bounded.max_scan,
            &mut budget,
        )
        .expect("metadata scan");
    assert_eq!(
        rows.len(),
        1,
        "the second long ID cannot be copied into the result vector"
    );
    assert!(!complete);
    let metadata_bytes = u64::try_from(
        rows[0]
            .0
            .len()
            .saturating_add(rows[0].1.len())
            .saturating_add(std::mem::size_of::<(String, String)>()),
    )
    .expect("size");
    assert_eq!(
        budget.remaining_bytes(),
        bounded
            .max_candidate_bytes_per_commit
            .saturating_sub(metadata_bytes)
    );
    let result = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &bounded, 1).expect("evaluates");
    assert!(result.items.is_empty());
    assert!(
        !result.complete,
        "long nonmatching IDs still consume candidate bytes"
    );
    let (second, complete) = store
        .read()
        .root_rows_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            bounded.max_scan,
            &mut budget,
        )
        .expect("second scan");
    assert!(
        second.is_empty(),
        "another slice shares the remaining metadata budget"
    );
    assert!(!complete);
}

#[test]
fn an_individual_id_larger_than_the_budget_is_not_fetched() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let key = "x".repeat(300 * 1_024);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: vec![FactWriteRow::Put {
                object_type: "commerce.SalesOrder".to_owned(),
                key: key.clone(),
                values: BTreeMap::from([
                    ("so_number".to_owned(), Value::from(key)),
                    ("status".to_owned(), Value::from("draft")),
                ]),
            }],
            ..CommitBatch::default()
        })
        .expect("commits");
    let bounded = limits();
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (rows, complete) = store
        .read()
        .root_rows_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            bounded.max_scan,
            &mut budget,
        )
        .expect("metadata scan");
    assert!(
        rows.is_empty(),
        "the length-only pass excludes oversized ID/type values"
    );
    assert!(!complete);
    assert_eq!(
        budget.remaining_bytes(),
        bounded.max_candidate_bytes_per_commit,
        "rejected metadata was not allocated or charged"
    );
    let result = evaluate(&store.read(), MAIN_WORLD, &on_hold(), &bounded, 1).expect("evaluates");
    assert!(result.items.is_empty());
    assert!(!result.complete);
}

#[test]
fn budgeted_id_page_skips_oversized_legacy_ids_before_fetching_them() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let oversized = format!("a{}", "x".repeat(300 * 1_024));
    let small = "z-normal";
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: [oversized, small.to_owned()]
                .into_iter()
                .map(|key| FactWriteRow::Put {
                    object_type: "commerce.SalesOrder".to_owned(),
                    key: key.clone(),
                    values: BTreeMap::from([
                        ("so_number".to_owned(), Value::from(key.as_str())),
                        ("status".to_owned(), Value::from("draft")),
                    ]),
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("commits");

    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 100_000,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (ids, complete) = store
        .read()
        .ids_after_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            None,
            10,
            4_096,
            &mut budget,
        )
        .expect("preflights IDs");
    assert_eq!(ids, [small]);
    assert!(
        complete,
        "the filtered ID stream has no further supported IDs"
    );
    assert_eq!(
        budget.remaining_bytes(),
        bounded
            .max_candidate_bytes_per_commit
            .saturating_sub(u64::try_from(small.len()).expect("ID length")),
        "the oversized legacy ID was neither fetched nor charged"
    );
}

#[test]
fn budgeted_id_page_returns_an_incomplete_affordable_prefix() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let keys: Vec<String> = (0..8)
        .map(|index| format!("{}-{index:02}", "x".repeat(100)))
        .collect();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            writes: keys
                .iter()
                .map(|key| FactWriteRow::Put {
                    object_type: "commerce.SalesOrder".to_owned(),
                    key: key.clone(),
                    values: BTreeMap::from([
                        ("so_number".to_owned(), Value::from(key.as_str())),
                        ("status".to_owned(), Value::from("draft")),
                    ]),
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("commits");

    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 1_000,
        ..limits()
    };
    let mut budget = norn_cell::reactive::DecodeBudget::new(&bounded);
    let (ids, complete) = store
        .read()
        .ids_after_with_budget(
            MAIN_WORLD,
            "commerce.SalesOrder",
            None,
            8,
            4_096,
            &mut budget,
        )
        .expect("preflights IDs");
    assert_eq!(ids.len(), 4, "only the affordable ID prefix is fetched");
    assert!(!complete, "a byte-truncated page must be resumed later");
    assert_eq!(
        budget.remaining_bytes(),
        1_000_u64.saturating_sub(
            ids.iter()
                .map(|id| u64::try_from(id.len()).expect("ID length"))
                .sum::<u64>()
        )
    );
}

#[test]
fn dependency_capture_refuses_entry_and_byte_overflow_without_truncation() {
    use norn_cell::reactive::{Dependency, DependencySet};
    let mut dependencies = DependencySet::default();
    for index in 0..DependencySet::MAX_ENTRIES {
        dependencies
            .record(Dependency::Object {
                object_type: "test.Record".to_owned(),
                key: index.to_string(),
            })
            .expect("fits");
    }
    assert!(matches!(
        dependencies.record(Dependency::Object {
            object_type: "test.Record".to_owned(),
            key: "overflow".to_owned()
        }),
        Err(norn_cell::CellError::DependencyBudgetExceeded)
    ));
    assert_eq!(dependencies.entries.len(), DependencySet::MAX_ENTRIES);
    let mut dependencies = DependencySet::default();
    assert!(matches!(
        dependencies.record(Dependency::Object {
            object_type: "test.Record".to_owned(),
            key: "x".repeat(DependencySet::MAX_BYTES)
        }),
        Err(norn_cell::CellError::DependencyBudgetExceeded)
    ));
    assert!(dependencies.entries.is_empty());
}
