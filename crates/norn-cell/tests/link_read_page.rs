#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

mod common;

use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::read::LinkDirection;
use norn_cell::{CellStore, CommitBatch, LinkChange, MAIN_WORLD};
use norn_ir::FactChange;
use serde_json::json;

fn node_model() -> norn_cell::Model {
    let node = serde_json::from_value(json!({
        "name": "test.Node",
        "properties": [],
        "links": [{
            "id": "related",
            "target": "test.Node",
            "cardinality": { "to": "many", "from": "many" }
        }, {
            "id": "mentions",
            "target": "test.Node",
            "cardinality": { "to": "many", "from": "many" }
        }]
    }))
    .expect("decodes node type");
    common::model_of(&[node])
}

fn create(key: &str) -> FactChange {
    serde_json::from_value(json!({
        "op": "create",
        "objectType": "test.Node",
        "key": key,
        "values": {}
    }))
    .expect("decodes fact")
}

#[test]
fn link_pages_use_stable_keyset_positions_and_report_creation_commit() {
    let mut store = CellStore::open_in_memory(node_model()).expect("opens store");
    let links = ["a", "b", "c"]
        .into_iter()
        .map(|target| LinkChange::Add {
            object_type: "test.Node".to_owned(),
            link: "related".to_owned(),
            source: "source".to_owned(),
            target: target.to_owned(),
            values: std::collections::BTreeMap::default(),
            target_type: None,
        })
        .collect();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            facts: ["source", "a", "b", "c"].into_iter().map(create).collect(),
            links,
            ..CommitBatch::default()
        })
        .expect("commits nodes and links");

    let first = store
        .read()
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "source",
            LinkDirection::Out,
            Some("related"),
            None,
            1,
        )
        .expect("reads first page");
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.candidate_count, 1);
    assert!(first.candidate_bytes > 0);
    assert_eq!(first.items[0].edge.target, "a");
    assert_eq!(
        store
            .read()
            .link_creation_commits(MAIN_WORLD, &first.items)
            .expect("reads first creation commit"),
        vec![1]
    );
    assert!(first.has_more_candidates);
    assert_eq!(first.next_after, Some(first.items[0].position.clone()));

    let second = store
        .read()
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "source",
            LinkDirection::Out,
            Some("related"),
            first.items.first().map(|item| &item.position),
            1,
        )
        .expect("reads second page");
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.candidate_count, 1);
    assert_eq!(second.items[0].edge.target, "b");
    assert_eq!(
        store
            .read()
            .link_creation_commits(MAIN_WORLD, &second.items)
            .expect("reads second creation commit"),
        vec![1]
    );
    assert!(second.has_more_candidates);

    let third = store
        .read()
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "source",
            LinkDirection::Out,
            Some("related"),
            second.items.first().map(|item| &item.position),
            1,
        )
        .expect("reads third page");
    assert_eq!(third.items.len(), 1);
    assert_eq!(third.candidate_count, 1);
    assert_eq!(third.items[0].edge.target, "c");
    assert_eq!(
        store
            .read()
            .link_creation_commits(MAIN_WORLD, &third.items)
            .expect("reads third creation commit"),
        vec![1]
    );
    assert!(!third.has_more_candidates);
    assert_eq!(third.next_after, None);
}

fn related(add: bool) -> LinkChange {
    if add {
        LinkChange::Add {
            object_type: "test.Node".to_owned(),
            link: "related".to_owned(),
            source: "source".to_owned(),
            target: "target".to_owned(),
            values: std::collections::BTreeMap::default(),
            target_type: None,
        }
    } else {
        LinkChange::Remove {
            object_type: "test.Node".to_owned(),
            link: "related".to_owned(),
            source: "source".to_owned(),
            target: "target".to_owned(),
            target_type: None,
        }
    }
}

fn named_link(link: &str, source: &str, target: &str) -> LinkChange {
    LinkChange::Add {
        object_type: "test.Node".to_owned(),
        link: link.to_owned(),
        source: source.to_owned(),
        target: target.to_owned(),
        values: std::collections::BTreeMap::default(),
        target_type: None,
    }
}

