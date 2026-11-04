//! `norn_api::objects` projects cell reads field for field: decimals keep the
//! stored number token, and link pages say whether more links follow without
//! exposing the physical keyset position.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "projection conformance"
)]
use norn_api::objects::{NornLinkPage, NornObjectRecord};
use norn_cell::read::LinkDirection;
use norn_cell::{CellStore, CommitBatch, LinkChange, MAIN_WORLD, Model};
use norn_ir::FactChange;

fn store() -> CellStore {
    let node = serde_json::from_str(
        r#"{"name":"test.Node","properties":[{"id":"amount","type":{"kind":"decimal","scale":6}}],
            "links":[{"id":"related","target":"test.Node","cardinality":{"to":"many","from":"many"}}]}"#,
    )
    .expect("node type");
    let model = Model::from_definitions(&[node], &[]).expect("model");
    let mut store = CellStore::open_in_memory(model).expect("store");
    let create = |key: &str| -> FactChange {
        serde_json::from_str(&format!(
            r#"{{"op":"create","objectType":"test.Node","key":"{key}","values":{{"amount":12345678901234.123457}}}}"#
        ))
        .expect("fact")
    };
    let link = |target: &str| LinkChange::Add {
        object_type: "test.Node".to_owned(),
        link: "related".to_owned(),
        source: "a".to_owned(),
        target: target.to_owned(),
        values: std::collections::BTreeMap::default(),
        target_type: None,
    };
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            facts: ["a", "b", "c"].into_iter().map(create).collect(),
            links: vec![link("b"), link("c")],
            ..CommitBatch::default()
        })
        .expect("commit");
    store
}

#[test]
fn an_object_keeps_its_decimal_token() {
    let store = store();
    let record = store
        .read()
        .authoritative_object(MAIN_WORLD, "test.Node", "a")
        .expect("read")
        .expect("present");
    let text = serde_json::to_string(&NornObjectRecord::from(record)).expect("JSON");
    assert!(text.contains(r#""amount":12345678901234.123457"#), "{text}");
}

#[test]
fn a_limited_link_page_says_more_follow_until_the_last() {
    let store = store();
    let reader = store.read();
    let first = reader
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "a",
            LinkDirection::Both,
            None,
            None,
            1,
        )
        .expect("first page");
    let after = first.next_after.clone();
    let page = NornLinkPage::new("test.Node", "a", first);
    assert!(page.has_more);
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        (
            page.items[0].source_type.as_str(),
            page.items[0].link.as_str(),
            page.items[0].source.as_str(),
            page.items[0].target.as_str()
        ),
        ("test.Node", "related", "a", "b")
    );
    let last = reader
        .links_touching_page(
            MAIN_WORLD,
            "test.Node",
            "a",
            LinkDirection::Both,
            None,
            after.as_ref(),
            1,
        )
        .expect("last page");
    let page = NornLinkPage::new("test.Node", "a", last);
    assert!(!page.has_more);
    assert_eq!(page.items[0].target, "c");
}