#[test]
fn both_direction_pages_merge_ordered_indexes_dedupe_self_links_and_filter_link_type() {
    let mut store = CellStore::open_in_memory(node_model()).expect("opens store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            facts: ["a", "anchor", "b"].into_iter().map(create).collect(),
            links: vec![
                named_link("related", "a", "anchor"),
                named_link("related", "anchor", "anchor"),
                named_link("related", "anchor", "b"),
                named_link("mentions", "anchor", "a"),
            ],
            ..CommitBatch::default()
        })
        .expect("commits nodes and links");

    let all = store
        .read()
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "anchor",
            LinkDirection::Both,
            Some("related"),
            None,
            8,
        )
        .expect("reads merged page");
    assert_eq!(all.candidate_count, 3);
    assert!(!all.has_more_candidates);
    assert_eq!(
        all.items
            .iter()
            .filter(|item| item.edge.source == "anchor" && item.edge.target == "anchor")
            .count(),
        1
    );

    let mut after = None;
    let mut paged = Vec::new();
    for _ in 0..4 {
        let page = store
            .read()
            .links_touching_page(
                MAIN_WORLD,
                "test.Node",
                "anchor",
                LinkDirection::Both,
                Some("related"),
                after.as_ref(),
                1,
            )
            .expect("reads one merged candidate");
        assert_eq!(page.candidate_count, 1);
        paged.extend(page.items);
        if !page.has_more_candidates {
            break;
        }
        after = page.next_after;
        assert!(after.is_some());
    }
    assert_eq!(paged, all.items);

    let mentions = store
        .read()
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "anchor",
            LinkDirection::Both,
            Some("mentions"),
            None,
            8,
        )
        .expect("filters on indexed link type");
    assert_eq!(mentions.items.len(), 1);
    assert_eq!(mentions.items[0].edge.link, "mentions");
    assert_eq!(mentions.items[0].edge.target, "a");
}

fn link_commit(store: &mut CellStore, change: LinkChange) -> u64 {
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            links: vec![change],
            ..CommitBatch::default()
        })
        .expect("link commit")
        .receipt()
        .body
        .commit_seq
}

fn creation(store: &CellStore) -> Result<Vec<u64>, norn_cell::CellError> {
    let page = store.read().links_touching_page(
        MAIN_WORLD,
        "test.Node",
        "source",
        LinkDirection::Out,
        None,
        None,
        10,
    )?;
    store.read().link_creation_commits(MAIN_WORLD, &page.items)
}

fn budget(rows: usize, bytes: u64) -> DecodeBudget {
    let mut limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    limits.max_scan = rows;
    limits.max_candidate_bytes_per_commit = bytes;
    DecodeBudget::new(&limits)
}

fn neighborhood(with_link: bool) -> CellStore {
    let mut store = CellStore::open_in_memory(node_model()).expect("opens neighborhood store");
    let mut facts = vec![create("anchor")];
    let mut links = Vec::new();
    if with_link {
        facts.push(create("target"));
        links.push(named_link("related", "target", "anchor"));
    }
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            facts,
            links,
            ..CommitBatch::default()
        })
        .expect("seeds neighborhood");
    store
}

fn oversized_neighborhood() -> (CellStore, tempfile::TempDir) {
    use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};

    let directory = tempfile::tempdir().expect("oversized candidate directory");
    let path = directory.path().join("cell.sqlite");
    let model = node_model();
    let mut store = CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default())
        .expect("opens oversized candidate store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            facts: vec![create("anchor"), create("target")],
            links: vec![named_link("related", "target", "anchor")],
            ..CommitBatch::default()
        })
        .expect("seeds oversized candidate");
    drop(store);

    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default())
        .expect("opens candidate engine");
    engine
        .execute(
            "UPDATE _norn_link_version SET source = ?1 WHERE source = ?2 AND target = ?3",
            &[
                SqlValue::from("x".repeat(17 * 1024 * 1024)),
                SqlValue::from("target"),
                SqlValue::from("anchor"),
            ],
        )
        .expect("makes a legacy oversized candidate");
    drop(engine);

    (
        CellStore::open(&path, model, norn_cell::StoreOptions::default())
            .expect("reopens oversized candidate store"),
        directory,
    )
}

fn budgeted_incoming_page(
    store: &CellStore,
    budget: &mut DecodeBudget,
    max_candidate_bytes: u64,
) -> Option<norn_cell::read::LinkReadPage> {
    store
        .read()
        .links_touching_page_with_budget(
            MAIN_WORLD,
            "test.Node",
            "anchor",
            LinkDirection::In,
            Some("related"),
            None,
            1,
            budget,
            max_candidate_bytes,
        )
        .expect("reads bounded incoming page")
}

#[test]
fn budgeted_link_page_preflights_capacity_for_empty_normal_and_oversized_neighborhoods() {
    let empty = neighborhood(false);
    let normal = neighborhood(true);
    let mut empty_rows = budget(0, 4096);
    let mut normal_rows = budget(0, 4096);
    assert!(budgeted_incoming_page(&empty, &mut empty_rows, 4096).is_none());
    assert!(budgeted_incoming_page(&normal, &mut normal_rows, 4096).is_none());
    assert_eq!(empty_rows.remaining_rows(), normal_rows.remaining_rows());
    assert_eq!(empty_rows.remaining_bytes(), normal_rows.remaining_bytes());

    let mut normal_candidate_budget = budget(10, 4096);
    let normal_page = budgeted_incoming_page(&normal, &mut normal_candidate_budget, 4096)
        .expect("positive capacity reads the seeded incoming candidate");
    assert_eq!(normal_page.candidate_count, 1);
    assert_eq!(normal_page.items.len(), 1);
    assert_eq!(normal_page.items[0].edge.source, "target");

    let (oversized, _directory) = oversized_neighborhood();
    let mut empty_zero_rows = budget(0, 4096);
    let mut oversized_zero_rows = budget(0, 4096);
    assert!(budgeted_incoming_page(&empty, &mut empty_zero_rows, 4096).is_none());
    assert!(budgeted_incoming_page(&oversized, &mut oversized_zero_rows, 4096).is_none());
    assert_eq!(
        empty_zero_rows.remaining_rows(),
        oversized_zero_rows.remaining_rows()
    );
    assert_eq!(
        empty_zero_rows.remaining_bytes(),
        oversized_zero_rows.remaining_bytes()
    );

    let mut oversized_candidate_budget = budget(10, 4096);
    let oversized_page = budgeted_incoming_page(&oversized, &mut oversized_candidate_budget, 4096)
        .expect("positive capacity returns a page after skipping the oversized candidate");
    assert_eq!(oversized_page.candidate_count, 1);
    assert!(oversized_page.items.is_empty());
}

#[test]
fn zero_capacity_root_scan_is_incomplete_for_empty_and_nonempty_tables() {
    let empty_store = CellStore::open_in_memory(node_model()).expect("opens empty root scan store");
    let mut store = CellStore::open_in_memory(node_model()).expect("opens root scan store");
    let mut empty_budget = budget(0, 4096);
    let (empty_rows, empty_complete) = empty_store
        .read()
        .root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut empty_budget)
        .expect("reads empty zero-capacity root page");
    assert!(empty_rows.is_empty());
    assert!(!empty_complete);

    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            facts: vec![create("present")],
            ..CommitBatch::default()
        })
        .expect("seeds root row");
    let mut nonempty_budget = budget(0, 4096);
    let (nonempty_rows, nonempty_complete) = store
        .read()
        .root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut nonempty_budget)
        .expect("reads nonempty zero-capacity root page");
    assert!(nonempty_rows.is_empty());
    assert!(!nonempty_complete);

    let header_bytes =
        u64::try_from(std::mem::size_of::<(String, String)>()).expect("root row header size");
    for bytes in [0, header_bytes.saturating_sub(1)] {
        let mut empty_byte_budget = budget(1, bytes);
        let mut nonempty_byte_budget = budget(1, bytes);
        let (empty_byte_rows, empty_byte_complete) = empty_store
            .read()
            .root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut empty_byte_budget)
            .expect("reads empty root page below the header byte floor");
        let (nonempty_byte_rows, nonempty_byte_complete) = store
            .read()
            .root_rows_with_budget(MAIN_WORLD, "test.Node", 1, &mut nonempty_byte_budget)
            .expect("reads nonempty root page below the header byte floor");
        assert!(empty_byte_rows.is_empty());
        assert!(nonempty_byte_rows.is_empty());
        assert!(!empty_byte_complete);
        assert!(!nonempty_byte_complete);
        assert_eq!(
            empty_byte_budget.remaining_rows(),
            nonempty_byte_budget.remaining_rows()
        );
        assert_eq!(
            empty_byte_budget.remaining_bytes(),
            nonempty_byte_budget.remaining_bytes()
        );
        assert_eq!(empty_byte_budget.remaining_rows(), 1);
        assert_eq!(empty_byte_budget.remaining_bytes(), bytes);
    }
}

#[test]
fn creation_survives_replacement_reopen_and_resets_after_removal_or_cascade() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let model = node_model();
    let mut store =
        CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).expect("store");
    store
        .commit(&CommitBatch {
            facts: ["source", "target"].into_iter().map(create).collect(),
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            ..CommitBatch::default()
        })
        .expect("nodes");
    let first = link_commit(&mut store, related(true));
    let replacement = link_commit(&mut store, related(true));
    assert!(replacement > first);
    assert_eq!(creation(&store).expect("original creation"), vec![first]);
    drop(store);
    let mut store =
        CellStore::open(&path, model, norn_cell::StoreOptions::default()).expect("reopen");
    assert_eq!(creation(&store).expect("durable creation"), vec![first]);
    link_commit(&mut store, related(false));
    assert!(creation(&store).expect("removed").is_empty());
    link_commit(&mut store, related(false));
    let recreated = link_commit(&mut store, related(true));
    assert_eq!(creation(&store).expect("recreated"), vec![recreated]);
    store
        .commit(&CommitBatch {
            facts: vec![
                serde_json::from_value(json!({
                    "op": "delete", "objectType": "test.Node", "key": "source"
                }))
                .expect("delete"),
            ],
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            ..CommitBatch::default()
        })
        .expect("cascade");
    store
        .commit(&CommitBatch {
            facts: vec![create("source")],
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            ..CommitBatch::default()
        })
        .expect("recreate source");
    assert!(creation(&store).expect("cascaded link").is_empty());
    let after_cascade = link_commit(&mut store, related(true));
    assert_eq!(
        creation(&store).expect("new incarnation"),
        vec![after_cascade]
    );
}

#[test]
fn additive_legacy_schema_preserves_unambiguous_evidence_and_refuses_ambiguous_creation() {
    use norn_cell::engine::{NativeEngine, SqlEngine};

    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let model = node_model();
    let mut store =
        CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).expect("store");
    store
        .commit(&CommitBatch {
            facts: ["source", "target"].into_iter().map(create).collect(),
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            ..CommitBatch::default()
        })
        .expect("nodes");
    let first = link_commit(&mut store, related(true));
    drop(store);
    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).expect("engine");
    engine
        .execute_batch("DROP TABLE _norn_link_creation")
        .expect("legacy schema");
    drop(engine);
    let mut store = CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default())
        .expect("additive migration");
    assert_eq!(creation(&store).expect("legacy first add"), vec![first]);
    link_commit(&mut store, related(true));
    assert_eq!(
        creation(&store).expect("preserved legacy first add"),
        vec![first]
    );
    drop(store);
    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).expect("engine");
    engine
        .execute_batch("DROP TABLE _norn_link_creation")
        .expect("ambiguous legacy schema");
    drop(engine);
    let mut store =
        CellStore::open(&path, model, norn_cell::StoreOptions::default()).expect("legacy reopen");
    assert!(matches!(
        creation(&store),
        Err(norn_cell::CellError::OutOfRange(
            "link creation unavailable"
        ))
    ));
    link_commit(&mut store, related(true));
    assert!(matches!(
        creation(&store),
        Err(norn_cell::CellError::OutOfRange(
            "link creation unavailable"
        ))
    ));
    link_commit(&mut store, related(false));
    let recreated = link_commit(&mut store, related(true));
    assert_eq!(
        creation(&store).expect("new recorded creation"),
        vec![recreated]
    );
}

#[test]
fn interface_target_type_change_with_the_same_key_starts_new_creation() {
    let definitions = [
        json!({
            "name": "test.Node", "properties": [],
            "links": [{"id": "related", "target": "test.Target",
                "cardinality": {"to": "many", "from": "many"}}]
        }),
        json!({"name": "test.TargetA", "implements": ["test.Target"], "properties": []}),
        json!({"name": "test.TargetChild", "extends": "test.TargetA", "properties": []}),
        json!({"name": "test.TargetB", "implements": ["test.Target"], "properties": []}),
    ]
    .into_iter()
    .map(|definition| serde_json::from_value(definition).expect("definition"))
    .collect::<Vec<_>>();
    let interface = serde_json::from_value(json!({"name": "test.Target", "properties": []}))
        .expect("interface");
    let model = norn_cell::Model::from_definitions_with_interfaces(&definitions, &[], &[interface])
        .expect("model");
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            facts: [
                ("test.Node", "source"),
                ("test.TargetChild", "target"),
                ("test.TargetB", "target"),
            ]
            .into_iter()
            .map(|(object_type, key)| {
                serde_json::from_value(json!({
                    "op": "create", "objectType": object_type, "key": key, "values": {}
                }))
                .expect("fact")
            })
            .collect(),
            ..CommitBatch::default()
        })
        .expect("typed nodes sharing a raw key");
    let add = |target_type: &str| LinkChange::Add {
        object_type: "test.Node".to_owned(),
        link: "related".to_owned(),
        source: "source".to_owned(),
        target: "target".to_owned(),
        values: std::collections::BTreeMap::default(),
        target_type: Some(target_type.to_owned()),
    };
    let first = link_commit(&mut store, add("test.TargetA"));
    link_commit(&mut store, add("test.TargetChild"));
    assert_eq!(creation(&store).expect("replacement"), vec![first]);
    let changed_target = link_commit(&mut store, add("test.TargetB"));
    assert_eq!(
        creation(&store).expect("different typed target"),
        vec![changed_target]
    );
    link_commit(&mut store, add("test.TargetB"));
    assert_eq!(
        creation(&store).expect("new target replacement"),
        vec![changed_target]
    );
}

#[test]
fn finding_candidates_skip_unrelated_payloads_and_refuse_oversized_evidence() {
    let mut store = CellStore::open_in_memory(node_model()).expect("store");
    let large = "x".repeat(300_000);
    for events in [
        vec![json!({"type":"observation.recorded","raw":large})],
        vec![
            json!({"type":"observation.recorded","observationId":"one"}),
            json!({"op":"data_quality_finding","fact":{"id":"one"}}),
        ],
        vec![json!({"op":"data_quality_finding","fact":{"raw":large}})],
    ] {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                logical_time: 1,
                events: events
                    .into_iter()
                    .map(|event| norn_ir::runtime::encode(&event).expect("event"))
                    .collect(),
                ..CommitBatch::default()
            })
            .expect("events");
    }
    let mut limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    limits.max_canonical_bytes = 4096;
    limits.max_candidate_bytes_per_commit = 4096;
    let mut budget = DecodeBudget::new(&limits);
    let rows_before = budget.remaining_rows();
    let page = store
        .read()
        .finding_ledger_page_with_budget(0, 3, &mut budget)
        .expect("page")
        .expect("fits");
    assert_eq!(budget.remaining_rows(), rows_before - 6);
    assert_eq!(
        page.scanned_rows, 6,
        "three outer rows and three same-commit lookup rows"
    );
    assert_eq!(page.scanned_through, 3);
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].seq, 2);
    assert_eq!(page.entries[1].seq, 3);
    assert!(
        store
            .read()
            .finding_ledger_page_with_budget(3, 1, &mut budget)
            .expect("bounded")
            .is_none()
    );
    limits.max_scan = 0;
    let mut exhausted = DecodeBudget::new(&limits);
    assert!(
        store
            .read()
            .finding_ledger_page_with_budget(0, 1, &mut exhausted)
            .expect("real row over exhausted scan")
            .is_none()
    );
    let end = store
        .read()
        .finding_ledger_page_with_budget(4, 1, &mut exhausted)
        .expect("end of history")
        .expect("empty page needs no scan credit");
    assert_eq!(end.scanned_rows, 0);
    assert!(end.entries.is_empty());
}

#[test]
fn budgeted_ledger_page_never_returns_a_partial_receipt_span() {
    let mut store = CellStore::open_in_memory(node_model()).expect("opens store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            events: vec![
                norn_ir::runtime::encode(&json!({"type": "first"})).expect("event"),
                norn_ir::runtime::encode(&json!({"type": "second"})).expect("event"),
            ],
            ..CommitBatch::default()
        })
        .expect("commits events");
    let mut limits = norn_cell::reactive::ReactiveLimits::from_profile(
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    );
    limits.max_scan = 1;
    let mut budget = norn_cell::reactive::DecodeBudget::new(&limits);
    assert!(
        store
            .read()
            .ledger_page_with_budget(0, 2, &mut budget)
            .expect("bounded page")
            .is_none()
    );
    assert_eq!(budget.remaining_rows(), 1);
}

#[test]
fn oversized_raw_candidates_and_lookahead_do_not_block_readable_edges() {
    use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
    let directory = tempfile::tempdir().expect("cell directory");
    let path = directory.path().join("cell.sqlite");
    let model = node_model();
    let mut store =
        CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).expect("cell");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            facts: ["anchor", "a", "b", "z"].into_iter().map(create).collect(),
            links: ["a", "b", "z"]
                .into_iter()
                .map(|target| LinkChange::Add {
                    object_type: "test.Node".to_owned(),
                    link: "related".to_owned(),
                    source: "anchor".to_owned(),
                    target: target.to_owned(),
                    values: std::collections::BTreeMap::default(),
                    target_type: None,
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("seed links");
    drop(store);
    // A historical/stale version identity may exceed today's admitted key
    // sizes. It must not turn the readable a/z neighborhood into a refusal.
    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).expect("engine");
    engine
        .execute(
            "UPDATE _norn_link_version SET target = ?1 WHERE source = ?2 AND target = ?3",
            &[
                SqlValue::from("b".repeat(17 * 1024 * 1024)),
                SqlValue::from("anchor"),
                SqlValue::from("b"),
            ],
        )
        .expect("legacy candidate");
    drop(engine);
    let store =
        CellStore::open(&path, model, norn_cell::StoreOptions::default()).expect("reopen cell");
    let mut after = None;
    let mut targets = Vec::new();
    let mut saw_internal_skip = false;
    loop {
        let page = store
            .read()
            .links_touching_page(
                MAIN_WORLD,
                "test.Node",
                "anchor",
                LinkDirection::Out,
                None,
                after.as_ref(),
                128,
            )
            .expect("bounded candidate page");
        targets.extend(page.items.into_iter().map(|item| item.edge.target));
        saw_internal_skip |= page
            .next_after
            .as_ref()
            .is_some_and(|position| position.row_locator.is_some());
        if !page.has_more_candidates {
            break;
        }
        after = page.next_after;
    }
    assert_eq!(targets, ["a", "z"]);
    assert!(saw_internal_skip);
}

#[test]
fn empty_link_ranges_capture_direction_and_inverse_source_identity() {
    use norn_cell::reactive::Dependency;
    let store = neighborhood(false);
    let mut work = budget(100, 32 * 1024);
    work.capture();
    store
        .read()
        .links_touching_page_with_budget(
            MAIN_WORLD,
            "test.Node",
            "anchor",
            LinkDirection::Out,
            Some("related"),
            None,
            10,
            &mut work,
            32 * 1024,
        )
        .expect("empty outgoing range");
    store
        .read()
        .link_sources_to_with_budget(MAIN_WORLD, "test.Node", "related", "anchor", 10, &mut work)
        .expect("empty inverse range");
    let dependencies = work.take_dependencies().expect("complete capture");
    assert!(dependencies.entries.contains(&Dependency::Links {
        object_type: "test.Node".to_owned(),
        key: "anchor".to_owned(),
        link: Some("related".to_owned()),
        outgoing: true,
        incoming: false
    }));
    assert!(dependencies.entries.contains(&Dependency::IncomingLinks {
        source_type: "test.Node".to_owned(),
        target: "anchor".to_owned(),
        link: "related".to_owned()
    }));
}

#[test]
fn incoming_interface_dependencies_cover_empty_add_remove_and_inherited_target_reads() {
    let definitions = [
        json!({"name":"test.Node","properties":[],"links":[{"id":"related","target":"test.Target","cardinality":{"to":"many","from":"many"}}]}),
        json!({"name":"test.TargetA","implements":["test.Target"],"properties":[]}),
        json!({"name":"test.TargetChild","extends":"test.TargetA","properties":[]}),
    ].into_iter().map(|value| serde_json::from_value(value).expect("definition")).collect::<Vec<_>>();
    let interface =
        serde_json::from_value(json!({"name":"test.Target","properties":[]})).expect("interface");
    let model = norn_cell::Model::from_definitions_with_interfaces(&definitions, &[], &[interface])
        .expect("model");
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            facts: [("test.Node", "source"), ("test.TargetChild", "target")]
                .into_iter()
                .map(|(object_type, key)| {
                    serde_json::from_value(
                        json!({"op":"create","objectType":object_type,"key":key,"values":{}}),
                    )
                    .expect("fact")
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("endpoints");
    for stage in 0..3 {
        if stage == 1 {
            link_commit(
                &mut store,
                LinkChange::Add {
                    object_type: "test.Node".to_owned(),
                    link: "related".to_owned(),
                    source: "source".to_owned(),
                    target: "target".to_owned(),
                    values: std::collections::BTreeMap::new(),
                    target_type: None,
                },
            );
        } else if stage == 2 {
            link_commit(
                &mut store,
                LinkChange::Remove {
                    object_type: "test.Node".to_owned(),
                    link: "related".to_owned(),
                    source: "source".to_owned(),
                    target: "target".to_owned(),
                    target_type: None,
                },
            );
        }
        let mut decode = budget(10, 4096);
        decode.capture();
        let page = store
            .read()
            .links_touching_page_with_budget(
                MAIN_WORLD,
                "test.TargetChild",
                "target",
                LinkDirection::In,
                Some("related"),
                None,
                10,
                &mut decode,
                4096,
            )
            .expect("read")
            .expect("bounded");
        assert_eq!(page.items.len(), usize::from(stage == 1));
        if stage == 1 {
            assert_eq!(page.items[0].edge.target_type, "test.Target");
        }
        let dependencies = decode.take_dependencies().expect("complete capture");
        for object_type in ["test.TargetChild", "test.TargetA", "test.Target"] {
            assert!(
                dependencies
                    .entries
                    .contains(&norn_cell::reactive::Dependency::Links {
                        object_type: object_type.to_owned(),
                        key: "target".to_owned(),
                        link: Some("related".to_owned()),
                        outgoing: false,
                        incoming: true,
                    }),
                "stage {stage}: stored interface/ancestor aliases must intersect the captured range"
            );
        }
    }
}
